use std::{
    cell::RefCell,
    ffi::OsString,
    io,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        UI::{
            Controls::{
                Dialogs::{
                    CommDlgExtendedError, GetSaveFileNameW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
                    OFN_PATHMUSTEXIST, OPENFILENAMEW,
                },
                WM_MOUSELEAVE,
            },
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
                TRACKMOUSEEVENT, VK_CONTROL, VK_ESCAPE, VK_RETURN,
            },
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    capture::{Desktop, Surface},
    overlay_ui::{monitor_info, HintPanel, Loupe},
    selection_geometry::{contains, drag_rect, hit_drag, intersect, DragKind, Handle, HANDLES},
    show_error,
    targets::{self, ElementProbe, Request, WindowTarget, ELEMENT_READY},
};

const CLASS: *const u16 = w!("Snapshot.Selection");
const BORDER_BLUE: u32 = 0x00ff_8a26;

thread_local! {
    static SESSION: RefCell<Option<Selection>> = const { RefCell::new(None) };
}

struct Selection {
    desktop: Desktop,
    dimmed: Surface,
    back_buffer: Surface,
    canvas: RECT,
    targets: Vec<WindowTarget>,
    probe: Option<ElementProbe>,
    target: Option<usize>,
    pointer: Option<POINT>,
    automatic: bool,
    drag: Option<Drag>,
    rect: RECT,
    hint: HintPanel,
    loupe: Option<Loupe>,
    pointer_monitor: RECT,
}

struct Drag {
    anchor: POINT,
    previous: RECT,
    kind: DragKind,
    started: bool,
    copy_on_click: bool,
}

#[derive(Clone, Copy)]
struct Outline {
    rect: RECT,
    handles: bool,
}

impl Selection {
    fn outline(&self) -> Outline {
        Outline {
            rect: self.rect,
            handles: !self.automatic,
        }
    }
}

pub unsafe fn register(instance: HINSTANCE) -> io::Result<()> {
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: LoadCursorW(null_mut(), IDC_CROSS),
        lpszClassName: CLASS,
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub unsafe fn open(instance: HINSTANCE) -> io::Result<()> {
    if SESSION.with_borrow(Option::is_some) {
        return Ok(());
    }
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor);
    let targets = targets::windows_at_capture();
    let desktop = Desktop::capture()?;
    let bounds = desktop.bounds;
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let canvas = RECT {
        right: width,
        bottom: height,
        ..Default::default()
    };
    let source = desktop.surface.dc;
    let monitor = monitor_info(cursor);
    let work_area = monitor
        .and_then(|info| overlay_rect(info.rcWork, bounds))
        .unwrap_or(canvas);
    let pointer_monitor = monitor
        .and_then(|info| overlay_rect(info.rcMonitor, bounds))
        .unwrap_or(canvas);
    let hint = HintPanel::new(source, work_area)?;
    let dimmed = Surface::new(source, width, height)?;
    FillRect(dimmed.dc, &canvas, GetStockObject(BLACK_BRUSH));
    let blend = BLENDFUNCTION {
        SourceConstantAlpha: 102,
        ..Default::default()
    };
    if AlphaBlend(
        dimmed.dc, 0, 0, width, height, source, 0, 0, width, height, blend,
    ) == 0
    {
        return Err(io::Error::last_os_error());
    }
    let back_buffer = Surface::new(source, width, height)?;
    let window = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        CLASS,
        w!("Snapshot Selection"),
        WS_POPUP,
        bounds.left,
        bounds.top,
        bounds.right - bounds.left,
        bounds.bottom - bounds.top,
        null_mut(),
        null_mut(),
        instance,
        null(),
    );
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    SESSION.replace(Some(Selection {
        desktop,
        dimmed,
        back_buffer,
        canvas,
        targets,
        probe: ElementProbe::start(window),
        target: None,
        pointer: None,
        automatic: true,
        drag: None,
        rect: RECT::default(),
        hint,
        loupe: None,
        pointer_monitor,
    }));
    hover(
        window,
        POINT {
            x: cursor.x - bounds.left,
            y: cursor.y - bounds.top,
        },
    );
    ShowWindow(window, SW_SHOW);
    SetForegroundWindow(window);
    SetFocus(window);
    UpdateWindow(window);
    Ok(())
}

