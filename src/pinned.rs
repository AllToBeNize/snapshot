use crate::{
    capture::{Image, Surface},
    hotkey::DEFAULT_OPACITY_STEP,
    toolbar::paint_size_badge,
};
use std::{
    cell::Cell,
    io,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Input::KeyboardAndMouse::VK_ESCAPE, WindowsAndMessaging::*},
    },
};

const CLASS: *const u16 = w!("Snapshot.Pinned");
const GLOW: i32 = 5;
const BADGE_TIMER: usize = 1;
const BADGE_DURATION_MS: u32 = 1200;
const MIN_OPACITY: i32 = 15;
const WHEEL_NOTCH: i32 = 120;

thread_local! {
    static OPACITY_STEP: Cell<u8> = const { Cell::new(DEFAULT_OPACITY_STEP) };
}

pub fn opacity_step() -> u8 {
    OPACITY_STEP.get()
}

pub fn set_opacity_step(step: u8) {
    OPACITY_STEP.set(step);
}

#[derive(Clone, Copy)]
enum Badge {
    Zoom,
    Opacity,
}

struct Pin {
    surface: Surface,
    width: i32,
    height: i32,
    zoom: f64,
    opacity: i32,
    opacity_wheel_remainder: i32,
    badge: Option<Badge>,
}

pub unsafe fn register(instance: HINSTANCE) -> io::Result<()> {
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: LoadCursorW(null_mut(), IDC_SIZEALL),
        lpszClassName: CLASS,
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub unsafe fn open(image: Image, bounds: RECT) -> io::Result<()> {
    let (surface, width, height) = image.into_surface()?;
    let window = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
        CLASS,
        w!("Snapshot 贴图"),
        WS_POPUP,
        bounds.left - GLOW,
        bounds.top - GLOW,
        width + GLOW * 2,
        height + GLOW * 2,
        null_mut(),
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    let pin = Box::new(Pin {
        surface,
        width,
        height,
        zoom: 1.0,
        opacity: 100,
        opacity_wheel_remainder: 0,
        badge: None,
    });
    SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(pin) as isize);
    if SetLayeredWindowAttributes(window, 0, 255, LWA_ALPHA) == 0 {
        let error = io::Error::last_os_error();
        DestroyWindow(window);
        return Err(error);
    }
    ShowWindow(window, SW_SHOW);
    SetForegroundWindow(window);
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pin = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Pin;
    match message {
        WM_NCHITTEST => return HTCAPTION as LRESULT,
        WM_ERASEBKGND => return 1,
        WM_NCDESTROY if !pin.is_null() => {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            drop(Box::from_raw(pin));
            return DefWindowProcW(window, message, wparam, lparam);
        }
        WM_NCRBUTTONUP | WM_NCLBUTTONDBLCLK => {
            DestroyWindow(window);
        }
        WM_KEYDOWN if wparam == VK_ESCAPE as usize => {
            DestroyWindow(window);
        }
        WM_MOUSEWHEEL if !pin.is_null() => {
            let delta = i32::from((wparam >> 16) as i16);
            let badge = if wparam & 8 != 0 {
                let pending = (*pin).opacity_wheel_remainder + delta;
                (*pin).opacity_wheel_remainder = pending % WHEEL_NOTCH;
                let notches = pending / WHEEL_NOTCH;
                if notches == 0 {
                    return 0;
                }
                let opacity =
                    ((*pin).opacity + notches * i32::from(opacity_step())).clamp(MIN_OPACITY, 100);
                let alpha = ((opacity * 255 + 50) / 100) as u8;
                if SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA) != 0 {
                    (*pin).opacity = opacity;
                }
                Badge::Opacity
            } else {
                let zoom = ((*pin).zoom * 1.15_f64.powf(f64::from(delta) / f64::from(WHEEL_NOTCH)))
                    .clamp(0.15, 4.0);
                (*pin).zoom = zoom;
                let width = (f64::from((*pin).width) * zoom).round().max(1.0) as i32 + 2 * GLOW;
                let height = (f64::from((*pin).height) * zoom).round().max(1.0) as i32 + 2 * GLOW;
                SetWindowPos(
                    window,
                    null_mut(),
                    0,
                    0,
                    width,
                    height,
                    SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
                );
                Badge::Zoom
            };
            if SetTimer(window, BADGE_TIMER, BADGE_DURATION_MS, None) != 0 {
                (*pin).badge = Some(badge);
            }
            InvalidateRect(window, null(), 0);
        }
        WM_TIMER if !pin.is_null() && wparam == BADGE_TIMER => {
            KillTimer(window, BADGE_TIMER);
            (*pin).badge = None;
            InvalidateRect(window, null(), 0);
        }
        WM_PAINT if !pin.is_null() => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut bounds = RECT::default();
            GetClientRect(window, &mut bounds);
            let brush = GetStockObject(DC_BRUSH);
            for color in [
                0x0056_3515,
                0x0088_5220,
                0x00bd_7130,
                0x00ee_9a42,
                0x00ff_c174,
            ] {
                SetDCBrushColor(dc, color);
                FrameRect(dc, &bounds, brush);
                InflateRect(&mut bounds, -1, -1);
            }
            SetStretchBltMode(dc, HALFTONE);
            SetBrushOrgEx(dc, 0, 0, null_mut());
            StretchBlt(
                dc,
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                (*pin).surface.dc,
                0,
                0,
                (*pin).width,
                (*pin).height,
                SRCCOPY,
            );
            if let Some(badge) = (*pin).badge {
                paint_badge(dc, bounds, &*pin, badge);
            }
            EndPaint(window, &paint);
        }
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}

unsafe fn paint_badge(dc: HDC, image: RECT, pin: &Pin, badge: Badge) {
    const WIDTH: i32 = 110;
    const HEIGHT: i32 = 24;

    let width = image.right - image.left;
    if width < 44 || image.bottom - image.top < HEIGHT {
        return;
    }
    let (name, percent) = match badge {
        Badge::Zoom => ("大小", (pin.zoom * 100.0).round() as i32),
        Badge::Opacity => ("不透明度", pin.opacity),
    };
    let text = if width >= WIDTH {
        format!("{name} {percent}%")
    } else {
        format!("{percent}%")
    };
    let label: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let bounds = RECT {
        left: image.left,
        top: image.top,
        right: image.left + width.min(WIDTH),
        bottom: image.top + HEIGHT,
    };
    paint_size_badge(dc, bounds, label.as_ptr());
}
