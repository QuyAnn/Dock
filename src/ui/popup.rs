//! The single popup window. Created on `show`, destroyed on `hide` (or when dismissed).
//! Never activates: WS_EX_NOACTIVATE, so keyboard focus stays where it was.
//! Dismissal is polled (GetAsyncKeyState / GetCursorPos) because the window never gets input focus.
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::sync::Once;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowRect,
    KillTimer, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use super::dock::RectF;
use std::sync::Arc;

use super::liquid::LiquidBitmap;
use super::renderer::{apply_acrylic, apply_popup_corners, Renderer, POPUP_CORNER_RADIUS};
use crate::win::dpi;

const GAP: f32 = 8.0;
const MARGIN: f32 = 6.0;
/// Must equal the DWM corner so the drawn panel and the window clip match exactly.
const RADIUS: f32 = POPUP_CORNER_RADIUS;
const TIMER_ID: usize = 1;
const POLL_MS: u32 = 40;

struct Popup {
    hwnd: HWND,
    renderer: Renderer,
    anchor: RectF,
    /// Last polled key states. Start as "down" so the click that opened us is ignored.
    mouse_down: bool,
    esc_down: bool,
    /// Liquid Glass backdrop cut from the wallpaper at this popup's position; None until the
    /// wallpaper is decoded (plain acrylic panel then).
    backdrop: Option<Arc<LiquidBitmap>>,
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: we own this window; it is destroyed exactly once, here.
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_ID);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

thread_local! {
    static POPUP: RefCell<Option<Popup>> = const { RefCell::new(None) };
    static PAINTER: Cell<Option<fn(&mut Renderer, RectF)>> = const { Cell::new(None) };
    static CLICK: Cell<Option<fn(f32, f32)>> = const { Cell::new(None) };
    static ON_HIDE: Cell<Option<fn()>> = const { Cell::new(None) };
    static DRAG: Cell<Option<fn(DragPhase, f32, f32) -> bool>> = const { Cell::new(None) };
    static TICK: Cell<Option<fn()>> = const { Cell::new(None) };
    static RCLICK: Cell<Option<fn(f32, f32)>> = const { Cell::new(None) };
    /// True between a captured mouse-down and its mouse-up.
    static DRAGGING: Cell<bool> = const { Cell::new(false) };
    static LAST_POS: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

/// Phase of a captured drag inside the popup (sliders).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragPhase {
    Down,
    Move,
    Up,
}

/// Drag handler: called on mouse-down with `Down`; returning true captures the mouse, and then
/// every WM_MOUSEMOVE (`Move`) and the release (`Up`) go to it instead of the click handler.
pub fn set_drag_handler(f: fn(DragPhase, f32, f32) -> bool) {
    DRAG.with(|c| c.set(Some(f)));
}

/// Receives right clicks (button up) inside the popup, in client DIPs.
pub fn set_rclick_handler(f: fn(f32, f32)) {
    RCLICK.with(|c| c.set(Some(f)));
}

/// Pointer position in popup client DIPs while it is over the visible popup.
pub fn cursor_in_popup() -> Option<(f32, f32)> {
    POPUP.with(|p| {
        let g = p.try_borrow().ok()?;
        let pw = g.as_ref()?;
        let mut pt = POINT::default();
        let mut rc = RECT::default();
        // SAFETY: out-params are valid locals; hwnd is our live popup.
        unsafe {
            if !windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(pw.hwnd).as_bool() {
                return None;
            }
            GetCursorPos(&mut pt).ok()?;
            GetWindowRect(pw.hwnd, &mut rc).ok()?;
        }
        if pt.x < rc.left || pt.x >= rc.right || pt.y < rc.top || pt.y >= rc.bottom {
            return None;
        }
        let dpi = pw.renderer.dpi();
        Some((dpi::px_to_dip(pt.x - rc.left, dpi), dpi::px_to_dip(pt.y - rc.top, dpi)))
    })
}

