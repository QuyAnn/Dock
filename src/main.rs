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
use ui::popup_content::{self, MediaAction as PopupMedia, PopupKind};
use ui::renderer::{apply_dwm_rounding, Renderer};
use ui::svgpath;
use ui::thumbnails;
use ui::tiles::{self, DataState};
use win::appbar::{self, AbEdge};
use win::dpi;
use win::grokbot;
use win::system::{self, MediaAction};
use win::tasks::{self, Attention, ShellEvent};
use win::{monitors, wallpaper};

const DOCK_HEIGHT: f32 = 46.0;
const POPUP_W: f32 = 320.0;
const CONTROL_POPUP_W: f32 = 340.0;
const SETTINGS_POPUP_W: f32 = 460.0;
/// Height used when the popup content cannot be measured yet.
const POPUP_H_DEFAULT: f32 = 260.0;
const TIMER_REFRESH: usize = 1; // 1 s: apps, stats, popup refresh, repaint
const TIMER_GUARD: usize = 2; // 500 ms: keep the Windows taskbar hidden
const TIMER_VISUALIZER: usize = 3; // ~30 fps, enabled only while media is playing
const HOTKEY_TOGGLE: i32 = 1;

/// Data shared with the background refresh thread. Blocking calls run there, never on the UI thread.
struct Shared {
    data: Mutex<DataState>,
    control: Mutex<ControlState>,
    settings: Mutex<settings::Settings>,
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
    /// One dock per additional monitor; empty on single-display machines.
    secondaries: Vec<SecondaryDock>,
}

