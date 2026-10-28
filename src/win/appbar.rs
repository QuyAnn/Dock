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
    SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETSTATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE,
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
    Left,
    Right,
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
/// True while the docks are released (remote session or fullscreen RDP in front): the taskbar
/// is then left alone, so nothing re-hides or re-shows it under the remote desktop.
static DOCK_RELEASED: AtomicBool = AtomicBool::new(false);

pub fn set_dock_released(released: bool) {
    DOCK_RELEASED.store(released, Ordering::SeqCst);
}

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

/// The AppBar strip of `thickness_px` physical pixels along `edge` of monitor `mon`
/// (full monitor width for top/bottom, full height for left/right) and its ABE_* code.
pub fn edge_rect(mon: RECT, edge: AbEdge, thickness_px: i32) -> (u32, RECT) {
    match edge {
        AbEdge::Top => (
            ABE_TOP,
            RECT { left: mon.left, top: mon.top, right: mon.right, bottom: mon.top + thickness_px },
        ),
        AbEdge::Bottom => (
            ABE_BOTTOM,
            RECT { left: mon.left, top: mon.bottom - thickness_px, right: mon.right, bottom: mon.bottom },
        ),
        AbEdge::Left => (
            ABE_LEFT,
            RECT { left: mon.left, top: mon.top, right: mon.left + thickness_px, bottom: mon.bottom },
        ),
        AbEdge::Right => (
            ABE_RIGHT,
            RECT { left: mon.right - thickness_px, top: mon.top, right: mon.right, bottom: mon.bottom },
        ),
    }
}

/// The work area a monitor needs so maximized windows stop at a dock on `edge` whose window
/// rect is `dock`, or `None` when `work` already leaves the dock free (or the dock does not sit
/// on that edge of `mon`). Bottom keeps the original rule: the dock top in the lower half.
pub fn work_area_fix(edge: AbEdge, mon: RECT, work: RECT, dock: RECT) -> Option<RECT> {
    let mid_y = mon.top + (mon.bottom - mon.top) / 2;
    let mid_x = mon.left + (mon.right - mon.left) / 2;
    match edge {
        AbEdge::Bottom => (dock.top > mid_y && dock.top < mon.bottom && work.bottom > dock.top)
            .then_some(RECT { bottom: dock.top, ..work }),
        AbEdge::Top => (dock.bottom < mid_y && dock.bottom > mon.top && work.top < dock.bottom)
            .then_some(RECT { top: dock.bottom, ..work }),
        AbEdge::Left => (dock.right < mid_x && dock.right > mon.left && work.left < dock.right)
            .then_some(RECT { left: dock.right, ..work }),
        AbEdge::Right => (dock.left > mid_x && dock.left < mon.right && work.right > dock.left)
            .then_some(RECT { right: dock.left, ..work }),
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
    Ok(placed_strip(edge, mon, height_px, data.rc))
}

/// The strip the dock window is placed on after ABM_SETPOS returned `granted`. Explorer trims
/// `granted` around the hidden taskbar's stale reservation (Win11 26300: a right taskbar gives
/// (1868,0)-(1872,1080), a 4-px window; a left one shifts a bottom dock to x=48). The taskbar
/// is hidden while SmartDock runs, so the dock always takes its full strip on `mon`; the trim is
/// only logged. When Explorer grants exactly the strip (the normal bottom case) nothing changes.
pub fn placed_strip(edge: AbEdge, mon: RECT, thickness_px: i32, granted: RECT) -> RECT {
    let (_, full) = edge_rect(mon, edge, thickness_px);
    if granted != full {
        crate::log(&format!(
            "appbar: Explorer granted ({},{}..{},{}) for the {} strip; using the full strip ({},{}..{},{})",
            granted.left, granted.top, granted.right, granted.bottom, edge_name(edge),
            full.left, full.top, full.right, full.bottom
        ));
    }
    full
}

fn edge_name(edge: AbEdge) -> &'static str {
    match edge {
        AbEdge::Top => "top",
        AbEdge::Bottom => "bottom",
        AbEdge::Left => "left",
        AbEdge::Right => "right",
    }
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
    Ok(placed_strip(edge, mon, height_px, data.rc))
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
        after = h;
        // Skip our own Shell_TrayWnd (win::traysvc): only Explorer's windows are managed here.
        let mut pid = 0u32;
        // SAFETY: plain query on a live window.
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == std::process::id() {
            continue;
        }
        out.push(h);
    }
    out
}

/// All Shell_TrayWnd and Shell_SecondaryTrayWnd windows. The primary tray comes first.
fn tray_windows() -> Vec<HWND> {
    let mut out = find_class_windows(w!("Shell_TrayWnd"));
    out.extend(find_class_windows(w!("Shell_SecondaryTrayWnd")));
    out
}