fn overlay_rect(target: RECT, desktop: RECT) -> Option<RECT> {
    intersect(target, desktop).map(|clipped| RECT {
        left: clipped.left - desktop.left,
        top: clipped.top - desktop.top,
        right: clipped.right - desktop.left,
        bottom: clipped.bottom - desktop.top,
    })
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => paint(window),
        WM_ERASEBKGND => return 1,
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            SESSION.with_borrow_mut(|session| {
                if let Some(selection) = session {
                    let point = mouse_point(lparam);
                    if !contains(selection.canvas, point) {
                        return;
                    }
                    selection.drag = Some(Drag {
                        anchor: point,
                        previous: selection.rect,
                        kind: if selection.automatic {
                            DragKind::Draw
                        } else {
                            hit_drag(selection.rect, selection.canvas, point)
                        },
                        started: false,
                        copy_on_click: message == WM_LBUTTONDBLCLK,
                    });
                }
            });
            SetCapture(window);
            update_loupe(window, mouse_point(lparam));
        }
        WM_MOUSEMOVE => {
            let point = mouse_point(lparam);
            if !update_drag(window, point, false) {
                hover(window, point);
            }
            update_cursor(point);
            update_loupe(window, point);
        }
        WM_LBUTTONUP => {
            let point = mouse_point(lparam);
            update_drag(window, point, true);
            update_cursor(point);
            update_loupe(window, point);
        }
        WM_RBUTTONUP => reset_automatic(window, mouse_point(lparam)),
        WM_MOUSELEAVE => {
            if SESSION.with_borrow(|session| {
                session
                    .as_ref()
                    .is_some_and(|selection| selection.drag.is_none())
            }) {
                hide_loupe(window);
            }
        }
        ELEMENT_READY => refine_target(window),
        WM_SETCURSOR => {
            let mut point = POINT::default();
            if GetCursorPos(&mut point) != 0 && ScreenToClient(window, &mut point) != 0 {
                update_cursor(point);
                return 1;
            }
        }
        WM_CAPTURECHANGED => {
            let interrupted = SESSION.with_borrow_mut(|session| {
                session
                    .as_mut()
                    .and_then(|selection| selection.drag.take())
                    .is_some()
            });
            if interrupted {
                hide_loupe(window);
            }
        }
        WM_KEYDOWN => match wparam as u16 {
            VK_ESCAPE => {
                DestroyWindow(window);
            }
            VK_RETURN => output(window, false),
            key if GetKeyState(VK_CONTROL as i32) < 0 && key == b'C' as u16 => {
                output(window, false)
            }
            key if GetKeyState(VK_CONTROL as i32) < 0 && key == b'S' as u16 => output(window, true),
            _ => {}
        },
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_CLOSE => {
            DestroyWindow(window);
        }
        WM_DESTROY => {
            SESSION.take();
        }
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}

fn mouse_point(lparam: LPARAM) -> POINT {
    POINT {
        x: i32::from(lparam as i16),
        y: i32::from((lparam >> 16) as i16),
    }
}

unsafe fn update_drag(window: HWND, point: POINT, end: bool) -> bool {
    let dirty = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.outline();
        let drag = selection.drag.as_mut()?;
        drag.started |=
            (drag.anchor.x - point.x).abs() >= 2 || (drag.anchor.y - point.y).abs() >= 2;
        let inside = contains(selection.rect, point);
        let copy = end && drag.copy_on_click && !drag.started && inside;
        if drag.started {
            selection.rect = drag_rect(
                drag.kind,
                drag.previous,
                selection.canvas,
                drag.anchor,
                point,
            );
            selection.automatic = false;
        } else if end && inside {
            selection.automatic = false;
        }
        if end {
            selection.drag = None;
        }
        Some((previous, selection.outline(), selection.canvas, copy))
    });
    if let Some((old, new, canvas, copy)) = dirty {
        invalidate_change(window, old, new, canvas);
        if end {
            ReleaseCapture();
        }
        if copy {
            output(window, false);
        }
    }
    dirty.is_some()
}

