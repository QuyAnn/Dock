//! SmartDock's own `Shell_TrayWnd` listener (the RetroBar / Cairo ManagedShell `TrayService`
//! technique), so the dock can show the notification-area icons while Explorer's taskbar stays
//! hidden.
//!
//! `Shell_NotifyIcon` in every app does `FindWindow("Shell_TrayWnd")` and sends WM_COPYDATA to
//! it. We create a hidden, topmost window of that class (plus a `TrayNotifyWnd` child) after
//! Explorer's, so the lookup finds ours first. Each message is parsed to learn the icon
//! (owner hwnd, uID, callback message, icon, tooltip, version, state) and then forwarded
//! unchanged to Explorer's real `Shell_TrayWnd`, so the Windows tray keeps working exactly as
//! before. AppBar (dwData 0), icon-rect queries (dwData 3) and every other message are
//! forwarded too. On start we broadcast `TaskbarCreated` so running apps re-register their
//! icons; on exit we destroy our window and broadcast it again.
//!
//! Clicking an icon sends what the shell sends: the icon's own callback message to its owner
//! window (NOTIFYICON_VERSION_4 semantics when the app asked for them). No synthetic input.
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use std::time::Duration;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_BINARY,
    REG_DWORD, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, DrawIconEx, FindWindowExW, FindWindowW, GetMessageW, GetSystemMetrics, GetWindowLongW,
    GetWindowThreadProcessId, IsWindow, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SendMessageTimeoutW, SendNotifyMessageW, SetTimer, SetWindowLongW, SetWindowPos, TranslateMessage, DI_NORMAL,
    GWL_STYLE, HICON, HWND_BROADCAST, HWND_MESSAGE, HWND_TOPMOST, MSG, MSGFLT_ALLOW, SMTO_ABORTIFHUNG, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_ACTIVATEAPP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_COPYDATA, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WM_USER,
    WM_WINDOWPOSCHANGED, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WS_VISIBLE,
};

use crate::win::icons::IconBitmap;

/// One notification-area icon as the shell knows it.
#[derive(Clone, Debug)]
pub struct TrayIcon {
    pub hwnd: isize,
    pub uid: u32,
    pub guid: Option<[u8; 16]>,
    pub callback: u32,
    /// 0 (legacy), 3 or 4 (NOTIFYICON_VERSION_4).
    pub version: u32,
    pub tip: String,
    /// NIS_HIDDEN: the app asked for the icon to be hidden.
    pub hidden: bool,
    /// Owner process image path.
    pub path: String,
    pub icon: Option<IconBitmap>,
}

/// Mouse action to forward to an icon's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Left,
    Double,
    Right,
}

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_STATE: u32 = 0x8;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x1;
const NIN_SELECT: u32 = WM_USER;
/// Explorer expects this one posted, not sent (ManagedShell `ForwardMessagesPost`).
const WM_USER_372: u32 = WM_USER + 372;
const TIMER_RAISE: usize = 1;
const ICON_PX: i32 = 32;

static ICONS: Mutex<Vec<TrayIcon>> = Mutex::new(Vec::new());
/// Bumped on every icon change; the popup repaints when it moves.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static OUR_TRAY: AtomicIsize = AtomicIsize::new(0);
static EXPLORER_TRAY: AtomicIsize = AtomicIsize::new(0);
static STOPPED: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
/// Where apps should place their flyouts (Shell_NotifyIconGetRect): the dock's ^ key.
static ANCHOR: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
static RAISES: AtomicU64 = AtomicU64::new(0);
static DEPTH: AtomicIsize = AtomicIsize::new(0);

fn ts() -> String {
    chrono::Local::now().format("[%H:%M:%S%.3f]").to_string()
}

/// Starts the listener thread (idempotent). Failures are logged; the dock keeps working and
/// the popup then simply has no icons.
static THREAD_ALIVE: AtomicBool = AtomicBool::new(false);
static OWN_BROADCAST_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// True for a few seconds after we broadcast TaskbarCreated ourselves, so the dock does not
/// mistake our own broadcast for an Explorer restart.
pub fn own_broadcast_recent() -> bool {
    now_ms().saturating_sub(OWN_BROADCAST_MS.load(Ordering::SeqCst)) < 4000
}

