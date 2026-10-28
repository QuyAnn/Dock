//! Which screen edge the dock sits on: follows the Windows taskbar position (TaskbarLocation / StuckRects3) unless
//! settings.json `dockEdge` pins one. One process-wide value (`current`), read by layout, drawing,
//! popup placement and the wallpaper thread. Change detection runs on the UI thread only
//! (WM_SETTINGCHANGE, TaskbarCreated, and a registry-change event waited on by the message loop);
//! there is no polling thread.
use std::sync::atomic::{AtomicU8, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HANDLE, RECT};
use windows::Win32::System::Registry::{
    RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
    REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_THREAD_AGNOSTIC, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::Shell::{SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETTASKBARPOS, APPBARDATA};

pub use super::appbar::AbEdge;

const STUCK_RECTS: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StuckRects3");
/// Windows 11 (26xxx) Settings > Taskbar behaviors > Taskbar position writes DWORD
/// `TaskbarLocation` here (ABE_* order: 0 left, 1 top, 2 right, 3 bottom); StuckRects3 is no
/// longer updated by that page.
const EXPLORER_ADVANCED: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced");

/// ABE_* value of the current edge (bottom until told otherwise).
static CURRENT: AtomicU8 = AtomicU8::new(ABE_BOTTOM as u8);

impl AbEdge {
    pub fn vertical(self) -> bool {
        matches!(self, AbEdge::Left | AbEdge::Right)
    }

    /// ABE_* code (0 left, 1 top, 2 right, 3 bottom).
    pub fn abe(self) -> u32 {
        match self {
            AbEdge::Left => ABE_LEFT,
            AbEdge::Top => ABE_TOP,
            AbEdge::Right => ABE_RIGHT,
            AbEdge::Bottom => ABE_BOTTOM,
        }
    }

    pub fn from_abe(code: u32) -> Option<AbEdge> {
        match code {
            0 => Some(AbEdge::Left),
            1 => Some(AbEdge::Top),
            2 => Some(AbEdge::Right),
            3 => Some(AbEdge::Bottom),
            _ => None,
        }
    }

    /// `top|bottom|left|right` (settings.json `dockEdge`, SMARTDOCK_DOCK_EDGE). `auto` and
    /// anything else give `None`.
    pub fn parse(name: &str) -> Option<AbEdge> {
        match name.trim().to_ascii_lowercase().as_str() {
            "left" => Some(AbEdge::Left),
            "top" => Some(AbEdge::Top),
            "right" => Some(AbEdge::Right),
            "bottom" => Some(AbEdge::Bottom),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AbEdge::Left => "left",
            AbEdge::Top => "top",
            AbEdge::Right => "right",
            AbEdge::Bottom => "bottom",
        }
    }
}

pub fn current() -> AbEdge {
    AbEdge::from_abe(CURRENT.load(Ordering::Relaxed) as u32).unwrap_or(AbEdge::Bottom)
}

pub fn set_current(edge: AbEdge) {
    CURRENT.store(edge.abe() as u8, Ordering::Relaxed);
}

/// Edge stored in the StuckRects3 `Settings` blob: byte 12 holds the ABE_* code.
pub fn parse_stuck_rects(blob: &[u8]) -> Option<AbEdge> {
    if blob.len() < 16 {
        return None;
    }
    AbEdge::from_abe(blob[12] as u32)
}

fn registry_edge() -> Option<AbEdge> {
    let mut buf = [0u8; 64];
    let mut len = buf.len() as u32;
    // SAFETY: buf/len describe a valid writable buffer for the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            STUCK_RECTS,
            w!("Settings"),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if status.is_err() {
        return None;
    }
    parse_stuck_rects(&buf[..(len as usize).min(buf.len())])
}

/// Edge for an Explorer\Advanced `TaskbarLocation` DWORD (ABE_* code; anything else ignored).
pub fn parse_taskbar_location(value: u32) -> Option<AbEdge> {
    AbEdge::from_abe(value)
}

fn location_edge() -> Option<AbEdge> {
    let mut value = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: value/len describe a valid writable DWORD for the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            EXPLORER_ADVANCED,
            w!("TaskbarLocation"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut len),
        )
    };
    if status.is_err() {
        return None;
    }
    parse_taskbar_location(value)
}