#[derive(Clone, Copy, PartialEq)]
enum OpenPopup {
    Data(&'static str),
    Control,
    AppMenu,
}

/// Click resolved on the UI thread; side effects run after the APP borrow is released.
enum Click {
    None,
    Activate(isize),
    Start,
    GrokBot,
    Open(OpenPopup, RectF),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
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
    thread::spawn(move || {
        let mut tick: u64 = 0;
        loop {
            // Control state every 2 s (volume, mute, radios, brightness).
            if tick % 2 == 0 {
                let c = ControlState::read();
                *shared.control.lock().unwrap() = c;
            }
            // Media every 3 s.
            if tick % 3 == 0 {
                let mut local = shared.data.lock().unwrap().clone();
                local.refresh_media();
                *shared.data.lock().unwrap() = local;
            }
            // Git, AI usage and weather every 10 min (first pass runs immediately).
            if tick % 600 == 0 || shared.refresh_slow.swap(false, Ordering::SeqCst) {
                let current = shared.settings.lock().unwrap().clone();
                let repos = if current.git_folders.is_empty() { default_repos() } else { current.git_folders };
                let mut local = shared.data.lock().unwrap().clone();
                local.refresh_slow(&repos, &current.git_author, &current.weather_location);
                *shared.data.lock().unwrap() = local;
            }
            // Liquid Glass backdrop: one crop per monitor, refreshed when wallpaper/topology changes.
            if tick % 15 == 0
                && (tick == 0
                    || shared.refresh_liquid.swap(false, Ordering::SeqCst)
                    || wallpaper::wallpaper_changed())
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
                        *shared.liquid.lock().unwrap() = map;
                        log(&format!("liquid backdrop rendered for {n} monitor(s)"));
                    }
                    Err(e) => log(&format!("wallpaper load failed: {e}")),
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
        apply_dwm_rounding(hwnd);
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
    // Secondary trays stay visible; docks are additive.
    appbar::set_secondary_docks_active(false);
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
    // 46-DIP dock surface is visible; its upper corners retain compositor antialiasing.
    let dock_window_height_px = dock_height_px + corner_px;

    // Remove the taskbar's edge claim before asking the shell to place our AppBar. Otherwise
    // ABM_QUERYPOS can return a rect one taskbar-height above rcMonitor.bottom.
    appbar::hide_taskbar(initial_settings.taskbar_mode == "hidden");

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
    apply_dwm_rounding(hwnd);
    glass::apply_dock_glass(hwnd, initial_settings.glass_opacity);

    let shell_msg = tasks::register_shell_hook(hwnd);
    SHELL_MSG.with(|m| m.set(shell_msg));

    popup::set_painter(paint_popup);
    popup::set_click_handler(on_popup_click);
    popup::set_on_hide(on_popup_hidden);

    let shared = Arc::new(Shared {
        data: Mutex::new(DataState::default()),
        control: Mutex::new(ControlState::read()),
        settings: Mutex::new(initial_settings.clone()),
        liquid: Mutex::new(HashMap::new()),
        refresh_liquid: AtomicBool::new(false),
        refresh_slow: AtomicBool::new(false),
        audio_peak: AtomicU32::new(0.0f32.to_bits()),
    });
    start_background_refresh(shared.clone(), dock_height_px);
    start_audio_meter(shared.clone());

    let mut apps = AppsState::default();
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

    unsafe {
        // SAFETY: hwnd is live; timers drive refresh and the taskbar guard.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_REFRESH, 1000, None);
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

fn layout_tiles(app_count: usize, settings: &settings::Settings) -> Zones {
    // Tight to the drawn slots: padding here would push the visible cluster off the centre.
    let apps_w = apps::cluster_width(app_count);
    let t = |id, width| Tile { id, width, height: 0.0 };
    let mut left = Vec::new();
    let enabled = &settings.tiles;
    if enabled.git { left.push(t("git", 118.0)); }
    if enabled.claude { left.push(t("claude", 100.0)); }
    if enabled.codex { left.push(t("codex", 100.0)); }
    if enabled.cursor { left.push(t("cursor", 155.0)); }
    if enabled.grok { left.push(t("grok", 100.0)); }
    // Base width; layout() expands `media` into leftover side space toward the center cluster.
    if enabled.music { left.push(t("media", 200.0)); }
    if enabled.weather && settings.weather_position == "tile" { left.push(t("weather", 100.0)); }
    let mut right = vec![t("cc", 56.0), t("gear", 22.0)];
    // Wide enough for a tiny CPU/RAM label above the percent inside the ring.
    if enabled.cpu { right.push(t("cpu", 42.0)); }
    if enabled.ram { right.push(t("ram", 42.0)); }
    right.push(t("clock", 64.0));
    Zones {
        left,
        center: if app_count == 0 {
            vec![t("start", 38.0)]
        } else {
            vec![t("start", 38.0), Tile { id: "apps", width: apps_w, height: 44.0 }]
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
        Some(a) => (short_ai_status(&a.reset_text).into(), String::new(), None, 0),
        None => ("…".into(), String::new(), None, 0),
    };
    TileView { progress, accent, bg, sub, glyph: Some(glyph), ..TileView::card(label, value) }
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

fn grok_bot_view() -> TileView {
    let running = grokbot::is_running();
    let mut view = TileView {
        progress: None,
        accent: 0,
        bg: 0x0000_00FF,
        glyph: Some((svgpath::GROK_SLASH, Some(2.0))),
        ..TileView::card(grokbot::DISPLAY_NAME, if running { "Đang chạy" } else { "Mở" })
    };
    if let Some(icon) = grokbot::icon() {
        view.icon = Some(dock::Icon { w: icon.w, h: icon.h, rgba: icon.rgba });
        view.glyph = None;
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
    let now = Local::now();
    match id {
        "git" => tiles::git_tile(data),
        "claude" => ai_view(data, "Claude"),
        "codex" => ai_view(data, "Codex"),
        "cursor" => ai_view(data, "Cursor"),
        "grok" => grok_bot_view(),
        "media" => {
            let mut view = tiles::media_tile(data);
            view.audio_peak = audio_peak;
            view
        }
        "weather" => tiles::weather_tile(data),
        // .cc-btn: Wi-Fi and volume glyphs (Segoe MDL2 Assets).
        "cc" => TileView {
            style: Style::Button,
            value: "\u{E701}\u{E767}".into(),
            progress: control.wifi.map(|on| if on { 1.0 } else { 0.0 }),
            ..TileView::card("", "")
        },
        "start" => TileView { style: Style::Start, ..TileView::card("", "") },
        "gear" => TileView::bare("\u{E713}"),
        // Ring: short label above the percent (no "CPU 65" single-line overlap).
        "cpu" => TileView {
            style: Style::Ring,
            progress: Some(stats.cpu_pct / 100.0),
            accent: gauge_color(stats.cpu_pct),
            ..TileView::card("CPU", format!("{:.0}", stats.cpu_pct))
        },
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
    backdrop: Option<&LiquidBitmap>,
) {
    let zones = layout_tiles(apps_state.windows.len(), settings);
    let rects = dock::layout(&zones.left, &zones.center, &zones.right, width);
    let defs: Vec<&Tile> = zones.left.iter().chain(&zones.center).chain(&zones.right).collect();
    let views: Vec<TileView> = defs
        .iter()
        .map(|t| tile_view(t.id, data, control, stats, audio_peak, settings))
        .collect();
    *tiles_out = defs.iter().zip(rects.iter()).map(|(t, r)| (t.id, *r)).collect();
    let apps_rect = tiles_out.iter().find(|(id, _)| *id == "apps").map(|(_, r)| *r);
    let res = renderer
        .begin()
        .and_then(|_| dock::draw_dock(renderer, &views, &rects, hover, backdrop, settings.glass_opacity))
        .and_then(|_| match apps_rect {
            Some(r) => apps::draw_apps(renderer, r, apps_state),
            None => Ok(()),
        })
        .and_then(|_| renderer.end());
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
            primary_bg.as_deref(),
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
                bg.as_deref(),
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

fn open_app_context_menu(hwnd: HWND, x: f32, y: f32) {
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
        let (_, rect) = tiles.iter().find(|(id, r)| *id == "apps" && dock::hit_test(x, y, &[*r]).is_some())?;
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
        open_popup(OpenPopup::AppMenu, anchor);
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
            .map(|e| (e.hwnd, e.title.clone()))
            .collect();
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
        let Some((id, rect)) = tiles.iter().find(|(_, r)| dock::hit_test(x, y, &[*r]).is_some()).copied() else {
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
            "apps" => apps::hit_app(x, y, rect, &app.apps).map_or(Click::None, Click::Activate),
            "start" => Click::Start,
            "git" => Click::Open(OpenPopup::Data("git"), anchor),
            "claude" | "codex" | "cursor" => Click::Open(OpenPopup::Data(id), anchor),
            "grok" => Click::GrokBot,
            "media" => Click::Open(OpenPopup::Data("media"), anchor),
            "weather" => Click::Open(OpenPopup::Data("weather"), anchor),
            "clock" => Click::Open(OpenPopup::Data("clock"), anchor),
            "cpu" => Click::Open(OpenPopup::Data("cpu"), anchor),
            "ram" => Click::Open(OpenPopup::Data("ram"), anchor),
            "cc" => Click::Open(OpenPopup::Control, anchor),
            "gear" => Click::Open(OpenPopup::Data("settings"), anchor),
            _ => Click::None,
        }
    })
}

fn popup_kind(id: &str) -> PopupKind {
    match id {
        "git" => PopupKind::Git,
        "claude" | "codex" | "cursor" | "ai" => PopupKind::Ai,
        "weather" => PopupKind::Weather,
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
    if OPEN.with(Cell::get) == Some(which) {
        popup::hide();
        OPEN.with(|o| o.set(None));
        return;
    }
    match which {
        OpenPopup::Data(id) => {
            let kind = popup_kind(id);
            popup_content::set_ai_provider(match id {
                "claude" => Some("Claude"),
                "codex" => Some("Codex"),
                "cursor" => Some("Cursor"),
                _ => None,
            });
            let height = APP
                .with(|a| {
                    let slot = a.borrow();
                    let app = slot.as_ref()?;
                    let data = app.shared.data.lock().unwrap().clone();
                    feed_popup(kind, &data, &app.history);
                    popup_content::set_settings(app.shared.settings.lock().unwrap().clone());
                    Some(popup_content::content_height(kind, &data))
                })
                .unwrap_or(POPUP_H_DEFAULT);
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
            POPUP_H_CUR.with(|h| h.set(height));
            let width = if kind == PopupKind::Settings { SETTINGS_POPUP_W } else { POPUP_W };
            POPUP_W_CUR.with(|w| w.set(width));
            if popup::show(anchor, width, height as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
        }
        OpenPopup::Control => {
            let state = APP.with(|a| a.borrow().as_ref().map(|app| app.shared.control.lock().unwrap().clone()));
            CONTROL_VIEW.with(|c| *c.borrow_mut() = state);
            POPUP_W_CUR.with(|w| w.set(CONTROL_POPUP_W));
            if popup::show(anchor, CONTROL_POPUP_W, control_center::HEIGHT as i32).is_ok() {
                OPEN.with(|o| o.set(Some(which)));
            }
        }
        OpenPopup::AppMenu => {
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
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

/// Keeps an open data popup live: fresh data and history each second, then a repaint of the popup.
fn refresh_open_popup() {
    let Some(OpenPopup::Data(id)) = OPEN.with(Cell::get) else { return };
    APP.with(|a| {
        let slot = a.borrow();
        let Some(app) = slot.as_ref() else { return };
        let data = app.shared.data.lock().unwrap().clone();
        let kind = popup_kind(id);
        feed_popup(kind, &data, &app.history);
        if kind == PopupKind::Settings {
            popup_content::set_settings(app.shared.settings.lock().unwrap().clone());
        }
    });
    popup::invalidate();
}

fn paint_popup(r: &mut Renderer, rect: RectF) {
    if OPEN.with(Cell::get) == Some(OpenPopup::AppMenu) {
        let _ = appmenu::paint(r, rect);
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
    if OPEN.with(Cell::get) == Some(OpenPopup::AppMenu) {
        if let Some(action) = appmenu::hit_test(x, y) {
            APP.with(|a| {
                let slot = a.borrow();
                let Some(app) = slot.as_ref() else { return };
                let mut settings = app.shared.settings.lock().unwrap();
                appmenu::apply(action, &mut settings);
                let _ = settings::save(&settings);
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
            // Control actions can block (WinRT, COM); run them off the UI thread.
            thread::spawn(move || {
                let _ = control_center::apply(action);
            });
        }
        return;
    }
    if OPEN.with(Cell::get) == Some(OpenPopup::Data("settings")) {
        if let Some(action) = popup_content::hit_settings(x, y, rect) {
            APP.with(|a| {
                let slot = a.borrow();
                let Some(app) = slot.as_ref() else { return };
                let mut settings = app.shared.settings.lock().unwrap();
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
                    popup_content::SettingsAction::OpenConfig => {
                        let _ = settings::save(&settings);
                        if let Some(path) = settings::path() {
                            let _ = system::open_path(&path);
                        }
                    }
                }
                let _ = settings::save(&settings);
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
        thread::spawn(move || {
            let _ = system::media_control(action);
        });
    }
}

fn on_popup_hidden() {
    OPEN.with(|o| o.set(None));
    CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
    appmenu::clear();
}

fn shutdown() {
    popup::hide();
    OPEN.with(|o| o.set(None));
    let secondary_hwnds = APP.with(|a| {
        if let Some(mut app) = a.borrow_mut().take() {
            unsafe {
                // SAFETY: hwnd is still valid until DestroyWindow in the caller.
                let _ = KillTimer(app.hwnd, TIMER_REFRESH);
                let _ = KillTimer(app.hwnd, TIMER_GUARD);
                let _ = KillTimer(app.hwnd, TIMER_VISUALIZER);
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
                                let disk_settings = settings::load();
                                let mut current_settings = app.shared.settings.lock().unwrap();
                                if *current_settings != disk_settings {
                                    appbar::set_hidden_mode(disk_settings.taskbar_mode == "hidden");
                                    *current_settings = disk_settings;
                                    app.shared.refresh_slow.store(true, Ordering::SeqCst);
                                }
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
                    refresh_open_popup();
                    repaint();
                }
                TIMER_GUARD => appbar::enforce_taskbar_hidden(),
                TIMER_VISUALIZER => repaint(),
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
                        let _ = ShowWindow(app.hwnd, SW_SHOWNOACTIVATE);
                        for sec in &app.secondaries {
                            let _ = ShowWindow(sec.hwnd, SW_SHOWNOACTIVATE);
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
        WM_LBUTTONUP => {
            let (x, y) = client_point(lparam);
            let click = dock_click_on(hwnd, x, y);
            log(&format!("click at ({x},{y}) -> {}", match &click {
                Click::None => "none",
                Click::Activate(_) => "activate",
                Click::Start => "open Start",
                Click::GrokBot => "grok bot",
                Click::Open(..) => "open popup",
            }));
            match click {
                Click::None => {}
                Click::Activate(h) => tasks::activate_window(h),
                Click::Start => tasks::open_start_menu(),
                Click::GrokBot => grokbot::open_or_focus(),
                Click::Open(which, anchor) => open_popup(which, anchor),
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
            // Only the primary dock owns topology rebuilds; secondary HWNDs also receive this.
            let is_primary = APP.with(|a| a.borrow().as_ref().map(|app| app.hwnd == hwnd).unwrap_or(false));
            if is_primary {
                let mut stale = Vec::new();
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        let dpi_value = dpi::window_dpi(hwnd);
                        let dock_h = dpi::dip_to_px(DOCK_HEIGHT, dpi_value);
                        if let Ok(rect) = appbar::reposition_appbar(hwnd, AbEdge::Bottom, dock_h) {
                            let pixel_w = rect.right - rect.left;
                            let window_h = dock_h + dpi::dip_to_px(16.0, dpi_value);
                            let _ = SetWindowPos(
                                hwnd, HWND_TOPMOST, rect.left, rect.top, pixel_w, window_h, SWP_NOACTIVATE,
                            );
                            app.origin = (rect.left as f32, rect.top as f32);
                            app.dpi = dpi_value;
                            app.width = dpi::px_to_dip(pixel_w, dpi_value);
                            app.renderer.resize(pixel_w, window_h, dpi_value);
                            app.shared.refresh_liquid.store(true, Ordering::SeqCst);
                            log(&format!(
                                "display change: appbar=({},{}..{},{}) dpi={dpi_value}",
                                rect.left, rect.top, rect.right, rect.bottom
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
