use std::{
    cell::OnceCell,
    collections::HashSet,
    ptr::null_mut,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::HWND as AutomationHwnd,
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            Variant::{VARIANT, VT_DISPATCH, VT_I4},
        },
        UI::Accessibility::{
            AccessibleObjectFromWindow, CUIAutomation8, IAccessible, IUIAutomation, IUIAutomation2,
            IUIAutomationCacheRequest, IUIAutomationCondition, TreeScope_Children,
            UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId, UIA_IsOffscreenPropertyId,
            UIA_TextControlTypeId,
        },
    },
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT},
    Graphics::{
        Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS},
        Gdi::ScreenToClient,
    },
    UI::{
        Controls::{STATE_SYSTEM_INVISIBLE, STATE_SYSTEM_OFFSCREEN},
        WindowsAndMessaging::*,
    },
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
    pub region: Option<ElementRegion>,
}

#[derive(Clone, Copy)]
pub struct ElementRegion {
    pub rect: RECT,
    pub weak: bool,
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
                {
                    let automation = OnceCell::new();
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
                        let region = find_element(request, &automation);
                        // Browsers can expose their web accessibility tree after the first query.
                        if seen.insert(request.window.id) && region.is_none_or(|region| region.weak)
                        {
                            retry = Some(request);
                        }
                        if publish.send(ElementMatch { request, region }).is_err() {
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

unsafe fn find_element(
    request: Request,
    automation: &OnceCell<Option<Automation>>,
) -> Option<ElementRegion> {
    let window = request.window.id as HWND;
    if IsHungAppWindow(window) != 0 {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(100);
    let child = child_at(request);
    let mut hit =
        child.and_then(|(handle, bounds)| msaa_at(handle, bounds, request.point, deadline));
    if hit.is_none_or(|rect| area(rect) * 10 >= area(request.window.rect) * 9)
        && Instant::now() < deadline
    {
        hit = msaa_at(window, request.window.rect, request.point, deadline).or(hit);
    }
    let region = hit.map(|rect| ElementRegion {
        rect,
        weak: area(rect) * 10 >= area(request.window.rect) * 9,
    });
    if region.is_some_and(|region| !region.weak) {
        return region;
    }
    let uia = automation
        .get_or_init(|| Automation::new().ok())
        .as_ref()
        .and_then(|automation| automation.find(request).ok().flatten());
    uia.filter(|region| !region.weak)
        .or_else(|| child.map(|(_, rect)| ElementRegion { rect, weak: true }))
        .or(uia)
        .or(region)
}

unsafe fn child_at(request: Request) -> Option<(HWND, RECT)> {
    let root = request.window.id as HWND;
    let mut parent = root;
    let mut bounds = request.window.rect;
    for _ in 0..32 {
        let mut point = request.point;
        if ScreenToClient(parent, &mut point) == 0 {
            break;
        }
        let child = ChildWindowFromPointEx(parent, point, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT);
        if child.is_null() || child == parent {
            break;
        }
        let mut rect = RECT::default();
        if GetWindowRect(child, &mut rect) == 0 {
            break;
        }
        let Some(clipped) = intersect(rect, bounds).filter(|rect| contains(*rect, request.point))
        else {
            break;
        };
        parent = child;
        bounds = clipped;
    }
    (parent != root).then_some((parent, bounds))
}

unsafe fn msaa_at(window: HWND, bounds: RECT, point: POINT, deadline: Instant) -> Option<RECT> {
    let mut raw = null_mut();
    AccessibleObjectFromWindow(
        AutomationHwnd(window),
        OBJID_WINDOW as u32,
        &IAccessible::IID,
        &mut raw,
    )
    .ok()?;
    if raw.is_null() {
        return None;
    }
    let mut object = IAccessible::from_raw(raw);
    let self_id = VARIANT::from(CHILDID_SELF as i32);
    let mut clip = bounds;
    let mut best = None;
    for _ in 0..60 {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(hit) = object.accHitTest(point.x, point.y) else {
            break;
        };
        let next = match hit.vt() {
            VT_DISPATCH => IDispatch::try_from(&hit)
                .and_then(|dispatch| dispatch.cast::<IAccessible>())
                .ok(),
            VT_I4 => {
                if let Some(rect) = accessible_bounds(&object, &hit, clip, point) {
                    best = Some(rect);
                }
                break;
            }
            _ => break,
        };
        let Some(next) = next.filter(|next| *next != object) else {
            break;
        };
        if let Some(rect) = accessible_bounds(&next, &self_id, clip, point) {
            clip = rect;
            best = Some(rect);
        }
        object = next;
    }
    best
}

unsafe fn accessible_bounds(
    object: &IAccessible,
    child: &VARIANT,
    clip: RECT,
    point: POINT,
) -> Option<RECT> {
    let state = object
        .get_accState(child)
        .ok()
        .filter(|state| state.vt() == VT_I4)
        .and_then(|state| u32::try_from(&state).ok());
    if state.is_some_and(|state| state & (STATE_SYSTEM_INVISIBLE | STATE_SYSTEM_OFFSCREEN) != 0) {
        return None;
    }
    let (mut left, mut top, mut width, mut height) = (0, 0, 0, 0);
    object
        .accLocation(&mut left, &mut top, &mut width, &mut height, child)
        .ok()?;
    intersect(
        clip,
        RECT {
            left,
            top,
            right: left.checked_add(width)?,
            bottom: top.checked_add(height)?,
        },
    )
    .filter(|rect| contains(*rect, point))
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
        cache.AddProperty(UIA_ControlTypePropertyId)?;
        let condition = client.CreateTrueCondition()?;
        Ok(Self {
            client,
            cache,
            condition,
        })
    }

    unsafe fn find(&self, request: Request) -> windows::core::Result<Option<ElementRegion>> {
        let deadline = Instant::now() + Duration::from_millis(100);
        let root = self
            .client
            .ElementFromHandle(AutomationHwnd(request.window.id as _))?;
        let mut candidates = vec![(root, request.window.rect)];
        let mut best = None;
        let mut score = (true, i64::MAX);
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
                if bounds.left >= bounds.right || bounds.top >= bounds.bottom {
                    // Virtual accessibility groups can have no rectangle of their own.
                    candidates.push((child, parent_rect));
                    continue;
                }
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
                    let weak = size * 10 >= area(request.window.rect) * 9
                        || child
                            .CachedControlType()
                            .is_ok_and(|kind| kind == UIA_TextControlTypeId);
                    if (weak, size) < score {
                        score = (weak, size);
                        best = Some(ElementRegion { rect, weak });
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
