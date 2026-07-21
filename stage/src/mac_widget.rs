//! The windowless desktop pet — a transparent, borderless, always-on-top
//! AppKit window presenting the framebuffer through a layer-backed view. No
//! window chrome: only the cat + monitor pixels are opaque, everything else
//! is transparent, so the pet floats on the desktop.
//!
//! The Rust core still owns per-frame work; the QuickJS guest still owns
//! policy. Only the presentation shell changes from minifb to a CALayer
//! whose contents is a CGImage rebuilt from our RGBA buffer each frame.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use core_graphics::base::kCGRenderingIntentDefault;
use core_graphics::color_space::CGColorSpace;
use core_graphics::data_provider::CGDataProvider;
use core_graphics::image::CGImage;
use foreign_types::ForeignType;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
    NSEventMask, NSEventType, NSScreen, NSWindow, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString,
};

use crate::{render, step, Brain, Framebuffer, Sprites, Stage, H, W};

const K_ALPHA_PREMUL_LAST: u32 = 1; // kCGImageAlphaPremultipliedLast
const FLOATING_LEVEL: isize = 3; // NSFloatingWindowLevel

/// Incremental chat events streamed from cat-swarm (SSE) to the render loop.
enum ChatUpdate {
    Delta(String), // a token of the @cat reply
    Tool(String),  // a tool call name
    Done,          // turn finished — commit the streamed reply to the session
}

// A borderless NSWindow can't become key by default, so it never sees
// keyDown — which is why input used to fall back to a system dialog. This
// subclass says "yes, I can be key", so the chatbox can take typing inline.
declare_class!(
    struct PocketWindow;
    unsafe impl ClassType for PocketWindow {
        type Super = NSWindow;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "PocketCatWindow";
    }
    impl DeclaredClass for PocketWindow {}
    unsafe impl PocketWindow {
        #[method(canBecomeKeyWindow)]
        fn can_become_key(&self) -> bool {
            true
        }
    }
);

fn cg_image(fb: &Framebuffer) -> CGImage {
    let bytes = fb.to_rgba8();
    let provider = CGDataProvider::from_buffer(std::sync::Arc::new(bytes));
    let cs = CGColorSpace::create_device_rgb();
    CGImage::new(
        W,
        H,
        8,
        32,
        W * 4,
        &cs,
        K_ALPHA_PREMUL_LAST,
        &provider,
        false,
        kCGRenderingIntentDefault,
    )
}

fn set_layer_image(view: &objc2_app_kit::NSView, img: &CGImage) {
    unsafe {
        if let Some(layer) = view.layer() {
            let ptr: *const std::ffi::c_void = img.as_ptr() as *const _;
            let obj = &*(ptr as *const AnyObject);
            layer.setContents(Some(obj));
        }
    }
}

