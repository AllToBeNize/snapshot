use crate::capture::{Image, Surface};
use std::{
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

struct Pin {
    surface: Surface,
    width: i32,
    height: i32,
    zoom: f64,
    alpha: u8,
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
        alpha: 255,
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
            let steps = (wparam >> 16) as i16;
            if wparam & 8 != 0 {
                let alpha = (i32::from((*pin).alpha) + i32::from(steps) / 6).clamp(40, 255) as u8;
                (*pin).alpha = alpha;
                SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA);
            } else {
                let zoom = ((*pin).zoom * 1.15_f64.powf(f64::from(steps) / 120.0)).clamp(0.15, 4.0);
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
                InvalidateRect(window, null(), 0);
            }
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
            EndPaint(window, &paint);
        }
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}
