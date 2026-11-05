use std::{io, mem::size_of};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::*,
    },
};

use crate::{capture::Surface, selection_geometry::intersect};

const PANEL_WIDTH: i32 = 320;
const PANEL_HEIGHT: i32 = 138;
const PANEL_MARGIN: i32 = 16;
const ZOOM: i32 = 5;
const SAMPLE_WIDTH: i32 = 33;
const SAMPLE_HEIGHT: i32 = 25;
const BORDER: i32 = 2;
const LOUPE_WIDTH: i32 = SAMPLE_WIDTH * ZOOM + BORDER * 2;
const LOUPE_HEIGHT: i32 = SAMPLE_HEIGHT * ZOOM + BORDER * 2;
const POINTER_GAP: i32 = 18;

pub unsafe fn monitor_info(cursor: POINT) -> Option<MONITORINFO> {
    let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info)
}

pub struct HintPanel {
    pub bounds: RECT,
    surface: Surface,
}

impl HintPanel {
    pub unsafe fn new(source: HDC, work_area: RECT) -> io::Result<Self> {
        let width = PANEL_WIDTH.min(work_area.right - work_area.left);
        let height = PANEL_HEIGHT.min(work_area.bottom - work_area.top);
        let left = (work_area.left + PANEL_MARGIN).min(work_area.right - width);
        let top = (work_area.bottom - PANEL_MARGIN - height).max(work_area.top);
        let bounds = RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        let surface = Surface::new(source, width, height)?;
        let panel = RECT {
            right: width,
            bottom: height,
            ..Default::default()
        };
        let brush = GetStockObject(DC_BRUSH);
        FillRect(surface.dc, &panel, GetStockObject(BLACK_BRUSH));
        let blend = BLENDFUNCTION {
            SourceConstantAlpha: 26,
            ..Default::default()
        };
        AlphaBlend(
            surface.dc, 0, 0, width, height, source, left, top, width, height, blend,
        );
        SetDCBrushColor(surface.dc, 0x0040_4040);
        FrameRect(surface.dc, &panel, brush);
        SetBkMode(surface.dc, TRANSPARENT as i32);
        let font = CreateFontW(
            -18,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            w!("Microsoft YaHei UI"),
        );
        let selected_font = if font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            font
        };
        let previous_font = SelectObject(surface.dc, selected_font);
        for (row, label, color) in [
            (0, w!("Snapshot  ·  截图快捷操作"), 0x00ff_ffff),
            (1, w!("单击锁定  ·  拖动框选"), 0x00df_dfdf),
            (2, w!("拖框移动  ·  拖边缩放"), 0x00df_dfdf),
            (3, w!("右键重选  ·  Esc 退出"), 0x00df_dfdf),
            (4, w!("Enter 复制  ·  Ctrl+S 保存"), 0x00df_dfdf),
        ] {
            let mut line = RECT {
                left: 16,
                top: 9 + row * 24,
                right: width - 14,
                bottom: 33 + row * 24,
            };
            SetTextColor(surface.dc, color);
            DrawTextW(
                surface.dc,
                label,
                -1,
                &mut line,
                DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
        SelectObject(surface.dc, previous_font);
        if !font.is_null() {
            DeleteObject(font);
        }
        Ok(Self { bounds, surface })
    }

    pub unsafe fn paint(&self, dc: HDC, dirty: RECT) {
        if let Some(rect) = intersect(self.bounds, dirty) {
            BitBlt(
                dc,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                self.surface.dc,
                rect.left - self.bounds.left,
                rect.top - self.bounds.top,
                SRCCOPY,
            );
        }
    }
}

#[derive(Clone, Copy)]
pub struct Loupe {
    pub bounds: RECT,
    pub point: POINT,
}

impl Loupe {
    pub fn new(point: POINT, monitor: RECT) -> Option<Self> {
        if monitor.right - monitor.left < LOUPE_WIDTH || monitor.bottom - monitor.top < LOUPE_HEIGHT
        {
            return None;
        }
        let offset = |pointer: i32, start: i32, end: i32, size: i32| {
            let after = pointer + POINTER_GAP;
            if after + size <= end {
                after.max(start)
            } else {
                (pointer - POINTER_GAP - size).clamp(start, end - size)
            }
        };
        let left = offset(point.x, monitor.left, monitor.right, LOUPE_WIDTH);
        let top = offset(point.y, monitor.top, monitor.bottom, LOUPE_HEIGHT);
        Some(Self {
            bounds: RECT {
                left,
                top,
                right: left + LOUPE_WIDTH,
                bottom: top + LOUPE_HEIGHT,
            },
            point,
        })
    }