unsafe fn hover(window: HWND, point: POINT) {
    let change = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        if !selection.automatic
            || selection.drag.is_some()
            || selection
                .pointer
                .is_some_and(|last| last.x == point.x && last.y == point.y)
        {
            return None;
        }
        selection.pointer = Some(point);
        let desktop = selection.desktop.bounds;
        let screen_point = POINT {
            x: point.x + desktop.left,
            y: point.y + desktop.top,
        };
        let target = selection
            .targets
            .iter()
            .copied()
            .find(|target| contains(target.rect, screen_point));
        let previous = selection.outline();
        let id = target.map(|target| target.id);
        if id != selection.target || !contains(selection.rect, point) {
            selection.rect = target
                .and_then(|target| overlay_rect(target.rect, desktop))
                .unwrap_or_default();
        }
        selection.target = id;
        if let (Some(probe), Some(target)) = (&selection.probe, target) {
            probe.request(Request {
                window: target,
                point: screen_point,
            });
        }
        Some((previous, selection.outline(), selection.canvas))
    });
    if let Some((old, new, canvas)) = change {
        invalidate_change(window, old, new, canvas);
    }
}

unsafe fn refine_target(window: HWND) {
    let change = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let found = selection.probe.as_ref()?.latest()?;
        let desktop = selection.desktop.bounds;
        let point = selection.pointer?;
        let screen_point = POINT {
            x: point.x + desktop.left,
            y: point.y + desktop.top,
        };
        if !selection.automatic
            || selection.drag.is_some()
            || selection.target != Some(found.request.window.id)
        {
            return None;
        }
        let same_point =
            found.request.point.x == screen_point.x && found.request.point.y == screen_point.y;
        let rect = match found.rect {
            Some(rect) if contains(rect, screen_point) => rect,
            None if same_point => found.request.window.rect,
            _ => return None,
        };
        let previous = selection.outline();
        selection.rect = overlay_rect(rect, desktop)?;
        Some((previous, selection.outline(), selection.canvas))
    });
    if let Some((old, new, canvas)) = change {
        invalidate_change(window, old, new, canvas);
    }
}

unsafe fn reset_automatic(window: HWND, point: POINT) {
    let change = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.outline();
        let was_dragging = selection.drag.take().is_some();
        selection.automatic = true;
        selection.target = None;
        selection.pointer = None;
        selection.rect = RECT::default();
        Some((
            previous,
            selection.outline(),
            selection.canvas,
            was_dragging,
        ))
    });
    if let Some((old, new, canvas, was_dragging)) = change {
        if was_dragging {
            ReleaseCapture();
        }
        invalidate_change(window, old, new, canvas);
        hide_loupe(window);
        hover(window, point);
        update_cursor(point);
    }
}

unsafe fn update_loupe(window: HWND, point: POINT) {
    let change = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.loupe;
        let dragging = selection
            .drag
            .as_ref()
            .is_some_and(|drag| drag.started || !selection.automatic);
        let visible = contains(selection.canvas, point)
            && (dragging || !selection.automatic && contains(selection.rect, point));
        if visible
            && previous.is_some_and(|loupe| loupe.point.x == point.x && loupe.point.y == point.y)
        {
            return None;
        }
        selection.loupe = if visible {
            if !contains(selection.pointer_monitor, point) {
                let bounds = selection.desktop.bounds;
                let cursor = POINT {
                    x: point.x + bounds.left,
                    y: point.y + bounds.top,
                };
                selection.pointer_monitor = monitor_info(cursor)
                    .and_then(|info| overlay_rect(info.rcMonitor, bounds))
                    .unwrap_or(selection.canvas);
            }
            Loupe::new(point, selection.pointer_monitor)
        } else {
            None
        };
        Some((previous, selection.loupe))
    });
    if let Some((previous, current)) = change {
        if let Some(previous) = previous {
            InvalidateRect(window, &previous.bounds, 0);
        } else if current.is_some() {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: window,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tracking);
        }
        if let Some(current) = current {
            InvalidateRect(window, &current.bounds, 0);
        }
    }
}

