//! Window list, activation, shell hook messages and attention flags.
use std::collections::HashSet;

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
    QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_LWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, RegisterShellHookWindow, RegisterWindowMessageW, SetForegroundWindow,
    ShowWindow, DeregisterShellHookWindow, GWL_EXSTYLE, GW_OWNER, SW_RESTORE, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::model::WindowEntry;

/// Shell window classes that are never listed.
const SKIP_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// Top-level windows that the taskbar would show, excluding SmartDock's own windows.
pub fn list_windows() -> Vec<WindowEntry> {
    let mut out: Vec<WindowEntry> = Vec::new();
    // SAFETY: the callback runs only during EnumWindows and receives a pointer to `out`, which outlives the call.
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut out as *mut Vec<WindowEntry> as isize),
        );
    }
    out
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the `&mut Vec<WindowEntry>` passed by list_windows.
    let out = &mut *(lparam.0 as *mut Vec<WindowEntry>);
    if let Some(entry) = task_entry(hwnd) {
        out.push(entry);
    }
    BOOL(1)
}

/// Taskbar rules: visible, not owned (unless WS_EX_APPWINDOW), not a tool window, titled, not cloaked.
unsafe fn task_entry(hwnd: HWND) -> Option<WindowEntry> {
    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let app_window = (ex & WS_EX_APPWINDOW.0) != 0;
    let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
    if !app_window && (owned || (ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)) != 0) {
        return None;
    }

    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, &mut buf);
    let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);

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
    Some(WindowEntry {
        hwnd: hwnd.0 as isize,
        title,
        exe: process_image(pid).unwrap_or_default(),
        attention: false,
    })
}

/// Full image path of a process, or None if it cannot be opened.
fn process_image(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed before returning; buf and size describe the same buffer.
    unsafe {
        let process =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL::from(false), pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            Default::default(),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(process);
        ok.then(|| String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Brings a window to the front (restoring it if minimized). Never activates SmartDock.
pub fn activate_window(hwnd: isize) {
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
        let fg_tid = GetWindowThreadProcessId(GetForegroundWindow(), None);
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
