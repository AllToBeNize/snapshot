#![windows_subsystem = "windows"]

mod annotation;
mod capture;
mod hotkey;
mod icons;
mod overlay_ui;
mod panel_edges;
mod pinned;
mod scroll_capture;
mod scroll_input;
mod scroll_stitch;
mod selection;
mod selection_geometry;
mod settings;
mod targets;
mod toolbar;
mod typography;

use std::{
    cell::Cell,
    io,
    mem::size_of,
    ptr::{null, null_mut},
    sync::OnceLock,
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT},
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const MENU_CAPTURE: usize = 1;
const MENU_CONFIG: usize = 2;
const MENU_EXIT: usize = 3;
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
thread_local! {
    static HOTKEY_ID: Cell<i32> = const { Cell::new(1) };
}

fn main() {
    if let Err(error) = unsafe { run() } {
        unsafe { show_error(null_mut(), &error) };
        std::process::exit(1);
    }
}

unsafe fn run() -> Result<(), String> {
    if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
        return Err(format!(
            "无法启用高 DPI 支持。\n{}",
            io::Error::last_os_error()
        ));
    }
    let preferences = hotkey::load()?;
    let bindings = preferences.bindings;
    hotkey::activate(bindings);
    pinned::set_opacity_step(preferences.opacity_step);
    let instance = GetModuleHandleW(null());
    selection::register(instance).map_err(|error| error.to_string())?;
    scroll_capture::register(instance).map_err(|error| error.to_string())?;
    pinned::register(instance).map_err(|error| error.to_string())?;
    settings::register(instance).map_err(|error| error.to_string())?;
    TASKBAR_CREATED.get_or_init(|| RegisterWindowMessageW(w!("TaskbarCreated")));
    let class = WNDCLASSW {
        lpfnWndProc: Some(tray_proc),
        hInstance: instance,
        lpszClassName: w!("Snapshot.Tray"),
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let window = CreateWindowExW(
        0,
        class.lpszClassName,
        w!("Snapshot"),
        WS_OVERLAPPED,
        0,
        0,
        0,
        0,
        null_mut(),
        null_mut(),
        instance,
        null(),
    );
    if window.is_null() {
        return Err(io::Error::last_os_error().to_string());
    }

    let capture = bindings.get(hotkey::Binding::Capture);
    if RegisterHotKey(
        window,
        HOTKEY_ID.get(),
        capture.modifiers | MOD_NOREPEAT,
        capture.key,
    ) == 0
    {
        let error = io::Error::last_os_error();
        DestroyWindow(window);
        return Err(format!(
            "无法注册快捷键 {}。请关闭占用该快捷键的程序，或检查快捷键配置。\n{error}",
            capture.label(),
        ));
    }
    if !tray_icon(window, NIM_ADD) {
        DestroyWindow(window);
        return Err("无法创建 Snapshot 托盘图标。".into());
    }

    let mut message = MSG::default();
    loop {
        match GetMessageW(&mut message, null_mut(), 0, 0) {
            -1 => {
                let error = io::Error::last_os_error();
                DestroyWindow(window);
                return Err(error.to_string());
            }
            0 => return Ok(()),
            _ => {
                if !settings::dialog_message(&mut message) {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

unsafe extern "system" fn tray_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY if wparam == HOTKEY_ID.get() as usize => begin_capture(window),
        TRAY_MESSAGE => match lparam as u32 {
            WM_RBUTTONUP => tray_menu(window),
            WM_LBUTTONDBLCLK => begin_capture(window),
            _ => {}
        },
        WM_CLOSE => {
            DestroyWindow(window);
        }
        WM_DESTROY => {
            tray_icon(window, NIM_DELETE);
            UnregisterHotKey(window, HOTKEY_ID.get());
            PostQuitMessage(0);
        }
        taskbar_created if TASKBAR_CREATED.get() == Some(&taskbar_created) => {
            tray_icon(window, NIM_ADD);
        }
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}

unsafe fn tray_icon(window: HWND, message: u32) -> bool {
    let mut icon = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
        ..Default::default()
    };
    let label = format!(
        "Snapshot ({})",
        hotkey::current().get(hotkey::Binding::Capture).label()
    );
    for (unit, value) in icon.szTip.iter_mut().take(127).zip(label.encode_utf16()) {
        *unit = value;
    }
    Shell_NotifyIconW(message, &icon) != 0
}

unsafe fn tray_menu(window: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let capture = wide(&format!(
        "截图 ({})",
        hotkey::current().get(hotkey::Binding::Capture).label()
    ));
    AppendMenuW(menu, MF_STRING, MENU_CAPTURE, capture.as_ptr());
    AppendMenuW(menu, MF_STRING, MENU_CONFIG, w!("设置"));
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("退出"));
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor);
    SetForegroundWindow(window);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        cursor.x,
        cursor.y,
        0,
        window,
        null(),
    );
    PostMessageW(window, WM_NULL, 0, 0);
    DestroyMenu(menu);
    match command as usize {
        MENU_CAPTURE => begin_capture(window),
        MENU_CONFIG => {
            if let Err(error) = settings::open(window, GetModuleHandleW(null())) {
                show_error(window, &format!("无法打开设置。\n{error}"));
            }
        }
        MENU_EXIT => {
            DestroyWindow(window);
        }
        _ => {}
    }
}

unsafe fn begin_capture(window: HWND) {
    if settings::is_open() {
        return;
    }
    let instance: HINSTANCE = GetModuleHandleW(null());
    if let Err(error) = selection::open(instance) {
        show_error(window, &format!("无法截取桌面。\n{error}"));
    }
}

unsafe fn apply_preferences(window: HWND, preferences: hotkey::Preferences) -> Result<(), String> {
    let bindings = preferences.bindings.validate()?;
    let capture = bindings.get(hotkey::Binding::Capture);
    let previous = HOTKEY_ID.get();
    let changed = capture != hotkey::current().get(hotkey::Binding::Capture);
    let next = 3 - previous;
    if changed && RegisterHotKey(window, next, capture.modifiers | MOD_NOREPEAT, capture.key) == 0 {
        return Err(format!(
            "无法注册 {}，它可能已被其他程序占用。",
            capture.label()
        ));
    }
    if let Err(error) = hotkey::save(preferences) {
        if changed {
            UnregisterHotKey(window, next);
        }
        return Err(error);
    }
    if changed {
        UnregisterHotKey(window, previous);
        HOTKEY_ID.set(next);
    }
    hotkey::activate(bindings);
    pinned::set_opacity_step(preferences.opacity_step);
    tray_icon(window, NIM_MODIFY);
    Ok(())
}

unsafe fn show_error(window: HWND, message: &str) {
    MessageBoxW(
        window,
        wide(message).as_ptr(),
        w!("Snapshot"),
        MB_OK | MB_ICONERROR,
    );
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
