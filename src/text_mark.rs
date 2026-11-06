use windows_sys::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::*,
};

use crate::{
    selection_geometry::{intersect, BORDER_BLUE},
    typography,
};

#[derive(Clone, PartialEq)]
pub struct TextMark {
    pub text: Vec<u16>,
    pub size: i32,
    pub color: u32,
    center: (f64, f64),
    width: i32,
    height: i32,
    angle: f64,
}

#[derive(Clone, Copy)]
pub enum TextHandle {
    Move,
    Scale(i32, i32),
    Rotate,
}

const CORNERS: [(i32, i32); 4] = [(-1, -1), (1, -1), (1, 1), (-1, 1)];
const ROTATE_OFFSET: i32 = 14;
const ROTATE_RADIUS: i32 = 9;
const MIN_SIZE: i32 = 8;
const MAX_SIZE: i32 = 128;

impl TextMark {
    pub fn new(rect: RECT, size: i32, color: u32) -> Self {
        Self {
            text: Vec::new(),
            size,
            color,
            center: (
                f64::from(rect.left + rect.right) / 2.0,
                f64::from(rect.top + rect.bottom) / 2.0,
            ),
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
            angle: 0.0,
        }
    }

    fn local_rect(&self) -> RECT {
        let left = -self.width / 2;
        let top = -self.height / 2;
        RECT {
            left,
            top,
            right: left + self.width,
            bottom: top + self.height,
        }
    }

    pub fn edit_rect(&self, clip: RECT) -> RECT {
        if self.is_rotated() {
            let (sin, cos) = self.angle.sin_cos();
            let (sin, cos) = (sin.abs(), cos.abs());
            let width = f64::from(self.width);
            let height = f64::from(self.height);
            // Keep the horizontal native input inside the rotated outline.
            let projected_width = width * cos + height * sin;
            let projected_height = width * sin + height * cos;
            let scale = ((width - 4.0).max(1.0) / (projected_width * cos + projected_height * sin))
                .min((height - 4.0).max(1.0) / (projected_width * sin + projected_height * cos));
            let half_width = projected_width * scale / 2.0;
            let half_height = projected_height * scale / 2.0;
            let inside = RECT {
                left: (self.center.0 - half_width).ceil() as i32,
                top: (self.center.1 - half_height).ceil() as i32,
                right: (self.center.0 + half_width).floor() as i32,
                bottom: (self.center.1 + half_height).floor() as i32,
            };
            if let Some(visible) = intersect(inside, clip) {
                return visible;
            }
        }
        let width = self.width.max(32).min(clip.right - clip.left);
        let height = self.height.max(self.size + 10).min(clip.bottom - clip.top);
        let left = (self.center.0.round() as i32 - width / 2).clamp(clip.left, clip.right - width);
        let top = (self.center.1.round() as i32 - height / 2).clamp(clip.top, clip.bottom - height);
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    pub fn is_rotated(&self) -> bool {
        self.angle.cos() < 1.0 - f64::EPSILON
    }

    fn transform(&self, x: f64, y: f64) -> (f64, f64) {
        let (sin, cos) = self.angle.sin_cos();
        (
            self.center.0 + x * cos - y * sin,
            self.center.1 + x * sin + y * cos,
        )
    }

    fn local_point(&self, point: POINT) -> (f64, f64) {
        let (sin, cos) = self.angle.sin_cos();
        let x = f64::from(point.x) - self.center.0;
        let y = f64::from(point.y) - self.center.1;
        (x * cos + y * sin, -x * sin + y * cos)
    }

    pub fn contains(&self, point: POINT) -> bool {
        let (x, y) = self.local_point(point);
        let rect = self.local_rect();
        x >= f64::from(rect.left)
            && x <= f64::from(rect.right)
            && y >= f64::from(rect.top)
            && y <= f64::from(rect.bottom)
    }

    fn corner(&self, x: i32, y: i32) -> POINT {
        let rect = self.local_rect();
        self.screen_point(
            if x < 0 { rect.left } else { rect.right },
            if y < 0 { rect.top } else { rect.bottom },
        )
    }

    fn screen_point(&self, x: i32, y: i32) -> POINT {
        let (x, y) = self.transform(f64::from(x), f64::from(y));
        POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        }
    }

    fn rotation_point(&self) -> POINT {
        self.screen_point(0, self.local_rect().top - ROTATE_OFFSET)
    }

