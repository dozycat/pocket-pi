//! pocket-cat — native macOS pixel desktop-pet on the Pocket runtime family.
//!
//! The `stage` surface: the Rust core owns the native window (minifb), the
//! software framebuffer, the clock, input, and scene rotation; a QuickJS
//! guest (`cat-brain.js`, the cat harness policy) owns reactions, the
//! privacy judgement, and commands. Host → guest via __cat_event; guest →
//! host via the mounted `cat` ops. No Electron, no bun, no Python.
//!
//!   pocket-cat                       open the widget window
//!   pocket-cat --capture <dir>       headless: render key states to PNGs
//!   pocket-cat --scale N             window pixel scale (default 3)

mod assets;
mod capture;
mod fb;
mod sprites;
#[cfg(target_os = "macos")]
mod mac_widget;

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use rquickjs::{CatchResultExt, Context, Function, Object, Runtime};

use fb::{rgb, rgba, Argb, Font, Framebuffer};
use sprites::Sprites;

const W: usize = 300;
const H: usize = 150;

// ── observed scenes (示意 mirror of what the agent watches) ────────────────
struct Scene {
    name: &'static str,
    safe: bool,
    dur_ms: f64,
}
const SCENES: &[Scene] = &[
    Scene { name: "CODE", safe: true, dur_ms: 2600.0 },
    Scene { name: "BROWSER", safe: true, dur_ms: 2600.0 },
    Scene { name: "TERMINAL", safe: true, dur_ms: 2200.0 },
    Scene { name: "LOGIN", safe: false, dur_ms: 2400.0 },
    Scene { name: "CHAT", safe: true, dur_ms: 2200.0 },
    Scene { name: "BANK", safe: false, dur_ms: 2400.0 },
    Scene { name: "DOCS", safe: true, dur_ms: 2600.0 },
    Scene { name: "DM", safe: false, dur_ms: 2400.0 },
];

struct Browse {
    t: f64,
    x: f64,
    y: f64,
    tx: f64,
    ty: f64,
    step: i32,
    at: f64,
    click: f64,
}

struct Stage {
    // clock (host-driven so --capture is deterministic)
    clock_ms: f64,
    // render state written by the `cat` ops + host
    cat_state: String,
    frame: usize,
    anim_acc: f64,
    fx: String,
    fx_frame: usize,
    fx_acc: f64,
    observe: bool,
    privacy: bool,
    averting: bool,
    caption: String,
    caption_until: f64,
    cad_hz: f64,
    // host-owned
    scene_i: usize,
    scene_acc: f64,
    browse: Option<Browse>,
    // ui
    menu_open: bool,
    menu_x: i32,
    menu_y: i32,
    cmd: Option<String>,
    // request the host to kick off a browse scene (set by cat.browse op)
    want_browse: bool,
    // live mode: the monitor shows a REAL screen capture instead of synthetic
    // scenes; scene rotation is off and ticks come from frontmost-app changes.
    live: bool,
    shot: Option<fb::Sprite>,
    shot_blank: bool,
    front_app: String,
}

impl Stage {
    fn new() -> Rc<RefCell<Stage>> {
        Rc::new(RefCell::new(Stage {
            clock_ms: 0.0,
            cat_state: "idle".into(),
            frame: 0,
            anim_acc: 0.0,
            fx: "none".into(),
            fx_frame: 0,
            fx_acc: 0.0,
            observe: true,
            privacy: true,
            averting: false,
            caption: String::new(),
            caption_until: 0.0,
            cad_hz: 2.0,
            scene_i: 0,
            scene_acc: 0.0,
            browse: None,
            menu_open: false,
            menu_x: 0,
            menu_y: 0,
            cmd: None,
            want_browse: false,
            live: false,
            shot: None,
            shot_blank: false,
            front_app: String::new(),
        }))
    }
    fn scene(&self) -> &'static Scene {
        &SCENES[self.scene_i % SCENES.len()]
    }
}

fn state_fps(s: &str) -> f64 {
    match s {
        "work" => 9.0,
        "talk" => 6.0,
        "sleep" => 4.0,
        "excited" => 12.0,
        "jump" => 6.0,
        _ => 2.0,
    }
}

// ── the QuickJS brain ──────────────────────────────────────────────────────
struct Brain {
    _rt: Runtime,
    ctx: Context,
    stage: Rc<RefCell<Stage>>,
}

