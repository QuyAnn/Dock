//! The single popup window. Created on `show`, destroyed on `hide` (or when dismissed).
//! Never activates: WS_EX_NOACTIVATE, so keyboard focus stays where it was.
//! Dismissal is polled (GetAsyncKeyState / GetCursorPos) because the window never gets input focus.
use std::cell::RefCell;
use std::mem::size_of;
use std::sync::Once;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics, GetWindowRect,
    KillTimer, RegisterClassExW, SetTimer, ShowWindow, SM_CXSCREEN, SM_CYSCREEN,
    SW_SHOWNOACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use super::dock::RectF;
use super::renderer::{apply_acrylic, Renderer};

const POPUP_W: i32 = 320;
const GAP: f32 = 8.0;
const MARGIN: i32 = 6;
const RADIUS: f32 = 8.0;
const TIMER_ID: usize = 1;
const POLL_MS: u32 = 100;

struct Popup {
    hwnd: HWND,
    renderer: Renderer,
    anchor: RectF,
    /// Last polled key states. Start as "down" so the click that opened us is ignored.
    mouse_down: bool,
    esc_down: bool,
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
}

/// Opens the popup above (or below, if the anchor is in the top half) `anchor`, centered on it.
/// `anchor` and the result are in screen coordinates. Replaces any open popup.
pub fn show(anchor: RectF, height: i32) -> Result<()> {
    hide();

    // SAFETY: plain metric queries.
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let cx = (anchor.x + anchor.w / 2.0) as i32;
    let x = (cx - POPUP_W / 2).clamp(MARGIN, (sw - POPUP_W - MARGIN).max(MARGIN));
    let below = anchor.y + anchor.h / 2.0 < sh as f32 / 2.0;
    let y = (if below {
        anchor.y + anchor.h + GAP
    } else {
        anchor.y - height as f32 - GAP
    }) as i32;

    register_class();
    let hwnd = unsafe {
        // SAFETY: class is registered above; all string args are static literals.
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("SmartDockPopup"),
            w!(""),
            WS_POPUP,
            x,
            y,
            POPUP_W,
            height,
            None,
            None,
            hinstance(),
            None,
        )?
    };
    // Own the window before anything else can fail, so Drop destroys it on error.
    let pw = Popup {
        hwnd,
        renderer: Renderer::new(hwnd, POPUP_W, height),
        anchor,
        mouse_down: true,
        esc_down: true,
    };
    apply_acrylic(hwnd, 0x9E1E_1F24);
    POPUP.with(|p| *p.borrow_mut() = Some(pw));

    // SAFETY: hwnd is a live window owned by POPUP.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_ID, POLL_MS, None);
    }
    Ok(())
}

/// Destroys the popup and its D2D resources. No-op if nothing is open.
pub fn hide() {
    // Take it out first so Drop (which sends WM_DESTROY) never runs under a RefCell borrow.
    let old = POPUP.with(|p| p.borrow_mut().take());
    drop(old);
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
            let _ = pw.renderer.begin().and_then(|_| {
                pw.renderer.fill_rounded_rect(rect, RADIUS, 0x1E1F_249E)?;
                pw.renderer.stroke_rounded_rect(rect, RADIUS, 0xFFFF_FF21, 1.0)?;
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
