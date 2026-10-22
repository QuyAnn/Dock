//! App usage tracking for auto-pinned dock launchers.
//!
//! Foreground time and activation counts per exe are tracked locally from the
//! EVENT_SYSTEM_FOREGROUND hook and persisted to %APPDATA%\SmartDock\usage.json. On first run
//! the table is seeded from Windows' own UserAssist counters (HKCU, ROT13 names, run count +
//! focus time), resolved to existing .exe paths (.lnk shortcuts are resolved to their target).
//! The ranking is recomputed only at start-up and at most once a day, so the dock never
//! reshuffles while the user is looking. Unpinned auto pins are remembered in settings.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::settings::Settings;

const FILE: &str = "usage.json";
/// Ranked candidates kept on disk (the dock shows the first `auto_pin_count` that qualify).
const RANK_KEEP: usize = 24;
/// One foreground stretch counts at most this long (idle / away time is not usage).
const MAX_STRETCH_SECS: f64 = 30.0 * 60.0;
const DAY_SECS: u64 = 24 * 3600;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct AppUsage {
    exe: String,
    focus_secs: f64,
    launches: u32,
    last: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct UsageFile {
    apps: HashMap<String, AppUsage>,
    seeded: bool,
    ranked_at: u64,
    ranking: Vec<String>,
}

struct State {
    file: UsageFile,
    current: Option<(String, Instant)>,
    seen_pids: HashSet<u32>,
    dirty: bool,
    last_save: Instant,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("SmartDock").join(FILE))
}

/// Loads usage.json (seeding from UserAssist on first run) and ranks once. Call at start-up.
pub fn init() {
    let mut file: UsageFile = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if !file.seeded {
        let seeded = seed_from_user_assist();
        crate::log(&format!("autopin: seeded {} apps from UserAssist", seeded.len()));
        for (key, u) in seeded {
            file.apps.entry(key).or_insert(u);
        }
        file.seeded = true;
    }
    rank(&mut file);
    *STATE.lock().unwrap() = Some(State {
        file,
        current: None,
        seen_pids: HashSet::new(),
        dirty: true,
        last_save: Instant::now(),
    });
    save_if_due(true);
}

/// Foreground observer (from the WinEvent hook, UI thread): closes the previous stretch and
/// starts one for the new exe. Counts a launch the first time a process gets the foreground.
pub fn on_foreground(hwnd: isize) {
    let mut pid = 0u32;
    // SAFETY: plain query on a possibly stale handle.
    unsafe { GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut pid)) };
    if pid == 0 {
        return;
    }
    let exe = process_exe(pid);
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    close_stretch(st);
    if exe.is_empty() || skip(&exe) {
        return;
    }
    let key = exe.to_ascii_lowercase();
    let entry = st.file.apps.entry(key.clone()).or_insert_with(|| AppUsage { exe: exe.clone(), ..Default::default() });
    entry.exe = exe;
    entry.last = now();
    if st.seen_pids.insert(pid) {
        entry.launches += 1;
    }
    st.current = Some((key, Instant::now()));
    st.dirty = true;
}

fn close_stretch(st: &mut State) {
    if let Some((key, since)) = st.current.take() {
        let secs = since.elapsed().as_secs_f64().min(MAX_STRETCH_SECS);
        if let Some(u) = st.file.apps.get_mut(&key) {
            u.focus_secs += secs;
            st.dirty = true;
        }
    }
}

/// Call about once a second (refresh timer): saves at most once a minute and re-ranks at
/// most once a day.
pub fn tick() {
    {
        let Ok(mut g) = STATE.lock() else { return };
        let Some(st) = g.as_mut() else { return };
        if now().saturating_sub(st.file.ranked_at) >= DAY_SECS {
            // Fold the running stretch in before ranking.
            let cur = st.current.clone();
            close_stretch(st);
            if let Some((k, _)) = cur {
                st.current = Some((k, Instant::now()));
            }
            rank(&mut st.file);
            st.dirty = true;
        }
    }
    save_if_due(false);
}

/// Saves now (shutdown).
pub fn flush() {
    if let Ok(mut g) = STATE.lock() {
        if let Some(st) = g.as_mut() {
            let cur = st.current.clone();
            close_stretch(st);
            st.current = cur.map(|(k, _)| (k, Instant::now()));
        }
    }
    save_if_due(true);
}

fn save_if_due(force: bool) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    if !st.dirty || (!force && st.last_save.elapsed().as_secs() < 60) {
        return;
    }
    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&st.file) {
            let _ = std::fs::write(p, text);
        }
    }
    st.dirty = false;
    st.last_save = Instant::now();
}

