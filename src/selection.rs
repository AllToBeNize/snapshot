use std::{
    cell::RefCell,
    io,
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{EM_SETRECT, WM_MOUSELEAVE},
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
                TRACKMOUSEEVENT, VK_CONTROL, VK_ESCAPE, VK_RETURN,
            },
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    annotation::{Draft, History, Mark, Style, Tool},
    annotation_options::{Choice, Options},
    capture::{save_path, Desktop, Surface},
    hotkey::{self, Shortcut},
    icons::Cursor,
    overlay_ui::{monitor_info, HintMode, HintPanel, Loupe},
    pinned, scroll_capture,
    selection_geometry::{
        contains, drag_rect, hit_drag, intersect, DragKind, Handle, BORDER_BLUE, HANDLES,
    },
    show_error,
    targets::{self, ElementProbe, Request, WindowTarget, ELEMENT_READY},
    text_mark::{TextHandle, TextMark},
    toolbar::{paint_size_badge, paint_tooltip, Action, Toolbar},
    typography,
};

const CLASS: *const u16 = w!("Snapshot.Selection");
const FINISH_TEXT: u32 = WM_APP + 3;

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
    visual_probe: Option<(POINT, Option<RECT>)>,
    automatic: bool,
    drag: Option<Drag>,
    rect: RECT,
    hint: HintPanel,
    loupe: Option<Loupe>,
    pointer_monitor: RECT,
    annotations: History,
    toolbar: Option<Toolbar>,
    toolbar_hover: Option<Action>,
    options: Option<Options>,
    options_hover: Option<Choice>,
    style: Style,
    selected_text: Option<usize>,
    text_drag: Option<TextDrag>,
    editor: Option<Editor>,
    brush_cursor: Option<Cursor>,
    mosaic_cursor: Option<(i32, Cursor)>,
}

struct TextDrag {
    index: usize,
    anchor: POINT,
    handle: TextHandle,
    original: TextMark,
}

struct Editor {
    window: HWND,
    rect: RECT,
    background: HBRUSH,
    index: Option<usize>,
    text: TextMark,
}

impl Drop for Editor {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.background);
        }
    }
}

impl Selection {
    fn selected_text(&self) -> Option<&TextMark> {
        self.selected_text
            .and_then(|index| self.annotations.text(index))
    }

    fn option_style(&self) -> Style {
        let mut style = self.style;
        if let Some(text) = self
            .editor
            .as_ref()
            .map(|editor| &editor.text)
            .or_else(|| self.selected_text())
        {
            style.text_size = text.size;
            style.text_color = text.color;
        }
        style
    }
}

struct Drag {
    anchor: POINT,
    previous: RECT,
    kind: DragKind,
    started: bool,
    copy_on_click: bool,
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
    open_selection(instance, None)
}

pub unsafe fn reopen(instance: HINSTANCE, region: RECT) -> io::Result<()> {
    open_selection(instance, Some(region))
}

