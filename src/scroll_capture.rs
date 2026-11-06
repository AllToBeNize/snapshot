use std::{
    cell::RefCell,
    io,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::DwmFlush,
            Gdi::{
                BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, EqualRect, FillRect,
                GetStockObject, InvalidateRect, SetDCBrushColor, SetWindowRgn, COLOR_WINDOW,
                DC_BRUSH, HDC, PAINTSTRUCT,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    capture::{save_path, Image, RegionCapture},
    overlay_ui::monitor_info,
    scroll_input::{
        InputHooks, InputTarget, CANCEL_CAPTURE, COPY_CAPTURE, SAVE_CAPTURE, WHEEL_RECEIVED,
    },
    scroll_stitch::{Stitcher, Update, MIN_HEIGHT},
    selection,
    selection_geometry::{contains, BORDER_BLUE},
    show_error,
    toolbar::{paint_button, paint_panel, paint_tooltip, Action},
};

const CLASS: *const u16 = w!("Snapshot.ScrollCapture");
const OUTLINE_CLASS: *const u16 = w!("Snapshot.ScrollOutline");
const OUTLINE_WIDTH: i32 = 2;
const TIMER: usize = 1;
const SAMPLE_MS: u32 = 48;
const SAMPLE_TICKS: u8 = 9;
const PANEL_WIDTH: i32 = 204;
const PANEL_MIN_WIDTH: i32 = 152;
const PANEL_MIN_HEIGHT: i32 = 86;
const PANEL_MAX_HEIGHT: i32 = 460;
const PANEL_PADDING: i32 = 5;
const BUTTON_SIZE: i32 = 28;
const BUTTONS: [(Action, *const u16); 3] = [
    (Action::Copy, w!("复制")),
    (Action::Save, w!("保存")),
    (Action::Cancel, w!("取消")),
];

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

struct Session {
    capture: RegionCapture,
    stitcher: Stitcher,
    preview: Preview,
    _hooks: InputHooks,
    region: RECT,
    panel_limit: RECT,
    hovered: Option<Action>,
    pressed: Option<Action>,
    samples_left: u8,
    exporting: bool,
    stopped: bool,
}

impl Session {
    fn panel(&self) -> RECT {
        self.preview.panel(self.panel_limit, self.region)
    }
}

struct Preview {
    bounds: RECT,
    thumbnail: Image,
}

impl Preview {
    fn new(bounds: RECT, image: &Image) -> Self {
        let mut preview = Self {
            bounds,
            thumbnail: Image {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
        };
        preview.update(image);
        preview
    }

    fn update(&mut self, image: &Image) {
        let available_width = self.bounds.right - self.bounds.left - 2;
        let available_height = self.bounds.bottom - self.bounds.top - 2;
        let width = available_width
            .min(
                (i64::from(image.width) * i64::from(available_height) / i64::from(image.height))
                    as i32,
            )
            .max(1);
        let height = available_height
            .min((i64::from(image.height) * i64::from(width) / i64::from(image.width)) as i32)
            .max(1);
        let thumb = &mut self.thumbnail;
        thumb.width = width;
        thumb.height = height;
        thumb.pixels.resize(width as usize * height as usize * 4, 0);
        for (y, row) in thumb
            .pixels
            .chunks_exact_mut(width as usize * 4)
            .enumerate()
        {
            let source_y = (2 * y + 1) * image.height as usize / (2 * height as usize);
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                let source_x = (2 * x + 1) * image.width as usize / (2 * width as usize);
                let index = (source_y * image.width as usize + source_x) * 4;
                pixel.copy_from_slice(&image.pixels[index..index + 4]);
            }
        }
    }

    fn panel(&self, limit: RECT, region: RECT) -> RECT {
        let height = self.thumbnail.height + 2 + 3 * PANEL_PADDING + BUTTON_SIZE;
        let top = if limit.bottom <= region.top {
            limit.bottom - height
        } else {
            limit.top
        };
        RECT {
            top,
            bottom: top + height,
            ..limit
        }
    }

    fn button(&self, index: usize) -> RECT {
        let width = self.bounds.left + self.bounds.right;
        let left = (width - BUTTONS.len() as i32 * BUTTON_SIZE) / 2 + index as i32 * BUTTON_SIZE;
        let top = self.bounds.top + self.thumbnail.height + 2 + PANEL_PADDING;
        RECT {
            left,
            top,
            right: left + BUTTON_SIZE,
            bottom: top + BUTTON_SIZE,
        }
    }

    fn hit(&self, point: POINT) -> Option<Action> {
        BUTTONS
            .iter()
            .enumerate()
            .find_map(|(index, (action, _))| contains(self.button(index), point).then_some(*action))
    }

    unsafe fn paint(&self, dc: HDC) {
        SetDCBrushColor(dc, 0x00ef_efef);
        let visible = RECT {
            bottom: self.bounds.top + self.thumbnail.height + 2,
            ..self.bounds
        };
        FillRect(dc, &visible, GetStockObject(DC_BRUSH));
        self.thumbnail.blit(
            dc,
            (self.bounds.left + self.bounds.right - self.thumbnail.width) / 2,
            self.bounds.top + 1,
        );
    }
}

pub unsafe fn register(instance: HINSTANCE) -> io::Result<()> {
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW as usize + 1) as _,
        lpszClassName: CLASS,
        ..Default::default()
    };
    let outline_class = WNDCLASSW {
        lpfnWndProc: Some(outline_proc),
        hInstance: instance,
        lpszClassName: OUTLINE_CLASS,
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 || RegisterClassW(&outline_class) == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn is_open() -> bool {
    SESSION.with_borrow(Option::is_some)
}

pub unsafe fn start(region: RECT, first: Image) -> Result<(), String> {
    if is_open() {
        return Err("已经在截长图。".into());
    }
    if first.width < 32 || first.height < MIN_HEIGHT {
        return Err("请扩大选区，至少选中 32 × 64 像素。".into());
    }
    let center = POINT {
        x: region.left + first.width / 2,
        y: region.top + first.height / 2,
    };
    let panel_limit = monitor_info(center)
        .and_then(|monitor| panel_bounds(region, monitor.rcWork))
        .ok_or("请缩小选区，在同一屏幕上为长截图面板留出空间。")?;
    let stitcher = Stitcher::new(first).ok_or("选区超过长截图尺寸上限。")?;
    let capture = RegionCapture::new(region).map_err(|error| error.to_string())?;
    let preview = Preview::new(
        RECT {
            left: PANEL_PADDING,
            top: PANEL_PADDING,
            right: panel_limit.right - panel_limit.left - PANEL_PADDING,
            bottom: panel_limit.bottom - panel_limit.top - BUTTON_SIZE - 2 * PANEL_PADDING,
        },
        &stitcher.image,
    );
    let panel = preview.panel(panel_limit, region);
    let instance = GetModuleHandleW(null());
    let window = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        CLASS,
        w!("Snapshot 长截图"),
        WS_POPUP,
        panel.left,
        panel.top,
        panel.right - panel.left,
        panel.bottom - panel.top,
        null_mut(),
        null_mut(),
        instance,
        null(),
    );
    if window.is_null() {
        return Err(io::Error::last_os_error().to_string());
    }
    let edges = match create_outline(window, instance, region) {
        Ok(edges) => edges,
        Err(error) => {
            DestroyWindow(window);
            return Err(error.to_string());
        }
    };
    let hooks = match InputHooks::install(instance) {
        Ok(hooks) => hooks,
        Err(error) => {
            DestroyWindow(window);
            return Err(format!("无法开启长截图输入模式。\n{error}"));
        }
    };
    SESSION.replace(Some(Session {
        capture,
        stitcher,
        preview,
        _hooks: hooks,
        region,
        panel_limit,
        hovered: None,
        pressed: None,
        samples_left: 0,
        exporting: false,
        stopped: false,
    }));
    set_panel_shape(window, panel);
    InputHooks::target(Some(InputTarget {
        window,
        region,
        panel,
    }));
    ShowWindow(window, SW_SHOWNOACTIVATE);
    for edge in edges {
        ShowWindow(edge, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

unsafe fn create_outline(owner: HWND, instance: HINSTANCE, rect: RECT) -> io::Result<[HWND; 4]> {
    let border = OUTLINE_WIDTH;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let edges = [
        (
            rect.left - border,
            rect.top - border,
            width + 2 * border,
            border,
        ),
        (rect.left - border, rect.bottom, width + 2 * border, border),
        (rect.left - border, rect.top, border, height),
        (rect.right, rect.top, border, height),
    ];
    // Owned edge windows sit just outside the captured pixels and pass mouse input through.
    let mut windows = [null_mut(); 4];
    for ((x, y, width, height), window) in edges.into_iter().zip(&mut windows) {
        let edge = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            OUTLINE_CLASS,
            null(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            owner,
            null_mut(),
            instance,
            null(),
        );
        if edge.is_null() || SetLayeredWindowAttributes(edge, 0, 255, LWA_ALPHA) == 0 {
            return Err(io::Error::last_os_error());
        }
        *window = edge;
    }
    Ok(windows)
}

unsafe extern "system" fn outline_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != WM_PAINT {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(window, &mut paint);
    SetDCBrushColor(dc, BORDER_BLUE);
    FillRect(dc, &paint.rcPaint, GetStockObject(DC_BRUSH));
    EndPaint(window, &paint);
    0
}

fn panel_bounds(region: RECT, area: RECT) -> Option<RECT> {
    let width = PANEL_WIDTH.min(area.right - area.left);
    let height = (region.bottom - region.top)
        .clamp(180, PANEL_MAX_HEIGHT)
        .min(area.bottom - area.top);
    if width < PANEL_MIN_WIDTH || height < PANEL_MIN_HEIGHT {
        return None;
    }
    let gap = 7;
    let top = region.top.clamp(area.top, area.bottom - height);
    let right_edge = (region.right + gap).max(area.left);
    let right_width = width.min(area.right - right_edge);
    if right_width >= PANEL_MIN_WIDTH {
        return Some(RECT {
            left: right_edge,
            top,
            right: right_edge + right_width,
            bottom: top + height,
        });
    }
    let left_edge = (region.left - gap).min(area.right);
    let left_width = width.min(left_edge - area.left);
    if left_width >= PANEL_MIN_WIDTH {
        return Some(RECT {
            left: left_edge - left_width,
            top,
            right: left_edge,
            bottom: top + height,
        });
    }
    let left = (region.right - width).clamp(area.left, area.right - width);
    let below = (region.bottom + gap).max(area.top);
    let below_height = height.min(area.bottom - below);
    if below_height >= PANEL_MIN_HEIGHT {
        return Some(RECT {
            left,
            top: below,
            right: left + width,
            bottom: below + below_height,
        });
    }
    let above = (region.top - gap).min(area.bottom);
    let above_height = height.min(above - area.top);
    (above_height >= PANEL_MIN_HEIGHT).then_some(RECT {
        left,
        top: above - above_height,
        right: left + width,
        bottom: above,
    })
}

unsafe fn set_panel_shape(window: HWND, panel: RECT) {
    let region = CreateRoundRectRgn(
        0,
        0,
        panel.right - panel.left,
        panel.bottom - panel.top,
        6,
        6,
    );
    if !region.is_null() && SetWindowRgn(window, region, 1) == 0 {
        DeleteObject(region);
    }
}

fn mouse_point(position: LPARAM) -> POINT {
    POINT {
        x: i32::from(position as i16),
        y: i32::from((position >> 16) as i16),
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            SESSION.with_borrow(|session| {
                if let Some(session) = session {
                    let panel = session.panel();
                    paint_panel(
                        dc,
                        RECT {
                            right: panel.right - panel.left,
                            bottom: panel.bottom - panel.top,
                            ..Default::default()
                        },
                    );
                    session.preview.paint(dc);
                    for (index, (action, label)) in BUTTONS.into_iter().enumerate() {
                        let button = session.preview.button(index);
                        let hovered = session.hovered == Some(action);
                        paint_button(dc, button, action, true, false, hovered);
                        if hovered && button.top > 27 {
                            let left = button.left - 13;
                            paint_tooltip(
                                dc,
                                RECT {
                                    left,
                                    top: button.top - 24,
                                    right: left + 54,
                                    bottom: button.top - 4,
                                },
                                label,
                            );
                        }
                    }
                }
            });
            EndPaint(window, &paint);
        }
        WHEEL_RECEIVED => arm_sample(window),
        WM_TIMER if wparam == TIMER => {
            let ready = SESSION.with_borrow_mut(|session| {
                let Some(session) = session.as_mut() else {
                    return false;
                };
                if session.samples_left == 0 {
                    return false;
                }
                session.samples_left -= 1;
                if session.samples_left == 0 {
                    KillTimer(window, TIMER);
                }
                true
            });
            if ready {
                sample(window);
            }
        }
        WM_MOUSEMOVE => {
            update_hover(window, Some(mouse_point(lparam)));
        }
        WM_MOUSELEAVE => {
            update_hover(window, None);
        }
        WM_LBUTTONDOWN => {
            SESSION.with_borrow_mut(|session| {
                if let Some(session) = session.as_mut() {
                    session.pressed = session.preview.hit(mouse_point(lparam));
                }
            });
        }
        WM_LBUTTONUP => {
            let action = SESSION.with_borrow_mut(|session| {
                let session = session.as_mut()?;
                let pressed = session.pressed.take()?;
                (session.preview.hit(mouse_point(lparam)) == Some(pressed)).then_some(pressed)
            });
            match action {
                Some(Action::Copy) => export(window, false),
                Some(Action::Save) => export(window, true),
                Some(Action::Cancel) => cancel(window),
                _ => {}
            }
        }
        CANCEL_CAPTURE => cancel(window),
        COPY_CAPTURE => export(window, false),
        SAVE_CAPTURE => export(window, true),
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_CLOSE => {
            DestroyWindow(window);
        }
        WM_DESTROY => {
            KillTimer(window, TIMER);
            SESSION.take();
        }
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}

unsafe fn update_hover(window: HWND, point: Option<POINT>) {
    let result = SESSION.with_borrow_mut(|session| {
        let session = session.as_mut()?;
        let hovered = point.and_then(|point| session.preview.hit(point));
        if point.is_none() {
            session.pressed = None;
        }
        let previous = std::mem::replace(&mut session.hovered, hovered);
        Some((previous, hovered))
    });
    if let Some((previous, hovered)) = result {
        if previous != hovered {
            InvalidateRect(window, null(), 0);
            if hovered.is_some() {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
            }
        }
        if point.is_some() {
            let cursor = if hovered.is_some() {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            SetCursor(LoadCursorW(null_mut(), cursor));
        }
    }
}

unsafe fn cancel(window: HWND) {
    let region = SESSION.with_borrow(|session| {
        let session = session.as_ref()?;
        (!session.exporting).then_some(session.region)
    });
    if let Some(region) = region {
        DestroyWindow(window);
        DwmFlush();
        if let Err(error) = selection::reopen(GetModuleHandleW(null()), region) {
            show_error(null_mut(), &format!("无法返回截图选区。\n{error}"));
        }
    }
}

unsafe fn arm_sample(window: HWND) {
    SESSION.with_borrow_mut(|session| {
        if let Some(session) = session.as_mut() {
            if !session.exporting
                && !session.stopped
                && (session.samples_left != 0 || SetTimer(window, TIMER, SAMPLE_MS, None) != 0)
            {
                session.samples_left = SAMPLE_TICKS;
            }
        }
    });
}

unsafe fn sample(window: HWND) {
    let dirty = SESSION.with_borrow_mut(|session| {
        let session = session.as_mut()?;
        if session.exporting || session.stopped {
            return None;
        }
        let frame = session.capture.sample().ok()?;
        let height = session.stitcher.image.height;
        if session.stitcher.observe(frame) == Update::Limit {
            session.stopped = true;
            session.samples_left = 0;
            KillTimer(window, TIMER);
        }
        if height == session.stitcher.image.height {
            return None;
        }
        let old = session.panel();
        session.preview.update(&session.stitcher.image);
        Some((old, session.panel(), session.region))
    });
    if let Some((old, panel, region)) = dirty {
        if EqualRect(&old, &panel) == 0 {
            SetWindowPos(
                window,
                null_mut(),
                panel.left,
                panel.top,
                panel.right - panel.left,
                panel.bottom - panel.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            set_panel_shape(window, panel);
            InputHooks::target(Some(InputTarget {
                window,
                region,
                panel,
            }));
        }
        InvalidateRect(window, null(), 0);
    }
}

unsafe fn export(window: HWND, save: bool) {
    if SESSION.with_borrow(|session| session.as_ref().is_none_or(|s| s.exporting)) {
        return;
    }
    sample(window);
    SESSION.with_borrow_mut(|session| {
        if let Some(session) = session {
            session.exporting = true;
            session.samples_left = 0;
        }
    });
    KillTimer(window, TIMER);
    let input = InputHooks::target(None);
    let result = if save {
        save_path(window, "Snapshot-Long.png").and_then(|path| {
            SESSION.with_borrow(|session| match (path, session.as_ref()) {
                (Some(path), Some(session)) => session
                    .stitcher
                    .image
                    .save_png(&path)
                    .map(|()| true)
                    .map_err(|error| error.to_string()),
                _ => Ok(false),
            })
        })
    } else {
        SESSION.with_borrow(|session| match session {
            Some(session) => session
                .stitcher
                .image
                .copy_to_clipboard(window)
                .map(|()| true)
                .map_err(|error| error.to_string()),
            None => Ok(false),
        })
    };
    match result {
        Ok(true) => {
            DestroyWindow(window);
            return;
        }
        Ok(false) => {}
        Err(error) => show_error(window, &format!("输出长截图失败。\n{error}")),
    }
    let resume = SESSION.with_borrow_mut(|session| {
        if let Some(session) = session {
            session.exporting = false;
            return true;
        }
        false
    });
    if resume {
        InputHooks::target(input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Surface;
    use windows_sys::Win32::Graphics::Gdi::GetPixel;

    #[test]
    fn preview_paints_the_entire_stitched_image_top_to_bottom() {
        let image = Image {
            width: 1,
            height: 3,
            pixels: vec![0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0],
        };
        let bounds = RECT {
            right: 20,
            bottom: 20,
            ..Default::default()
        };
        let preview = Preview::new(bounds, &image);
        unsafe {
            let surface = Surface::new(null_mut(), 20, 20).unwrap();
            preview.paint(surface.dc);
            assert_eq!(GetPixel(surface.dc, 10, 2), 0x0000_00ff);
            assert_eq!(GetPixel(surface.dc, 10, 9), 0x0000_ff00);
            assert_eq!(GetPixel(surface.dc, 10, 17), 0x00ff_0000);
        }
    }
}
