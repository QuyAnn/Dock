//! Per-monitor DPI setup and conversions between Direct2D DIPs and Win32 physical pixels.
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

pub const BASE_DPI: u32 = 96;

/// Opts the process into crisp per-monitor-v2 rendering. Must run before any HWND is created.
pub fn enable_per_monitor_v2() {
    // A manifest or the host may already have selected an awareness mode; failure is harmless then.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn system_dpi() -> u32 {
    unsafe { GetDpiForSystem() }.max(BASE_DPI)
}

pub fn window_dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }.max(BASE_DPI)
}

pub fn scale(dpi: u32) -> f32 {
    dpi.max(1) as f32 / BASE_DPI as f32
}

pub fn dip_to_px(value: f32, dpi: u32) -> i32 {
    (value * scale(dpi)).round() as i32
}

pub fn px_to_dip(value: i32, dpi: u32) -> f32 {
    value as f32 / scale(dpi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_common_windows_scales() {
        assert_eq!(dip_to_px(56.0, 96), 56);
        assert_eq!(dip_to_px(56.0, 120), 70);
        assert_eq!(dip_to_px(56.0, 144), 84);
        assert_eq!(px_to_dip(84, 144), 56.0);
    }
}