/// Explorer's primary taskbar (never SmartDock's own Shell_TrayWnd).
pub fn primary_tray() -> Option<HWND> {
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
    if id_object != OBJID_WINDOW.0 || hwnd.is_invalid() || !APPLIED.load(Ordering::SeqCst) || DOCK_RELEASED.load(Ordering::SeqCst) {
        return;
    }
    let mut buf = [0u16; 32];
    // SAFETY: valid buffer; hwnd may be stale, in which case the call returns 0.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
        enforce_taskbar_hidden();
        // Explorer's tray may have been raised above ours: re-take the FindWindow slot.
        crate::win::traysvc::ensure_first();
    }
}

/// A previous (older) run that died mid-peek can leave the tray layered at alpha 0: undo that.
pub fn clear_stale_tray_ghost() {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetLayeredWindowAttributes, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, LAYERED_WINDOW_ATTRIBUTES_FLAGS,
        LWA_ALPHA, WS_EX_LAYERED,
    };
    let Some(tray) = primary_tray() else { return };
    // SAFETY: plain queries/updates on the live shell window.
    unsafe {
        let ex = GetWindowLongPtrW(tray, GWL_EXSTYLE);
        if ex & WS_EX_LAYERED.0 as isize == 0 {
            return;
        }
        let (mut key, mut alpha, mut flags) = (COLORREF(0), 255u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
        if GetLayeredWindowAttributes(tray, Some(&mut key), Some(&mut alpha), Some(&mut flags)).is_ok()
            && flags.0 & LWA_ALPHA.0 != 0
            && alpha == 0
        {
            SetWindowLongPtrW(tray, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize));
            crate::log("taskbar: cleared a stale transparent tray from a previous run");
        }
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
    // Hand the tray back first so apps re-register their icons with Explorer.
    crate::win::traysvc::shutdown();
    release_forced_work_areas();
    restore_taskbar();
}

/// Re-hides the primary tray (and the secondary trays while our secondary docks exist) if
/// Explorer re-showed them. Call about every 500 ms and from the show hook.
pub fn enforce_taskbar_hidden() {
    if DOCK_RELEASED.load(Ordering::SeqCst) {
        return;
    }
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
    clear_stale_tray_ghost();
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


/// True when the work area of `dock`'s monitor reaches under the dock (its inner edge).
pub fn work_area_missing(dock: HWND) -> bool {
    dock_work_area(dock).is_some_and(|(_, _, fix)| fix.is_some())
}

/// Monitor rect, work area and the needed fix (`work_area_fix`) for one dock window.
fn dock_work_area(dock: HWND) -> Option<(RECT, RECT, Option<RECT>)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindow};
    // SAFETY: plain queries on our own dock window.
    unsafe {
        if !IsWindow(dock).as_bool() {
            return None;
        }
        let mut wr = RECT::default();
        if GetWindowRect(dock, &mut wr).is_err() {
            return None;
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(dock, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return None;
        }
        let fix = work_area_fix(crate::win::edge::current(), mi.rcMonitor, mi.rcWork, wr);
        Some((mi.rcMonitor, mi.rcWork, fix))
    }
}

/// Work area without the hidden taskbar's leftover reservation: when Explorer's taskbar sits on
/// `taskbar` (another edge than the dock's) and is hidden by us, its old inset (at most
/// `max_inset` px) on that side is given back to the monitor. `None` when nothing changes.
/// A bottom taskbar under a bottom dock never changes anything (the original behaviour).
pub fn stale_taskbar_fix(dock_edge: AbEdge, taskbar: AbEdge, mon: RECT, work: RECT, max_inset: i32) -> Option<RECT> {
    if taskbar == dock_edge {
        return None;
    }
    let inset = |v: i32| v > 0 && v <= max_inset;
    let mut want = work;
    match taskbar {
        AbEdge::Left if inset(work.left - mon.left) => want.left = mon.left,
        AbEdge::Right if inset(mon.right - work.right) => want.right = mon.right,
        AbEdge::Top if inset(work.top - mon.top) => want.top = mon.top,
        AbEdge::Bottom if inset(mon.bottom - work.bottom) => want.bottom = mon.bottom,
        _ => return None,
    }
    Some(want)
}

/// Whether Explorer's taskbar window is hidden by us on the primary (`primary`) or secondary
/// monitors, so its reservation there is stale.
pub fn taskbar_hidden_on(primary: bool) -> bool {
    if primary {
        APPLIED.load(Ordering::SeqCst) && HIDE_WINDOW.load(Ordering::SeqCst)
    } else {
        secondaries_should_hide()
    }
}

