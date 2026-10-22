//! Window list, activation, shell hook messages and attention flags.
use std::collections::{HashMap, HashSet};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{LazyLock, Mutex};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
    QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::Storage::Packaging::Appx::{
    GetApplicationUserModelId, GetPackageFullName, GetPackagePathByFullName,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_LWIN,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumChildWindows, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, PostMessageW, RegisterShellHookWindow, RegisterWindowMessageW,
    SetForegroundWindow, DeregisterShellHookWindow, GWL_EXSTYLE, GW_OWNER, SW_MINIMIZE,
    SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    AllowSetForegroundWindow, FindWindowW, ShowWindowAsync, SwitchToThisWindow, ASFW_ANY, SC_TASKLIST,
    WM_SYSCOMMAND, SC_CLOSE, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, SC_MINIMIZE,
    ShowWindow, IsWindow, GetDesktopWindow, GetShellWindow,
};

use crate::model::WindowEntry;
use crate::win::icons::{aumid_from_hwnd, SETTINGS_AUMID};

/// Last foreground window that wasn't SmartDock (dock is non-activating).
static LAST_FOREIGN: Mutex<isize> = Mutex::new(0);

/// Shell window classes that are never listed.
const SKIP_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// ApplicationFrameHost hwnd → last resolved package exe (survives CoreWindow detach on minimize).
static FRAME_MEMO: LazyLock<Mutex<HashMap<isize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct EnumState {
    out: Vec<WindowEntry>,
    /// Title → pid of top-level `Windows.UI.Core.CoreWindow` (minimized UWP often lives here).
    core_by_title: HashMap<String, u32>,
}

/// Top-level windows that the taskbar would show, excluding SmartDock's own windows.
pub fn list_windows() -> Vec<WindowEntry> {
    let mut state = EnumState {
        out: Vec::new(),
        core_by_title: HashMap::new(),
    };
    // SAFETY: the callback runs only during EnumWindows and receives a pointer to `state`.
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut state as *mut EnumState as isize),
        );
    }
    // CoreWindows may be enumerated after their frame: resolve remaining frames by title.
    for entry in &mut state.out {
        if is_frame_host(&entry.exe) {
            if let Some(&pid) = state.core_by_title.get(&entry.title) {
                let identity = process_identity(pid);
                if !identity.0.is_empty() && !is_frame_host(&identity.0) {
                    entry.exe = identity.0.clone();
                    if !identity.1.is_empty() {
                        entry.aumid = identity.1;
                    }
                    if !identity.2.is_empty() {
                        entry.package_path = identity.2;
                    }
                    if let Ok(mut memo) = FRAME_MEMO.lock() {
                        memo.insert(entry.hwnd, identity.0);
                    }
                }
            }
        }
        fill_aumid(entry);
    }
    if let Ok(mut memo) = FRAME_MEMO.lock() {
        memo.retain(|hwnd, _| state.out.iter().any(|w| w.hwnd == *hwnd));
    }
    state.out
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the `&mut EnumState` passed by list_windows.
    let state = &mut *(lparam.0 as *mut EnumState);
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    let cls = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if cls == "Windows.UI.Core.CoreWindow" {
        let title = window_title(hwnd);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !title.is_empty() && pid != 0 {
            state.core_by_title.insert(title, pid);
        }
        return BOOL(1);
    }
    if let Some(entry) = task_entry(hwnd, &state.core_by_title) {
        state.out.push(entry);
    }
    BOOL(1)
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Taskbar rules: visible, not owned (unless WS_EX_APPWINDOW), not a tool window, titled, not cloaked.
unsafe fn task_entry(hwnd: HWND, core_by_title: &HashMap<String, u32>) -> Option<WindowEntry> {
    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let app_window = (ex & WS_EX_APPWINDOW.0) != 0;
    let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
    if !app_window && (owned || (ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)) != 0) {
        return None;
    }

    let title = window_title(hwnd);
    if title.is_empty() {
        return None;
    }

    let mut cloaked: u32 = 0;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4).is_ok()
        && cloaked != 0
    {
        return None;
    }

    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    let cls = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if SKIP_CLASSES.contains(&cls.as_str()) {
        return None;
    }

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    if pid == 0 || pid == GetCurrentProcessId() {
        return None;
    }
    let (mut exe, mut aumid, mut package_path) = process_identity(pid);
    // UWP apps are often hosted by ApplicationFrameHost; resolve the real package exe via a
    // child CoreWindow, same-titled top-level CoreWindow, or last successful memo.
    if is_frame_host(&exe) {
        let host = exe.clone();
        if let Some((real_exe, real_aumid, real_pkg)) = resolve_frame_host(hwnd, pid) {
            if !real_exe.is_empty() {
                exe = real_exe;
            }
            if !real_aumid.is_empty() {
                aumid = real_aumid;
            }
            if !real_pkg.is_empty() {
                package_path = real_pkg;
            }
        }
        if exe == host {
            if let Some(&cpid) = core_by_title.get(&title) {
                let identity = process_identity(cpid);
                if !identity.0.is_empty() && !is_frame_host(&identity.0) {
                    exe = identity.0;
                    if !identity.1.is_empty() {
                        aumid = identity.1;
                    }
                    if !identity.2.is_empty() {
                        package_path = identity.2;
                    }
                }
            }
        }
        if exe == host {
            if let Ok(memo) = FRAME_MEMO.lock() {
                if let Some(prev) = memo.get(&(hwnd.0 as isize)) {
                    exe = prev.clone();
                }
            }
        }
        if exe != host {
            if let Ok(mut memo) = FRAME_MEMO.lock() {
                memo.insert(hwnd.0 as isize, exe.clone());
            }
        }
    }
    let mut entry = WindowEntry {
        hwnd: hwnd.0 as isize,
        title,
        exe,
        aumid,
        package_path,
        attention: false,
        minimized: IsIconic(hwnd).as_bool(),
    };
    fill_aumid(&mut entry);
    Some(entry)
}

