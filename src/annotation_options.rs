use windows_sys::{
    core::w,
    Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::*,
    },
};

use crate::{
    annotation::{Style, Tool, INK},
    selection_geometry::{contains, intersect},
    toolbar::paint_panel,
    typography::{self, TextStyle},
    wide,
};

const HEIGHT: i32 = 32;
const GAP: i32 = 4;
const PADDING: i32 = 5;
const FIELD: i32 = 104;
const COLORS: [u32; 8] = [
    INK,
    0x0031_91ff,
    0x0000_c8f3,
    0x0061_a825,
    0x00ed_7a25,
    0x00c4_4e84,
    0x0022_2222,
    0x00ff_ffff,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Primary(i32),
    Block(i32),
    Color(u32),
}

impl Choice {
    pub fn adjust(self, tool: Tool, style: &mut Style) {
        match (tool, self) {
            (Tool::Brush, Self::Primary(step)) => {
                style.brush_width = (style.brush_width + step).clamp(1, 64)
            }
            (Tool::Mosaic, Self::Primary(step)) => {
                style.mosaic_width = (style.mosaic_width + step * 4).clamp(4, 128)
            }
            (Tool::Mosaic, Self::Block(step)) => {
                style.mosaic_block = (style.mosaic_block + step * 2).clamp(2, 48)
            }
            (Tool::Text, Self::Primary(step)) => {
                style.text_size = (style.text_size + step * 2).clamp(8, 128)
            }
            (Tool::Text, Self::Color(color)) => style.text_color = color,
            (Tool::Brush, Self::Color(color)) => style.brush_color = color,
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
pub struct Options {
    pub bounds: RECT,
    tool: Tool,
    swatch: i32,
}

impl Options {
    pub fn new(tool: Tool, toolbar: RECT, area: RECT, selection: RECT) -> Option<Self> {
        let available = area.right - area.left;
        let swatch = ((available - FIELD - PADDING * 2) / COLORS.len() as i32).min(22);
        let width = match tool {
            Tool::Rectangle => return None,
            Tool::Mosaic => FIELD * 2,
            _ if swatch >= 12 => FIELD + swatch * COLORS.len() as i32,
            _ => return None,
        } + PADDING * 2;
        if width > available {
            return None;
        }
        let left = (toolbar.right - width).clamp(area.left, area.right - width);
        let below = toolbar.bottom + GAP;
        let above = toolbar.top - GAP - HEIGHT;
        let top = if toolbar.bottom <= selection.top && above >= area.top {
            above
        } else if below + HEIGHT <= area.bottom {
            below
        } else if above >= area.top {
            above
        } else {
            return None;
        };
        Some(Self {
            bounds: RECT {
                left,
                top,
                right: left + width,
                bottom: top + HEIGHT,
            },
            tool,
            swatch,
        })
    }

    pub fn near_text(mut self, frame: RECT, toolbar: RECT, area: RECT) -> Option<Self> {
        let width = self.bounds.right - self.bounds.left;
        if area.right - area.left < width || area.bottom - area.top < HEIGHT {
            return None;
        }
        let left = frame.left.clamp(area.left, area.right - width);
        let top = frame.top.clamp(area.top, area.bottom - HEIGHT);
        let candidates = [
            (left, frame.bottom + GAP),
            (left, frame.top - GAP - HEIGHT),
            (frame.right + GAP, top),
            (frame.left - GAP - width, top),
        ];
        self.bounds = candidates
            .into_iter()
            .map(|(left, top)| RECT {
                left,
                top,
                right: left + width,
                bottom: top + HEIGHT,
            })
            .find(|bounds| {
                bounds.left >= area.left
                    && bounds.top >= area.top
                    && bounds.right <= area.right
                    && bounds.bottom <= area.bottom
                    && intersect(*bounds, toolbar).is_none()
            })?;
        Some(self)
    }

    fn cell(self, left: i32, width: i32) -> RECT {
        let left = self.bounds.left + PADDING + left;
        RECT {
            left,
            top: self.bounds.top + 4,
            right: left + width,
            bottom: self.bounds.bottom - 4,
        }
    }

    fn buttons(self) -> impl Iterator<Item = (RECT, Choice)> {
        let primary = [
            (self.cell(31, 22), Choice::Primary(-1)),
            (self.cell(81, 22), Choice::Primary(1)),
        ];
        let block = [
            (self.cell(FIELD + 31, 22), Choice::Block(-1)),
            (self.cell(FIELD + 81, 22), Choice::Block(1)),
        ];
        let colors = COLORS.into_iter().enumerate().map(move |(index, color)| {
            (
                self.cell(FIELD + self.swatch * index as i32, self.swatch),
                Choice::Color(color),
            )
        });
        let mosaic = self.tool == Tool::Mosaic;
        primary
            .into_iter()
            .chain(block.into_iter().filter(move |_| mosaic))
            .chain(colors.filter(move |_| !mosaic))
    }

    pub fn hit(self, point: POINT) -> Option<Choice> {
        self.buttons()
            .find_map(|(rect, choice)| contains(rect, point).then_some(choice))
    }

    pub unsafe fn paint(self, dc: HDC, style: Style, hovered: Option<Choice>) {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        paint_panel(dc, self.bounds);
        SelectObject(dc, typography::font(TextStyle::HintDescription));
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x0033_3333);
        let (name, value, color) = match self.tool {
            Tool::Brush => (w!("粗细"), style.brush_width, style.brush_color),
            Tool::Mosaic => (w!("粗细"), style.mosaic_width, 0),
            Tool::Text => (w!("字号"), style.text_size, style.text_color),
            Tool::Rectangle => unreachable!(),
        };
        label(dc, self.cell(0, 31), name);
        label(dc, self.cell(53, 28), wide(&value.to_string()).as_ptr());
        if self.tool == Tool::Mosaic {
            label(dc, self.cell(FIELD, 31), w!("像素"));
            label(
                dc,
                self.cell(FIELD + 53, 28),
                wide(&style.mosaic_block.to_string()).as_ptr(),
            );
        }
        for (mut rect, choice) in self.buttons() {
            let hover = hovered == Some(choice);
            SelectObject(dc, GetStockObject(DC_BRUSH));
            SelectObject(dc, GetStockObject(DC_PEN));
            match choice {
                Choice::Color(swatch) => {
                    InflateRect(&mut rect, -2, -4);
                    SetDCBrushColor(dc, swatch);
                    SetDCPenColor(
                        dc,
                        if swatch == color || hover {
                            0x00dd_852b
                        } else {
                            0x00d0_d0d0
                        },
                    );
                    RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, 4, 4);
                    if swatch == color {
                        InflateRect(&mut rect, 1, 1);
                        SetDCBrushColor(dc, 0x00dd_852b);
                        FrameRect(dc, &rect, GetStockObject(DC_BRUSH));
                    }
                }
                Choice::Primary(step) | Choice::Block(step) => {
                    if hover {
                        SetDCBrushColor(dc, 0x00f0_f0f0);
                        SetDCPenColor(dc, 0x00f0_f0f0);
                        RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, 5, 5);
                    }
                    label(dc, rect, if step < 0 { w!("−") } else { w!("+") });
                }
            }
        }
        RestoreDC(dc, saved);
    }
}

unsafe fn label(dc: HDC, mut rect: RECT, text: *const u16) {
    DrawTextW(
        dc,
        text,
        -1,
        &mut rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
}
