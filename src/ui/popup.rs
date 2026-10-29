//! The single popup HWND is reused; rendering resources live only while visible.
//! Never activates: WS_EX_NOACTIVATE, so keyboard focus stays where it was.
//! Dismissal is polled (GetAsyncKeyState / GetCursorPos) because the window never gets input focus.
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::sync::Once;
use std::time::Instant;

use super::anim::{self, SurfaceFrame, SURFACE_SECS};

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
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, WM_CAPTURECHANGED, WM_CHAR, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use super::dock::RectF;
use std::sync::Arc;

use super::liquid::LiquidBitmap;
use super::renderer::{apply_acrylic, Renderer};
use crate::win::dpi;

const GAP: f32 = 8.0;
const MARGIN: f32 = 6.0;
/// Must equal the DWM corner so the drawn panel and the window clip match exactly.
const TIMER_ID: usize = 1;
const POLL_MS: u32 = 40;

struct Popup {
    hwnd: HWND,
    renderer: Renderer,
    anchor: RectF,
    /// Screen position when fully open (before open/close slide offset).
    rest: RECT,
    /// Last polled key states. Start as "down" so the click that opened us is ignored.
    mouse_down: bool,
    esc_down: bool,
    /// Liquid Glass backdrop cut from the wallpaper at this popup's position; None until the
    /// wallpaper is decoded (plain acrylic panel then).
    backdrop: Option<Arc<LiquidBitmap>>,
    /// Control Center-only mode: body modules float over the transparent window without a panel.
    floating_modules: bool,
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
    static ESC: Cell<Option<fn() -> bool>> = const { Cell::new(None) };
    /// True between a captured mouse-down and its mouse-up.
    static DRAGGING: Cell<bool> = const { Cell::new(false) };
    static LAST_POS: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
    static CHAR: Cell<Option<fn(char)>> = const { Cell::new(None) };
    static WHEEL: Cell<Option<fn(f32, f32, f32)>> = const { Cell::new(None) };
    static CENTERED: Cell<bool> = const { Cell::new(false) };
    static HIGH: Cell<u16> = const { Cell::new(0) };
    static VIS: Cell<Option<VisAnim>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
enum VisAnim {
    In(Instant),
    Out(Instant),
}

/// Current open/close presentation (opacity, scale, slide).
pub fn presentation() -> SurfaceFrame {
    VIS.with(|v| match v.get() {
        Some(VisAnim::In(t)) => anim::surface_in(t.elapsed().as_secs_f32()),
        Some(VisAnim::Out(t)) => anim::surface_out(t.elapsed().as_secs_f32()),
        None => SurfaceFrame::REST,
    })
}

pub fn vis_animating() -> bool {
    VIS.with(|v| match v.get() {
        Some(VisAnim::In(t)) => anim::surface_in_running(t.elapsed().as_secs_f32()),
        Some(VisAnim::Out(t)) => anim::surface_out_running(t.elapsed().as_secs_f32()),
        None => false,
    })
}

/// Applies layered alpha and finishes a close animation (no per-frame D2D repaint).
pub fn tick_vis() -> bool {
    let hwnd = POPUP.with(|p| {
        p.try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|pw| pw.hwnd))
    });
    let Some(hwnd) = hwnd else {
        VIS.with(|v| v.set(None));
        return false;
    };
    if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) }.as_bool() {
        VIS.with(|v| v.set(None));
        return false;
    }
    let frame = presentation();
    let alpha = (frame.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    super::renderer::set_window_alpha(hwnd, alpha);
    if alpha == 255 {
        super::renderer::clear_window_layered(hwnd);
    }
    if let Some(VisAnim::Out(t)) = VIS.with(Cell::get) {
        if t.elapsed().as_secs_f32() >= SURFACE_SECS {
            VIS.with(|v| v.set(None));
            hide_immediate();
            return false;
        }
    }
    if let Some(VisAnim::In(t)) = VIS.with(Cell::get) {
        if t.elapsed().as_secs_f32() >= SURFACE_SECS {
            VIS.with(|v| v.set(None));
        }
    }
    vis_animating()
}

fn begin_vis_in() {
    VIS.with(|v| v.set(Some(VisAnim::In(Instant::now()))));
    super::arm_frame_timer();
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

/// Called on an Esc press before the popup closes; returning true keeps it open (the handler
/// consumed Esc, e.g. to leave Control Center edit mode).
pub fn set_esc_handler(f: fn() -> bool) {
    ESC.with(|c| c.set(Some(f)));
}

/// Anchor of the open (or last shown) popup, in screen pixels.
pub fn anchor() -> Option<RectF> {
    POPUP.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|pw| pw.anchor)))
}

