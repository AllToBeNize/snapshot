use std::{
    cell::Cell,
    io,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::COLOR_WINDOW,
        UI::{
            Controls::{EM_LIMITTEXT, EM_SETSEL},
            Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_TAB},
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    apply_preferences,
    hotkey::{self, Bindings, Preferences, Shortcut, BINDINGS},
    overlay_ui::monitor_info,
    pinned, show_error,
    typography::{self, TextStyle},
    wide,
};

const CLASS: *const u16 = w!("Snapshot.Settings");
const WIDTH: i32 = 396;
const HEIGHT: i32 = 476;
const ROW: i32 = 30;
const FIELD: usize = 100;
const OPACITY_STEP_FIELD: usize = FIELD + BINDINGS.len();
const APPLY: usize = 1;
const CANCEL: usize = 2;
const RESET: usize = 3;

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(null_mut()) };
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
    if RegisterClassW(&class) == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn is_open() -> bool {
    !WINDOW.get().is_null()
}

pub unsafe fn dialog_message(message: &mut MSG) -> bool {
    let window = WINDOW.get();
    if window.is_null() || (message.hwnd != window && IsChild(window, message.hwnd) == 0) {
        return false;
    }
    if message.message == WM_KEYDOWN && message.wParam as u16 == VK_ESCAPE {
        DestroyWindow(window);
        return true;
    }
    if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN)
        && (FIELD as i32..OPACITY_STEP_FIELD as i32).contains(&GetDlgCtrlID(message.hwnd))
        && Shortcut::from_key(hotkey::modifiers(), message.wParam as u32).is_some()
    {
        DispatchMessageW(message);
        return true;
    }
    message.message == WM_KEYDOWN
        && message.wParam as u16 == VK_TAB
        && IsDialogMessageW(window, message) != 0
}

pub unsafe fn open(owner: HWND, instance: HINSTANCE) -> io::Result<()> {
    let existing = WINDOW.get();
    if !existing.is_null() {
        SetForegroundWindow(existing);
        return Ok(());
    }
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    let mut frame = RECT {
        right: WIDTH,
        bottom: HEIGHT,
        ..Default::default()
    };
    AdjustWindowRectEx(&mut frame, style, 0, WS_EX_DLGMODALFRAME);
    let width = frame.right - frame.left;
    let height = frame.bottom - frame.top;
    let mut point = POINT::default();
    GetCursorPos(&mut point);
    let (x, y) = monitor_info(point).map_or((CW_USEDEFAULT, CW_USEDEFAULT), |info| {
        let area = info.rcWork;
        (
            (area.left + (area.right - area.left - width) / 2).max(area.left),
            (area.top + (area.bottom - area.top - height) / 2).max(area.top),
        )
    });
    let window = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        CLASS,
        w!("Snapshot 设置"),
        style,
        x,
        y,
        width,
        height,
        owner,
        null_mut(),
        instance,
        null(),
    );
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    if let Err(error) = create_controls(window, instance) {
        DestroyWindow(window);
        return Err(error);
    }
    WINDOW.set(window);
    ShowWindow(window, SW_SHOW);
    SetForegroundWindow(window);
    SetFocus(GetDlgItem(window, FIELD as i32));
    Ok(())
}

unsafe fn create_controls(window: HWND, instance: HINSTANCE) -> io::Result<()> {
    let font = typography::font(TextStyle::Ui) as usize;
    let control = |class, label, style, rect: (i32, i32, i32, i32), id: usize| {
        let (x, y, width, height) = rect;
        let child = CreateWindowExW(
            if id >= FIELD { WS_EX_CLIENTEDGE } else { 0 },
            class,
            label,
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            width,
            height,
            window,
            id as _,
            instance,
            null(),
        );
        if child.is_null() {
            return Err(io::Error::last_os_error());
        }
        SendMessageW(child, WM_SETFONT, font, 0);
        Ok(child)
    };
    control(w!("STATIC"), w!("快捷键"), 0, (22, 14, 340, 20), 0)?;
    control(
        w!("STATIC"),
        w!("点击输入框后按键，保存后立即生效。"),
        0,
        (22, 38, 350, 23),
        0,
    )?;
    let bindings = hotkey::current();
    for (index, binding) in BINDINGS.into_iter().enumerate() {
        let y = 67 + index as i32 * ROW;
        control(
            w!("STATIC"),
            wide(binding.label()).as_ptr(),
            0,
            (23, y + 4, 142, 22),
            0,
        )?;
        let label = wide(&bindings.get(binding).label());
        let input = control(
            w!("EDIT"),
            label.as_ptr(),
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            (169, y, 204, 25),
            FIELD + index,
        )?;
        SendMessageW(input, EM_LIMITTEXT, 40, 0);
        if SetWindowSubclass(input, Some(key_field_proc), 1, 0) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let y = 67 + BINDINGS.len() as i32 * ROW;
    control(
        w!("STATIC"),
        w!("不透明度步长 (%)"),
        0,
        (23, y + 4, 142, 22),
        0,
    )?;
    let opacity_step = wide(&pinned::opacity_step().to_string());
    let input = control(
        w!("EDIT"),
        opacity_step.as_ptr(),
        WS_TABSTOP | ES_NUMBER as u32,
        (169, y, 204, 25),
        OPACITY_STEP_FIELD,
    )?;
    SendMessageW(input, EM_LIMITTEXT, 3, 0);
    control(
        w!("BUTTON"),
        w!("恢复默认"),
        WS_TABSTOP,
        (22, 437, 86, 27),
        RESET,
    )?;
    control(
        w!("BUTTON"),
        w!("取消"),
        WS_TABSTOP,
        (215, 437, 73, 27),
        CANCEL,
    )?;
    control(
        w!("BUTTON"),
        w!("保存"),
        WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        (301, 437, 73, 27),
        APPLY,
    )?;
    Ok(())
}

unsafe extern "system" fn key_field_proc(
    window: HWND,
    message: u32,
    key: WPARAM,
    data: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        if let Some(shortcut) = Shortcut::from_key(hotkey::modifiers(), key as u32) {
            let label = wide(&shortcut.label());
            SetWindowTextW(window, label.as_ptr());
            SendMessageW(
                window,
                EM_SETSEL,
                label.len() - 1,
                (label.len() - 1) as LPARAM,
            );
            return 0;
        }
    }
    DefSubclassProc(window, message, key, data)
}