/// Taskbar edge from the sources in priority order; the first that answers wins.
pub fn pick_edge(location: Option<AbEdge>, stuck: Option<AbEdge>, appbar: impl FnOnce() -> Option<AbEdge>) -> AbEdge {
    location.or(stuck).or_else(appbar).unwrap_or(AbEdge::Bottom)
}

fn appbar_edge() -> Option<AbEdge> {
    let mut data = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    // SAFETY: data is a valid APPBARDATA for the call.
    if unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) } == 0 {
        return None;
    }
    AbEdge::from_abe(data.uEdge)
}

/// Where the Windows taskbar is configured: Explorer\Advanced `TaskbarLocation`, then
/// StuckRects3, then ABM_GETTASKBARPOS, then bottom.
pub fn taskbar_edge() -> AbEdge {
    pick_edge(location_edge(), registry_edge(), appbar_edge)
}

/// The dock edge for a `dockEdge` setting: a fixed edge, or the taskbar's for `auto`.
/// SMARTDOCK_DOCK_EDGE overrides both (renders and testing).
pub fn resolve(setting: &str) -> AbEdge {
    if let Some(edge) = std::env::var("SMARTDOCK_DOCK_EDGE").ok().as_deref().and_then(AbEdge::parse) {
        return edge;
    }
    AbEdge::parse(setting).unwrap_or_else(taskbar_edge)
}

/// Window/popup placement beside a dock anchor (physical px): the popup of `size` goes on the
/// screen side of the anchor (above a bottom dock, below a top dock, right of a left dock...)
/// with `gap` px between, centred on the anchor along the dock and kept inside `bounds`.
pub fn place_beside(edge: AbEdge, anchor: RECT, size: (i32, i32), gap: i32, bounds: RECT) -> (i32, i32) {
    let (w, h) = size;
    let clamp = |v: i32, lo: i32, hi: i32| if hi < lo { lo } else { v.clamp(lo, hi) };
    match edge {
        AbEdge::Bottom | AbEdge::Top => {
            let cx = (anchor.left + anchor.right) / 2;
            let x = clamp(cx - w / 2, bounds.left, bounds.right - w);
            let y = if edge == AbEdge::Bottom { anchor.top - gap - h } else { anchor.bottom + gap };
            (x, y)
        }
        AbEdge::Left | AbEdge::Right => {
            let cy = (anchor.top + anchor.bottom) / 2;
            let y = clamp(cy - h / 2, bounds.top, bounds.bottom - h);
            let x = if edge == AbEdge::Left { anchor.right + gap } else { anchor.left - gap - w };
            (x, y)
        }
    }
}

// Declared here so the `windows` crate does not need its Win32_Security feature for one call.
#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(attrs: *const core::ffi::c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> isize;
}

/// Registry watch on StuckRects3 and Explorer\Advanced: one auto-reset event (shared by both
/// keys) the message loop waits on next to its messages. Re-armed after each signal
/// (RegNotifyChangeKeyValue fires once per arm).
pub struct TaskbarWatch {
    keys: Vec<HKEY>,
    event: HANDLE,
}