impl Brain {
    fn new(stage: Rc<RefCell<Stage>>) -> Result<Brain> {
        let rt = Runtime::new()?;
        let ctx = Context::full(&rt)?;
        let st = stage.clone();
        ctx.with(|ctx| -> rquickjs::Result<()> {
            // console.log → stdout (brain diagnostics)
            let console = Object::new(ctx.clone())?;
            console.set(
                "log",
                Function::new(ctx.clone(), |args: rquickjs::function::Rest<rquickjs::Value>| {
                    let mut s = String::new();
                    for v in args.iter() {
                        if let Some(x) = v.as_string() {
                            s.push_str(&x.to_string().unwrap_or_default());
                        }
                        s.push(' ');
                    }
                    println!("[brain] {}", s.trim());
                })?,
            )?;
            ctx.globals().set("console", console)?;

            // __now() — host clock (ms), so setTimeout in the guest is exact.
            let s1 = st.clone();
            ctx.globals().set(
                "__now",
                Function::new(ctx.clone(), move || -> f64 { s1.borrow().clock_ms })?,
            )?;

            // the `cat` surface — guest → host intents.
            let cat = Object::new(ctx.clone())?;
            macro_rules! op {
                ($n:literal, $f:expr) => {
                    cat.set($n, Function::new(ctx.clone(), $f)?)?;
                };
            }
            let s = st.clone();
            op!("state", move |name: String| {
                let mut g = s.borrow_mut();
                if g.cat_state != name {
                    g.cat_state = name;
                    g.frame = 0;
                    g.anim_acc = 0.0;
                }
            });
            let s = st.clone();
            op!("say", move |t: String, ms: f64| {
                let mut g = s.borrow_mut();
                let now = g.clock_ms;
                g.caption = t;
                g.caption_until = now + ms;
            });
            let s = st.clone();
            op!("observe", move |b: bool| s.borrow_mut().observe = b);
            let s = st.clone();
            op!("privacy", move |b: bool| s.borrow_mut().privacy = b);
            let s = st.clone();
            op!("avert", move |b: bool| s.borrow_mut().averting = b);
            let s = st.clone();
            op!("fx", move |k: String| {
                let mut g = s.borrow_mut();
                if g.fx != k {
                    g.fx = k;
                    g.fx_frame = 0;
                }
            });
            let s = st.clone();
            op!("cad", move |hz: f64| s.borrow_mut().cad_hz = hz);
            let s = st.clone();
            op!("browse", move || s.borrow_mut().want_browse = true);
            ctx.globals().set("cat", cat)?;
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("mount cat surface: {e}"))?;

        // timer prelude (setTimeout over the host clock) + the brain policy.
        let prelude = r#"
            var __timers=[], __tid=1;
            globalThis.setTimeout=function(fn,ms){var id=__tid++;__timers.push({id:id,at:__now()+(ms||0),fn:fn});return id;};
            globalThis.clearTimeout=function(id){__timers=__timers.filter(function(t){return t.id!==id;});};
            globalThis.__drain=function(now){
              var due=__timers.filter(function(t){return t.at<=now;});
              __timers=__timers.filter(function(t){return t.at>now;});
              due.sort(function(a,b){return a.at-b.at;});
              for(var i=0;i<due.length;i++){try{due[i].fn();}catch(e){}}
            };
        "#;
        let src = format!("{}\n{}", prelude, include_str!("cat-brain.js"));
        ctx.with(|ctx| -> Result<()> {
            ctx.eval::<(), _>(src.as_bytes())
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("eval cat-brain: {e}"))?;
            Ok(())
        })?;
        Ok(Brain { _rt: rt, ctx, stage })
    }

    fn call(&self, name: &str, arg: String) {
        self.ctx.with(|ctx| {
            if let Ok(f) = ctx.globals().get::<_, Function>(name) {
                if let Err(e) = f.call::<_, ()>((arg,)).catch(&ctx) {
                    eprintln!("pocket-cat: {name} threw: {e}");
                }
            }
        });
    }
    fn event(&self, v: serde_json::Value) {
        self.call("__cat_event", v.to_string());
    }
    fn drain(&self) {
        let now = self.stage.borrow().clock_ms;
        self.ctx.with(|ctx| {
            if let Ok(f) = ctx.globals().get::<_, Function>("__drain") {
                let _ = f.call::<_, ()>((now,)).catch(&ctx);
            }
        });
    }
}

