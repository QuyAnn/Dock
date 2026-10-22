//! Monitor topology helpers.
//!
//! Selection of which displays get a *secondary* dock is pure (no Win32) so it can be
//! unit-tested with the real dual-monitor layout from the field.
use windows::core::Result;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};

/// Full `rcMonitor` in physical pixels. Never confuse this with an AppBar dock strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorBounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl MonitorBounds {
    pub const fn from_rect(r: RECT) -> Self {
        Self { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
    }

    pub const fn to_rect(self) -> RECT {
        RECT { left: self.left, top: self.top, right: self.right, bottom: self.bottom }
    }

    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// Monitors that should host a secondary dock.
///
/// `primary` **must** be the primary display's full `rcMonitor`. Passing the dock AppBar
/// strip (e.g. `(0,1034,1920,1080)`) fails to match `(0,0,1920,1080)` and incorrectly
/// schedules a second dock on the primary display — the 66ecd48 regression.
pub fn secondary_monitor_bounds(
    all: &[MonitorBounds],
    primary: MonitorBounds,
) -> Vec<MonitorBounds> {
    all.iter().copied().filter(|m| *m != primary).collect()
}

/// `rcMonitor` for the monitor that contains `hwnd` (nearest if the window spans displays).
pub fn bounds_for_window(hwnd: HWND) -> Result<MonitorBounds> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi lives for the call; hwnd is a live top-level window.
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return primary_bounds();
        }
    }
    Ok(MonitorBounds::from_rect(mi.rcMonitor))
}

/// Primary display `rcMonitor` (monitor containing the virtual desktop origin).
pub fn primary_bounds() -> Result<MonitorBounds> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            let (w, h) = (
                windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
                ),
                windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
                ),
            );
            return Ok(MonitorBounds { left: 0, top: 0, right: w, bottom: h });
        }
    }
    Ok(MonitorBounds::from_rect(mi.rcMonitor))
}

/// Every attached monitor's `rcMonitor`, primary first.
pub fn enum_bounds() -> Vec<MonitorBounds> {
    unsafe extern "system" fn callback(
        mon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        data: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        let out = &mut *(data.0 as *mut Vec<MonitorBounds>);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            out.push(MonitorBounds::from_rect(mi.rcMonitor));
        }
        windows::Win32::Foundation::BOOL(1)
    }
    let mut out: Vec<MonitorBounds> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(callback),
            windows::Win32::Foundation::LPARAM(&mut out as *mut _ as isize),
        );
    }
    if out.is_empty() {
        if let Ok(primary) = primary_bounds() {
            out.push(primary);
        }
    } else if let Ok(primary) = primary_bounds() {
        out.sort_by_key(|rc| *rc != primary);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field layout from the dual-monitor regression report.
    fn home_dual() -> (MonitorBounds, MonitorBounds) {
        let primary = MonitorBounds { left: 0, top: 0, right: 1920, bottom: 1080 };
        let secondary = MonitorBounds { left: -1920, top: -108, right: 0, bottom: 972 };
        (primary, secondary)
    }

    #[test]
    fn secondary_selection_skips_primary_rc_monitor() {
        let (primary, secondary) = home_dual();
        let all = [primary, secondary];
        assert_eq!(secondary_monitor_bounds(&all, primary), vec![secondary]);
    }

    #[test]
    fn dock_appbar_strip_must_not_be_used_as_primary() {
        // 66ecd48 passed the dock strip (0,1034,1920,1080) into build_secondary_docks.
        // That does not equal rcMonitor (0,0,1920,1080), so primary was not skipped and a
        // blank "secondary" dock covered the real primary dock.
        let (primary_monitor, secondary) = home_dual();
        let dock_strip = MonitorBounds { left: 0, top: 1034, right: 1920, bottom: 1080 };
        let all = [primary_monitor, secondary];

        let wrong = secondary_monitor_bounds(&all, dock_strip);
        assert_eq!(
            wrong.len(),
            2,
            "dock strip as primary key incorrectly keeps both monitors (incl. real primary)"
        );
        assert!(wrong.contains(&primary_monitor));

        let right = secondary_monitor_bounds(&all, primary_monitor);
        assert_eq!(right, vec![secondary]);
        assert!(!right.contains(&primary_monitor));
    }

    #[test]
    fn single_monitor_yields_no_secondaries() {
        let primary = MonitorBounds { left: 0, top: 0, right: 1920, bottom: 1080 };
        assert!(secondary_monitor_bounds(&[primary], primary).is_empty());
    }

    #[test]
    fn order_of_enumeration_does_not_matter() {
        let (primary, secondary) = home_dual();
        let all = [secondary, primary];
        assert_eq!(secondary_monitor_bounds(&all, primary), vec![secondary]);
    }
}