/// Called every poll tick (~100 ms) while the popup is visible.
pub fn set_tick_handler(f: fn()) {
    TICK.with(|c| c.set(Some(f)));
}

/// True while a slider drag holds the mouse capture.
pub fn is_dragging() -> bool {
    DRAGGING.with(Cell::get)
}

/// Invalidates and paints the popup synchronously (live slider feedback).
pub fn repaint_now() {
    let hwnd = POPUP.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|pw| pw.hwnd)));
    if let Some(hwnd) = hwnd {
        // SAFETY: hwnd is our live popup.
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
            let _ = UpdateWindow(hwnd);
        }
    }
}

fn client_dip(lp: LPARAM) -> (f32, f32) {
    let dpi = POPUP.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|p| p.renderer.dpi())).unwrap_or(96));
    let x = dpi::px_to_dip((lp.0 & 0xffff) as i16 as i32, dpi);
    let y = dpi::px_to_dip(((lp.0 >> 16) & 0xffff) as i16 as i32, dpi);
    (x, y)
}

/// Sets the body painter, called inside each frame after the background (see `paint`).
/// Receives left clicks inside the popup, in popup client coordinates.
pub fn set_click_handler(f: fn(f32, f32)) {
    CLICK.with(|c| c.set(Some(f)));
}

pub fn set_painter(f: fn(&mut Renderer, RectF)) {
    PAINTER.with(|p| p.set(Some(f)));
}

pub fn set_on_hide(f: fn()) {
    ON_HIDE.with(|c| c.set(Some(f)));
}

/// Opens the popup above (or below, if the anchor is in the top half) `anchor`, centered on it.
/// `anchor` and the result are in screen coordinates. Replaces any open popup.
pub fn show(anchor: RectF, width: f32, height: i32) -> Result<()> {
    let dpi = dpi::system_dpi();
    let width_px = dpi::dip_to_px(width, dpi);
    let height_px = dpi::dip_to_px(height as f32, dpi);
    let monitor = monitor_rect(anchor);
    let rect = popup_rect_for(anchor, width_px, height_px, dpi, monitor);

    register_class();
    let reused = POPUP.with(|p| {
        let mut state = p.borrow_mut();
        let Some(pw) = state.as_mut() else { return false };
        pw.anchor = anchor;
        pw.mouse_down = true;
        pw.esc_down = true;
        // Swapping popups: take the old one off screen first so the grab behind the new one is clean.
        // SAFETY: our own window; DwmFlush waits for the composition that removes it.
        unsafe {
            if windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(pw.hwnd).as_bool() {
                let _ = ShowWindow(pw.hwnd, SW_HIDE);
                let _ = windows::Win32::Graphics::Dwm::DwmFlush();
            }
        }
        pw.backdrop = super::glass::popup_backdrop(rect, monitor, dpi);
        pw.renderer.resize(width_px, height_px, dpi);
        unsafe {
            let _ = SetWindowPos(
                pw.hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                width_px,
                height_px,
                SWP_NOACTIVATE,
            );
        }
        apply_popup_corners(pw.hwnd);
        apply_acrylic(pw.hwnd, super::glass::PANEL_ACRYLIC_TINT);
        unsafe {
            let _ = ShowWindow(pw.hwnd, SW_SHOWNOACTIVATE);
            SetTimer(pw.hwnd, TIMER_ID, POLL_MS, None);
        }
        true
    });
    if reused {
        return Ok(());
    }

    let hwnd = unsafe {
        // SAFETY: class is registered above; all string args are static literals.
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("SmartDockPopup"),
            w!(""),
            WS_POPUP,
            rect.left,
            rect.top,
            width_px,
            height_px,
            None,
            None,
            hinstance(),
            None,
        )?
    };
    // Own the window before anything else can fail, so Drop destroys it on error.
    let pw = Popup {
        hwnd,
        renderer: Renderer::new(hwnd, width_px, height_px, dpi),
        anchor,
        mouse_down: true,
        esc_down: true,
        backdrop: super::glass::popup_backdrop(rect, monitor, dpi),
    };
    apply_popup_corners(hwnd);
    apply_acrylic(hwnd, super::glass::PANEL_ACRYLIC_TINT);
    POPUP.with(|p| *p.borrow_mut() = Some(pw));

    // SAFETY: hwnd is a live window owned by POPUP.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_ID, POLL_MS, None);
    }
    Ok(())
}

