#![windows_subsystem = "windows"] // GUI app: no console window when launched from Explorer
//! SmartDock entry point: AppBar dock window with Direct2D tiles and popups.
//! Startup: recover taskbar from a crash -> create dock -> register AppBar -> hide taskbar.
//! Every exit path (WM_CLOSE, panic, Ctrl+C) restores the taskbar.
mod data;
mod model;
mod settings;
mod ui;
mod win;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{Datelike, Local};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, SetLastError, BOOL, E_FAIL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WIN32_ERROR, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, TrackMouseEvent, UnregisterHotKey, MOD_ALT, MOD_CONTROL, TME_LEAVE,
    TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use data::history::History;
use model::Stats;
use ui::appmenu;
use ui::apps::{self, AppsState};
use ui::control_center::{self, ControlState};
use ui::dock::{self, RectF, Style, Tile, TileView};
use ui::glass;
use ui::liquid::{self, LiquidBitmap};
use ui::popup;
use ui::trayview;
use ui::popup_content::{self, MediaAction as PopupMedia, PopupKind};
use ui::renderer::Renderer;
use ui::svgpath;
use ui::thumbnails;
use ui::tiles::{self, DataState};
use win::appbar::{self, AbEdge};
use win::dpi;
use win::grokbot;
use win::system::{self, MediaAction};
use win::tasks::{self, Attention, ShellEvent};
use win::{monitors, wallpaper};

const DOCK_HEIGHT: f32 = dock::DOCK_H;
const POPUP_W: f32 = 320.0;
const CONTROL_POPUP_W: f32 = 296.0;
const SETTINGS_POPUP_W: f32 = 460.0;
/// Height used when the popup content cannot be measured yet.
const POPUP_H_DEFAULT: f32 = 260.0;
const TIMER_REFRESH: usize = 1; // 1 s: apps, stats, popup refresh, repaint
const TIMER_GUARD: usize = 2; // 500 ms: keep the Windows taskbar hidden
const TIMER_EXPLORER: usize = 0x5E5E; // one-shot: Explorer restarted, re-attach after it settles
const TIMER_VISUALIZER: usize = 3; // ~30 fps, enabled only while media is playing
const TIMER_STOPWATCH: usize = 4; // tenths of a second, only while its widget/popup is visible
const HOTKEY_TOGGLE: i32 = 1;

/// Data shared with the background refresh thread. Blocking calls run there, never on the UI thread.
struct Shared {
    /// `data` and `settings` are swapped copy-on-write: readers take a cheap `Arc` clone
    /// instead of cloning the whole state on every repaint.
    data: Mutex<Arc<DataState>>,
    control: Mutex<ControlState>,
    settings: Mutex<Arc<settings::Settings>>,
    /// Liquid Glass backdrop per monitor (`rcMonitor` key), rendered off the UI thread.
    liquid: Mutex<HashMap<(i32, i32, i32, i32), Arc<LiquidBitmap>>>,
    /// Set by WM_DISPLAYCHANGE so monitor/virtual-screen geometry is sampled again.
    refresh_liquid: AtomicBool,
    refresh_slow: AtomicBool,
    /// Peak of the default render endpoint as f32 bits.
    audio_peak: AtomicU32,
}

/// Extra dock surface on a non-primary monitor (same tiles, own HWND / AppBar / renderer).
struct SecondaryDock {
    hwnd: HWND,
    monitor: RECT,
    origin: (f32, f32),
    dpi: u32,
    width: f32,
    renderer: Renderer,
    tiles: Vec<(&'static str, RectF)>,
    hover: Option<usize>,
}

struct App {
    hwnd: HWND,
    /// Dock top-left in screen coordinates, for popup anchors.
    origin: (f32, f32),
    dpi: u32,
    width: f32,
    renderer: Renderer,
    shared: Arc<Shared>,
    apps: AppsState,
    attention: Attention,
    /// Tile id and rect from the last paint, used for hit testing.
    tiles: Vec<(&'static str, RectF)>,
    /// Index into `tiles` of the tile under the pointer.
    hover: Option<usize>,
    /// Latest CPU/RAM sample, read once per second on the timer.
    stats: Stats,
    /// Two-minute CPU/RAM history for the sparklines.
    history: History,
    visualizer_running: bool,
    dock_visible: bool,
    /// True while this session is a remote desktop session: the dock is hidden so it does not cover the remote taskbar.
    remote_hidden: bool,
    /// True while a fullscreen RDP client is in front (the debounced answer of `update_remote_state`).
    rdp_front: bool,
    /// The RDP answer seen on the last tick when it differs from `rdp_front`; it must repeat once to flip.
    rdp_pending: Option<bool>,
    /// Monitors whose dock is hidden because a fullscreen window covers them.
    fs_hidden: Vec<isize>,
    fs_pending: Option<Vec<isize>>,
    /// True while the docks are hidden and their AppBars removed (`remote_hidden || rdp_front`).
    docks_released: bool,
    /// A display change arrived while the docks were released; they are rebuilt on restore.
    topology_stale: bool,
    /// One dock per additional monitor; empty on single-display machines.
    secondaries: Vec<SecondaryDock>,
}

#[derive(Clone, Copy, PartialEq)]
enum OpenPopup {
    Data(&'static str),
    Widget(&'static str),
    Control,
    AppMenu,
    /// Right-click on empty dock area ("Cài đặt SmartDock").
    DockMenu,
    /// Widget gallery sheet.
    Gallery,
    /// SmartDock's own tray overflow (hidden notification icons).
    Tray,
}

/// Click resolved on the UI thread; side effects run after the APP borrow is released.
enum Click {
    None,
    Activate(isize),
    /// Pinned app with no open window.
    Launch(String),
    Start,
    GrokBot,
    Search,
    Open(OpenPopup, RectF),
    /// Tray chevron: toggle Windows' own hidden-icons flyout (anchor in physical px).
    TrayOverflow(RectF),
    /// Widget that acts on a click instead of opening a popup (focus timer, bin, downloads).
    Action(&'static str),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Modified time of settings.json at the last reload (see `settings::load_if_changed`).
    static SETTINGS_SEEN: RefCell<Option<Option<std::time::SystemTime>>> = const { RefCell::new(None) };
    /// Control Center state while that popup is open; `None` means a data popup.
    static CONTROL_VIEW: RefCell<Option<ControlState>> = const { RefCell::new(None) };
    static OPEN: Cell<Option<OpenPopup>> = const { Cell::new(None) };
    /// Height of the currently shown data popup, from `popup_content::content_height`.
    static POPUP_H_CUR: Cell<f32> = const { Cell::new(POPUP_H_DEFAULT) };
    static POPUP_W_CUR: Cell<f32> = const { Cell::new(POPUP_W) };
    static SHELL_MSG: Cell<u32> = const { Cell::new(0) };
}

/// Search roots / env override for git. Empty means `refresh_git` will auto-discover repos.
fn default_repos() -> Vec<PathBuf> {
    if let Ok(value) = std::env::var("SMARTDOCK_REPOS") {
        let configured: Vec<PathBuf> = value
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect();
        if !configured.is_empty() {
            return configured;
        }
    }
    // Empty → data::git::discover_repos() (user.email + profile scan).
    Vec::new()
}

fn main() {
    dpi::enable_per_monitor_v2();
    appbar::recover_from_previous_crash();

    // Restore the taskbar on any panic before the default hook prints.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The GUI build has no console: keep the message in the log so a crash can be traced.
        log(&format!("PANIC: {info}"));
        appbar::ensure_restored_on_exit();
        default_hook(info);
    }));

    // Ctrl+C / console close: restore, then exit. Returning TRUE stops the default termination.
    unsafe {
        // SAFETY: handler is a plain extern fn with the required signature.
        let _ = SetConsoleCtrlHandler(Some(console_handler), BOOL(1));
    }

