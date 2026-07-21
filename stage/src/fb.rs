//! Framebuffer — the `stage` core's pixel surface. Pure (no windowing): the
//! window host blits it, and `--capture` writes it to PNG, so the render path
//! is verifiable headless (RUNTIMES.md: a runtime without a headless story is
//! not done). Pixels are 0xAARRGGBB; blits are nearest-neighbour so the pixel
//! art stays crisp at any integer-ish scale.

use std::collections::HashMap;

pub type Argb = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Argb {
    0xff00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Argb {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

pub struct Sprite {
    pub w: u32,
    pub h: u32,
    pub px: Vec<Argb>, // straight RGBA, alpha in top byte
}

pub struct Framebuffer {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Argb>,
}

impl Framebuffer {
    pub fn new(w: usize, h: usize) -> Self {
        Framebuffer { w, h, px: vec![0xff00_0000; w * h] }
    }

    pub fn clear(&mut self, c: Argb) {
        for p in self.px.iter_mut() {
            *p = c;
        }
    }

    #[inline]
    fn blend(dst: Argb, src: Argb) -> Argb {
        let a = (src >> 24) & 0xff;
        if a == 0 {
            return dst;
        }
        if a == 0xff {
            return 0xff00_0000 | (src & 0x00ff_ffff);
        }
        let ia = 255 - a;
        let sr = (src >> 16) & 0xff;
        let sg = (src >> 8) & 0xff;
        let sb = src & 0xff;
        let dr = (dst >> 16) & 0xff;
        let dg = (dst >> 8) & 0xff;
        let db = dst & 0xff;
        let r = (sr * a + dr * ia) / 255;
        let g = (sg * a + dg * ia) / 255;
        let b = (sb * a + db * ia) / 255;
        0xff00_0000 | (r << 16) | (g << 8) | b
    }

    #[inline]
    pub fn put(&mut self, x: i32, y: i32, c: Argb) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.px[i] = Self::blend(self.px[i], c);
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Argb) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, c);
            }
        }
    }

    pub fn frame_rect(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: Argb) {
        self.rect(x, y, w, t, c);
        self.rect(x, y + h - t, w, t, c);
        self.rect(x, y, t, h, c);
        self.rect(x + w - t, y, t, h, c);
    }

    /// Blit a sprite scaled by integer `s`, optionally mirrored in X.
    pub fn blit(&mut self, sp: &Sprite, dx: i32, dy: i32, s: i32, flip: bool) {
        let s = s.max(1);
        for sy in 0..sp.h as i32 {
            for sx in 0..sp.w as i32 {
                let src_x = if flip { sp.w as i32 - 1 - sx } else { sx };
                let c = sp.px[(sy * sp.w as i32 + src_x) as usize];
                if (c >> 24) == 0 {
                    continue;
                }
                for oy in 0..s {
                    for ox in 0..s {
                        self.put(dx + sx * s + ox, dy + sy * s + oy, c);
                    }
                }
            }
        }
    }

    /// Draw ASCII text with the built-in glyphs, scaled by `s`. Returns end x.
    pub fn text(&mut self, font: &Font, t: &str, x: i32, y: i32, c: Argb, s: i32) -> i32 {
        let s = s.max(1);
        let mut cx = x;
        for ch in t.chars() {
            if ch == ' ' {
                cx += 4 * s;
                continue;
            }
            if let Some(g) = font.glyphs.get(&ch.to_ascii_uppercase()) {
                for (ry, row) in g.iter().enumerate() {
                    for cxi in 0..FONT_W {
                        if row & (1 << (FONT_W - 1 - cxi)) != 0 {
                            self.rect(cx + cxi as i32 * s, y + ry as i32 * s, s, s, c);
                        }
                    }
                }
            }
            cx += (FONT_W as i32 + 1) * s;
        }
        cx
    }

    pub fn text_w(&self, t: &str, s: i32) -> i32 {
        t.chars().map(|ch| if ch == ' ' { 4 * s } else { (FONT_W as i32 + 1) * s }).sum()
    }

    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        for &p in &self.px {
            out.push(((p >> 16) & 0xff) as u8);
            out.push(((p >> 8) & 0xff) as u8);
            out.push((p & 0xff) as u8);
            out.push(((p >> 24) & 0xff) as u8);
        }
        out
    }

    /// 0x00RRGGBB for minifb (drops alpha over the window's own background).
    pub fn to_minifb(&self) -> Vec<u32> {
        self.px.iter().map(|&p| p & 0x00ff_ffff).collect()
    }
}

