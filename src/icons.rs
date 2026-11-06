mod phosphor;

use std::{cell::RefCell, collections::HashMap, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::RECT,
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyCursor, GetCursor, SetCursor, HCURSOR, ICONINFO,
    },
};

use crate::capture::{Image, Surface};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Rectangle,
    Text,
    Brush,
    Mosaic,
    Undo,
    Redo,
    LongCapture,
    Copy,
    Save,
    Pin,
    Close,
    MouseLeft,
    MouseRight,
    Drag,
    Move,
    Resize,
}

pub struct Cursor(pub HCURSOR);

impl Cursor {
    pub fn brush() -> Option<Self> {
        let mut image = tinted_icon(Icon::Brush, 24, 0x001a_1a1a);
        let ink = image.pixels.clone();
        for y in 0..24_usize {
            for x in 0..24_usize {
                let offset = (y * 24 + x) * 4;
                if ink[offset + 3] == 0
                    && (y.saturating_sub(1)..=(y + 1).min(23)).any(|row| {
                        (x.saturating_sub(1)..=(x + 1).min(23))
                            .any(|col| ink[(row * 24 + col) * 4 + 3] > 100)
                    })
                {
                    image.pixels[offset..offset + 4].fill(255);
                }
            }
        }
        Self::new(image, 4, 20)
    }

    pub fn mosaic(diameter: i32) -> Option<Self> {
        let size = diameter + 4;
        let center = size / 2;
        let radius = f64::from(diameter) / 2.0;
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let distance = (f64::from(x - center).hypot(f64::from(y - center)) - radius).abs();
                let pixel = if distance <= 0.65 {
                    [20, 20, 20, 255]
                } else if distance <= 1.65 {
                    [255; 4]
                } else {
                    [0; 4]
                };
                pixels.extend_from_slice(&pixel);
            }
        }
        Self::new(
            Image {
                width: size,
                height: size,
                pixels,
            },
            center as u32,
            center as u32,
        )
    }

    fn new(image: Image, x: u32, y: u32) -> Option<Self> {
        unsafe {
            let (surface, width, height) = image.into_surface().ok()?;
            let mask_stride = (width as usize).div_ceil(16) * 2;
            let mask_bits = vec![0_u8; mask_stride * height as usize];
            let mask = CreateBitmap(width, height, 1, 1, mask_bits.as_ptr().cast());
            if mask.is_null() {
                return None;
            }
            let cursor = CreateIconIndirect(&ICONINFO {
                fIcon: 0,
                xHotspot: x,
                yHotspot: y,
                hbmMask: mask,
                hbmColor: surface.bitmap(),
            });
            DeleteObject(mask);
            (!cursor.is_null()).then_some(Self(cursor))
        }
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        unsafe {
            if GetCursor() == self.0 {
                SetCursor(null_mut());
            }
            DestroyCursor(self.0);
        }
    }
}

const MAX_CACHED_TILES: usize = 96;
const ACCENT: u32 = 0x00ff_b969;
type TileKey = (Icon, i32, u32);

thread_local! {
    static TILES: RefCell<HashMap<TileKey, Surface>> = RefCell::new(HashMap::new());
}

pub unsafe fn paint(dc: HDC, bounds: RECT, icon: Icon, color: u32) {
    let size = (bounds.right - bounds.left).min(bounds.bottom - bounds.top);
    if size <= 0 {
        return;
    }
    let key = (icon, size, color);
    TILES.with_borrow_mut(|tiles| {
        if tiles.len() >= MAX_CACHED_TILES && !tiles.contains_key(&key) {
            GdiFlush();
            tiles.clear();
        }
        let tile = match tiles.entry(key) {
            std::collections::hash_map::Entry::Occupied(tile) => tile.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let Ok((surface, _, _)) = tinted_icon(icon, size, color).into_surface() else {
                    return;
                };
                entry.insert(surface)
            }
        };
        AlphaBlend(
            dc,
            bounds.left + (bounds.right - bounds.left - size) / 2,
            bounds.top + (bounds.bottom - bounds.top - size) / 2,
            size,
            size,
            tile.dc,
            0,
            0,
            size,
            size,
            BLENDFUNCTION {
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
                ..Default::default()
            },
        );
    });
}