    if let Err(e) = run() {
        let msg = format!("SmartDock không khởi động được:\n{e}");
        log(&msg);
        show_error(&msg);
    }
    log("exit");
    appbar::ensure_restored_on_exit();
}

/// Appends a line to `%APPDATA%\SmartDock\log.txt`. The GUI build has no console to print to.
fn log(msg: &str) {
    use std::io::Write;
    // Unit tests must never write into the user's real log.
    if cfg!(test) {
        return;
    }
    let Ok(appdata) = std::env::var("APPDATA") else { return };
    let dir = std::path::Path::new(&appdata).join("SmartDock");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
        let _ = writeln!(f, "{msg}");
    }
}

fn show_error(msg: &str) {
    let text = windows::core::HSTRING::from(msg);
    let title = windows::core::HSTRING::from("SmartDock");
    unsafe {
        // SAFETY: both strings are live for the call.
        MessageBoxW(None, &text, &title, MB_ICONERROR);
    }
}

unsafe extern "system" fn console_handler(_ctrl_type: u32) -> BOOL {
    appbar::ensure_restored_on_exit();
    std::process::exit(0);
}

fn start_background_refresh(shared: Arc<Shared>, dock_height_px: i32) {
    // Independent workers keep WMI, image decoding and public APIs away from rendering and
    // the existing weather/media sampler. No shared data/settings lock is held across I/O.
    let screen_shared = shared.clone();
    thread::spawn(move || {
        let mut tracker = data::screentime::Tracker::default();
        loop {
            let enabled = screen_shared.settings.lock().unwrap().tiles.screentime;
            tracker.sample(enabled);
            thread::sleep(Duration::from_secs(1));
        }
    });
    thread::spawn(|| {
        let mut last = std::time::Instant::now() - Duration::from_secs(30);
        loop {
            if last.elapsed().as_secs() >= 15 || win::widget_tools::brightness_pending() {
                win::widget_tools::poll_brightness(); last = std::time::Instant::now();
            }
            thread::sleep(Duration::from_millis(250));
        }
    });
    thread::spawn(|| loop {
        win::widget_tools::refresh_shots();
        thread::sleep(Duration::from_secs(5));
    });
    thread::spawn(|| {
        let mut btc = std::time::Instant::now() - Duration::from_secs(60);
        let mut rates = std::time::Instant::now() - Duration::from_secs(3600);
        loop {
            if btc.elapsed().as_secs() >= 60 { btc = std::time::Instant::now(); data::finance::refresh_btc(); }
            if rates.elapsed().as_secs() >= 3600 { rates = std::time::Instant::now(); data::finance::refresh_rates(); }
            thread::sleep(Duration::from_millis(500));
        }
    });
    let liquid_shared = shared.clone();
    thread::spawn(move || {
        let mut tick: u64 = 0;
        let mut have = false;
        loop {
            // Liquid Glass backdrop: one crop per monitor, refreshed when wallpaper/topology changes.
            // Own thread: the slow pass (git/AI/weather over the network) used to run first and
            // left the bar as bare black + tint for many seconds after every start.
            if (!have && tick % 20 == 0)
                || liquid_shared.refresh_liquid.swap(false, Ordering::SeqCst)
                || (tick % 15 == 0 && wallpaper::wallpaper_changed())
            {
                let virtual_screen = RECT {
                    left: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
                    top: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
                    right: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) },
                    bottom: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN) },
                };
                let virtual_size = (virtual_screen.right - virtual_screen.left, virtual_screen.bottom - virtual_screen.top);
                let monitors = appbar::enum_monitor_rects();
                match wallpaper::source_path() {
                    Some(path) => log(&format!("wallpaper candidate={}", path.display())),
                    None => log("wallpaper candidate not found"),
                }
                let max_w = monitors
                    .iter()
                    .map(|m| m.right - m.left)
                    .max()
                    .unwrap_or(virtual_size.0)
                    .max(virtual_size.0) as u32;
                match wallpaper::load(max_w) {
                    Ok(wp) => {
                        let mut map = HashMap::new();
                        for monitor in &monitors {
                            let (bar, screen) = if wp.mode == wallpaper::FitMode::Span {
                                (
                                    RectF {
                                        x: (monitor.left - virtual_screen.left) as f32,
                                        y: (monitor.bottom - virtual_screen.top - dock_height_px) as f32,
                                        w: (monitor.right - monitor.left) as f32,
                                        h: dock_height_px as f32,
                                    },
                                    virtual_size,
                                )
                            } else {
                                (
                                    RectF {
                                        x: 0.0,
                                        y: (monitor.bottom - monitor.top - dock_height_px) as f32,
                                        w: (monitor.right - monitor.left) as f32,
                                        h: dock_height_px as f32,
                                    },
                                    (monitor.right - monitor.left, monitor.bottom - monitor.top),
                                )
                            };
                            log(&format!(
                                "wallpaper source={} mode={:?} monitor=({},{}..{},{}) bar=({:.0},{:.0},{:.0},{:.0})",
                                wp.source.display(),
                                wp.mode,
                                monitor.left,
                                monitor.top,
                                monitor.right,
                                monitor.bottom,
                                bar.x,
                                bar.y,
                                bar.w,
                                bar.h,
                            ));
                            let bitmap = liquid::render_liquid(&wp, bar, screen);
                            map.insert(
                                (monitor.left, monitor.top, monitor.right, monitor.bottom),
                                Arc::new(bitmap),
                            );
                        }
                        let n = map.len();
                        *liquid_shared.liquid.lock().unwrap() = map;
                        have = n > 0;
                        log(&format!("liquid backdrop rendered for {n} monitor(s)"));
                        // Popups render their own glass from the same wallpaper when they open.
                        glass::set_wallpaper(wp);
                    }
                    Err(e) => log(&format!("wallpaper load failed: {e}")),
                }
            }
            tick += 1;
            // Poll fast until the first backdrop exists, then once a second.
            thread::sleep(Duration::from_millis(if have { 1000 } else { 250 }));
        }
    });
    let widget_shared = shared.clone();
    thread::spawn(move || {
        let mut sampler = data::widgets::Sampler::default();
        loop {
            let sample = sampler.read();
            Arc::make_mut(&mut widget_shared.data.lock().unwrap()).apply(tiles::DataUpdate::Widgets(sample));
            thread::sleep(Duration::from_secs(1));
        }
    });
    thread::spawn(move || {
        let mut tick: u64 = 0;
        loop {
            // Control state every 2 s (volume, mute, radios, brightness).
            if tick % 2 == 0 {
                let c = ControlState::read();
                *shared.control.lock().unwrap() = c;
            }
            // Recycle Bin and Downloads readings every 5 s.
            if tick % 5 == 0 {
                win::shellinfo::refresh();
            }
            // Media every second, so play/pause and track changes show up quickly.
            let update = DataState::fetch_media();
            Arc::make_mut(&mut shared.data.lock().unwrap()).apply(update);
            // Git, AI usage and weather every 10 min (first pass runs immediately).
            if tick % 600 == 0 || shared.refresh_slow.swap(false, Ordering::SeqCst) {
                let current = shared.settings.lock().unwrap().clone();
                let repos = if current.git_folders.is_empty() { default_repos() } else { current.git_folders.clone() };
                let updates = DataState::fetch_slow(&repos, &current.git_author, &current.weather_location);
                let mut guard = shared.data.lock().unwrap();
                let data = Arc::make_mut(&mut guard);
                for update in updates {
                    data.apply(update);
                }
            }
            tick += 1;
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn start_audio_meter(shared: Arc<Shared>) {
    thread::spawn(move || {
        let mut ticks: u64 = 0;
        loop {
            let playing = shared
                .data
                .lock()
                .ok()
                .and_then(|data| data.media.as_ref().map(|media| media.playing))
                .unwrap_or(false);
            if playing {
                let peak = system::audio_peak();
                shared.audio_peak.store(peak.to_bits(), Ordering::Relaxed);
                // Debug: one peak sample per second so field logs prove the meter moves with music.
                if ticks % 30 == 0 {
                    log(&format!("audio_peak={peak:.3} (eMultimedia/eConsole meter)"));
                }
                ticks = ticks.wrapping_add(1);
                thread::sleep(Duration::from_millis(33));
            } else {
                shared.audio_peak.store(0.0f32.to_bits(), Ordering::Relaxed);
                ticks = 0;
                thread::sleep(Duration::from_millis(250));
            }
        }
    });
}

/// Creates the dock window. Tries the full dock style first, then plainer styles, logging each attempt.
fn create_dock_window(
    hinst: HINSTANCE,
    class: PCWSTR,
    screen_w: i32,
    screen_h: i32,
    dock_height_px: i32,
) -> windows::core::Result<HWND> {
    create_dock_window_at(hinst, class, 0, screen_h - dock_height_px, screen_w, dock_height_px, "SmartDock")
}

fn create_dock_window_at(
    hinst: HINSTANCE,
    class: PCWSTR,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    title: &str,
) -> windows::core::Result<HWND> {
    let title_wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let attempts = [
        ("dock", WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, WS_POPUP, x, y, w, h),
        ("plain popup", WINDOW_EX_STYLE(0), WS_POPUP, x, y, w, h),
    ];
    let mut created = None;
    for (name, ex, style, x, y, w, h) in attempts {
        // SAFETY: class is registered by the caller; title wide string lives for the call.
        let res = unsafe {
            SetLastError(WIN32_ERROR(0));
            let res = CreateWindowExW(
                ex,
                class,
                PCWSTR(title_wide.as_ptr()),
                style,
                x,
                y,
                w,
                h,
                None,
                None,
                hinst,
                None,
            );
            log(&format!("CreateWindowExW '{name}' last_error={:?}", GetLastError()));
            res
        };
        match res {
            Ok(hwnd) if !hwnd.is_invalid() => {
                log(&format!("window '{name}' created"));
                log(&format!("dwm transparency: {}", ui::renderer::apply_dwm_transparency(hwnd)));
                created = Some(hwnd);
                break;
            }
            _ => log(&format!("window '{name}' failed")),
        }
    }
    created.ok_or_else(|| windows::core::Error::new(E_FAIL, "CreateWindowExW failed for every style"))
}

/// Builds a dock HWND + AppBar + renderer for every non-primary monitor.
///
/// `primary_hwnd` is used with `MonitorFromWindow` so we skip the primary by full
/// `rcMonitor` / HMONITOR — never by the dock AppBar strip (that was the 66ecd48 bug).
/// Windows are created hidden; the caller must paint then `ShowWindow`.
fn build_secondary_docks(
    hinst: HINSTANCE,
    class: PCWSTR,
    primary_hwnd: HWND,
    dock_height_dip: f32,
) -> Vec<SecondaryDock> {
    let primary = monitors::bounds_for_window(primary_hwnd)
        .or_else(|_| monitors::primary_bounds())
        .unwrap_or(monitors::MonitorBounds { left: 0, top: 0, right: 0, bottom: 0 });
    if primary.width() <= 0 || primary.height() <= 0 {
        log("secondary docks: primary monitor bounds unavailable, skipping");
        appbar::set_secondary_docks_active(false);
        return Vec::new();
    }
    let targets = monitors::secondary_monitor_bounds(&monitors::enum_bounds(), primary);
    log(&format!(
        "monitors: primary=({},{}..{},{}) secondary_targets={}",
        primary.left, primary.top, primary.right, primary.bottom, targets.len()
    ));

    let mut out = Vec::new();
    for mon_bounds in targets {
        let mon = mon_bounds.to_rect();
        let mon_w = mon_bounds.width();
        let mon_h = mon_bounds.height();
        if mon_w <= 0 || mon_h <= 0 {
            continue;
        }
        // Approximate DPI; refined after CreateWindow via window_dpi.
        let approx_dpi = 96u32;
        let dock_h = dpi::dip_to_px(dock_height_dip, approx_dpi);
        let corner = dpi::dip_to_px(16.0, approx_dpi);
        let Ok(hwnd) = create_dock_window_at(
            hinst,
            class,
            mon.left,
            mon.bottom - dock_h,
            mon_w,
            dock_h + corner,
            "SmartDockSecondary",
        ) else {
            log(&format!(
                "secondary dock create failed for monitor ({},{}..{},{})",
                mon.left, mon.top, mon.right, mon.bottom
            ));
            continue;
        };
        let dpi_value = dpi::window_dpi(hwnd);
        let dock_h = dpi::dip_to_px(dock_height_dip, dpi_value);
        let corner = dpi::dip_to_px(16.0, dpi_value);
        let Ok(rect) = appbar::register_appbar_on(hwnd, AbEdge::Bottom, dock_h, mon) else {
            log("secondary appbar register failed");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            continue;
        };
        let pixel_w = rect.right - rect.left;
        let window_h = dock_h + corner;
        // Position but keep hidden — showing before APP owns the dock lets WM_PAINT
        // validate an empty frame (EndPaint) that never gets tiles.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                pixel_w,
                window_h,
                SWP_NOACTIVATE | SWP_HIDEWINDOW,
            );
        }
        ui::renderer::apply_dock_no_chrome(hwnd);
        let renderer = Renderer::new(hwnd, pixel_w, window_h, dpi_value);
        log(&format!(
            "secondary dock on monitor ({},{}..{},{}) hwnd={:?} (hidden until painted)",
            mon.left, mon.top, mon.right, mon.bottom, hwnd
        ));
        out.push(SecondaryDock {
            hwnd,
            monitor: mon,
            origin: (rect.left as f32, rect.top as f32),
            dpi: dpi_value,
            width: dpi::px_to_dip(pixel_w, dpi_value),
            renderer,
            tiles: Vec::new(),
            hover: None,
        });
    }
    // Our secondary docks replace Shell_SecondaryTrayWnd (hidden in "hidden" taskbar mode).
    appbar::set_secondary_docks_active(!out.is_empty());
    out
}

fn show_secondary_docks(docks: &[SecondaryDock]) {
    for dock in docks {
        unsafe {
            let _ = ShowWindow(dock.hwnd, SW_SHOWNOACTIVATE);
        }
    }
    appbar::ensure_secondary_trays_visible();
}

/// Unregisters AppBars and returns HWNDs to destroy. DestroyWindow must run **outside**
/// any `APP` RefCell borrow — it re-enters `wnd_proc`.
fn take_secondary_dock_hwnds(docks: &mut Vec<SecondaryDock>) -> Vec<HWND> {
    let hwnds: Vec<HWND> = docks
        .drain(..)
        .map(|dock| {
            appbar::unregister_appbar(dock.hwnd);
            dock.hwnd
        })
        .collect();
    appbar::set_secondary_docks_active(false);
    hwnds
}

fn destroy_secondary_hwnds(hwnds: &[HWND]) {
    for &hwnd in hwnds {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn run() -> windows::core::Result<()> {
    log("start");
    let initial_settings = settings::load();
    data::ai::set_claude_source(initial_settings.claude_source == "apikey");
    let hinst: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h.into(),
        Err(e) => {
            log(&format!("GetModuleHandleW failed: {e}"));
            return Err(e);
        }
    };
    log("module handle ok");
    let class = w!("SmartDockDock");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst,
        lpszClassName: class,
        ..Default::default()
    };
    unsafe {
        // SAFETY: wc and the class name are valid for the call.
        let atom = RegisterClassExW(&wc);
        log(&format!("RegisterClassExW atom={atom}"));
    }

    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let initial_height = dpi::dip_to_px(DOCK_HEIGHT, dpi::system_dpi());
    let hwnd = create_dock_window(hinst, class, screen_w, screen_h, initial_height)?;
    let dpi_value = dpi::window_dpi(hwnd);
    let dock_height_px = dpi::dip_to_px(DOCK_HEIGHT, dpi_value);
    let corner_px = dpi::dip_to_px(16.0, dpi_value);
    // Extend the HWND below rcMonitor so DWM's lower rounded corners are off-screen. Only the
    // 48-DIP dock surface is visible; its upper corners retain compositor antialiasing.
    let dock_window_height_px = dock_height_px + corner_px;

    // Remove the taskbar's edge claim before asking the shell to place our AppBar. Otherwise
    // ABM_QUERYPOS can return a rect one taskbar-height above rcMonitor.bottom.
    appbar::hide_taskbar(initial_settings.taskbar_mode == "hidden");
    appbar::install_tray_show_hook();
    tasks::install_foreground_hook(Some(data::usage::on_foreground));

    // Reserve the work area at the bottom so maximized windows stop above the dock.
    let rect: RECT = appbar::register_appbar(hwnd, AbEdge::Bottom, dock_height_px)?;
    log(&format!("appbar rect {:?}", (rect.left, rect.top, rect.right, rect.bottom)));
    let pixel_width = rect.right - rect.left;
    let width = dpi::px_to_dip(pixel_width, dpi_value);
    unsafe {
        // SAFETY: hwnd is our live window.
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rect.left,
            rect.top,
            pixel_width,
            dock_window_height_px,
            SWP_NOACTIVATE,
        );
    }
    // No acrylic and no rounding/border: the bar is invisible, only the tiles are drawn.
    ui::renderer::apply_dock_no_chrome(hwnd);

    let shell_msg = tasks::register_shell_hook(hwnd);
    SHELL_MSG.with(|m| m.set(shell_msg));

    popup::set_painter(paint_popup);
    popup::set_click_handler(on_popup_click);
    popup::set_on_hide(on_popup_hidden);
    popup::set_char_handler(on_popup_char);
    popup::set_wheel_handler(on_popup_wheel);
    popup::set_drag_handler(on_popup_drag);
    popup::set_tick_handler(on_popup_tick);
    popup::set_rclick_handler(on_popup_rclick);
    win::system::watch_volume();

    let shared = Arc::new(Shared {
        data: Mutex::new(Arc::new(DataState::default())),
        control: Mutex::new(ControlState::read()),
        settings: Mutex::new(Arc::new(initial_settings.clone())),
        liquid: Mutex::new(HashMap::new()),
        refresh_liquid: AtomicBool::new(false),
        refresh_slow: AtomicBool::new(false),
        audio_peak: AtomicU32::new(0.0f32.to_bits()),
    });
    start_background_refresh(shared.clone(), dock_height_px);
    start_audio_meter(shared.clone());

    let mut apps = AppsState::default();
    data::usage::init();
    apps.pins = pins_of(&initial_settings);
    log(&format!(
        "autopin: enabled={} count={} shown=[{}]",
        initial_settings.auto_pin,
        initial_settings.auto_pin_count,
        data::usage::auto_pins(&initial_settings).iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")
    ));
    let attention = Attention::default();
    apps.refresh(&attention);

    let renderer = Renderer::new(hwnd, pixel_width, dock_window_height_px, dpi_value);
    // Pass the primary HWND so selection uses MonitorFromWindow → full rcMonitor
    // (never the AppBar strip rect, which caused a blank dock on the primary in 66ecd48).
    let secondaries = build_secondary_docks(hinst, class, hwnd, DOCK_HEIGHT);
    for sec in &secondaries {
        glass::apply_dock_glass(sec.hwnd, initial_settings.glass_opacity);
    }
    log(&format!("secondary docks: {}", secondaries.len()));
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            hwnd,
            origin: (rect.left as f32, rect.top as f32),
            dpi: dpi_value,
            width,
            renderer,
            shared,
            apps,
            attention,
            tiles: Vec::new(),
            hover: None,
            stats: data::stats::read_stats(),
            history: History::default(),
            visualizer_running: false,
            dock_visible: true,
            remote_hidden: false,
            rdp_front: false,
            rdp_pending: None,
            fs_hidden: Vec::new(),
            fs_pending: None,
            docks_released: false,
            topology_stale: false,
            secondaries,
        })
    });
    // Paint tiles into secondary render targets while still hidden, then show.
    repaint();
    APP.with(|a| {
        if let Some(app) = a.borrow().as_ref() {
            for sec in &app.secondaries {
                log(&format!(
                    "secondary dock tiles={} width={:.0} monitor=({},{}..{},{})",
                    sec.tiles.len(),
                    sec.width,
                    sec.monitor.left,
                    sec.monitor.top,
                    sec.monitor.right,
                    sec.monitor.bottom
                ));
            }
            show_secondary_docks(&app.secondaries);
        }
    });
    appbar::ensure_secondary_trays_visible();

    // The tray listener starts only after every AppBar is registered with Explorer; the
    // work-area guard (TIMER_GUARD) turns it off again if the reservation does not hold.
    if initial_settings.tray_service {
        win::traysvc::start();
    } else {
        log("tray: listener disabled by settings (trayService=false)");
    }

    unsafe {
        // SAFETY: hwnd is live; timers drive refresh and the taskbar guard.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_REFRESH, 1000, None);
        SetTimer(hwnd, TIMER_STOPWATCH, 100, None);
        SetTimer(hwnd, TIMER_GUARD, 500, None);
        let _ = RegisterHotKey(hwnd, HOTKEY_TOGGLE, MOD_CONTROL | MOD_ALT, b'D' as u32);
    }
    repaint();

    log("message loop");
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log("message loop ended");
    Ok(())
}

/// Tiles per zone, in display order, with widths from styles.css.
/// Left: git, AI providers, media. Center: running apps. Right: Control Center, settings, CPU, RAM, clock.
struct Zones {
    left: Vec<Tile>,
    center: Vec<Tile>,
    right: Vec<Tile>,
}

/// Provider card size: icon plate plus up to three limit lines (height 0 = the standard widget height).
const USAGE_TILE_W: f32 = 170.0;
/// Width of the wide clock style.
const CLOCK_WIDE_W: f32 = 130.0;
const USAGE_TILE_H: f32 = 0.0; // same height as every other widget

/// Dock width of a widget.
fn widget_width(id: &str) -> f32 {
    if let Some(width) = ui::widget_design::width(id) { return width; }
    if let Some(width) = ui::more_widgets::width(id) { return width; }
    match id {
        "git" => dock::GIT_W,
        "claude" | "codex" | "cursor" | "grok" => USAGE_TILE_W,
        "media" => dock::MEDIA_W,
        "weather" => 100.0,
        "focus" => 84.0,
        "timer" => 104.0,
        "battery" => 114.0,
        "net" => 116.0,
        "disk" => 124.0,
        "gpu" => 64.0,
        "date" => 112.0,
        "todo" => 160.0,
        "downloads" => 150.0,
        "chevron" => 30.0,
        "cc" => 78.0,
        "clock" => 64.0,
        "lang" => 42.0,
        _ => 42.0, // cpu / ram: wide enough for a tiny label above the percent inside the ring
    }
}

fn configured_widget_width(id:&str,s:&settings::Settings)->f32 {
    if id=="clock" {
        if s.clock_style=="wide" {CLOCK_WIDE_W}
        else if s.tiles.weather && s.weather_position=="clock" {42.0}
        else {widget_width(id)}
    } else if settings::is_fixed_widget(id) {widget_width(id)}
    else {(widget_width(id)*s.widget_scale()).round()}
}

fn layout_tiles(app_count: usize, settings: &settings::Settings) -> Zones {
    // Tight to the drawn slots: padding here would push the visible cluster off the centre.
    let apps_w = apps::cluster_width(app_count);
    let t = |id, width| Tile { id, width, height: 0.0 };
    let mut left = Vec::new();
    // Fixed widths: tiles never resize with their text (long values ellipsize inside).
    let widgets = |ids: &[String]| -> Vec<Tile> {
        ids.iter()
            .filter_map(|id| settings::WIDGET_IDS.iter().copied().find(|w| *w == id))
            .filter(|id| settings.widget_shown(id))
            .filter(|id| *id != "brightness" || win::widget_tools::brightness().is_some())
            .map(|id| Tile {
                id,
                width: configured_widget_width(id,settings),
                // The provider cards carry up to three limit lines.
                height: if matches!(id, "claude" | "codex" | "cursor" | "grok") { USAGE_TILE_H } else { 0.0 },
            })
            .collect()
    };
    left.extend(widgets(&settings.layout.left));
    let right = widgets(&settings.layout.right);
    Zones {
        left,
        // Variant A: Start squircle, Search, thin divider, then the app icons; the Recycle Bin
        // closes the cluster after a second divider (like macOS).
        center: {
            let cell = |id| Tile { id, width: apps::ICON, height: apps::CLUSTER_H };
            let mut center = vec![cell("start"), cell("systemsearch")];
            if app_count > 0 {
                center.push(Tile { id: "sep", width: 1.0, height: 24.0 });
                center.push(Tile { id: "apps", width: apps_w, height: apps::CLUSTER_H });
            }
            center.push(Tile { id: "sep2", width: 1.0, height: 24.0 });
            center.push(cell("trash"));
            center
        },
        right,
    }
}

/// Bar color for AI usage, from `barColor` in AiTiles.tsx.
fn usage_color(pct: f32) -> u32 {
    // Over limit → red; ≥90% → orange; high → amber; otherwise green.
    if pct > 100.0 {
        0xFF5D_5DFF
    } else if pct >= 90.0 {
        0xFFB3_40FF
    } else if pct >= 70.0 {
        0xE8A0_20FF
    } else {
        0x3FD2_7FFF
    }
}

/// Gauge color for CPU/RAM, from `pctColor` in common.tsx.
fn gauge_color(pct: f32) -> u32 {
    if pct > 85.0 {
        0xFF5D_5DFF
    } else if pct > 65.0 {
        0xFFB3_40FF
    } else {
        0x3FD2_7FFF
    }
}

