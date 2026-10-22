//! AppBar registration (work-area reservation) and taskbar hide/restore with crash recovery.
//! Port of the old taskbar.ts / dock.ts behavior.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{w, Error, Result};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_TOP, ABM_GETSTATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE,
    ABM_SETPOS, ABM_SETSTATE, ABS_ALWAYSONTOP, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, ShowWindow, SW_HIDE, SW_SHOW, WM_APP,
};

/// Callback message for AppBar notifications (ABN_*), WM_APP + 0x5d as in the old app.
pub const APPBAR_CALLBACK: u32 = WM_APP + 0x5d;

const STATE_FILE: &str = "taskbar-state.json";
const STILL_ACTIVE: u32 = 259;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbEdge {
    Top,
    Bottom,
}

/// True while the taskbar is hidden by us.
static APPLIED: AtomicBool = AtomicBool::new(false);
/// Taskbar AppBar state (ABS_*) from before we changed it.
static ORIGINAL_STATE: AtomicU32 = AtomicU32::new(ABS_ALWAYSONTOP);

fn abd(hwnd: HWND, edge: u32, rc: RECT) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: APPBAR_CALLBACK,
        uEdge: edge,
        rc,
        lParam: LPARAM(0),
    }
}

fn primary_monitor_rect() -> Result<RECT> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi is correctly sized and lives across the call; (0,0) is on the primary monitor.
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        // Some machines report failure with no error code; fall back to the screen size instead of aborting.
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            let (w, h) = (
                windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN),
                windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN),
            );
            return Ok(RECT { left: 0, top: 0, right: w, bottom: h });
        }
    }
    Ok(mi.rcMonitor)
}

/// Reserves a full-width strip of `height_px` physical pixels on the primary monitor's edge.
/// Maximized windows then stop above it. Returns the reserved rect.
pub fn register_appbar(hwnd: HWND, edge: AbEdge, height_px: i32) -> Result<RECT> {
    let mon = primary_monitor_rect()?;
    let (edge_raw, rc) = match edge {
        AbEdge::Top => (
            ABE_TOP,
            RECT { left: mon.left, top: mon.top, right: mon.right, bottom: mon.top + height_px },
        ),
        AbEdge::Bottom => (
            ABE_BOTTOM,
            RECT { left: mon.left, top: mon.bottom - height_px, right: mon.right, bottom: mon.bottom },
        ),
    };
    let mut data = abd(hwnd, edge_raw, rc);
    // SAFETY: data is a valid APPBARDATA for the duration of each call.
    unsafe {
        if SHAppBarMessage(ABM_NEW, &mut data) == 0 {
            return Err(Error::new(E_FAIL, "ABM_NEW failed"));
        }
        SHAppBarMessage(ABM_QUERYPOS, &mut data);
        // The shell may move the strip; keep our exact height.
        match edge {
            AbEdge::Top => data.rc.bottom = data.rc.top + height_px,
            AbEdge::Bottom => data.rc.top = data.rc.bottom - height_px,
        }
        SHAppBarMessage(ABM_SETPOS, &mut data);
    }
    Ok(data.rc)
}

/// Releases the reserved strip. Safe to call when not registered.
pub fn unregister_appbar(hwnd: HWND) {
    let mut data = abd(hwnd, ABE_TOP, RECT::default());
    // SAFETY: data is a valid APPBARDATA.
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut data);
    }
}

/// All Shell_TrayWnd and Shell_SecondaryTrayWnd windows. The primary tray comes first.
fn tray_windows() -> Vec<HWND> {
    let mut out = Vec::new();
    for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
        let mut after = HWND::default();
        // SAFETY: class is a static wide string; searching top-level windows only.
        while let Ok(h) = unsafe { FindWindowExW(None, after, class, None) } {
            if h.0.is_null() {
                break;
            }
            out.push(h);
            after = h;
        }
    }
    out
}

fn primary_tray() -> Option<HWND> {
    tray_windows().first().copied()
}

