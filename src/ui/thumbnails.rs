//! Live DWM window thumbnails shown above the dock on app-icon hover (no focus steal).
use std::cell::RefCell;
use std::mem::size_of;
use std::sync::Once;

use windows::core::w;
use windows::Win32::Foundation::{
    BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetDC, InvalidateRect,
    ReleaseDC, SetBkMode, SetTextColor, TextOutW, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowRect,
    KillTimer, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, WM_LBUTTONUP, WM_PAINT, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::win::dpi;
use crate::win::tasks::focus_window;

pub const TIMER_THUMB_DELAY: usize = 40;
pub const TIMER_THUMB_POLL: usize = 41;

const HOVER_DELAY_MS: u32 = 350;
const POLL_MS: u32 = 80;
const THUMB_W: f32 = 200.0;
const THUMB_H: f32 = 120.0;
const TITLE_H: f32 = 22.0;
const GAP: f32 = 8.0;
const PAD: f32 = 10.0;

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
    fn DwmQueryThumbnailSourceSize(id: isize, size: *mut SIZE) -> i32;
}

struct Thumb {
    hwnd_src: isize,
    title: String,
    id: isize,
    dest: RECT,
}

struct Host {
    hwnd: HWND,
    thumbs: Vec<Thumb>,
    anchor: RECT,
}

struct Pending {
    dock: HWND,
    anchor: RECT,
    windows: Vec<(isize, String)>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

/// Schedule peeks after a short delay. Replaces any pending request.
pub fn schedule(dock: HWND, anchor_screen: RECT, windows: Vec<(isize, String)>) {
    if windows.is_empty() {
        hide();
        return;
    }
    PENDING.with(|p| {
        *p.borrow_mut() = Some(Pending {
            dock,
            anchor: anchor_screen,
            windows,
        });
    });
    unsafe {
        let _ = KillTimer(dock, TIMER_THUMB_DELAY);
        SetTimer(dock, TIMER_THUMB_DELAY, HOVER_DELAY_MS, None);
    }
}

/// Cancel a pending show (pointer left before delay elapsed).
pub fn cancel_pending(dock: HWND) {
    PENDING.with(|p| *p.borrow_mut() = None);
    unsafe {
        let _ = KillTimer(dock, TIMER_THUMB_DELAY);
    }
}

/// Timer callback from the dock wndproc.
pub fn on_dock_timer(dock: HWND, id: usize) {
    match id {
        TIMER_THUMB_DELAY => {
            if let Some(p) = PENDING.with(|p| p.borrow_mut().take()) {
                show(p);
            }
        }
        TIMER_THUMB_POLL => poll_leave(dock),
        _ => {}
    }
}

fn show(pending: Pending) {
    hide();
    register_class();
    let dpi = dpi::system_dpi();
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
    let y = pending.anchor.top - height - dpi::dip_to_px(8.0, dpi);

    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("SmartDockThumbs"),
            w!(""),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            hinstance(),
            None,
        )
    }) else {
        return;
    };

    let mut thumbs = Vec::new();
    for (i, (src, title)) in pending.windows.into_iter().enumerate() {
        let left = pad + i as i32 * (thumb_w + gap);
        let dest = RECT {
            left,
            top: pad + title_h,
            right: left + thumb_w,
            bottom: pad + title_h + thumb_h,
        };
        let mut id = 0isize;
        let hr = unsafe { DwmRegisterThumbnail(hwnd, HWND(src as _), &mut id) };
        if hr < 0 || id == 0 {
            continue;
        }
        let mut src_size = SIZE::default();
        unsafe {
            let _ = DwmQueryThumbnailSourceSize(id, &mut src_size);
        }
        let props = DwmThumbnailProperties {
            dw_flags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_OPACITY
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rc_destination: dest,
            rc_source: RECT::default(),
            opacity: 255,
            f_visible: BOOL::from(true),
            f_source_client_area_only: BOOL::from(true),
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(id, &props);
        }
        thumbs.push(Thumb {
            hwnd_src: src,
            title,
            id,
            dest,
        });
    }

    if thumbs.is_empty() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }

    HOST.with(|h| {
        *h.borrow_mut() = Some(Host {
            hwnd,
            thumbs,
            anchor: pending.anchor,
        });
    });

    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(pending.dock, TIMER_THUMB_POLL, POLL_MS, None);
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Hide peeks and unregister thumbnails.
pub fn hide() {
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().take() {
            for t in &host.thumbs {
                unsafe {
                    let _ = DwmUnregisterThumbnail(t.id);
                }
            }
            unsafe {
                let _ = ShowWindow(host.hwnd, SW_HIDE);
                let _ = DestroyWindow(host.hwnd);
            }
        }
    });
}

fn poll_leave(dock: HWND) {
    let leave = HOST.with(|h| {
        let Ok(g) = h.try_borrow() else {
            return false;
        };
        let Some(host) = g.as_ref() else {
            return false;
        };
        let mut pt = POINT::default();
        let mut rc = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = GetWindowRect(host.hwnd, &mut rc);
        }
        let in_host = pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom;
        let a = host.anchor;
        let in_anchor = pt.x >= a.left && pt.x < a.right && pt.y >= a.top && pt.y < a.bottom;
        !in_host && !in_anchor
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
            BeginPaint(hwnd, &mut ps);
            paint(hwnd);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
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
                hide();
                focus_window(src);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn paint(hwnd: HWND) {
    HOST.with(|h| {
        let Ok(g) = h.try_borrow() else {
            return;
        };
        let Some(host) = g.as_ref() else {
            return;
        };
        if host.hwnd != hwnd {
            return;
        }
        unsafe {
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let hdc = GetDC(hwnd);
            let brush = CreateSolidBrush(COLORREF(0x0028_282C));
            FillRect(hdc, &client, brush);
            let _ = DeleteObject(brush);
            let _ = SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00F1_F1F4));
            for t in &host.thumbs {
                let mut wide: Vec<u16> = t.title.encode_utf16().take(28).collect();
                if wide.len() == 28 {
                    wide.pop();
                    wide.extend("…".encode_utf16());
                }
                let _ = TextOutW(hdc, t.dest.left, t.dest.top - 18, &wide);
            }
            let _ = ReleaseDC(hwnd, hdc);
        }
    });
}