/// AI tile: colored square with the provider logo, label, session percent, reset time and a usage bar.
fn ai_view(data: &DataState, name: &str) -> TileView {
    let (label, bg, glyph) = match name {
        "Claude" => ("Claude", 0xD977_57FF, (svgpath::CLAUDE_SPARK, None)),
        "Codex" => ("ChatGPT", 0x0D0D_0DFF, (svgpath::CODEX_ORBITS, Some(1.7))),
        "Cursor" => ("Cursor", 0x1414_14FF, (svgpath::CURSOR_CUBE, Some(1.5))),
        _ => ("Grok", 0x0000_00FF, (svgpath::GROK_SLASH, Some(2.0))),
    };
    let usage = data.ai.iter().find(|a| a.name == name);
    let (value, sub, progress, accent) = match usage {
        Some(a) if name == "Cursor" && !a.session_pct.is_nan() => (
            a.reset_text.clone(),
            String::new(),
            // Bar fill is capped at 100%; colour still reflects over-limit spend.
            Some((a.session_pct / 100.0).clamp(0.0, 1.0)),
            usage_color(a.session_pct),
        ),
        Some(a) if !a.session_pct.is_nan() => (
            format!("{:.0}%", a.session_pct),
            a.reset_text.clone(),
            Some((a.session_pct / 100.0).clamp(0.0, 1.0)),
            usage_color(a.session_pct),
        ),
        // API key mode: the month's spend, the value the tile is for.
        Some(a) if a.cost_usd.is_some() => (format!("${:.2}", a.cost_usd.unwrap_or_default()), "tháng này".into(), None, 0),
        Some(a) => (short_ai_status(&a.reset_text).into(), String::new(), None, 0),
        None => ("…".into(), String::new(), None, 0),
    };
    // Cursor: keep the $ value + bar; the two extra Dock-Rc limits become small bars.
    let mini = match usage {
        Some(a) if name == "Cursor" => a
            .windows
            .iter()
            .skip(1)
            .map(|w| ((w.used_pct / 100.0).clamp(0.0, 1.0), usage_color(w.used_pct)))
            .collect(),
        _ => Vec::new(),
    };
    // Signed in with live numbers (not API-key spend): the LumaBar-style limit lines.
    if let Some(a) = usage.filter(|a| !a.session_pct.is_nan() && a.cost_usd.is_none()) {
        return TileView {
            style: Style::Usage,
            rows: usage_rows(name, a),
            bg,
            glyph: Some(glyph),
            ..TileView::card(label, "")
        };
    }
    TileView { progress, accent, bg, sub, mini, glyph: Some(glyph), ..TileView::card(label, value) }
}

/// Limit lines for the LumaBar-style usage tile: up to three, from the provider's windows.
fn usage_rows(name: &str, a: &model::AiUsage) -> Vec<dock::UsageRow> {
    let short = |label: &str| -> String {
        match label {
            "Plan (chu kỳ)" => "Plan".to_owned(),
            "Auto + Composer" => "Auto".to_owned(),
            "API models" => "API".to_owned(),
            other => other.split(" (").next().unwrap_or(other).to_owned(),
        }
    };
    // Reset text: the date after "·" (Cursor's plan line), "reset 28/05" -> "28/05", or the text itself.
    let reset = |text: &str| -> String {
        let t = text.rsplit('·').next().unwrap_or("").trim().trim_start_matches("reset ").trim();
        if t.contains('$') { String::new() } else { t.to_owned() }
    };
    let row = |label: String, pct: f32, right: String| dock::UsageRow {
        label,
        frac: (pct / 100.0).clamp(0.0, 1.0),
        color: usage_color(pct),
        right,
        value: format!("{pct:.0}%"),
    };
    if a.windows.is_empty() {
        let label = if name == "Grok" { "Credits" } else { "Usage" };
        return vec![row(label.to_owned(), a.session_pct, reset(&a.reset_text))];
    }
    a.windows
        .iter()
        .take(3)
        .map(|w| {
            let label = if name == "Grok" { "Credits".to_owned() } else { short(&w.label) };
            row(label, w.used_pct, reset(&w.reset_text))
        })
        .collect()
}

fn short_ai_status(status: &str) -> &'static str {
    let s = status.to_ascii_lowercase();
    if s.contains("sign in") || s.contains("đăng nhập") {
        "Đăng nhập"
    } else if s.contains("network") {
        // A provider/TLS/proxy failure is not proof that Windows has lost connectivity.
        "Lỗi API"
    } else if s.contains("expired") {
        "Hết phiên"
    } else if s.contains("rate limit") {
        "Chờ thử lại"
    } else if s.contains("no usage") || s.contains("unavailable") {
        "N/A"
    } else {
        "N/A"
    }
}

/// Weather glyph for a WMO code, from `weatherInfo` in common.tsx.
fn weather_icon(code: u32) -> &'static str {
    match code {
        0 => "☀️",
        1..=2 => "🌤️",
        3 => "☁️",
        4..=48 => "🌫️",
        49..=57 => "🌦️",
        58..=67 => "🌧️",
        68..=77 => "🌨️",
        78..=82 => "🌧️",
        83..=86 => "🌨️",
        _ => "⛈️",
    }
}

/// Grok Bot tile: the app icon, with the Grok usage (from the Cursor sign-in) when it is known.
fn grok_bot_view(data: &DataState) -> TileView {
    let running = grokbot::is_running();
    let usage = data.ai.iter().find(|a| a.name == "Grok");
    let (value, sub, progress, accent) = match usage {
        Some(a) if !a.session_pct.is_nan() => (
            format!("{:.0}%", a.session_pct),
            a.reset_text.clone(),
            Some((a.session_pct / 100.0).clamp(0.0, 1.0)),
            usage_color(a.session_pct),
        ),
        Some(a) if !a.reset_text.is_empty() => (short_ai_status(&a.reset_text).into(), String::new(), None, 0),
        _ => (if running { "Đang chạy".into() } else { "Mở".into() }, String::new(), None, 0),
    };
    let mut view = TileView {
        progress,
        accent,
        bg: 0x0000_00FF,
        sub,
        glyph: Some((svgpath::GROK_SLASH, Some(2.0))),
        ..TileView::card(grokbot::DISPLAY_NAME, value)
    };
    if let Some(icon) = grokbot::icon() {
        view.icon = Some(dock::Icon { w: icon.w, h: icon.h, rgba: icon.rgba });
        view.glyph = None;
    }
    if let Some(a) = usage.filter(|a| !a.session_pct.is_nan()) {
        view.style = Style::Usage;
        view.rows = usage_rows("Grok", a);
    }
    view
}

fn tile_view(
    id: &str,
    data: &DataState,
    control: &ControlState,
    stats: &Stats,
    audio_peak: f32,
    settings: &settings::Settings,
) -> TileView {
    let mut view = tile_view_data(id, data, control, stats, audio_peak, settings);
    view.design_id = if id == "clock" {
        if settings.clock_style == "wide" {"clockwide"} else if settings.tiles.weather && settings.weather_position=="clock" {"clockweather"} else {"clock"}
    } else {id}.into();
    if id == "todo" {
        view.title = "Việc cần làm".into();
        let done = settings.todos.iter().filter(|t| t.done).count();
        view.value = format!("{done}/{}", settings.todos.len());
        view.progress = (!settings.todos.is_empty()).then_some(done as f32 / settings.todos.len() as f32);
    }
    if id == "net" {
        view.series = vec![data.net_history.iter().map(|p| p.0).collect(), data.net_history.iter().map(|p| p.1).collect()];
    }
    view
}

fn tile_view_data(
    id: &str, data: &DataState, control: &ControlState, stats: &Stats,
    audio_peak: f32, settings: &settings::Settings,
) -> TileView {
    if let Some(mut view) = ui::more_widgets::view(id, data, settings) {
        ui::more_widgets::enrich(&mut view, data, settings);
        return view;
    }
    let now = Local::now();
    match id {
        "git" => tiles::git_tile(data),
        "claude" => {
            let mut view = ai_view(data, "Claude");
            // API key mode: show the budget the spend is measured against.
            if data.ai.iter().any(|a| a.name == "Claude" && a.cost_usd.is_some()) {
                view.sub = format!("/ {}$", settings.claude_monthly_budget);
            }
            view
        }
        "codex" => ai_view(data, "Codex"),
        "cursor" => ai_view(data, "Cursor"),
        "grok" => grok_bot_view(data),
        "media" => {
            let mut view = tiles::media_tile(data);
            view.audio_peak = audio_peak;
            view
        }
        "weather" => tiles::weather_tile(data),
        "battery" => tiles::battery_tile(&data.widgets),
        "net" => tiles::net_tile(&data.widgets),
        "disk" => tiles::disk_tile(&data.widgets),
        "gpu" => tiles::gpu_tile(&data.widgets),
        "timer" => tiles::timer_tile(),
        "date" => tiles::date_tile(now.date_naive()),
        "focus" => {
            let f = data::focus::snapshot();
            let (title, accent) = match f.phase {
                data::focus::Phase::Work => ("TẬP TRUNG", 0xFF5D_5DFF),
                data::focus::Phase::Break => ("NGHỈ", 0x3FD2_7FFF),
            };
            let elapsed = 1.0 - f.remaining as f32 / f.total.max(1) as f32;
            TileView {
                progress: Some(elapsed.clamp(0.0, 1.0)),
                accent,
                sub: if f.running { "▶".into() } else { "❚❚".into() },
                ..TileView::card(title, data::focus::mmss(f.remaining))
            }
        }
        "todo" => {
            let latest = settings.todos.iter().rev().find(|t| !t.done).or(settings.todos.last());
            let sub = latest.map_or("Chưa có ghi chú".to_owned(), |t| t.text.clone());
            TileView { style: Style::Note, sub, ..TileView::card("Ghi chú", "") }
        }
        // Recycle Bin: a dock icon at the end of the centre cluster (full when it holds items).
        "trash" => TileView {
            style: Style::Trash,
            progress: (win::shellinfo::info().bin_items > 0).then_some(1.0),
            ..TileView::card("", "")
        },
        "sep2" => TileView { style: Style::Separator, ..TileView::card("", "") },
        "downloads" => {
            let info = win::shellinfo::info();
            match info.latest {
                Some((name, age)) => TileView {
                    sub: win::shellinfo::age_text(age),
                    ..TileView::card(format!("TẢI XUỐNG · {} hôm nay", info.downloads_today), name)
                },
                None => TileView::card("TẢI XUỐNG", "Trống"),
            }
        }
        // .cc-btn: macOS menu-bar style Wi-Fi and speaker icons (D2D geometry).
        "cc" => TileView {
            style: Style::Button,
            status: Some(dock::StatusIcons {
                wifi: ui::sficons::Wifi::from_state(control.wifi, control.wifi_signal),
                volume: control.volume,
                muted: control.muted,
            }),
            ..TileView::card("", "")
        },
        // macOS-style "^" that opens Windows' hidden tray icons; points down while open.
        "chevron" => TileView {
            style: Style::Button,
            chevron: Some(OPEN.with(Cell::get) == Some(OpenPopup::Tray)),
            ..TileView::card("", "")
        },
        "start" => TileView { style: Style::Start, ..TileView::card("", "") },
        "systemsearch" => TileView { style: Style::Search, ..TileView::card("", "") },
        "sep" => TileView { style: Style::Separator, ..TileView::card("", "") },
        // Ring: short label above the percent (no "CPU 65" single-line overlap).
        "cpu" => TileView {
            style: Style::Ring,
            progress: Some(stats.cpu_pct / 100.0),
            accent: gauge_color(stats.cpu_pct),
            ..TileView::card("CPU", format!("{:.0}", stats.cpu_pct))
        },
        "lang" => TileView { style: Style::Label, ..TileView::card("", win::kbd::label()) },
        "ram" => {
            let pct = if stats.ram_total_mb > 0 { stats.ram_used_mb as f32 * 100.0 / stats.ram_total_mb as f32 } else { 0.0 };
            TileView {
                style: Style::Ring,
                progress: Some(pct / 100.0),
                // RAM is blue until it is nearly full (AppSystem tiles use #4aa8ff).
                accent: if pct > 85.0 { 0xFF5D_5DFF } else { 0x4AA8_FFFF },
                ..TileView::card("RAM", format!("{pct:.0}"))
            }
        }
        "clock" => {
            let day = ["CN", "T2", "T3", "T4", "T5", "T6", "T7"][now.weekday().num_days_from_sunday() as usize];
            let sub = if settings.tiles.weather && settings.weather_position == "clock" {
                data.weather
                    .as_ref()
                    .map(|w| format!("{} {:.0}°", weather_icon(w.current.code), w.current.temp))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if settings.clock_style == "wide" {
                const WEEKDAYS: [&str; 7] = ["CHỦ NHẬT", "THỨ HAI", "THỨ BA", "THỨ TƯ", "THỨ NĂM", "THỨ SÁU", "THỨ BẢY"];
                return TileView {
                    style: Style::ClockWide,
                    sub,
                    extra: now.format("%d/%m").to_string(),
                    ..TileView::card(WEEKDAYS[now.weekday().num_days_from_sunday() as usize], now.format("%H:%M").to_string())
                };
            }
            TileView {
                style: Style::Clock,
                sub,
                ..TileView::card(format!("{day} {}", now.format("%d")), now.format("%H:%M").to_string())
            }
        }
        // The apps zone is drawn by apps::draw_apps; it has no background of its own.
        _ => TileView::bare(""),
    }
}

fn paint_surface(
    renderer: &mut Renderer,
    width: f32,
    hover: Option<usize>,
    tiles_out: &mut Vec<(&'static str, RectF)>,
    apps_state: &AppsState,
    stats: &Stats,
    data: &DataState,
    control: &ControlState,
    audio_peak: f32,
    settings: &settings::Settings,
    backdrop: Option<&Arc<LiquidBitmap>>,
) {
    glass::set_style(&settings.dock_style);
    win::deskglass::set_enabled(settings.desktop_glass);
    let zones = layout_tiles(apps_state.windows.len(), settings);
    let rects = dock::layout(&zones.left, &zones.center, &zones.right, width);
    let defs: Vec<&Tile> = zones.left.iter().chain(&zones.center).chain(&zones.right).collect();
    let views: Vec<TileView> = defs
        .iter()
        .map(|t| tile_view(t.id, data, control, stats, audio_peak, settings))
        .collect();
    *tiles_out = defs.iter().zip(rects.iter()).map(|(t, r)| (t.id, *r)).collect();
    // Glass shelf behind the whole centre cluster (Start .. last app), centred with it.
    let c0 = zones.left.len();
    let shelf = (!zones.center.is_empty()).then(|| dock::shelf_for(rects[c0], rects[c0 + zones.center.len() - 1]));
    dock::set_shelf(shelf);
    let apps_rect = tiles_out.iter().find(|(id, _)| *id == "apps").map(|(_, r)| *r);
    let res = renderer
        .begin()
        .and_then(|_| {
            // Glass tiles/plates sample this dock's own wallpaper crop at their positions.
            glass::set_backdrop(backdrop.cloned(), width, dock::DOCK_H);
            dock::draw_dock(renderer, &views, &rects, hover, backdrop.map(|b| b.as_ref()), settings.glass_opacity)
        })
        .and_then(|_| match apps_rect {
            Some(r) => apps::draw_apps(renderer, r, apps_state),
            None => Ok(()),
        })
        .and_then(|_| draw_drag_overlay(renderer, tiles_out))
        .and_then(|_| renderer.end());
    glass::set_backdrop(None, width, dock::DOCK_H);
    if let Err(e) = res {
        log(&format!("paint failed: {e}"));
    }
}

fn liquid_for(map: &HashMap<(i32, i32, i32, i32), Arc<LiquidBitmap>>, mon: RECT) -> Option<Arc<LiquidBitmap>> {
    let key = (mon.left, mon.top, mon.right, mon.bottom);
    if let Some(hit) = map.get(&key) {
        return Some(hit.clone());
    }
    // Topology can race with WM_DISPLAYCHANGE — pick the crop whose rect overlaps this monitor most.
    map.iter()
        .max_by_key(|((l, t, r, b), _)| {
            let x0 = (*l).max(mon.left);
            let y0 = (*t).max(mon.top);
            let x1 = (*r).min(mon.right);
            let y1 = (*b).min(mon.bottom);
            ((x1 - x0).max(0) as i64) * ((y1 - y0).max(0) as i64)
        })
        .filter(|((l, t, r, b), _)| {
            let x0 = (*l).max(mon.left);
            let y0 = (*t).max(mon.top);
            let x1 = (*r).min(mon.right);
            let y1 = (*b).min(mon.bottom);
            x1 > x0 && y1 > y0
        })
        .map(|(_, bmp)| bmp.clone())
        // A crop from another monitor still beats a black bar until topology catches up.
        .or_else(|| map.values().next().cloned())
}

fn repaint() {
    repaint_docks(None);
}

/// Repaints every dock, or only the dock whose HWND is `only` (pointer moves).
fn repaint_docks(only: Option<HWND>) {
    APP.with(|a| {
        let mut slot = a.borrow_mut();
        let Some(app) = slot.as_mut() else { return };

        let stats = app.stats.clone();
        let data = app.shared.data.lock().unwrap().clone();
        let control = app.shared.control.lock().unwrap().clone();
        let audio_peak = f32::from_bits(app.shared.audio_peak.load(Ordering::Relaxed));
        let liquids = app.shared.liquid.lock().unwrap().clone();
        let settings = app.shared.settings.lock().unwrap().clone();
        let primary_mon = appbar::primary_monitor_rect().unwrap_or(RECT {
            left: app.origin.0 as i32,
            top: app.origin.1 as i32,
            right: app.origin.0 as i32 + dpi::dip_to_px(app.width, app.dpi),
            bottom: app.origin.1 as i32 + dpi::dip_to_px(DOCK_HEIGHT, app.dpi),
        });
        let primary_bg = liquid_for(&liquids, primary_mon);

        if only.is_none_or(|h| h == app.hwnd) {
        paint_surface(
            &mut app.renderer,
            app.width,
            app.hover,
            &mut app.tiles,
            &app.apps,
            &stats,
            &data,
            &control,
            audio_peak,
            &settings,
            primary_bg.as_ref(),
        );
        }
        for sec in app.secondaries.iter_mut().filter(|s| only.is_none_or(|h| h == s.hwnd)) {
            let bg = liquid_for(&liquids, sec.monitor);
            paint_surface(
                &mut sec.renderer,
                sec.width,
                sec.hover,
                &mut sec.tiles,
                &app.apps,
                &stats,
                &data,
                &control,
                audio_peak,
                &settings,
                bg.as_ref(),
            );
        }
    });
}

/// Updates the hovered tile from a pointer position. Returns true when the hovered tile changed.
fn set_hover_on(hwnd: HWND, hover_at: Option<(f32, f32)>) -> bool {
    APP.with(|a| {
        let Ok(mut slot) = a.try_borrow_mut() else { return false };
        let Some(app) = slot.as_mut() else { return false };
        if app.hwnd == hwnd {
            let rects: Vec<RectF> = app.tiles.iter().map(|(_, r)| *r).collect();
            let next = hover_at.and_then(|(x, y)| dock::hit_test(x, y, &rects));
            let changed = next != app.hover;
            app.hover = next;
            return changed;
        }
        if let Some(sec) = app.secondaries.iter_mut().find(|s| s.hwnd == hwnd) {
            let rects: Vec<RectF> = sec.tiles.iter().map(|(_, r)| *r).collect();
            let next = hover_at.and_then(|(x, y)| dock::hit_test(x, y, &rects));
            let changed = next != sec.hover;
            sec.hover = next;
            return changed;
        }
        false
    })
}

thread_local! {
    /// Whether the last pointer move was close enough to the app icons to magnify them.
    static NEAR_APPS: Cell<bool> = const { Cell::new(false) };
}

/// True when `x` (dock-local DIP) is within magnification range of this dock's app icons.
fn near_apps(hwnd: HWND, x: f32) -> bool {
    APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return true };
        let Some(app) = slot.as_ref() else { return false };
        let tiles = if app.hwnd == hwnd {
            &app.tiles
        } else if let Some(sec) = app.secondaries.iter().find(|s| s.hwnd == hwnd) {
            &sec.tiles
        } else {
            return false;
        };
        tiles
            .iter()
            .find(|(id, _)| *id == "apps")
            .is_some_and(|(_, r)| x >= r.x - 70.0 && x <= r.x + r.w + 70.0)
    })
}

