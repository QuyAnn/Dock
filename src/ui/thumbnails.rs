//! Live DWM window thumbnails shown above the dock on app-icon hover (no focus steal).
use std::cell::RefCell;
use std::mem::size_of;
use std::sync::Once;

use windows::core::w;
use windows::Win32::Foundation::{
    BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, HDC, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, GetWindowRect, KillTimer,
    RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, MA_NOACTIVATE, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::dock::RectF;
use super::renderer::{apply_acrylic, apply_popup_corners, Align, Renderer, POPUP_CORNER_RADIUS};
use crate::win::dpi;
use crate::win::tasks::focus_window;

pub const TIMER_THUMB_DELAY: usize = 40;
pub const TIMER_THUMB_POLL: usize = 41;

/// Rest time on an icon before the first peek appears.
const HOVER_DELAY_MS: u32 = 250;
/// Switching between icons while a peek is already open: near-instant, no flicker.
const SWITCH_DELAY_MS: u32 = 60;
/// Pointer must stay outside icon + peek this long before the peek hides (crossing the gap).
const LEAVE_GRACE_MS: u32 = 150;
const POLL_MS: u32 = 40;
const THUMB_W: f32 = 200.0;
const THUMB_H: f32 = 120.0;
const TITLE_H: f32 = 22.0;
const GAP: f32 = 8.0;
const PAD: f32 = 10.0;
/// Space between the peek and the dock top, DIP. The leave test bridges it.
const LIFT: f32 = 8.0;

#[repr(C)]
struct DwmThumbnailProperties {
    dw_flags: u32,
    rc_destination: RECT,
    rc_source: RECT,
    opacity: u8,
    f_visible: BOOL,
    f_source_client_area_only: BOOL,
}

const DWM_TNP_RECTDESTINATION: u32 = 0x1;
const DWM_TNP_VISIBLE: u32 = 0x8;
const DWM_TNP_OPACITY: u32 = 0x4;
const DWM_TNP_SOURCECLIENTAREAONLY: u32 = 0x10;

#[link(name = "dwmapi")]
extern "system" {
    fn DwmRegisterThumbnail(dest: HWND, src: HWND, id: *mut isize) -> i32;
    fn DwmUnregisterThumbnail(id: isize) -> i32;
    fn DwmUpdateThumbnailProperties(id: isize, props: *const DwmThumbnailProperties) -> i32;
}

struct Thumb {
    hwnd_src: isize,
    title: String,
    id: isize,
    dest: RECT,
}

/// The single peek window. Created once, then only shown/hidden/moved.
struct Host {
    hwnd: HWND,
    /// D2D surface for the glass panel and titles (window is reused, so is the renderer).
    renderer: Renderer,
    visible: bool,
    thumbs: Vec<Thumb>,
    anchor: RECT,
    dock: HWND,
    /// Poll ticks the pointer has been outside icon + peek.
    outside_ms: u32,
}

struct Pending {
    dock: HWND,
    anchor: RECT,
    windows: Vec<(isize, String)>,
}

thread_local! {
    /// True while an app context menu is open: no peeks, no pending timers.
    static SUPPRESSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

fn same_windows(a: &[(isize, String)], b: impl Iterator<Item = isize>) -> bool {
    let b: Vec<isize> = b.collect();
    a.len() == b.len() && a.iter().zip(&b).all(|((x, _), y)| x == y)
}

/// Suppresses peeks (e.g. while the app context menu is open). Turning it on hides any visible
/// peek and cancels a pending one immediately; hover is ignored until it is turned off.
pub fn set_suppressed(on: bool) {
    SUPPRESSED.with(|s| s.set(on));
    if on {
        if let Some(p) = PENDING.with(|p| p.borrow_mut().take()) {
            unsafe {
                let _ = KillTimer(p.dock, TIMER_THUMB_DELAY);
            }
        }
        hide();
    }
}

/// Requests peeks for one app's windows. Called on every pointer move over an icon, so it is
/// idempotent: the same app neither restarts the delay nor re-registers DWM thumbnails.
pub fn schedule(dock: HWND, anchor_screen: RECT, windows: Vec<(isize, String)>) {
    if SUPPRESSED.with(|s| s.get()) {
        return;
    }
    if windows.is_empty() {
        cancel_pending(dock);
        return;
    }
    // Already showing this app: keep it, just refresh the anchor (magnification moves slots).
    let showing = HOST.with(|h| {
        let mut g = h.borrow_mut();
        match g.as_mut() {
            Some(host) if host.visible && same_windows(&windows, host.thumbs.iter().map(|t| t.hwnd_src)) => {
                host.anchor = anchor_screen;
                host.outside_ms = 0;
                Some(true)
            }
            Some(host) if host.visible => Some(false),
            _ => None,
        }
    });
    if showing == Some(true) {
        cancel_pending(dock);
        return;
    }
    let already_pending = PENDING.with(|p| {
        p.borrow()
            .as_ref()
            .is_some_and(|q| q.dock == dock && same_windows(&windows, q.windows.iter().map(|(h, _)| *h)))
    });
    PENDING.with(|p| {
        *p.borrow_mut() = Some(Pending { dock, anchor: anchor_screen, windows });
    });
    if already_pending {
        return; // keep the running timer; do not restart the delay on every move
    }
    let delay = if showing == Some(false) { SWITCH_DELAY_MS } else { HOVER_DELAY_MS };
    unsafe {
        let _ = KillTimer(dock, TIMER_THUMB_DELAY);
        SetTimer(dock, TIMER_THUMB_DELAY, delay, None);
    }
}

/// Cancel a pending show (pointer left before delay elapsed).
pub fn cancel_pending(dock: HWND) {
    let had = PENDING.with(|p| p.borrow_mut().take().is_some());
    if had {
        unsafe {
            let _ = KillTimer(dock, TIMER_THUMB_DELAY);
        }
    }
}

/// Timer callback from the dock wndproc.
pub fn on_dock_timer(dock: HWND, id: usize) {
    match id {
        TIMER_THUMB_DELAY => {
            unsafe {
                let _ = KillTimer(dock, TIMER_THUMB_DELAY);
            }
            if let Some(p) = PENDING.with(|p| p.borrow_mut().take()) {
                if !SUPPRESSED.with(|s| s.get()) {
                    show(p);
                }
            }
        }
        TIMER_THUMB_POLL => poll_leave(dock),
        _ => {}
    }
}

/// Creates the peek window once (hidden). Later calls return the same HWND.
fn host_hwnd() -> Option<HWND> {
    if let Some(h) = HOST.with(|h| h.borrow().as_ref().map(|host| host.hwnd)) {
        return Some(h);
    }
    register_class();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("SmartDockThumbs"),
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinstance(),
            None,
        )
    }
    .ok()?;
    apply_popup_corners(hwnd);
    apply_acrylic(hwnd, super::glass::PANEL_ACRYLIC_TINT);
    HOST.with(|h| {
        *h.borrow_mut() = Some(Host {
            hwnd,
            renderer: Renderer::new(hwnd, 1, 1, dpi::system_dpi()),
            visible: false,
            thumbs: Vec::new(),
            anchor: RECT::default(),
            dock: HWND::default(),
            outside_ms: 0,
        });
    });
    Some(hwnd)
}