/// Client size of the popup window in DIPs.
pub fn size_dip() -> Option<(f32, f32)> {
    POPUP.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|pw| pw.renderer.size())))
}

/// Tallest popup (DIPs) that fits between `anchor` and the far screen edge.
pub fn max_height_for(anchor: RectF) -> f32 {
    let dpi = dpi::system_dpi();
    let monitor = monitor_rect(anchor);
    let margin = dpi::dip_to_px(MARGIN, dpi);
    let gap = dpi::dip_to_px(GAP, dpi);
    if beside_side_dock(crate::win::edge::current(), anchor, monitor) {
        // Beside a left/right dock the popup may use the monitor's full height.
        return (monitor.bottom - monitor.top - 2 * margin).max(1) as f32 * 96.0 / dpi as f32;
    }
    let below = anchor.y + anchor.h / 2.0 < (monitor.top + monitor.bottom) as f32 / 2.0;
    let px = if below {
        monitor.bottom - margin - ((anchor.y + anchor.h).round() as i32 + gap)
    } else {
        anchor.y.round() as i32 - gap - (monitor.top + margin)
    };
    px.max(1) as f32 * 96.0 / dpi as f32
}

/// Receives right clicks (button up) inside the popup, in client DIPs.
pub fn set_rclick_handler(f: fn(f32, f32)) {
    RCLICK.with(|c| c.set(Some(f)));
}

/// Receives typed characters (WM_CHAR) while the popup holds keyboard focus.
pub fn set_char_handler(f: fn(char)) {
    CHAR.with(|c| c.set(Some(f)));
}

/// Receives mouse-wheel notches over the popup: (x, y) in client DIPs, positive = wheel up.
pub fn set_wheel_handler(f: fn(f32, f32, f32)) {
    WHEEL.with(|c| c.set(Some(f)));
}

/// Gives the popup keyboard focus (it never takes it on click). Used by text inputs.
pub fn focus() {
    let hwnd = POPUP.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|pw| pw.hwnd)));
    if let Some(hwnd) = hwnd {
        // SAFETY: hwnd is our live popup; the dock process owns the foreground when the user clicks it.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
        }
    }
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

/// Opens the popup centred on the primary monitor (the widget gallery).
pub fn show_centered(width: f32, height: i32) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    // SAFETY: plain metric queries.
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    CENTERED.with(|c| c.set(true));
    show(RectF { x: sw as f32 / 2.0, y: sh as f32 / 2.0, w: 0.0, h: 0.0 }, width, height)
}

/// Opens the popup above (or below, if the anchor is in the top half) `anchor`, centered on it.
/// `anchor` and the result are in screen coordinates. Replaces any open popup.
pub fn show(anchor: RectF, width: f32, height: i32) -> Result<()> {
    show_impl(anchor, width, height, false)
}

/// Opens the shared popup window without its usual outer panel. The body painter can still sample
/// the one cached popup backdrop, allowing independent glass modules without per-frame captures.
pub fn show_floating_modules(anchor: RectF, width: f32, height: i32) -> Result<()> {
    show_impl(anchor, width, height, true)
}