pub fn run(scale: f64) -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("must run on the main thread"))?;

    let stage = Stage::new();
    let brain = Brain::new(stage.clone())?;
    let sprites = Sprites::load();
    let font = crate::Font::build();
    let text = crate::text::Text::load();
    let mut fb = Framebuffer::new(W, H);

    // @cat replies stream back from a worker over this channel (the user line
    // is appended immediately on send; the reply streams in token by token).
    let (chat_tx, chat_rx) = std::sync::mpsc::channel::<ChatUpdate>();
    // chat sessions (chat-per-session + the session log), persisted on disk.
    let mut sessions = crate::session::Sessions::load();
    let sync_sessions = |stage: &Rc<RefCell<Stage>>, sessions: &crate::session::Sessions| {
        let mut g = stage.borrow_mut();
        g.chat_pos = sessions.pos_label();
        g.chat_msgs = sessions
            .current()
            .msgs
            .iter()
            .map(|m| (m.role.clone(), m.text.clone()))
            .collect();
    };
    sync_sessions(&stage, &sessions);

    let app = NSApplication::sharedApplication(mtm);
    // Accessory: no Dock icon / menu bar — it's a widget, not an app window.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let win_w = W as f64 * scale;
    let win_h = H as f64 * scale;

    // bottom-right of the main screen, with a margin
    let (ox, oy) = if let Some(screen) = NSScreen::mainScreen(mtm) {
        let vf = screen.visibleFrame();
        (vf.origin.x + vf.size.width - win_w - 40.0, vf.origin.y + 40.0)
    } else {
        (200.0, 200.0)
    };

    let content = NSRect::new(NSPoint::new(ox, oy), NSSize::new(win_w, win_h));
    let window: Retained<PocketWindow> = unsafe {
        msg_send_id![
            mtm.alloc::<PocketWindow>(),
            initWithContentRect: content,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::NSBackingStoreBuffered,
            defer: false,
        ]
    };
    unsafe {
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setLevel(FLOATING_LEVEL);
        window.setHasShadow(false);
        window.setMovableByWindowBackground(true);
        window.setIgnoresMouseEvents(false);
    }

    // layer-backed content view; nearest-neighbour scaling keeps pixels crisp.
    let view = window.contentView().ok_or_else(|| anyhow::anyhow!("no content view"))?;
    unsafe {
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            let nearest = NSString::from_str("nearest");
            layer.setMagnificationFilter(&nearest);
            layer.setContentsGravity(&NSString::from_str("resize"));
        }
    }

    unsafe {
        window.makeKeyAndOrderFront(None);
        window.orderFrontRegardless();
    }

    // start REAL screen capture; the monitor now mirrors the actual desktop.
    let cap = crate::capture::start(W as u32, H as u32, 800);
    stage.borrow_mut().live = true;

    brain.event(serde_json::json!({"t":"boot"}));

    let distant_past = unsafe { NSDate::distantPast() };
    let mut last = std::time::Instant::now();
    let mut front_acc = 0.0f64;
    let mut last_front = String::new();
    // drag / click bookkeeping
    let mut dragged = false;

    loop {
        // pump all pending events without blocking
        loop {
            let ev: Option<Retained<NSEvent>> = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&distant_past),
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            let Some(ev) = ev else { break };
            let ty = unsafe { ev.r#type() };
            // the keypad controller below the monitor drives everything (both
            // modes) — this replaces the right-click menu.
            if ty == NSEventType::LeftMouseDown {
                let loc = unsafe { ev.locationInWindow() };
                let fx = (loc.x / scale) as i32;
                let fy = (H as i32) - (loc.y / scale) as i32;
                if let Some(act) = crate::controller_hit(fx, fy) {
                    match act {
                        "chat" => {
                            let open = stage.borrow().chat_open;
                            stage.borrow_mut().chat_open = !open;
                            if !open {
                                unsafe { window.makeKeyAndOrderFront(None) };
                                app.activateIgnoringOtherApps(true);
                            }
                        }
                        "about" => {}
                        other => brain.event(serde_json::json!({"t":"menu","act":other})),
                    }
                    continue;
                }
            }
            // typing goes into the chatbox input (the window is key-capable)
            if stage.borrow().chat_open && ty == NSEventType::KeyDown {
                let key = unsafe { ev.keyCode() };
                match key {
                    53 => { stage.borrow_mut().chat_open = false; } // esc
                    51 => { stage.borrow_mut().chat_input.pop(); }  // delete
                    36 | 76 => { // return / enter → send
                        let text = stage.borrow().chat_input.trim().to_string();
                        if !text.is_empty() && !stage.borrow().chat_pending {
                            {
                                let mut g = stage.borrow_mut();
                                g.chat_input.clear();
                                g.stream_buf.clear();
                            }
                            sessions.append("user", &text);
                            sync_sessions(&stage, &sessions);
                            send_message(&stage, &chat_tx, sessions.cur_id().to_string(), text);
                        }
                    }
                    _ => {
                        if let Some(s) = unsafe { ev.characters() } {
                            for c in s.to_string().chars() {
                                if !c.is_control() {
                                    stage.borrow_mut().chat_input.push(c);
                                }
                            }
                        }
                    }
                }
                continue; // consume (no system beep)
            }
            // When the chatbox is open, left clicks drive its controls
            // (session log + input focus) here — this is where `sessions` lives.
            if stage.borrow().chat_open && ty == NSEventType::LeftMouseDown {
                let loc = unsafe { ev.locationInWindow() };
                let fx = (loc.x / scale) as i32;
                let fy = (H as i32) - (loc.y / scale) as i32;
                if (128..=150).contains(&fx) && (4..=18).contains(&fy) {
                    stage.borrow_mut().chat_open = false; // close
                } else if (8..=40).contains(&fx) && (4..=18).contains(&fy) {
                    sessions.new_session();
                    sync_sessions(&stage, &sessions); // +NEW
                } else if (42..=54).contains(&fx) && (4..=18).contains(&fy) {
                    sessions.prev();
                    sync_sessions(&stage, &sessions); // <
                } else if (78..=92).contains(&fx) && (4..=18).contains(&fy) {
                    sessions.next();
                    sync_sessions(&stage, &sessions); // >
                } else if (214..=236).contains(&fy) {
                    // focus the window so keystrokes land in the input bar
                    unsafe { window.makeKeyAndOrderFront(None) };
                    app.activateIgnoringOtherApps(true);
                }
                continue; // consume: don't drag the window while chatting
            }
            handle_event(&ev, &window, &stage, &brain, scale, &mut dragged);
            unsafe { app.sendEvent(&ev) };
        }

        // drain streamed chat updates → live @cat reply in the chatbox
        while let Ok(u) = chat_rx.try_recv() {
            match u {
                ChatUpdate::Delta(d) => stage.borrow_mut().stream_buf.push_str(&d),
                ChatUpdate::Tool(name) => {
                    let mut g = stage.borrow_mut();
                    if !g.stream_buf.is_empty() && !g.stream_buf.ends_with('\n') {
                        g.stream_buf.push('\n');
                    }
                    g.stream_buf.push_str(&format!("· {name}\n"));
                }
                ChatUpdate::Done => {
                    let reply = stage.borrow().stream_buf.trim().to_string();
                    {
                        let mut g = stage.borrow_mut();
                        g.stream_buf.clear();
                        g.chat_pending = false;
                        g.cat_state = "talk".into();
                        g.cad_hz = 14.0;
                    }
                    if !reply.is_empty() {
                        sessions.append("cat", &reply);
                    }
                    sync_sessions(&stage, &sessions);
                }
            }
        }

        let now = std::time::Instant::now();
        let dt = (now - last).as_secs_f64() * 1000.0;
        last = now;

        // pull the latest real screenshot (keep only the newest) + gate capture
        let mut newest = None;
        while let Ok(shot) = cap.rx.try_recv() {
            newest = Some(shot);
        }
        if let Some(shot) = newest {
            let mut g = stage.borrow_mut();
            g.shot_blank = shot.blank;
            g.shot = Some(shot.sprite);
        }
        cap.enabled.store(stage.borrow().observe, std::sync::atomic::Ordering::Relaxed);

        // frontmost app → drives ticks (privacy avert) + activity sequence
        front_acc += dt;
        if front_acc >= 700.0 {
            front_acc = 0.0;
            let app_name = frontmost_app();
            if !app_name.is_empty() && app_name != last_front {
                last_front = app_name.clone();
                stage.borrow_mut().front_app = app_name.clone();
                let sensitive = is_sensitive(&app_name);
                let blank = stage.borrow().shot_blank;
                append_sequence(&app_name, W as u32, H as u32, blank);
                if stage.borrow().observe {
                    brain.event(serde_json::json!({"t":"tick","scene":app_name,"safe":!sensitive}));
                }
            }
        }

        step(&stage, &brain, &sprites, dt.min(100.0));

        render(&mut fb, &font, &sprites, &stage.borrow(), true, &text);
        let img = cg_image(&fb);
        set_layer_image(&view, &img);

        std::thread::sleep(std::time::Duration::from_millis(33));
    }
}