fn unregister_all(thumbs: &mut Vec<Thumb>) {
    for t in thumbs.drain(..) {
        unsafe {
            let _ = DwmUnregisterThumbnail(t.id);
        }
    }
}

fn show(pending: Pending) {
    let Some(hwnd) = host_hwnd() else { return };
    let dpi = dpi::window_dpi(pending.dock);
    let n = pending.windows.len().max(1) as f32;
    let width = dpi::dip_to_px(PAD * 2.0 + n * THUMB_W + (n - 1.0).max(0.0) * GAP, dpi);
    let height = dpi::dip_to_px(PAD * 2.0 + TITLE_H + THUMB_H, dpi);
    let thumb_w = dpi::dip_to_px(THUMB_W, dpi);
    let thumb_h = dpi::dip_to_px(THUMB_H, dpi);
    let title_h = dpi::dip_to_px(TITLE_H, dpi);
    let pad = dpi::dip_to_px(PAD, dpi);
    let gap = dpi::dip_to_px(GAP, dpi);

    let cx = (pending.anchor.left + pending.anchor.right) / 2;
    let x = cx - width / 2;
    let y = pending.anchor.top - height - dpi::dip_to_px(LIFT, dpi);

    let mut thumbs = Vec::new();
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            unregister_all(&mut host.thumbs);
        }
    });
    for (i, (src, title)) in pending.windows.into_iter().enumerate() {
        let left = pad + i as i32 * (thumb_w + gap);
        let dest = RECT { left, top: pad + title_h, right: left + thumb_w, bottom: pad + title_h + thumb_h };
        let mut id = 0isize;
        let hr = unsafe { DwmRegisterThumbnail(hwnd, HWND(src as _), &mut id) };
        if hr < 0 || id == 0 {
            continue;
        }
        let props = DwmThumbnailProperties {
            dw_flags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
            rc_destination: dest,
            rc_source: RECT::default(),
            opacity: 255,
            f_visible: BOOL::from(true),
            f_source_client_area_only: BOOL::from(true),
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(id, &props);
        }
        thumbs.push(Thumb { hwnd_src: src, title, id, dest });
    }
    if thumbs.is_empty() {
        hide();
        return;
    }
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            host.thumbs = thumbs;
            host.renderer.resize(width, height, dpi);
            host.anchor = pending.anchor;
            host.dock = pending.dock;
            host.visible = true;
            host.outside_ms = 0;
        }
    });
    unsafe {
        // One SetWindowPos moves, resizes and shows without activating; no destroy/recreate.
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(hwnd, None, false);
        let _ = KillTimer(pending.dock, TIMER_THUMB_POLL);
        SetTimer(pending.dock, TIMER_THUMB_POLL, POLL_MS, None);
    }
}

