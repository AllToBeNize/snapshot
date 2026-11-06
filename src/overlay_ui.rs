use std::{io, mem::size_of};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::*,
    },
};

use crate::{
    annotation::Tool,
    capture::Surface,
    hotkey::{self, Binding},
    icons::{self, Icon},
    selection_geometry::intersect,
    typography::{self, TextStyle},
};

const PANEL_WIDTH: i32 = 360;
const ROW_HEIGHT: i32 = 25;
const PANEL_INSET: i32 = 8;
const PANEL_HEIGHT: i32 = 5 * ROW_HEIGHT + 2 * PANEL_INSET;
const PANEL_MARGIN: i32 = 12;
const KEYCAP_HEIGHT: i32 = 18;
const KEYCAP_GAP: i32 = 3;
const COLUMN_GAP: i32 = 11;
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
    max_width: i32,
    max_height: i32,
    bottom: i32,
    mode: Option<HintMode>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HintMode {
    Automatic,
    Selected,
    Annotate(Tool),
    SelectedText,
    EditText,
}

impl HintMode {
    fn rows(self) -> &'static [HintRow] {
        use HintInput::{Bound, Key, Mouse};
        match self {
            Self::Automatic => {
                const {
                    &[
                        (Mouse(Icon::MouseLeft), w!("锁定自动选区")),
                        (Mouse(Icon::Drag), w!("手动拖出选区")),
                        (
                            HintInput::MouseOrKey(Icon::MouseRight, "Esc"),
                            w!("退出截图"),
                        ),
                    ]
                }
            }
            Self::Selected => {
                const {
                    &[
                        (Mouse(Icon::Move), w!("拖动框内移动")),
                        (Mouse(Icon::Resize), w!("拖动边线缩放")),
                        (Mouse(Icon::MouseRight), w!("返回自动框选")),
                        (Bound(Binding::Copy), w!("复制截图")),
                        (Key("Esc"), w!("退出截图")),
                    ]
                }
            }
            Self::Annotate(Tool::Rectangle) => {
                const {
                    &[
                        (Mouse(Icon::Drag), w!("绘制矩形")),
                        (Bound(Binding::Rectangle), w!("退出矩形工具")),
                        (Bound(Binding::Undo), w!("撤回标注")),
                    ]
                }
            }
            Self::Annotate(Tool::Text) => {
                const {
                    &[
                        (Mouse(Icon::MouseLeft), w!("插入文本框")),
                        (Mouse(Icon::Drag), w!("指定文本框大小")),
                        (Bound(Binding::Text), w!("退出文本工具")),
                    ]
                }
            }
            Self::Annotate(Tool::Brush) => {
                const {
                    &[
                        (Mouse(Icon::Drag), w!("自由绘制")),
                        (Bound(Binding::Brush), w!("退出画笔工具")),
                        (Bound(Binding::Undo), w!("撤回标注")),
                    ]
                }
            }
            Self::Annotate(Tool::Mosaic) => {
                const {
                    &[
                        (Mouse(Icon::Drag), w!("涂抹马赛克")),
                        (Bound(Binding::Mosaic), w!("退出马赛克工具")),
                        (Bound(Binding::Undo), w!("撤回标注")),
                    ]
                }
            }
            Self::SelectedText => {
                const {
                    &[
                        (Mouse(Icon::MouseLeft), w!("双击文本框编辑")),
                        (Mouse(Icon::Move), w!("拖动文本框移动")),
                        (Mouse(Icon::Resize), w!("拖动四角缩放")),
                        (Mouse(Icon::Drag), w!("拖动上方圆点旋转")),
                        (Bound(Binding::Undo), w!("撤回上一步")),
                    ]
                }
            }
            Self::EditText => {
                const {
                    &[
                        (Key("Enter"), w!("换行")),
                        (Key("Ctrl+Enter"), w!("确认文本")),
                        (Key("Esc"), w!("取消文本输入")),
                    ]
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum HintInput {
    Mouse(Icon),
    MouseOrKey(Icon, &'static str),
    Key(&'static str),
    Bound(Binding),
}

type HintRow = (HintInput, *const u16);

enum Keycap {
    Mouse(Icon),
    Text(Vec<u16>),
    Separator,
}

impl HintInput {
    fn keycaps(self, bindings: hotkey::Bindings) -> Vec<Keycap> {
        let label;
        let chord = match self {
            Self::Mouse(icon) => return vec![Keycap::Mouse(icon)],
            Self::MouseOrKey(icon, key) => {
                return vec![
                    Keycap::Mouse(icon),
                    Keycap::Separator,
                    Keycap::Text(crate::wide(key)),
                ];
            }
            Self::Key(chord) => chord,
            Self::Bound(binding) => {
                label = bindings.get(binding).label();
                &label
            }
        };
        chord
            .split('+')
            .map(|part| Keycap::Text(crate::wide(part)))
            .collect()
    }
}

impl Keycap {
    unsafe fn width(&self, dc: HDC) -> i32 {
        match self {
            Self::Mouse(_) => KEYCAP_HEIGHT,
            Self::Text(text) => (text_width(dc, text.as_ptr()) + 8).max(KEYCAP_HEIGHT),
            Self::Separator => 6,
        }
    }

    unsafe fn paint(&self, dc: HDC, rect: RECT) {
        if !matches!(self, Self::Separator) {
            SetDCBrushColor(dc, 0x00b0_b0b0);
            FrameRect(dc, &rect, GetStockObject(DC_BRUSH));
        }
        match self {
            Self::Mouse(icon) => icons::paint(
                dc,
                RECT {
                    left: rect.left + 3,
                    top: rect.top + 2,
                    right: rect.right - 3,
                    bottom: rect.bottom - 2,
                },
                *icon,
                0x00ff_ffff,
            ),
            Self::Text(_) | Self::Separator => {
                let mut text_rect = rect;
                SetTextColor(dc, 0x00ff_ffff);
                let label = match self {
                    Self::Text(text) => text.as_ptr(),
                    _ => w!("/"),
                };
                DrawTextW(
                    dc,
                    label,
                    -1,
                    &mut text_rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
        }
    }
}

unsafe fn text_width(dc: HDC, text: *const u16) -> i32 {
    let mut bounds = RECT::default();
    DrawTextW(
        dc,
        text,
        -1,
        &mut bounds,
        DT_SINGLELINE | DT_CALCRECT | DT_NOPREFIX,
    );
    bounds.right
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
            max_width: width,
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
        let surface = &self.surface;
        let description_font = typography::font(TextStyle::HintDescription);
        let key_font = typography::font(TextStyle::Ui);
        let previous_font = SelectObject(surface.dc, description_font);
        let description_column = rows
            .iter()
            .map(|&(_, description)| text_width(surface.dc, description))
            .max()
            .unwrap_or_default();
        SelectObject(surface.dc, key_font);
        let bindings = hotkey::current();
        let mut key_column = 0;
        let lines: Vec<_> = rows
            .iter()
            .map(|&(input, description)| {
                let caps: Vec<_> = input
                    .keycaps(bindings)
                    .into_iter()
                    .map(|cap| {
                        let width = cap.width(surface.dc);
                        (cap, width)
                    })
                    .collect();
                let cap_width = caps
                    .iter()
                    .map(|(_, width)| width + KEYCAP_GAP)
                    .sum::<i32>()
                    - KEYCAP_GAP;
                key_column = key_column.max(cap_width);
                (caps, cap_width, description)
            })
            .collect();
        let width =
            (2 * PANEL_INSET + key_column + COLUMN_GAP + description_column).min(self.max_width);
        self.bounds.right = self.bounds.left + width;
        let panel = RECT {
            right: width,
            bottom: height,
            ..Default::default()
        };
        FillRect(surface.dc, &panel, GetStockObject(BLACK_BRUSH));
        let blend = BLENDFUNCTION {
            SourceConstantAlpha: 51,
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
        let brush = GetStockObject(DC_BRUSH);
        SetDCBrushColor(surface.dc, 0x0057_5757);
        FrameRect(surface.dc, &panel, brush);
        SetBkMode(surface.dc, TRANSPARENT as i32);
        let split = (PANEL_INSET + key_column).min(width.saturating_sub(2 * PANEL_INSET));
        for (index, (caps, cap_width, description)) in lines.into_iter().enumerate() {
            let top = PANEL_INSET + index as i32 * ROW_HEIGHT;
            let cap_top = top + (ROW_HEIGHT - KEYCAP_HEIGHT) / 2;
            let mut x = split - cap_width;
            for (cap, width) in caps {
                if x >= PANEL_INSET {
                    cap.paint(
                        surface.dc,
                        RECT {
                            left: x,
                            top: cap_top,
                            right: x + width,
                            bottom: cap_top + KEYCAP_HEIGHT,
                        },
                    );
                }
                x += width + KEYCAP_GAP;
            }
            let mut detail = RECT {
                left: split + COLUMN_GAP,
                top,
                right: width - PANEL_INSET,
                bottom: top + ROW_HEIGHT,
            };
            SelectObject(surface.dc, description_font);
            SetTextColor(surface.dc, 0x00f4_f4f4);
            DrawTextW(
                surface.dc,
                description,
                -1,
                &mut detail,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            SelectObject(surface.dc, key_font);
        }
        SelectObject(surface.dc, previous_font);
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
            let mut custom = hotkey::DEFAULT;
            custom.set(
                Binding::Copy,
                hotkey::Shortcut::parse("Ctrl+Alt+Shift+F10").unwrap(),
            );
            hotkey::activate(custom);
            let [_, wider] = hint.set_mode(source.dc, HintMode::Selected).unwrap();
            hotkey::activate(hotkey::DEFAULT);
            assert_eq!(wider.left, selected.left);
            assert_eq!(wider.bottom, selected.bottom);
            assert!(wider.right > selected.right);
            assert!(wider.right <= area.right);
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