/// Whether a dock-local point (DIP) is on tile `id`. The apps tile takes the dock's full height, not
/// just its 34-DIP icon row, so clicks near the top/bottom edge still reach the icons.
fn in_tile(id: &str, r: &RectF, x: f32, y: f32) -> bool {
    if id == "apps" {
        return x >= r.x && x < r.x + r.w && (0.0..DOCK_HEIGHT).contains(&y);
    }
    dock::hit_test(x, y, &[*r]).is_some()
}

fn open_app_context_menu(hwnd: HWND, x: f32, y: f32) {
    // Stopwatch: a right click records a lap and presents all laps, including on secondary docks.
    let special = APP.with(|a| {
        let slot = a.borrow(); let app = slot.as_ref()?;
        let (tiles, origin, dpi_value) = if app.hwnd == hwnd { (&app.tiles, app.origin, app.dpi) }
            else { let sec = app.secondaries.iter().find(|s| s.hwnd == hwnd)?; (&sec.tiles, sec.origin, sec.dpi) };
        let (id,rc) = tiles.iter().find(|(id,rc)| matches!(*id,"stopwatch"|"recentshots") && in_tile(id,rc,x,y))?;
        let scale = dpi::scale(dpi_value);
        Some((*id, RectF { x:origin.0+rc.x*scale,y:origin.1,w:rc.w*scale,h:DOCK_HEIGHT*scale }))
    });
    if let Some((id, anchor)) = special {
        if id == "stopwatch" { data::productivity::lap(); }
        OPEN.with(|o| o.set(None));
        open_popup(OpenPopup::Widget(id), anchor);
        repaint();
        return;
    }
    thumbnails::hide();
    let anchor = APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return None };
        let app = slot.as_ref()?;
        let (tiles, origin, dpi_value) = if app.hwnd == hwnd {
            (&app.tiles, app.origin, app.dpi)
        } else {
            let sec = app.secondaries.iter().find(|s| s.hwnd == hwnd)?;
            (&sec.tiles, sec.origin, sec.dpi)
        };
        // Only the apps tile has app icons; any other tile (a widget) falls through to the dock menu.
        let (_, rect) = tiles.iter().find(|(id, r)| *id == "apps" && in_tile(id, r, x, y))?;
        let idx = apps::hit_app_index(x, y, *rect, &app.apps)?;
        let w = &app.apps.windows[idx];
        let group: Vec<_> = apps::group_windows(&app.apps, w).into_iter().cloned().collect();
        let settings = app.shared.settings.lock().unwrap().clone();
        appmenu::set_menu(appmenu::from_group(
            &apps::app_name(w),
            &w.exe,
            &group,
            apps::icon_pixels(&app.apps, w),
            &settings,
        ));
        let scale = dpi::scale(dpi_value);
        let slot = apps::slot_rect(*rect, &app.apps, idx)?;
        Some(RectF {
            x: origin.0 + slot.x * scale,
            y: origin.1,
            w: slot.w * scale,
            h: DOCK_HEIGHT * scale,
        })
    });
    if let Some(anchor) = anchor {
        // Peeks stay off while the menu is up (and the pending hover timer is cancelled).
        thumbnails::set_suppressed(true);
        open_popup(OpenPopup::AppMenu, anchor);
        thumbnails::set_suppressed(OPEN.with(Cell::get) == Some(OpenPopup::AppMenu));
        return;
    }
    // Not on an app icon: the dock's own menu (widget actions + "Cài đặt SmartDock"), anchored at
    // the pointer. Over a widget it also offers move / remove for that widget.
    let anchor = APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return None };
        let app = slot.as_ref()?;
        let (tiles, origin, dpi_value) = if app.hwnd == hwnd {
            (&app.tiles, app.origin, app.dpi)
        } else {
            let sec = app.secondaries.iter().find(|s| s.hwnd == hwnd)?;
            (&sec.tiles, sec.origin, sec.dpi)
        };
        let target = tiles
            .iter()
            .find(|(id, r)| in_tile(id, r, x, y))
            .and_then(|(id, _)| if *id == "trash" { Some("trash") } else { settings::WIDGET_IDS.iter().copied().find(|w| w == id) });
        let settings = app.shared.settings.lock().ok()?.clone();
        DOCK_MENU_H.with(|h| h.set(appmenu::open_dock_menu(target, &settings)));
        let scale = dpi::scale(dpi_value);
        Some(RectF { x: origin.0 + (x - 1.0) * scale, y: origin.1, w: 2.0 * scale, h: DOCK_HEIGHT * scale })
    });
    if let Some(anchor) = anchor {
        log("dockmenu: opened on empty dock area");
        thumbnails::set_suppressed(true);
        open_popup(OpenPopup::DockMenu, anchor);
        thumbnails::set_suppressed(false);
    }
}

fn update_app_thumbnails(hwnd: HWND, x: f32, y: f32) {
    let info = APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return None };
        let app = slot.as_ref()?;
        let (tiles, origin, dpi_value) = if app.hwnd == hwnd {
            (&app.tiles, app.origin, app.dpi)
        } else {
            let sec = app.secondaries.iter().find(|s| s.hwnd == hwnd)?;
            (&sec.tiles, sec.origin, sec.dpi)
        };
        let (_, rect) = tiles.iter().find(|(id, _)| *id == "apps")?;
        let idx = apps::hit_app_index(x, y, *rect, &app.apps)?;
        let w = &app.apps.windows[idx];
        let wins: Vec<(isize, String)> = apps::group_windows(&app.apps, w)
            .into_iter()
            .filter(|e| e.hwnd != 0) // pinned launchers have nothing to preview
            .map(|e| (e.hwnd, e.title.clone()))
            .collect();
        if wins.is_empty() {
            return None;
        }
        let scale = dpi::scale(dpi_value);
        let slot = apps::slot_rect(*rect, &app.apps, idx)?;
        let anchor = RECT {
            left: (origin.0 + slot.x * scale).round() as i32,
            top: origin.1.round() as i32,
            right: (origin.0 + (slot.x + slot.w) * scale).round() as i32,
            bottom: (origin.1 + DOCK_HEIGHT * scale).round() as i32,
        };
        Some((wins, anchor))
    });
    match info {
        Some((wins, anchor)) => thumbnails::schedule(hwnd, anchor, wins),
        None => thumbnails::cancel_pending(hwnd),
    }
}

/// Manual pins first, then the auto-pinned most-used apps (stable between daily re-ranks).
fn pins_of(settings: &settings::Settings) -> Vec<(String, String)> {
    let mut pins: Vec<(String, String)> = settings.dock_pins.iter().map(|p| (p.exe.clone(), p.name.clone())).collect();
    pins.extend(data::usage::auto_pins(settings));
    pins
}

// ---------- volume on the speaker of the Wi-Fi / volume button: wheel and drag ----------

/// Share of the volume range per wheel notch, and per pixel of horizontal drag.
const VOLUME_WHEEL_STEP: f32 = 0.05;
const VOLUME_PER_DIP: f32 = 0.008;
/// Speaker part of the button starts this far (DIP) from its left edge (after the Wi-Fi fan).
const SPEAKER_X: f32 = 24.0;

thread_local! {
    /// Volume drag in progress: (pointer x at press, volume at press, moved far enough yet).
    static VOL_DRAG: Cell<Option<(f32, f32, bool)>> = const { Cell::new(None) };
}

fn on_speaker(tiles: &TileRects, x: f32, y: f32) -> bool {
    tiles.iter().any(|(id, r)| *id == "cc" && in_tile(id, r, x, y) && x >= r.x + SPEAKER_X)
}

fn current_volume() -> f32 {
    APP.with(|a| {
        let slot = a.try_borrow().ok()?;
        let v = slot.as_ref()?.shared.control.lock().ok()?.volume;
        Some(v)
    })
    .unwrap_or(0.5)
}

/// Sets the master volume now: the dock's own state first (the icon follows immediately), the
/// system value through the coalescing worker.
fn set_volume_live(v: f32) {
    let v = v.clamp(0.0, 1.0);
    APP.with(|a| {
        if let Ok(slot) = a.try_borrow() {
            if let Some(app) = slot.as_ref() {
                let mut c = app.shared.control.lock().unwrap();
                c.volume = v;
                if v > 0.0 {
                    c.muted = false;
                }
            }
        }
    });
    if v > 0.0 {
        let _ = win::system::set_mute(false);
    }
    live_apply(control_center::ControlAction::SetVolume(v));
    repaint();
}

/// Wheel over the speaker: +/- 5% per notch.
fn volume_wheel(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> bool {
    use windows::Win32::Foundation::POINT;
    let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
    // SAFETY: `pt` is a valid local; `hwnd` is our window.
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
    }
    let (x, y) = client_point(LPARAM(((pt.y as isize) << 16) | (pt.x as isize & 0xffff)));
    let Some(tiles) = dock_tiles_of(hwnd) else { return false };
    if !on_speaker(&tiles, x, y) {
        return false;
    }
    let notches = ((wparam.0 >> 16) as i16) as f32 / 120.0;
    let target = current_volume() + notches * VOLUME_WHEEL_STEP;
    log(&format!("volume wheel: {:.0}% -> {:.0}%", current_volume() * 100.0, target.clamp(0.0, 1.0) * 100.0));
    set_volume_live(target);
    true
}

// ---------- drag a widget to another place (or the other side) ----------

struct Drag {
    id: &'static str,
    start_x: f32,
    active: bool,
}

