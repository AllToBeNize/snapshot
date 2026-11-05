use std::{io, mem::size_of};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::*,
    },
};

use crate::{annotation::Tool, capture::Surface, selection_geometry::intersect};

const PANEL_WIDTH: i32 = 272;
const ROW_HEIGHT: i32 = 23;
const PANEL_INSET: i32 = 10;
const PANEL_HEIGHT: i32 = 5 * ROW_HEIGHT + 2 * PANEL_INSET;
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
    max_height: i32,
    bottom: i32,
    mode: Option<HintMode>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HintMode {
    Automatic,
    Selected,
    Annotate(Tool),
    EditText,
}

impl HintMode {
    fn rows(self) -> &'static [(*const u16, *const u16)] {
        match self {
            Self::Automatic => {
                const {
                    &[
                        (w!("左键"), w!("锁定自动选区")),
                        (w!("左键拖动"), w!("手动框选")),
                        (w!("Esc"), w!("退出截图")),
                    ]
                }
            }
            Self::Selected => {
                const {
                    &[
                        (w!("框内拖动"), w!("移动选区")),
                        (w!("边线拖动"), w!("调整选区大小")),
                        (w!("右键"), w!("返回自动框选")),
                        (w!("Ctrl+C"), w!("复制截图")),
                        (w!("Esc"), w!("退出截图")),
                    ]
                }
            }
            Self::Annotate(Tool::Rectangle) => {
                const {
                    &[
                        (w!("左键拖动"), w!("绘制矩形")),
                        (w!("Ctrl+1"), w!("退出矩形工具")),
                        (w!("Ctrl+5"), w!("撤回标注")),
                    ]
                }
            }
            Self::Annotate(Tool::Text) => {
                const {
                    &[
                        (w!("左键"), w!("插入文本框")),
                        (w!("左键拖动"), w!("指定文本框大小")),
                        (w!("Ctrl+2"), w!("退出文本工具")),
                    ]
                }
            }
            Self::Annotate(Tool::Brush) => {
                const {
                    &[
                        (w!("左键拖动"), w!("自由绘制")),
                        (w!("Ctrl+3"), w!("退出画笔工具")),
                        (w!("Ctrl+5"), w!("撤回标注")),
                    ]
                }
            }
            Self::Annotate(Tool::Mosaic) => {
                const {
                    &[
                        (w!("左键拖动"), w!("框选马赛克范围")),
                        (w!("Ctrl+4"), w!("退出马赛克工具")),
                        (w!("Ctrl+5"), w!("撤回标注")),
                    ]
                }
            }
            Self::EditText => {
                const {
                    &[
                        (w!("Enter"), w!("换行")),
                        (w!("Ctrl+Enter"), w!("确认文本")),
                        (w!("Esc"), w!("取消文本输入")),
                    ]
                }
            }
        }
    }
}

impl HintPanel {
    pub unsafe fn new(source: HDC, work_area: RECT) -> io::Result<Self> {
        let width = PANEL_WIDTH.min(work_area.right - work_area.left);
        let max_height = PANEL_HEIGHT.min(work_area.bottom - work_area.top);
        let left = (work_area.left + PANEL_MARGIN).min(work_area.right - width);
        let bottom = (work_area.bottom - PANEL_MARGIN).max(work_area.top + max_height);
        let surface = Surface::new(source, width, max_height)?;
        let mut hint = Self {
            bounds: RECT {
                left,
                right: left + width,
                top: bottom,
                bottom,
            },
            surface,
            max_height,
            bottom,
            mode: None,
        };
        hint.set_mode(source, HintMode::Automatic);
        Ok(hint)
    }

    pub unsafe fn set_mode(&mut self, source: HDC, mode: HintMode) -> Option<[RECT; 2]> {
        if self.mode == Some(mode) {
            return None;
        }
        let previous = self.bounds;
        self.mode = Some(mode);
        let rows = mode.rows();
        let height = (rows.len() as i32 * ROW_HEIGHT + 2 * PANEL_INSET).min(self.max_height);
        self.bounds.top = self.bottom - height;
        let width = self.bounds.right - self.bounds.left;
        let surface = &self.surface;
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
            surface.dc,
            0,
            0,
            width,
            height,
            source,
            self.bounds.left,
            self.bounds.top,
            width,
            height,
            blend,
        );
        SetDCBrushColor(surface.dc, 0x0060_6060);
        FrameRect(surface.dc, &panel, brush);
        SetBkMode(surface.dc, TRANSPARENT as i32);
        let font = CreateFontW(
            -16,
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
            ANTIALIASED_QUALITY as u32,
            DEFAULT_PITCH as u32,
            w!("Microsoft YaHei UI"),
        );
        let selected_font = if font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            font
        };
        let previous_font = SelectObject(surface.dc, selected_font);
        let split = (width * 2 / 5).min(126);
        for (index, &(key, description)) in rows.iter().enumerate() {
            let top = PANEL_INSET + index as i32 * ROW_HEIGHT;
            let mut size = RECT::default();
            DrawTextW(surface.dc, key, -1, &mut size, DT_SINGLELINE | DT_CALCRECT);
            let mut badge = RECT {
                left: (split - size.right - 14).max(6),
                top: top + 2,
                right: split,
                bottom: top + ROW_HEIGHT - 2,
            };
            SetDCBrushColor(surface.dc, 0x0036_3636);
            FillRect(surface.dc, &badge, brush);
            SetDCBrushColor(surface.dc, 0x0078_7878);
            FrameRect(surface.dc, &badge, brush);
            badge.left += 3;
            badge.right -= 7;
            SetTextColor(surface.dc, 0x00ff_ffff);
            DrawTextW(
                surface.dc,
                key,
                -1,
                &mut badge,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            let mut detail = RECT {
                left: split + 11,
                top,
                right: width - 10,
                bottom: top + ROW_HEIGHT,
            };
            SetTextColor(surface.dc, 0x00df_dfdf);
            DrawTextW(
                surface.dc,
                description,
                -1,
                &mut detail,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
        SelectObject(surface.dc, previous_font);
        if !font.is_null() {
            DeleteObject(font);
        }
        Some([previous, self.bounds])
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
    fn hint_rows_refresh_by_mode_and_keep_the_bottom_anchor() {
        unsafe {
            let area = RECT {
                right: 800,
                bottom: 600,
                ..Default::default()
            };
            let source = Surface::new(null_mut(), area.right, area.bottom).unwrap();
            let mut hint = HintPanel::new(source.dc, area).unwrap();
            let automatic = hint.bounds;
            let [before, selected] = hint.set_mode(source.dc, HintMode::Selected).unwrap();
            assert_eq!(before.top, automatic.top);
            assert_eq!(selected.bottom, automatic.bottom);
            assert_eq!(automatic.top - selected.top, 2 * ROW_HEIGHT);
            assert!(hint.set_mode(source.dc, HintMode::Selected).is_none());
            let [_, text] = hint.set_mode(source.dc, HintMode::EditText).unwrap();
            assert_eq!(text.top, automatic.top);
            assert_eq!(text.bottom, automatic.bottom);
        }
    }

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
