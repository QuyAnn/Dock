//! Windows' own "Show hidden icons" tray overflow flyout, opened from the dock's chevron.
//!
//! The real taskbar is hidden, but its XAML tray still exposes the chevron
//! (`SystemTray.AccentButton`, AutomationId `SystemTrayIcon`, name "Show Hidden Icons").
//! In response to the user's click on the dock chevron we call UI Automation Invoke on
//! that shell button (no synthetic mouse/keyboard input), so every tray icon keeps its
//! native left/right click and menus. The flyout host window
//! (`TopLevelWindowForOverflowXamlIsland`) is then moved, without activating it, so it
//! sits just above the dock near the chevron.
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{w, VARIANT};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern, TreeScope_Descendants,
    UIA_AutomationIdPropertyId, UIA_InvokePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, IsWindowVisible, SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER,
};

/// Whether the flyout was open when the current dock click began (it closes itself on
/// the outside click, so the decision is taken at mouse-down like Start).
static OPEN_AT_DOWN: AtomicBool = AtomicBool::new(false);

fn host() -> Option<HWND> {
    // SAFETY: plain lookup by class.
    let h = unsafe { FindWindowW(w!("TopLevelWindowForOverflowXamlIsland"), None) }.ok()?;
    (!h.0.is_null()).then_some(h)
}

fn host_rect(h: HWND) -> Option<RECT> {
    let mut rc = RECT::default();
    // SAFETY: valid out pointer; a stale handle just fails.
    unsafe { GetWindowRect(h, &mut rc) }.ok()?;
    Some(rc)
}

/// The overflow flyout is currently shown.
pub fn is_open() -> bool {
    let Some(h) = host() else { return false };
    // SAFETY: plain queries on a shell window.
    let visible = unsafe { IsWindowVisible(h).as_bool() };
    if !visible {
        return false;
    }
    let mut cloaked = 0u32;
    let _ = unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, std::mem::size_of::<u32>() as u32)
    };
    cloaked == 0 && host_rect(h).is_some_and(|r| r.bottom > r.top && r.right > r.left)
}

/// Call on WM_LBUTTONDOWN on the dock.
pub fn capture_click() {
    OPEN_AT_DOWN.store(is_open(), Ordering::SeqCst);
}

/// Chevron clicked. `anchor_*` are the chevron's physical screen x range; `dock_top` is
/// the visible bar's top edge (physical px). The flyout is clamped to that monitor.
pub fn toggle(anchor_left: i32, anchor_right: i32, dock_top: i32) {
    let was_open = OPEN_AT_DOWN.swap(false, Ordering::SeqCst);
    let (mon_left, mon_right) = monitor_x_range((anchor_left + anchor_right) / 2, dock_top);
    std::thread::spawn(move || {
        let now_open = is_open();
        let t = ts();
        if was_open {
            if now_open {
                let ok = invoke_chevron();
                crate::log(&format!("{t} overflow: chevron click while open -> close via UIA Invoke ok={ok}"));
            } else {
                crate::log(&format!("{t} overflow: chevron click while open -> Windows already closed it"));
            }
            return;
        }
        let ok = invoke_chevron();
        crate::log(&format!("{t} overflow: chevron click -> open via UIA Invoke ok={ok}"));
        if !ok {
            return;
        }
        // Wait for the flyout, then park it above the dock next to the chevron.
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if is_open() {
                place(anchor_left, anchor_right, dock_top, mon_left, mon_right);
                // The shell may re-place it once its content is laid out; check again.
                std::thread::sleep(std::time::Duration::from_millis(250));
                if is_open() {
                    place(anchor_left, anchor_right, dock_top, mon_left, mon_right);
                }
                return;
            }
        }
        crate::log(&format!("{} overflow: flyout did not appear within 1 s", ts()));
    });
}

/// Pure placement: the flyout's new top-left, centred on the chevron, bottom 8 px above
/// the dock, clamped to the monitor. `None` when it already sits there.
pub fn target_pos(rc: (i32, i32, i32, i32), anchor: (i32, i32), dock_top: i32, mon: (i32, i32)) -> Option<(i32, i32)> {
    let (l, t, r, b) = rc;
    let (w, h) = (r - l, b - t);
    let cx = (anchor.0 + anchor.1) / 2;
    let x = (cx - w / 2).clamp(mon.0 + 8, (mon.1 - 8 - w).max(mon.0 + 8));
    let y = dock_top - 8 - h;
    (x != l || y != t).then_some((x, y))
}