thread_local! {
    static DRAG: RefCell<Option<Drag>> = const { RefCell::new(None) };
    /// While dragging: (dragged widget, drop on the left side, slot among the widgets shown there).
    static DROP: Cell<Option<(&'static str, bool, usize)>> = const { Cell::new(None) };
}

type TileRects = [(&'static str, RectF)];

/// Pointer must move this far (DIP) before a press on a widget becomes a drag, not a click.
const DRAG_THRESHOLD: f32 = 8.0;

fn dock_tiles_of(hwnd: HWND) -> Option<Vec<(&'static str, RectF)>> {
    APP.with(|a| {
        let slot = a.try_borrow().ok()?;
        let app = slot.as_ref()?;
        if app.hwnd == hwnd {
            Some(app.tiles.clone())
        } else {
            app.secondaries.iter().find(|s| s.hwnd == hwnd).map(|s| s.tiles.clone())
        }
    })
}

fn widget_id_at(tiles: &TileRects, x: f32, y: f32) -> Option<&'static str> {
    let (id, _) = tiles.iter().find(|(id, r)| in_tile(id, r, x, y))?;
    settings::WIDGET_IDS.iter().copied().find(|w| w == id)
}

/// x of the middle of the centre cluster; widgets left of it are on the left side.
fn center_mid(tiles: &TileRects) -> f32 {
    let centre = tiles.iter().filter(|(id, _)| matches!(*id, "start" | "search" | "sep" | "apps" | "sep2" | "trash"));
    let (lo, hi) = centre.fold((f32::MAX, f32::MIN), |(lo, hi), (_, r)| (lo.min(r.x), hi.max(r.x + r.w)));
    if lo > hi { 0.0 } else { (lo + hi) / 2.0 }
}

/// Widgets drawn on one side, left to right, without `skip`.
fn side_widgets(tiles: &TileRects, left: bool, skip: &str) -> Vec<(&'static str, RectF)> {
    let mid = center_mid(tiles);
    tiles
        .iter()
        .filter(|(id, r)| *id != skip && settings::WIDGET_IDS.contains(id) && ((r.x + r.w / 2.0) < mid) == left)
        .copied()
        .collect()
}

/// Where a widget dropped at dock-local `x` would land: (left side?, slot among that side's widgets).
fn drop_slot(tiles: &TileRects, id: &str, x: f32) -> (bool, usize) {
    let left = x < center_mid(tiles);
    let k = side_widgets(tiles, left, id).iter().filter(|(_, r)| r.x + r.w / 2.0 < x).count();
    (left, k)
}

/// x of the insertion marker for slot `k` on a side.
fn drop_line_x(tiles: &TileRects, id: &str, left: bool, k: usize) -> f32 {
    let side = side_widgets(tiles, left, id);
    if let Some((_, r)) = side.get(k) {
        r.x - 2.5
    } else if let Some((_, r)) = side.last() {
        r.x + r.w + 2.5
    } else if left {
        5.0
    } else {
        // Empty right side: just after the Control Center button.
        tiles.iter().find(|(i, _)| *i == "cc").map_or(5.0, |(_, r)| r.x + r.w + 2.5)
    }
}

/// Handles a WM_MOUSEMOVE during a possible drag. True when the move was consumed by a drag.
fn drag_mouse_move(hwnd: HWND, x: f32, left_down: bool) -> bool {
    let id = DRAG.with(|d| {
        let mut d = d.borrow_mut();
        let drag = d.as_mut()?;
        if !left_down {
            *d = None;
            return None;
        }
        if !drag.active && (x - drag.start_x).abs() > DRAG_THRESHOLD {
            drag.active = true;
        }
        drag.active.then_some(drag.id)
    });
    let Some(id) = id else { return false };
    let Some(tiles) = dock_tiles_of(hwnd) else { return true };
    let (left, k) = drop_slot(&tiles, id, x);
    if DROP.with(Cell::get) != Some((id, left, k)) {
        DROP.with(|d| d.set(Some((id, left, k))));
        thumbnails::cancel_pending(hwnd);
        repaint();
    }
    true
}

/// Ends a drag on mouse-up. True when it was a real drag (so the release is not a click).
fn drag_finish(hwnd: HWND, x: f32) -> bool {
    let drag = DRAG.with(|d| d.borrow_mut().take());
    let Some(drag) = drag.filter(|d| d.active) else { return false };
    DROP.with(|d| d.set(None));
    if let Some(tiles) = dock_tiles_of(hwnd) {
        let (left, k) = drop_slot(&tiles, drag.id, x);
        let shown: Vec<&str> = side_widgets(&tiles, left, drag.id).into_iter().map(|(i, _)| i).collect();
        log(&format!("drag: {} -> {} slot {k}", drag.id, if left { "left" } else { "right" }));
        APP.with(|a| {
            let slot = a.borrow();
            let Some(app) = slot.as_ref() else { return };
            let mut guard = app.shared.settings.lock().unwrap();
            let settings = Arc::make_mut(&mut guard);
            settings.layout.move_to(drag.id, left, &shown, k);
            let _ = settings::save(settings);
        });
    }
    repaint();
    true
}

/// Dims the dragged widget and draws the drop marker on a dock surface.
fn draw_drag_overlay(r: &mut Renderer, tiles: &TileRects) -> windows::core::Result<()> {
    let Some((id, left, k)) = DROP.with(Cell::get) else { return Ok(()) };
    if let Some((_, rc)) = tiles.iter().find(|(i, _)| *i == id) {
        r.fill_rounded_rect(*rc, 12.0, 0x0000_0099)?;
    }
    let lx = drop_line_x(tiles, id, left, k);
    r.fill_rounded_rect(RectF { x: lx - 1.5, y: 8.0, w: 3.0, h: DOCK_HEIGHT - 16.0 }, 1.5, 0x0A84_FFFF)
}

fn dock_click_on(hwnd: HWND, x: f32, y: f32) -> Click {
    APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return Click::None };
        let Some(app) = slot.as_ref() else { return Click::None };
        let (tiles, origin, dpi_value) = if app.hwnd == hwnd {
            (&app.tiles, app.origin, app.dpi)
        } else if let Some(sec) = app.secondaries.iter().find(|s| s.hwnd == hwnd) {
            (&sec.tiles, sec.origin, sec.dpi)
        } else {
            return Click::None;
        };
        let Some((id, rect)) = tiles.iter().find(|(id, r)| in_tile(id, r, x, y)).copied() else {
            return Click::None;
        };
        let scale = dpi::scale(dpi_value);
        let anchor = RectF {
            x: origin.0 + rect.x * scale,
            y: origin.1,
            w: rect.w * scale,
            h: DOCK_HEIGHT * scale,
        };
        match id {
            "apps" => match apps::hit_app_index(x, y, rect, &app.apps).map(|i| &app.apps.windows[i]) {
                Some(w) if w.hwnd == 0 => Click::Launch(w.exe.clone()),
                // Closed since the last refresh: the entry still holds a dead handle, so a pinned app
                // relaunches (as its launcher would) and an unpinned one just waits for the refresh.
                // SAFETY: IsWindow on a possibly stale handle just returns false.
                Some(w) if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(HWND(w.hwnd as _)) }.as_bool() => {
                    if app.apps.pins.iter().any(|(exe, _)| exe.eq_ignore_ascii_case(&w.exe)) {
                        Click::Launch(w.exe.clone())
                    } else {
                        Click::None
                    }
                }
                Some(w) => Click::Activate(w.hwnd),
                None => Click::None,
            },
            "start" => Click::Start,
            "systemsearch" => Click::Search,
            "pomodoro" => Click::Action(if x >= rect.x + rect.w * (126.0/134.0) { "pomodoro_skip" } else { "pomodoro" }),
            "stopwatch" => Click::Action("stopwatch"),
            "recentshots" => Click::Action(match (((x-rect.x-5.0)/((rect.w-12.0)/3.0)) as usize).min(2) { 0=>"shot0",1=>"shot1",_=>"shot2" }),
            "theme" | "screenshot" | "recyclebin" => Click::Action(id),
            "analog" | "flipclock" | "segclock" | "daynum" | "dayprog" | "monthprog" | "yearprog" => Click::Open(OpenPopup::Data("clock"), anchor),
            id if ui::more_widgets::has_popup(id) => Click::Open(OpenPopup::Widget(id), anchor),
            "git" => Click::Open(OpenPopup::Data("git"), anchor),
            "claude" | "codex" | "cursor" => Click::Open(OpenPopup::Data(id), anchor),
            "grok" => Click::Open(OpenPopup::Data("grok"), anchor),
            "media" => Click::Open(OpenPopup::Data("media"), anchor),
            "weather" => Click::Open(OpenPopup::Data("weather"), anchor),
            "clock" => Click::Open(OpenPopup::Data("clock"), anchor),
            "date" => Click::Open(OpenPopup::Data("clock"), anchor),
            "cpu" => Click::Open(OpenPopup::Data("cpu"), anchor),
            "ram" => Click::Open(OpenPopup::Data("ram"), anchor),
            "cc" => Click::Open(OpenPopup::Control, anchor),
            "chevron" => Click::TrayOverflow(anchor),
            "focus" => Click::Action("focus"),
            "timer" => Click::Action("timer"),
            "lang" => Click::Action("lang"),
            "trash" => Click::Action("trash"),
            "downloads" => Click::Action("downloads"),
            "todo" => Click::Open(OpenPopup::Data("todo"), anchor),
            _ => Click::None,
        }
    })
}

/// What a click on an action widget does. Also used by the widget's right-click menu.
fn run_widget_action(id: &str) {
    match id {
        "pomodoro" => data::productivity::pomo_toggle(),
        "pomodoro_skip" => data::productivity::pomo_skip(),
        "pomodoro_reset" => data::productivity::pomo_reset(),
        "stopwatch" => data::productivity::watch_toggle(),
        "stopwatch_reset" => data::productivity::watch_reset(),
        "theme" => { thread::spawn(|| { if let Err(e) = win::widget_tools::toggle_theme() { show_error(&format!("Không đổi được giao diện: {e}")); } }); }
        "screenshot" => { let _ = system::open_path(std::path::Path::new("ms-screenclip:")); }
        "recyclebin" => { thread::spawn(|| { win::shellinfo::empty_bin(); win::shellinfo::refresh(); }); }
        "shot0" | "shot1" | "shot2" => {
            let index = match id {"shot1"=>1,"shot2"=>2,_=>0};
            if let Some(shot)=win::widget_tools::shots().get(index) { let path=shot.path.clone();thread::spawn(move || {let _=system::open_path(&path);}); }
        }
        "focus" => data::focus::toggle(),
        "timer" => data::timer::toggle(),
        "timer_reset" => data::timer::reset(),
        "lang" => win::kbd::switch(),
        "focus_reset" => data::focus::reset(),
        "trash" => {
            let _ = system::open_path(std::path::Path::new("shell:RecycleBinFolder"));
        }
        "trash_empty" => {
            thread::spawn(|| {
                win::shellinfo::empty_bin();
                win::shellinfo::refresh();
            });
        }
        "downloads" => {
            let _ = system::open_path(&win::shellinfo::downloads_dir());
        }
        _ => {}
    }
    repaint();
}

fn popup_kind(id: &str) -> PopupKind {
    match id {
        "git" => PopupKind::Git,
        "claude" | "codex" | "cursor" | "grok" | "ai" => PopupKind::Ai,
        "weather" => PopupKind::Weather,
        "todo" => PopupKind::Todo,
        "clock" => PopupKind::Clock,
        "cpu" => PopupKind::Cpu,
        "ram" => PopupKind::Ram,
        "settings" => PopupKind::Settings,
        _ => PopupKind::Media,
    }
}

/// Pushes fresh data (and sparkline history for CPU/RAM) into the popup content.
fn feed_popup(kind: PopupKind, data: &DataState, history: &History) {
    popup_content::set_content(kind, data.clone());
    let samples = match kind {
        PopupKind::Cpu => Some(history.snapshot().0),
        PopupKind::Ram => Some(history.snapshot().1),
        _ => None,
    };
    if let Some(s) = samples {
        popup_content::set_history(kind, s);
    }
}

