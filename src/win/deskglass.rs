//! Liquid Glass sheen over the desktop icons.
//!
//! Explorer draws the icons, so we cannot restyle them. Instead a click-through, per-pixel-alpha
//! child window of the desktop's `SHELLDLL_DefView` sits above the icon list and paints a gloss
//! and glass rims over each icon's rectangle (read from the list view). Off by default; the dock
//! menu switches it. Everything here is best effort: if Explorer's layout is not the usual one
//! the overlay simply stays hidden.
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Memory::{VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{LVIR_ICON, LVM_GETITEMCOUNT, LVM_GETITEMRECT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetClientRect,
    GetWindowThreadProcessId, IsWindow, PeekMessageW, RegisterClassExW, SendMessageTimeoutW, SetWindowPos, ShowWindow,
    TranslateMessage, UpdateLayeredWindow, HTTRANSPARENT, HWND_TOP, MSG, PM_REMOVE, SMTO_ABORTIFHUNG, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_NCHITTEST, WNDCLASSEXW, WS_CHILD, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);

/// Share of the icon size used as the glass corner radius (same as the dock icons).
const RADIUS: f32 = 0.24;

/// Turns the effect on or off; the first `true` starts the worker thread.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on && !STARTED.swap(true, Ordering::SeqCst) {
        if !crate::background::spawn("desktop-glass", 512 * 1024, run) {
            STARTED.store(false, Ordering::SeqCst);
        }
    }
}

