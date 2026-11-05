#![windows_subsystem = "windows"]

mod capture;
mod hotkey;
mod selection;
mod selection_geometry;
mod targets;

use std::{
    io,
    mem::size_of,
    process::Command,
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
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey},
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const HOTKEY_ID: i32 = 1;
const MENU_CAPTURE: usize = 1;
const MENU_CONFIG: usize = 2;
const MENU_EXIT: usize = 3;
static CONFIG: OnceLock<hotkey::Config> = OnceLock::new();
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

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
    let config = hotkey::Config::load()?;
    let config = CONFIG.get_or_init(|| config);
    let instance = GetModuleHandleW(null());
    selection::register(instance).map_err(|error| error.to_string())?;
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

    if RegisterHotKey(window, HOTKEY_ID, config.modifiers, config.key) == 0 {
        let error = io::Error::last_os_error();
        DestroyWindow(window);
        return Err(format!(
            "无法注册快捷键 {}。请关闭占用该快捷键的程序，或编辑 {} 后重启 Snapshot。\n{error}",
            config.label,
            config.path.display()
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
                TranslateMessage(&message);
                DispatchMessageW(&message);
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
        WM_HOTKEY if wparam == HOTKEY_ID as usize => begin_capture(window),
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
            UnregisterHotKey(window, HOTKEY_ID);
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
    let config = CONFIG.get().expect("configuration initialized");
    let mut icon = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
        ..Default::default()
    };
    let label = format!("Snapshot ({})", config.label);
    for (unit, value) in icon.szTip.iter_mut().take(127).zip(label.encode_utf16()) {
        *unit = value;
    }
    Shell_NotifyIconW(message, &icon) != 0
}

unsafe fn tray_menu(window: HWND) {
    let config = CONFIG.get().expect("configuration initialized");
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let capture = wide(&format!("截图 ({})", config.label));
    AppendMenuW(menu, MF_STRING, MENU_CAPTURE, capture.as_ptr());
    AppendMenuW(menu, MF_STRING, MENU_CONFIG, w!("编辑快捷键配置"));
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
            if let Err(error) = Command::new("notepad.exe").arg(&config.path).spawn() {
                show_error(window, &format!("无法打开配置。\n{error}"));
            }
        }
        MENU_EXIT => {
            DestroyWindow(window);
        }
        _ => {}
    }
}

unsafe fn begin_capture(window: HWND) {
    let instance: HINSTANCE = GetModuleHandleW(null());
    if let Err(error) = selection::open(instance) {
        show_error(window, &format!("无法截取桌面。\n{error}"));
    }
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