    pub unsafe fn paint(self, dc: HDC, source: HDC, canvas: RECT) {
        let black = GetStockObject(BLACK_BRUSH);
        let white = GetStockObject(WHITE_BRUSH);
        FillRect(dc, &self.bounds, black);

        let sample = RECT {
            left: self.point.x - SAMPLE_WIDTH / 2,
            top: self.point.y - SAMPLE_HEIGHT / 2,
            right: self.point.x + SAMPLE_WIDTH / 2 + 1,
            bottom: self.point.y + SAMPLE_HEIGHT / 2 + 1,
        };
        let left = self.bounds.left + BORDER;
        let top = self.bounds.top + BORDER;
        if let Some(visible) = intersect(sample, canvas) {
            SetStretchBltMode(dc, COLORONCOLOR);
            StretchBlt(
                dc,
                left + (visible.left - sample.left) * ZOOM,
                top + (visible.top - sample.top) * ZOOM,
                (visible.right - visible.left) * ZOOM,
                (visible.bottom - visible.top) * ZOOM,
                source,
                visible.left,
                visible.top,
                visible.right - visible.left,
                visible.bottom - visible.top,
                SRCCOPY,
            );
        }
        FrameRect(dc, &self.bounds, white);
        let x = left + SAMPLE_WIDTH / 2 * ZOOM;
        let y = top + SAMPLE_HEIGHT / 2 * ZOOM;
        let center_x = x + ZOOM / 2;
        let center_y = y + ZOOM / 2;
        // Leave the sampled pixel clear and outline the guides for contrast on any background.
        for (radius, brush) in [(1, black), (0, white)] {
            for arm in [
                RECT {
                    left,
                    top: center_y - radius,
                    right: x - 2,
                    bottom: center_y + radius + 1,
                },
                RECT {
                    left: x + ZOOM + 2,
                    top: center_y - radius,
                    right: self.bounds.right - BORDER,
                    bottom: center_y + radius + 1,
                },
                RECT {
                    left: center_x - radius,
                    top,
                    right: center_x + radius + 1,
                    bottom: y - 2,
                },
                RECT {
                    left: center_x - radius,
                    top: y + ZOOM + 2,
                    right: center_x + radius + 1,
                    bottom: self.bounds.bottom - BORDER,
                },
            ] {
                FillRect(dc, &arm, brush);
            }
        }
        FrameRect(
            dc,
            &RECT {
                left: x - 2,
                top: y - 2,
                right: x + ZOOM + 2,
                bottom: y + ZOOM + 2,
            },
            black,
        );
        FrameRect(
            dc,
            &RECT {
                left: x - 1,
                top: y - 1,
                right: x + ZOOM + 1,
                bottom: y + ZOOM + 1,
            },
            white,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;

    #[test]
    fn loupe_draws_both_guides_without_covering_the_sampled_pixel() {
        unsafe {
            let canvas = RECT {
                right: 400,
                bottom: 300,
                ..Default::default()
            };
            let source = Surface::new(null_mut(), canvas.right, canvas.bottom).unwrap();
            let rendered = Surface::new(null_mut(), canvas.right, canvas.bottom).unwrap();
            let source_color = 0x005a_c8f5;
            SetDCBrushColor(source.dc, source_color);
            FillRect(source.dc, &canvas, GetStockObject(DC_BRUSH));

            let loupe = Loupe::new(POINT { x: 100, y: 100 }, canvas).unwrap();
            loupe.paint(rendered.dc, source.dc, canvas);
            let x = loupe.bounds.left + LOUPE_WIDTH / 2;
            let y = loupe.bounds.top + LOUPE_HEIGHT / 2;
            assert_eq!(GetPixel(rendered.dc, x - 20, y), 0x00ff_ffff);
            assert_eq!(GetPixel(rendered.dc, x, y - 20), 0x00ff_ffff);
            assert_eq!(GetPixel(rendered.dc, x - 20, y - 1), 0);
            assert_eq!(GetPixel(rendered.dc, x, y), source_color);
        }
    }

    #[test]
    fn loupe_flips_at_each_monitor_edge_and_handles_negative_origins() {
        let monitor = RECT {
            left: -1200,
            top: -800,
            right: 0,
            bottom: 0,
        };
        for (point, left, top) in [
            (POINT { x: -700, y: -500 }, false, false),
            (POINT { x: -10, y: -500 }, true, false),
            (POINT { x: -700, y: -10 }, false, true),
            (POINT { x: -10, y: -10 }, true, true),
        ] {
            let rect = Loupe::new(point, monitor).unwrap().bounds;
            assert_eq!(rect.right < point.x, left);
            assert_eq!(rect.bottom < point.y, top);
            assert!(rect.left >= monitor.left && rect.right <= monitor.right);
            assert!(rect.top >= monitor.top && rect.bottom <= monitor.bottom);
        }
    }
}