impl TaskbarWatch {
    pub fn new() -> Option<TaskbarWatch> {
        let mut keys = Vec::new();
        for path in [EXPLORER_ADVANCED, STUCK_RECTS] {
            let mut key = HKEY::default();
            // SAFETY: opening an existing HKCU key for notifications only.
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path, 0, KEY_NOTIFY, &mut key) }.is_ok() {
                keys.push(key);
            }
        }
        if keys.is_empty() {
            return None;
        }
        // SAFETY: no security attributes, auto-reset, initially unsignalled, unnamed.
        let raw = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        let event = HANDLE(raw as _);
        let Some(event) = (!event.is_invalid() && raw != 0).then_some(event) else {
            // SAFETY: keys were opened above.
            for key in keys {
                unsafe {
                    let _ = RegCloseKey(key);
                }
            }
            return None;
        };
        let watch = TaskbarWatch { keys, event };
        watch.arm().then_some(watch)
    }

    pub fn event(&self) -> HANDLE {
        self.event
    }

    /// Asks for the next change of the keys' values. True if at least one key is armed.
    pub fn arm(&self) -> bool {
        let mut any = false;
        for &key in &self.keys {
            // SAFETY: keys and event stay valid while self lives.
            any |= unsafe {
                RegNotifyChangeKeyValue(key, false, REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC, self.event, true)
            }
            .is_ok();
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(edge: u8) -> Vec<u8> {
        // Real Windows 11 StuckRects3 value (48 bytes) with the edge byte replaced.
        let mut b = vec![
            0x30, 0x00, 0x00, 0x00, 0xfe, 0xff, 0xff, 0xff, 0x03, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
            0x30, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x04, 0x00, 0x00,
            0x80, 0x07, 0x00, 0x00, 0x38, 0x04, 0x00, 0x00, 0x60, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        ];
        b[12] = edge;
        b
    }

    #[test]
    fn dock_edge_parses_stuck_rects_byte_12() {
        assert_eq!(parse_stuck_rects(&blob(0)), Some(AbEdge::Left));
        assert_eq!(parse_stuck_rects(&blob(1)), Some(AbEdge::Top));
        assert_eq!(parse_stuck_rects(&blob(2)), Some(AbEdge::Right));
        assert_eq!(parse_stuck_rects(&blob(3)), Some(AbEdge::Bottom));
        assert_eq!(parse_stuck_rects(&blob(7)), None);
        assert_eq!(parse_stuck_rects(&blob(3)[..12]), None);
        assert_eq!(parse_stuck_rects(&[]), None);
    }

    #[test]
    fn dock_edge_prefers_taskbar_location_over_stale_stuck_rects() {
        // Windows 11 26300: Settings says Right (TaskbarLocation = 2) while StuckRects3 still says bottom.
        assert_eq!(parse_taskbar_location(0), Some(AbEdge::Left));
        assert_eq!(parse_taskbar_location(1), Some(AbEdge::Top));
        assert_eq!(parse_taskbar_location(2), Some(AbEdge::Right));
        assert_eq!(parse_taskbar_location(3), Some(AbEdge::Bottom));
        assert_eq!(parse_taskbar_location(4), None);
        let stale = parse_stuck_rects(&blob(3));
        assert_eq!(pick_edge(parse_taskbar_location(2), stale, || Some(AbEdge::Bottom)), AbEdge::Right);
        // Value missing or out of range: StuckRects3, then the AppBar query, then bottom.
        assert_eq!(pick_edge(parse_taskbar_location(7), parse_stuck_rects(&blob(1)), || None), AbEdge::Top);
        assert_eq!(pick_edge(None, None, || Some(AbEdge::Left)), AbEdge::Left);
        assert_eq!(pick_edge(None, None, || None), AbEdge::Bottom);
    }

    #[test]
    fn dock_edge_names_round_trip() {
        for e in [AbEdge::Left, AbEdge::Top, AbEdge::Right, AbEdge::Bottom] {
            assert_eq!(AbEdge::parse(e.name()), Some(e));
            assert_eq!(AbEdge::from_abe(e.abe()), Some(e));
        }
        assert_eq!(AbEdge::parse("auto"), None);
        assert_eq!(AbEdge::parse(" Left "), Some(AbEdge::Left));
        assert!(AbEdge::Left.vertical() && AbEdge::Right.vertical());
        assert!(!AbEdge::Top.vertical() && !AbEdge::Bottom.vertical());
    }

    #[test]
    fn dock_edge_places_popups_on_the_screen_side() {
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let a = RECT { left: 900, top: 1028, right: 940, bottom: 1080 };
        assert_eq!(place_beside(AbEdge::Bottom, a, (200, 100), 8, mon), (820, 920));
        let a = RECT { left: 900, top: 0, right: 940, bottom: 52 };
        assert_eq!(place_beside(AbEdge::Top, a, (200, 100), 8, mon), (820, 60));
        let a = RECT { left: 0, top: 500, right: 52, bottom: 540 };
        assert_eq!(place_beside(AbEdge::Left, a, (200, 100), 8, mon), (60, 470));
        let a = RECT { left: 1868, top: 1040, right: 1920, bottom: 1080 };
        // Clamped inside the monitor along the dock.
        assert_eq!(place_beside(AbEdge::Right, a, (200, 100), 8, mon), (1660, 980));
    }
}
