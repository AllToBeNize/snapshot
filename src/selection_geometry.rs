use windows_sys::Win32::Foundation::{POINT, RECT};

pub const HANDLE_RADIUS: i32 = 4;
pub const HANDLES: [Handle; 8] = [
    Handle(-1, -1),
    Handle(0, -1),
    Handle(1, -1),
    Handle(1, 0),
    Handle(1, 1),
    Handle(0, 1),
    Handle(-1, 1),
    Handle(-1, 0),
];

#[derive(Clone, Copy)]
pub struct Handle(pub i32, pub i32);

#[derive(Clone, Copy)]
pub enum DragKind {
    Draw,
    Move,
    Resize(Handle),
}

pub fn contains(rect: RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

pub fn intersect(first: RECT, second: RECT) -> Option<RECT> {
    let rect = RECT {
        left: first.left.max(second.left),
        top: first.top.max(second.top),
        right: first.right.min(second.right),
        bottom: first.bottom.min(second.bottom),
    };
    (rect.left < rect.right && rect.top < rect.bottom).then_some(rect)
}

impl Handle {
    pub fn bounds(self, rect: RECT, canvas: RECT) -> RECT {
        let center = |start, end, edge| match edge {
            -1 => start,
            1 => end,
            _ => start + (end - start) / 2,
        };
        let x = center(rect.left, rect.right, self.0).clamp(
            canvas.left + HANDLE_RADIUS,
            canvas.right - HANDLE_RADIUS - 1,
        );
        let y = center(rect.top, rect.bottom, self.1).clamp(
            canvas.top + HANDLE_RADIUS,
            canvas.bottom - HANDLE_RADIUS - 1,
        );
        RECT {
            left: x - HANDLE_RADIUS,
            top: y - HANDLE_RADIUS,
            right: x + HANDLE_RADIUS + 1,
            bottom: y + HANDLE_RADIUS + 1,
        }
    }
}

pub fn hit_drag(rect: RECT, canvas: RECT, point: POINT) -> DragKind {
    if rect.left < rect.right && rect.top < rect.bottom {
        for handle in HANDLES {
            if contains(handle.bounds(rect, canvas), point) {
                return DragKind::Resize(handle);
            }
        }
        if contains(rect, point) {
            return DragKind::Move;
        }
    }
    DragKind::Draw
}

pub fn drag_rect(kind: DragKind, rect: RECT, canvas: RECT, start: POINT, point: POINT) -> RECT {
    let dx = point.x - start.x;
    let dy = point.y - start.y;
    match kind {
        DragKind::Draw => {
            let x = point.x.clamp(canvas.left, canvas.right - 1);
            let y = point.y.clamp(canvas.top, canvas.bottom - 1);
            RECT {
                left: x.min(start.x),
                top: y.min(start.y),
                right: x.max(start.x) + 1,
                bottom: y.max(start.y) + 1,
            }
        }
        DragKind::Move => {
            let dx = dx.clamp(canvas.left - rect.left, canvas.right - rect.right);
            let dy = dy.clamp(canvas.top - rect.top, canvas.bottom - rect.bottom);
            RECT {
                left: rect.left + dx,
                top: rect.top + dy,
                right: rect.right + dx,
                bottom: rect.bottom + dy,
            }
        }
        DragKind::Resize(Handle(x, y)) => RECT {
            left: if x == -1 {
                (rect.left + dx).clamp(canvas.left, rect.right - 1)
            } else {
                rect.left
            },
            top: if y == -1 {
                (rect.top + dy).clamp(canvas.top, rect.bottom - 1)
            } else {
                rect.top
            },
            right: if x == 1 {
                (rect.right + dx).clamp(rect.left + 1, canvas.right)
            } else {
                rect.right
            },
            bottom: if y == 1 {
                (rect.bottom + dy).clamp(rect.top + 1, canvas.bottom)
            } else {
                rect.bottom
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn all_eight_handles_hit_and_resize_the_expected_edges() {
        let canvas = rect(0, 0, 300, 300);
        let selection = rect(50, 50, 150, 150);
        for handle in HANDLES {
            let bounds = handle.bounds(selection, canvas);
            let start = POINT {
                x: bounds.left + HANDLE_RADIUS,
                y: bounds.top + HANDLE_RADIUS,
            };
            let kind = hit_drag(selection, canvas, start);
            let next = drag_rect(
                kind,
                selection,
                canvas,
                start,
                POINT {
                    x: start.x + 10,
                    y: start.y + 15,
                },
            );
            assert_eq!(next.left, 50 + 10 * i32::from(handle.0 == -1));
            assert_eq!(next.top, 50 + 15 * i32::from(handle.1 == -1));
            assert_eq!(next.right, 150 + 10 * i32::from(handle.0 == 1));
            assert_eq!(next.bottom, 150 + 15 * i32::from(handle.1 == 1));
        }
    }

    #[test]
    fn moving_stops_at_canvas_without_shrinking_the_selection() {
        let canvas = rect(0, 0, 300, 300);
        let selection = rect(50, 50, 150, 150);
        let start = POINT { x: 100, y: 100 };
        let moved = drag_rect(
            hit_drag(selection, canvas, start),
            selection,
            canvas,
            start,
            POINT { x: -20, y: 400 },
        );
        assert_eq!(
            (moved.left, moved.top, moved.right, moved.bottom),
            (0, 200, 100, 300)
        );
    }
}