/// Hides peeks and releases the DWM thumbnails. The window itself is kept for reuse.
pub fn hide() {
    let hidden = HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else { return None };
        let host = g.as_mut()?;
        unregister_all(&mut host.thumbs);
        let was = host.visible;
        host.visible = false;
        host.outside_ms = 0;
        Some((host.hwnd, host.dock, was))
    });
    if let Some((hwnd, dock, was)) = hidden {
        if was {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                if !dock.is_invalid() {
                    let _ = KillTimer(dock, TIMER_THUMB_POLL);
                }
            }
        }
    }
}

fn poll_leave(dock: HWND) {
    let leave = HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else {
            return false;
        };
        let Some(host) = g.as_mut() else {
            return true;
        };
        if !host.visible {
            return true;
        }
        let mut pt = POINT::default();
        let mut rc = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = GetWindowRect(host.hwnd, &mut rc);
        }
        let a = host.anchor;
        // Bridge the gap between the icon and the peek so moving up never flickers it away.
        let in_host = pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom;
        let in_bridge = pt.x >= a.left && pt.x < a.right && pt.y >= rc.bottom && pt.y < a.top;
        let in_anchor = pt.x >= a.left && pt.x < a.right && pt.y >= a.top && pt.y < a.bottom;
        if in_host || in_bridge || in_anchor {
            host.outside_ms = 0;
            false
        } else {
            host.outside_ms += POLL_MS;
            host.outside_ms >= LEAVE_GRACE_MS
        }
    });
    if leave {
        hide();
        unsafe {
            let _ = KillTimer(dock, TIMER_THUMB_POLL);
        }
    }
}

fn hinstance() -> HINSTANCE {
    let m = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    HINSTANCE(m.0)
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance(),
            lpszClassName: w!("SmartDockThumbs"),
            ..Default::default()
        };
        unsafe {
            RegisterClassExW(&wc);
        }
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        // Painted fully in WM_PAINT; skipping erase avoids a white/black flash on show.
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            let x = (lp.0 & 0xffff) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
            let target = HOST.with(|h| {
                h.borrow().as_ref().and_then(|host| {
                    host.thumbs
                        .iter()
                        .find(|t| {
                            x >= t.dest.left
                                && x < t.dest.right
                                && y >= t.dest.top - 20
                                && y < t.dest.bottom
                        })
                        .map(|t| t.hwnd_src)
                })
            });
            if let Some(src) = target {
                crate::log(&format!("peek click -> focus hwnd={src:#x}"));
                hide();
                focus_window(src);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn paint(hwnd: HWND, _hdc: HDC) {
    HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else {
            return;
        };
        let Some(host) = g.as_mut() else {
            return;
        };
        if host.hwnd != hwnd {
            return;
        }
        let scale = dpi::scale(host.renderer.dpi());
        let (w, hgt) = host.renderer.size();
        let titles: Vec<(String, RectF)> = host
            .thumbs
            .iter()
            .map(|t| {
                (
                    t.title.clone(),
                    RectF {
                        x: t.dest.left as f32 / scale,
                        y: t.dest.top as f32 / scale - 20.0,
                        w: (t.dest.right - t.dest.left) as f32 / scale,
                        h: 18.0,
                    },
                )
            })
            .collect();
        let r = &mut host.renderer;
        let panel = RectF { x: 0.5, y: 0.5, w: w - 1.0, h: hgt - 1.0 };
        let _ = r.begin().and_then(|_| {
            super::glass::draw_panel(r, panel, POPUP_CORNER_RADIUS)?;
            for (title, rc) in &titles {
                // DirectWrite: full Unicode (Vietnamese) titles, ellipsized to the thumbnail width.
                r.draw_text(title, *rc, 11.0, 0xF4F4_F7FF, Align::Left)?;
            }
            r.end()
        });
    });
}
