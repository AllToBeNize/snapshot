use crate::{annotation::Tool, selection_geometry::contains};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::*,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Tool(Tool),
    Undo,
    Redo,
    Copy,
    Save,
    Pin,
}

pub const ACTIONS: [Action; 9] = [
    Action::Tool(Tool::Rectangle),
    Action::Tool(Tool::Text),
    Action::Tool(Tool::Brush),
    Action::Tool(Tool::Mosaic),
    Action::Undo,
    Action::Redo,
    Action::Copy,
    Action::Save,
    Action::Pin,
];
const LABELS: [*const u16; 9] = [
    w!("矩形"),
    w!("文本"),
    w!("画笔"),
    w!("马赛克"),
    w!("撤回"),
    w!("重做"),
    w!("复制"),
    w!("保存"),
    w!("贴图"),
];
const OFFSET: i32 = 8;
const GROUP_GAP: i32 = 9;
const PADDING: i32 = 4;
const HEIGHT: i32 = 36;

#[derive(Clone, Copy)]
pub struct Toolbar {
    pub bounds: RECT,
    cell: i32,
}

impl Toolbar {
    pub fn new(selection: RECT, area: RECT) -> Option<Self> {
        let spacing = GROUP_GAP + PADDING * 2;
        let cell = ((area.right - area.left - spacing) / 9).min(46);
        if cell < 12 || area.bottom - area.top < HEIGHT {
            return None;
        }
        let width = cell * 9 + spacing;
        let left = (selection.right - width).clamp(area.left, area.right - width);
        let below = selection.bottom + OFFSET;
        let top = if below + HEIGHT <= area.bottom {
            below
        } else {
            (selection.top - OFFSET - HEIGHT).clamp(area.top, area.bottom - HEIGHT)
        };
        Some(Self {
            bounds: RECT {
                left,
                top,
                right: left + width,
                bottom: top + HEIGHT,
            },
            cell,
        })
    }

    fn button(self, index: usize) -> RECT {
        let left = self.bounds.left
            + PADDING
            + index as i32 * self.cell
            + if index >= 6 { GROUP_GAP } else { 0 };
        RECT {
            left,
            right: left + self.cell - 1,
            top: self.bounds.top + PADDING,
            bottom: self.bounds.bottom - PADDING,
        }
    }

    pub fn hit(self, point: POINT) -> Option<Action> {
        ACTIONS
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, action)| contains(self.button(index), point).then_some(action))
    }

    pub unsafe fn paint(self, dc: HDC, active: Option<Tool>, hovered: Option<Action>) {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let brush = GetStockObject(DC_BRUSH);
        SelectObject(dc, brush);
        SelectObject(dc, GetStockObject(DC_PEN));
        SetDCBrushColor(dc, 0x00ff_ffff);
        SetDCPenColor(dc, 0x00e5_e5e5);
        rounded(dc, self.bounds, 11);

        let divider = self.bounds.left + PADDING + self.cell * 6 + GROUP_GAP / 2;
        let line = RECT {
            left: divider,
            right: divider + 1,
            top: self.bounds.top + 10,
            bottom: self.bounds.bottom - 10,
        };
        SetDCBrushColor(dc, 0x00e9_e9e9);
        FillRect(dc, &line, brush);

        SelectObject(dc, GetStockObject(NULL_PEN));
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        SetBkMode(dc, TRANSPARENT as i32);
        for (index, action) in ACTIONS.iter().enumerate() {
            let mut rect = self.button(index);
            let selected = active.is_some_and(|tool| *action == Action::Tool(tool));
            let (background, text) = match (selected, hovered == Some(*action)) {
                (true, true) => (0x00fa_e6d8, 0x008b_4c22),
                (true, false) => (0x00fc_eee4, 0x008b_4c22),
                (false, true) => (0x00f8_f1eb, 0x0044_3328),
                (false, false) => (0x00f9_f9f9, 0x0050_4945),
            };
            SetDCBrushColor(dc, background);
            rounded(dc, rect, 7);
            SetTextColor(dc, text);
            DrawTextW(
                dc,
                LABELS[index],
                -1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
        RestoreDC(dc, saved);
    }
}

unsafe fn rounded(dc: HDC, rect: RECT, diameter: i32) {
    RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        diameter,
        diameter,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Surface;
    use std::ptr::null_mut;

    #[test]
    fn panel_is_right_aligned_and_highlights_only_the_hovered_button() {
        let area = RECT {
            right: 900,
            bottom: 600,
            ..Default::default()
        };
        let selection = RECT {
            left: 380,
            top: 120,
            right: 820,
            bottom: 390,
        };
        let toolbar = Toolbar::new(selection, area).unwrap();
        assert_eq!(toolbar.bounds.right, selection.right);
        assert_eq!(toolbar.bounds.top, selection.bottom + OFFSET);
        let first = toolbar.button(0);
        let point = POINT {
            x: first.left + 9,
            y: first.top + 3,
        };
        let hovered = toolbar.hit(point);
        assert_eq!(hovered.map(|action| action == ACTIONS[0]), Some(true));
        let divider = toolbar.button(5).right + 1 + GROUP_GAP / 2;
        assert!(toolbar
            .hit(POINT {
                x: divider,
                y: point.y
            })
            .is_none());
        unsafe {
            let surface = Surface::new(null_mut(), area.right, area.bottom).unwrap();
            toolbar.paint(surface.dc, None, hovered);
            assert_eq!(GetPixel(surface.dc, point.x, point.y), 0x00f8_f1eb);
            assert_eq!(
                GetPixel(surface.dc, point.x + toolbar.cell, point.y),
                0x00f9_f9f9
            );
            assert_eq!(
                GetPixel(surface.dc, divider, toolbar.bounds.top + 12),
                0x00e9_e9e9
            );
        }
    }
}