/// Removes the hidden taskbar's stale work-area inset on `dock`'s monitor (taskbar on another
/// edge than the dock), then makes sure the work area stops at the dock. Returns true when it
/// changed the work area.
pub fn release_stale_taskbar_area(dock: HWND, primary: bool, dock_px: i32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPIF_SENDCHANGE, SPI_SETWORKAREA};
    if !taskbar_hidden_on(primary) || DOCK_RELEASED.load(Ordering::SeqCst) {
        return false;
    }
    let dock_edge = crate::win::edge::current();
    let taskbar = crate::win::edge::taskbar_edge();
    let Some((m, work, _)) = dock_work_area(dock) else { return false };
    let Some(mut want) = stale_taskbar_fix(dock_edge, taskbar, m, work, dock_px * 3) else { return false };
    // Keep the dock's own side reserved in the same write.
    if let Some(fixed) = dock_rect(dock).and_then(|wr| work_area_fix(dock_edge, m, want, wr)) {
        want = fixed;
    }
    // SAFETY: want is a valid RECT that outlives the SPI call.
    let ok = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut want as *mut RECT as *mut _), SPIF_SENDCHANGE) }.is_ok();
    WORK_AREA_FORCED.store(true, Ordering::SeqCst);
    crate::log(&format!(
        "workarea: hidden {} taskbar still reserved ({},{}..{},{}) on monitor ({},{}..{},{}); set ({},{}..{},{}) ok={ok}",
        edge_name(taskbar), work.left, work.top, work.right, work.bottom, m.left, m.top, m.right, m.bottom,
        want.left, want.top, want.right, want.bottom
    ));
    true
}

fn dock_rect(dock: HWND) -> Option<RECT> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut wr = RECT::default();
    // SAFETY: plain query on our own window.
    unsafe { GetWindowRect(dock, &mut wr) }.ok().map(|_| wr)
}

/// Set once we wrote a work area ourselves (SPI_SETWORKAREA); undone on exit.
static WORK_AREA_FORCED: AtomicBool = AtomicBool::new(false);
static WORK_AREA_LOGS: AtomicU32 = AtomicU32::new(0);

/// If the work area of `dock`'s monitor reaches under the dock (maximized windows would slide
/// under it), set it explicitly to end at the dock's inner edge. Returns true when
/// it had to fix something.
pub fn ensure_work_area(dock: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPIF_SENDCHANGE, SPI_SETWORKAREA};
    let Some((m, work, Some(mut want))) = dock_work_area(dock) else { return false };
    // SAFETY: want is a valid RECT that outlives the SPI call.
    let ok = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut want as *mut RECT as *mut _), SPIF_SENDCHANGE) }.is_ok();
    WORK_AREA_FORCED.store(true, Ordering::SeqCst);
    let n = WORK_AREA_LOGS.fetch_add(1, Ordering::SeqCst);
    if n < 20 || n % 100 == 0 {
        crate::log(&format!(
            "workarea: monitor ({},{}..{},{}) work ({},{}..{},{}) overlapped the {} dock; set ({},{}..{},{}) ok={ok}",
            m.left, m.top, m.right, m.bottom, work.left, work.top, work.right, work.bottom,
            crate::win::edge::current().name(), want.left, want.top, want.right, want.bottom
        ));
    }
    true
}

