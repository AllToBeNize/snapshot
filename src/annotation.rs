use windows_sys::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::*,
};

use crate::{capture::Surface, selection_geometry::intersect};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rectangle,
    Text,
    Brush,
    Mosaic,
}

pub struct Draft {
    pub tool: Tool,
    start: POINT,
    end: POINT,
    points: Vec<POINT>,
}

pub enum Mark {
    Rectangle(RECT),
    Text(RECT, Vec<u16>),
    Brush(Vec<POINT>),
    Mosaic(RECT, Surface, i32, i32),
}

#[derive(Default)]
pub struct History {
    pub tool: Option<Tool>,
    pub draft: Option<Draft>,
    marks: Vec<Mark>,
    undone: Vec<Mark>,
}

impl History {
    pub fn push(&mut self, mark: Mark) {
        self.undone.clear();
        self.marks.push(mark);
    }

    pub fn undo(&mut self) {
        if let Some(mark) = self.marks.pop() {
            self.undone.push(mark);
        }
    }

    pub fn redo(&mut self) {
        if let Some(mark) = self.undone.pop() {
            self.marks.push(mark);
        }
    }

    pub unsafe fn paint(&self, dc: HDC, clip: RECT, preview: bool) {
        if self.marks.is_empty() && (!preview || self.draft.is_none()) {
            return;
        }
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        IntersectClipRect(dc, clip.left, clip.top, clip.right, clip.bottom);
        for mark in &self.marks {
            mark.paint(dc);
        }
        if preview {
            if let Some(draft) = &self.draft {
                match draft.tool {
                    Tool::Brush => stroke(dc, &draft.points),
                    _ => outline(dc, draft.rect()),
                }
            }
        }
        RestoreDC(dc, saved);
    }
}

impl Draft {
    pub fn new(tool: Tool, point: POINT) -> Self {
        Self {
            tool,
            start: point,
            end: point,
            points: vec![point],
        }
    }

    pub fn update(&mut self, point: POINT, clip: RECT) {
        let point = POINT {
            x: point.x.clamp(clip.left, clip.right - 1),
            y: point.y.clamp(clip.top, clip.bottom - 1),
        };
        if point.x != self.end.x || point.y != self.end.y {
            self.end = point;
            if self.tool == Tool::Brush {
                self.points.push(point);
            }
        }
    }

    pub fn rect(&self) -> RECT {
        RECT {
            left: self.start.x.min(self.end.x),
            top: self.start.y.min(self.end.y),
            right: self.start.x.max(self.end.x) + 1,
            bottom: self.start.y.max(self.end.y) + 1,
        }
    }

    pub fn text_rect(&self, clip: RECT) -> RECT {
        let rect = self.rect();
        if rect.right - rect.left < 24 || rect.bottom - rect.top < 20 {
            let left = self.start.x.min((clip.right - 180).max(clip.left));
            let top = self.start.y.min((clip.bottom - 54).max(clip.top));
            RECT {
                left,
                top,
                right: (left + 180).min(clip.right),
                bottom: (top + 54).min(clip.bottom),
            }
        } else {
            rect
        }
    }

    pub unsafe fn finish(self, source: HDC) -> std::io::Result<Mark> {
        let rect = self.rect();
        match self.tool {
            Tool::Rectangle => Ok(Mark::Rectangle(rect)),
            Tool::Brush => Ok(Mark::Brush(self.points)),
            Tool::Mosaic => {
                let width = ((rect.right - rect.left) as u32).div_ceil(12) as i32;
                let height = ((rect.bottom - rect.top) as u32).div_ceil(12) as i32;
                let tiles = Surface::new(source, width, height)?;
                SetStretchBltMode(tiles.dc, COLORONCOLOR);
                if StretchBlt(
                    tiles.dc,
                    0,
                    0,
                    width,
                    height,
                    source,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SRCCOPY,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Mark::Mosaic(rect, tiles, width, height))
            }
            Tool::Text => unreachable!("text is committed by the editor"),
        }
    }
}

impl Mark {
    unsafe fn paint(&self, dc: HDC) {
        match self {
            Self::Rectangle(rect) => outline(dc, *rect),
            Self::Brush(points) => stroke(dc, points),
            Self::Mosaic(rect, tiles, width, height) => {
                SetStretchBltMode(dc, COLORONCOLOR);
                StretchBlt(
                    dc,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    tiles.dc,
                    0,
                    0,
                    *width,
                    *height,
                    SRCCOPY,
                );
            }
            Self::Text(bounds, text) => {
                FillRect(dc, bounds, GetStockObject(WHITE_BRUSH));
                let previous = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(dc, 0);
                let mut text_rect = intersect(
                    *bounds,
                    RECT {
                        left: bounds.left + 3,
                        top: bounds.top + 2,
                        ..*bounds
                    },
                )
                .unwrap_or(*bounds);
                DrawTextW(
                    dc,
                    text.as_ptr(),
                    text.len() as i32,
                    &mut text_rect,
                    DT_WORDBREAK | DT_NOPREFIX,
                );
                SelectObject(dc, previous);
            }
        }
    }
}

unsafe fn outline(dc: HDC, rect: RECT) {
    let points = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right - 1,
            y: rect.top,
        },
        POINT {
            x: rect.right - 1,
            y: rect.bottom - 1,
        },
        POINT {
            x: rect.left,
            y: rect.bottom - 1,
        },
        POINT {
            x: rect.left,
            y: rect.top,
        },
    ];
    stroke(dc, &points);
}

unsafe fn stroke(dc: HDC, points: &[POINT]) {
    const INK: u32 = 0x0038_46ff;
    let pen = CreatePen(PS_SOLID, 3, INK);
    if pen.is_null() {
        return;
    }
    let previous = SelectObject(dc, pen);
    if points.len() == 1 {
        let point = points[0];
        MoveToEx(dc, point.x, point.y, std::ptr::null_mut());
        LineTo(dc, point.x + 1, point.y);
    } else {
        Polyline(dc, points.as_ptr(), points.len() as i32);
    }
    SelectObject(dc, previous);
    DeleteObject(pen);
}
