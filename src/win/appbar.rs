//! AppBar registration (work-area reservation) and taskbar hide/restore with crash recovery.
//! Port of the old taskbar.ts / dock.ts behavior.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{w, Error, Result};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HWND, LPARAM, RECT};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_TOP, ABM_GETSTATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE,
    ABM_SETPOS, ABM_SETSTATE, ABS_ALWAYSONTOP, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, ShowWindow,
    EVENT_OBJECT_SHOW, OBJID_WINDOW, SW_HIDE, SW_SHOW, WINEVENT_OUTOFCONTEXT, WM_APP,
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
/// Hidden mode removes the primary tray HWND; autohide mode leaves it available at the edge.
static HIDE_WINDOW: AtomicBool = AtomicBool::new(true);
/// True while SmartDock has docks on secondary monitors: their Shell_SecondaryTrayWnd is
/// then hidden too (in hidden mode), since our dock replaces it there.
static SECONDARY_ACTIVE: AtomicBool = AtomicBool::new(false);
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

pub fn primary_monitor_rect() -> Result<RECT> {
    Ok(crate::win::monitors::primary_bounds()?.to_rect())
}

/// Every attached monitor's `rcMonitor`, primary first.
pub fn enum_monitor_rects() -> Vec<RECT> {
    crate::win::monitors::enum_bounds().into_iter().map(|b| b.to_rect()).collect()
}

fn edge_rect(mon: RECT, edge: AbEdge, height_px: i32) -> (u32, RECT) {
    match edge {
        AbEdge::Top => (
            ABE_TOP,
            RECT { left: mon.left, top: mon.top, right: mon.right, bottom: mon.top + height_px },
        ),
        AbEdge::Bottom => (
            ABE_BOTTOM,
            RECT { left: mon.left, top: mon.bottom - height_px, right: mon.right, bottom: mon.bottom },
        ),
    }
}

/// Reserves a full-width strip of `height_px` physical pixels on the primary monitor's edge.
/// Maximized windows then stop above it. Returns the reserved rect.
pub fn register_appbar(hwnd: HWND, edge: AbEdge, height_px: i32) -> Result<RECT> {
    register_appbar_on(hwnd, edge, height_px, primary_monitor_rect()?)
}

/// AppBar strip on a specific monitor (used for secondary docks).
pub fn register_appbar_on(hwnd: HWND, edge: AbEdge, height_px: i32, mon: RECT) -> Result<RECT> {
    let (edge_raw, rc) = edge_rect(mon, edge, height_px);
    let mut data = abd(hwnd, edge_raw, rc);
    // SAFETY: data is a valid APPBARDATA for the duration of each call.
    unsafe {
        if SHAppBarMessage(ABM_NEW, &mut data) == 0 {
            return Err(Error::new(E_FAIL, "ABM_NEW failed"));
        }
        SHAppBarMessage(ABM_QUERYPOS, &mut data);
        // ABM_QUERYPOS may return the edge above the old taskbar reservation even after its HWND is
        // hidden. Anchor explicitly to rcMonitor so no taskbar-height wallpaper strip remains.
        data.rc = rc;
        SHAppBarMessage(ABM_SETPOS, &mut data);
    }
    Ok(data.rc)
}

/// Re-applies the primary monitor edge after a topology/resolution change.
pub fn reposition_appbar(hwnd: HWND, edge: AbEdge, height_px: i32) -> Result<RECT> {
    reposition_appbar_on(hwnd, edge, height_px, primary_monitor_rect()?)
}

pub fn reposition_appbar_on(hwnd: HWND, edge: AbEdge, height_px: i32, mon: RECT) -> Result<RECT> {
    let (edge_raw, rc) = edge_rect(mon, edge, height_px);
    let mut data = abd(hwnd, edge_raw, rc);
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut data);
        data.rc = rc;
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

fn find_class_windows(class: windows::core::PCWSTR) -> Vec<HWND> {
    let mut out = Vec::new();
    let mut after = HWND::default();
    // SAFETY: class is a static wide string; searching top-level windows only.
    while let Ok(h) = unsafe { FindWindowExW(None, after, class, None) } {
        if h.0.is_null() {
            break;
        }
        out.push(h);
        after = h;
    }
    out
}

/// All Shell_TrayWnd and Shell_SecondaryTrayWnd windows. The primary tray comes first.
fn tray_windows() -> Vec<HWND> {
    let mut out = find_class_windows(w!("Shell_TrayWnd"));
    out.extend(find_class_windows(w!("Shell_SecondaryTrayWnd")));
    out
}

fn primary_tray() -> Option<HWND> {
    find_class_windows(w!("Shell_TrayWnd")).first().copied()
}

fn secondary_trays() -> Vec<HWND> {
    find_class_windows(w!("Shell_SecondaryTrayWnd"))
}