/// On exit: give every monitor its full area back if we ever forced one, so nothing stays
/// reserved for a dock that is gone (Explorer re-adds its taskbar when it is shown again).
fn release_forced_work_areas() {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPIF_SENDCHANGE, SPI_SETWORKAREA};
    if !WORK_AREA_FORCED.swap(false, Ordering::SeqCst) {
        return;
    }
    for mut m in enum_monitor_rects() {
        // SAFETY: m is a valid RECT for the call.
        let ok = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut m as *mut RECT as *mut _), SPIF_SENDCHANGE) }.is_ok();
        crate::log(&format!("workarea: released ({},{}..{},{}) ok={ok}", m.left, m.top, m.right, m.bottom));
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;

    const MON: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };

    #[test]
    fn dock_edge_rect_covers_each_edge() {
        assert_eq!(edge_rect(MON, AbEdge::Bottom, 65), (ABE_BOTTOM, RECT { left: 0, top: 1015, right: 1920, bottom: 1080 }));
        assert_eq!(edge_rect(MON, AbEdge::Top, 65), (ABE_TOP, RECT { left: 0, top: 0, right: 1920, bottom: 65 }));
        assert_eq!(edge_rect(MON, AbEdge::Left, 65), (ABE_LEFT, RECT { left: 0, top: 0, right: 65, bottom: 1080 }));
        assert_eq!(edge_rect(MON, AbEdge::Right, 65), (ABE_RIGHT, RECT { left: 1855, top: 0, right: 1920, bottom: 1080 }));
        // A secondary monitor left of the primary (negative coordinates).
        let sec = RECT { left: -1280, top: 100, right: 0, bottom: 1124 };
        assert_eq!(edge_rect(sec, AbEdge::Right, 52).1, RECT { left: -52, top: 100, right: 0, bottom: 1124 });
        assert_eq!(edge_rect(sec, AbEdge::Left, 52).1, RECT { left: -1280, top: 100, right: -1228, bottom: 1124 });
    }

    #[test]
    fn dock_edge_placed_strip_ignores_explorer_trims() {
        let sec = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        // Real Win11 26300 grants with the taskbar hidden on another edge.
        assert_eq!(placed_strip(AbEdge::Right, MON, 52, RECT { left: 1868, top: 0, right: 1872, bottom: 1080 }), RECT { left: 1868, top: 0, right: 1920, bottom: 1080 });
        assert_eq!(placed_strip(AbEdge::Left, MON, 52, RECT { left: 48, top: 0, right: 52, bottom: 1080 }), RECT { left: 0, top: 0, right: 52, bottom: 1080 });
        assert_eq!(placed_strip(AbEdge::Bottom, MON, 52, RECT { left: 48, top: 1028, right: 1920, bottom: 1080 }), RECT { left: 0, top: 1028, right: 1920, bottom: 1080 });
        // Bottom granted exactly: unchanged.
        let full = edge_rect(MON, AbEdge::Bottom, 52).1;
        assert_eq!(placed_strip(AbEdge::Bottom, MON, 52, full), full);
        for edge in [AbEdge::Top, AbEdge::Bottom, AbEdge::Left, AbEdge::Right] {
            for mon in [MON, sec] {
                let full = edge_rect(mon, edge, 52).1;
                assert_eq!(placed_strip(edge, mon, 52, RECT { right: full.left + 4, ..full }), full);
            }
        }
    }

    #[test]
    fn dock_edge_stale_taskbar_inset_is_released() {
        // Taskbar moved to the left and hidden; bottom dock: give the 48 px back.
        let work = RECT { left: 48, top: 0, right: 1920, bottom: 1028 };
        assert_eq!(stale_taskbar_fix(AbEdge::Bottom, AbEdge::Left, MON, work, 156), Some(RECT { left: 0, ..work }));
        // Right taskbar, left dock.
        let work = RECT { left: 52, top: 0, right: 1872, bottom: 1080 };
        assert_eq!(stale_taskbar_fix(AbEdge::Left, AbEdge::Right, MON, work, 156), Some(RECT { right: 1920, ..work }));
        // Same edge (the original bottom/bottom setup): never touched.
        assert_eq!(stale_taskbar_fix(AbEdge::Bottom, AbEdge::Bottom, MON, RECT { bottom: 1028, ..MON }, 156), None);
        assert_eq!(stale_taskbar_fix(AbEdge::Right, AbEdge::Right, MON, RECT { right: 1868, ..MON }, 156), None);
        // No inset, or an inset too large to be the taskbar (another app bar): left alone.
        assert_eq!(stale_taskbar_fix(AbEdge::Bottom, AbEdge::Left, MON, RECT { bottom: 1028, ..MON }, 156), None);
        assert_eq!(stale_taskbar_fix(AbEdge::Bottom, AbEdge::Left, MON, RECT { left: 400, bottom: 1028, ..MON }, 156), None);
    }

    #[test]
    fn dock_edge_work_area_fix_per_edge() {
        // Bottom: unchanged rule (window may extend below the monitor for the corner).
        let dock = RECT { left: 0, top: 1028, right: 1920, bottom: 1096 };
        assert_eq!(work_area_fix(AbEdge::Bottom, MON, MON, dock), Some(RECT { bottom: 1028, ..MON }));
        assert_eq!(work_area_fix(AbEdge::Bottom, MON, RECT { bottom: 1028, ..MON }, dock), None);
        let dock = edge_rect(MON, AbEdge::Top, 52).1;
        assert_eq!(work_area_fix(AbEdge::Top, MON, MON, dock), Some(RECT { top: 52, ..MON }));
        assert_eq!(work_area_fix(AbEdge::Top, MON, RECT { top: 52, ..MON }, dock), None);
        let dock = edge_rect(MON, AbEdge::Left, 52).1;
        assert_eq!(work_area_fix(AbEdge::Left, MON, MON, dock), Some(RECT { left: 52, ..MON }));
        assert_eq!(work_area_fix(AbEdge::Left, MON, RECT { left: 60, ..MON }, dock), None);
        let dock = edge_rect(MON, AbEdge::Right, 52).1;
        assert_eq!(work_area_fix(AbEdge::Right, MON, MON, dock), Some(RECT { right: 1868, ..MON }));
        // A dock that is not on that edge is never "fixed".
        assert_eq!(work_area_fix(AbEdge::Left, MON, MON, edge_rect(MON, AbEdge::Right, 52).1), None);
    }
}