/// Win11 ignores ABM_GETSTATE/ABM_SETSTATE unless hWnd is the taskbar itself.
fn taskbar_state(tray: HWND) -> u32 {
    let mut data = abd(tray, ABE_TOP, RECT::default());
    // SAFETY: data is a valid APPBARDATA.
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) as u32 }
}

fn set_taskbar_state(tray: HWND, state: u32) {
    let mut data = abd(tray, ABE_TOP, RECT::default());
    data.lParam = LPARAM(state as isize);
    // SAFETY: data is a valid APPBARDATA.
    unsafe {
        SHAppBarMessage(ABM_SETSTATE, &mut data);
    }
}

fn show_trays(show: bool) {
    let cmd = if show { SW_SHOW } else { SW_HIDE };
    for h in tray_windows() {
        // SAFETY: h is a live tray window handle from the enumeration above.
        unsafe {
            let _ = ShowWindow(h, cmd);
        }
    }
}

/// %APPDATA%\SmartDock
fn state_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("SmartDock"))
}

fn save_state(original: u32) -> std::io::Result<()> {
    let dir = state_dir().ok_or_else(|| std::io::Error::other("APPDATA not set"))?;
    std::fs::create_dir_all(&dir)?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let json = serde_json::json!({ "state": original, "pid": std::process::id(), "time": time });
    std::fs::write(dir.join(STATE_FILE), json.to_string())
}

fn remove_state_file() {
    if let Some(dir) = state_dir() {
        let _ = std::fs::remove_file(dir.join(STATE_FILE));
    }
}

/// Auto-hides the taskbar on all monitors and hides its windows (as the old app did).
/// The original state is saved to disk first; if that fails, nothing is changed.
/// No-op if already hidden.
pub fn hide_taskbar() {
    if APPLIED.load(Ordering::SeqCst) {
        return;
    }
    let Some(tray) = primary_tray() else { return };
    let original = taskbar_state(tray);
    if let Err(e) = save_state(original) {
        eprintln!("[taskbar] cannot save state, not hiding: {e}");
        return;
    }
    ORIGINAL_STATE.store(original, Ordering::SeqCst);
    APPLIED.store(true, Ordering::SeqCst);
    set_taskbar_state(tray, ABS_AUTOHIDE);
    show_trays(false);
}

/// Restores the taskbar saved by `hide_taskbar`. Idempotent and synchronous: safe to call
/// from any thread, including panic hooks and Ctrl+C handlers.
pub fn restore_taskbar() {
    if !APPLIED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(tray) = primary_tray() {
        set_taskbar_state(tray, ORIGINAL_STATE.load(Ordering::SeqCst));
    }
    show_trays(true);
    APPLIED.store(false, Ordering::SeqCst);
    remove_state_file();
}

/// Exit-path entry point (panic hook, Ctrl+C handler, WM_ENDSESSION). Same as `restore_taskbar`.
pub fn ensure_restored_on_exit() {
    restore_taskbar();
}

/// Re-hides any tray window Explorer re-showed (flash, notification). Call about every 500 ms.
/// No-op unless the taskbar is hidden by us.
pub fn enforce_taskbar_hidden() {
    if APPLIED.load(Ordering::SeqCst) {
        show_trays(false);
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: handle is closed before returning.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = CloseHandle(h);
        ok && code == STILL_ACTIVE
    }
}

/// Restores the taskbar if a previous run died while it was hidden, then deletes the state file.
/// Skipped if the recorded process is still running (second instance), to avoid fighting it.
pub fn recover_from_previous_crash() {
    let Some(dir) = state_dir() else { return };
    let path = dir.join(STATE_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if let Some(pid) = v["pid"].as_u64().map(|p| p as u32) {
        if pid != std::process::id() && process_alive(pid) {
            return;
        }
    }
    let state = v["state"].as_u64().map(|s| s as u32).unwrap_or(ABS_ALWAYSONTOP);
    eprintln!("[taskbar] restoring state {state} from previous run");
    if let Some(tray) = primary_tray() {
        set_taskbar_state(tray, state);
    }
    show_trays(true);
    let _ = std::fs::remove_file(&path);
}
