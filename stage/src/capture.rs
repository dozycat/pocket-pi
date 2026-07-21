//! Real OS-level screen capture (macOS). A background thread grabs the
//! screen with the system `screencapture`, downscales it with `sips`, decodes
//! the small PNG, and ships it to the render loop over a channel. This is the
//! genuine desktop the cat "watches" in its monitor — the same OS capability
//! cat's ScreenCollector uses, driven here directly so the widget is
//! self-contained. Capture only runs while observation is enabled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use crate::fb::Sprite;
use crate::sprites::decode;

pub struct Shot {
    pub sprite: Sprite,
    /// true if the frame is (almost) all black — Screen Recording permission
    /// is likely not granted yet.
    pub blank: bool,
}

pub struct Capture {
    pub rx: Receiver<Shot>,
    pub enabled: Arc<AtomicBool>,
}

/// Start the capture thread. `w`×`h` is the target (monitor screen) size.
pub fn start(w: u32, h: u32, interval_ms: u64) -> Capture {
    let (tx, rx) = channel::<Shot>();
    let enabled = Arc::new(AtomicBool::new(true));
    let en = enabled.clone();
    let tmp = std::env::temp_dir();
    let raw = tmp.join("pocketcat_raw.png");
    let small = tmp.join("pocketcat_small.png");

    std::thread::spawn(move || loop {
        if !en.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(interval_ms));
            continue;
        }
        use std::process::Stdio;
        // -x: no sound, -r: raw (no window shadow / cursor), main display.
        let cap = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", "-r", "-t", "png"])
            .arg(&raw)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if cap.map(|s| s.success()).unwrap_or(false) {
            // downscale natively; -z forces height,width.
            let z = std::process::Command::new("/usr/bin/sips")
                .args(["-s", "format", "png", "-z", &h.to_string(), &w.to_string()])
                .arg(&raw)
                .arg("--out")
                .arg(&small)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if z.map(|s| s.success()).unwrap_or(false) {
                if let Ok(bytes) = std::fs::read(&small) {
                    let sprite = decode(&bytes);
                    let blank = is_blank(&sprite);
                    let _ = tx.send(Shot { sprite, blank });
                }
            }
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    });

    Capture { rx, enabled }
}

fn is_blank(sp: &Sprite) -> bool {
    if sp.px.is_empty() {
        return true;
    }
    let mut sum: u64 = 0;
    for &p in &sp.px {
        let r = (p >> 16) & 0xff;
        let g = (p >> 8) & 0xff;
        let b = p & 0xff;
        sum += (r + g + b) as u64;
    }
    // mean luminance-ish; near-zero → black frame (no permission)
    (sum / sp.px.len() as u64) < 8
}
