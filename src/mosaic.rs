use std::{cell::RefCell, collections::HashMap, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::*,
};

use crate::{
    capture::{Desktop, Surface},
    selection_geometry::intersect,
};

const CHUNK: i32 = 64;
const STRIDE: usize = CHUNK as usize * 4;
const CHUNK_BYTES: usize = STRIDE * CHUNK as usize;

thread_local! {
    static BLEND_TILE: RefCell<Option<Surface>> = const { RefCell::new(None) };
}

#[derive(Default)]
pub struct Mosaic {
    colors: HashMap<(i32, i32), [u8; 3]>,
    chunks: HashMap<(i32, i32), Box<[u8]>>,
}

impl Mosaic {
    pub fn finish(mut self) -> Option<Self> {
        self.colors = HashMap::new();
        (!self.chunks.is_empty()).then_some(self)
    }

    pub fn extend(
        &mut self,
        source: &Desktop,
        clip: RECT,
        from: POINT,
        to: POINT,
        width: i32,
        block: i32,
    ) {
        let Self { colors, chunks } = self;
        let outer = f64::from(width) / 2.0 + 1.0;
        let inner = outer - 2.0;
        let pad = outer.ceil() as i32;
        let dx = f64::from(to.x - from.x);
        let dy = f64::from(to.y - from.y);
        let length_squared = (dx * dx + dy * dy).max(1.0);
        let area = RECT {
            left: (from.x.min(to.x) - pad).max(clip.left),
            top: (from.y.min(to.y) - pad).max(clip.top),
            right: (from.x.max(to.x) + pad + 1).min(clip.right),
            bottom: (from.y.max(to.y) + pad + 1).min(clip.bottom),
        };
        for y in (area.top / CHUNK * CHUNK..area.bottom).step_by(CHUNK as usize) {
            let (start, end) = if dy == 0.0 {
                (0.0, 1.0)
            } else {
                let a = f64::from(y - pad - from.y) / dy;
                let b = f64::from(y + CHUNK + pad - from.y) / dy;
                (a.min(b).clamp(0.0, 1.0), a.max(b).clamp(0.0, 1.0))
            };
            let x1 = f64::from(from.x) + dx * start;
            let x2 = f64::from(from.x) + dx * end;
            let first_x = (x1.min(x2).floor() as i32 - pad).max(area.left) / CHUNK * CHUNK;
            let last_x = (x1.max(x2).ceil() as i32 + pad + 1).min(area.right);
            for x in (first_x..last_x).step_by(CHUNK as usize) {
                let chunk = chunk_rect(x, y);
                let Some(dirty) = intersect(area, chunk) else {
                    continue;
                };
                let pixels = chunks
                    .entry((x, y))
                    .or_insert_with(|| vec![0; CHUNK_BYTES].into_boxed_slice());
                for py in dirty.top..dirty.bottom {
                    let row = (py - y) as usize * STRIDE;
                    let y_from = f64::from(py - from.y);
                    let mut cached = None;
                    for px in dirty.left..dirty.right {
                        let x_from = f64::from(px - from.x);
                        let t = ((x_from * dx + y_from * dy) / length_squared).clamp(0.0, 1.0);
                        let gap_x = x_from - t * dx;
                        let gap_y = y_from - t * dy;
                        let distance_squared = gap_x * gap_x + gap_y * gap_y;
                        if distance_squared >= outer * outer {
                            continue;
                        }
                        let alpha = if distance_squared <= inner * inner {
                            255
                        } else {
                            ((outer - distance_squared.sqrt()) * 127.5).round() as u8
                        };
                        let offset = row + (px - x) as usize * 4;
                        if alpha <= pixels[offset + 3] {
                            continue;
                        }
                        let tile_x = px / block * block;
                        let tile_y = py / block * block;
                        let color = if let Some((_, color)) = cached.filter(|(x, _)| *x == tile_x) {
                            color
                        } else {
                            let color = *colors.entry((tile_x, tile_y)).or_insert_with(|| {
                                let tile = intersect(
                                    clip,
                                    RECT {
                                        left: tile_x,
                                        top: tile_y,
                                        right: tile_x + block,
                                        bottom: tile_y + block,
                                    },
                                )
                                .unwrap();
                                source.average_bgr(tile)
                            });
                            cached = Some((tile_x, color));
                            color
                        };
                        for (index, channel) in color.into_iter().enumerate() {
                            pixels[offset + index] =
                                ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8;
                        }
                        pixels[offset + 3] = alpha;
                    }
                }
            }
        }
    }

    pub unsafe fn paint(&self, dc: HDC) {
        let mut dirty = RECT::default();
        if GetClipBox(dc, &mut dirty) <= NULLREGION {
            return;
        }
        BLEND_TILE.with_borrow_mut(|scratch| {
            let mut visible = self
                .chunks
                .iter()
                .filter_map(|(&(x, y), pixels)| {
                    Some((x, y, intersect(dirty, chunk_rect(x, y))?, pixels))
                })
                .peekable();
            if visible.peek().is_none() {
                return;
            }
            if scratch.is_none() {
                *scratch = Surface::new(null_mut(), CHUNK, CHUNK).ok();
            }
            let Some(scratch) = scratch else { return };
            for (x, y, area, pixels) in visible {
                GdiFlush();
                scratch.write_pixels(pixels);
                let width = area.right - area.left;
                let height = area.bottom - area.top;
                AlphaBlend(
                    dc,
                    area.left,
                    area.top,
                    width,
                    height,
                    scratch.dc,
                    area.left - x,
                    area.top - y,
                    width,
                    height,
                    BLENDFUNCTION {
                        SourceConstantAlpha: 255,
                        AlphaFormat: AC_SRC_ALPHA as u8,
                        ..Default::default()
                    },
                );
            }
        });
    }
}

fn chunk_rect(x: i32, y: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + CHUNK,
        bottom: y + CHUNK,
    }
}
