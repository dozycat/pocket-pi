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

    // @pb chat replies come back from a worker thread over this channel.
    let (chat_tx, chat_rx) = std::sync::mpsc::channel::<String>();

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
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            content,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::NSBackingStoreBuffered,
            false,
        )
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
            handle_event(&ev, &window, &stage, &brain, scale, &mut dragged, &chat_tx);
            unsafe { app.sendEvent(&ev) };
        }

        // a chat reply arrived → show it, cat talks
        if let Ok(reply) = chat_rx.try_recv() {
            let mut g = stage.borrow_mut();
            g.chat_pending = false;
            g.chat_reply = reply;
            g.chat_reply_until = g.clock_ms + 14000.0;
            g.cat_state = "talk".into();
            g.cad_hz = 14.0;
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

/// Append a real activity-sequence record (paperboy-shaped) to
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

/// The @pb chatbox: a native input dialog (osascript — supports IME/CJK),
/// then POST {message, app, sequences} to the pb-bridge (the ported
/// paperboy-chat agent) and hand the reply back to the render loop.
fn start_chat(stage: &Rc<RefCell<Stage>>, tx: &std::sync::mpsc::Sender<String>) {
    if stage.borrow().chat_pending {
        return;
    }
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
        // native text input (owns IME, so Chinese input works)
        let out = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg("display dialog \"跟 @pb 说：\" default answer \"\" with title \"Pocket Cat\" buttons {\"取消\",\"发送\"} default button \"发送\"")
            .output();
        let message = match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.split("text returned:").nth(1).map(|t| t.trim().to_string()).unwrap_or_default()
            }
            _ => String::new(), // cancelled
        };
        if message.is_empty() {
            let _ = tx.send(String::new()); // clears pending, no reply
            return;
        }
        let url = std::env::var("POCKET_CAT_PB_URL").unwrap_or_else(|_| "http://127.0.0.1:8848/chat".into());
        let body = serde_json::json!({ "message": message, "app": app, "sequences": sequences });
        let reply = match ureq::post(&url).timeout(std::time::Duration::from_secs(60)).send_json(body) {
            Ok(resp) => resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["reply"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "(空回复)".into()),
            Err(_) => "(pb-bridge 没连上 — 先跑 npx tsx examples/pb-bridge.ts)".into(),
        };
        // log the exchange
        append_chat(&message, &reply);
        let _ = tx.send(reply);
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

fn append_chat(message: &str, reply: &str) {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = format!("{home}/.pocket-cat");
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rec = serde_json::json!({ "ts": ts, "message": message, "reply": reply });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}/chat.jsonl")) {
        let _ = f.write_all(format!("{}\n", rec).as_bytes());
    }
}

fn handle_event(
    ev: &NSEvent,
    window: &NSWindow,
    stage: &Rc<RefCell<Stage>>,
    brain: &Brain,
    scale: f64,
    dragged: &mut bool,
    chat_tx: &std::sync::mpsc::Sender<String>,
) {
    let ty = unsafe { ev.r#type() };
    // window point (origin bottom-left) → framebuffer pixel (origin top-left)
    let loc = unsafe { ev.locationInWindow() };
    let fx = (loc.x / scale) as i32;
    let fy = (H as i32) - (loc.y / scale) as i32;

    match ty {
        NSEventType::RightMouseDown => {
            let mut g = stage.borrow_mut();
            g.menu_open = true;
            g.menu_x = fx.min(W as i32 - 96);
            g.menu_y = fy.min(H as i32 - 76);
        }
        NSEventType::LeftMouseDown => {
            *dragged = false;
            let open = stage.borrow().menu_open;
            if open {
                let hit = crate::menu_hit(&stage.borrow(), fx, fy);
                stage.borrow_mut().menu_open = false;
                if let Some(act) = hit {
                    if act == "chat" {
                        start_chat(stage, chat_tx);
                    } else if act != "about" {
                        brain.event(serde_json::json!({"t":"menu","act":act}));
                    }
                }
            }
        }
        NSEventType::LeftMouseDragged => {
            *dragged = true;
            // move the window with the cursor
            let dx = unsafe { ev.deltaX() };
            let dy = unsafe { ev.deltaY() };
            let frame = window.frame();
            let origin = NSPoint::new(frame.origin.x + dx, frame.origin.y - dy);
            unsafe { window.setFrameOrigin(origin) };
        }
        NSEventType::LeftMouseUp => {
            if !*dragged && !stage.borrow().menu_open {
                // a click (not a drag) on the cat → pet it
                if fx >= 150 && fy >= 62 {
                    brain.event(serde_json::json!({"t":"pet"}));
                }
            }
        }
        _ => {}
    }
}
