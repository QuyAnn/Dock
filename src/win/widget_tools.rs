//! Local widget platform operations. Potentially blocking work belongs on worker threads.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime};
use std::os::windows::process::CommandExt;
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
pub fn light_theme() -> Option<bool> {
    let mut value = 0u32; let mut size = 4u32;
    // SAFETY: static key/name and correctly sized output.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, PERSONALIZE, w!("AppsUseLightTheme"), RRF_RT_REG_DWORD,
        None, Some((&mut value as *mut u32).cast()), Some(&mut size)) }.ok().ok()?;
    Some(value != 0)
}
pub fn toggle_theme() -> windows::core::Result<()> {
    let value = u32::from(!light_theme().unwrap_or(true));
    // SAFETY: both DWORD writes use a live u32. Broadcast tells Windows/applications to reload.
    unsafe {
        for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
            RegSetKeyValueW(HKEY_CURRENT_USER, PERSONALIZE, name, REG_DWORD.0,
                Some((&value as *const u32).cast()), 4).ok()?;
        }
        let mut result = 0;
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG, 1000, Some(&mut result));
    }
    Ok(())
}

static BRIGHTNESS: AtomicI32 = AtomicI32::new(-1);
static BRIGHTNESS_REQUEST: AtomicI32 = AtomicI32::new(-1);
pub fn brightness() -> Option<u32> { u32::try_from(BRIGHTNESS.load(Ordering::Relaxed)).ok() }
pub fn request_brightness(value: u32) {
    if brightness().is_some() { BRIGHTNESS_REQUEST.store(value.min(100) as i32, Ordering::Relaxed); }
}
/// Uses the Windows inbox CIM/WMI bridge for internal laptop panels. External panels may
/// legitimately have no WmiMonitorBrightness instance. No DDC/CI or invented fallback values.
pub fn poll_brightness() {
    let requested = BRIGHTNESS_REQUEST.swap(-1, Ordering::Relaxed);
    let set = if requested >= 0 {
        format!("Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods -ErrorAction Stop | Where-Object Active | Invoke-CimMethod -MethodName WmiSetBrightness -Arguments @{{Timeout=[uint32]1;Brightness=[byte]{}}} -ErrorAction Stop | Out-Null;", requested.clamp(0,100))
    } else { String::new() };
    let script = format!("$ErrorActionPreference='Stop';{set}$b=Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness | Where-Object Active | Select-Object -First 1; if($null -eq $b){{exit 2}}; [Console]::Write($b.CurrentBrightness)");
    let Some(root) = std::env::var_os("SystemRoot") else { return };
    let exe = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let child = std::process::Command::new(exe).args(["-NoLogo","-NoProfile","-NonInteractive","-Command",&script])
        .creation_flags(0x08000000).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn();
    let Ok(mut child) = child else { BRIGHTNESS.store(-1, Ordering::Relaxed); return };
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let value = if status.success() { child.wait_with_output().ok().and_then(|o| String::from_utf8(o.stdout).ok())
                    .and_then(|s| s.trim().parse::<i32>().ok()).filter(|v| (0..=100).contains(v)).unwrap_or(-1) } else { -1 };
                BRIGHTNESS.store(value, Ordering::Relaxed); break;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => { let _ = child.kill(); let _ = child.wait(); BRIGHTNESS.store(-1, Ordering::Relaxed); break; }
        }
    }
}
pub fn brightness_pending() -> bool { BRIGHTNESS_REQUEST.load(Ordering::Relaxed) >= 0 }

#[derive(Clone, Debug)]
pub struct Shot { pub path: PathBuf, pub w: u32, pub h: u32, pub rgba: Arc<Vec<u8>> }
static SHOTS: Mutex<Vec<Shot>> = Mutex::new(Vec::new());
pub fn shots() -> Vec<Shot> { SHOTS.lock().unwrap_or_else(|e| e.into_inner()).clone() }
pub fn screenshots_dir() -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Screenshots, KF_FLAG_DEFAULT};
    use windows::Win32::System::Com::CoTaskMemFree;
    // SAFETY: shell allocates a terminated path; copy before freeing with COM's allocator.
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Screenshots, KF_FLAG_DEFAULT, None) {
            let result = p.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(p.0.cast()));
            if result.is_some() { return result; }
        }
    }
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures/Screenshots"))
}
pub fn refresh_shots() {
    let Some(dir) = screenshots_dir() else { return };
    let mut files: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| {
        let path = e.path();
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !["png","jpg","jpeg","bmp"].contains(&ext.as_str()) { return None; }
        let meta = e.metadata().ok()?;
        meta.is_file().then_some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), path))
    }).collect();
    files.sort_by(|a,b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    files.truncate(3);
    // Cache decoded thumbnails; avoid decoding unchanged files every five seconds.
    static SIGNATURE: Mutex<Vec<(SystemTime,u64,PathBuf)>> = Mutex::new(Vec::new());
    if *SIGNATURE.lock().unwrap_or_else(|e| e.into_inner()) == files { return; }
    let next = files.iter().filter_map(|(_,_,path)| {
        let (w,h,rgba) = super::wallpaper::thumbnail(path, 320).ok()?;
        Some(Shot { path: path.clone(), w, h, rgba: Arc::new(rgba) })
    }).collect();
    *SHOTS.lock().unwrap_or_else(|e| e.into_inner()) = next;
    *SIGNATURE.lock().unwrap_or_else(|e| e.into_inner()) = files;
}

/// Percent-encode UTF-8 bytes, including shell punctuation. Never build a command line from input.
pub fn search_url(query: &str) -> String {
    let encoded: String = query.trim().bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
        (b as char).to_string()
    } else { format!("%{b:02X}") }).collect();
    format!("https://www.google.com/search?q={encoded}")
}
pub fn search(query: &str) -> windows::core::Result<()> {
    let path = PathBuf::from(query.trim().trim_matches('"'));
    // A folder is the only local target accepted. Other text always becomes a web query.
    if path.is_dir() { super::system::open_path(&path) }
    else { super::system::open_path(std::path::Path::new(&search_url(query))) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_encodes_unicode_and_url_delimiters() {
        assert_eq!(search_url(" a &b# "), "https://www.google.com/search?q=a%20%26b%23");
        assert!(search_url("Việt").contains("Vi%E1%BB%87t"));
        assert!(search_url("ms-settings:").ends_with("ms-settings%3A"));
    }
}