fn monitor_rect(anchor: RectF) -> RECT {
    let point = POINT {
        x: (anchor.x + anchor.w / 2.0).round() as i32,
        y: (anchor.y + anchor.h / 2.0).round() as i32,
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            return info.rcMonitor;
        }
    }
    let width = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
        windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
    ) };
    let height = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
        windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
    ) };
    RECT { left: 0, top: 0, right: width, bottom: height }
}

fn popup_rect_for(anchor: RectF, width: i32, height: i32, dpi: u32, monitor: RECT) -> RECT {
    let margin = dpi::dip_to_px(MARGIN, dpi);
    let gap = dpi::dip_to_px(GAP, dpi);
    let min_x = monitor.left + margin;
    let max_x = (monitor.right - width - margin).max(min_x);
    let x = ((anchor.x + anchor.w / 2.0).round() as i32 - width / 2).clamp(min_x, max_x);
    let below = anchor.y + anchor.h / 2.0 < (monitor.top + monitor.bottom) as f32 / 2.0;
    let preferred_y = if below {
        (anchor.y + anchor.h).round() as i32 + gap
    } else {
        anchor.y.round() as i32 - height - gap
    };
    let min_y = monitor.top + margin;
    let max_y = (monitor.bottom - height - margin).max(min_y);
    let y = preferred_y.clamp(min_y, max_y);
    RECT { left: x, top: y, right: x + width, bottom: y + height }
}

/// Asks for a repaint of the open popup (e.g. after new sparkline samples). No-op if nothing is open.
pub fn invalidate() {
    POPUP.with(|p| {
        let Ok(g) = p.try_borrow() else { return };
        if let Some(pw) = g.as_ref() {
            // SAFETY: hwnd is a live window owned by POPUP; a null rect invalidates the whole client area.
            unsafe {
                let _ = InvalidateRect(pw.hwnd, None, false);
            }
        }
    });
}

/// Hides the persistent popup window. Keeping its HWND and D2D resources alive makes the next open
/// a synchronous move/resize/show operation, matching the Electron fast path.
pub fn hide() {
    if DRAGGING.with(Cell::get) {
        // SAFETY: drops our own capture; WM_CAPTURECHANGED finishes the drag.
        unsafe {
            let _ = ReleaseCapture();
        }
    }
    POPUP.with(|p| {
        if let Some(pw) = p.borrow().as_ref() {
            unsafe {
                let _ = KillTimer(pw.hwnd, TIMER_ID);
                let _ = ShowWindow(pw.hwnd, SW_HIDE);
            }
        }
    });
    if let Some(f) = ON_HIDE.with(Cell::get) {
        f();
    }
}