unsafe fn hide_loupe(window: HWND) {
    let loupe = SESSION.with_borrow_mut(|session| {
        session
            .as_mut()
            .and_then(|selection| selection.loupe.take())
    });
    if let Some(loupe) = loupe {
        InvalidateRect(window, &loupe.bounds, 0);
    }
}

unsafe fn update_cursor(point: POINT) {
    let cursor = SESSION.with_borrow(|session| {
        let Some(selection) = session else {
            return IDC_CROSS;
        };
        let kind = selection
            .drag
            .as_ref()
            .map(|drag| drag.kind)
            .unwrap_or_else(|| {
                if selection.automatic {
                    DragKind::Draw
                } else {
                    hit_drag(selection.rect, selection.canvas, point)
                }
            });
        match kind {
            DragKind::Draw => IDC_CROSS,
            DragKind::Move => IDC_SIZEALL,
            DragKind::Resize(Handle(0, _)) => IDC_SIZENS,
            DragKind::Resize(Handle(_, 0)) => IDC_SIZEWE,
            DragKind::Resize(Handle(x, y)) if x == y => IDC_SIZENWSE,
            DragKind::Resize(_) => IDC_SIZENESW,
        }
    });
    SetCursor(LoadCursorW(null_mut(), cursor));
}

unsafe fn invalidate_change(window: HWND, old: Outline, new: Outline, canvas: RECT) {
    let changed = EqualRect(&old.rect, &new.rect) == 0;
    if changed {
        // The shared interior keeps the same pixels as the selection moves.
        let region = CreateRectRgnIndirect(&old.rect);
        let next = CreateRectRgnIndirect(&new.rect);
        if !region.is_null() && !next.is_null() && CombineRgn(region, region, next, RGN_XOR) != 0 {
            InvalidateRgn(window, region, 0);
        } else {
            InvalidateRect(window, &old.rect, 0);
            InvalidateRect(window, &new.rect, 0);
        }
        if !next.is_null() {
            DeleteObject(next);
        }
        if !region.is_null() {
            DeleteObject(region);
        }
    }
    if changed || old.handles != new.handles {
        for outline in [old, new] {
            for edge in edges(outline.rect) {
                InvalidateRect(window, &edge, 0);
            }
            if outline.handles && !outline_empty(outline.rect) {
                for handle in HANDLES {
                    InvalidateRect(window, &handle.bounds(outline.rect, canvas), 0);
                }
            }
        }
    }
}

fn outline_empty(rect: RECT) -> bool {
    rect.left >= rect.right || rect.top >= rect.bottom
}

fn edges(rect: RECT) -> [RECT; 4] {
    if outline_empty(rect) {
        return [RECT::default(); 4];
    }
    [
        RECT {
            right: (rect.left + 2).min(rect.right),
            ..rect
        },
        RECT {
            bottom: (rect.top + 2).min(rect.bottom),
            ..rect
        },
        RECT {
            left: (rect.right - 2).max(rect.left),
            ..rect
        },
        RECT {
            top: (rect.bottom - 2).max(rect.top),
            ..rect
        },
    ]
}