/// Starts the listener thread (again, after a shutdown) unless it is already running.
pub fn start() {
    if THREAD_ALIVE.swap(true, Ordering::SeqCst) {
        return;
    }
    STOPPED.store(false, Ordering::SeqCst);
    FAILED.store(false, Ordering::SeqCst);
    let spawned = std::thread::Builder::new().name("tray-svc".into()).stack_size(512 * 1024).spawn(|| {
        run();
        OUR_TRAY.store(0, Ordering::SeqCst);
        THREAD_ALIVE.store(false, Ordering::SeqCst);
    });
    if let Err(e) = spawned {
        crate::log(&format!("tray: cannot start listener thread: {e}"));
        FAILED.store(true, Ordering::SeqCst);
        THREAD_ALIVE.store(false, Ordering::SeqCst);
    }
}

/// The listener window exists.
pub fn running() -> bool {
    OUR_TRAY.load(Ordering::SeqCst) != 0 && !STOPPED.load(Ordering::SeqCst)
}

/// Snapshot of the known icons plus the generation it belongs to.
pub fn icons() -> (u64, Vec<TrayIcon>) {
    let list = ICONS.lock().map(|l| l.clone()).unwrap_or_default();
    (GENERATION.load(Ordering::SeqCst), list)
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// True if our listener could not be installed (popup shows a short note instead).
pub fn failed() -> bool {
    FAILED.load(Ordering::SeqCst)
}

/// Screen rect (physical px) that answers Shell_NotifyIconGetRect for our icons.
pub fn set_anchor(left: i32, top: i32, right: i32, bottom: i32) {
    if let Ok(mut a) = ANCHOR.lock() {
        *a = Some((left, top, right, bottom));
    }
}

/// Re-takes the first `FindWindow("Shell_TrayWnd")` slot if Explorer's tray got above ours.
pub fn ensure_first() {
    let ours = OUR_TRAY.load(Ordering::SeqCst);
    if ours == 0 {
        return;
    }
    // SAFETY: plain lookup / z-order change on our own hidden window.
    unsafe {
        let first = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default();
        if first.0 as isize != ours {
            let _ = SetWindowPos(HWND(ours as _), HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            let n = RAISES.fetch_add(1, Ordering::SeqCst);
            if n < 5 || n % 100 == 0 {
                crate::log(&format!("{} tray: Explorer's Shell_TrayWnd was first; raised ours (#{})", ts(), n + 1));
            }
        }
    }
}

/// Destroys our window and broadcasts TaskbarCreated so apps re-register with Explorer.
/// Safe from any thread; waits briefly for the listener thread.
pub fn shutdown() {
    let ours = OUR_TRAY.load(Ordering::SeqCst);
    if ours == 0 || STOPPED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: our own window; the listener thread destroys it and broadcasts.
    unsafe {
        let _ = PostMessageW(HWND(ours as _), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    for _ in 0..40 {
        if OUR_TRAY.load(Ordering::SeqCst) == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn hinstance() -> windows::Win32::Foundation::HINSTANCE {
    // SAFETY: NULL asks for our own module.
    let m = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    windows::Win32::Foundation::HINSTANCE(m.0)
}

fn broadcast_taskbar_created(why: &str) {
    // SAFETY: registered message broadcast, non-blocking.
    unsafe {
        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        if msg != 0 {
            OWN_BROADCAST_MS.store(now_ms(), Ordering::SeqCst);
            let ok = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0)).is_ok();
            // Broadcasts skip message-only windows, which some apps use for their icon
            // (e.g. NVIDIA's NotificationIconWindowClass): post to those directly.
            let me = std::process::id();
            let mut posted = 0;
            let mut after = HWND::default();
            while let Ok(h) = FindWindowExW(HWND_MESSAGE, after, PCWSTR::null(), PCWSTR::null()) {
                if h.0.is_null() || posted > 4000 {
                    break;
                }
                after = h;
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                if pid != me && PostMessageW(h, msg, WPARAM(0), LPARAM(0)).is_ok() {
                    posted += 1;
                }
            }
            crate::log(&format!("{} tray: TaskbarCreated broadcast ({why}) ok={ok}, +{posted} message-only windows", ts()));
        }
    }
}

fn run() {
    let hinst = hinstance();
    // SAFETY: classes and windows owned by this thread; strings are static literals.
    let tray = unsafe {
        for class in [w!("Shell_TrayWnd"), w!("TrayNotifyWnd")] {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst,
                lpszClassName: class,
                style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0x8),
                ..Default::default()
            };
            // A restarted listener finds its classes still registered: that is fine.
            if RegisterClassW(&wc) == 0
                && windows::Win32::Foundation::GetLastError() != windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS
            {
                crate::log("tray: RegisterClass failed; tray icons unavailable");
                FAILED.store(true, Ordering::SeqCst);
                return;
            }
        }
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let tray = match CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("Shell_TrayWnd"),
            w!(""),
            WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            sh - 48,
            sw,
            48,
            None,
            None,
            hinst,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                crate::log(&format!("tray: cannot create Shell_TrayWnd: {e}"));
                FAILED.store(true, Ordering::SeqCst);
                return;
            }
        };
        let _ = CreateWindowExW(
            Default::default(),
            w!("TrayNotifyWnd"),
            PCWSTR::null(),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            sw,
            48,
            tray,
            None,
            hinst,
            None,
        );
        // Medium/low integrity apps must be able to reach us like they reach Explorer.
        let _ = ChangeWindowMessageFilterEx(tray, WM_COPYDATA, MSGFLT_ALLOW, None);
        OUR_TRAY.store(tray.0 as isize, Ordering::SeqCst);
        SetTimer(tray, TIMER_RAISE, 100, None);
        tray
    };
    ensure_first();
    let explorer = explorer_tray();
    crate::log(&format!(
        "{} tray: listener up (ours {:#x}, Explorer's {:#x}); FindWindow first = ours: {}",
        ts(),
        tray.0 as isize,
        explorer.map_or(0, |h| h.0 as isize),
        unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.is_ok_and(|h| h == tray)
    ));
    broadcast_taskbar_created("start");
    // Summarise after apps had a moment to re-register.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(4));
        log_summary("after start");
        // Some apps are busy at the first broadcast (or only listen once their UI is up):
        // ask once more, then summarise again.
        std::thread::sleep(Duration::from_secs(8));
        if running() {
            broadcast_taskbar_created("retry");
            std::thread::sleep(Duration::from_secs(4));
            log_summary("after retry");
        }
    });

    let mut msg = MSG::default();
    // SAFETY: standard message loop for this thread's windows.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    OUR_TRAY.store(0, Ordering::SeqCst);
    broadcast_taskbar_created("exit");
}