pub const FONT_W: usize = 5;
pub const FONT_H: usize = 7;

pub struct Font {
    pub glyphs: HashMap<char, [u8; FONT_H]>,
}

impl Font {
    /// Author glyphs as 5×7 string art — legible to check by eye, parsed once.
    pub fn build() -> Font {
        let src: &[(char, [&str; 7])] = &[
            ('A', ["01110","10001","10001","11111","10001","10001","00000"]),
            ('B', ["11110","10001","11110","10001","10001","11110","00000"]),
            ('C', ["01110","10001","10000","10000","10001","01110","00000"]),
            ('D', ["11110","10001","10001","10001","10001","11110","00000"]),
            ('E', ["11111","10000","11110","10000","10000","11111","00000"]),
            ('F', ["11111","10000","11110","10000","10000","10000","00000"]),
            ('G', ["01110","10001","10000","10111","10001","01110","00000"]),
            ('H', ["10001","10001","11111","10001","10001","10001","00000"]),
            ('I', ["01110","00100","00100","00100","00100","01110","00000"]),
            ('J', ["00111","00010","00010","00010","10010","01100","00000"]),
            ('K', ["10001","10010","11100","10010","10001","10001","00000"]),
            ('L', ["10000","10000","10000","10000","10000","11111","00000"]),
            ('M', ["10001","11011","10101","10001","10001","10001","00000"]),
            ('N', ["10001","11001","10101","10011","10001","10001","00000"]),
            ('O', ["01110","10001","10001","10001","10001","01110","00000"]),
            ('P', ["11110","10001","11110","10000","10000","10000","00000"]),
            ('Q', ["01110","10001","10001","10101","10010","01101","00000"]),
            ('R', ["11110","10001","11110","10100","10010","10001","00000"]),
            ('S', ["01111","10000","01110","00001","00001","11110","00000"]),
            ('T', ["11111","00100","00100","00100","00100","00100","00000"]),
            ('U', ["10001","10001","10001","10001","10001","01110","00000"]),
            ('V', ["10001","10001","10001","10001","01010","00100","00000"]),
            ('W', ["10001","10001","10001","10101","11011","10001","00000"]),
            ('X', ["10001","01010","00100","00100","01010","10001","00000"]),
            ('Y', ["10001","01010","00100","00100","00100","00100","00000"]),
            ('Z', ["11111","00010","00100","01000","10000","11111","00000"]),
            ('0', ["01110","10011","10101","11001","10001","01110","00000"]),
            ('1', ["00100","01100","00100","00100","00100","01110","00000"]),
            ('2', ["01110","10001","00010","00100","01000","11111","00000"]),
            ('3', ["11110","00001","01110","00001","00001","11110","00000"]),
            ('4', ["00010","00110","01010","10010","11111","00010","00000"]),
            ('5', ["11111","10000","11110","00001","00001","11110","00000"]),
            ('6', ["01110","10000","11110","10001","10001","01110","00000"]),
            ('7', ["11111","00001","00010","00100","01000","01000","00000"]),
            ('8', ["01110","10001","01110","10001","10001","01110","00000"]),
            ('9', ["01110","10001","10001","01111","00001","01110","00000"]),
            ('.', ["00000","00000","00000","00000","00000","00100","00000"]),
            ('-', ["00000","00000","01110","00000","00000","00000","00000"]),
            (':', ["00000","00100","00000","00000","00100","00000","00000"]),
            ('/', ["00001","00010","00100","01000","10000","00000","00000"]),
            ('!', ["00100","00100","00100","00100","00000","00100","00000"]),
            ('+', ["00000","00100","01110","00100","00000","00000","00000"]),
            ('*', ["00000","10101","01110","10101","00000","00000","00000"]),
            (',', ["00000","00000","00000","00000","00100","01000","00000"]),
            ('?', ["01110","10001","00010","00100","00000","00100","00000"]),
            ('%', ["11001","11010","00100","01011","10011","00000","00000"]),
        ];
        let mut glyphs = HashMap::new();
        for (ch, rows) in src {
            let mut g = [0u8; FONT_H];
            for (i, row) in rows.iter().enumerate() {
                let mut bits = 0u8;
                for (j, c) in row.chars().enumerate() {
                    if c == '1' {
                        bits |= 1 << (FONT_W - 1 - j);
                    }
                }
                g[i] = bits;
            }
            glyphs.insert(*ch, g);
        }
        Font { glyphs }
    }
}