unsafe fn open_selection(instance: HINSTANCE, region: Option<RECT>) -> io::Result<()> {
    if SESSION.with_borrow(Option::is_some) || scroll_capture::is_open() {
        return Ok(());
    }
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor);
    let targets = targets::windows_at_capture();
    let desktop = Desktop::capture()?;
    let bounds = desktop.bounds;
    let restored = region.and_then(|rect| overlay_rect(rect, bounds));
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let canvas = RECT {
        right: width,
        bottom: height,
        ..Default::default()
    };
    let source = desktop.surface.dc;
    let monitor = monitor_info(region.map_or(cursor, |rect| POINT {
        x: rect.left + (rect.right - rect.left) / 2,
        y: rect.top + (rect.bottom - rect.top) / 2,
    }));
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
        WS_POPUP | WS_CLIPCHILDREN,
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
        visual_probe: None,
        automatic: restored.is_none(),
        drag: None,
        rect: restored.unwrap_or_default(),
        hint,
        loupe: None,
        pointer_monitor,
        annotations: History::default(),
        toolbar: None,
        toolbar_hover: None,
        options: None,
        options_hover: None,
        style: Style::default(),
        selected_text: None,
        text_drag: None,
        editor: None,
        brush_cursor: None,
        mosaic_cursor: None,
    }));
    if restored.is_some() {
        refresh_toolbar(window);
        refresh_hint(window);
    } else {
        hover(
            window,
            POINT {
                x: cursor.x - bounds.left,
                y: cursor.y - bounds.top,
            },
        );
    }
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
            let point = mouse_point(lparam);
            if let Some(options) = SESSION
                .with_borrow(|session| session.as_ref()?.options)
                .filter(|options| contains(options.bounds, point))
            {
                if let Some(choice) = options.hit(point) {
                    change_option(window, choice);
                }
                return 0;
            }
            finish_text(window, true);
            let toolbar = SESSION.with_borrow(|session| session.as_ref()?.toolbar);
            if let Some(toolbar) = toolbar.filter(|toolbar| contains(toolbar.bounds, point)) {
                if let Some(action) = toolbar.hit(point) {
                    command(window, action);
                }
                return 0;
            }
            if begin_text_drag(window, point, message == WM_LBUTTONDBLCLK) {
                hide_loupe(window);
                return 0;
            }
            if begin_annotation(window, point) {
                SetCapture(window);
                hide_loupe(window);
                invalidate_toolbar(window);
                return 0;
            }
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
            if update_text_drag(window, point, false) || update_annotation(window, point, false) {
                return 0;
            }
            if !update_drag(window, point, false) {
                hover(window, point);
            }
            update_toolbar_hover(window, Some(point));
            update_options_hover(window, Some(point));
            update_cursor(point);
            update_loupe(window, point);
        }
        WM_LBUTTONUP => {
            let point = mouse_point(lparam);
            if update_text_drag(window, point, true) || update_annotation(window, point, true) {
                return 0;
            }
            update_drag(window, point, true);
            update_toolbar_hover(window, Some(point));
            update_options_hover(window, Some(point));
            update_cursor(point);
            update_loupe(window, point);
        }
        WM_RBUTTONUP => {
            let editing =
                SESSION.with_borrow(|session| session.as_ref().is_some_and(|s| s.editor.is_some()));
            finish_text(window, false);
            if editing || leave_tool(window, mouse_point(lparam)) {
                return 0;
            }
            if SESSION.with_borrow(|session| session.as_ref().is_some_and(|s| s.automatic)) {
                DestroyWindow(window);
            } else {
                reset_automatic(window, mouse_point(lparam));
            }
        }
        FINISH_TEXT => {
            if SESSION.with_borrow(|session| {
                session
                    .as_ref()
                    .and_then(|selection| selection.editor.as_ref())
                    .is_some_and(|editor| editor.window == lparam as HWND)
            }) {
                finish_text(window, wparam != 0);
            }
        }
        WM_CTLCOLOREDIT => {
            if let Some((brush, color)) = SESSION.with_borrow(|session| {
                let editor = session.as_ref()?.editor.as_ref()?;
                (editor.window == lparam as HWND).then_some((editor.background, editor.text.color))
            }) {
                SetTextColor(wparam as HDC, color);
                SetBkMode(wparam as HDC, TRANSPARENT as i32);
                SetBrushOrgEx(wparam as HDC, -1, -1, null_mut());
                return brush as LRESULT;
            }
            return DefWindowProcW(window, message, wparam, lparam);
        }
        WM_MOUSEWHEEL => {
            let delta = i32::from((wparam >> 16) as i16);
            if delta.abs() >= 120 {
                change_option(window, Choice::Primary(delta / 120));
            }
        }
        WM_MOUSELEAVE => {
            update_toolbar_hover(window, None);
            update_options_hover(window, None);
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
            cancel_text_drag(window);
            let (interrupted, draft) = SESSION.with_borrow_mut(|session| {
                let Some(selection) = session else {
                    return (false, None);
                };
                (
                    selection.drag.take().is_some(),
                    selection.annotations.draft.take().map(|_| selection.rect),
                )
            });
            if interrupted {
                hide_loupe(window);
            }
            if let Some(rect) = draft {
                InvalidateRect(window, &rect, 0);
                invalidate_toolbar(window);
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let key = wparam as u32;
            let modifiers = hotkey::modifiers();
            if key == u32::from(VK_ESCAPE) && modifiers == 0 {
                DestroyWindow(window);
            } else if let Some(action) = hotkey::current()
                .resolve(Shortcut { modifiers, key })
                .and_then(Action::from_binding)
            {
                if lparam & (1 << 30) == 0 {
                    command(window, action);
                }
            } else if message == WM_SYSKEYDOWN {
                return DefWindowProcW(window, message, wparam, lparam);
            }
        }
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

unsafe fn refresh_toolbar(window: HWND) {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.toolbar.take();
        if !selection.automatic && selection.drag.is_none() && !outline_empty(selection.rect) {
            let bounds = selection.desktop.bounds;
            let point = POINT {
                x: selection.rect.right - 1 + bounds.left,
                y: selection.rect.bottom - 1 + bounds.top,
            };
            let area = monitor_info(point)
                .and_then(|info| overlay_rect(info.rcMonitor, bounds))
                .unwrap_or(selection.canvas);
            selection.toolbar = Toolbar::new(selection.rect, area);
        }
        if let (Some(old), Some(new)) = (previous, selection.toolbar) {
            if EqualRect(&old.bounds, &new.bounds) != 0 {
                return None;
            }
        }
        let old_tip = previous
            .zip(selection.toolbar_hover.take())
            .and_then(|(toolbar, action)| toolbar.tooltip(action).map(|(bounds, _)| bounds));
        Some([
            previous.map(|toolbar| toolbar.bounds),
            selection.toolbar.map(|toolbar| toolbar.bounds),
            old_tip,
        ])
    });
    if let Some(changed) = changed {
        for bounds in changed.into_iter().flatten() {
            InvalidateRect(window, &bounds, 0);
        }
    }
    refresh_options(window);
}

unsafe fn refresh_options(window: HWND) {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.options.take();
        selection.options = selection
            .toolbar
            .zip(selection.annotations.tool)
            .and_then(|(toolbar, tool)| toolbar.options(tool, selection.rect));
        selection.options_hover = None;
        let tip = selection
            .toolbar
            .zip(selection.toolbar_hover)
            .and_then(|(toolbar, action)| toolbar.tooltip(action))
            .map(|(bounds, _)| bounds);
        Some([
            previous.map(|options| options.bounds),
            selection.options.map(|options| options.bounds),
            tip,
        ])
    });
    if let Some(changed) = changed {
        for bounds in changed.into_iter().flatten() {
            InvalidateRect(window, &bounds, 0);
        }
    }
}

