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
const GAP: i32 = 8;
const HEIGHT: i32 = 31;

#[derive(Clone, Copy)]
pub struct Toolbar {
    pub bounds: RECT,
    cell: i32,
}

impl Toolbar {
    pub fn new(selection: RECT, area: RECT) -> Option<Self> {
        let cell = ((area.right - area.left - GAP) / 9).min(54);
        if cell <= 0 || area.bottom - area.top < HEIGHT {
            return None;
        }
        let width = cell * 9 + GAP;
        let left = selection.left.clamp(area.left, area.right - width);
        let below = selection.bottom + GAP;
        let top = if below + HEIGHT <= area.bottom {
            below
        } else {
            (selection.top - GAP - HEIGHT).clamp(area.top, area.bottom - HEIGHT)
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
        let left = self.bounds.left + index as i32 * self.cell + if index >= 6 { GAP } else { 0 };
        RECT {
            left,
            right: left + self.cell,
            ..self.bounds
        }
    }

    pub fn hit(self, point: POINT) -> Option<Action> {
        ACTIONS
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, action)| contains(self.button(index), point).then_some(action))
    }

    pub unsafe fn paint(self, dc: HDC, active: Option<Tool>) {
        let brush = GetStockObject(DC_BRUSH);
        let previous = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00ff_ffff);
        for (index, action) in ACTIONS.iter().enumerate() {
            let mut rect = self.button(index);
            let color = if active.is_some_and(|tool| *action == Action::Tool(tool)) {
                0x00bd_6428
            } else {
                0x0026_2626
            };
            SetDCBrushColor(dc, color);
            FillRect(dc, &rect, brush);
            SetDCBrushColor(dc, 0x0040_4040);
            FrameRect(dc, &rect, brush);
            DrawTextW(
                dc,
                LABELS[index],
                -1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        SelectObject(dc, previous);
    }
}