// ── the pump: one logical step of `dt` ms ──────────────────────────────────
fn step(stage: &Rc<RefCell<Stage>>, brain: &Brain, sprites: &Sprites, dt: f64) {
    {
        let mut g = stage.borrow_mut();
        g.clock_ms += dt;
    }
    // host owns the browse-scene animation; hand control back to the brain when done.
    let (browsing, want_browse) = {
        let g = stage.borrow();
        (g.browse.is_some(), g.want_browse)
    };
    if want_browse && !browsing {
        let mut g = stage.borrow_mut();
        g.want_browse = false;
        g.browse = Some(Browse { t: 0.0, x: 14.0, y: 60.0, tx: 90.0, ty: 20.0, step: 0, at: 0.0, click: 0.0 });
    }
    if browsing {
        let mut done = false;
        {
            let mut g = stage.borrow_mut();
            let clock = g.clock_ms;
            if let Some(b) = g.browse.as_mut() {
                b.t += dt;
                b.x += (b.tx - b.x) * 0.2;
                b.y += (b.ty - b.y) * 0.2;
                if (b.x - b.tx).abs() < 2.0 && b.t - b.at > 500.0 {
                    b.at = b.t;
                    b.click = 8.0;
                    b.step += 1;
                    let targets = [(90.0, 20.0), (70.0, 38.0), (90.0, 54.0), (60.0, 54.0)];
                    let idx = (b.step as usize).min(3);
                    b.tx = targets[idx].0;
                    b.ty = targets[idx].1;
                    if b.step >= 3 {
                        done = true;
                    }
                }
                if b.click > 0.0 {
                    b.click -= 1.0;
                }
                let _ = clock;
            }
        }
        if done {
            stage.borrow_mut().browse = None;
            brain.event(serde_json::json!({"t":"browse_done"}));
        }
    } else {
        // scene rotation (only while observing) → tick the brain on each change.
        // In live mode the real frontmost-app changes drive ticks instead.
        let advance = {
            let mut g = stage.borrow_mut();
            if g.observe && !g.live {
                g.scene_acc += dt;
                let dur = g.scene().dur_ms;
                if g.scene_acc >= dur {
                    g.scene_acc = 0.0;
                    g.scene_i = (g.scene_i + 1) % SCENES.len();
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if advance {
            let (name, safe) = {
                let g = stage.borrow();
                (g.scene().name, g.scene().safe)
            };
            brain.event(serde_json::json!({"t":"tick","scene":name,"safe":safe}));
        }
    }
    brain.drain();
    // advance cat animation
    {
        let mut g = stage.borrow_mut();
        let fps = if g.cad_hz >= 10.0 { state_fps(&g.cat_state).max(10.0) } else { state_fps(&g.cat_state) };
        g.anim_acc += dt;
        if g.anim_acc >= 1000.0 / fps {
            g.anim_acc = 0.0;
            let n = sprites.group(&g.cat_state).len().max(1);
            g.frame = (g.frame + 1) % n;
        }
        if g.fx != "none" {
            g.fx_acc += dt;
            if g.fx_acc >= 220.0 {
                g.fx_acc = 0.0;
                let n = sprites.group(&g.fx).len().max(1);
                g.fx_frame = (g.fx_frame + 1) % n;
            }
        }
    }
}

// ── render ─────────────────────────────────────────────────────────────────
const C_DESK: Argb = rgb(0x26, 0x20, 0x18);
const C_BODY: Argb = rgb(0x2b, 0x26, 0x20);
const C_BODY2: Argb = rgb(0x17, 0x14, 0x10);
const C_INK: Argb = rgb(0x0a, 0x08, 0x06);
const C_SCREEN: Argb = rgb(0x08, 0x12, 0x0a);
const C_GLOW: Argb = rgb(0x86, 0xef, 0xac);
const C_GLOW2: Argb = rgb(0x3f, 0xae, 0x6a);
const C_AMBER: Argb = rgb(0xe0, 0xb0, 0x4a);
const C_HEART: Argb = rgb(0xec, 0x7c, 0x7c);
const C_PAPER: Argb = rgb(0xff, 0xfa, 0xf0);
const C_ORANGE: Argb = rgb(0xf0, 0x91, 0x2f);
const C_ORANGE2: Argb = rgb(0xb9, 0x56, 0x0e);

const SX: i32 = 20; // screen origin
const SY: i32 = 30;
const SW: i32 = 116;
const SH: i32 = 74;

fn render(fb: &mut Framebuffer, font: &Font, sprites: &Sprites, stage: &Stage, transparent: bool) {
    if transparent {
        // windowless widget: only the monitor + cat + bubbles are opaque; the
        // rest is fully transparent so the pet floats on the desktop.
        fb.clear(0x0000_0000);
    } else {
        fb.clear(C_DESK);
        fb.rect(0, H as i32 - 14, W as i32, 3, rgba(0x8a, 0x63, 0x30, 120));
    }

    // ── monitor ──
    fb.rect(8, 18, 140, 104, C_BODY);
    fb.frame_rect(8, 18, 140, 104, 3, C_INK);
    // recessed screen
    fb.rect(SX - 3, SY - 3, SW + 6, SH + 6, C_INK);
    fb.rect(SX, SY, SW, SH, C_SCREEN);
    draw_screen(fb, font, stage);
    // scanlines
    let mut y = SY;
    while y < SY + SH {
        fb.rect(SX, y, SW, 1, rgba(0, 0, 0, 40));
        y += 3;
    }
    // brand + LED
    fb.text(font, "DOZY-CRT", 14, 110, C_GLOW2, 1);
    let led = if stage.observe { C_GLOW } else { rgb(0x5a, 0x2a, 0x2a) };
    fb.rect(138, 110, 6, 6, led);
    // neck + base
    fb.rect(70, 122, 16, 6, C_BODY2);
    fb.rect(58, 128, 40, 5, C_BODY);
    fb.frame_rect(58, 128, 40, 5, 2, C_INK);

    // cadence label
    let cad = if !stage.observe {
        "OBS 0 STANDBY".to_string()
    } else {
        let hz = if stage.cad_hz >= 10.0 { 14 } else { 2 };
        format!("OBS {}FPS COALESCED", hz)
    };
    fb.text(font, &cad, 8, 6, rgb(0xcb, 0xb0, 0x88), 1);

    // ── cat ──  faces LEFT (toward the monitor) normally; turns away to avert
    let cs = sprites.group(&stage.cat_state);
    if !cs.is_empty() {
        let sp = &cs[stage.frame % cs.len()];
        let cx = 150;
        let cy = 62;
        fb.blit(sp, cx, cy, 1, !stage.averting);
        // cover-face paws when averting
        if stage.averting {
            let px = cx + 40;
            let py = cy + 40;
            for (i, dx) in [0i32, 22].iter().enumerate() {
                let _ = i;
                fb.rect(px + dx, py, 20, 22, C_ORANGE);
                fb.frame_rect(px + dx, py, 20, 22, 2, C_ORANGE2);
                fb.rect(px + dx + 5, py + 3, 2, 7, C_ORANGE2);
                fb.rect(px + dx + 11, py + 3, 2, 7, C_ORANGE2);
            }
        }
        // fx above the head
        if stage.fx != "none" {
            if let Some(f) = sprites.frame(&stage.fx, stage.fx_frame) {
                fb.blit(f, cx + 44, cy - 18, 1, false);
            }
        }
    }

    // caption bubble (latin) above the cat
    if !stage.caption.is_empty() && stage.clock_ms < stage.caption_until {
        let tw = fb.text_w(&stage.caption, 1);
        let bx = 168;
        let by = 30;
        fb.rect(bx, by, tw + 12, 16, C_PAPER);
        fb.frame_rect(bx, by, tw + 12, 16, 2, C_INK);
        fb.text(font, &stage.caption, bx + 6, by + 5, rgb(0x3a, 0x26, 0x14), 1);
        fb.rect(bx + 10, by + 16, 6, 5, C_PAPER);
    }

    // context menu
    if stage.menu_open {
        draw_menu(fb, font, stage);
    }
    // command input line
    if let Some(cmd) = &stage.cmd {
        fb.rect(8, H as i32 - 12, W as i32 - 16, 12, C_PAPER);
        fb.frame_rect(8, H as i32 - 12, W as i32 - 16, 12, 2, C_INK);
        let shown = format!("> {}_", cmd);
        fb.text(font, &shown, 12, H as i32 - 9, rgb(0x3a, 0x26, 0x14), 1);
    }
}

const MENU: &[(&str, &str)] = &[
    ("observe", "OBSERVE"),
    ("privacy", "PRIVACY"),
    ("browse", "BROWSE"),
    ("nap", "NAP"),
    ("about", "ABOUT"),
];

fn draw_menu(fb: &mut Framebuffer, font: &Font, stage: &Stage) {
    let x = stage.menu_x;
    let y = stage.menu_y;
    let w = 92;
    let h = MENU.len() as i32 * 13 + 6;
    fb.rect(x, y, w, h, C_PAPER);
    fb.frame_rect(x, y, w, h, 2, C_INK);
    for (i, (act, label)) in MENU.iter().enumerate() {
        let iy = y + 4 + i as i32 * 13;
        fb.text(font, label, x + 8, iy + 2, rgb(0x3a, 0x26, 0x14), 1);
        let st = match *act {
            "observe" => Some(stage.observe),
            "privacy" => Some(stage.privacy),
            _ => None,
        };
        if let Some(on) = st {
            fb.rect(x + w - 14, iy + 1, 8, 8, if on { C_GLOW2 } else { rgb(0xd8, 0xc6, 0xa0) });
            fb.frame_rect(x + w - 14, iy + 1, 8, 8, 1, C_INK);
        }
    }
}

fn draw_screen(fb: &mut Framebuffer, font: &Font, stage: &Stage) {
    if !stage.observe {
        fb.text(font, "- STANDBY -", SX + 24, SY + SH / 2, C_GLOW2, 1);
        return;
    }
    if let Some(b) = &stage.browse {
        draw_browse(fb, font, b);
        return;
    }
    if stage.averting {
        draw_censored(fb, font);
        return;
    }
    // ── live mode: show the real screen capture ──
    if stage.live {
        match &stage.shot {
            Some(shot) if !stage.shot_blank => {
                // blit the downscaled screenshot to fill the screen region
                let sx = SW as f32 / shot.w.max(1) as f32;
                let sy = SH as f32 / shot.h.max(1) as f32;
                for y in 0..SH {
                    for x in 0..SW {
                        let px = (x as f32 / sx) as u32;
                        let py = (y as f32 / sy) as u32;
                        let i = (py.min(shot.h - 1) * shot.w + px.min(shot.w - 1)) as usize;
                        if let Some(&c) = shot.px.get(i) {
                            fb.put(SX + x, SY + y, c);
                        }
                    }
                }
                // LIVE badge + frontmost app
                fb.rect(SX, SY, SW, 9, rgba(0x0e, 0x1f, 0x14, 210));
                fb.rect(SX + 3, SY + 3, 4, 3, C_HEART);
                fb.text(font, "LIVE", SX + 10, SY + 2, C_GLOW, 1);
                if !stage.front_app.is_empty() {
                    let up = stage.front_app.to_uppercase();
                    let t: String = up.chars().take(16).collect();
                    fb.text(font, &t, SX + 34, SY + 2, C_AMBER, 1);
                }
                return;
            }
            Some(_) => {
                // blank frame → needs Screen Recording permission
                fb.text(font, "GRANT SCREEN", SX + 18, SY + SH / 2 - 6, C_AMBER, 1);
                fb.text(font, "RECORDING", SX + 24, SY + SH / 2 + 4, C_AMBER, 1);
                return;
            }
            None => {
                fb.text(font, "CAPTURING...", SX + 20, SY + SH / 2, C_GLOW2, 1);
                return;
            }
        }
    }
    let sc = stage.scene();
    // title bar
    fb.rect(SX, SY, SW, 10, rgb(0x0e, 0x1f, 0x14));
    fb.rect(SX + 3, SY + 3, 6, 4, C_GLOW);
    fb.text(font, sc.name, SX + 22, SY + 2, C_GLOW, 1);
    match sc.name {
        "CODE" => {
            for i in 0..8 {
                let w = 24 + (i * 29) % 70;
                fb.rect(SX + 6, SY + 14 + i * 7, 3, 3, C_GLOW2);
                fb.rect(SX + 12, SY + 14 + i * 7, w, 3, if i % 3 == 0 { C_AMBER } else { rgb(0x2a, 0x5a, 0x3a) });
            }
        }
        "BROWSER" | "DOCS" => {
            fb.rect(SX + 6, SY + 13, SW - 12, 8, rgb(0x09, 0x14, 0x0d));
            fb.text(font, if sc.name == "DOCS" { "POCKET-PI/README" } else { "GITHUB.COM" }, SX + 9, SY + 15, C_GLOW, 1);
            let rows = ["SPEC/", "HOST/", "SDK/", "MIT LICENSE"];
            for (i, r) in rows.iter().enumerate() {
                let iy = SY + 25 + i as i32 * 11;
                fb.rect(SX + 6, iy, SW - 12, 8, if i % 2 == 0 { rgb(0x12, 0x28, 0x1b) } else { rgb(0x0f, 0x1f, 0x15) });
                fb.text(font, r, SX + 9, iy + 1, C_GLOW, 1);
            }
        }
        "TERMINAL" => {
            fb.text(font, "$ CARGO BUILD", SX + 6, SY + 16, C_GLOW, 1);
            fb.text(font, "  FINISHED", SX + 6, SY + 27, C_GLOW2, 1);
            fb.text(font, "$ POCKET-CAT", SX + 6, SY + 42, C_GLOW, 1);
            let blink = ((stage.clock_ms / 400.0) as i64) % 2 == 0;
            if blink {
                fb.rect(SX + 6, SY + 52, 4, 7, C_GLOW);
            }
        }
        "CHAT" => {
            fb.rect(SX + 6, SY + 14, 74, 9, rgb(0x12, 0x28, 0x1b));
            fb.text(font, "KAI: STATUS?", SX + 9, SY + 16, C_GLOW, 1);
            fb.rect(SX + SW - 80, SY + 27, 74, 9, rgb(0x17, 0x3a, 0x24));
            fb.text(font, "CAT: 3 PR DONE", SX + SW - 77, SY + 29, C_AMBER, 1);
        }
        "LOGIN" => draw_sensitive(fb, font, "LOGIN - PASSWORD", &["USER@DOZY.IO", "........."]),
        "BANK" => draw_sensitive(fb, font, "ACCOUNT BALANCE", &["BAL 128402.55", "CARD .... 7781"]),
        "DM" => draw_sensitive(fb, font, "PRIVATE MESSAGE", &["MOM: CALL ME", "(PRIVATE)"]),
        _ => {}
    }
}

fn draw_sensitive(fb: &mut Framebuffer, font: &Font, title: &str, lines: &[&str]) {
    fb.rect(SX + 4, SY + 12, SW - 8, SH - 16, rgb(0x1a, 0x13, 0x10));
    fb.text(font, title, SX + 8, SY + 16, rgb(0xee, 0x88, 0x88), 1);
    for (i, l) in lines.iter().enumerate() {
        fb.text(font, l, SX + 10, SY + 30 + i as i32 * 12, rgb(0xd5, 0xc0, 0xa0), 1);
    }
}

fn draw_censored(fb: &mut Framebuffer, font: &Font) {
    fb.rect(SX, SY, SW, SH, rgb(0x0a, 0x0f, 0x0b));
    let mut y = SY + 4;
    while y < SY + SH - 4 {
        let mut x = SX + 4;
        while x < SX + SW - 4 {
            let c = if (x + y) % 16 == 0 { rgb(0x14, 0x25, 0x1a) } else { rgb(0x0e, 0x1b, 0x12) };
            fb.rect(x, y, 7, 7, c);
            x += 8;
        }
        y += 8;
    }
    // lock glyph
    let cx = SX + SW / 2 - 6;
    let cy = SY + SH / 2 - 6;
    fb.rect(cx, cy, 14, 10, C_GLOW);
    fb.frame_rect(cx + 3, cy - 6, 8, 8, 2, C_GLOW);
    fb.rect(cx + 5, cy + 3, 3, 4, rgb(0x0a, 0x0f, 0x0b));
    fb.text(font, "PRIVACY", SX + SW / 2 - 18, SY + SH - 10, C_GLOW, 1);
}

fn draw_browse(fb: &mut Framebuffer, font: &Font, b: &Browse) {
    fb.rect(SX, SY, SW, 10, rgb(0x0e, 0x1f, 0x14));
    fb.text(font, "BROWSER-USE", SX + 4, SY + 2, C_GLOW, 1);
    fb.rect(SX + 4, SY + 12, SW - 8, 8, rgb(0x09, 0x14, 0x0d));
    fb.text(font, "GITHUB/POCKET-PI", SX + 6, SY + 14, C_GLOW, 1);
    let rows = ["OPEN POCKET-STACK", "ENTER POCKET-PI", "CLICK STAR", "STARRED"];
    for (i, r) in rows.iter().enumerate() {
        let on = i as i32 <= b.step;
        let iy = SY + 24 + i as i32 * 11;
        fb.rect(SX + 4, iy, SW - 8, 9, if on { rgb(0x17, 0x32, 0x22) } else { rgb(0x0f, 0x1f, 0x15) });
        if on {
            fb.text(font, r, SX + 7, iy + 1, C_AMBER, 1);
        }
    }
    if b.click > 0.0 {
        let r = ((8.0 - b.click) * 2.0) as i32;
        fb.frame_rect(SX + b.x as i32 - r, SY + b.y as i32 - r, r * 2, r * 2, 1, C_GLOW);
    }
    // pixel cursor
    let x = SX + b.x as i32;
    let y = SY + b.y as i32;
    fb.rect(x, y, 2, 8, C_PAPER);
    fb.rect(x + 2, y + 2, 2, 5, C_PAPER);
    fb.rect(x + 4, y + 5, 2, 4, C_PAPER);
}

// ── capture (headless verification) ────────────────────────────────────────
fn write_png(path: &str, fb: &Framebuffer) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), fb.w as u32, fb.h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&fb.to_rgba8())?;
    Ok(())
}