unsafe fn update_options_hover(window: HWND, point: Option<POINT>) {
    let bounds = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let options = selection.options?;
        let hovered = point.and_then(|point| options.hit(point));
        if selection.options_hover == hovered {
            return None;
        }
        selection.options_hover = hovered;
        Some(options.bounds)
    });
    if let Some(bounds) = bounds {
        InvalidateRect(window, &bounds, 0);
    }
}

unsafe fn change_option(window: HWND, choice: Choice) {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let tool = selection.annotations.tool?;
        if selection.annotations.draft.is_some() || selection.text_drag.is_some() {
            return None;
        }
        let mut style = selection.option_style();
        choice.adjust(tool, &mut style);
        let mut dirty = [None, None];
        let mut editor_font = None;
        if tool != Tool::Text {
            selection.style = style;
        } else if let Some(editor) = &selection.editor {
            let mut text = editor.text.clone();
            text.size = style.text_size;
            text.color = style.text_color;
            let rect = text_editor_bounds(selection, &text);
            let layout = (EqualRect(&editor.rect, &rect) == 0)
                .then(|| text_editor_background(selection, rect, editor.index).ok())
                .flatten();
            let editor = selection.editor.as_mut().unwrap();
            editor.text = text;
            let resized = layout.map(|background| {
                DeleteObject(std::mem::replace(&mut editor.background, background));
                dirty[0] = Some(editor.rect);
                editor.rect = rect;
                rect
            });
            editor_font = Some((
                editor.window,
                typography::annotation_font(style.text_size),
                resized,
            ));
        } else if let Some(index) = selection.selected_text {
            if let Some(mut text) = selection.annotations.text(index).cloned() {
                dirty[0] = Some(text.bounds());
                text.size = style.text_size;
                text.color = style.text_color;
                text.fit_text(selection.desktop.surface.dc);
                dirty[1] = Some(text.bounds());
                selection.annotations.replace_text(index, text);
            }
        } else {
            selection.style = style;
        }
        Some((dirty, editor_font, selection.options))
    });
    if let Some((dirty, editor_font, options)) = changed {
        for rect in dirty.into_iter().flatten() {
            InvalidateRect(window, &rect, 0);
        }
        if let Some((editor, font, resized)) = editor_font {
            SendMessageW(editor, WM_SETFONT, font as usize, 0);
            if let Some(rect) = resized {
                SetWindowPos(
                    editor,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                set_editor_format(editor, rect);
            }
            InvalidateRect(editor, null(), 1);
            SetFocus(editor);
        }
        if let Some(options) = options {
            InvalidateRect(window, &options.bounds, 0);
        }
        invalidate_toolbar(window);
        update_cursor_at_pointer(window);
    }
}

unsafe fn update_toolbar_hover(window: HWND, point: Option<POINT>) {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let toolbar = selection.toolbar?;
        let hovered = point.and_then(|point| toolbar.hit(point));
        if hovered == selection.toolbar_hover {
            return None;
        }
        let previous = selection.toolbar_hover;
        let entered = previous.is_none() && hovered.is_some();
        selection.toolbar_hover = hovered;
        let tip_bounds = |action: Option<Action>| {
            action.and_then(|action| toolbar.tooltip(action).map(|(bounds, _)| bounds))
        };
        Some((
            [
                Some(toolbar.bounds),
                tip_bounds(previous),
                tip_bounds(hovered),
            ],
            entered,
        ))
    });
    if let Some((bounds, entered)) = changed {
        for rect in bounds.into_iter().flatten() {
            InvalidateRect(window, &rect, 0);
        }
        if entered {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: window,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tracking);
        }
    }
}

unsafe fn refresh_hint(window: HWND) {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let mode = if selection.editor.is_some() {
            HintMode::EditText
        } else if selection.selected_text().is_some() {
            HintMode::SelectedText
        } else if selection.automatic {
            HintMode::Automatic
        } else if let Some(tool) = selection.annotations.tool {
            HintMode::Annotate(tool)
        } else {
            HintMode::Selected
        };
        selection.hint.set_mode(selection.desktop.surface.dc, mode)
    });
    if let Some(bounds) = changed {
        for rect in bounds {
            InvalidateRect(window, &rect, 0);
        }
    }
}

unsafe fn invalidate_toolbar(window: HWND) {
    if let Some(toolbar) = SESSION.with_borrow(|session| session.as_ref()?.toolbar) {
        InvalidateRect(window, &toolbar.bounds, 0);
    }
}

unsafe fn command(window: HWND, action: Action) {
    finish_text(window, true);
    if action == Action::ScrollCapture {
        start_scroll_capture(window);
        return;
    }
    if matches!(action, Action::Copy | Action::Save | Action::Pin) {
        output(window, action);
        return;
    }
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let toolbar = selection.toolbar?;
        if selection.annotations.draft.is_some()
            || selection.text_drag.is_some()
            || !action.enabled(&selection.annotations)
        {
            return None;
        }
        let previous = selection.selected_text().map(TextMark::bounds);
        match action {
            Action::Tool(tool) => {
                selection.annotations.tool =
                    (selection.annotations.tool != Some(tool)).then_some(tool);
                selection.selected_text = None;
            }
            Action::Undo | Action::Redo => {
                let text = if action == Action::Undo {
                    selection.annotations.undo()
                } else {
                    selection.annotations.redo()
                };
                selection.selected_text =
                    text.filter(|_| selection.annotations.tool == Some(Tool::Text));
            }
            _ => unreachable!(),
        }
        if selection.selected_text().is_none() {
            selection.selected_text = None;
        }
        Some((
            selection.rect,
            toolbar.bounds,
            previous,
            selection.selected_text().map(TextMark::bounds),
        ))
    });
    if let Some((rect, toolbar, previous, current)) = changed {
        InvalidateRect(window, &rect, 0);
        InvalidateRect(window, &toolbar, 0);
        for bounds in [previous, current].into_iter().flatten() {
            InvalidateRect(window, &bounds, 0);
        }
        refresh_options(window);
        refresh_hint(window);
        if SESSION.with_borrow(|session| {
            session
                .as_ref()
                .is_some_and(|selection| selection.annotations.tool.is_some())
        }) {
            hide_loupe(window);
        }
        update_cursor_at_pointer(window);
    }
}