/// Frontmost application's localized name (no special permission needed —
/// this is the app identity, not its window contents).
fn frontmost_app() -> String {
    unsafe {
        let ws = NSWorkspace::sharedWorkspace();
        if let Some(app) = ws.frontmostApplication() {
            if let Some(name) = app.localizedName() {
                return name.to_string();
            }
        }
    }
    String::new()
}

/// Apps whose mere frontness means "don't look" — the cat averts on these.
const SENSITIVE: &[&str] = &[
    "1Password", "Keychain Access", "Bitwarden", "Dashlane", "LastPass", "Proton Pass",
];
fn is_sensitive(app: &str) -> bool {
    SENSITIVE.iter().any(|s| app.eq_ignore_ascii_case(s))
}

/// Append a real activity-sequence record (cat-shaped) to
/// ~/.pocket-cat/sequences.jsonl. One observation per frontmost-app change.
fn append_sequence(app: &str, w: u32, h: u32, blank: bool) {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = format!("{home}/.pocket-cat");
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rec = serde_json::json!({
        "id": format!("seq-{ts}"),
        "ts": ts,
        "kind": "observation",
        "source": "pocket-cat",
        "app": app,
        "screen": { "w": w, "h": h, "captured": !blank },
    });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}/sequences.jsonl")) {
        let _ = f.write_all(format!("{}\n", rec).as_bytes());
    }
}