fn run_capture(dir: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let stage = Stage::new();
    let brain = Brain::new(stage.clone())?;
    let sprites = Sprites::load();
    let font = Font::build();
    let mut fb = Framebuffer::new(W, H);

    let advance = |stage: &Rc<RefCell<Stage>>, brain: &Brain, ms: f64| {
        let mut left = ms;
        while left > 0.0 {
            step(stage, brain, &sprites, 50.0);
            left -= 50.0;
        }
    };

    // Freeze a scene deterministically: pin the index, zero its timer so the
    // rotation can't advance past it, tick the brain, settle briefly.
    let freeze = |stage: &Rc<RefCell<Stage>>, brain: &Brain, i: usize| {
        {
            let mut g = stage.borrow_mut();
            g.scene_i = i;
            g.scene_acc = 0.0;
        }
        let (name, safe) = {
            let g = stage.borrow();
            (g.scene().name, g.scene().safe)
        };
        brain.event(serde_json::json!({"t":"tick","scene":name,"safe":safe}));
        advance(stage, brain, 200.0);
    };

    brain.event(serde_json::json!({"t":"boot"}));
    advance(&stage, &brain, 2000.0);
    freeze(&stage, &brain, 0); // CODE (safe)
    render(&mut fb, &font, &sprites, &stage.borrow(), false);
    write_png(&format!("{dir}/1-watch.png"), &fb)?;

    freeze(&stage, &brain, 3); // LOGIN (sensitive) → avert + censor
    render(&mut fb, &font, &sprites, &stage.borrow(), false);
    write_png(&format!("{dir}/2-privacy.png"), &fb)?;

    // browser-use — snapshot mid-animation
    brain.event(serde_json::json!({"t":"menu","act":"browse"}));
    advance(&stage, &brain, 1200.0);
    render(&mut fb, &font, &sprites, &stage.borrow(), false);
    write_png(&format!("{dir}/3-browse.png"), &fb)?;
    // let it finish and the post-browse reaction settle before the next state
    advance(&stage, &brain, 3200.0);

    // menu open snapshot (over a safe scene)
    freeze(&stage, &brain, 0);
    {
        let mut g = stage.borrow_mut();
        g.menu_open = true;
        g.menu_x = 120;
        g.menu_y = 40;
    }
    render(&mut fb, &font, &sprites, &stage.borrow(), false);
    write_png(&format!("{dir}/4-menu.png"), &fb)?;

    // nap — clean, nothing pending
    {
        stage.borrow_mut().menu_open = false;
    }
    brain.event(serde_json::json!({"t":"menu","act":"nap"}));
    advance(&stage, &brain, 800.0);
    render(&mut fb, &font, &sprites, &stage.borrow(), false);
    write_png(&format!("{dir}/5-nap.png"), &fb)?;

    println!("wrote 5 frames to {dir}/");
    Ok(())
}