unsafe fn begin_annotation(window: HWND, point: POINT) -> bool {
    let started = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        if selection.automatic || !contains(selection.rect, point) {
            return None;
        }
        let tool = selection.annotations.tool?;
        let previous = selection.selected_text().map(TextMark::bounds);
        selection.selected_text = None;
        let draft = Draft::new(
            tool,
            point,
            selection.style,
            selection.rect,
            selection.desktop.surface.dc,
        );
        let dirty = draft.start_bounds();
        selection.annotations.draft = Some(draft);
        Some((dirty, previous))
    });
    if let Some((dirty, previous)) = started {
        InvalidateRect(window, &dirty, 0);
        if let Some(previous) = previous {
            InvalidateRect(window, &previous, 0);
            refresh_options(window);
            refresh_hint(window);
        }
    }
    started.is_some()
}

unsafe fn update_annotation(window: HWND, point: POINT, end: bool) -> bool {
    let change = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let draft = selection.annotations.draft.as_mut()?;
        let dirty = draft.update(point, selection.rect, selection.desktop.surface.dc);
        let finished = end.then(|| selection.annotations.draft.take().unwrap());
        Some((dirty, selection.rect, selection.style, finished))
    });
    let Some((dirty, clip, style, finished)) = change else {
        return false;
    };
    InvalidateRect(window, &dirty, 0);
    if let Some(draft) = finished {
        ReleaseCapture();
        if draft.tool == Tool::Text {
            open_text(
                window,
                None,
                TextMark::new(draft.text_rect(clip), style.text_size, style.text_color),
            );
        } else if let Some(mark) = draft.finish() {
            SESSION.with_borrow_mut(|session| {
                session.as_mut().unwrap().annotations.push(mark);
            });
        }
        invalidate_toolbar(window);
    }
    true
}

unsafe fn begin_text_drag(window: HWND, point: POINT, double: bool) -> bool {
    let target = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        if selection.automatic || !matches!(selection.annotations.tool, None | Some(Tool::Text)) {
            return None;
        }
        let previous = selection.selected_text().map(TextMark::bounds);
        let (index, handle) = selection
            .selected_text
            .zip(selection.selected_text().and_then(|text| text.hit(point)))
            .or_else(|| {
                contains(selection.rect, point)
                    .then(|| selection.annotations.hit_text(point))
                    .flatten()
                    .map(|index| (index, TextHandle::Move))
            })?;
        let text = selection.annotations.text(index)?.clone();
        let bounds = text.bounds();
        selection.selected_text = Some(index);
        selection.annotations.tool = Some(Tool::Text);
        let edit = double && matches!(handle, TextHandle::Move);
        if edit {
            Some((previous, bounds, Some((index, text))))
        } else {
            selection.text_drag = Some(TextDrag {
                index,
                anchor: point,
                handle,
                original: text,
            });
            Some((previous, bounds, None))
        }
    });
    let Some((previous, bounds, edit)) = target else {
        return false;
    };
    for rect in previous.into_iter().chain([bounds]) {
        InvalidateRect(window, &rect, 0);
    }
    refresh_options(window);
    refresh_hint(window);
    invalidate_toolbar(window);
    if let Some((index, text)) = edit {
        open_text(window, Some(index), text);
    } else {
        SetCapture(window);
        update_cursor(point);
    }
    true
}

unsafe fn update_text_drag(window: HWND, point: POINT, end: bool) -> bool {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let drag = selection.text_drag.as_ref()?;
        let text = selection.annotations.text_mut(drag.index)?;
        let previous = text.bounds();
        let limit = if matches!(drag.handle, TextHandle::Move) {
            selection.rect
        } else {
            selection.canvas
        };
        let point = POINT {
            x: point.x.clamp(limit.left, limit.right - 1),
            y: point.y.clamp(limit.top, limit.bottom - 1),
        };
        let mut next = drag
            .original
            .transform_drag(drag.handle, drag.anchor, point);
        if matches!(drag.handle, TextHandle::Scale(_, _)) && next.size != drag.original.size {
            next.fit_text(selection.desktop.surface.dc);
        }
        let current = next.bounds();
        if end {
            let drag = selection.text_drag.take().unwrap();
            *text = drag.original;
            selection.annotations.replace_text(drag.index, next);
        } else {
            *text = next;
        }
        Some([previous, current])
    });
    if let Some(dirty) = changed {
        for rect in dirty {
            InvalidateRect(window, &rect, 0);
        }
        if end {
            ReleaseCapture();
            invalidate_toolbar(window);
            update_cursor(point);
        }
        if let Some(options) = SESSION.with_borrow(|session| session.as_ref()?.options) {
            InvalidateRect(window, &options.bounds, 0);
        }
    }
    changed.is_some()
}