/// Whether secondary trays should currently be hidden: taskbar hidden by us in "hidden" mode
/// and a SmartDock dock exists on the secondary monitors.
fn secondaries_should_hide() -> bool {
    APPLIED.load(Ordering::SeqCst) && HIDE_WINDOW.load(Ordering::SeqCst) && SECONDARY_ACTIVE.load(Ordering::SeqCst)
}

/// Brings every Shell_SecondaryTrayWnd to the wanted state (hidden while our secondary docks
/// replace it, otherwise shown). Only acts on trays whose visibility differs, so it is cheap
/// to call from the guard timer and the show-event hook.
pub fn sync_secondary_trays() {
    let hide = secondaries_should_hide();
    for h in secondary_trays() {
        // SAFETY: h is a live tray window from FindWindowExW.
        unsafe {
            let visible = IsWindowVisible(h).as_bool();
            if hide && visible {
                let _ = ShowWindow(h, SW_HIDE);
            } else if !hide && !visible {
                let _ = ShowWindow(h, SW_SHOW);
            }
        }
    }
}

/// Kept for call sites: same as `sync_secondary_trays`.
pub fn ensure_secondary_trays_visible() {
    sync_secondary_trays();
}

/// Tells the taskbar logic whether SmartDock currently has secondary-monitor docks.
pub fn set_secondary_docks_active(active: bool) {
    SECONDARY_ACTIVE.store(active, Ordering::SeqCst);
    sync_secondary_trays();
}

/// Explorer re-shows a hidden tray when the pointer touches an auto-hide edge. Re-hide it the
/// moment it is shown (EVENT_OBJECT_SHOW from Explorer's process only), not 500 ms later.
pub fn install_tray_show_hook() {
    let Some(tray) = primary_tray() else { return };
    let mut pid = 0u32;
    // SAFETY: plain query on a live window; the hook is out-of-context and lives for the process.
    unsafe {
        GetWindowThreadProcessId(tray, Some(&mut pid));
        if pid == 0 {
            return;
        }
        let hook = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, None, Some(on_tray_show), pid, 0, WINEVENT_OUTOFCONTEXT);
        crate::log(&format!("taskbar: show hook installed={}", !hook.is_invalid()));
    }
}

unsafe extern "system" fn on_tray_show(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW.0 || hwnd.is_invalid() || !APPLIED.load(Ordering::SeqCst) {
        return;
    }
    let mut buf = [0u16; 32];
    // SAFETY: valid buffer; hwnd may be stale, in which case the call returns 0.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
        enforce_taskbar_hidden();
    }
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

fn show_primary_tray(show: bool) {
    let cmd = if show { SW_SHOW } else { SW_HIDE };
    if let Some(h) = primary_tray() {
        // SAFETY: h is the live primary tray window. Never touch Shell_SecondaryTrayWnd here —
        // hiding the primary can make Explorer sync-hide secondaries; we re-show them after.
        unsafe {
            let _ = ShowWindow(h, cmd);
        }
    }
    sync_secondary_trays();
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

/// Auto-hides the primary taskbar and hides only its window.
/// The original state is saved to disk first; if that fails, nothing is changed.
/// No-op if already hidden.
pub fn hide_taskbar(hide_window: bool) {
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
    HIDE_WINDOW.store(hide_window, Ordering::SeqCst);
    set_taskbar_state(tray, ABS_AUTOHIDE);
    show_primary_tray(!hide_window);
}

pub fn set_hidden_mode(hidden: bool) {
    HIDE_WINDOW.store(hidden, Ordering::SeqCst);
    if APPLIED.load(Ordering::SeqCst) {
        show_primary_tray(!hidden);
    }
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
    APPLIED.store(false, Ordering::SeqCst);
    show_primary_tray(true); // also re-shows secondary trays (APPLIED is false now)
    remove_state_file();
}

/// Exit-path entry point (panic hook, Ctrl+C handler, WM_ENDSESSION). Same as `restore_taskbar`.
pub fn ensure_restored_on_exit() {
    restore_taskbar();
}

/// Re-hides the primary tray (and the secondary trays while our secondary docks exist) if
/// Explorer re-showed them. Call about every 500 ms and from the show hook.
pub fn enforce_taskbar_hidden() {
    if APPLIED.load(Ordering::SeqCst) && HIDE_WINDOW.load(Ordering::SeqCst) {
        if let Some(h) = primary_tray() {
            // SAFETY: live tray window.
            if unsafe { IsWindowVisible(h).as_bool() } {
                show_primary_tray(false);
                return;
            }
        }
    }
    sync_secondary_trays();
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
    // Older builds hid secondary taskbars too; recover all tray windows after a stale state file.
    for h in tray_windows() {
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
        }
    }
    let _ = std::fs::remove_file(&path);
}
