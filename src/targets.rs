use std::{
    collections::HashSet,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::HWND as AutomationHwnd,
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_MULTITHREADED,
        },
        UI::Accessibility::{
            CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest,
            IUIAutomationCondition, TreeScope_Children, UIA_BoundingRectanglePropertyId,
            UIA_IsOffscreenPropertyId,
        },
    },
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT},
    Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS},
    UI::WindowsAndMessaging::*,
};

use crate::selection_geometry::{contains, intersect};

pub const ELEMENT_READY: u32 = WM_APP + 2;

#[derive(Clone, Copy)]
pub struct WindowTarget {
    pub id: usize,
    pub rect: RECT,
}

pub unsafe fn windows_at_capture() -> Vec<WindowTarget> {
    let mut windows = Vec::new();
    EnumWindows(
        Some(collect_window),
        (&mut windows as *mut Vec<WindowTarget>) as LPARAM,
    );
    windows
}

unsafe extern "system" fn collect_window(window: HWND, context: LPARAM) -> i32 {
    if IsWindowVisible(window) == 0
        || IsIconic(window) != 0
        || window == GetShellWindow()
        || GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT != 0
    {
        return 1;
    }
    let mut class = [0u16; 32];
    let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) as usize;
    if ["WorkerW", "Progman"]
        .iter()
        .any(|name| class[..length].iter().copied().eq(name.encode_utf16()))
    {
        return 1;
    }
    let mut cloaked = 0u32;
    DwmGetWindowAttribute(
        window,
        DWMWA_CLOAKED as u32,
        (&mut cloaked as *mut u32).cast(),
        size_of::<u32>() as u32,
    );
    if cloaked != 0 {
        return 1;
    }

    let mut rect = RECT::default();
    let status = DwmGetWindowAttribute(
        window,
        DWMWA_EXTENDED_FRAME_BOUNDS as u32,
        (&mut rect as *mut RECT).cast(),
        size_of::<RECT>() as u32,
    );
    if status >= 0 || GetWindowRect(window, &mut rect) != 0 {
        let windows = &mut *(context as *mut Vec<WindowTarget>);
        windows.push(WindowTarget {
            id: window as usize,
            rect,
        });
    }
    1
}

#[derive(Clone, Copy)]
pub struct Request {
    pub window: WindowTarget,
    pub point: POINT,
}

pub struct ElementMatch {
    pub request: Request,
    pub rect: Option<RECT>,
}

pub struct ElementProbe {
    pending: Arc<Mutex<Option<Request>>>,
    wake: mpsc::SyncSender<()>,
    results: mpsc::Receiver<ElementMatch>,
}

impl ElementProbe {
    pub fn start(overlay: HWND) -> Option<Self> {
        let overlay = overlay as usize;
        let pending = Arc::new(Mutex::new(None::<Request>));
        let inbox = Arc::clone(&pending);
        let (wake, events) = mpsc::sync_channel(1);
        let (publish, results) = mpsc::channel();
        thread::Builder::new()
            .name("snapshot-elements".into())
            .spawn(move || unsafe {
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    return;
                }
                if let Ok(automation) = Automation::new() {
                    let mut seen = HashSet::new();
                    let mut retry = None;
                    loop {
                        let request = match retry.take() {
                            Some(request) => {
                                match events.recv_timeout(Duration::from_millis(160)) {
                                    Ok(()) => inbox.lock().unwrap().take(),
                                    Err(mpsc::RecvTimeoutError::Timeout) => Some(request),
                                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                }
                            }
                            None if events.recv().is_ok() => inbox.lock().unwrap().take(),
                            None => break,
                        };
                        let Some(request) = request else {
                            continue;
                        };
                        let rect = automation.find(request).ok().flatten();
                        // Browsers can expose their web accessibility tree after the first query.
                        if seen.insert(request.window.id)
                            && rect
                                .is_none_or(|rect| area(rect) * 10 >= area(request.window.rect) * 9)
                        {
                            retry = Some(request);
                        }
                        if publish.send(ElementMatch { request, rect }).is_err() {
                            break;
                        }
                        PostMessageW(overlay as HWND, ELEMENT_READY, 0, 0);
                    }
                }
                CoUninitialize();
            })
            .ok()?;
        Some(Self {
            pending,
            wake,
            results,
        })
    }

    pub fn request(&self, request: Request) {
        *self.pending.lock().unwrap() = Some(request);
        let _ = self.wake.try_send(());
    }

    pub fn latest(&self) -> Option<ElementMatch> {
        self.results.try_iter().last()
    }
}

struct Automation {
    client: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    condition: IUIAutomationCondition,
}

impl Automation {
    unsafe fn new() -> windows::core::Result<Self> {
        let client: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
        if let Ok(timeouts) = client.cast::<IUIAutomation2>() {
            let _ = timeouts.SetConnectionTimeout(120);
            let _ = timeouts.SetTransactionTimeout(120);
        }
        let cache = client.CreateCacheRequest()?;
        cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
        cache.AddProperty(UIA_IsOffscreenPropertyId)?;
        let condition = client.CreateTrueCondition()?;
        Ok(Self {
            client,
            cache,
            condition,
        })
    }

    unsafe fn find(&self, request: Request) -> windows::core::Result<Option<RECT>> {
        let deadline = Instant::now() + Duration::from_millis(100);
        let root = self
            .client
            .ElementFromHandle(AutomationHwnd(request.window.id as _))?;
        let mut candidates = vec![(root, request.window.rect)];
        let mut best = None;
        let mut smallest = i64::MAX;
        for _ in 0..32 {
            if Instant::now() >= deadline {
                break;
            }
            let Some((parent, parent_rect)) = candidates.pop() else {
                break;
            };
            let Ok(children) =
                parent.FindAllBuildCache(TreeScope_Children, &self.condition, &self.cache)
            else {
                continue;
            };
            let Ok(length) = children.Length() else {
                continue;
            };
            for index in (0..length.min(384)).rev() {
                let Ok(child) = children.GetElement(index) else {
                    continue;
                };
                if child
                    .CachedIsOffscreen()
                    .is_ok_and(|offscreen| offscreen.as_bool())
                {
                    continue;
                }
                let Ok(bounds) = child.CachedBoundingRectangle() else {
                    continue;
                };
                let Some(rect) = intersect(
                    parent_rect,
                    RECT {
                        left: bounds.left,
                        top: bounds.top,
                        right: bounds.right,
                        bottom: bounds.bottom,
                    },
                ) else {
                    continue;
                };
                if contains(rect, request.point) {
                    let size = area(rect);
                    if size < smallest {
                        smallest = size;
                        best = Some(rect);
                    }
                    candidates.push((child, rect));
                }
            }
        }
        Ok(best)
    }
}

fn area(rect: RECT) -> i64 {
    i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top)
}