    pub fn bounds(&self) -> RECT {
        let rotation = self.rotation_point();
        let mut bounds = RECT {
            left: rotation.x,
            top: rotation.y,
            right: rotation.x,
            bottom: rotation.y,
        };
        for (x, y) in CORNERS {
            let point = self.corner(x, y);
            bounds.left = bounds.left.min(point.x);
            bounds.top = bounds.top.min(point.y);
            bounds.right = bounds.right.max(point.x);
            bounds.bottom = bounds.bottom.max(point.y);
        }
        RECT {
            left: bounds.left - ROTATE_RADIUS - 2,
            top: bounds.top - ROTATE_RADIUS - 2,
            right: bounds.right + ROTATE_RADIUS + 3,
            bottom: bounds.bottom + ROTATE_RADIUS + 3,
        }
    }

    pub fn hit(&self, point: POINT) -> Option<TextHandle> {
        self.hit_chrome(point)
            .or_else(|| self.contains(point).then_some(TextHandle::Move))
    }

    pub fn hit_chrome(&self, point: POINT) -> Option<TextHandle> {
        let near =
            |handle: POINT| (handle.x - point.x).abs() <= 6 && (handle.y - point.y).abs() <= 6;
        let rotate = self.rotation_point();
        let dx = rotate.x - point.x;
        let dy = rotate.y - point.y;
        if i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)
            <= i64::from((ROTATE_RADIUS + 1).pow(2))
        {
            return Some(TextHandle::Rotate);
        }
        for (x, y) in CORNERS {
            if near(self.corner(x, y)) {
                return Some(TextHandle::Scale(x, y));
            }
        }
        let (x, y) = self.local_point(point);
        let rect = self.local_rect();
        let distance = [
            x - f64::from(rect.left),
            f64::from(rect.right) - x,
            y - f64::from(rect.top),
            f64::from(rect.bottom) - y,
        ]
        .into_iter()
        .fold(f64::INFINITY, f64::min);
        (-5.0..=3.0).contains(&distance).then_some(TextHandle::Move)
    }

    pub fn transform_drag(&self, handle: TextHandle, anchor: POINT, point: POINT) -> Self {
        self.transform_drag_from(self, handle, anchor, point)
    }

    pub fn transform_drag_from(
        &self,
        frame: &Self,
        handle: TextHandle,
        anchor: POINT,
        point: POINT,
    ) -> Self {
        let mut next = self.clone();
        if anchor.x == point.x && anchor.y == point.y {
            return next;
        }
        match handle {
            TextHandle::Move => {
                next.center.0 += f64::from(point.x - anchor.x);
                next.center.1 += f64::from(point.y - anchor.y);
            }
            TextHandle::Rotate => {
                let angle = |point: POINT| {
                    (f64::from(point.y) - frame.center.1).atan2(f64::from(point.x) - frame.center.0)
                };
                next.angle += angle(point) - angle(anchor);
            }
            TextHandle::Scale(x, y) => {
                let fixed = self.corner(-x, -y);
                let origin = frame.corner(-x, -y);
                let dragged = frame.corner(x, y);
                let dx = f64::from(dragged.x - origin.x);
                let dy = f64::from(dragged.y - origin.y);
                let delta_x = f64::from(point.x - anchor.x);
                let delta_y = f64::from(point.y - anchor.y);
                let factor = 1.0 + (delta_x * dx + delta_y * dy) / (dx * dx + dy * dy).max(1.0);
                next.size = (f64::from(self.size) * factor)
                    .round()
                    .clamp(f64::from(MIN_SIZE), f64::from(MAX_SIZE))
                    as i32;
                let scale = f64::from(next.size) / f64::from(self.size);
                next.width = (f64::from(self.width) * scale).round().max(1.0) as i32;
                next.height = (f64::from(self.height) * scale).round().max(1.0) as i32;
                next.center = (
                    f64::from(fixed.x) + (self.center.0 - f64::from(fixed.x)) * scale,
                    f64::from(fixed.y) + (self.center.1 - f64::from(fixed.y)) * scale,
                );
            }
        }
        next
    }

    pub unsafe fn fit_text(&mut self, dc: HDC) {
        if self.text.is_empty() {
            return;
        }
        self.width = self.width.max(self.size + 6);
        let previous = SelectObject(dc, typography::annotation_font(self.size));
        let mut bounds = RECT {
            right: self.width - 6,
            ..Default::default()
        };
        DrawTextW(
            dc,
            self.text.as_ptr(),
            self.text.len() as i32,
            &mut bounds,
            DT_WORDBREAK | DT_NOPREFIX | DT_CALCRECT | DT_EDITCONTROL,
        );
        SelectObject(dc, previous);
        self.height = self.height.max(bounds.bottom + 6);
    }

    pub unsafe fn paint(&self, dc: HDC) {
        if self.text.is_empty() {
            return;
        }
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let (sin, cos) = self.angle.sin_cos();
        let transform = XFORM {
            eM11: cos as f32,
            eM12: sin as f32,
            eM21: -sin as f32,
            eM22: cos as f32,
            eDx: self.center.0 as f32,
            eDy: self.center.1 as f32,
        };
        if SetGraphicsMode(dc, GM_ADVANCED) != 0 && SetWorldTransform(dc, &transform) != 0 {
            SelectObject(dc, typography::annotation_font(self.size));
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, self.color);
            let mut rect = self.local_rect();
            InflateRect(&mut rect, -3, -3);
            DrawTextW(
                dc,
                self.text.as_ptr(),
                self.text.len() as i32,
                &mut rect,
                DT_WORDBREAK | DT_NOPREFIX | DT_EDITCONTROL,
            );
        }
        RestoreDC(dc, saved);
    }

    pub unsafe fn paint_frame(&self, dc: HDC) {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let pen = CreatePen(PS_SOLID, 2, BORDER_BLUE);
        if pen.is_null() {
            SelectObject(dc, GetStockObject(DC_PEN));
            SetDCPenColor(dc, BORDER_BLUE);
        } else {
            SelectObject(dc, pen);
        }
        SelectObject(dc, GetStockObject(WHITE_BRUSH));
        let corners = CORNERS.map(|(x, y)| self.corner(x, y));
        let outline = [corners[0], corners[1], corners[2], corners[3], corners[0]];
        Polyline(dc, outline.as_ptr(), outline.len() as i32);
        let rotation = self.rotation_point();
        let stem = [self.screen_point(0, self.local_rect().top), rotation];
        Polyline(dc, stem.as_ptr(), 2);
        for point in corners {
            Ellipse(dc, point.x - 4, point.y - 4, point.x + 5, point.y + 5);
        }
        let POINT { x, y } = rotation;
        Ellipse(
            dc,
            x - ROTATE_RADIUS,
            y - ROTATE_RADIUS,
            x + ROTATE_RADIUS + 1,
            y + ROTATE_RADIUS + 1,
        );
        SetArcDirection(dc, AD_CLOCKWISE);
        Arc(dc, x - 5, y - 5, x + 6, y + 6, x + 4, y + 3, x + 5, y - 3);
        let arrow = [
            POINT { x: x + 5, y: y - 6 },
            POINT { x: x + 5, y: y - 2 },
            POINT { x: x + 1, y: y - 2 },
        ];
        Polyline(dc, arrow.as_ptr(), arrow.len() as i32);
        RestoreDC(dc, saved);
        if !pen.is_null() {
            DeleteObject(pen);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Desktop, Surface};
    use std::ptr::null_mut;

    #[test]
    fn rotated_text_renders_on_a_translated_crop_without_a_fill_or_handles() {
        unsafe {
            let desktop_bounds = RECT {
                right: 220,
                bottom: 180,
                ..Default::default()
            };
            let surface = Surface::new(null_mut(), 220, 180).unwrap();
            SetDCBrushColor(surface.dc, 0x0030_2010);
            FillRect(surface.dc, &desktop_bounds, GetStockObject(DC_BRUSH));
            GdiFlush();
            let desktop = Desktop {
                bounds: desktop_bounds,
                surface,
            };
            let frame = RECT {
                left: 80,
                top: 55,
                right: 170,
                bottom: 105,
            };
            let mut mark = TextMark::new(frame, 26, 0x00ff_ffff);
            mark.text = "TEST".encode_utf16().collect();
            let anchor = mark.rotation_point();
            let rotated = mark.transform_drag(TextHandle::Rotate, anchor, POINT { x: 169, y: 80 });
            assert!(rotated.angle > 1.5);
            let clip = RECT {
                left: 70,
                top: 20,
                right: 190,
                bottom: 140,
            };
            let image = desktop
                .composite_crop(clip, |dc| rotated.paint(dc))
                .unwrap();
            let background = [0x30, 0x20, 0x10];
            let changed_rows = image
                .pixels
                .chunks_exact(image.width as usize * 4)
                .filter(|row| row.chunks_exact(4).any(|pixel| pixel[..3] != background))
                .count();
            assert!(
                (35..100).contains(&changed_rows),
                "changed rows: {changed_rows}"
            );
            assert_eq!(image.pixels[..3], background);
        }
    }
}