fn score(u: &AppUsage) -> f64 {
    u.focus_secs / 60.0 + u.launches as f64 * 2.0
}

fn rank(file: &mut UsageFile) {
    let mut list: Vec<&AppUsage> = file
        .apps
        .values()
        .filter(|u| !skip(&u.exe) && Path::new(&u.exe).is_file())
        .collect();
    list.sort_by(|a, b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal).then(a.exe.cmp(&b.exe)));
    file.ranking = list.iter().take(RANK_KEEP).map(|u| u.exe.clone()).collect();
    file.ranked_at = now();
    let names: Vec<String> = file.ranking.iter().take(12).map(|e| exe_name(e)).collect();
    crate::log(&format!("autopin: ranking=[{}]", names.join(", ")));
}

/// Auto pins for the dock: (exe, display name), stable between re-ranks. Empty when disabled.
/// Skips manual pins (they come first anyway) and apps the user unpinned.
pub fn auto_pins(settings: &Settings) -> Vec<(String, String)> {
    if !settings.auto_pin {
        return Vec::new();
    }
    let Ok(g) = STATE.lock() else { return Vec::new() };
    let Some(st) = g.as_ref() else { return Vec::new() };
    st.file
        .ranking
        .iter()
        .filter(|exe| !settings.auto_pin_hidden.iter().any(|h| h.eq_ignore_ascii_case(exe)))
        .filter(|exe| !settings.dock_pins.iter().any(|p| p.exe.eq_ignore_ascii_case(exe)))
        .take(settings.auto_pin_count as usize)
        .map(|exe| (exe.clone(), display_name(exe)))
        .collect()
}

/// True when `exe` is currently shown as an auto pin.
pub fn is_auto_pinned(exe: &str, settings: &Settings) -> bool {
    auto_pins(settings).iter().any(|(e, _)| e.eq_ignore_ascii_case(exe))
}

fn exe_name(exe: &str) -> String {
    Path::new(exe).file_name().and_then(|n| n.to_str()).unwrap_or(exe).to_owned()
}

fn display_name(exe: &str) -> String {
    let stem = Path::new(exe).file_stem().and_then(|n| n.to_str()).unwrap_or(exe);
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => stem.to_owned(),
    }
}

/// System / background / installer exes that must never be auto-pinned.
pub fn skip(exe: &str) -> bool {
    let p = exe.to_ascii_lowercase().replace('/', "\\");
    if !p.ends_with(".exe") {
        return true;
    }
    let name = p.rsplit('\\').next().unwrap_or(&p).to_owned();
    const NAMES: [&str; 34] = [
        "smartdock.exe", "applicationframehost.exe", "searchhost.exe", "searchapp.exe",
        "startmenuexperiencehost.exe", "shellexperiencehost.exe", "textinputhost.exe", "lockapp.exe",
        "rundll32.exe", "dllhost.exe", "consent.exe", "msiexec.exe", "conhost.exe", "openwith.exe",
        "sihost.exe", "ctfmon.exe", "runtimebroker.exe", "systemsettingsbroker.exe", "werfault.exe",
        "mmc.exe", "control.exe", "pickerhost.exe", "credentialuibroker.exe", "useroobebroker.exe",
        "windowspackagemanagerserver.exe", "appinstaller.exe", "smartscreen.exe", "taskhostw.exe",
        "explorer_helper.exe", "crashpad_handler.exe", "elevation_service.exe", "notification_helper.exe",
        "grok bot helper.exe", "msedgewebview2.exe",
    ];
    if NAMES.contains(&name.as_str()) {
        return true;
    }
    const PATTERNS: [&str; 9] = ["setup", "install", "update", "uninst", "unins0", "helper", "crash", "redist", "vc_redist"];
    if PATTERNS.iter().any(|k| name.contains(k)) {
        return true;
    }
    // Temp / download folders hold installers and one-offs, not apps.
    p.contains("\\appdata\\local\\temp\\") || p.contains("\\downloads\\") || p.contains("\\windows\\installer\\")
}

fn process_exe(pid: u32) -> String {
    // SAFETY: the handle is closed before returning.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(h);
        if ok {
            String::from_utf16_lossy(&buf[..size as usize])
        } else {
            String::new()
        }
    }
}

// ---------------------------------------------------------------- UserAssist seed

/// UserAssist GUIDs: executables and shortcuts.
const UA_KEYS: [&str; 2] = [
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}\\Count",
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{F4E57C4B-2036-45F0-A9AB-443BCFE33D9F}\\Count",
];

fn rot13(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            _ => c,
        })
        .collect()
}