/// Full image paths of running processes (best-effort). Used by Grok Bot discovery only.
pub fn snapshot_process_exes() -> Vec<PathBuf> {
    let mut pids = vec![0u32; 1024];
    let mut needed = 0u32;
    // SAFETY: EnumProcesses writes PID dwords into `pids`.
    let ok = unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed).is_ok() };
    if !ok {
        return Vec::new();
    }
    let count = (needed as usize / 4).min(pids.len());
    let mut out = Vec::new();
    for &pid in &pids[..count] {
        if pid == 0 {
            continue;
        }
        let identity = process_identity(pid);
        if !identity.0.is_empty() {
            out.push(PathBuf::from(identity.0));
        }
    }
    out
}

/// Prefer SHGetPropertyStoreForWindow, then known Settings AUMID for SystemSettings.exe.
fn fill_aumid(entry: &mut WindowEntry) {
    if entry.aumid.is_empty() {
        if let Some(id) = aumid_from_hwnd(entry.hwnd) {
            entry.aumid = id;
        }
    }
    if entry.aumid.is_empty() && is_settings_exe(&entry.exe) {
        entry.aumid = SETTINGS_AUMID.to_owned();
    }
}

fn is_settings_exe(exe: &str) -> bool {
    Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("SystemSettings.exe"))
}

fn is_frame_host(exe: &str) -> bool {
    Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("ApplicationFrameHost.exe"))
}