unsafe fn read_bindings(window: HWND) -> Result<Bindings, String> {
    let mut bindings = hotkey::DEFAULT;
    for (index, binding) in BINDINGS.into_iter().enumerate() {
        let input = GetDlgItem(window, (FIELD + index) as i32);
        let mut text = [0u16; 42];
        let length = GetWindowTextW(input, text.as_mut_ptr(), text.len() as i32);
        let text = String::from_utf16_lossy(&text[..length as usize]);
        let Some(shortcut) = Shortcut::parse(&text) else {
            SetFocus(input);
            return Err(format!(
                "「{}」的快捷键无效。请输入 Ctrl+字母、数字或功能键（F12 除外）。",
                binding.label()
            ));
        };
        bindings.set(binding, shortcut);
    }
    bindings.validate()
}

unsafe fn read_preferences(window: HWND) -> Result<Preferences, String> {
    let bindings = read_bindings(window)?;
    let input = GetDlgItem(window, OPACITY_STEP_FIELD as i32);
    let mut text = [0u16; 4];
    let length = GetWindowTextW(input, text.as_mut_ptr(), text.len() as i32);
    let opacity_step =
        hotkey::parse_opacity_step(&String::from_utf16_lossy(&text[..length as usize]))
            .inspect_err(|_| {
                SetFocus(input);
            })?;
    Ok(Preferences {
        bindings,
        opacity_step,
    })
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => match wparam & 0xffff {
            APPLY => {
                let owner = GetWindow(window, GW_OWNER);
                match read_preferences(window).and_then(|config| apply_preferences(owner, config)) {
                    Ok(()) => {
                        DestroyWindow(window);
                    }
                    Err(error) => show_error(window, &error),
                }
            }
            CANCEL => {
                DestroyWindow(window);
            }
            RESET => {
                for (index, binding) in BINDINGS.into_iter().enumerate() {
                    let input = GetDlgItem(window, (FIELD + index) as i32);
                    let label = wide(&hotkey::DEFAULT.get(binding).label());
                    SetWindowTextW(input, label.as_ptr());
                }
                let input = GetDlgItem(window, OPACITY_STEP_FIELD as i32);
                let label = wide(&hotkey::DEFAULT_OPACITY_STEP.to_string());
                SetWindowTextW(input, label.as_ptr());
            }
            _ => {}
        },
        WM_CLOSE => {
            DestroyWindow(window);
        }
        WM_DESTROY => {
            if WINDOW.get() == window {
                WINDOW.set(null_mut());
            }
        }
        _ => return DefWindowProcW(window, message, wparam, lparam),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::Binding;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    #[test]
    fn settings_reads_edited_native_fields_and_rejects_duplicate_keys() {
        unsafe {
            let instance = GetModuleHandleW(null());
            register(instance).unwrap();
            let window = CreateWindowExW(
                0,
                CLASS,
                null(),
                WS_OVERLAPPED,
                0,
                0,
                WIDTH,
                HEIGHT,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            assert!(!window.is_null());
            create_controls(window, instance).unwrap();
            assert_eq!(read_preferences(window).unwrap(), Preferences::default());
            let opacity = GetDlgItem(window, OPACITY_STEP_FIELD as i32);
            SetWindowTextW(opacity, w!("0"));
            assert!(read_preferences(window).is_err());
            SetWindowTextW(opacity, w!("10"));
            assert_eq!(read_preferences(window).unwrap().opacity_step, 10);
            let copy = GetDlgItem(window, (FIELD + Binding::Copy as usize) as i32);
            SetWindowTextW(copy, w!("Alt+C"));
            assert_eq!(
                read_bindings(window).unwrap().get(Binding::Copy),
                Shortcut::parse("Alt+C").unwrap()
            );
            SetWindowTextW(copy, w!("Ctrl+S"));
            assert!(read_bindings(window).is_err());
            DestroyWindow(window);
        }
    }
}