/// Known-folder GUID prefixes used in UserAssist names.
fn known_folder(guid: &str) -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let g = guid.to_ascii_uppercase();
    match g.as_str() {
        "{6D809377-6AF0-444B-8957-A3773F02200E}" => env("ProgramW6432").or_else(|| env("ProgramFiles")),
        "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}" => env("ProgramFiles(x86)"),
        "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}" => env("windir").map(|w| w.join("System32")),
        "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}" => env("windir").map(|w| w.join("SysWOW64")),
        "{F38BF404-1D43-42F2-9305-67DE0B28FC23}" => env("windir"),
        "{62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}" => env("ProgramData"),
        "{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}" => env("LOCALAPPDATA"),
        "{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}" => env("APPDATA"),
        "{5E6C858F-0E22-4760-9AFE-EA3317B67173}" => env("USERPROFILE"),
        "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}" => env("APPDATA").map(|a| a.join("Microsoft\\Windows\\Start Menu\\Programs")),
        "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}" => env("ProgramData").map(|a| a.join("Microsoft\\Windows\\Start Menu\\Programs")),
        "{9E3995AB-1F9C-4F13-B827-48B24B6C7174}" => env("APPDATA").map(|a| a.join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned")),
        "{B4BFCC3A-DB2C-424C-B029-7FE99A87C641}" => env("USERPROFILE").map(|a| a.join("Desktop")),
        _ => None,
    }
}

/// Expands a decoded UserAssist name ("{GUID}\\rest" or an absolute path) to a path.
fn expand_name(name: &str) -> Option<PathBuf> {
    if name.starts_with('{') {
        let end = name.find('}')?;
        let base = known_folder(&name[..=end])?;
        let rest = name[end + 1..].trim_start_matches('\\');
        return Some(base.join(rest));
    }
    let p = PathBuf::from(name);
    (p.is_absolute()).then_some(p)
}

/// Target exe of a .lnk shortcut (empty when it does not point at an exe).
fn resolve_lnk(lnk: &Path) -> Option<String> {
    let wide: Vec<u16> = lnk.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    // SAFETY: COM objects are used and released on this thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = String::from_utf16_lossy(&buf[..n]);
        (target.to_ascii_lowercase().ends_with(".exe")).then_some(target)
    }
}

fn seed_from_user_assist() -> HashMap<String, AppUsage> {
    let mut out: HashMap<String, AppUsage> = HashMap::new();
    for key in UA_KEYS {
        let wide: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let mut hkey = HKEY::default();
        // SAFETY: valid key path; hkey closed below.
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(wide.as_ptr()), 0, KEY_READ, &mut hkey) } != ERROR_SUCCESS {
            continue;
        }
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 1024];
            let mut name_len = name.len() as u32;
            let mut data = [0u8; 128];
            let mut data_len = data.len() as u32;
            // SAFETY: buffers and lengths describe valid local arrays.
            let rc = unsafe {
                RegEnumValueW(
                    hkey,
                    index,
                    PWSTR(name.as_mut_ptr()),
                    &mut name_len,
                    None,
                    None,
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            index += 1;
            if rc != ERROR_SUCCESS {
                break;
            }
            if data_len < 20 {
                continue;
            }
            let decoded = rot13(&String::from_utf16_lossy(&name[..name_len as usize]));
            let runs = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            let focus_ms = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
            let Some(p) = expand_name(&decoded) else { continue };
            let exe = if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                match resolve_lnk(&p) {
                    Some(t) => t,
                    None => continue,
                }
            } else {
                p.to_string_lossy().into_owned()
            };
            if skip(&exe) || !Path::new(&exe).is_file() {
                continue;
            }
            let k = exe.to_ascii_lowercase();
            let e = out.entry(k).or_insert_with(|| AppUsage { exe: exe.clone(), ..Default::default() });
            // Windows' totals are long-term; weight them down so local use soon dominates.
            e.focus_secs += focus_ms as f64 / 1000.0 * 0.5;
            e.launches += runs.min(500);
        }
        // SAFETY: opened above.
        unsafe {
            let _ = RegCloseKey(hkey);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_layout_autopin_skips_system_and_installers() {
        assert!(skip("C:\\Windows\\System32\\rundll32.exe"));
        assert!(skip("C:\\Users\\a\\Downloads\\ChromeSetup.exe"));
        assert!(skip("C:\\Program Files\\App\\Update.exe"));
        assert!(skip("C:\\Program Files\\SmartDock\\smartdock.exe"));
        assert!(skip("C:\\x\\notes.txt"));
        assert!(!skip("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"));
        assert!(!skip("C:\\Windows\\explorer.exe"));
        assert_eq!(rot13("Pbqr.rkr"), "Code.exe");
    }
}
