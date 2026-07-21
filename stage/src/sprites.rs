//! Decode the embedded sprite groups (assets.rs) into RGBA sprites, keyed by
//! group name. Frames are the cat's animation states plus fx (heart/zzz).

use std::collections::HashMap;

use crate::assets::GROUPS;
use crate::fb::{rgba, Sprite};

pub struct Sprites {
    pub groups: HashMap<String, Vec<Sprite>>,
}

impl Sprites {
    pub fn load() -> Sprites {
        let mut groups = HashMap::new();
        for g in GROUPS {
            let frames: Vec<Sprite> = g.frames.iter().map(|bytes| decode(bytes)).collect();
            groups.insert(g.name.to_string(), frames);
        }
        Sprites { groups }
    }

    pub fn group(&self, name: &str) -> &[Sprite] {
        self.groups.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn frame(&self, name: &str, i: usize) -> Option<&Sprite> {
        self.groups.get(name).and_then(|v| v.get(i % v.len().max(1)))
    }
}

fn decode(bytes: &[u8]) -> Sprite {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().expect("valid PNG asset");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("PNG frame");
    let (w, h) = (info.width, info.height);
    let mut px = Vec::with_capacity((w * h) as usize);
    match info.color_type {
        png::ColorType::Rgba => {
            for c in buf.chunks_exact(4) {
                px.push(rgba(c[0], c[1], c[2], c[3]));
            }
        }
        png::ColorType::Rgb => {
            for c in buf.chunks_exact(3) {
                px.push(rgba(c[0], c[1], c[2], 255));
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for c in buf.chunks_exact(2) {
                px.push(rgba(c[0], c[0], c[0], c[1]));
            }
        }
        png::ColorType::Grayscale => {
            for &g in buf.iter() {
                px.push(rgba(g, g, g, 255));
            }
        }
        png::ColorType::Indexed => {
            // read_info expands most, but be safe: treat as opaque grey
            for &g in buf.iter() {
                px.push(rgba(g, g, g, 255));
            }
        }
    }
    px.truncate((w * h) as usize);
    while px.len() < (w * h) as usize {
        px.push(0);
    }
    Sprite { w, h, px }
}