/// Walk children of an ApplicationFrameHost frame for a different PID's image / package.
unsafe fn resolve_frame_host(frame: HWND, host_pid: u32) -> Option<(String, String, String)> {
    struct State {
        host_pid: u32,
        found: Option<(String, String, String)>,
    }
    unsafe extern "system" fn child_proc(child: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if state.found.is_some() {
            return BOOL(0);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(child, Some(&mut pid));
        if pid == 0 || pid == state.host_pid {
            return BOOL(1);
        }
        let identity = process_identity(pid);
        if !identity.0.is_empty() && !is_frame_host(&identity.0) {
            state.found = Some(identity);
            return BOOL(0);
        }
        // Still useful when only AUMID/package is available on the child process.
        if !identity.1.is_empty() || !identity.2.is_empty() {
            state.found = Some(identity);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut state = State { host_pid, found: None };
    let _ = EnumChildWindows(frame, Some(child_proc), LPARAM(&mut state as *mut _ as isize));
    state.found
}

/// Full image path plus packaged app identity while the process handle is open.
fn process_identity(pid: u32) -> (String, String, String) {
    // SAFETY: the handle is closed before returning; buf and size describe the same buffer.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL::from(false), pid) else {
            return Default::default();
        };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            Default::default(),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let exe = ok.then(|| String::from_utf16_lossy(&buf[..size as usize])).unwrap_or_default();
        let aumid = process_string(process, GetApplicationUserModelId);
        let package_name = process_string(process, GetPackageFullName);
        let package_path = package_name
            .as_ref()
            .and_then(|name| package_path(name))
            .unwrap_or_default();
        let _ = CloseHandle(process);
        (exe, aumid.unwrap_or_default(), package_path)
    }
}

unsafe fn process_string(
    process: windows::Win32::Foundation::HANDLE,
    getter: unsafe fn(windows::Win32::Foundation::HANDLE, *mut u32, PWSTR) -> windows::Win32::Foundation::WIN32_ERROR,
) -> Option<String> {
    let mut len = 0u32;
    let _ = getter(process, &mut len, PWSTR::null());
    if len == 0 {
        return None;
    }
    let mut value = vec![0u16; len as usize];
    if getter(process, &mut len, PWSTR(value.as_mut_ptr())).is_err() {
        return None;
    }
    Some(String::from_utf16_lossy(&value[..len.saturating_sub(1) as usize]))
}

unsafe fn package_path(full_name: &str) -> Option<String> {
    let wide: Vec<u16> = full_name.encode_utf16().chain(Some(0)).collect();
    let mut len = 0u32;
    let _ = GetPackagePathByFullName(PCWSTR(wide.as_ptr()), &mut len, PWSTR::null());
    if len == 0 {
        return None;
    }
    let mut path = vec![0u16; len as usize];
    if GetPackagePathByFullName(PCWSTR(wide.as_ptr()), &mut len, PWSTR(path.as_mut_ptr())).is_err() {
        return None;
    }
    Some(String::from_utf16_lossy(&path[..len.saturating_sub(1) as usize]))
}

/// Foreground HWND, or 0 when unavailable.
pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Remembers the last non-dock foreground window (call when listing / on shell activate).
pub fn note_foreign_foreground(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    if let Ok(mut g) = LAST_FOREIGN.lock() {
        if *g != hwnd {
            if let Ok(mut b) = LAST_FOREIGN_BEFORE.lock() {
                *b = *g;
            }
        }
        *g = hwnd;
    }
}

/// The foreign foreground window before `LAST_FOREIGN` (where focus goes back to).
static LAST_FOREIGN_BEFORE: Mutex<isize> = Mutex::new(0);

fn last_foreign() -> isize {
    LAST_FOREIGN.lock().map(|g| *g).unwrap_or(0)
}

/// Foreground as reported by EVENT_SYSTEM_FOREGROUND (never a SmartDock window).
static HOOK_FG: AtomicIsize = AtomicIsize::new(0);
/// Last foreground *app* window that is not the Start/Search host (used to dismiss Start).
static LAST_APP: AtomicIsize = AtomicIsize::new(0);
/// Foreground captured when the mouse went down on the dock, before any activation.
static CLICK_FG: AtomicIsize = AtomicIsize::new(0);
/// Whether Start was open when the mouse went down on the dock.
static CLICK_START_OPEN: AtomicBool = AtomicBool::new(false);
/// Optional observer of foreground changes (usage tracking for auto-pins).
static FG_OBSERVER: Mutex<Option<fn(isize)>> = Mutex::new(None);

/// Tracks the foreground window with an out-of-context WinEvent hook (skips our own process),
/// so toggle decisions never depend on a racy GetForegroundWindow at click time.
/// Call once on the UI thread (needs its message loop).
pub fn install_foreground_hook(observer: Option<fn(isize)>) {
    if let Ok(mut o) = FG_OBSERVER.lock() {
        *o = observer;
    }
    // SAFETY: the hook lives for the process; the callback only touches atomics.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    let fg = foreground_hwnd();
    if fg != 0 && !is_own(fg) {
        HOOK_FG.store(fg, Ordering::SeqCst);
    }
    crate::log(&format!("foreground hook installed={}", !hook.is_invalid()));
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let h = hwnd.0 as isize;
    if h == 0 {
        return;
    }
    HOOK_FG.store(h, Ordering::SeqCst);
    note_foreign_foreground(h);
    if !is_start_host(hwnd) {
        LAST_APP.store(h, Ordering::SeqCst);
    }
    let observer = FG_OBSERVER.lock().ok().and_then(|o| *o);
    if let Some(f) = observer {
        f(h);
    }
}

fn is_own(hwnd: isize) -> bool {
    let mut pid = 0u32;
    // SAFETY: plain query.
    unsafe { GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() }
}

/// The foreground app right now: the real foreground unless it is SmartDock itself, then the
/// hook's last foreign foreground.
fn effective_foreground() -> isize {
    let fg = foreground_hwnd();
    if fg != 0 && !is_own(fg) {
        return fg;
    }
    let hooked = HOOK_FG.load(Ordering::SeqCst);
    if hooked != 0 {
        hooked
    } else {
        last_foreign()
    }
}

/// Call on WM_LBUTTONDOWN on the dock: snapshot foreground + Start state before anything
/// the click does can change them.
pub fn capture_click_state() {
    let fg = effective_foreground();
    CLICK_FG.store(fg, Ordering::SeqCst);
    let start = start_open_now();
    CLICK_START_OPEN.store(start, Ordering::SeqCst);
    if start {
        crate::log(&format!("{} click down: Start is open", ts()));
    }
}

/// Whether Start was open when the current click began.
pub fn start_was_open_at_click() -> bool {
    CLICK_START_OPEN.load(Ordering::SeqCst)
}

/// Taskbar-style click: the app that was active at mouse-down (and not minimized) is
/// minimized; anything else is restored + focused.
pub fn activate_window(hwnd: isize) {
    let captured = CLICK_FG.swap(0, Ordering::SeqCst);
    let fg = if captured != 0 { captured } else { effective_foreground() };
    // SAFETY: IsIconic on a possibly stale handle just returns false.
    let iconic = unsafe { IsIconic(HWND(hwnd as _)).as_bool() };
    let active = fg == hwnd || owner_root(fg) == hwnd;
    crate::log(&format!(
        "toggle hwnd={hwnd:#x} fg_at_down={fg:#x} iconic={iconic} -> {}",
        if active && !iconic { "minimize" } else { "focus" }
    ));
    if active && !iconic {
        minimize_window(hwnd);
        return;
    }
    focus_window(hwnd);
}

/// Top-level owner of `hwnd` (dialogs / owned windows belong to their app window).
fn owner_root(hwnd: isize) -> isize {
    let mut h = HWND(hwnd as _);
    // SAFETY: GetWindow on possibly stale handles just fails.
    unsafe {
        for _ in 0..8 {
            match GetWindow(h, GW_OWNER) {
                Ok(o) if !o.is_invalid() => h = o,
                _ => break,
            }
        }
    }
    h.0 as isize
}

/// Brings a window to the front (restoring it if minimized). Never activates SmartDock.
///
/// The click on the dock gives this process the foreground right; we pass it on with
/// AllowSetForegroundWindow, restore asynchronously (works for hung/elevated windows such as
/// Task Manager without blocking the dock), then SetForegroundWindow with a brief input attach.
/// If Windows still refuses, SwitchToThisWindow is the shell's own fallback.
pub fn focus_window(hwnd: isize) {
    let target = HWND(hwnd as _);
    // SAFETY: plain Win32 calls on a handle that may be stale; failures are ignored.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid as *mut u32));
        if pid == 0 || pid == GetCurrentProcessId() {
            crate::log(&format!("focus hwnd={hwnd:#x} skipped (pid={pid})"));
            return;
        }
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let iconic = IsIconic(target).as_bool();
        if iconic {
            let _ = ShowWindowAsync(target, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let cur_tid = GetCurrentThreadId();
        let attach = fg_tid != 0 && fg_tid != cur_tid;
        if attach {
            let _ = AttachThreadInput(cur_tid, fg_tid, BOOL::from(true));
        }
        let _ = BringWindowToTop(target);
        let set_ok = SetForegroundWindow(target).as_bool();
        if attach {
            let _ = AttachThreadInput(cur_tid, fg_tid, BOOL::from(false));
        }
        let mut switched = false;
        if !set_ok || GetForegroundWindow() != target {
            SwitchToThisWindow(target, BOOL::from(true));
            switched = true;
        }
        note_foreign_foreground(hwnd);
        crate::log(&format!(
            "focus hwnd={hwnd:#x} iconic={iconic} set_fg={set_ok} switch_fallback={switched} fg_is_target={}",
            GetForegroundWindow() == target
        ));
    }
}

/// Minimizes a user window without activating SmartDock.
///
/// ShowWindowAsync first (never blocks). Elevated windows (Task Manager) can refuse calls from
/// our medium-integrity process, so a worker verifies with IsIconic and falls back to
/// ShowWindow, then WM_SYSCOMMAND/SC_MINIMIZE, and finally hands the foreground back to the
/// previous app so the elevated window at least stops being the active one. Every step is logged.
pub fn minimize_window(hwnd: isize) {
    let target = HWND(hwnd as _);
    let mut pid = 0u32;
    // SAFETY: plain query on a possibly stale handle.
    unsafe { GetWindowThreadProcessId(target, Some(&mut pid as *mut u32)) };
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return;
    }
    // SAFETY: async show command on a foreign top-level window.
    let posted = unsafe { ShowWindowAsync(target, SW_MINIMIZE).as_bool() };
    let prev = LAST_FOREIGN_BEFORE.lock().map(|g| *g).unwrap_or(0);
    std::thread::spawn(move || {
        let target = HWND(hwnd as _);
        let iconic = || unsafe { IsIconic(target).as_bool() };
        std::thread::sleep(std::time::Duration::from_millis(150));
        if iconic() {
            crate::log(&format!("minimize hwnd={hwnd:#x} async={posted} ok"));
            return;
        }
        // SAFETY: synchronous variant on a worker thread, so a hung target can't stall the UI.
        unsafe {
            let _ = ShowWindow(target, SW_MINIMIZE);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        if iconic() {
            crate::log(&format!("minimize hwnd={hwnd:#x} async={posted} sync=ok"));
            return;
        }
        let sc = unsafe { PostMessageW(target, WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0)).is_ok() };
        std::thread::sleep(std::time::Duration::from_millis(150));
        if iconic() {
            crate::log(&format!("minimize hwnd={hwnd:#x} async={posted} sc_minimize={sc} ok"));
            return;
        }
        // Blocked (UIPI). Give the foreground to the previous app instead.
        let back = if prev != 0 && prev != hwnd && unsafe { IsWindow(HWND(prev as _)).as_bool() } {
            prev
        } else {
            unsafe { GetShellWindow().0 as isize }
        };
        crate::log(&format!(
            "minimize hwnd={hwnd:#x} blocked (async={posted} sc_minimize={sc}); focusing previous {back:#x}"
        ));
        if back != 0 {
            unsafe {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = SetForegroundWindow(HWND(back as _));
            }
        }
    });
}

/// Graceful close via WM_CLOSE (never TerminateProcess / taskkill).
pub fn close_window(hwnd: isize) {
    close_windows(vec![hwnd]);
}

/// Taskbar-style "Close window(s)": asks every real top-level window of the app to close,
/// politely (WM_SYSCOMMAND SC_CLOSE, like the taskbar's own menu). Never kills a process.
/// Runs on a worker thread and logs every attempt; elevated targets (UIPI refuses our
/// messages) are retried through UI Automation's WindowPattern.Close, and if that is refused
/// too the user gets a small "needs admin" hint.
pub fn close_windows(hwnds: Vec<isize>) {
    let mut targets: Vec<isize> = Vec::new();
    for h in hwnds {
        let root = close_target(h);
        if root != 0 && !targets.contains(&root) {
            targets.push(root);
        }
    }
    crate::log(&format!("{} close: request for {} window(s) {:x?}", ts(), targets.len(), targets));
    if targets.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let mut blocked: Vec<(isize, String)> = Vec::new();
        for &h in &targets {
            let hwnd = HWND(h as _);
            let mut pid = 0u32;
            // SAFETY: plain queries / posts on a possibly stale handle.
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
                crate::log(&format!("{} close: hwnd={h:#x} skipped (gone or own window)", ts()));
                continue;
            }
            let exe = process_identity(pid).0;
            let name = Path::new(&exe).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let sys = unsafe { PostMessageW(hwnd, WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0)) };
            let sys_err = sys.as_ref().err().map(|e| format!("{:#x}", e.code().0));
            let mut ok = sys.is_ok();
            let mut via = "sc_close";
            if !ok {
                // Some windows ignore system commands they did not create; WM_CLOSE is the fallback.
                ok = unsafe { PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok();
                via = "wm_close";
            }
            crate::log(&format!(
                "{} close: hwnd={h:#x} class={} exe={name} pid={pid} posted={ok} via={via} sc_err={}",
                ts(),
                class_of(hwnd),
                sys_err.as_deref().unwrap_or("-")
            ));
            if !ok {
                // UIPI: a medium-integrity process cannot post to an elevated window.
                let uia = uia_close(hwnd);
                crate::log(&format!("{} close: hwnd={h:#x} uia_close={uia}", ts()));
                if !uia {
                    blocked.push((h, name));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
        for &h in &targets {
            // SAFETY: plain queries.
            let alive = unsafe { IsWindow(HWND(h as _)).as_bool() && IsWindowVisible(HWND(h as _)).as_bool() };
            crate::log(&format!("{} close: hwnd={h:#x} after 1.5s -> {}", ts(), if alive { "still open (app may be asking to save, or hides to tray)" } else { "closed" }));
        }
        if !blocked.is_empty() {
            let names: Vec<String> = blocked.iter().map(|(_, n)| n.clone()).collect();
            crate::log(&format!("{} close: needs admin for {:?} (elevated, UIPI blocked)", ts(), names));
            needs_admin_hint(&names.join(", "));
        }
    });
}

/// The window that should receive the close: the root owner of `hwnd` when that owner is a
/// visible top-level window (dialogs close with their app window), else `hwnd` itself.
fn close_target(hwnd: isize) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_ROOTOWNER};
    let h = HWND(hwnd as _);
    // SAFETY: plain queries on a possibly stale handle.
    unsafe {
        if !IsWindow(h).as_bool() {
            return 0;
        }
        let root = GetAncestor(h, GA_ROOTOWNER);
        if !root.is_invalid() && root != h && IsWindowVisible(root).as_bool() {
            return root.0 as isize;
        }
    }
    hwnd
}

