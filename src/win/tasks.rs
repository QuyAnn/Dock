//! Window list, activation, shell hook messages and attention flags.
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
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
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumChildWindows, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, PostMessageW, RegisterShellHookWindow, RegisterWindowMessageW,
    SetForegroundWindow, ShowWindow, DeregisterShellHookWindow, GWL_EXSTYLE, GW_OWNER, SW_MINIMIZE,
    SW_RESTORE, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::core::w;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use crate::model::WindowEntry;

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
        if !is_frame_host(&entry.exe) {
            continue;
        }
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
    Some(WindowEntry {
        hwnd: hwnd.0 as isize,
        title,
        exe,
        aumid,
        package_path,
        attention: false,
        minimized: IsIconic(hwnd).as_bool(),
    })
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
        *g = hwnd;
    }
}

fn last_foreign() -> isize {
    LAST_FOREIGN.lock().map(|g| *g).unwrap_or(0)
}

/// Taskbar-style click: focused (or last-foreign when fg is 0) → minimize; otherwise restore+focus.
pub fn activate_window(hwnd: isize) {
    let fg = foreground_hwnd();
    if fg == hwnd || (fg == 0 && last_foreign() == hwnd) {
        minimize_window(hwnd);
        return;
    }
    focus_window(hwnd);
}

/// Brings a window to the front (restoring it if minimized). Never activates SmartDock.
pub fn focus_window(hwnd: isize) {
    let target = HWND(hwnd as _);
    // SAFETY: plain Win32 calls on a handle that may be stale; failures are ignored.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid as *mut u32));
        if pid == 0 || pid == GetCurrentProcessId() {
            return;
        }
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        // Attaching to the foreground thread lets SetForegroundWindow succeed from a background process.
        // Always detach below, even if the calls fail, so input state is never left shared.
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let cur_tid = GetCurrentThreadId();
        let attach = fg_tid != 0 && fg_tid != cur_tid;
        if attach {
            let _ = AttachThreadInput(cur_tid, fg_tid, BOOL::from(true));
        }
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        if attach {
            let _ = AttachThreadInput(cur_tid, fg_tid, BOOL::from(false));
        }
        note_foreign_foreground(hwnd);
    }
}

/// Minimizes a user window without activating SmartDock.
pub fn minimize_window(hwnd: isize) {
    let target = HWND(hwnd as _);
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid as *mut u32));
        if pid == 0 || pid == GetCurrentProcessId() {
            return;
        }
        let _ = ShowWindow(target, SW_MINIMIZE);
    }
}

/// Graceful close via WM_CLOSE (never TerminateProcess / taskkill).
pub fn close_window(hwnd: isize) {
    let target = HWND(hwnd as _);
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid as *mut u32));
        if pid == 0 || pid == GetCurrentProcessId() {
            return;
        }
        let _ = PostMessageW(target, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
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
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// Opens Start by synthesizing one Win-key press. Called only from the explicit Start button.
pub fn open_start_menu() {
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
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
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
