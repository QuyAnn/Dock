//! Small shell readings for the Recycle Bin, Downloads and To-do widgets. The readings are
//! refreshed by the data thread (`refresh`) and read by the UI without blocking.
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHQueryRecycleBinW, SHERB_NOSOUND, SHQUERYRBINFO};

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub bin_items: u64,
    pub bin_bytes: u64,
    /// Files in Downloads changed in the last 24 hours.
    pub downloads_today: u32,
    /// Newest file in Downloads, with its age in seconds.
    pub latest: Option<(String, u64)>,
}

static INFO: Mutex<Info> = Mutex::new(Info { bin_items: 0, bin_bytes: 0, downloads_today: 0, latest: None });

pub fn info() -> Info {
    INFO.lock().unwrap().clone()
}

pub fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    home.join("Downloads")
}

/// Re-reads the bin and the Downloads folder. Blocking disk/shell work: call off the UI thread.
pub fn refresh() {
    let mut next = Info::default();
    let mut q = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: `q` is a valid, sized struct; a null root path means every drive.
    if unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut q) }.is_ok() {
        next.bin_items = q.i64NumItems.max(0) as u64;
        next.bin_bytes = q.i64Size.max(0) as u64;
    }
    let now = SystemTime::now();
    if let Ok(dir) = std::fs::read_dir(downloads_dir()) {
        let mut newest: Option<(SystemTime, String)> = None;
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Browsers keep a partial file under these names while downloading.
            if name.ends_with(".crdownload") || name.ends_with(".tmp") || name.ends_with(".part") || name.starts_with("desktop.ini") {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
            if now.duration_since(modified).is_ok_and(|d| d.as_secs() < 24 * 3600) {
                next.downloads_today += 1;
            }
            if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                newest = Some((modified, name));
            }
        }
        next.latest = newest.map(|(t, n)| (n, now.duration_since(t).map_or(0, |d| d.as_secs())));
    }
    *INFO.lock().unwrap() = next;
}

/// Empties the Recycle Bin; Windows asks for confirmation itself.
pub fn empty_bin() {
    // SAFETY: plain shell call with no owner window.
    unsafe {
        let _ = SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOSOUND);
    }
}

/// Current clipboard text, trimmed to one line of at most 120 characters.
pub fn clipboard_text() -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    // SAFETY: the clipboard is opened and closed on this thread; the global memory is locked
    // only while we copy the NUL-terminated UTF-16 text out of it.
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let handle: HANDLE = GetClipboardData(CF_UNICODETEXT).ok()?;
            let hglobal = windows::Win32::Foundation::HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(hglobal);
            Some(s)
        })();
        let _ = CloseClipboard();
        let line = text?.lines().map(str::trim).find(|l| !l.is_empty())?.chars().take(120).collect::<String>();
        Some(line)
    }
}

/// "340 MB" / "1.2 GB".
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB * KB {
        format!("{:.0} KB", (b / KB).max(0.0))
    } else if b < KB * KB * KB {
        format!("{:.0} MB", b / KB / KB)
    } else {
        format!("{:.1} GB", b / KB / KB / KB)
    }
}

/// "vừa xong", "5 phút", "3 giờ", "2 ngày".
pub fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => "vừa xong".to_owned(),
        60..=3599 => format!("{} phút", secs / 60),
        3600..=86_399 => format!("{} giờ", secs / 3600),
        _ => format!("{} ngày", secs / 86_400),
    }
}