// ── native window ──────────────────────────────────────────────────────────
fn run_window(scale: usize) -> Result<()> {
    use minifb::{Key, MouseButton, MouseMode, Scale, Window, WindowOptions};
    let stage = Stage::new();
    let brain = Brain::new(stage.clone())?;
    let sprites = Sprites::load();
    let font = Font::build();
    let mut fb = Framebuffer::new(W, H);

    let scale_opt = match scale {
        1 => Scale::X1,
        2 => Scale::X2,
        _ => Scale::X4,
    };
    let mut win = Window::new(
        "Pocket Cat",
        W,
        H,
        WindowOptions { borderless: false, resize: false, topmost: true, scale: scale_opt, ..Default::default() },
    )?;
    win.set_target_fps(30);
    brain.event(serde_json::json!({"t":"boot"}));

    let sf = scale.max(1) as f32;
    let mut prev_l = false;
    let mut prev_r = false;
    let mut last = std::time::Instant::now();

    while win.is_open() && !win.is_key_down(Key::Escape) {
        let now = std::time::Instant::now();
        let dt = (now - last).as_secs_f64() * 1000.0;
        last = now;
        step(&stage, &brain, &sprites, dt.min(100.0));

        // input
        let (mx, my) = win.get_mouse_pos(MouseMode::Clamp).unwrap_or((0.0, 0.0));
        let (lx, ly) = ((mx / sf) as i32, (my / sf) as i32);
        let ldown = win.get_mouse_down(MouseButton::Left);
        let rdown = win.get_mouse_down(MouseButton::Right);

        if rdown && !prev_r {
            let mut g = stage.borrow_mut();
            g.menu_open = true;
            g.menu_x = lx.min(W as i32 - 96);
            g.menu_y = ly.min(H as i32 - 76);
        }
        if ldown && !prev_l {
            let open = stage.borrow().menu_open;
            if open {
                let hit = menu_hit(&stage.borrow(), lx, ly);
                stage.borrow_mut().menu_open = false;
                if let Some(act) = hit {
                    if act == "about" {
                        println!("Pocket Cat — native macOS widget on Pocket Pi (Rust + QuickJS). Right-click for the menu.");
                    } else {
                        brain.event(serde_json::json!({"t":"menu","act":act}));
                    }
                }
            } else if lx >= 150 && ly >= 62 {
                brain.event(serde_json::json!({"t":"pet"}));
            }
        }
        prev_l = ldown;
        prev_r = rdown;

        render(&mut fb, &font, &sprites, &stage.borrow(), false);
        win.update_with_buffer(&fb.to_minifb(), W, H)?;
    }
    Ok(())
}