/// UI Automation WindowPattern.Close; works across integrity levels only where Windows allows.
fn uia_close(hwnd: HWND) -> bool {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationWindowPattern, UIA_WindowPatternId};
    // SAFETY: COM calls on this worker thread; every interface is released on drop.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else {
            return false;
        };
        let Ok(el) = uia.ElementFromHandle(hwnd) else { return false };
        let Ok(pattern) = el.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId) else {
            return false;
        };
        pattern.Close().is_ok()
    }
}

/// Small non-blocking note when an elevated app refused to be closed by the dock.
fn needs_admin_hint(names: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_SETFOREGROUND, MB_TOPMOST};
    let text = windows::core::HSTRING::from(format!(
        "{names} is running as administrator, so SmartDock (not elevated) is not allowed to close it.\n\nClose it from its own window instead."
    ));
    let title = windows::core::HSTRING::from("SmartDock · needs admin");
    std::thread::spawn(move || unsafe {
        // SAFETY: both strings live for the call; ownerless box on its own thread.
        MessageBoxW(None, &text, &title, MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND);
    });
}

/// Local wall-clock timestamp for decision logs.
fn ts() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: plain query.
    let t = unsafe { GetLocalTime() };
    format!("[{:02}:{:02}:{:02}.{:03}]", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

/// Starts a new instance of an exe (or .lnk). Packaged apps go through the shell.
pub fn launch_exe(exe: &str) {
    if exe.is_empty() || !PathBuf::from(exe).exists() {
        return;
    }
    let wide: Vec<u16> = std::ffi::OsStr::new(exe).encode_wide().chain(Some(0)).collect();
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Dock Start button: toggles the Start menu. The decision (open vs close) is taken from the
/// state captured at mouse-down; opening sends exactly one toggle (SC_TASKLIST) and never a
/// second one, so Start cannot close itself right after opening. Outside clicks are left to Windows.
pub fn open_start_menu() {
    if start_was_open_at_click() {
        crate::log(&format!("{} start: open at mouse-down -> closing", ts()));
        close_start_menu();
        return;
    }
    // SAFETY: FindWindowW/PostMessageW with static strings; the tray HWND may be absent.
    let posted = unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        match FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            Ok(tray) if !tray.is_invalid() => {
                PostMessageW(tray, WM_SYSCOMMAND, WPARAM(SC_TASKLIST as usize), LPARAM(0)).is_ok()
            }
            _ => false,
        }
    };
    crate::log(&format!("{} start: closed at mouse-down -> open; SC_TASKLIST posted={posted}", ts()));
    if !posted {
        // The only toggle sent in this case (SC_TASKLIST never went out).
        send_win_key();
        return;
    }
    std::thread::spawn(|| {
        // Observe only: learn the Start host window so later clicks can close it. No toggles here.
        for delay in [300u64, 700] {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            let fg = unsafe { GetForegroundWindow() };
            if let Some(h) = start_window().or_else(|| fg_start_like(fg).then_some(fg)) {
                START_HWND.store(h.0 as isize, Ordering::SeqCst);
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
                let exe = process_identity(pid).0;
                crate::log(&format!(
                    "{} start: open (hwnd={:#x} host={} foreground={})",
                    ts(),
                    h.0 as isize,
                    Path::new(&exe).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    fg == h
                ));
                return;
            }
        }
        crate::log(&format!(
            "{} start: no Start window seen yet (fg class {}); no fallback toggle sent",
            ts(),
            class_of(unsafe { GetForegroundWindow() })
        ));
    });
}