fn hinstance() -> HINSTANCE {
    // SAFETY: NULL asks for the calling process's module.
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
            lpszClassName: w!("SmartDockPopup"),
            ..Default::default()
        };
        // SAFETY: `wc` and its strings outlive the call. A failure (already registered) is harmless.
        unsafe { RegisterClassExW(&wc) };
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: standard paint bracket for our own window.
            unsafe {
                BeginPaint(hwnd, &mut ps);
                paint();
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_ID => {
            poll();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = client_dip(lp);
            LAST_POS.with(|p| p.set((x, y)));
            if let Some(f) = DRAG.with(Cell::get) {
                if f(DragPhase::Down, x, y) {
                    DRAGGING.with(|d| d.set(true));
                    // SAFETY: our own window takes the capture until mouse-up.
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if DRAGGING.with(Cell::get) {
                let (x, y) = client_dip(lp);
                LAST_POS.with(|p| p.set((x, y)));
                if let Some(f) = DRAG.with(Cell::get) {
                    f(DragPhase::Move, x, y);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = client_dip(lp);
            if DRAGGING.with(|d| d.replace(false)) {
                // SAFETY: releases the capture taken on mouse-down.
                unsafe {
                    let _ = ReleaseCapture();
                }
                if let Some(f) = DRAG.with(Cell::get) {
                    f(DragPhase::Up, x, y);
                }
                return LRESULT(0);
            }
            if let Some(f) = CLICK.with(Cell::get) {
                f(x, y);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = client_dip(lp);
            if let Some(f) = RCLICK.with(Cell::get) {
                f(x, y);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Capture lost without a mouse-up (e.g. popup hidden): finish the drag where it was.
            if DRAGGING.with(|d| d.replace(false)) {
                let (x, y) = LAST_POS.with(Cell::get);
                if let Some(f) = DRAG.with(Cell::get) {
                    f(DragPhase::Up, x, y);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn paint() {
    POPUP.with(|p| {
        // Skip the frame if the state is already borrowed; the next WM_PAINT retries.
        let Ok(mut g) = p.try_borrow_mut() else { return };
        if let Some(pw) = g.as_mut() {
            let (w, h) = pw.renderer.size();
            let rect = RectF { x: 0.5, y: 0.5, w: w - 1.0, h: h - 1.0 };
            let backdrop = pw.backdrop.clone();
            let _ = pw.renderer.begin().and_then(|_| {
                match &backdrop {
                    // Same Liquid Glass as the dock: refracted wallpaper, a dim for legible text,
                    // then rims. Pieces drawn by the painter sample this backdrop too.
                    Some(bd) => {
                        let full = RectF { x: 0.0, y: 0.0, w, h };
                        pw.renderer.draw_bitmap(bd.w, bd.h, &bd.rgba, full)?;
                        super::glass::draw_panel_tint(&mut pw.renderer, rect, RADIUS)?;
                        super::glass::set_backdrop(Some(bd.clone()), w, h);
                    }
                    // Fallback over live DWM acrylic: light dim, top highlight, specular rims.
                    None => super::glass::draw_panel(&mut pw.renderer, rect, RADIUS)?,
                }
                if let Some(f) = PAINTER.with(Cell::get) {
                    f(&mut pw.renderer, rect);
                }
                super::glass::set_backdrop(None, w, h);
                pw.renderer.end()
            });
        }
    });
}

fn key_down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // SAFETY: plain key-state query.
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16) & 0x8000 != 0 }
}

/// Closes on Esc (rising edge) or on a click outside both the popup and its anchor.
/// Clicks on the anchor are left to the dock, which toggles or switches the popup.
fn poll() {
    if let Some(f) = TICK.with(Cell::get) {
        f();
    }
    let close = POPUP.with(|p| {
        let Ok(mut g) = p.try_borrow_mut() else { return false };
        let Some(pw) = g.as_mut() else { return false };

        let esc = key_down(VK_ESCAPE);
        let mouse = key_down(VK_LBUTTON) || key_down(VK_RBUTTON) || key_down(VK_MBUTTON);
        let mut close = esc && !pw.esc_down;
        if mouse && !pw.mouse_down {
            let mut pt = POINT::default();
            let mut rc = RECT::default();
            // SAFETY: out-params are valid locals.
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = GetWindowRect(pw.hwnd, &mut rc);
            }
            let (x, y) = (pt.x as f32, pt.y as f32);
            let in_popup = x >= rc.left as f32 && x < rc.right as f32 && y >= rc.top as f32 && y < rc.bottom as f32;
            let a = pw.anchor;
            let in_anchor = x >= a.x && x < a.x + a.w && y >= a.y && y < a.y + a.h;
            close = !in_popup && !in_anchor;
        }
        pw.esc_down = esc;
        pw.mouse_down = mouse;
        close
    });
    if close {
        hide();
    }
}