unsafe fn cancel_text_drag(window: HWND) {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let drag = selection.text_drag.take()?;
        let text = selection.annotations.text_mut(drag.index)?;
        let previous = text.bounds();
        *text = drag.original;
        Some([previous, text.bounds()])
    });
    if let Some(dirty) = changed {
        for rect in dirty {
            InvalidateRect(window, &rect, 0);
        }
        refresh_options(window);
        invalidate_toolbar(window);
    }
}

unsafe fn leave_tool(window: HWND, point: POINT) -> bool {
    let changed = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        selection.annotations.tool.take()?;
        let frame = selection.selected_text().map(TextMark::bounds);
        selection.selected_text = None;
        let draft = selection.annotations.draft.take().is_some();
        Some((
            selection.rect,
            frame,
            draft || selection.text_drag.is_some(),
        ))
    });
    if let Some((rect, frame, captured)) = changed {
        if captured {
            cancel_text_drag(window);
            ReleaseCapture();
        }
        for rect in [Some(rect), frame].into_iter().flatten() {
            InvalidateRect(window, &rect, 0);
        }
        refresh_options(window);
        refresh_hint(window);
        invalidate_toolbar(window);
        update_cursor(point);
    }
    changed.is_some()
}

unsafe fn open_text(window: HWND, index: Option<usize>, text: TextMark) {
    let background = SESSION.with_borrow(|session| {
        let selection = session.as_ref().unwrap();
        let rect = text_editor_bounds(selection, &text);
        text_editor_background(selection, rect, index).map(|brush| (rect, brush))
    });
    let (rect, background) = match background {
        Ok(value) => value,
        Err(error) => {
            show_error(window, &format!("无法创建文本框。\n{error}"));
            return;
        }
    };
    let content: Vec<u16> = text.text.iter().copied().chain([0]).collect();
    let editor = CreateWindowExW(
        0,
        w!("EDIT"),
        content.as_ptr(),
        WS_CHILD | WS_BORDER | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32,
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
        window,
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    if editor.is_null() || SetWindowSubclass(editor, Some(text_proc), 1, window as usize) == 0 {
        let error = io::Error::last_os_error();
        DeleteObject(background);
        if !editor.is_null() {
            DestroyWindow(editor);
        }
        show_error(window, &format!("无法创建文本框。\n{error}"));
        return;
    }
    let font = typography::annotation_font(text.size);
    let bounds = text.bounds();
    SESSION.with_borrow_mut(|session| {
        session.as_mut().unwrap().editor = Some(Editor {
            window: editor,
            rect,
            background,
            index,
            text,
        });
    });
    SendMessageW(editor, WM_SETFONT, font as usize, 0);
    set_editor_format(editor, rect);
    ShowWindow(editor, SW_SHOW);
    SetFocus(editor);
    InvalidateRect(window, &bounds, 0);
    refresh_options(window);
    refresh_hint(window);
}

unsafe fn set_editor_format(editor: HWND, rect: RECT) {
    let format_rect = RECT {
        left: 2,
        top: 2,
        right: (rect.right - rect.left - 4).max(3),
        bottom: (rect.bottom - rect.top - 4).max(3),
    };
    SendMessageW(editor, EM_SETRECT, 0, &format_rect as *const RECT as LPARAM);
}

unsafe fn text_editor_background(
    selection: &Selection,
    rect: RECT,
    index: Option<usize>,
) -> io::Result<HBRUSH> {
    let image = selection.desktop.composite_crop(rect, |dc| {
        selection
            .annotations
            .paint(dc, selection.rect, false, index);
    })?;
    let (surface, _, _) = image.into_surface()?;
    let brush = CreatePatternBrush(surface.bitmap());
    if brush.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(brush)
}

fn text_editor_bounds(selection: &Selection, text: &TextMark) -> RECT {
    let clip = selection.rect;
    let mut rect = text.edit_rect(clip);
    let panels = [
        selection.toolbar.map(|toolbar| toolbar.bounds),
        selection.options.map(|options| options.bounds),
    ];
    if !panels
        .into_iter()
        .flatten()
        .any(|bounds| intersect(bounds, rect).is_some())
    {
        return rect;
    }
    let top = panels
        .into_iter()
        .flatten()
        .map(|bounds| bounds.top)
        .min()
        .unwrap();
    let bottom = panels
        .into_iter()
        .flatten()
        .map(|bounds| bounds.bottom)
        .max()
        .unwrap();
    let above = top - clip.top - 2;
    let below = clip.bottom - bottom - 2;
    let height = rect.bottom - rect.top;
    if above >= below && above >= 20 {
        rect.bottom = top - 2;
        rect.top = rect.bottom - height.min(above);
    } else if below >= 20 {
        rect.top = bottom + 2;
        rect.bottom = rect.top + height.min(below);
    }
    rect
}

unsafe extern "system" fn text_proc(
    editor: HWND,
    message: u32,
    key: WPARAM,
    lparam: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    if message == WM_KEYDOWN {
        let escape = key == VK_ESCAPE as usize;
        let control = GetKeyState(VK_CONTROL as i32) < 0;
        if escape || (key == VK_RETURN as usize && control) {
            PostMessageW(
                parent as HWND,
                FINISH_TEXT,
                usize::from(!escape),
                editor as LPARAM,
            );
            return 0;
        }
    }
    DefSubclassProc(editor, message, key, lparam)
}

unsafe fn finish_text(window: HWND, save: bool) {
    let editor = SESSION.with_borrow_mut(|session| session.as_mut()?.editor.take());
    let Some(editor) = editor else { return };
    let changed = if save {
        let length = GetWindowTextLengthW(editor.window);
        let mut value = vec![0; length as usize + 1];
        let read = GetWindowTextW(editor.window, value.as_mut_ptr(), value.len() as i32);
        value.truncate(read as usize);
        SESSION.with_borrow_mut(|session| {
            let selection = session.as_mut().unwrap();
            if value.is_empty() {
                if let Some(index) = editor.index {
                    selection.annotations.remove_text(index);
                }
                selection.selected_text = None;
                return None;
            }
            let mut text = editor.text.clone();
            text.text = value;
            text.fit_text(selection.desktop.surface.dc);
            let bounds = text.bounds();
            selection.selected_text = if let Some(index) = editor.index {
                selection.annotations.replace_text(index, text);
                Some(index)
            } else {
                selection.style.text_size = text.size;
                selection.style.text_color = text.color;
                Some(selection.annotations.push(Mark::Text(text)))
            };
            Some(bounds)
        })
    } else {
        None
    };
    DestroyWindow(editor.window);
    SetFocus(window);
    for rect in [Some(editor.rect), Some(editor.text.bounds()), changed]
        .into_iter()
        .flatten()
    {
        InvalidateRect(window, &rect, 0);
    }
    refresh_options(window);
    invalidate_toolbar(window);
    refresh_hint(window);
}

unsafe fn update_drag(window: HWND, point: POINT, end: bool) -> bool {
    let dirty = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.rect;
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
        Some((previous, selection.rect, selection.canvas, copy))
    });
    if let Some((old, new, canvas, copy)) = dirty {
        invalidate_change(window, old, new, canvas);
        refresh_toolbar(window);
        refresh_hint(window);
        if end {
            ReleaseCapture();
        }
        if copy {
            command(window, Action::Copy);
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
        let previous = selection.rect;
        let id = target.map(|target| target.id);
        if id != selection.target {
            selection.visual_probe = None;
        }
        if id != selection.target || !contains(selection.rect, point) {
            selection.rect = target
                .and_then(|target| overlay_rect(target.rect, desktop))
                .map(|bounds| visual_region(selection, bounds, point).unwrap_or(bounds))
                .unwrap_or_default();
        }
        selection.target = id;
        if let (Some(probe), Some(target)) = (&selection.probe, target) {
            probe.request(Request {
                window: target,
                point: screen_point,
            });
        }
        Some((previous, selection.rect, selection.canvas))
    });
    if let Some((old, new, canvas)) = change {
        invalidate_change(window, old, new, canvas);
    }
}