/// Closes Start: hand the foreground back to the last app (Start dismisses itself on losing
/// focus, exactly like clicking the desktop); if it is verifiably still open 300 ms later,
/// one Win key (a close, never an open: it is only sent while Start is seen open).
pub fn close_start_menu() {
    let back = LAST_APP.load(Ordering::SeqCst);
    let target = if back != 0 && unsafe { IsWindow(HWND(back as _)).as_bool() } {
        HWND(back as _)
    } else {
        unsafe { GetShellWindow() }
    };
    let ok = unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        SetForegroundWindow(target).as_bool()
    };
    crate::log(&format!("{} start: closing via foreground -> {:#x} ({}) ok={ok}", ts(), target.0 as isize, class_of(target)));
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if start_open_now() {
            crate::log(&format!("{} start: still open, sending Win key to close", ts()));
            send_win_key();
        } else {
            crate::log(&format!("{} start: closed", ts()));
        }
    });
}

/// Called for a genuine dock click that hit nothing: dismisses Start only if it was open when
/// this click went down (never on mouse-up or focus changes).
pub fn dismiss_start_if_open() {
    if start_was_open_at_click() {
        crate::log(&format!("{} start: empty dock click while open -> closing", ts()));
        close_start_menu();
    }
}

/// Start host window learned after opening (Windows builds differ in which process hosts it).
static START_HWND: AtomicIsize = AtomicIsize::new(0);