unsafe fn paint(window: HWND) {
    let update_region = CreateRectRgn(0, 0, 0, 0);
    let clipped = !update_region.is_null() && GetUpdateRgn(window, update_region, 0) != 0;
    let mut paint = PAINTSTRUCT::default();
    let window_dc = BeginPaint(window, &mut paint);
    SESSION.with_borrow(|session| {
        let Some(selection) = session else { return };
        let area = paint.rcPaint;
        let width = area.right - area.left;
        let height = area.bottom - area.top;
        if width <= 0 || height <= 0 {
            return;
        }

        let dc = selection.back_buffer.dc;
        let source = selection.desktop.surface.dc;
        if clipped {
            SelectClipRgn(dc, update_region);
        } else {
            IntersectClipRect(dc, area.left, area.top, area.right, area.bottom);
        }
        BitBlt(
            dc,
            area.left,
            area.top,
            width,
            height,
            selection.dimmed.dc,
            area.left,
            area.top,
            SRCCOPY,
        );
        let rect = selection.rect;
        if let Some(visible) = intersect(rect, area) {
            BitBlt(
                dc,
                visible.left,
                visible.top,
                visible.right - visible.left,
                visible.bottom - visible.top,
                source,
                visible.left,
                visible.top,
                SRCCOPY,
            );
        }
        if !outline_empty(rect) {
            let blue = GetStockObject(DC_BRUSH);
            SetDCBrushColor(dc, BORDER_BLUE);
            for edge in edges(rect) {
                FillRect(dc, &edge, blue);
            }
            if !selection.automatic {
                let white = GetStockObject(WHITE_BRUSH);
                for handle in HANDLES {
                    let bounds = handle.bounds(rect, selection.canvas);
                    FillRect(dc, &bounds, blue);
                    let core = RECT {
                        left: bounds.left + 2,
                        top: bounds.top + 2,
                        right: bounds.right - 2,
                        bottom: bounds.bottom - 2,
                    };
                    FillRect(dc, &core, white);
                }
            }
        }
        selection.hint.paint(dc, area);
        if let Some(loupe) = selection.loupe {
            if intersect(loupe.bounds, area).is_some() {
                loupe.paint(dc, source, selection.canvas);
            }
        }
        SelectClipRgn(dc, null_mut());
        BitBlt(
            window_dc, area.left, area.top, width, height, dc, area.left, area.top, SRCCOPY,
        );
    });
    EndPaint(window, &paint);
    if !update_region.is_null() {
        DeleteObject(update_region);
    }
}

unsafe fn output(window: HWND, save: bool) {
    let image = SESSION.with_borrow(|session| {
        let selection = session.as_ref()?;
        let rect = selection.rect;
        if selection.drag.is_some() || rect.left == rect.right || rect.top == rect.bottom {
            return None;
        }
        Some(selection.desktop.crop(rect))
    });
    let Some(image) = image else { return };
    let result = match image {
        Ok(image) if save => save_image(window, image),
        Ok(image) => image
            .copy_to_clipboard(window)
            .map(|()| true)
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(true) => {
            DestroyWindow(window);
        }
        Ok(false) => {}
        Err(error) => show_error(window, &format!("输出截图失败。\n{error}")),
    }
}

unsafe fn save_image(window: HWND, image: crate::capture::Image) -> Result<bool, String> {
    let mut filename = [0u16; 1024];
    let default_name: Vec<u16> = "Snapshot.png".encode_utf16().collect();
    filename[..default_name.len()].copy_from_slice(&default_name);
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: window,
        lpstrFilter: w!("PNG 图片 (*.png)\0*.png\0"),
        lpstrFile: filename.as_mut_ptr(),
        nMaxFile: filename.len() as u32,
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        lpstrDefExt: w!("png"),
        ..Default::default()
    };
    if GetSaveFileNameW(&mut dialog) == 0 {
        let error = CommDlgExtendedError();
        return if error == 0 {
            Ok(false)
        } else {
            Err(format!("保存对话框错误 {error:#x}"))
        };
    }
    let end = filename
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(filename.len());
    let path = OsString::from_wide(&filename[..end]);
    image
        .save_png(path.as_ref())
        .map_err(|error| error.to_string())?;
    Ok(true)
}