fn show_impl(anchor: RectF, width: f32, height: i32, floating_modules: bool) -> Result<()> {
    // Popups follow the system appearance: smoky dark or milky light glass.
    super::glass::set_control_dark(crate::win::system::dark_mode());
    let dpi = dpi::system_dpi();
    let width_px = dpi::dip_to_px(width, dpi);
    let height_px = dpi::dip_to_px(height as f32, dpi);
    let monitor = monitor_rect(anchor);
    let mut rect = popup_rect_for(anchor, width_px, height_px, dpi, monitor);
    if CENTERED.with(|c| c.replace(false)) {
        let (cx, cy) = ((monitor.left + monitor.right) / 2, (monitor.top + monitor.bottom) / 2);
        rect = RECT { left: cx - width_px / 2, top: cy - height_px / 2, right: cx + (width_px + 1) / 2, bottom: cy + (height_px + 1) / 2 };
    }

    register_class();
    let reused = POPUP.with(|p| {
        let mut state = p.borrow_mut();
        let Some(pw) = state.as_mut() else { return false };
        pw.anchor = anchor;
        pw.rest = rect;
        pw.floating_modules = floating_modules;
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
        VIS.with(|v| v.set(None));
        pw.renderer.release();
        if let Some(bg) = pw.backdrop.take() { super::glass::release_backdrop(&bg); }
        pw.backdrop = super::glass::popup_backdrop_for(rect, monitor, dpi, true);
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
        style_window(pw.hwnd);
        unsafe {
            let _ = ShowWindow(pw.hwnd, SW_SHOWNOACTIVATE);
            SetTimer(pw.hwnd, TIMER_ID, POLL_MS, None);
        }
        begin_vis_in();
        super::renderer::set_window_alpha(pw.hwnd, 0);
        unsafe {
            let _ = InvalidateRect(pw.hwnd, None, false);
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
        rest: rect,
        mouse_down: true,
        esc_down: true,
        backdrop: super::glass::popup_backdrop_for(rect, monitor, dpi, true),
        floating_modules,
    };
    style_window(hwnd);
    POPUP.with(|p| *p.borrow_mut() = Some(pw));

    // SAFETY: hwnd is a live window owned by POPUP.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_ID, POLL_MS, None);
    }
    begin_vis_in();
    super::renderer::set_window_alpha(hwnd, 0);
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
    Ok(())
}

/// Frame style of the popup window.
fn style_window(hwnd: HWND) {
    // Fully transparent window: the panel (frosted backdrop, rounded corners, rims) is drawn by
    // `paint`, so its corners can have any radius and stay anti-aliased.
    apply_acrylic(hwnd, 0);
    super::renderer::apply_dwm_transparency(hwnd);
    super::renderer::apply_dock_no_chrome(hwnd);
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
    popup_rect_on(crate::win::edge::current(), anchor, width, height, dpi, monitor)
}

/// True when `anchor` sits on the dock of a left/right `edge` (its centre within the outer
/// quarter of the monitor on that side): such popups open beside the dock.
fn beside_side_dock(edge: crate::win::edge::AbEdge, anchor: RectF, monitor: RECT) -> bool {
    use crate::win::edge::AbEdge;
    let cx = anchor.x + anchor.w / 2.0;
    let quarter = (monitor.right - monitor.left) as f32 / 4.0;
    match edge {
        AbEdge::Left => cx < monitor.left as f32 + quarter,
        AbEdge::Right => cx > monitor.right as f32 - quarter,
        _ => false,
    }
}

/// Popup window rect (physical px) for a dock on `edge`: above a bottom dock / below a top dock
/// (the anchor's half of the monitor decides, as always), beside a left or right dock, centred
/// on the anchor along the dock and kept `MARGIN` inside the monitor.
pub(crate) fn popup_rect_on(edge: crate::win::edge::AbEdge, anchor: RectF, width: i32, height: i32, dpi: u32, monitor: RECT) -> RECT {
    use crate::win::edge::AbEdge;
    let margin = dpi::dip_to_px(MARGIN, dpi);
    let gap = dpi::dip_to_px(GAP, dpi);
    let min_x = monitor.left + margin;
    let max_x = (monitor.right - width - margin).max(min_x);
    if beside_side_dock(edge, anchor, monitor) {
        let x = if edge == AbEdge::Left {
            (anchor.x + anchor.w).round() as i32 + gap
        } else {
            anchor.x.round() as i32 - width - gap
        }
        .clamp(min_x, max_x);
        let min_y = monitor.top + margin;
        let max_y = (monitor.bottom - height - margin).max(min_y);
        let y = ((anchor.y + anchor.h / 2.0).round() as i32 - height / 2).clamp(min_y, max_y);
        return RECT { left: x, top: y, right: x + width, bottom: y + height };
    }
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

/// Keep the HWND for reuse; release its surface, GPU bitmaps and CPU backdrop on hide.
pub fn hide() {
    if vis_animating() {
        if VIS.with(|v| matches!(v.get(), Some(VisAnim::Out(_)))) {
            return;
        }
    }
    if POPUP.with(|p| p.try_borrow().ok().is_some_and(|g| g.is_some())) {
        VIS.with(|v| v.set(Some(VisAnim::Out(Instant::now()))));
        super::arm_frame_timer();
        invalidate();
        return;
    }
    hide_immediate();
}

fn hide_immediate() {
    if DRAGGING.with(Cell::get) {
        // SAFETY: drops our own capture; WM_CAPTURECHANGED finishes the drag.
        unsafe {
            let _ = ReleaseCapture();
        }
    }
    POPUP.with(|p| {
        if let Some(pw) = p.borrow_mut().as_mut() {
            unsafe {
                let _ = KillTimer(pw.hwnd, TIMER_ID);
                let _ = ShowWindow(pw.hwnd, SW_HIDE);
            }
            super::renderer::clear_window_layered(pw.hwnd);
            pw.renderer.release();
            if let Some(bg) = pw.backdrop.take() { super::glass::release_backdrop(&bg); }
        }
    });
    VIS.with(|v| v.set(None));
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
            let _ = tick_vis();
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
        WM_MOUSEWHEEL => {
            // The wheel message carries screen coordinates; convert to popup client DIPs.
            let mut pt = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            let mut rc = RECT::default();
            // SAFETY: valid out-param; hwnd is our popup.
            unsafe {
                let _ = GetWindowRect(hwnd, &mut rc);
            }
            pt.x -= rc.left;
            pt.y -= rc.top;
            let dpi = POPUP.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|p| p.renderer.dpi())).unwrap_or(96));
            let notches = ((wp.0 >> 16) as i16) as f32 / 120.0;
            if let Some(f) = WHEEL.with(Cell::get) {
                f(dpi::px_to_dip(pt.x, dpi), dpi::px_to_dip(pt.y, dpi), notches);
            }
            LRESULT(0)
        }
        WM_CHAR => {
            let unit = wp.0 as u16;
            let ch = if (0xD800..0xDC00).contains(&unit) {
                HIGH.with(|h| h.set(unit));
                None
            } else if (0xDC00..0xE000).contains(&unit) {
                let hi = HIGH.with(|h| h.replace(0));
                char::decode_utf16([hi, unit]).next().and_then(|r| r.ok())
            } else {
                char::from_u32(unit as u32)
            };
            if let (Some(ch), Some(f)) = (ch, CHAR.with(Cell::get)) {
                f(ch);
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

/// Every non-Control-Center popup panel in the Control Center's clear glass: the CC panel layer
/// (light DPI-correct detail blur, local adaptive smoke, thin lens rim) instead of the milky
/// popup frost, white text with the brightness-adaptive halo, and dark ink only over a
/// near-white page (the CC rule, `update_control_ink`). `body` paints the content at `rect`.
/// Needs the popup backdrop set (`glass::set_backdrop`) and `glass::set_control_dark`.
pub(crate) fn paint_clear_glass(
    r: &mut super::renderer::Renderer,
    rect: RectF,
    radius: f32,
    body: impl FnOnce(&mut super::renderer::Renderer, RectF),
) -> windows::core::Result<()> {
    use super::{glass, renderer};
    glass::update_control_ink(rect);
    glass::set_cc_clear(true);
    renderer::set_halo_sampler(Some(glass::local_brightness));
    let ink = glass::control_ink_dark();
    // Over a near-white page the clear panel would let the page's own text compete with the
    // popup's (the Control Center has its module cards for that): a light white veil, as macOS
    // does on bright pages. Darker backdrops keep the pure clear glass and its adaptive smoke.
    let res = glass::draw_control_panel(r, rect, radius).and_then(|_| {
        if ink {
            let rr = radius.min(rect.w / 2.0).min(rect.h / 2.0);
            r.fill_rounded_rect(rect, rr, 0xFFFF_FF5C)
        } else {
            Ok(())
        }
    });
    renderer::set_light_ink(ink);
    renderer::set_text_shadow(!ink);
    // Heavier popup type (the Control Center sets its own weights).
    renderer::set_popup_text(true);
    if res.is_ok() {
        body(r, rect);
    }
    renderer::set_popup_text(false);
    renderer::set_text_shadow(false);
    renderer::set_light_ink(false);
    renderer::set_halo_sampler(None);
    glass::set_cc_clear(false);
    res
}

fn paint() {
    POPUP.with(|p| {
        // Skip the frame if the state is already borrowed; the next WM_PAINT retries.
        let Ok(mut g) = p.try_borrow_mut() else { return };
        if let Some(pw) = g.as_mut() {
            // A queued invalidation after hide must not recreate the released target.
            if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(pw.hwnd) }.as_bool() { return; }
            let (w, h) = pw.renderer.size();
            let rect = RectF { x: 0.5, y: 0.5, w: w - 1.0, h: h - 1.0 };
            let backdrop = pw.backdrop.clone();
            let res = pw.renderer.begin().and_then(|_| {
                // Apple-style continuous-looking corners: the window is fully transparent and the
                // panel is drawn (anti-aliased) with its own radius.
                let radius = if super::glass::panel_light() { 24.0 } else { 18.0 };
                // Every popup shares the Control Center glass (captured once per open).
                super::glass::set_backdrop(backdrop.clone(), w, h);
                let painter = PAINTER.with(Cell::get);
                let res = if pw.floating_modules {
                    // The Control Center draws its own panel, ink and halo.
                    if let Some(f) = painter {
                        f(&mut pw.renderer, rect);
                    }
                    Ok(())
                } else {
                    paint_clear_glass(&mut pw.renderer, rect, radius, |r, rc| {
                        if let Some(f) = painter {
                            f(r, rc);
                        }
                    })
                };
                super::glass::set_backdrop(None, w, h);
                res?;
                pw.renderer.end()
            });
            if let Err(e) = res {
                crate::log(&format!("popup paint failed: {e}"));
            }
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
        let Ok(mut g) = p.try_borrow_mut() else { return None };
        let Some(pw) = g.as_mut() else { return None };

        let esc = key_down(VK_ESCAPE);
        let mouse = key_down(VK_LBUTTON) || key_down(VK_RBUTTON) || key_down(VK_MBUTTON);
        let esc_edge = esc && !pw.esc_down;
        let mut close = esc_edge;
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
        if esc_edge && close { Some(true) } else if close { Some(false) } else { None }
    });
    // Esc first goes to the handler (outside the POPUP borrow: it may resize the popup).
    let close = match close {
        Some(true) => !ESC.with(Cell::get).is_some_and(|f| f()),
        Some(false) => true,
        None => false,
    };
    if close {
        hide();
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    #[test]
    fn hiding_discards_resources_and_hidden_paints_do_not_recreate_them() {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let hwnd = unsafe { CreateWindowExW(
            WS_EX_TOOLWINDOW, w!("STATIC"), w!("Popup hide test"), WS_POPUP,
            0, 0, 100, 100, None, None, None, None,
        ) }.unwrap();
        let bg = Arc::new(LiquidBitmap { w: 2, h: 2, rgba: vec![255; 16], plain: Vec::new() });
        let mut renderer = Renderer::new(hwnd, 100, 100, 96);
        renderer.begin().unwrap();
        super::super::glass::set_backdrop(Some(bg.clone()), 100.0, 100.0);
        super::super::glass::draw_panel_glass(&mut renderer, RectF { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, 12.0).unwrap();
        renderer.end().unwrap();
        POPUP.with(|p| *p.borrow_mut() = Some(Popup {
            hwnd,
            renderer,
            anchor: RectF::default(),
            rest: RECT::default(),
            mouse_down: false,
            esc_down: false,
            backdrop: Some(bg.clone()),
            floating_modules: false,
        }));
        hide();
        while vis_animating() {
            tick_vis();
        }
        paint();
        POPUP.with(|p| {
            let p = p.borrow();
            let pw = p.as_ref().unwrap();
            assert!(pw.renderer.is_released());
            assert!(pw.backdrop.is_none());
        });
        assert_eq!(Arc::strong_count(&bg), 1);
        POPUP.with(|p| { p.borrow_mut().take(); });
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use crate::win::edge::AbEdge;

    const MON: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };

    #[test]
    fn dock_edge_popup_opens_toward_the_screen() {
        // Bottom dock: above the anchor; top dock: below it (unchanged rule).
        let a = RectF { x: 900.0, y: 1028.0, w: 40.0, h: 52.0 };
        let r = popup_rect_on(AbEdge::Bottom, a, 300, 200, 96, MON);
        assert!(r.bottom <= 1028 && r.left == 770);
        let a = RectF { x: 900.0, y: 0.0, w: 40.0, h: 52.0 };
        let r = popup_rect_on(AbEdge::Top, a, 300, 200, 96, MON);
        assert!(r.top >= 52 && r.left == 770);
        // Left dock: right of the dock, centred on the anchor vertically.
        let a = RectF { x: 0.0, y: 500.0, w: 52.0, h: 40.0 };
        let r = popup_rect_on(AbEdge::Left, a, 300, 200, 96, MON);
        assert!(r.left >= 52 && r.top == 420, "{r:?}");
        // Right dock: left of the dock, clamped inside the monitor near the bottom.
        let a = RectF { x: 1868.0, y: 1000.0, w: 52.0, h: 40.0 };
        let r = popup_rect_on(AbEdge::Right, a, 300, 200, 96, MON);
        assert!(r.right <= 1868 && r.bottom <= 1080, "{r:?}");
        // An anchor away from the side dock falls back to the above/below rule.
        let a = RectF { x: 900.0, y: 100.0, w: 40.0, h: 40.0 };
        let r = popup_rect_on(AbEdge::Left, a, 300, 200, 96, MON);
        assert!(r.top >= 140);
    }
}
