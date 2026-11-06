use std::{cell::Cell, io, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

use crate::{
    hotkey::{self, Binding, Shortcut},
    selection_geometry::contains,
};

pub const WHEEL_RECEIVED: u32 = WM_APP + 4;
pub const CANCEL_CAPTURE: u32 = WM_APP + 5;
pub const COPY_CAPTURE: u32 = WM_APP + 6;
pub const SAVE_CAPTURE: u32 = WM_APP + 7;

#[derive(Clone, Copy)]
pub struct InputTarget {
    pub window: HWND,
    pub region: RECT,
    pub panel: RECT,
}

thread_local! {
    static TARGET: Cell<Option<InputTarget>> = const { Cell::new(None) };
    static KEYS: Cell<Keys> = const { Cell::new(Keys { modifiers: 0, pending: None }) };
}

#[derive(Clone, Copy, Default)]
struct Keys {
    modifiers: u16,
    pending: Option<(u32, u32)>,
}

impl Keys {
    fn event(&mut self, key: u32, down: bool) -> Option<u32> {
        let bit = match key as u16 {
            VK_LSHIFT => 1 << 0,
            VK_RSHIFT => 1 << 1,
            VK_LCONTROL => 1 << 2,
            VK_RCONTROL => 1 << 3,
            VK_LMENU => 1 << 4,
            VK_RMENU => 1 << 5,
            VK_LWIN => 1 << 6,
            VK_RWIN => 1 << 7,
            VK_SHIFT => 1 << 0,
            VK_CONTROL => 1 << 2,
            VK_MENU => 1 << 4,
            _ => 0,
        };
        if bit != 0 {
            if down {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
            return None;
        }
        if !down {
            if self.pending.is_some_and(|(pressed, _)| pressed == key) {
                return self.pending.take().map(|(_, action)| action);
            }
            return None;
        }
        if self.pending.is_some() {
            return None;
        }
        let modifiers = u32::from(self.modifiers & 0x03 != 0) * MOD_SHIFT
            | u32::from(self.modifiers & 0x0c != 0) * MOD_CONTROL
            | u32::from(self.modifiers & 0x30 != 0) * MOD_ALT
            | u32::from(self.modifiers & 0xc0 != 0) * MOD_WIN;
        let action = if key == u32::from(VK_ESCAPE) && modifiers == 0 {
            Some(CANCEL_CAPTURE)
        } else {
            match hotkey::current().resolve(Shortcut { modifiers, key }) {
                Some(Binding::Copy) => Some(COPY_CAPTURE),
                Some(Binding::Save) => Some(SAVE_CAPTURE),
                _ => None,
            }
        };
        self.pending = action.map(|action| (key, action));
        None
    }
}

pub struct InputHooks {
    mouse: HHOOK,
    keyboard: HHOOK,
}

impl InputHooks {
    pub unsafe fn install(instance: HINSTANCE) -> io::Result<Self> {
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), instance, 0);
        if mouse.is_null() {
            return Err(io::Error::last_os_error());
        }
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), instance, 0);
        if keyboard.is_null() {
            let error = io::Error::last_os_error();
            UnhookWindowsHookEx(mouse);
            return Err(error);
        }
        Ok(Self { mouse, keyboard })
    }

    pub fn target(target: Option<InputTarget>) -> Option<InputTarget> {
        let previous = TARGET.replace(target);
        if previous.is_none() || target.is_none() {
            KEYS.set(Keys::default());
        }
        previous
    }
}

impl Drop for InputHooks {
    fn drop(&mut self) {
        Self::target(None);
        unsafe {
            UnhookWindowsHookEx(self.keyboard);
            UnhookWindowsHookEx(self.mouse);
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(target) = TARGET.get() {
            let point = (*(data as *const MSLLHOOKSTRUCT)).pt;
            match message as u32 {
                WM_MOUSEMOVE => {}
                WM_MOUSEWHEEL if contains(target.region, point) => {
                    PostMessageW(target.window, WHEEL_RECEIVED, 0, 0);
                }
                WM_LBUTTONDOWN | WM_LBUTTONUP if contains(target.panel, point) => {}
                WM_RBUTTONUP => {
                    PostMessageW(target.window, CANCEL_CAPTURE, 0, 0);
                    return 1;
                }
                _ => return 1,
            }
        }
    }
    CallNextHookEx(null_mut(), code, message, data)
}

unsafe extern "system" fn keyboard_proc(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(target) = TARGET.get() {
            let key = (*(data as *const KBDLLHOOKSTRUCT)).vkCode;
            let down = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let mut keys = KEYS.get();
            let command = keys.event(key, down);
            KEYS.set(keys);
            if let Some(command) = command {
                PostMessageW(target.window, command, 0, 0);
            }
            return 1;
        }
    }
    CallNextHookEx(null_mut(), code, message, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null;
    use windows_sys::{
        core::w,
        Win32::{Foundation::POINT, System::LibraryLoader::GetModuleHandleW},
    };

    #[test]
    fn long_capture_uses_the_same_rebound_copy_shortcut() {
        let mut bindings = hotkey::DEFAULT;
        bindings.set(Binding::Copy, Shortcut::parse("Alt+X").unwrap());
        hotkey::activate(bindings);
        let mut keys = Keys::default();
        keys.event(u32::from(VK_LMENU), true);
        assert_eq!(keys.event(u32::from(b'X'), true), None);
        keys.event(u32::from(VK_LMENU), false);
        assert_eq!(keys.event(u32::from(b'X'), false), Some(COPY_CAPTURE));
        keys.event(u32::from(VK_LCONTROL), true);
        keys.event(u32::from(b'C'), true);
        assert_eq!(keys.event(u32::from(b'C'), false), None);
        hotkey::activate(hotkey::DEFAULT);
    }

    #[test]
    fn scroll_input_blocks_app_clicks_and_queues_wheel_and_cancel() {
        unsafe {
            let window = CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!window.is_null());
            InputHooks::target(Some(InputTarget {
                window,
                region: RECT {
                    right: 100,
                    bottom: 100,
                    ..Default::default()
                },
                panel: RECT {
                    left: 110,
                    right: 200,
                    bottom: 100,
                    ..Default::default()
                },
            }));
            let mut event = MSLLHOOKSTRUCT::default();
            let mut message = MSG::default();
            let dispatch = |kind, event: &MSLLHOOKSTRUCT| {
                mouse_proc(
                    HC_ACTION as i32,
                    kind as WPARAM,
                    event as *const _ as LPARAM,
                )
            };
            let mut take = |kind| PeekMessageW(&mut message, window, kind, kind, PM_REMOVE) != 0;
            event.pt = POINT { x: 50, y: 50 };
            assert_eq!(dispatch(WM_LBUTTONDOWN, &event), 1);
            assert_eq!(dispatch(WM_MOUSEHWHEEL, &event), 1);
            dispatch(WM_MOUSEWHEEL, &event);
            assert!(take(WHEEL_RECEIVED));
            event.pt.x = 120;
            assert_eq!(dispatch(WM_MOUSEWHEEL, &event), 1);
            assert!(!take(WHEEL_RECEIVED));
            assert_eq!(dispatch(WM_RBUTTONUP, &event), 1);
            assert!(take(CANCEL_CAPTURE));
            InputHooks::target(None);
            DestroyWindow(window);
        }
    }
}
