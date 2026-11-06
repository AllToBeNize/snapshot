use std::{collections::HashSet, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::*,
};

use crate::{selection_geometry::intersect, text_mark::TextMark};

pub const INK: u32 = 0x0038_46ff;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rectangle,
    Text,
    Brush,
    Mosaic,
}

#[derive(Clone, Copy)]
pub struct Style {
    pub brush_width: i32,
    pub brush_color: u32,
    pub mosaic_width: i32,
    pub mosaic_block: i32,
    pub text_size: i32,
    pub text_color: u32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            brush_width: 4,
            brush_color: INK,
            mosaic_width: 32,
            mosaic_block: 12,
            text_size: 20,
            text_color: INK,
        }
    }
}

pub struct Draft {
    pub tool: Tool,
    style: Style,
    start: POINT,
    end: POINT,
    points: Vec<POINT>,
    visited: HashSet<(i32, i32)>,
    tiles: Vec<(RECT, u32)>,
}

pub enum Mark {
    Rectangle(RECT),
    Text(TextMark),
    Brush(Vec<POINT>, i32, u32),
    Mosaic(Vec<(RECT, u32)>),
}

enum Edit {
    Remove(usize),
    Insert(usize, Mark),
    ReplaceText(usize, TextMark),
}

impl Edit {
    fn selected_text(&self) -> Option<usize> {
        match self {
            Self::Insert(index, Mark::Text(_)) | Self::ReplaceText(index, _) => Some(*index),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct History {
    pub tool: Option<Tool>,
    pub draft: Option<Draft>,
    marks: Vec<Mark>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl History {
    pub fn has_marks(&self) -> bool {
        !self.marks.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn push(&mut self, mark: Mark) -> usize {
        self.redo.clear();
        self.undo.push(Edit::Remove(self.marks.len()));
        self.marks.push(mark);
        self.marks.len() - 1
    }

    pub fn text(&self, index: usize) -> Option<&TextMark> {
        match self.marks.get(index)? {
            Mark::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn text_mut(&mut self, index: usize) -> Option<&mut TextMark> {
        match self.marks.get_mut(index)? {
            Mark::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn hit_text(&self, point: POINT) -> Option<usize> {
        self.marks
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, mark)| match mark {
                Mark::Text(text) if text.contains(point) => Some(index),
                _ => None,
            })
    }

    pub fn replace_text(&mut self, index: usize, text: TextMark) {
        let Some(previous) = self.text_mut(index) else {
            return;
        };
        if *previous != text {
            let previous = std::mem::replace(previous, text);
            self.redo.clear();
            self.undo.push(Edit::ReplaceText(index, previous));
        }
    }

    pub fn remove_text(&mut self, index: usize) {
        if self.text(index).is_some() {
            let mark = self.marks.remove(index);
            self.redo.clear();
            self.undo.push(Edit::Insert(index, mark));
        }
    }

    pub fn undo(&mut self) -> Option<usize> {
        let edit = self.undo.pop()?;
        let selected = edit.selected_text();
        let inverse = self.apply(edit);
        self.redo.push(inverse);
        selected
    }

    pub fn redo(&mut self) -> Option<usize> {
        let edit = self.redo.pop()?;
        let selected = edit.selected_text();
        let inverse = self.apply(edit);
        self.undo.push(inverse);
        selected
    }

    fn apply(&mut self, edit: Edit) -> Edit {
        match edit {
            Edit::Remove(index) => Edit::Insert(index, self.marks.remove(index)),
            Edit::Insert(index, mark) => {
                self.marks.insert(index, mark);
                Edit::Remove(index)
            }
            Edit::ReplaceText(index, previous) => Edit::ReplaceText(
                index,
                std::mem::replace(self.text_mut(index).unwrap(), previous),
            ),
        }
    }

    pub unsafe fn paint(&self, dc: HDC, clip: RECT, preview: bool, hidden_text: Option<usize>) {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        IntersectClipRect(dc, clip.left, clip.top, clip.right, clip.bottom);
        for (index, mark) in self.marks.iter().enumerate() {
            if hidden_text != Some(index) {
                mark.paint(dc);
            }
        }
        if let Some(draft) = self.draft.as_ref().filter(|_| preview) {
            match draft.tool {
                Tool::Brush => stroke(
                    dc,
                    &draft.points,
                    draft.style.brush_width,
                    draft.style.brush_color,
                ),
                Tool::Mosaic => paint_tiles(dc, &draft.tiles),
                _ => outline(dc, draft.rect()),
            }
        }
        RestoreDC(dc, saved);
    }
}

impl Draft {
    pub unsafe fn new(tool: Tool, point: POINT, style: Style, clip: RECT, source: HDC) -> Self {
        let mut draft = Self {
            tool,
            style,
            start: point,
            end: point,
            points: vec![point],
            visited: HashSet::new(),
            tiles: Vec::new(),
        };
        if tool == Tool::Mosaic {
            draft.mosaic_segment(point, point, clip, source);
        }
        draft
    }

    pub unsafe fn update(&mut self, point: POINT, clip: RECT, source: HDC) -> RECT {
        let point = POINT {
            x: point.x.clamp(clip.left, clip.right - 1),
            y: point.y.clamp(clip.top, clip.bottom - 1),
        };
        let previous = self.end;
        let old_rect = self.rect();
        if (point.x, point.y) != (previous.x, previous.y) {
            self.end = point;
            match self.tool {
                Tool::Brush => self.points.push(point),
                Tool::Mosaic => self.mosaic_segment(previous, point, clip, source),
                _ => {}
            }
        }
        match self.tool {
            Tool::Brush | Tool::Mosaic => self.stroke_bounds(previous, point),
            _ => padded(join(old_rect, self.rect()), 3),
        }
    }

    pub fn start_bounds(&self) -> RECT {
        self.stroke_bounds(self.start, self.start)
    }

    fn stroke_bounds(&self, a: POINT, b: POINT) -> RECT {
        let pad = match self.tool {
            Tool::Brush => self.style.brush_width / 2 + 2,
            Tool::Mosaic => self.style.mosaic_width / 2 + self.style.mosaic_block * 2,
            _ => 3,
        };
        padded(between(a, b), pad)
    }

    unsafe fn mosaic_segment(&mut self, from: POINT, to: POINT, clip: RECT, source: HDC) {
        let block = self.style.mosaic_block;
        // A tile can touch the stroke even when its center lies outside the brush radius.
        let reach = f64::from(self.style.mosaic_width) / 2.0
            + f64::from(block) * std::f64::consts::FRAC_1_SQRT_2;
        let pad = reach.ceil() as i32;
        let dx = f64::from(to.x - from.x);
        let dy = f64::from(to.y - from.y);
        let length_squared = (dx * dx + dy * dy).max(1.0);
        let first_y = (from.y.min(to.y) - pad).max(clip.top) / block * block;
        let last_y = (from.y.max(to.y) + pad).min(clip.bottom - 1);
        for y in (first_y..=last_y).step_by(block as usize) {
            let (start, end) = if dy == 0.0 {
                (0.0, 1.0)
            } else {
                let a = f64::from(y - pad - from.y) / dy;
                let b = f64::from(y + block + pad - from.y) / dy;
                (a.min(b).clamp(0.0, 1.0), a.max(b).clamp(0.0, 1.0))
            };
            let x1 = f64::from(from.x) + dx * start;
            let x2 = f64::from(from.x) + dx * end;
            let first_x = (x1.min(x2).floor() as i32 - pad).max(clip.left) / block * block;
            let last_x = (x1.max(x2).ceil() as i32 + pad).min(clip.right - 1);
            for x in (first_x..=last_x).step_by(block as usize) {
                if self.visited.contains(&(x, y)) {
                    continue;
                }
                let tile = intersect(
                    clip,
                    RECT {
                        left: x,
                        top: y,
                        right: x + block,
                        bottom: y + block,
                    },
                )
                .unwrap();
                let cx = (tile.left + tile.right) / 2;
                let cy = (tile.top + tile.bottom) / 2;
                let x_from = f64::from(cx - from.x);
                let y_from = f64::from(cy - from.y);
                let t = ((x_from * dx + y_from * dy) / length_squared).clamp(0.0, 1.0);
                let gap_x = x_from - t * dx;
                let gap_y = y_from - t * dy;
                if gap_x * gap_x + gap_y * gap_y <= reach * reach {
                    self.visited.insert((x, y));
                    let color = GetPixel(source, cx, cy);
                    if color != CLR_INVALID {
                        self.tiles.push((tile, color));
                    }
                }
            }
        }
    }

    fn rect(&self) -> RECT {
        between(self.start, self.end)
    }

    pub fn text_rect(&self, clip: RECT) -> RECT {
        let rect = self.rect();
        if rect.right - rect.left >= 24 && rect.bottom - rect.top >= 20 {
            return rect;
        }
        let left = self.start.x.min((clip.right - 180).max(clip.left));
        let top = self.start.y.min((clip.bottom - 68).max(clip.top));
        RECT {
            left,
            top,
            right: (left + 180).min(clip.right),
            bottom: (top + 68).min(clip.bottom),
        }
    }

    pub fn finish(self) -> Option<Mark> {
        match self.tool {
            Tool::Rectangle => ((self.start.x, self.start.y) != (self.end.x, self.end.y))
                .then(|| Mark::Rectangle(self.rect())),
            Tool::Brush => Some(Mark::Brush(
                self.points,
                self.style.brush_width,
                self.style.brush_color,
            )),
            Tool::Mosaic => (!self.tiles.is_empty()).then_some(Mark::Mosaic(self.tiles)),
            Tool::Text => None,
        }
    }
}

impl Mark {
    unsafe fn paint(&self, dc: HDC) {
        match self {
            Self::Rectangle(rect) => outline(dc, *rect),
            Self::Brush(points, width, color) => stroke(dc, points, *width, *color),
            Self::Mosaic(tiles) => paint_tiles(dc, tiles),
            Self::Text(text) => text.paint(dc),
        }
    }
}

fn between(a: POINT, b: POINT) -> RECT {
    RECT {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x) + 1,
        bottom: a.y.max(b.y) + 1,
    }
}

fn join(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn padded(rect: RECT, n: i32) -> RECT {
    RECT {
        left: rect.left - n,
        top: rect.top - n,
        right: rect.right + n,
        bottom: rect.bottom + n,
    }
}

unsafe fn paint_tiles(dc: HDC, tiles: &[(RECT, u32)]) {
    let mut dirty = RECT::default();
    if GetClipBox(dc, &mut dirty) <= NULLREGION {
        return;
    }
    let brush = GetStockObject(DC_BRUSH);
    for (rect, color) in tiles {
        if intersect(*rect, dirty).is_some() {
            SetDCBrushColor(dc, *color);
            FillRect(dc, rect, brush);
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
    stroke(dc, &points, 3, INK);
}

unsafe fn stroke(dc: HDC, points: &[POINT], width: i32, color: u32) {
    if points.len() == 1 {
        let point = points[0];
        let half = width / 2;
        let brush = SelectObject(dc, GetStockObject(DC_BRUSH));
        let pen = SelectObject(dc, GetStockObject(NULL_PEN));
        SetDCBrushColor(dc, color);
        Ellipse(
            dc,
            point.x - half,
            point.y - half,
            point.x - half + width,
            point.y - half + width,
        );
        SelectObject(dc, pen);
        SelectObject(dc, brush);
        return;
    }
    let brush = LOGBRUSH {
        lbStyle: BS_SOLID,
        lbColor: color,
        lbHatch: 0,
    };
    let pen = ExtCreatePen(
        (PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND) as u32,
        width as u32,
        &brush,
        0,
        null_mut(),
    );
    if !pen.is_null() {
        let previous = SelectObject(dc, pen);
        Polyline(dc, points.as_ptr(), points.len() as i32);
        SelectObject(dc, previous);
        DeleteObject(pen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capture::Surface, text_mark::TextHandle};

    #[test]
    fn undo_and_redo_keep_text_creation_edits_and_transforms_separate() {
        let mut history = History::default();
        let rect = RECT {
            left: 10,
            top: 10,
            right: 110,
            bottom: 70,
        };
        let mut original = TextMark::new(rect, 20, INK);
        original.text = vec![65];
        let text_index = history.push(Mark::Text(original.clone()));
        history.push(Mark::Rectangle(rect));

        let mut edited = original.clone();
        edited.text.push(66);
        history.replace_text(text_index, edited.clone());
        let moved = edited.transform_drag(
            TextHandle::Move,
            POINT { x: 30, y: 30 },
            POINT { x: 50, y: 35 },
        );
        history.replace_text(text_index, moved.clone());

        assert_eq!(history.undo(), Some(text_index));
        assert!(history.text(text_index) == Some(&edited));
        assert_eq!(history.undo(), Some(text_index));
        assert!(history.text(text_index) == Some(&original));
        assert_eq!(history.marks.len(), 2);
        history.redo();
        history.redo();
        assert!(history.text(text_index) == Some(&moved));
        history.remove_text(text_index);
        assert_eq!(history.marks.len(), 1);
        assert_eq!(history.undo(), Some(text_index));
        assert!(history.text(text_index) == Some(&moved));
    }

    #[test]
    fn mosaic_stroke_covers_sparse_pointer_samples_and_only_the_brushed_area() {
        unsafe {
            let source = Surface::new(null_mut(), 120, 90).unwrap();
            let target = Surface::new(null_mut(), 120, 90).unwrap();
            let clip = RECT {
                right: 120,
                bottom: 90,
                ..Default::default()
            };
            FillRect(source.dc, &clip, GetStockObject(WHITE_BRUSH));
            FillRect(target.dc, &clip, GetStockObject(BLACK_BRUSH));
            let mut draft = Draft::new(
                Tool::Mosaic,
                POINT { x: 15, y: 40 },
                Style::default(),
                clip,
                source.dc,
            );
            draft.update(POINT { x: 105, y: 40 }, clip, source.dc);
            let mut history = History::default();
            history.push(draft.finish().unwrap());
            history.paint(target.dc, clip, false, None);
            for x in 15..=105 {
                assert_eq!(GetPixel(target.dc, x, 40), 0x00ff_ffff);
                assert_eq!(GetPixel(target.dc, x, 72), 0);
            }
            history.undo();
            assert!(!history.has_marks());
        }
    }
}