fn open_popup(which: OpenPopup, anchor: RectF) {
    LAST_ANCHOR.with(|a| a.set(anchor));
    if which != OpenPopup::AppMenu && which != OpenPopup::DockMenu {
        thumbnails::set_suppressed(false);
    }
    if OPEN.with(Cell::get) == Some(which) {
        popup::hide();
        OPEN.with(|o| o.set(None));
        return;
    }
    match which {
        OpenPopup::Widget(id) => {
            let settings = APP.with(|a| a.borrow().as_ref().map(|app| app.shared.settings.lock().unwrap().clone()));
            if let Some(settings) = settings { ui::more_widgets::prepare(id, &settings); }
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
            glass::set_panel_light(false);
            let height = ui::more_widgets::popup_height(id);
            POPUP_H_CUR.with(|h| h.set(height));
            POPUP_W_CUR.with(|w| w.set(POPUP_W));
            if popup::show(anchor, POPUP_W, height as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
                if ui::more_widgets::editable(id) { popup::focus(); }
            }
        }
        OpenPopup::Data(id) => {
            let kind = popup_kind(id);
            popup_content::set_ai_provider(match id {
                "claude" => Some("Claude"),
                "codex" => Some("Codex"),
                "cursor" => Some("Cursor"),
                "grok" => Some("Grok"),
                _ => None,
            });
            let height = APP
                .with(|a| {
                    let slot = a.borrow();
                    let app = slot.as_ref()?;
                    let data = app.shared.data.lock().unwrap().clone();
                    feed_popup(kind, &data, &app.history);
                    popup_content::set_settings((**app.shared.settings.lock().unwrap()).clone());
                    Some(popup_content::content_height(kind, &data))
                })
                .unwrap_or(POPUP_H_DEFAULT);
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
            glass::set_panel_light(kind == PopupKind::Clock);
            POPUP_H_CUR.with(|h| h.set(height));
            let width = match kind {
                PopupKind::Settings => SETTINGS_POPUP_W,
                PopupKind::Clock => ui::calendar::WIDTH,
                _ => POPUP_W,
            };
            if kind == PopupKind::Clock {
                ui::calendar::reset();
            }
            POPUP_W_CUR.with(|w| w.set(width));
            if popup::show(anchor, width, height as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
        }
        OpenPopup::Control => {
            let state = APP.with(|a| a.borrow().as_ref().map(|app| app.shared.control.lock().unwrap().clone()));
            glass::set_panel_light(true);
            CONTROL_VIEW.with(|c| *c.borrow_mut() = state);
            POPUP_W_CUR.with(|w| w.set(CONTROL_POPUP_W));
            if popup::show(anchor, CONTROL_POPUP_W, control_center::HEIGHT as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
        }
        OpenPopup::DockMenu => {
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
    glass::set_panel_light(false);
            let height = DOCK_MENU_H.with(Cell::get);
            POPUP_H_CUR.with(|h| h.set(height));
            POPUP_W_CUR.with(|w| w.set(appmenu::DOCK_MENU_W));
            if popup::show(anchor, appmenu::DOCK_MENU_W, height as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
        }
        OpenPopup::Gallery => open_gallery(),
        OpenPopup::Tray => {
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
    glass::set_panel_light(false);
            let (w, h) = trayview::open(anchor);
            POPUP_H_CUR.with(|c| c.set(h));
            POPUP_W_CUR.with(|c| c.set(w));
            if popup::show(anchor, w, h as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
            repaint();
        }
        OpenPopup::AppMenu => {
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
    glass::set_panel_light(false);
            let height = appmenu::current()
                .map(|m| appmenu::content_height(&m))
                .unwrap_or(180.0);
            POPUP_H_CUR.with(|h| h.set(height));
            POPUP_W_CUR.with(|w| w.set(260.0));
            if popup::show(anchor, 260.0, height as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
        }
    }
}

thread_local! {
    /// Height (DIPs) of the dock menu built by the last right-click.
    static DOCK_MENU_H: Cell<f32> = const { Cell::new(44.0) };
    /// Anchor of the most recently opened popup (screen px), reused when one popup swaps to another.
    static LAST_ANCHOR: Cell<RectF> = const { Cell::new(RectF { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }) };
}

/// Shows SmartDock Settings in the popup, anchored where the current popup was opened.
/// Entries for the widget gallery: every widget with its live preview tile.
fn gallery_entries() -> Vec<ui::gallery::Entry> {
    APP.with(|a| {
        let slot = a.borrow();
        let Some(app) = slot.as_ref() else { return Vec::new() };
        let data = app.shared.data.lock().unwrap().clone();
        let control = app.shared.control.lock().unwrap().clone();
        let settings = app.shared.settings.lock().unwrap().clone();
        let stats = app.stats.clone();
        ui::gallery::ITEMS
            .iter()
            .map(|&(id, cat)| ui::gallery::Entry {
                id,
                name: settings::widget_label(id),
                cat,
                view: tile_view(id, &data, &control, &stats, 0.0, &settings),
                width: configured_widget_width(id,&settings),
                on: settings.widget_shown(id),
                fixed: settings::is_fixed_widget(id),
                disabled: id == "brightness" && win::widget_tools::brightness().is_none() && !settings.widget_shown(id),
            })
            .collect()
    })
}

fn open_gallery() {
    OPEN.with(|o| o.set(None));
    appmenu::clear();
    popup::hide();
    ui::gallery::reset();
    CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
    glass::set_panel_light(true);
    POPUP_W_CUR.with(|w| w.set(ui::gallery::W));
    POPUP_H_CUR.with(|h| h.set(ui::gallery::H));
    if popup::show_centered(ui::gallery::W, ui::gallery::H as i32).is_ok() {
        OPEN.with(|o| o.set(Some(OpenPopup::Gallery)));
        popup::focus();
    }
}

fn open_settings_popup() {
    let anchor = LAST_ANCHOR.with(Cell::get);
    if OPEN.with(Cell::get) == Some(OpenPopup::Data("settings")) {
        return;
    }
    // Swap in place: clear the open kind so open_popup does not treat it as a toggle.
    OPEN.with(|o| o.set(None));
    appmenu::clear();
    open_popup(OpenPopup::Data("settings"), anchor);
}

/// Keeps an open data popup live: fresh data and history each second, then a repaint of the popup.
fn refresh_open_popup() {
    if matches!(OPEN.with(Cell::get), Some(OpenPopup::Widget(_))) {
        popup::invalidate(); return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Gallery) {
        popup::invalidate();
        return;
    }
    let Some(OpenPopup::Data(id)) = OPEN.with(Cell::get) else { return };
    APP.with(|a| {
        let slot = a.borrow();
        let Some(app) = slot.as_ref() else { return };
        let data = app.shared.data.lock().unwrap().clone();
        let kind = popup_kind(id);
        feed_popup(kind, &data, &app.history);
        if kind == PopupKind::Settings {
            popup_content::set_settings((**app.shared.settings.lock().unwrap()).clone());
        }
    });
    popup::invalidate();
}

fn paint_popup(r: &mut Renderer, rect: RectF) {
    if let Some(OpenPopup::Widget(id)) = OPEN.with(Cell::get) {
        APP.with(|a| {
            let slot = a.borrow();
            if let Some(app) = slot.as_ref() {
                let data = app.shared.data.lock().unwrap().clone();
                let settings = app.shared.settings.lock().unwrap().clone();
                let _ = ui::more_widgets::paint_popup(r, id, rect, &data, &settings);
            }
        });
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Gallery) {
        let entries = gallery_entries();
        let _ = ui::gallery::draw(r, rect, &entries);
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::AppMenu) {
        let _ = appmenu::paint(r, rect);
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::DockMenu) {
        let _ = appmenu::paint_dock_menu(r, rect);
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Tray) {
        let _ = trayview::paint(r, rect);
        return;
    }
    let control = CONTROL_VIEW.with(|c| c.borrow().clone());
    match control {
        Some(state) => {
            let media = APP.with(|a| {
                a.borrow()
                    .as_ref()
                    .and_then(|app| app.shared.data.lock().ok()?.media.clone())
            });
            let _ = control_center::draw(r, rect, &state, media.as_ref());
        }
        None => popup_content::paint_content(r, rect),
    }
}

fn popup_rect() -> RectF {
    let h = if CONTROL_VIEW.with(|c| c.borrow().is_some()) { control_center::HEIGHT } else { POPUP_H_CUR.with(Cell::get) };
    RectF { x: 0.5, y: 0.5, w: POPUP_W_CUR.with(Cell::get) - 1.0, h: h - 1.0 }
}

fn on_popup_click(x: f32, y: f32) {
    let rect = popup_rect();
    if let Some(OpenPopup::Widget(id)) = OPEN.with(Cell::get) {
        let action = ui::more_widgets::click(id, x, y, rect);
        apply_more_widget_action(action);
        if ui::more_widgets::editable(id) && OPEN.with(Cell::get)==Some(OpenPopup::Widget(id)) { popup::focus(); }
        popup::repaint_now(); return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::DockMenu) {
        match appmenu::dock_menu_hit(x, y) {
            Some(appmenu::DockMenuAction::Gallery) => {
                open_gallery();
            }
            Some(appmenu::DockMenuAction::Settings) => {
                log("settings: opened from the dock menu (Cài đặt SmartDock)");
                open_settings_popup();
            }
            Some(action) => {
                log(&format!("dockmenu: {action:?}"));
                if let appmenu::DockMenuAction::Extra(id) = action {
                    popup::hide();
                    OPEN.with(|o| o.set(None));
                    run_widget_action(id);
                    return;
                }
                APP.with(|a| {
                    let slot = a.borrow();
                    let Some(app) = slot.as_ref() else { return };
                    let mut guard = app.shared.settings.lock().unwrap();
                    let settings = Arc::make_mut(&mut guard);
                    appmenu::apply_dock_menu(action, settings);
                    let _ = settings::save(settings);
                });
                popup::hide();
                OPEN.with(|o| o.set(None));
                repaint();
            }
            None => {}
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Tray) {
        if trayview::on_click(x, y, trayview::Button::Left) {
            close_tray_popup();
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::AppMenu) {
        let hit = appmenu::hit_test(x, y);
        log(&format!("appmenu: click at ({x:.0},{y:.0}) -> {hit:?}"));
        if let Some(action) = hit {
            APP.with(|a| {
                let slot = a.borrow();
                let Some(app) = slot.as_ref() else { return };
                let mut guard = app.shared.settings.lock().unwrap();
                let settings = Arc::make_mut(&mut guard);
                appmenu::apply(action, settings);
                let _ = settings::save(settings);
            });
            popup::hide();
            OPEN.with(|o| o.set(None));
            appmenu::clear();
            repaint();
        }
        return;
    }
    let control = CONTROL_VIEW.with(|c| c.borrow().clone());
    if let Some(state) = control {
        if let Some(action) = control_center::hit_test(x, y, rect, &state) {
            if action == control_center::ControlAction::OpenSmartDockSettings {
                log("settings: opened from the Control Center gear");
                open_settings_popup();
                return;
            }
            if action == control_center::ControlAction::MediaPlayPause {
                // Flip the icon right away; the next media poll confirms the real state.
                APP.with(|a| {
                    if let Ok(slot) = a.try_borrow() {
                        if let Some(app) = slot.as_ref() {
                            let mut guard = app.shared.data.lock().unwrap();
                            if let Some(m) = Arc::make_mut(&mut guard).media.as_mut() {
                                m.playing = !m.playing;
                            }
                        }
                    }
                });
                popup::repaint_now();
            }
            // Control actions can block (WinRT, COM); run them off the UI thread.
            thread::spawn(move || {
                let _ = control_center::apply(action);
            });
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Gallery) {
        let entries = gallery_entries();
        match ui::gallery::hit(x, y, rect, &entries) {
            Some(ui::gallery::Action::Close) => {
                popup::hide();
                OPEN.with(|o| o.set(None));
            }
            Some(ui::gallery::Action::Category(i)) => {
                ui::gallery::select(i);
                popup::repaint_now();
            }
            Some(ui::gallery::Action::FocusSearch) => {
                ui::gallery::set_focus(true);
                popup::focus();
                popup::repaint_now();
            }
            Some(ui::gallery::Action::Toggle(id)) => {
                let on = entries.iter().find(|e| e.id == id).is_some_and(|e| e.on);
                APP.with(|a| {
                    let slot = a.borrow();
                    let Some(app) = slot.as_ref() else { return };
                    let mut guard = app.shared.settings.lock().unwrap();
                    let settings = Arc::make_mut(&mut guard);
                    let action = if on { appmenu::DockMenuAction::Remove(id) } else { appmenu::DockMenuAction::Add(id) };
                    appmenu::apply_dock_menu(action, settings);
                    let _ = settings::save(settings);
                });
                popup::repaint_now();
                repaint();
            }
            None => ui::gallery::set_focus(false),
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Data("clock")) {
        if let Some(action) = ui::calendar::hit(x, y, rect) {
            ui::calendar::apply(action);
            popup::repaint_now();
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Data("todo")) {
        if let Some(action) = popup_content::hit_todo(x, y, rect) {
            if action == popup_content::TodoAction::FocusInput {
                popup_content::todo_input_set_focus(true);
                popup::focus();
                popup::repaint_now();
                return;
            }
            use popup_content::TodoAction;
            APP.with(|a| {
                let slot = a.borrow();
                let Some(app) = slot.as_ref() else { return };
                let mut guard = app.shared.settings.lock().unwrap();
                let settings = Arc::make_mut(&mut guard);
                match action {
                    TodoAction::Toggle(i) => {
                        if let Some(t) = settings.todos.get_mut(i) {
                            t.done = !t.done;
                        }
                    }
                    TodoAction::Delete(i) => {
                        if i < settings.todos.len() {
                            settings.todos.remove(i);
                        }
                    }
                    TodoAction::AddFromClipboard => {
                        if let Some(text) = win::shellinfo::clipboard_text() {
                            settings.todos.push(settings::TodoItem { text, done: false });
                        }
                    }
                    TodoAction::ClearDone => settings.todos.retain(|t| !t.done),
                    TodoAction::FocusInput => {}
                }
                let _ = settings::save(settings);
                popup_content::set_settings(settings.clone());
            });
            // The list length changed: reopen at the new height.
            let anchor = LAST_ANCHOR.with(Cell::get);
            OPEN.with(|o| o.set(None));
            open_popup(OpenPopup::Data("todo"), anchor);
            repaint();
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Data("settings")) {
        if let Some(action) = popup_content::hit_settings(x, y, rect) {
            APP.with(|a| {
                let slot = a.borrow();
                let Some(app) = slot.as_ref() else { return };
                let mut guard = app.shared.settings.lock().unwrap();
                let settings = Arc::make_mut(&mut guard);
                match action {
                    popup_content::SettingsAction::GlassOpacity(value) => {
                        settings.glass_opacity = value;
                        glass::apply_dock_glass(app.hwnd, value);
                        for sec in &app.secondaries {
                            glass::apply_dock_glass(sec.hwnd, value);
                        }
                    }
                    popup_content::SettingsAction::HiddenTaskbar(hidden) => {
                        settings.taskbar_mode = if hidden { "hidden" } else { "autohide" }.to_owned();
                        appbar::set_hidden_mode(hidden);
                    }
                    popup_content::SettingsAction::ToggleWidget(id) => match id {
                        "git" => settings.tiles.git = !settings.tiles.git,
                        "claude" => settings.tiles.claude = !settings.tiles.claude,
                        "codex" => settings.tiles.codex = !settings.tiles.codex,
                        "cursor" => settings.tiles.cursor = !settings.tiles.cursor,
                        "grok" => settings.tiles.grok = !settings.tiles.grok,
                        "music" => settings.tiles.music = !settings.tiles.music,
                        "cpu" => settings.tiles.cpu = !settings.tiles.cpu,
                        "ram" => settings.tiles.ram = !settings.tiles.ram,
                        "weather" => settings.tiles.weather = !settings.tiles.weather,
                        _ => {}
                    },
                    popup_content::SettingsAction::ToggleWeatherPosition => {
                        settings.weather_position =
                            if settings.weather_position == "clock" { "tile" } else { "clock" }.to_owned();
                    }
                    popup_content::SettingsAction::ToggleStartup => {
                        settings.launch_at_startup = !settings.launch_at_startup;
                        settings::sync_startup(settings.launch_at_startup);
                    }
                    popup_content::SettingsAction::ToggleAutoPin => {
                        settings.auto_pin = !settings.auto_pin;
                        log(&format!("autopin: {} from Settings", if settings.auto_pin { "turned on" } else { "turned off" }));
                    }
                    popup_content::SettingsAction::CycleAutoPinCount => {
                        settings.auto_pin = true;
                        settings.auto_pin_count = match settings.auto_pin_count {
                            0..=3 => 4,
                            4..=5 => 6,
                            6..=7 => 8,
                            8..=9 => 10,
                            10..=11 => 12,
                            _ => 4,
                        };
                    }
                    popup_content::SettingsAction::ToggleClaudeSource => {
                        settings.claude_source = if settings.claude_source == "apikey" { "login" } else { "apikey" }.to_owned();
                        data::ai::set_claude_source(settings.claude_source == "apikey");
                        // Fetch Claude usage from the new source now, not at the next 10-minute pass.
                        app.shared.refresh_slow.store(true, Ordering::SeqCst);
                    }
                    popup_content::SettingsAction::CycleClaudeBudget => {
                        settings.claude_monthly_budget = match settings.claude_monthly_budget {
                            0..=49 => 50,
                            50..=99 => 100,
                            100..=199 => 200,
                            200..=499 => 500,
                            500..=999 => 1000,
                            _ => 50,
                        };
                    }
                    popup_content::SettingsAction::RefreshUsage => {
                        app.shared.refresh_slow.store(true, Ordering::SeqCst);
                    }
                    popup_content::SettingsAction::OpenConfig => {
                        let _ = settings::save(settings);
                        if let Some(path) = settings::path() {
                            let _ = system::open_path(&path);
                        }
                    }
                }
                let _ = settings::save(settings);
                popup_content::set_settings(settings.clone());
            });
            popup::invalidate();
            repaint();
        }
        return;
    }
    if let Some(action) = popup_content::hit_media_button(x, y, rect) {
        let action = match action {
            PopupMedia::Previous => MediaAction::Previous,
            PopupMedia::PlayPause => MediaAction::PlayPause,
            PopupMedia::Next => MediaAction::Next,
        };
        let shared = APP.with(|a| a.borrow().as_ref().map(|app| app.shared.clone()));
        thread::spawn(move || {
            let res = system::media_control(action);
            log(&format!("media control: {res:?}"));
            // Re-read the session right away (after the player had a moment to react), so the
            // popup and the dock tile flip without waiting for the next poll.
            if let Some(shared) = shared {
                thread::sleep(Duration::from_millis(250));
                let update = DataState::fetch_media();
                Arc::make_mut(&mut shared.data.lock().unwrap()).apply(update);
            }
        });
    }
}

/// Right-click inside the popup (only the tray overflow uses it).
fn on_popup_rclick(x: f32, y: f32) {
    if OPEN.with(Cell::get) == Some(OpenPopup::Tray) && trayview::on_click(x, y, trayview::Button::Right) {
        close_tray_popup();
    }
}

fn close_tray_popup() {
    popup::hide();
    repaint();
}

/// Typed character for the note input; Enter adds the note.
fn on_popup_char(ch: char) {
    if let Some(OpenPopup::Widget(id)) = OPEN.with(Cell::get) {
        apply_more_widget_action(ui::more_widgets::input(id, ch));
        popup::repaint_now(); return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Gallery) {
        if ui::gallery::focused() {
            ui::gallery::input_char(ch);
            popup::repaint_now();
        }
        return;
    }
    if OPEN.with(Cell::get) != Some(OpenPopup::Data("todo")) || !popup_content::todo_input_focused() {
        return;
    }
    match popup_content::todo_input_char(ch) {
        Some(text) => {
            APP.with(|a| {
                let slot = a.borrow();
                let Some(app) = slot.as_ref() else { return };
                let mut guard = app.shared.settings.lock().unwrap();
                let settings = Arc::make_mut(&mut guard);
                settings.todos.push(settings::TodoItem { text, done: false });
                let _ = settings::save(settings);
                popup_content::set_settings(settings.clone());
            });
            // The list grew: reopen at the new height, keeping the input focused.
            let anchor = LAST_ANCHOR.with(Cell::get);
            OPEN.with(|o| o.set(None));
            open_popup(OpenPopup::Data("todo"), anchor);
            popup_content::todo_input_set_focus(true);
            popup::focus();
            popup::repaint_now();
        }
        None => popup::repaint_now(),
    }
}

fn apply_more_widget_action(action: ui::more_widgets::Action) {
    use ui::more_widgets::Action;
    match action {
        Action::SaveNote(_) | Action::SetCities(_) | Action::SetCurrency(_,_,_) => {
            let saved = APP.with(|a| {
                let slot = a.borrow(); let Some(app) = slot.as_ref() else { return Ok(()) };
                let mut guard = app.shared.settings.lock().unwrap();
                let mut updated = (**guard).clone();
                match action {
                    Action::SaveNote(note) => updated.note = note,
                    Action::SetCities(cities) => updated.world_cities = cities,
                    Action::SetCurrency(amount,from,to) => { updated.currency_amount=amount;updated.currency_from=from;updated.currency_to=to; }
                    _ => unreachable!(),
                }
                settings::save(&updated)?;
                *guard = Arc::new(updated);
                Ok::<(),std::io::Error>(())
            });
            match saved {
                Ok(()) => { popup::hide(); repaint(); }
                Err(e) => show_error(&format!("Không lưu được widget: {e}")),
            }
        }
        Action::Search(query) => {
            popup::hide();
            thread::spawn(move || { if let Err(e) = win::widget_tools::search(&query) { show_error(&format!("Không mở được tìm kiếm: {e}")); } });
        }
        Action::WatchToggle => data::productivity::watch_toggle(),
        Action::WatchReset => data::productivity::watch_reset(),
        Action::Lap => data::productivity::lap(),
        Action::OpenShot(i) => {
            if let Some(shot) = win::widget_tools::shots().get(i) { let path=shot.path.clone(); popup::hide();thread::spawn(move || {let _=system::open_path(&path);}); }
        }
        Action::None => {},
    }
    repaint();
}

fn on_popup_hidden() {
    popup_content::todo_input_set_focus(false);
    if OPEN.with(Cell::get) == Some(OpenPopup::Tray) {
        trayview::closed();
        repaint();
    }
    OPEN.with(|o| o.set(None));
    thumbnails::set_suppressed(false);
    CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
    glass::set_panel_light(false);
    appmenu::clear();
}

thread_local! {
    /// Slider being dragged in the Control Center.
    static DRAG_SLIDER: Cell<Option<control_center::Slider>> = const { Cell::new(None) };
    static CC_MEDIA_SIG: RefCell<Option<(String, bool)>> = const { RefCell::new(None) };
    /// When the last slider drag ended (the 2 s system poll must not snap the knob back).
    static LAST_DRAG_END: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// Control Center slider drag: value follows the pointer on every move, the panel repaints
/// immediately and the system value is applied live (coalesced to ~30 Hz off the UI thread).
fn on_popup_drag(phase: popup::DragPhase, x: f32, _y: f32) -> bool {
    let rect = popup_rect();
    if OPEN.with(Cell::get) == Some(OpenPopup::Widget("brightness")) {
        let handled = ui::more_widgets::brightness_drag(phase, x, _y, rect);
        if handled { popup::repaint_now(); }
        return handled;
    }
    let slider = match phase {
        popup::DragPhase::Down => {
            let hit = CONTROL_VIEW.with(|c| c.borrow().as_ref().and_then(|st| control_center::slider_at(x, _y, rect, st)));
            DRAG_SLIDER.with(|d| d.set(hit));
            match hit {
                Some(s) => s,
                None => return false,
            }
        }
        _ => match DRAG_SLIDER.with(Cell::get) {
            Some(s) => s,
            None => return false,
        },
    };
    let v = control_center::slider_value(slider, x, rect);
    let action = match slider {
        control_center::Slider::Volume => control_center::ControlAction::SetVolume(v),
        control_center::Slider::Brightness => control_center::ControlAction::SetBrightness((v * 100.0).round() as u32),
    };
    CONTROL_VIEW.with(|c| {
        if let Some(st) = c.borrow_mut().as_mut() {
            match action {
                control_center::ControlAction::SetVolume(v) => st.volume = v,
                control_center::ControlAction::SetBrightness(p) => st.brightness = Some(p),
                _ => {}
            }
            let st = st.clone();
            APP.with(|a| {
                if let Ok(slot) = a.try_borrow() {
                    if let Some(app) = slot.as_ref() {
                        *app.shared.control.lock().unwrap() = st;
                    }
                }
            });
        }
    });
    live_apply(action);
    popup::repaint_now();
    if phase == popup::DragPhase::Up {
        DRAG_SLIDER.with(|d| d.set(None));
        LAST_DRAG_END.with(|t| t.set(Some(std::time::Instant::now())));
        log(&format!("control: {:?} set by drag", action));
    }
    true
}

/// Mouse wheel over a Control Center slider card: volume +-5% / brightness +-5 per notch.
fn on_popup_wheel(x: f32, y: f32, notches: f32) {
    if OPEN.with(Cell::get) == Some(OpenPopup::Gallery) {
        ui::gallery::scroll(notches);
        popup::repaint_now();
        return;
    }
    let rect = popup_rect();
    let Some(state) = CONTROL_VIEW.with(|c| c.borrow().clone()) else { return };
    let Some(slider) = control_center::slider_card_at(x, y, rect) else { return };
    let action = match slider {
        control_center::Slider::Volume => {
            control_center::ControlAction::SetVolume((state.volume + notches * 0.05).clamp(0.0, 1.0))
        }
        control_center::Slider::Brightness => {
            let Some(p) = state.brightness else { return };
            control_center::ControlAction::SetBrightness((p as f32 + notches * 5.0).clamp(0.0, 100.0).round() as u32)
        }
    };
    CONTROL_VIEW.with(|c| {
        if let Some(st) = c.borrow_mut().as_mut() {
            match action {
                control_center::ControlAction::SetVolume(v) => {
                    st.volume = v;
                    st.muted = false;
                }
                control_center::ControlAction::SetBrightness(p) => st.brightness = Some(p),
                _ => {}
            }
            let st = st.clone();
            APP.with(|a| {
                if let Ok(slot) = a.try_borrow() {
                    if let Some(app) = slot.as_ref() {
                        *app.shared.control.lock().unwrap() = st;
                    }
                }
            });
        }
    });
    LAST_DRAG_END.with(|t| t.set(Some(std::time::Instant::now())));
    live_apply(action);
    popup::repaint_now();
}

/// Queues a slider value for the live-apply worker, which applies only the newest value of each
/// kind at most ~30 times a second (DDC/CI brightness can take tens of ms per call).
fn live_apply(action: control_center::ControlAction) {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::OnceLock;
    static TX: OnceLock<std::sync::Mutex<Sender<control_center::ControlAction>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<control_center::ControlAction>();
        thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut vol = None;
                let mut br = None;
                let mut take = |a| match a {
                    control_center::ControlAction::SetVolume(_) => vol = Some(a),
                    control_center::ControlAction::SetBrightness(_) => br = Some(a),
                    _ => {}
                };
                take(first);
                while let Ok(a) = rx.try_recv() {
                    take(a);
                }
                for a in [vol, br].into_iter().flatten() {
                    let _ = control_center::apply(a);
                }
                thread::sleep(Duration::from_millis(33));
            }
        });
        std::sync::Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send(action);
    }
}

/// Popup tick (~100 ms): reflect external volume/mute changes (IAudioEndpointVolumeCallback)
/// and fresh radio/brightness state while the Control Center stays open.
fn on_popup_tick() {
    if OPEN.with(Cell::get) == Some(OpenPopup::Tray) {
        match trayview::tick() {
            trayview::Tick::Close => close_tray_popup(),
            trayview::Tick::Repaint => popup::invalidate(),
            trayview::Tick::Idle => {}
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Data("media")) {
        // Live media popup: session state and audio level straight from the shared data, so the
        // wave moves and play/pause shows without reopening.
        let live = APP.with(|a| {
            let slot = a.try_borrow().ok()?;
            let app = slot.as_ref()?;
            let media = app.shared.data.lock().ok()?.media.clone();
            Some((media, f32::from_bits(app.shared.audio_peak.load(Ordering::Relaxed))))
        });
        if let Some((media, peak)) = live {
            popup_content::update_media(media, peak);
            popup::invalidate();
        }
        return;
    }
    if CONTROL_VIEW.with(|c| c.borrow().is_none()) {
        let _ = win::system::take_volume_change();
        return;
    }
    if popup::is_dragging() {
        let _ = win::system::take_volume_change();
        return;
    }
    let mut changed = false;
    if let Some((v, m)) = win::system::take_volume_change() {
        CONTROL_VIEW.with(|c| {
            if let Some(st) = c.borrow_mut().as_mut() {
                if (st.volume - v).abs() > 0.004 || st.muted != m {
                    st.volume = v;
                    st.muted = m;
                    changed = true;
                }
            }
        });
    }
    let settled = LAST_DRAG_END.with(Cell::get).map_or(true, |t| t.elapsed() > Duration::from_millis(2500));
    if settled {
        let shared = APP.with(|a| a.try_borrow().ok().and_then(|s| s.as_ref().map(|app| app.shared.control.lock().unwrap().clone())));
        if let Some(sh) = shared {
            CONTROL_VIEW.with(|c| {
                if let Some(st) = c.borrow_mut().as_mut() {
                    if st.wifi != sh.wifi || st.bluetooth != sh.bluetooth || st.dark != sh.dark || st.brightness != sh.brightness {
                        st.wifi = sh.wifi;
                        st.bluetooth = sh.bluetooth;
                        st.dark = sh.dark;
                        st.brightness = sh.brightness;
                        changed = true;
                    }
                }
            });
        }
    }
    // Media title / play state changes (this also redraws the play/pause icon after a click).
    let media_sig = APP.with(|a| {
        a.try_borrow().ok().and_then(|s| s.as_ref().and_then(|app| app.shared.data.lock().ok()?.media.as_ref().map(|m| (m.title.clone(), m.playing))))
    });
    if CC_MEDIA_SIG.with(|s| s.replace(media_sig.clone())) != media_sig {
        changed = true;
    }
    if changed {
        popup::repaint_now();
    }
}

thread_local! {
    /// Times the work-area guard found a dock reservation missing (for fight detection).
    static WORK_AREA_MISSES: RefCell<Vec<std::time::Instant>> = const { RefCell::new(Vec::new()) };
    static LAST_REREGISTER: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static LAST_FORCE: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    /// The guard turned the tray listener off for this session (it kept breaking the work area).
    static TRAY_OFF_BY_GUARD: Cell<bool> = const { Cell::new(false) };
}

fn taskbar_created_msg() -> u32 {
    static MSG_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // SAFETY: registers (or looks up) a global message name.
    *MSG_ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

thread_local! {
    /// When the docks were last brought back: the guard leaves the reservation alone for a moment.
    static LAST_RESTORE: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// How long after the docks come back the work-area guard does not repair the reservation.
const RESTORE_SETTLE: std::time::Duration = std::time::Duration::from_secs(15);

/// Registers the primary dock's AppBar and places the window on the reserved strip.
/// Returns the reserved rect, or `None` when Explorer refused the reservation.
fn attach_primary(app: &mut App) -> Option<RECT> {
    let dpi_value = dpi::window_dpi(app.hwnd);
    let dock_h = dpi::dip_to_px(DOCK_HEIGHT, dpi_value);
    let rect = appbar::register_appbar(app.hwnd, AbEdge::Bottom, dock_h).ok()?;
    let pixel_w = rect.right - rect.left;
    let window_h = dock_h + dpi::dip_to_px(16.0, dpi_value);
    // SAFETY: our own dock window; the position is the rect Explorer just reserved.
    unsafe {
        let _ = SetWindowPos(app.hwnd, HWND_TOPMOST, rect.left, rect.top, pixel_w, window_h, SWP_NOACTIVATE);
    }
    app.origin = (rect.left as f32, rect.top as f32);
    app.dpi = dpi_value;
    app.width = dpi::px_to_dip(pixel_w, dpi_value);
    app.renderer.resize(pixel_w, window_h, dpi_value);
    Some(rect)
}

/// Registers an existing secondary dock's AppBar again on its own monitor.
fn attach_secondary(sec: &SecondaryDock) {
    let dock_h = dpi::dip_to_px(DOCK_HEIGHT, sec.dpi);
    let Ok(rect) = appbar::register_appbar_on(sec.hwnd, AbEdge::Bottom, dock_h, sec.monitor) else {
        log("secondary appbar re-register failed");
        return;
    };
    // SAFETY: our own dock window, placed on the strip Explorer just reserved.
    unsafe {
        let _ = SetWindowPos(
            sec.hwnd,
            HWND_TOPMOST,
            rect.left,
            rect.top,
            rect.right - rect.left,
            dock_h + dpi::dip_to_px(16.0, sec.dpi),
            SWP_NOACTIVATE,
        );
    }
}

/// Puts the docks away (`released`: windows hidden, AppBars removed) or brings them back.
/// Used for a remote session and for a fullscreen RDP client in front, so the dock never sits
/// over a remote taskbar. Coming back registers the AppBars once; no WM_DISPLAYCHANGE is posted
/// unless the monitor layout changed meanwhile.
fn set_docks_released(app: &mut App, released: bool) {
    app.docks_released = released;
    appbar::set_dock_released(released);
    if released {
        // SAFETY: our own dock windows.
        unsafe {
            let _ = ShowWindow(app.hwnd, SW_HIDE);
            for sec in &app.secondaries {
                let _ = ShowWindow(sec.hwnd, SW_HIDE);
            }
        }
        appbar::unregister_appbar(app.hwnd);
        for sec in &app.secondaries {
            appbar::unregister_appbar(sec.hwnd);
        }
        log("docks released: hidden, AppBars removed");
        return;
    }
    if attach_primary(app).is_none() {
        log("docks: primary AppBar not restored");
    }
    for sec in &app.secondaries {
        attach_secondary(sec);
    }
    if app.dock_visible {
        let hidden = app.shared.settings.lock().unwrap().taskbar_mode == "hidden";
        appbar::hide_taskbar(hidden);
        // SAFETY: our own dock windows.
        unsafe {
            let _ = ShowWindow(app.hwnd, SW_SHOWNOACTIVATE);
            for sec in &app.secondaries {
                let _ = ShowWindow(sec.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
    LAST_RESTORE.with(|c| c.set(Some(std::time::Instant::now())));
    if app.topology_stale {
        app.topology_stale = false;
        // The monitors changed while the docks were away: rebuild the secondaries now.
        // SAFETY: posting to our own primary dock window.
        unsafe {
            let _ = PostMessageW(app.hwnd, WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0));
        }
    }
    log("docks restored: AppBars registered, shown");
}

/// Hides the dock on the monitor a fullscreen window covers and shows the others.
fn apply_fullscreen_hiding(app: &App) {
    if app.docks_released {
        return;
    }
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
    for hwnd in std::iter::once(app.hwnd).chain(app.secondaries.iter().map(|s| s.hwnd)) {
        // SAFETY: our own dock windows.
        unsafe {
            let on = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST).0 as isize;
            let hide = app.fs_hidden.contains(&on) || !app.dock_visible;
            log(&format!("fullscreen: dock {:?} on monitor {on:#x} hide={hide} (fullscreen monitors {:x?})", hwnd.0, app.fs_hidden));
            let _ = ShowWindow(hwnd, if hide { SW_HIDE } else { SW_SHOWNOACTIVATE });
        }
    }
}

/// Runs every tick (500 ms). Tracks this PC being remoted into and a fullscreen RDP client in
/// front (debounced: a new answer must repeat on the next tick), then releases or restores the
/// docks when the combined state changes.
fn update_remote_state(app: &mut App) {
    // SAFETY: plain metric query.
    let in_session = unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0;
    if in_session != app.remote_hidden {
        app.remote_hidden = in_session;
        log(if in_session { "this PC is in a remote session" } else { "local session again" });
    }
    match tasks::foreground_rdp_fullscreen() {
        // SmartDock itself has focus: keep the previous answer.
        None => {}
        Some(now) if now == app.rdp_front => app.rdp_pending = None,
        Some(now) if app.rdp_pending == Some(now) => {
            app.rdp_front = now;
            app.rdp_pending = None;
            log(if now { "fullscreen RDP in front" } else { "fullscreen RDP left" });
        }
        Some(now) => app.rdp_pending = Some(now),
    }
    // Fullscreen windows (game, video, F11 browser) on any monitor: hide only that monitor's
    // dock. A new answer must repeat on the next tick (debounce).
    let now = tasks::fullscreen_monitors();
    if now == app.fs_hidden {
        app.fs_pending = None;
    } else if app.fs_pending.as_ref() == Some(&now) {
        app.fs_pending = None;
        app.fs_hidden = now;
        apply_fullscreen_hiding(app);
        log(&format!("fullscreen monitors: {}", app.fs_hidden.len()));
        if app.fs_hidden.is_empty() && app.topology_stale && !app.docks_released {
            app.topology_stale = false;
            LAST_RESTORE.with(|c| c.set(Some(std::time::Instant::now())));
            // SAFETY: posting to our own primary dock window.
            unsafe {
                let _ = PostMessageW(app.hwnd, WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0));
            }
        }
    } else {
        app.fs_pending = Some(now);
    }
    let released = app.remote_hidden || app.rdp_front;
    if released != app.docks_released {
        set_docks_released(app, released);
    }
}

/// Explorer restarted: tray listener off, taskbars hidden once, AppBars registered with the
/// new Explorer, then the listener back on (if enabled).
fn reattach_to_explorer(primary: HWND) {
    // Released docks hold no AppBar; bringing them back registers them with the new Explorer.
    if APP.with(|a| a.try_borrow().ok().and_then(|b| b.as_ref().map(|app| app.docks_released)).unwrap_or(false)) {
        log("explorer: re-attach skipped while the docks are released");
        return;
    }
    let tray_enabled = APP.with(|a| {
        a.try_borrow().ok().and_then(|b| b.as_ref().map(|app| app.shared.settings.lock().unwrap().tray_service)).unwrap_or(false)
    });
    win::traysvc::shutdown();
    appbar::enforce_taskbar_hidden();
    unsafe {
        // Synchronous: ABM_REMOVE + ABM_NEW for the primary, secondaries rebuilt.
        SendMessageW(primary, WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0));
    }
    LAST_REREGISTER.with(|c| c.set(Some(std::time::Instant::now())));
    WORK_AREA_MISSES.with(|m| m.borrow_mut().clear());
    appbar::enforce_taskbar_hidden();
    let tray_enabled = tray_enabled && !TRAY_OFF_BY_GUARD.with(Cell::get);
    if tray_enabled {
        win::traysvc::start();
    }
    log(&format!("explorer: re-attached (AppBars re-registered, tray listener {})", if tray_enabled { "restarted" } else { "off" }));
}

/// Keeps maximized windows above every dock. A missing reservation is first repaired the
/// normal way (re-register the AppBars); only if that does not hold is the work area set
/// directly, at most every 5 s so we never fight Explorer in a tight loop (each set makes
/// Explorer re-lay-out its taskbars, which flickers). If misses keep coming while our tray
/// listener runs, the listener is turned off.
fn guard_work_area(primary: HWND) {
    // The docks release their AppBars on purpose (remote session, fullscreen RDP), and for a moment
    // after they come back Explorer is still settling. A missing reservation is expected then;
    // repairing it re-registers the AppBars and makes Explorer re-lay out the taskbar.
    let released = APP.with(|a| a.try_borrow().ok().and_then(|b| b.as_ref().map(|app| app.docks_released || !app.fs_hidden.is_empty()))).unwrap_or(false);
    // Right after the docks come back, Explorer often reports the reservation missing for a few
    // seconds. Repairing it then re-attaches to Explorer, which flickers and has crashed; leave it.
    let settling = LAST_RESTORE.with(|c| c.get()).is_some_and(|t| t.elapsed() < RESTORE_SETTLE);
    if released || settling {
        WORK_AREA_MISSES.with(|m| m.borrow_mut().clear());
        return;
    }
    let docks: Vec<HWND> = APP.with(|a| {
        a.try_borrow()
            .ok()
            .and_then(|b| b.as_ref().map(|app| std::iter::once(app.hwnd).chain(app.secondaries.iter().map(|s| s.hwnd)).collect()))
            .unwrap_or_default()
    });
    let missing: Vec<HWND> = docks.into_iter().filter(|&h| appbar::work_area_missing(h)).collect();
    if missing.is_empty() {
        return;
    }
    let now = std::time::Instant::now();
    let misses = WORK_AREA_MISSES.with(|m| {
        let mut m = m.borrow_mut();
        m.push(now);
        m.retain(|t| now.duration_since(*t).as_secs() < 30);
        m.len()
    });
    if misses >= 8 && win::traysvc::running() {
        log("tray: work area keeps losing the dock reservation while the listener runs; turning the listener off for this session");
        TRAY_OFF_BY_GUARD.with(|c| c.set(true));
        win::traysvc::shutdown();
    }
    let since = |c: &'static std::thread::LocalKey<Cell<Option<std::time::Instant>>>| {
        c.with(|c| c.get()).map_or(u64::MAX, |t| now.duration_since(t).as_secs())
    };
    if since(&LAST_REREGISTER) >= 15 {
        LAST_REREGISTER.with(|c| c.set(Some(now)));
        let since_restore = LAST_RESTORE.with(|c| c.get()).map_or(-1.0, |t| t.elapsed().as_secs_f32());
        log(&format!("workarea: dock reservation missing ({since_restore:.1} s after the docks came back); re-registering the AppBars"));
        // AppBars must be registered while Explorer owns Shell_TrayWnd, so this also
        // pauses our tray listener around the registration.
        reattach_to_explorer(primary);
        return;
    }
    if since(&LAST_REREGISTER) >= 2 && since(&LAST_FORCE) >= 5 {
        LAST_FORCE.with(|c| c.set(Some(now)));
        for h in missing {
            appbar::ensure_work_area(h);
        }
    }
}

fn shutdown() {
    // Hand the tray back to Explorer before the AppBars are removed.
    win::traysvc::shutdown();
    data::usage::flush();
    data::screentime::flush();
    popup::hide();
    OPEN.with(|o| o.set(None));
    let secondary_hwnds = APP.with(|a| {
        if let Some(mut app) = a.borrow_mut().take() {
            unsafe {
                // SAFETY: hwnd is still valid until DestroyWindow in the caller.
                let _ = KillTimer(app.hwnd, TIMER_REFRESH);
                let _ = KillTimer(app.hwnd, TIMER_GUARD);
                let _ = KillTimer(app.hwnd, TIMER_VISUALIZER);
                let _ = KillTimer(app.hwnd, TIMER_STOPWATCH);
                let _ = UnregisterHotKey(app.hwnd, HOTKEY_TOGGLE);
            }
            tasks::unregister_shell_hook(app.hwnd);
            appbar::unregister_appbar(app.hwnd);
            take_secondary_dock_hwnds(&mut app.secondaries)
        } else {
            Vec::new()
        }
    });
    destroy_secondary_hwnds(&secondary_hwnds);
    appbar::ensure_restored_on_exit();
}

fn on_shell_event(event: ShellEvent) {
    APP.with(|a| {
        let Ok(mut slot) = a.try_borrow_mut() else { return };
        let Some(app) = slot.as_mut() else { return };
        match event {
            ShellEvent::Created(_) | ShellEvent::Destroyed(_) => app.apps.refresh(&app.attention),
            ShellEvent::Flash(h) => app.attention.mark(h),
            ShellEvent::Activated(h) => {
                app.attention.clear(h);
                tasks::note_foreign_foreground(h);
            }
            ShellEvent::Other => {}
        }
    });
    repaint();
}

fn client_point(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xffff) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
    let dpi_value = APP.with(|a| a.borrow().as_ref().map(|app| app.dpi).unwrap_or(96));
    (dpi::px_to_dip(x, dpi_value), dpi::px_to_dip(y, dpi_value))
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Returning TRUE from WM_NCCREATE tells Windows to continue creating the window. Letting
    // DefWindowProc answer it made CreateWindowExW fail for this window class.
    if msg == WM_NCCREATE {
        return LRESULT(1);
    }
    let taskbar_created = taskbar_created_msg();
    if taskbar_created != 0 && msg == taskbar_created {
        let is_primary = APP.with(|a| a.try_borrow().ok().and_then(|b| b.as_ref().map(|app| app.hwnd == hwnd)).unwrap_or(false));
        if is_primary && !win::traysvc::own_broadcast_recent() {
            // A new Explorer: wait until it has finished building its taskbars (debounced),
            // then hide them once, register the AppBars with it and restart the tray listener.
            log("explorer: TaskbarCreated from a new Explorer; re-attaching in 2 s");
            unsafe {
                SetTimer(hwnd, TIMER_EXPLORER, 2000, None);
            }
        }
        return LRESULT(0);
    }
    let shell_msg = SHELL_MSG.with(Cell::get);
    if shell_msg != 0 && msg == shell_msg {
        if let Some(event) = tasks::parse_shell_message(wparam.0, lparam.0) {
            on_shell_event(event);
        }
        return LRESULT(0);
    }
    match msg {
        WM_TIMER => {
            match wparam.0 {
                TIMER_REFRESH => {
                    APP.with(|a| {
                        if let Ok(mut slot) = a.try_borrow_mut() {
                            if let Some(app) = slot.as_mut() {
                                let mut current_settings = app.shared.settings.lock().unwrap();
                                if let Some(disk_settings) = SETTINGS_SEEN.with(|s| settings::load_if_changed(&mut s.borrow_mut())) {
                                    if **current_settings != disk_settings {
                                        appbar::set_hidden_mode(disk_settings.taskbar_mode == "hidden");
                                        *current_settings = Arc::new(disk_settings);
                                        app.shared.refresh_slow.store(true, Ordering::SeqCst);
                                    }
                                }
                                data::usage::tick();
                                app.apps.pins = pins_of(&current_settings);
                                drop(current_settings);
                                app.apps.refresh(&app.attention);
                                app.stats = data::stats::read_stats();
                                app.history.push(&app.stats);
                                let playing = app
                                    .shared
                                    .data
                                    .lock()
                                    .ok()
                                    .and_then(|data| data.media.as_ref().map(|media| media.playing))
                                    .unwrap_or(false);
                                if playing != app.visualizer_running {
                                    app.visualizer_running = playing;
                                    if playing {
                                        SetTimer(hwnd, TIMER_VISUALIZER, 33, None);
                                    } else {
                                        let _ = KillTimer(hwnd, TIMER_VISUALIZER);
                                    }
                                }
                            }
                        }
                    });
                    if data::focus::tick() {
                        // A focus or break session just ended.
                        unsafe {
                            let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
                        }
                    }
                    refresh_open_popup();
                    repaint();
                }
                TIMER_EXPLORER => {
                    let _ = KillTimer(hwnd, TIMER_EXPLORER);
                    reattach_to_explorer(hwnd);
                }
                TIMER_GUARD => {
                    APP.with(|a| {
                        if let Ok(mut slot) = a.try_borrow_mut() {
                            if let Some(app) = slot.as_mut() {
                                update_remote_state(app);
                            }
                        }
                    });
                    appbar::enforce_taskbar_hidden();
                    guard_work_area(hwnd);
                }
                TIMER_VISUALIZER => repaint(),
                TIMER_STOPWATCH => {
                    let (_,running,_) = data::productivity::watch();
                    if running {
                        let visible = APP.with(|a| a.try_borrow().ok().is_some_and(|slot| slot.as_ref().is_some_and(|app| app.shared.settings.lock().unwrap().tiles.stopwatch)));
                        if visible { repaint(); }
                        if matches!(OPEN.with(Cell::get),Some(OpenPopup::Widget("stopwatch")|OpenPopup::Gallery)) { popup::invalidate(); }
                    }
                }
                id if id == thumbnails::TIMER_THUMB_DELAY || id == thumbnails::TIMER_THUMB_POLL => {
                    thumbnails::on_dock_timer(hwnd, id);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_TOGGLE => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.dock_visible = !app.dock_visible;
                    if app.dock_visible {
                        let hidden = app.shared.settings.lock().unwrap().taskbar_mode == "hidden";
                        appbar::hide_taskbar(hidden);
                        // While released, the docks stay hidden; they show when the release ends.
                        if !app.docks_released {
                            let _ = ShowWindow(app.hwnd, SW_SHOWNOACTIVATE);
                            for sec in &app.secondaries {
                                let _ = ShowWindow(sec.hwnd, SW_SHOWNOACTIVATE);
                            }
                        }
                    } else {
                        let _ = ShowWindow(app.hwnd, SW_HIDE);
                        for sec in &app.secondaries {
                            let _ = ShowWindow(sec.hwnd, SW_HIDE);
                        }
                        appbar::restore_taskbar();
                    }
                }
            });
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: standard paint cycle for our own window.
            BeginPaint(hwnd, &mut ps);
            repaint();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // Snapshot foreground + Start state before the click can change them.
            tasks::capture_click_state();
            let (x, y) = client_point(lparam);
            log(&format!("[{}] down at ({x},{y})", chrono::Local::now().format("%H:%M:%S%.3f")));
            let tiles_now = dock_tiles_of(hwnd);
            if tiles_now.as_ref().is_some_and(|t| on_speaker(t, x, y)) {
                // Dragging sideways on the speaker changes the volume; a plain click still opens the panel.
                VOL_DRAG.with(|d| d.set(Some((x, current_volume(), false))));
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
            } else if let Some(id) = tiles_now.and_then(|t| widget_id_at(&t, x, y)) {
                DRAG.with(|d| *d.borrow_mut() = Some(Drag { id, start_x: x, active: false }));
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONUP => {
            let (x, y) = client_point(lparam);
            // Finish the drag first: releasing the capture sends WM_CAPTURECHANGED, which cancels one.
            let vol_dragged = VOL_DRAG.with(|d| d.take()).is_some_and(|(_, _, moved)| moved);
            let dragged = drag_finish(hwnd, x) || vol_dragged;
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
            if dragged {
                return LRESULT(0);
            }
            let click = dock_click_on(hwnd, x, y);
            log(&format!("[{}] click at ({x},{y}) -> {}", chrono::Local::now().format("%H:%M:%S%.3f"), match &click {
                Click::None => "none",
                Click::Activate(_) => "activate",
                Click::Launch(_) => "launch pinned",
                Click::Start => "open Start",
                Click::GrokBot => "grok bot",
                Click::Search => "search",
                Click::Open(..) => "open popup",
                Click::TrayOverflow(_) => "tray overflow",
                Click::Action(_) => "widget action",
            }));
            // A click on an icon acts on the app itself; a lingering hover peek must not stay up
            // and catch the next click (it would focus the window instead of toggling it).
            if matches!(click, Click::Activate(_) | Click::Launch(_)) {
                thumbnails::cancel_pending(hwnd);
                thumbnails::hide();
            }
            match click {
                Click::None => tasks::dismiss_start_if_open(),
                Click::Activate(h) => tasks::activate_window(h),
                Click::Launch(exe) => tasks::launch_exe(&exe),
                Click::Start => tasks::open_start_menu(),
                Click::Search => tasks::open_search(),
                Click::GrokBot => grokbot::open_or_focus(),
                Click::Open(which, anchor) => open_popup(which, anchor),
                Click::Action(id) => run_widget_action(id),
                Click::TrayOverflow(a) => {
                    let was_open = OPEN.with(Cell::get) == Some(OpenPopup::Tray);
                    log(&format!("overflow: ^ clicked -> {}", if was_open { "close" } else { "open own popup" }));
                    open_popup(OpenPopup::Tray, a);
                    repaint();
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = client_point(lparam);
            open_app_context_menu(hwnd, x, y);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_point(lparam);
            if let Some((x0, v0, moved)) = VOL_DRAG.with(Cell::get) {
                if wparam.0 & 1 == 0 {
                    VOL_DRAG.with(|d| d.set(None));
                } else {
                    let moved = moved || (x - x0).abs() > DRAG_THRESHOLD;
                    VOL_DRAG.with(|d| d.set(Some((x0, v0, moved))));
                    if moved {
                        set_volume_live(v0 + (x - x0) * VOLUME_PER_DIP);
                        return LRESULT(0);
                    }
                }
            }
            if drag_mouse_move(hwnd, x, wparam.0 & 1 != 0) {
                return LRESULT(0);
            }
            // Ask for WM_MOUSELEAVE; Windows cancels the request after each leave.
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: tme is a valid local struct for the call.
            let _ = TrackMouseEvent(&mut tme);
            // Only this dock repaints, and only when something visible changes: the hovered
            // tile, or icon magnification near the apps zone (also once after leaving it).
            apps::set_pointer(Some(x));
            let hover_changed = set_hover_on(hwnd, Some((x, y)));
            let near = near_apps(hwnd, x);
            let was_near = NEAR_APPS.with(|c| c.replace(near));
            update_app_thumbnails(hwnd, x, y);
            if hover_changed || near || was_near {
                repaint_docks(Some(hwnd));
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // Answer hit tests ourselves: DefWindowProc returned HTNOWHERE here, so the dock was
            // skipped by mouse input and clicks reached the desktop instead.
            LRESULT(HTCLIENT as isize)
        }
        WM_MOUSEWHEEL => {
            if volume_wheel(hwnd, wparam, lparam) {
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CAPTURECHANGED => {
            VOL_DRAG.with(|d| d.set(None));
            // Capture taken away mid-drag (Alt+Tab, another popup): drop the drag without applying it.
            if DRAG.with(|d| d.borrow_mut().take()).is_some_and(|d| d.active) {
                DROP.with(|d| d.set(None));
                repaint();
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            NEAR_APPS.with(|c| c.set(false));
            apps::set_pointer(None);
            set_hover_on(hwnd, None);
            thumbnails::cancel_pending(hwnd);
            // Keep peeks if the pointer moved onto the peek host; poll handles leave.
            repaint();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // A sent WM_DISPLAYCHANGE can arrive while a timer handler holds APP (its own
            // ShowWindow calls pump sent messages): handle it again once that has finished.
            if APP.with(|a| a.try_borrow().is_err()) {
                unsafe {
                    let _ = PostMessageW(hwnd, WM_DISPLAYCHANGE, wparam, lparam);
                }
                return LRESULT(0);
            }
            // Only the primary dock owns topology rebuilds; secondary HWNDs also receive this.
            let is_primary = APP.with(|a| a.borrow().as_ref().map(|app| app.hwnd == hwnd).unwrap_or(false));
            if is_primary {
                // Released docks hold no AppBar and stay hidden: they are rebuilt when they come back.
                let released = APP.with(|a| {
                    let mut slot = a.borrow_mut();
                    slot.as_mut().map(|app| {
                        // A fullscreen game switching modes fires bursts of these: rebuild after it ends.
                        let hold = app.docks_released || !app.fs_hidden.is_empty();
                        if hold {
                            app.topology_stale = true;
                        }
                        hold
                    })
                });
                if released == Some(true) {
                    log("display change while the docks are released: rebuilt when they come back");
                    return LRESULT(0);
                }
                let mut stale = Vec::new();
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.topology_stale = false;
                        // Full re-registration: after an Explorer restart the new Explorer does
                        // not know the old AppBar, so a reposition alone reserves nothing.
                        appbar::unregister_appbar(hwnd);
                        if let Some(rect) = attach_primary(app) {
                            app.shared.refresh_liquid.store(true, Ordering::SeqCst);
                            log(&format!(
                                "display change: appbar=({},{}..{},{}) dpi={}",
                                rect.left, rect.top, rect.right, rect.bottom, app.dpi
                            ));
                            stale = take_secondary_dock_hwnds(&mut app.secondaries);
                        }
                    }
                });
                destroy_secondary_hwnds(&stale);
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        let hinst: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
                            Ok(h) => h.into(),
                            Err(_) => return,
                        };
                        let class = w!("SmartDockDock");
                        let opacity = app.shared.settings.lock().unwrap().glass_opacity;
                        app.secondaries = build_secondary_docks(hinst, class, app.hwnd, DOCK_HEIGHT);
                        for sec in &app.secondaries {
                            glass::apply_dock_glass(sec.hwnd, opacity);
                        }
                    }
                });
                appbar::enforce_taskbar_hidden();
                repaint();
                APP.with(|a| {
                    if let Some(app) = a.borrow().as_ref() {
                        show_secondary_docks(&app.secondaries);
                    }
                });
                appbar::ensure_secondary_trays_visible();
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            appbar::enforce_taskbar_hidden();
            LRESULT(0)
        }
        WM_ENDSESSION | WM_CLOSE => {
            let is_primary = APP.with(|a| a.borrow().as_ref().map(|app| app.hwnd == hwnd).unwrap_or(true));
            if is_primary {
                shutdown();
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // Secondary docks are destroyed during rebuild while APP may already be borrowed.
            // Only end the message loop for the primary (or after shutdown cleared APP).
            let quit = APP
                .try_with(|a| match a.try_borrow() {
                    Ok(slot) => match slot.as_ref() {
                        None => true, // shutdown already took App
                        Some(app) => app.hwnd == hwnd,
                    },
                    Err(_) => false, // nested borrow during rebuild
                })
                .unwrap_or(false);
            if quit {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
