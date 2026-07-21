//! Antialiased text for the chat panel — the 5×7 pixel font is ASCII-only,
//! but @pb replies can be any language, so the chat renders through fontdue
//! over a system font (STHeiti has CJK). Loaded at runtime; if unavailable,
//! chat falls back to the pixel font (ASCII).

use fontdue::{Font, FontSettings};

use crate::fb::{Argb, Framebuffer};

const CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Helvetica.ttc",
];

pub struct Text {
    font: Option<Font>,
}

impl Text {
    pub fn load() -> Text {
        for path in CANDIDATES {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                    return Text { font: Some(font) };
                }
            }
        }
        Text { font: None }
    }

    pub fn available(&self) -> bool {
        self.font.is_some()
    }

    /// Draw one line at pixel height `px`, blended with `color`. Returns the
    /// advance width. No-op (returns 0) if no font loaded.
    pub fn line(&self, fb: &mut Framebuffer, s: &str, x: i32, y: i32, px: f32, color: Argb) -> i32 {
        let Some(font) = &self.font else { return 0 };
        let mut cx = x as f32;
        let cr = (color >> 16) & 0xff;
        let cg = (color >> 8) & 0xff;
        let cb = color & 0xff;
        for ch in s.chars() {
            let (m, bitmap) = font.rasterize(ch, px);
            let top = y + (px as i32 - m.height as i32) - m.ymin.max(0);
            for gy in 0..m.height {
                for gx in 0..m.width {
                    let cov = bitmap[gy * m.width + gx];
                    if cov == 0 {
                        continue;
                    }
                    let a = cov as u32;
                    let argb = (a << 24) | (cr << 16) | (cg << 8) | cb;
                    fb.put(cx as i32 + gx as i32 + m.xmin, top + gy as i32, argb);
                }
            }
            cx += m.advance_width.max(px * 0.3);
        }
        (cx - x as f32) as i32
    }

    /// Word/char-wrap `s` to `max_w` px and draw as lines; returns lines drawn.
    pub fn wrapped(&self, fb: &mut Framebuffer, s: &str, x: i32, y: i32, max_w: i32, px: f32, lh: i32, color: Argb, max_lines: usize) -> usize {
        if self.font.is_none() {
            return 0;
        }
        let mut line = String::new();
        let mut cy = y;
        let mut n = 0;
        let flush = |line: &str, cy: i32, this: &Self, fb: &mut Framebuffer| {
            this.line(fb, line, x, cy, px, color);
        };
        for ch in s.chars() {
            let trial = format!("{line}{ch}");
            if self.measure(&trial, px) > max_w && !line.is_empty() {
                flush(&line, cy, self, fb);
                n += 1;
                cy += lh;
                line.clear();
                if n >= max_lines {
                    return n;
                }
                if ch != ' ' {
                    line.push(ch);
                }
            } else {
                line.push(ch);
            }
        }
        if !line.is_empty() && n < max_lines {
            flush(&line, cy, self, fb);
            n += 1;
        }
        n
    }

    pub fn measure(&self, s: &str, px: f32) -> i32 {
        let Some(font) = &self.font else { return 0 };
        let mut w = 0.0f32;
        for ch in s.chars() {
            let m = font.metrics(ch, px);
            w += m.advance_width.max(px * 0.3);
        }
        w as i32
    }
}