/// Whether Start is open right now: a visible, uncloaked Start host window, or a shell
/// CoreWindow flyout (Start/Search) holding the foreground.
fn start_open_now() -> bool {
    start_window().is_some() || fg_start_like(unsafe { GetForegroundWindow() })
}

fn fg_start_like(fg: HWND) -> bool {
    if fg.is_invalid() || class_of(fg) != "Windows.UI.Core.CoreWindow" {
        return false;
    }
    if fg.0 as isize == START_HWND.load(Ordering::SeqCst) || is_start_host(fg) {
        return true;
    }
    let mut pid = 0u32;
    // SAFETY: plain query.
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    let exe = process_identity(pid).0.to_ascii_lowercase();
    // The touch keyboard / emoji panel is a CoreWindow too but never Start.
    !exe.ends_with("textinputhost.exe")
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: valid buffer; stale handles return 0.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The Start / Search host window (Windows.UI.Core.CoreWindow owned by
/// StartMenuExperienceHost.exe or SearchHost.exe).
fn is_start_host(hwnd: HWND) -> bool {
    if class_of(hwnd) != "Windows.UI.Core.CoreWindow" {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: plain query.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let exe = process_identity(pid).0.to_ascii_lowercase();
    ["startmenuexperiencehost.exe", "searchhost.exe", "searchapp.exe", "shellhost.exe"]
        .iter()
        .any(|n| exe.ends_with(n))
}

/// The visible (not cloaked) Start/Search host window, if Start is open.
fn start_window() -> Option<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Option<HWND>);
        if !IsWindowVisible(hwnd).as_bool() || !is_start_host(hwnd) {
            return BOOL::from(true);
        }
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4).is_ok() && cloaked != 0 {
            return BOOL::from(true);
        }
        *out = Some(hwnd);
        BOOL::from(false)
    }
    let mut found: Option<HWND> = None;
    // SAFETY: the callback only runs during EnumWindows with a pointer to `found`.
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut Option<HWND> as isize));
    }
    let _ = GetDesktopWindow;
    found
}