/// Send the typed message to cat-swarm and STREAM the reply back (SSE):
/// text deltas + tool calls arrive as ChatUpdate events. Input is typed
/// inline in the chatbox — no system dialog.
fn send_message(
    stage: &Rc<RefCell<Stage>>,
    tx: &std::sync::mpsc::Sender<ChatUpdate>,
    session_id: String,
    message: String,
) {
    use std::io::BufRead;
    let app = stage.borrow().front_app.clone();
    let sequences = recent_sequences(8);
    {
        let mut g = stage.borrow_mut();
        g.chat_pending = true;
        g.cat_state = "work".into();
        g.cad_hz = 14.0;
    }
    let tx = tx.clone();
    std::thread::spawn(move || {
        let url = std::env::var("POCKET_CAT_URL").unwrap_or_else(|_| "http://127.0.0.1:8848/chat".into());
        let body = serde_json::json!({ "message": message, "sessionId": session_id, "app": app, "sequences": sequences });
        match ureq::post(&url).timeout(std::time::Duration::from_secs(180)).send_json(body) {
            Ok(resp) => {
                let reader = std::io::BufReader::new(resp.into_reader());
                let mut ev = String::new();
                for line in reader.lines().map_while(Result::ok) {
                    if let Some(e) = line.strip_prefix("event: ") {
                        ev = e.to_string();
                    } else if let Some(d) = line.strip_prefix("data: ") {
                        let v: serde_json::Value = serde_json::from_str(d).unwrap_or_default();
                        match ev.as_str() {
                            "text" => {
                                if let Some(s) = v["delta"].as_str() {
                                    let _ = tx.send(ChatUpdate::Delta(s.to_string()));
                                }
                            }
                            "tool" => {
                                if let Some(s) = v["name"].as_str() {
                                    let _ = tx.send(ChatUpdate::Tool(s.to_string()));
                                }
                            }
                            "done" => {
                                let _ = tx.send(ChatUpdate::Done);
                            }
                            "error" => {
                                let _ = tx.send(ChatUpdate::Delta(format!("(error: {})", v["message"].as_str().unwrap_or(""))));
                                let _ = tx.send(ChatUpdate::Done);
                            }
                            _ => {}
                        }
                    }
                }
                let _ = tx.send(ChatUpdate::Done); // ensure completion
            }
            Err(_) => {
                let _ = tx.send(ChatUpdate::Delta("(cat-swarm not reachable — run `npx tsx examples/cat-swarm.ts`)".into()));
                let _ = tx.send(ChatUpdate::Done);
            }
        }
    });
}

fn recent_sequences(n: usize) -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = format!("{home}/.pocket-cat/sequences.jsonl");
    let Ok(content) = std::fs::read_to_string(path) else { return vec![] };
    content
        .lines()
        .rev()
        .take(n)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| v["app"].as_str().unwrap_or("?").to_string())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn handle_event(
    ev: &NSEvent,
    window: &NSWindow,
    stage: &Rc<RefCell<Stage>>,
    brain: &Brain,
    scale: f64,
    dragged: &mut bool,
) {
    let ty = unsafe { ev.r#type() };
    // window point (origin bottom-left) → framebuffer pixel (origin top-left)
    let loc = unsafe { ev.locationInWindow() };
    let fx = (loc.x / scale) as i32;
    let fy = (H as i32) - (loc.y / scale) as i32;

    match ty {
        NSEventType::LeftMouseDown => {
            *dragged = false;
        }
        NSEventType::LeftMouseDragged => {
            *dragged = true;
            // drag the widget around the desktop
            let dx = unsafe { ev.deltaX() };
            let dy = unsafe { ev.deltaY() };
            let frame = window.frame();
            let origin = NSPoint::new(frame.origin.x + dx, frame.origin.y - dy);
            unsafe { window.setFrameOrigin(origin) };
        }
        NSEventType::LeftMouseUp => {
            // a click (not a drag) on the cat → pet it
            if !*dragged && !stage.borrow().chat_open && (45..128).contains(&fy) {
                brain.event(serde_json::json!({"t":"pet"}));
            }
        }
        _ => {}
    }
}