/// Logs how many icons we know, with their tooltips' first words (no other data).
pub fn log_summary(when: &str) {
    let (_, list) = icons();
    let names: Vec<String> =
        list.iter().map(|i| format!("{}{}", short_name(i), if i.callback == 0 { " [no callback yet]" } else { "" })).collect();
    crate::log(&format!("{} tray: {} icon(s) known {when}: {}", ts(), list.len(), names.join(" | ")));
}

/// A short display label: first tooltip line, else the exe stem.
pub fn short_name(i: &TrayIcon) -> String {
    let tip = i.tip.lines().next().unwrap_or("").trim();
    let base = if tip.is_empty() {
        std::path::Path::new(&i.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        tip.to_owned()
    };
    base.chars().take(40).collect()
}

/// Explorer's real Shell_TrayWnd (cached; re-resolved when it goes away).
fn explorer_tray() -> Option<HWND> {
    let cached = EXPLORER_TRAY.load(Ordering::SeqCst);
    // SAFETY: IsWindow on a possibly stale handle is fine.
    if cached != 0 && unsafe { IsWindow(HWND(cached as _)).as_bool() } {
        return Some(HWND(cached as _));
    }
    let me = std::process::id();
    let mut after = HWND::default();
    // SAFETY: top-level class search; pid query on live windows.
    while let Ok(h) = unsafe { FindWindowExW(None, after, w!("Shell_TrayWnd"), PCWSTR::null()) } {
        if h.0.is_null() {
            break;
        }
        after = h;
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid != me {
            EXPLORER_TRAY.store(h.0 as isize, Ordering::SeqCst);
            return Some(h);
        }
    }
    None
}

fn forward(msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(target) = explorer_tray() else { return LRESULT(0) };
    // SAFETY: forwarding to Explorer's window. WM_COPYDATA is re-marshalled by the system
    // from our (still valid) copy of the sender's data.
    unsafe {
        if msg == WM_USER_372 {
            let _ = PostMessageW(target, msg, wp, lp);
            return LRESULT(0);
        }
        let mut result = 0usize;
        let ok = SendMessageTimeoutW(target, msg, wp, lp, SMTO_ABORTIFHUNG, 4000, Some(&mut result));
        if ok.0 == 0 {
            return LRESULT(0);
        }
        LRESULT(result as isize)
    }
}

#[repr(C)]
struct CopyData {
    dw_data: usize,
    cb_data: u32,
    lp_data: *const u8,
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_wstr(b: &[u8], off: usize, chars: usize) -> String {
    let Some(s) = b.get(off..off + chars * 2) else { return String::new() };
    let w: Vec<u16> = s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&w)
}

/// User handles are 32-bit values sign-extended on 64-bit Windows.
fn handle32(v: u32) -> isize {
    v as i32 as isize
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let is_main = hwnd.0 as isize == OUR_TRAY.load(Ordering::SeqCst);
    match msg {
        WM_TIMER if wp.0 == TIMER_RAISE => {
            ensure_first();
            return LRESULT(0);
        }
        WM_CLOSE if is_main => {
            // SAFETY: our own window on its own thread.
            unsafe {
                let _ = DestroyWindow(hwnd);
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        WM_WINDOWPOSCHANGED if is_main => {
            // Someone (ShowWindow on "the taskbar") showed us: stay invisible.
            // SAFETY: lp points at a WINDOWPOS for this message.
            let pos = unsafe { &*(lp.0 as *const WINDOWPOS) };
            if pos.flags.0 & windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW.0 != 0 {
                unsafe {
                    let style = GetWindowLongW(hwnd, GWL_STYLE);
                    SetWindowLongW(hwnd, GWL_STYLE, style & !(WS_VISIBLE.0 as i32));
                }
                crate::log("tray: our Shell_TrayWnd was shown by someone; kept hidden");
            }
        }
        WM_COPYDATA if lp.0 != 0 => {
            // Guard against a forwarding loop (Explorer sending back to "the tray").
            if DEPTH.fetch_add(1, Ordering::SeqCst) > 2 {
                DEPTH.fetch_sub(1, Ordering::SeqCst);
                return LRESULT(0);
            }
            let result = on_copydata(wp, lp);
            DEPTH.fetch_sub(1, Ordering::SeqCst);
            return result;
        }
        _ => {}
    }
    if msg == WM_COPYDATA || msg == WM_ACTIVATEAPP || msg == WM_COMMAND || msg >= WM_USER {
        return forward(msg, wp, lp);
    }
    // SAFETY: default handling for our own window.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn on_copydata(wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: lp is a COPYDATASTRUCT for WM_COPYDATA, valid during this call.
    let cds = unsafe { &*(lp.0 as *const CopyData) };
    let bytes: &[u8] = if cds.lp_data.is_null() || cds.cb_data == 0 {
        &[]
    } else {
        // SAFETY: the system guarantees cb_data readable bytes at lp_data.
        unsafe { std::slice::from_raw_parts(cds.lp_data, cds.cb_data as usize) }
    };
    match cds.dw_data {
        1 => {
            let ours = std::panic::catch_unwind(|| on_tray_data(bytes)).unwrap_or(false);
            let theirs = forward(WM_COPYDATA, wp, lp);
            // Explorer already knows icons re-added after our TaskbarCreated: success if
            // either side accepted it.
            LRESULT((ours || theirs.0 != 0) as isize)
        }
        3 => {
            // Shell_NotifyIconGetRect: our icons live behind the dock's ^ key.
            if let Some(r) = icon_rect_answer(bytes) {
                return r;
            }
            forward(WM_COPYDATA, wp, lp)
        }
        _ => forward(WM_COPYDATA, wp, lp),
    }
}

/// WINNOTIFYICONIDENTIFIER: magic, message (1 = top-left, 2 = bottom-right), cbSize, pad,
/// hWnd (32-bit), uID, guid.
fn icon_rect_answer(b: &[u8]) -> Option<LRESULT> {
    let which = rd_u32(b, 4)?;
    let hwnd = handle32(rd_u32(b, 16)?);
    let uid = rd_u32(b, 20)?;
    let guid: Option<[u8; 16]> = b.get(24..40).map(|g| g.try_into().unwrap()).filter(|g: &[u8; 16]| g.iter().any(|&x| x != 0));
    let known = ICONS.lock().ok()?.iter().any(|i| same(i, hwnd, uid, guid));
    if !known {
        return None;
    }
    let (l, t, r, btm) = (*ANCHOR.lock().ok()?)?;
    let (x, y) = if which == 1 { (l, t) } else { (r, btm) };
    Some(LRESULT(((x as u16 as u32) | ((y as u16 as u32) << 16)) as i32 as isize))
}

fn same(i: &TrayIcon, hwnd: isize, uid: u32, guid: Option<[u8; 16]>) -> bool {
    match (guid, i.guid) {
        (Some(g), Some(h)) => g == h,
        _ => i.hwnd == hwnd && i.uid == uid,
    }
}

/// SHELLTRAYDATA: dwSignature, dwMessage, then NOTIFYICONDATAW with 32-bit handles:
/// +8 cbSize, +12 hWnd, +16 uID, +20 uFlags, +24 uCallbackMessage, +28 hIcon, +32 szTip[128],
/// +288 dwState, +292 dwStateMask, +296 szInfo[256], +808 uVersion, +812 szInfoTitle[64],
/// +940 dwInfoFlags, +944 guidItem.
fn on_tray_data(b: &[u8]) -> bool {
    let Some(message) = rd_u32(b, 4) else { return false };
    let hwnd = handle32(rd_u32(b, 12).unwrap_or(0));
    let uid = rd_u32(b, 16).unwrap_or(0);
    let flags = rd_u32(b, 20).unwrap_or(0);
    let guid: Option<[u8; 16]> = if flags & NIF_GUID != 0 {
        b.get(944..960).map(|g| g.try_into().unwrap())
    } else {
        None
    };
    if hwnd == 0 && (guid.is_none() || message == NIM_ADD) {
        return false;
    }
    let Ok(mut list) = ICONS.lock() else { return false };
    let pos = list.iter().position(|i| same(i, hwnd, uid, guid));
    let ok = match message {
        NIM_ADD | NIM_MODIFY => {
            let is_new = pos.is_none();
            if is_new && message == NIM_MODIFY {
                // Modify for an icon we never saw (registered before we started): learn it
                // anyway, the app will not re-add it until the next TaskbarCreated.
            }
            let mut icon = match pos {
                Some(p) => list[p].clone(),
                None => TrayIcon {
                    hwnd,
                    uid,
                    guid,
                    callback: 0,
                    version: 0,
                    tip: String::new(),
                    hidden: false,
                    path: process_path(hwnd),
                    icon: None,
                },
            };
            if hwnd != 0 {
                icon.hwnd = hwnd;
                icon.uid = uid;
            }
            if guid.is_some() {
                icon.guid = guid;
            }
            if flags & NIF_MESSAGE != 0 {
                icon.callback = rd_u32(b, 24).unwrap_or(0);
            }
            if flags & NIF_ICON != 0 {
                let h = handle32(rd_u32(b, 28).unwrap_or(0));
                icon.icon = if h == 0 { None } else { hicon_rgba(HICON(h as _)).or(icon.icon.take()) };
            }
            if flags & NIF_TIP != 0 {
                icon.tip = rd_wstr(b, 32, 128);
            }
            if flags & NIF_STATE != 0 {
                let state = rd_u32(b, 288).unwrap_or(0);
                let mask = rd_u32(b, 292).unwrap_or(0);
                if mask & NIS_HIDDEN != 0 {
                    icon.hidden = state & NIS_HIDDEN != 0;
                }
            }
            match pos {
                Some(p) => list[p] = icon,
                None => {
                    crate::log(&format!(
                        "{} tray: icon added: {} (v{} {})",
                        ts(),
                        short_name(&icon),
                        icon.version,
                        if message == NIM_ADD { "add" } else { "late modify" }
                    ));
                    list.push(icon);
                }
            }
            true
        }
        NIM_DELETE => match pos {
            Some(p) => {
                let gone = list.remove(p);
                crate::log(&format!("{} tray: icon removed: {}", ts(), short_name(&gone)));
                true
            }
            None => false,
        },
        NIM_SETVERSION => {
            let v = rd_u32(b, 808).unwrap_or(0);
            if let Some(p) = pos {
                list[p].version = v;
            }
            v <= 4
        }
        _ => true,
    };
    drop(list);
    GENERATION.fetch_add(1, Ordering::SeqCst);
    ok
}

fn process_path(hwnd: isize) -> String {
    let mut pid = 0u32;
    // SAFETY: plain query; the process handle is closed before returning.
    unsafe {
        GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if ok {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

/// Renders an HICON at 32x32 into straight RGBA. Drawn on black and on white so icons without
/// an alpha channel (mask icons) get a correct alpha too.
fn hicon_rgba(icon: HICON) -> Option<IconBitmap> {
    let n = ICON_PX;
    let black = render_icon(icon, n, 0x00)?;
    let white = render_icon(icon, n, 0xFF)?;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    let mut any = false;
    for (b, w) in black.chunks_exact(4).zip(white.chunks_exact(4)) {
        // alpha = 255 - (white - black), per channel; colour = black / alpha.
        let diff = (w[0] as i32 - b[0] as i32).max(w[1] as i32 - b[1] as i32).max(w[2] as i32 - b[2] as i32);
        let a = (255 - diff).clamp(0, 255) as u32;
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        any = true;
        let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
        rgba.extend_from_slice(&[un(b[2]), un(b[1]), un(b[0]), a as u8]);
    }
    any.then_some(IconBitmap { w: n as u32, h: n as u32, rgba: rgba.into() })
}

fn render_icon(icon: HICON, n: i32, bg: u8) -> Option<Vec<u8>> {
    // SAFETY: a private DIB section and DC, both released before returning.
    unsafe {
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = n;
        bmi.bmiHeader.biHeight = -n;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, HGDIOBJ(bmp.0));
        let len = (n * n * 4) as usize;
        std::ptr::write_bytes(bits as *mut u8, bg, len);
        let ok = DrawIconEx(dc, 0, 0, icon, n, n, 0, None, DI_NORMAL).is_ok();
        let out = ok.then(|| std::slice::from_raw_parts(bits as *const u8, len).to_vec());
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(dc);
        out
    }
}

/// Sends the shell's click sequence for `action` to the icon's owner window.
/// `x, y`: screen point of the click (NOTIFYICON_VERSION_4 passes it in wParam).
pub fn click(icon: &TrayIcon, action: Action, x: i32, y: i32) -> bool {
    let owner = HWND(icon.hwnd as _);
    // SAFETY: plain queries; SendNotifyMessage never blocks on the owner.
    unsafe {
        if icon.callback == 0 || !IsWindow(owner).as_bool() {
            crate::log(&format!("{} overflow: '{}' has no live owner/callback; not sent", ts(), short_name(icon)));
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        // We just received the user's click, so we may hand foreground rights to the owner
        // (its menu / window can then come to the front and dismiss properly).
        let fg = AllowSetForegroundWindow(pid).is_ok();
        let v4 = icon.version >= 4;
        let wparam = if v4 { ((x as u16 as u32) | ((y as u16 as u32) << 16)) as usize } else { icon.uid as usize };
        let hi = if v4 { icon.uid << 16 } else { 0 };
        let send = |m: u32| SendNotifyMessageW(owner, icon.callback, WPARAM(wparam), LPARAM((m | hi) as isize)).is_ok();
        let seq: &[u32] = match action {
            Action::Left => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
            Action::Double => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
            Action::Right => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        };
        let mut ok = true;
        for &m in seq {
            // NIN_SELECT / WM_CONTEXTMENU only for version 3+ (as Explorer does).
            if (m == NIN_SELECT || m == WM_CONTEXTMENU) && icon.version < 3 {
                continue;
            }
            ok &= send(m);
        }
        crate::log(&format!(
            "{} overflow: {:?} -> '{}' (v{}, cb {:#x}) at ({x},{y}) sent={ok} allow_fg={fg}",
            ts(),
            action,
            short_name(icon),
            icon.version,
            icon.callback
        ));
        ok
    }
}

/// Drops icons whose owner window has gone away (app crashed without NIM_DELETE).
pub fn prune_dead() {
    let Ok(mut list) = ICONS.lock() else { return };
    let before = list.len();
    // SAFETY: IsWindow on possibly stale handles.
    list.retain(|i| unsafe { IsWindow(HWND(i.hwnd as _)).as_bool() });
    if list.len() != before {
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

// ---------- pinned vs hidden (HKCU\Control Panel\NotifyIconSettings) ----------

/// One entry of Windows' per-icon settings.
#[derive(Clone, Debug, Default)]
pub struct IconSetting {
    /// Lower-cased path tail after a known-folder GUID prefix (or the full path).
    pub path_tail: String,
    pub uid: Option<u32>,
    pub guid: Option<[u8; 16]>,
    pub promoted: bool,
}

pub fn icon_settings() -> Vec<IconSetting> {
    let mut out = Vec::new();
    // SAFETY: registry reads into local buffers; keys are closed.
    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Control Panel\\NotifyIconSettings"), 0, KEY_READ, &mut root).is_err() {
            return out;
        }
        let mut i = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(root, i, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None).is_err() {
                break;
            }
            i += 1;
            let mut key = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(name.as_ptr()), 0, KEY_READ, &mut key).is_err() {
                continue;
            }
            let mut s = IconSetting::default();
            if let Some(path) = reg_string(key, w!("ExecutablePath")) {
                s.path_tail = path_tail(&path);
            }
            s.uid = reg_dword(key, w!("UID"));
            s.promoted = reg_dword(key, w!("IsPromoted")) == Some(1);
            s.guid = reg_guid(key, w!("IconGuid"));
            let _ = RegCloseKey(key);
            out.push(s);
        }
        let _ = RegCloseKey(root);
    }
    out
}

/// "{6D809377-...}\Tailscale\x.exe" -> "\tailscale\x.exe"; plain paths are just lower-cased.
pub fn path_tail(path: &str) -> String {
    let p = path.to_lowercase();
    if p.starts_with('{') {
        if let Some(i) = p.find('}') {
            return p[i + 1..].to_owned();
        }
    }
    p
}

/// Whether Windows has this icon pinned to the taskbar (IsPromoted=1). Unknown = hidden,
/// which is Windows 11's default for new icons.
pub fn is_promoted(icon: &TrayIcon, settings: &[IconSetting]) -> bool {
    let path = icon.path.to_lowercase();
    settings.iter().any(|s| {
        let id_match = match (icon.guid, s.guid) {
            (Some(a), Some(b)) => a == b,
            _ => !s.path_tail.is_empty() && path.ends_with(&s.path_tail) && s.uid.is_none_or(|u| u == icon.uid),
        };
        id_match && s.promoted
    })
}

unsafe fn reg_raw(key: HKEY, name: PCWSTR) -> Option<(REG_VALUE_TYPE, Vec<u8>)> {
    let mut ty = REG_VALUE_TYPE(0);
    let mut len = 0u32;
    if RegQueryValueExW(key, name, None, Some(&mut ty), None, Some(&mut len)).is_err() || len == 0 || len > 65536 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    RegQueryValueExW(key, name, None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len)).ok().ok()?;
    buf.truncate(len as usize);
    Some((ty, buf))
}

unsafe fn reg_string(key: HKEY, name: PCWSTR) -> Option<String> {
    let (ty, b) = reg_raw(key, name)?;
    (ty == REG_SZ).then(|| rd_wstr(&b, 0, b.len() / 2))
}

unsafe fn reg_dword(key: HKEY, name: PCWSTR) -> Option<u32> {
    let (ty, b) = reg_raw(key, name)?;
    if ty == REG_DWORD {
        rd_u32(&b, 0)
    } else {
        None
    }
}

unsafe fn reg_guid(key: HKEY, name: PCWSTR) -> Option<[u8; 16]> {
    let (ty, b) = reg_raw(key, name)?;
    if ty == REG_BINARY && b.len() == 16 {
        return b[..16].try_into().ok();
    }
    if ty == REG_SZ {
        return parse_guid(&rd_wstr(&b, 0, b.len() / 2));
    }
    None
}

/// "{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}" -> GUID bytes in memory (little-endian) order.
pub fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok();
    let mut g = [0u8; 16];
    let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    for (dst, &src) in order.iter().enumerate() {
        g[dst] = byte(src)?;
    }
    Some(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tray_data(message: u32, hwnd: u32, uid: u32, flags: u32, cb: u32, tip: &str, state: (u32, u32), ver: u32) -> Vec<u8> {
        let mut b = vec![0u8; 964];
        b[0..4].copy_from_slice(&0x3475_3423u32.to_le_bytes());
        b[4..8].copy_from_slice(&message.to_le_bytes());
        b[8..12].copy_from_slice(&956u32.to_le_bytes());
        b[12..16].copy_from_slice(&hwnd.to_le_bytes());
        b[16..20].copy_from_slice(&uid.to_le_bytes());
        b[20..24].copy_from_slice(&flags.to_le_bytes());
        b[24..28].copy_from_slice(&cb.to_le_bytes());
        for (i, c) in tip.encode_utf16().enumerate() {
            b[32 + i * 2..34 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        b[288..292].copy_from_slice(&state.0.to_le_bytes());
        b[292..296].copy_from_slice(&state.1.to_le_bytes());
        b[808..812].copy_from_slice(&ver.to_le_bytes());
        b
    }

    #[test]
    fn dock_layout_tray_messages_add_modify_version_delete() {
        ICONS.lock().unwrap().clear();
        let add = tray_data(NIM_ADD, 0x1234, 7, NIF_MESSAGE | NIF_TIP, 0x8001, "Tailscale\nConnected", (0, 0), 0);
        assert!(on_tray_data(&add));
        let (_, l) = icons();
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].hwnd, l[0].uid, l[0].callback), (0x1234, 7, 0x8001));
        assert_eq!(short_name(&l[0]), "Tailscale");
        assert!(on_tray_data(&tray_data(NIM_SETVERSION, 0x1234, 7, 0, 0, "", (0, 0), 4)));
        assert!(on_tray_data(&tray_data(NIM_MODIFY, 0x1234, 7, NIF_STATE, 0, "", (1, 1), 0)));
        let (_, l) = icons();
        assert_eq!(l[0].version, 4);
        assert!(l[0].hidden);
        assert!(on_tray_data(&tray_data(NIM_DELETE, 0x1234, 7, 0, 0, "", (0, 0), 0)));
        assert!(icons().1.is_empty());
    }

    #[test]
    fn dock_layout_tray_promoted_matches_path_tail_and_uid() {
        let s = vec![IconSetting { path_tail: path_tail("{6D809377-6AF0-444B-8957-A3773F02200E}\\Tailscale\\tailscale-ipn.exe"), uid: None, guid: None, promoted: true }];
        let icon = TrayIcon {
            hwnd: 1, uid: 0, guid: None, callback: 1, version: 4, tip: String::new(), hidden: false,
            path: "C:\\Program Files\\Tailscale\\tailscale-ipn.exe".into(), icon: None,
        };
        assert!(is_promoted(&icon, &s));
        let other = TrayIcon { path: "C:\\x\\zalo.exe".into(), ..icon };
        assert!(!is_promoted(&other, &s));
        assert_eq!(parse_guid("{7820ae78-23e3-4229-82c1-e41cb67d5b9c}").unwrap()[0], 0x78);
        assert_eq!(parse_guid("{7820ae78-23e3-4229-82c1-e41cb67d5b9c}").unwrap()[3], 0x78);
        assert_eq!(parse_guid("{7820ae78-23e3-4229-82c1-e41cb67d5b9c}").unwrap()[1], 0xae);
    }
}