fn send_win_key() {
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VK_LWIN, ..Default::default() },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VK_LWIN, dwFlags: KEYEVENTF_KEYUP, ..Default::default() },
            },
        },
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    crate::log(&format!("start: win key sent={sent}"));
}

/// Registers `hwnd` for shell messages; returns the message id of "SHELLHOOK".
pub fn register_shell_hook(hwnd: HWND) -> u32 {
    // SAFETY: hwnd is a window owned by this process; the message name is a static literal.
    unsafe {
        let _ = RegisterShellHookWindow(hwnd);
        RegisterWindowMessageW(w!("SHELLHOOK"))
    }
}

/// Undoes register_shell_hook. Call on shutdown.
pub fn unregister_shell_hook(hwnd: HWND) {
    // SAFETY: hwnd is the window previously passed to register_shell_hook.
    unsafe {
        let _ = DeregisterShellHookWindow(hwnd);
    }
}

const HSHELL_WINDOWCREATED: usize = 1;
const HSHELL_WINDOWDESTROYED: usize = 2;
const HSHELL_ACTIVATESHELLWINDOW: usize = 3;
const HSHELL_WINDOWACTIVATED: usize = 4;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
const HSHELL_FLASH: usize = 0x8006;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    Created(isize),
    Destroyed(isize),
    Activated(isize),
    Flash(isize),
    /// Known shell message we don't act on.
    Other,
}

/// Decodes a SHELLHOOK message (wparam = code, lparam = hwnd). None for unknown codes.
pub fn parse_shell_message(wparam: usize, lparam: isize) -> Option<ShellEvent> {
    Some(match wparam {
        HSHELL_WINDOWCREATED => ShellEvent::Created(lparam),
        HSHELL_WINDOWDESTROYED => ShellEvent::Destroyed(lparam),
        HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED => ShellEvent::Activated(lparam),
        HSHELL_FLASH => ShellEvent::Flash(lparam),
        HSHELL_ACTIVATESHELLWINDOW => ShellEvent::Other,
        _ => return None,
    })
}

/// Windows that asked for attention (HSHELL_FLASH) and have not been activated since.
#[derive(Default)]
pub struct Attention {
    hwnds: HashSet<isize>,
}

impl Attention {
    pub fn mark(&mut self, hwnd: isize) {
        self.hwnds.insert(hwnd);
    }

    pub fn clear(&mut self, hwnd: isize) {
        self.hwnds.remove(&hwnd);
    }

    pub fn contains(&self, hwnd: isize) -> bool {
        self.hwnds.contains(&hwnd)
    }
}