fn tinted_icon(icon: Icon, size: i32, color: u32) -> Image {
    let native = if size <= 12 { 12 } else { 18 };
    let index = usize::from(native == 18);
    let mask = phosphor::mask(icon)[index];
    let accent = match icon {
        Icon::MouseLeft => Some(phosphor::MOUSE_LEFT_DOT[index]),
        Icon::MouseRight => Some(phosphor::MOUSE_RIGHT_DOT[index]),
        _ => None,
    };
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let base = coverage(mask, native, size, x, y);
            let dot = accent.map_or(0, |mask| coverage(mask, native, size, x, y));
            let visible_base = (base * (255 - dot) + 127) / 255;
            for shift in [16, 8, 0] {
                let ink = ((color >> shift) & 255) as u16;
                let accent = ((ACCENT >> shift) & 255) as u16;
                pixels.push(((ink * visible_base + accent * dot + 127) / 255) as u8);
            }
            pixels.push((visible_base + dot) as u8);
        }
    }
    Image {
        width: size,
        height: size,
        pixels,
    }
}

fn coverage(mask: &[u8], native: i32, size: i32, x: i32, y: i32) -> u16 {
    let sample = |x: i32, y: i32| {
        let digit = mask[(y * native + x) as usize];
        let value = if digit <= b'9' {
            digit - b'0'
        } else {
            digit - b'a' + 10
        };
        u16::from(value) * 17
    };
    if native == size {
        return sample(x, y);
    }
    let source = |value| {
        ((value as f32 + 0.5) * native as f32 / size as f32 - 0.5).clamp(0.0, (native - 1) as f32)
    };
    let sx = source(x);
    let sy = source(y);
    let left = sx as i32;
    let top = sy as i32;
    let right = (left + 1).min(native - 1);
    let bottom = (top + 1).min(native - 1);
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let upper = mix(
        f32::from(sample(left, top)),
        f32::from(sample(right, top)),
        sx.fract(),
    );
    let lower = mix(
        f32::from(sample(left, bottom)),
        f32::from(sample(right, bottom)),
        sx.fract(),
    );
    mix(upper, lower, sy.fract()).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;

    #[test]
    fn native_icon_uses_the_requested_color_inside_its_cell() {
        let bounds = RECT {
            left: 10,
            top: 8,
            right: 28,
            bottom: 26,
        };
        unsafe {
            let surface = Surface::new(null_mut(), 38, 34).unwrap();
            let area = RECT {
                right: 38,
                bottom: 34,
                ..Default::default()
            };
            FillRect(surface.dc, &area, GetStockObject(WHITE_BRUSH));
            let color = 0x0030_7090;
            paint(surface.dc, bounds, Icon::Copy, color);
            let mut painted = 0;
            let mut softened = 0;
            for y in area.top..area.bottom {
                for x in area.left..area.right {
                    let pixel = GetPixel(surface.dc, x, y);
                    if pixel != 0x00ff_ffff {
                        assert!((bounds.left..bounds.right).contains(&x));
                        assert!((bounds.top..bounds.bottom).contains(&y));
                        painted += 1;
                        if pixel != color {
                            softened += 1;
                        }
                    }
                }
            }
            assert!(painted > 25);
            assert!(softened > 0);
        }
    }

    #[test]
    fn mosaic_has_two_dark_diagonal_tiles_and_two_open_tiles() {
        let image = tinted_icon(Icon::Mosaic, 18, 0);
        let alpha = |x: usize, y: usize| image.pixels[(y * 18 + x) * 4 + 3];
        assert_eq!(alpha(5, 5), 255);
        assert_eq!(alpha(12, 12), 255);
        assert_eq!(alpha(12, 5), 0);
        assert_eq!(alpha(5, 12), 0);
    }
}
#[test]
fn creates_native_cursors_for_the_brush_and_mosaic_size_limits() {
    assert!(Cursor::brush().is_some());
    assert!(Cursor::mosaic(4).is_some());
    assert!(Cursor::mosaic(128).is_some());
}