fn log(msg: &str) {
    crate::log(&format!("deskglass: {msg}"));
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

extern "system" fn each_worker(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the address of an `HWND` local that outlives the enumeration.
    unsafe {
        let mut buf = [0u16; 16];
        let n = GetClassNameW(hwnd, &mut buf);
        if String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "WorkerW" {
            if let Ok(v) = FindWindowExW(hwnd, None, w!("SHELLDLL_DefView"), PCWSTR::null()) {
                *(lparam.0 as *mut HWND) = v;
                return false.into();
            }
        }
    }
    true.into()
}

/// The desktop's `SysListView32` and its parent `SHELLDLL_DefView`, wherever Explorer put them
/// (under Progman, or under a WorkerW when the wallpaper is raised).
fn find_desktop_list() -> Option<(HWND, HWND)> {
    // SAFETY: plain window lookups; the enumeration callback only writes the local below.
    unsafe {
        let mut defview = HWND::default();
        if let Ok(progman) = FindWindowW(w!("Progman"), PCWSTR::null()) {
            if let Ok(v) = FindWindowExW(progman, None, w!("SHELLDLL_DefView"), PCWSTR::null()) {
                defview = v;
            }
        }
        if defview.is_invalid() {
            let _ = EnumWindows(Some(each_worker), LPARAM(&mut defview as *mut HWND as isize));
        }
        if defview.is_invalid() {
            return None;
        }
        let list = FindWindowExW(defview, None, w!("SysListView32"), PCWSTR::null()).ok()?;
        Some((list, defview))
    }
}

/// Icon rectangles (list-view client coordinates) read out of Explorer's address space.
fn icon_rects(list: HWND) -> Vec<RECT> {
    // SAFETY: the remote RECT is allocated, written, read and freed here; every Win32 call is
    // checked and the process handle is closed before returning.
    unsafe {
        let mut count = 0usize;
        let sent = SendMessageTimeoutW(list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 200, Some(&mut count));
        if sent.0 == 0 || count == 0 || count > 2000 {
            return Vec::new();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(list, Some(&mut pid));
        let Ok(process) = OpenProcess(
            PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION,
            false,
            pid,
        ) else {
            return Vec::new();
        };
        let size = std::mem::size_of::<RECT>();
        let remote = VirtualAllocEx(process, None, size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        let mut out = Vec::new();
        if !remote.is_null() {
            for i in 0..count {
                // LVM_GETITEMRECT reads the wanted part from `left` before it fills the rect in.
                let want = RECT { left: LVIR_ICON as i32, top: 0, right: 0, bottom: 0 };
                if WriteProcessMemory(process, remote, &want as *const RECT as *const _, size, None).is_err() {
                    break;
                }
                let mut ok = 0usize;
                let sent = SendMessageTimeoutW(
                    list,
                    LVM_GETITEMRECT,
                    WPARAM(i),
                    LPARAM(remote as isize),
                    SMTO_ABORTIFHUNG,
                    200,
                    Some(&mut ok),
                );
                if sent.0 == 0 || ok == 0 {
                    continue;
                }
                let mut rect = RECT::default();
                if ReadProcessMemory(process, remote, &mut rect as *mut RECT as *mut _, size, None).is_ok()
                    && rect.right > rect.left
                    && rect.bottom > rect.top
                {
                    out.push(rect);
                }
            }
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
        }
        let _ = windows::Win32::Foundation::CloseHandle(process);
        out
    }
}

/// Premultiplied BGRA sheen for `rects` in a `w` x `h` buffer.
fn render(w: i32, h: i32, rects: &[RECT]) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for cell in rects {
        // The list view's icon rectangle is wider than the icon: use the largest centred square,
        // so the glass hugs the icon like a dock icon does.
        let side = (cell.right - cell.left).min(cell.bottom - cell.top);
        let left = cell.left + (cell.right - cell.left - side) / 2;
        let top = cell.top + (cell.bottom - cell.top - side) / 2;
        let rc = &RECT { left, top, right: left + side, bottom: top + side };
        let (rw, rh) = (side as f32, side as f32);
        let radius = rw.min(rh) * RADIUS;
        for y in rc.top.max(0)..rc.bottom.min(h) {
            for x in rc.left.max(0)..rc.right.min(w) {
                let (lx, ly) = ((x - rc.left) as f32 + 0.5, (y - rc.top) as f32 + 0.5);
                // Signed distance inside the rounded rect (>= 0 inside).
                let qx = (lx - rw / 2.0).abs() - (rw / 2.0 - radius);
                let qy = (ly - rh / 2.0).abs() - (rh / 2.0 - radius);
                let outside = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
                let depth = -outside;
                if depth < 0.0 {
                    continue;
                }
                // Gloss over the upper ~55%, strongest at the top.
                let t = ly / rh;
                let gloss = if t < 0.55 { 54.0 * (1.0 - t / 0.55) } else { 0.0 };
                // Rim: a 1 px band inside the edge, brightest at the top-left.
                let rim = if depth < 1.2 {
                    let k = 1.0 - (lx / rw + ly / rh) / 2.0;
                    (30.0 + 120.0 * k) * (1.0 - depth / 1.2)
                } else {
                    0.0
                };
                let a = (gloss + rim).min(255.0) as u8;
                if a > 0 {
                    let i = ((y * w + x) * 4) as usize;
                    // White, premultiplied: every channel equals the alpha.
                    px[i..i + 4].copy_from_slice(&[a, a, a, a]);
                }
            }
        }
    }
    px
}

fn run() {
    log("starting");
    // SAFETY: window creation and message dispatch on this worker thread, which owns the overlay.
    unsafe {
        let class = w!("SmartDockDeskGlass");
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let mut overlay = HWND::default();
        let mut size = (0i32, 0i32);
        let mut last: Vec<RECT> = Vec::new();
        let mut shown = false;
        loop {
            if !ENABLED.load(Ordering::Relaxed) {
                if shown && !overlay.is_invalid() {
                    let _ = ShowWindow(overlay, SW_HIDE);
                    shown = false;
                }
                thread::sleep(Duration::from_millis(400));
                continue;
            }
            let Some((list, defview)) = find_desktop_list() else {
                thread::sleep(Duration::from_secs(2));
                continue;
            };
            let mut client = RECT::default();
            let _ = GetClientRect(list, &mut client);
            let (w, h) = (client.right - client.left, client.bottom - client.top);
            if w <= 0 || h <= 0 {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            if overlay.is_invalid() || !IsWindow(overlay).as_bool() {
                match CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
                    class,
                    w!(""),
                    WS_CHILD,
                    0,
                    0,
                    w,
                    h,
                    defview,
                    None,
                    hinst,
                    None,
                ) {
                    Ok(hwnd) => {
                        overlay = hwnd;
                        shown = false;
                        log("overlay window created");
                    }
                    Err(e) => {
                        log(&format!("create failed: {e}"));
                        thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
            }
            let rects = icon_rects(list);
            if rects != last || size != (w, h) || !shown {
                let pixels = render(w, h, &rects);
                if paint(overlay, w, h, &pixels) {
                    let _ = SetWindowPos(overlay, HWND_TOP, 0, 0, w, h, SWP_NOACTIVATE | SWP_NOMOVE | SWP_SHOWWINDOW);
                    let _ = ShowWindow(overlay, SW_SHOWNOACTIVATE);
                    shown = true;
                    size = (w, h);
                    last = rects;
                } else {
                    thread::sleep(Duration::from_secs(5));
                }
            }
            // Let the window process its messages between polls.
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Pushes the pixels to the layered window. False when Windows refuses (logged once).
fn paint(hwnd: HWND, w: i32, h: i32, pixels: &[u8]) -> bool {
    // SAFETY: the DIB section is created, filled and released inside this call; `pixels` has
    // exactly w * h * 4 bytes.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let ok = match CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) if !bits.is_null() => {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
                let old = SelectObject(mem, bmp);
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let res = UpdateLayeredWindow(
                    hwnd,
                    screen,
                    None,
                    Some(&SIZE { cx: w, cy: h }),
                    mem,
                    Some(&POINT { x: 0, y: 0 }),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                if let Err(e) = &res {
                    static ONCE: AtomicBool = AtomicBool::new(false);
                    if !ONCE.swap(true, Ordering::Relaxed) {
                        log(&format!("UpdateLayeredWindow failed: {e}"));
                    }
                }
                SelectObject(mem, old);
                let _ = DeleteObject(bmp);
                res.is_ok()
            }
            _ => false,
        };
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        ok
    }
}