fn menu_hit(stage: &Stage, x: i32, y: i32) -> Option<&'static str> {
    if !stage.menu_open {
        return None;
    }
    if x < stage.menu_x || x > stage.menu_x + 92 {
        return None;
    }
    for (i, (act, _)) in MENU.iter().enumerate() {
        let iy = stage.menu_y + 4 + i as i32 * 13;
        if y >= iy && y < iy + 13 {
            return Some(act);
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Default 1× ≈ 300×150 pt — a compact desktop pet (~1/5 of a laptop's width).
    let mut scale: f64 = 1.0;
    if let Some(i) = args.iter().position(|a| a == "--scale") {
        if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
            scale = v;
        }
    }
    let windowed = args.iter().any(|a| a == "--windowed");
    let result = if let Some(i) = args.iter().position(|a| a == "--capture") {
        let dir = args.get(i + 1).map(|s| s.as_str()).unwrap_or("captures");
        run_capture(dir)
    } else if windowed {
        run_window((scale.round() as usize).max(1)) // opaque titled window (minifb) — for debugging
    } else {
        // default: the windowless transparent desktop pet
        #[cfg(target_os = "macos")]
        {
            mac_widget::run(scale)
        }
        #[cfg(not(target_os = "macos"))]
        {
            run_window((scale.round() as usize).max(1))
        }
    };
    if let Err(e) = result {
        eprintln!("pocket-cat: {e:#}");
        std::process::exit(1);
    }
}
