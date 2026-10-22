//! Grok Bot desktop app launcher tile helpers.
//!
//! Resolves the installed exe / Start-menu shortcut / running process for icon + open/focus.
//! The `%USERPROFILE%\.grokbot` folder holds config/daemon state only — never read, open, or
//! log credential / connection / daemon JSON from that tree.
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::icons::{icon_for_exe, IconBitmap};
use super::tasks::{activate_window, list_windows, snapshot_process_exes};

/// Cached install path (exe or .lnk). Cleared when the file disappears.
static CACHED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Display name for the tile / settings.
pub const DISPLAY_NAME: &str = "Grok Bot";

/// Best-effort path to the Grok Bot launcher (exe or Start-menu .lnk). Never touches `.grokbot`.
pub fn resolve_path() -> Option<PathBuf> {
    if let Ok(guard) = CACHED.lock() {
        if let Some(path) = guard.as_ref() {
            if path.is_file() {
                return Some(path.clone());
            }
        }
    }
    let found = candidates().into_iter().find(|p| p.is_file());
    if let Ok(mut guard) = CACHED.lock() {
        *guard = found.clone();
    }
    found
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("Programs");
        out.push(root.join("Grok Bot").join("Grok Bot.exe"));
        out.push(root.join("GrokBot").join("GrokBot.exe"));
        out.push(root.join("Grok Bot").join("GrokBot.exe"));
    }
    // Start Menu shortcuts: launch via the .lnk; SHGetFileInfo resolves its icon.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let programs = PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        push_start_menu_hits(&programs, &mut out);
    }
    if let Some(program_data) = std::env::var_os("ProgramData") {
        let programs = PathBuf::from(program_data)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        push_start_menu_hits(&programs, &mut out);
    }
    // Running process image (do not inspect %USERPROFILE%\.grokbot).
    if let Some(exe) = running_exe_path() {
        out.push(exe);
    }
    out
}

fn push_start_menu_hits(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            push_start_menu_hits(&path, out);
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.ends_with(".lnk") && name.contains("grok") && name.contains("bot") {
            out.push(path);
        }
    }
}

fn running_exe_path() -> Option<PathBuf> {
    snapshot_process_exes()
        .into_iter()
        .find(|p| is_grok_bot_exe(p))
}

fn is_grok_bot_exe(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace([' ', '_'], "");
    name == "grokbot.exe"
}

/// True when a taskbar-listed window belongs to Grok Bot.
pub fn is_running() -> bool {
    list_windows()
        .iter()
        .any(|w| is_grok_bot_exe(Path::new(&w.exe)))
        || running_exe_path().is_some()
}

/// HWND of a visible Grok Bot window, if any.
pub fn running_hwnd() -> Option<isize> {
    list_windows()
        .into_iter()
        .find(|w| is_grok_bot_exe(Path::new(&w.exe)))
        .map(|w| w.hwnd)
}

/// App icon for the tile (exe / shortcut / running process). Falls back to None for the SVG glyph.
pub fn icon() -> Option<IconBitmap> {
    let path = resolve_path()?;
    let s = path.to_string_lossy();
    icon_for_exe(&s, "", "")
}

/// Opens Grok Bot if needed, otherwise focuses an existing window.
pub fn open_or_focus() {
    if let Some(hwnd) = running_hwnd() {
        activate_window(hwnd);
        return;
    }
    let Some(path) = resolve_path() else {
        return;
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: null-terminated path; ShellExecuteW does not take ownership.
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

/// Probe that a PID's image path can be queried (used only for running-process discovery).
#[allow(dead_code)]
fn exe_for_pid(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL::from(false), pid).ok()?;
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
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..size as usize])))
    }
}