fn visual_region(selection: &mut Selection, bounds: RECT, point: POINT) -> Option<RECT> {
    if let Some((last, region)) = selection.visual_probe {
        if region.is_some_and(|region| contains(region, point)) {
            return region;
        }
        if (last.x - point.x).abs() < 24 && (last.y - point.y).abs() < 24 {
            return None;
        }
    }
    let region = selection.desktop.visual_panel(bounds, point);
    selection.visual_probe = Some((point, region));
    region
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
        let (rect, weak) = match found.region {
            Some(region) if contains(region.rect, screen_point) => (region.rect, region.weak),
            None if same_point => (found.request.window.rect, true),
            _ => return None,
        };
        let previous = selection.rect;
        let bounds = overlay_rect(found.request.window.rect, desktop)?;
        let semantic = overlay_rect(rect, desktop)?;
        let visual = visual_region(selection, bounds, point);
        selection.rect = visual
            .filter(|visual| {
                weak || (visual.left >= semantic.left
                    && visual.top >= semantic.top
                    && visual.right <= semantic.right
                    && visual.bottom <= semantic.bottom
                    && rect_area(*visual) * 5 < rect_area(semantic) * 4)
            })
            .unwrap_or(semantic);
        Some((previous, selection.rect, selection.canvas))
    });
    if let Some((old, new, canvas)) = change {
        invalidate_change(window, old, new, canvas);
    }
}

fn rect_area(rect: RECT) -> i64 {
    i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top)
}