fn place(anchor_left: i32, anchor_right: i32, dock_top: i32, mon_left: i32, mon_right: i32) {
    let Some(h) = host() else { return };
    let Some(rc) = host_rect(h) else { return };
    let cur = (rc.left, rc.top, rc.right, rc.bottom);
    match target_pos(cur, (anchor_left, anchor_right), dock_top, (mon_left, mon_right)) {
        None => crate::log(&format!("{} overflow: shown at {cur:?}, already above the dock", ts())),
        Some((x, y)) => {
            // SAFETY: moves the shell's flyout host without activating or re-ordering it.
            let moved = unsafe {
                SetWindowPos(h, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS)
            }
            .is_ok();
            crate::log(&format!("{} overflow: shown at {cur:?} -> move to ({x},{y}) ok={moved}", ts()));
        }
    }
}

/// UIA Invoke on the hidden taskbar's "Show Hidden Icons" button.
fn invoke_chevron() -> bool {
    // SAFETY: COM on this worker thread; interfaces release on drop.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else {
            crate::log("overflow: UIAutomation unavailable");
            return false;
        };
        let Some(button) = find_chevron(&uia) else {
            crate::log("overflow: 'Show Hidden Icons' button not found in Shell_TrayWnd");
            return false;
        };
        let Ok(invoke) = button.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) else {
            crate::log("overflow: chevron has no Invoke pattern");
            return false;
        };
        invoke.Invoke().is_ok()
    }
}

unsafe fn find_chevron(uia: &IUIAutomation) -> Option<IUIAutomationElement> {
    let tray = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
    let root = uia.ElementFromHandle(tray).ok()?;
    let cond = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from("SystemTrayIcon")).ok()?;
    let all = root.FindAll(TreeScope_Descendants, &cond).ok()?;
    let n = all.Length().ok()?;
    let mut leftmost: Option<(i32, IUIAutomationElement)> = None;
    for i in 0..n {
        let Ok(el) = all.GetElement(i) else { continue };
        let name = el.CurrentName().map(|b| b.to_string()).unwrap_or_default().to_lowercase();
        if name.contains("hidden") || name.contains("ẩn") {
            return Some(el);
        }
        let x = el.CurrentBoundingRectangle().map(|r| r.left).unwrap_or(i32::MAX);
        if leftmost.as_ref().map_or(true, |(lx, _)| x < *lx) {
            leftmost = Some((x, el));
        }
    }
    // Localised name not matched: the chevron is the leftmost tray button.
    crate::log("overflow: chevron name not matched; using the leftmost tray button");
    leftmost.map(|(_, el)| el)
}

fn monitor_x_range(x: i32, y: i32) -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    // SAFETY: plain queries with a sized out struct.
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            (info.rcMonitor.left, info.rcMonitor.right)
        } else {
            (x - 960, x + 960)
        }
    }
}

fn ts() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: plain query.
    let t = unsafe { GetLocalTime() };
    format!("[{:02}:{:02}:{:02}.{:03}]", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

#[cfg(test)]
mod tests {
    use super::target_pos;

    #[test]
    fn dock_layout_overflow_flyout_sits_above_dock_near_chevron() {
        // 200x150 flyout shown at the old taskbar spot, chevron at x 1600..1628, dock top 1032.
        let p = target_pos((1600, 980, 1800, 1130), (1600, 1628), 1032, (0, 1920)).unwrap();
        assert_eq!(p, (1514, 1032 - 8 - 150));
        // Near the right edge it is clamped onto the monitor.
        let p = target_pos((0, 0, 200, 150), (1900, 1916), 1032, (0, 1920)).unwrap();
        assert_eq!(p.0, 1920 - 8 - 200);
        // Already in place: no move.
        assert!(target_pos((1514, 874, 1714, 1024), (1600, 1628), 1032, (0, 1920)).is_none());
    }
}