unsafe fn reset_automatic(window: HWND, point: POINT) {
    let change = SESSION.with_borrow_mut(|session| {
        let selection = session.as_mut()?;
        let previous = selection.rect;
        let was_dragging = selection.drag.take().is_some() || selection.annotations.draft.is_some();
        selection.automatic = true;
        selection.target = None;
        selection.pointer = None;
        selection.visual_probe = None;
        selection.rect = RECT::default();
        selection.annotations = History::default();
        selection.selected_text = None;
        Some((previous, selection.rect, selection.canvas, was_dragging))
    });
    if let Some((old, new, canvas, was_dragging)) = change {
        if was_dragging {
            ReleaseCapture();
        }
        invalidate_change(window, old, new, canvas);
        InvalidateRect(window, &old, 0);
        refresh_toolbar(window);
        refresh_hint(window);
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
        let visible = selection.annotations.tool.is_none()
            && contains(selection.canvas, point)
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
    let cursor = SESSION.with_borrow_mut(|session| {
        let Some(selection) = session else {
            return LoadCursorW(null_mut(), IDC_CROSS);
        };
        let system = |cursor| LoadCursorW(null_mut(), cursor);
        if let Some(options) = selection
            .options
            .filter(|options| contains(options.bounds, point))
        {
            return system(if options.hit(point).is_some() {
                IDC_HAND
            } else {
                IDC_ARROW
            });
        }
        if let Some(toolbar) = selection
            .toolbar
            .filter(|toolbar| contains(toolbar.bounds, point))
        {
            return system(
                if toolbar
                    .hit(point)
                    .is_some_and(|action| action.enabled(&selection.annotations))
                {
                    IDC_HAND
                } else {
                    IDC_ARROW
                },
            );
        }
        let text_handle = selection
            .text_drag
            .as_ref()
            .map(|drag| drag.handle)
            .or_else(|| selection.selected_text().and_then(|text| text.hit(point)));
        if let Some(handle) = text_handle {
            return system(match handle {
                TextHandle::Move => IDC_SIZEALL,
                TextHandle::Scale(x, y) if x == y => IDC_SIZENWSE,
                TextHandle::Scale(..) => IDC_SIZENESW,
                TextHandle::Rotate => IDC_HAND,
            });
        }
        if contains(selection.rect, point) && !selection.automatic && selection.drag.is_none() {
            match selection.annotations.tool {
                Some(Tool::Brush) => {
                    if selection.brush_cursor.is_none() {
                        selection.brush_cursor = Cursor::brush();
                    }
                    if let Some(cursor) = &selection.brush_cursor {
                        return cursor.0;
                    }
                }
                Some(Tool::Mosaic) => {
                    let width = selection.style.mosaic_width;
                    if selection
                        .mosaic_cursor
                        .as_ref()
                        .is_none_or(|(cached, _)| *cached != width)
                    {
                        selection.mosaic_cursor =
                            Cursor::mosaic(width).map(|cursor| (width, cursor));
                    }
                    if let Some((_, cursor)) = &selection.mosaic_cursor {
                        return cursor.0;
                    }
                }
                Some(Tool::Text) => {
                    return system(if selection.annotations.hit_text(point).is_some() {
                        IDC_SIZEALL
                    } else {
                        IDC_IBEAM
                    });
                }
                None if selection.annotations.hit_text(point).is_some() => {
                    return system(IDC_SIZEALL)
                }
                _ => {}
            }
        }
        let kind = selection
            .drag
            .as_ref()
            .map(|drag| drag.kind)
            .unwrap_or_else(|| {
                if selection.automatic
                    || (selection.annotations.tool.is_some() && contains(selection.rect, point))
                {
                    DragKind::Draw
                } else {
                    hit_drag(selection.rect, selection.canvas, point)
                }
            });
        system(match kind {
            DragKind::Draw => IDC_CROSS,
            DragKind::Move => IDC_SIZEALL,
            DragKind::Resize(Handle(0, _)) => IDC_SIZENS,
            DragKind::Resize(Handle(_, 0)) => IDC_SIZEWE,
            DragKind::Resize(Handle(x, y)) if x == y => IDC_SIZENWSE,
            DragKind::Resize(_) => IDC_SIZENESW,
        })
    });
    SetCursor(cursor);
}

unsafe fn update_cursor_at_pointer(window: HWND) {
    let mut point = POINT::default();
    if GetCursorPos(&mut point) != 0 && ScreenToClient(window, &mut point) != 0 {
        update_cursor(point);
    }
}

unsafe fn invalidate_change(window: HWND, old: RECT, new: RECT, canvas: RECT) {
    if EqualRect(&old, &new) == 0 {
        // The shared interior keeps the same pixels as the selection moves.
        let region = CreateRectRgnIndirect(&old);
        let next = CreateRectRgnIndirect(&new);
        if !region.is_null() && !next.is_null() && CombineRgn(region, region, next, RGN_XOR) != 0 {
            InvalidateRgn(window, region, 0);
        } else {
            InvalidateRect(window, &old, 0);
            InvalidateRect(window, &new, 0);
        }
        if !next.is_null() {
            DeleteObject(next);
        }
        if !region.is_null() {
            DeleteObject(region);
        }
        for rect in [old, new] {
            for edge in edges(rect) {
                InvalidateRect(window, &edge, 0);
            }
            if !outline_empty(rect) {
                for handle in HANDLES {
                    InvalidateRect(window, &handle.bounds(rect, canvas), 0);
                }
            }
        }
        if let Some(desktop) = SESSION.with_borrow(|session| Some(session.as_ref()?.desktop.bounds))
        {
            for badge in [old, new]
                .into_iter()
                .filter_map(|rect| size_badge(rect, desktop, canvas))
            {
                InvalidateRect(window, &badge, 0);
            }
        }
    }
}

unsafe fn size_badge(rect: RECT, desktop: RECT, canvas: RECT) -> Option<RECT> {
    if outline_empty(rect) {
        return None;
    }
    let monitor = monitor_info(POINT {
        x: rect.left + desktop.left,
        y: rect.top + desktop.top,
    })
    .and_then(|info| overlay_rect(info.rcMonitor, desktop))
    .unwrap_or(canvas);
    let width = 146;
    let height = 24;
    if monitor.right - monitor.left < width || monitor.bottom - monitor.top < height {
        return None;
    }
    let left = (rect.left + 5).clamp(monitor.left, monitor.right - width);
    let above = rect.top - height - 3;
    let top = if above >= monitor.top {
        above
    } else {
        (rect.top + 6).clamp(monitor.top, monitor.bottom - height)
    };
    Some(RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    })
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
            selection.annotations.paint(
                dc,
                rect,
                true,
                selection.editor.as_ref().and_then(|editor| editor.index),
            );
        }
        if !outline_empty(rect) {
            let blue = GetStockObject(DC_BRUSH);
            SetDCBrushColor(dc, BORDER_BLUE);
            for edge in edges(rect) {
                FillRect(dc, &edge, blue);
            }
            let old_pen = SelectObject(dc, GetStockObject(DC_PEN));
            let old_brush = SelectObject(dc, GetStockObject(WHITE_BRUSH));
            SetDCPenColor(dc, BORDER_BLUE);
            for handle in HANDLES {
                let bounds = handle.bounds(rect, selection.canvas);
                Ellipse(dc, bounds.left, bounds.top, bounds.right, bounds.bottom);
            }
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
        }
        if let Some(text) = selection
            .selected_text()
            .filter(|_| selection.editor.is_none())
        {
            if intersect(text.bounds(), area).is_some() {
                text.paint_frame(dc);
            }
        }
        selection.hint.paint(dc, area);
        if let Some(loupe) = selection.loupe {
            if intersect(loupe.bounds, area).is_some() {
                loupe.paint(dc, source, selection.canvas);
            }
        }
        if let Some(badge) = size_badge(rect, selection.desktop.bounds, selection.canvas)
            .filter(|badge| intersect(*badge, area).is_some())
        {
            let size = crate::wide(&format!(
                "{} × {} px",
                rect.right - rect.left,
                rect.bottom - rect.top
            ));
            paint_size_badge(dc, badge, size.as_ptr());
        }
        if let Some(toolbar) = selection.toolbar {
            if intersect(toolbar.bounds, area).is_some() {
                toolbar.paint(dc, &selection.annotations, selection.toolbar_hover);
            }
            if let Some((bounds, label)) = selection
                .toolbar_hover
                .and_then(|action| toolbar.tooltip(action))
                .filter(|(bounds, _)| intersect(*bounds, area).is_some())
                .filter(|(bounds, _)| {
                    selection
                        .options
                        .is_none_or(|options| intersect(*bounds, options.bounds).is_none())
                })
            {
                paint_tooltip(dc, bounds, label);
            }
        }
        if let Some(options) = selection
            .options
            .filter(|options| intersect(options.bounds, area).is_some())
        {
            options.paint(dc, selection.option_style(), selection.options_hover);
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

unsafe fn start_scroll_capture(window: HWND) {
    let selected = SESSION.with_borrow(|session| {
        let selection = session.as_ref()?;
        let rect = selection.rect;
        if selection.automatic
            || selection.drag.is_some()
            || selection.text_drag.is_some()
            || selection.annotations.draft.is_some()
            || outline_empty(rect)
        {
            return None;
        }
        let desktop = selection.desktop.bounds;
        let screen = RECT {
            left: rect.left + desktop.left,
            top: rect.top + desktop.top,
            right: rect.right + desktop.left,
            bottom: rect.bottom + desktop.top,
        };
        Some(if selection.annotations.has_marks() {
            Err("请先撤回选区里的标注，再开始长截图。".into())
        } else {
            selection
                .desktop
                .crop(rect)
                .map(|image| {
                    let center = POINT {
                        x: screen.left + (screen.right - screen.left) / 2,
                        y: screen.top + (screen.bottom - screen.top) / 2,
                    };
                    let target = selection
                        .targets
                        .iter()
                        .find(|target| contains(target.rect, center));
                    (screen, image, target.map(|target| target.id as HWND))
                })
                .map_err(|error| error.to_string())
        })
    });
    let Some(selected) = selected else {
        return;
    };
    match selected
        .and_then(|(screen, image, target)| scroll_capture::start(screen, image).map(|()| target))
    {
        Ok(target) => {
            if let Some(target) = target {
                SetForegroundWindow(target);
            }
            DestroyWindow(window);
        }
        Err(error) => show_error(window, &error),
    }
}

unsafe fn output(window: HWND, action: Action) {
    let image = SESSION.with_borrow(|session| {
        let selection = session.as_ref()?;
        let rect = selection.rect;
        if selection.drag.is_some()
            || selection.text_drag.is_some()
            || selection.annotations.draft.is_some()
            || outline_empty(rect)
        {
            return None;
        }
        let image = selection.desktop.composite_crop(rect, |dc| {
            selection.annotations.paint(dc, rect, false, None)
        });
        let desktop = selection.desktop.bounds;
        let screen = RECT {
            left: rect.left + desktop.left,
            top: rect.top + desktop.top,
            right: rect.right + desktop.left,
            bottom: rect.bottom + desktop.top,
        };
        Some((image, screen))
    });
    let Some((image, screen)) = image else { return };
    let result = match image {
        Ok(image) => match action {
            Action::Save => save_path(window, "Snapshot.png").and_then(|path| match path {
                Some(path) => image
                    .save_png(&path)
                    .map(|()| true)
                    .map_err(|error| error.to_string()),
                None => Ok(false),
            }),
            Action::Pin => pinned::open(image, screen)
                .map(|()| true)
                .map_err(|error| error.to_string()),
            _ => image
                .copy_to_clipboard(window)
                .map(|()| true)
                .map_err(|error| error.to_string()),
        },
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
