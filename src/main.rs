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
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use data::history::History;
use model::Stats;
use ui::apps::{self, AppsState};
use ui::control_center::{self, ControlState};
use ui::dock::{self, RectF, Style, Tile, TileView};
use ui::glass;
use ui::liquid::{self, LiquidBitmap};
use ui::popup;
use ui::popup_content::{self, MediaAction as PopupMedia, PopupKind};
use ui::renderer::{apply_dwm_rounding, Renderer};
use ui::svgpath;
use ui::tiles::{self, DataState};
use win::appbar::{self, AbEdge};
use win::dpi;
use win::system::{self, MediaAction};
use win::tasks::{self, Attention, ShellEvent};
use win::wallpaper;

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
    /// Liquid Glass backdrop for the dock bar, rendered off the UI thread when the wallpaper changes.
    liquid: Mutex<Option<Arc<LiquidBitmap>>>,
    /// Set by WM_DISPLAYCHANGE so monitor/virtual-screen geometry is sampled again.
    refresh_liquid: AtomicBool,
    /// Peak of the default render endpoint as f32 bits.
    audio_peak: AtomicU32,
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
}

#[derive(Clone, Copy, PartialEq)]
enum OpenPopup {
    Data(&'static str),
    Control,
}

/// Click resolved on the UI thread; side effects run after the APP borrow is released.
enum Click {
    None,
    Activate(isize),
    Start,
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
    if let Some(path) = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("SmartDock").join("settings.json"))
    {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(items) = json.get("gitFolders").and_then(|v| v.as_array()) {
                    let configured: Vec<PathBuf> = items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(PathBuf::from)
                        .filter(|p| p.exists())
                        .collect();
                    if !configured.is_empty() {
                        return configured;
                    }
                }
            }
        }
    }
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for relative in [
            "source\\repos",
            "Documents\\GitHub",
            "Documents\\Projects",
            "Projects",
            "dev",
            "src",
        ] {
            let path = home.join(relative);
            if path.exists() {
                candidates.push(path);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    candidates
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
            if tick % 600 == 0 {
                let current = shared.settings.lock().unwrap().clone();
                let repos = if current.git_folders.is_empty() { default_repos() } else { current.git_folders };
                let mut local = shared.data.lock().unwrap().clone();
                local.refresh_slow(&repos, &current.git_author, &current.weather_location);
                *shared.data.lock().unwrap() = local;
            }
            // Liquid Glass backdrop: load and render on the first pass, then only when the wallpaper changes.
            if tick % 15 == 0
                && (tick == 0
                    || shared.refresh_liquid.swap(false, Ordering::SeqCst)
                    || wallpaper::wallpaper_changed())
            {
                let monitor = appbar::primary_monitor_rect().unwrap_or(RECT {
                    left: 0,
                    top: 0,
                    right: unsafe { GetSystemMetrics(SM_CXSCREEN) },
                    bottom: unsafe { GetSystemMetrics(SM_CYSCREEN) },
                });
                let virtual_screen = RECT {
                    left: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
                    top: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
                    right: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) },
                    bottom: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN) },
                };
                let virtual_size = (virtual_screen.right - virtual_screen.left, virtual_screen.bottom - virtual_screen.top);
                match wallpaper::source_path() {
                    Some(path) => log(&format!("wallpaper candidate={}", path.display())),
                    None => log("wallpaper candidate not found"),
                }
                match wallpaper::load(virtual_size.0.max(monitor.right - monitor.left) as u32) {
                    Ok(wp) => {
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
                            "wallpaper source={} mode={:?} decoded={}x{} original={}x{} monitor=({},{}..{},{}) virtual=({},{}..{},{}) bar=({:.0},{:.0},{:.0},{:.0})",
                            wp.source.display(), wp.mode, wp.w, wp.h, wp.orig_w, wp.orig_h,
                            monitor.left, monitor.top, monitor.right, monitor.bottom,
                            virtual_screen.left, virtual_screen.top, virtual_screen.right, virtual_screen.bottom,
                            bar.x, bar.y, bar.w, bar.h,
                        ));
                        let bitmap = liquid::render_liquid(&wp, bar, screen);
                        *shared.liquid.lock().unwrap() = Some(Arc::new(bitmap));
                        log("liquid backdrop rendered");
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
    thread::spawn(move || loop {
        let playing = shared
            .data
            .lock()
            .ok()
            .and_then(|data| data.media.as_ref().map(|media| media.playing))
            .unwrap_or(false);
        if playing {
            shared.audio_peak.store(system::audio_peak().to_bits(), Ordering::Relaxed);
            thread::sleep(Duration::from_millis(33));
        } else {
            shared.audio_peak.store(0.0f32.to_bits(), Ordering::Relaxed);
            thread::sleep(Duration::from_millis(250));
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
    let y = screen_h - dock_height_px;
    let attempts = [
        ("dock", WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, WS_POPUP, 0, y, screen_w, dock_height_px),
        ("plain popup", WINDOW_EX_STYLE(0), WS_POPUP, 0, y, screen_w, dock_height_px),
        ("overlapped", WINDOW_EX_STYLE(0), WS_OVERLAPPEDWINDOW, 100, 100, 400, 200),
    ];
    let mut created = None;
    for (name, ex, style, x, y, w, h) in attempts {
        // SAFETY: class is registered by the caller; all string args are static literals.
        let res = unsafe {
            SetLastError(WIN32_ERROR(0));
            let res = CreateWindowExW(ex, class, w!("SmartDock"), style, x, y, w, h, None, None, hinst, None);
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
        settings: Mutex::new(initial_settings),
        liquid: Mutex::new(None),
        refresh_liquid: AtomicBool::new(false),
        audio_peak: AtomicU32::new(0.0f32.to_bits()),
    });
    start_background_refresh(shared.clone(), dock_height_px);
    start_audio_meter(shared.clone());

    let mut apps = AppsState::default();
    let attention = Attention::default();
    apps.refresh(&attention);

    let renderer = Renderer::new(hwnd, pixel_width, dock_window_height_px, dpi_value);
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
        })
    });

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
    let apps_w = (16.0 + app_count as f32 * 38.0).max(72.0);
    let t = |id, width| Tile { id, width, height: 0.0 };
    let mut left = Vec::new();
    let enabled = &settings.tiles;
    if enabled.git { left.push(t("git", 118.0)); }
    if enabled.claude { left.push(t("claude", 100.0)); }
    if enabled.codex { left.push(t("codex", 100.0)); }
    if enabled.cursor { left.push(t("cursor", 115.0)); }
    if enabled.grok { left.push(t("grok", 100.0)); }
    if enabled.music { left.push(t("media", 165.0)); }
    if enabled.weather && settings.weather_position == "tile" { left.push(t("weather", 100.0)); }
    let mut right = vec![t("cc", 56.0), t("gear", 22.0)];
    if enabled.cpu { right.push(t("cpu", 42.0)); }
    if enabled.ram { right.push(t("ram", 42.0)); }
    right.push(t("clock", 64.0));
    Zones {
        left,
        center: vec![t("start", 38.0), Tile { id: "apps", width: apps_w, height: 44.0 }],
        right,
    }
}

/// Bar color for AI usage, from `barColor` in AiTiles.tsx.
fn usage_color(pct: f32) -> u32 {
    if pct >= 90.0 {
        0xFF5D_5DFF
    } else if pct >= 70.0 {
        0xFFB3_40FF
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
        "grok" => ai_view(data, "Grok"),
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
        "cpu" => TileView {
            style: Style::Ring,
            progress: Some(stats.cpu_pct / 100.0),
            accent: gauge_color(stats.cpu_pct),
            ..TileView::card("", format!("CPU {:.0}", stats.cpu_pct))
        },
        "ram" => {
            let pct = if stats.ram_total_mb > 0 { stats.ram_used_mb as f32 * 100.0 / stats.ram_total_mb as f32 } else { 0.0 };
            TileView {
                style: Style::Ring,
                progress: Some(pct / 100.0),
                // RAM is blue until it is nearly full (AppSystem tiles use #4aa8ff).
                accent: if pct > 85.0 { 0xFF5D_5DFF } else { 0x4AA8_FFFF },
                ..TileView::card("", format!("RAM {pct:.0}"))
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

fn repaint() {
    APP.with(|a| {
        let mut slot = a.borrow_mut();
        let Some(app) = slot.as_mut() else { return };

        let stats = app.stats.clone();
        let data = app.shared.data.lock().unwrap().clone();
        let control = app.shared.control.lock().unwrap().clone();
        let audio_peak = f32::from_bits(app.shared.audio_peak.load(Ordering::Relaxed));
        let backdrop = app.shared.liquid.lock().unwrap().clone();

        let settings = app.shared.settings.lock().unwrap().clone();
        let zones = layout_tiles(app.apps.windows.len(), &settings);
        let rects = dock::layout(&zones.left, &zones.center, &zones.right, app.width);
        let defs: Vec<&Tile> = zones.left.iter().chain(&zones.center).chain(&zones.right).collect();
        let views: Vec<TileView> = defs
            .iter()
            .map(|t| tile_view(t.id, &data, &control, &stats, audio_peak, &settings))
            .collect();
        app.tiles = defs.iter().zip(rects.iter()).map(|(t, r)| (t.id, *r)).collect();

        let apps_rect = app.tiles.iter().find(|(id, _)| *id == "apps").map(|(_, r)| *r);
        let res = app
            .renderer
            .begin()
            .and_then(|_| dock::draw_dock(
                &mut app.renderer,
                &views,
                &rects,
                app.hover,
                backdrop.as_deref(),
                settings.glass_opacity,
            ))
            .and_then(|_| match apps_rect {
                Some(r) => apps::draw_apps(&mut app.renderer, r, &app.apps),
                None => Ok(()),
            })
            .and_then(|_| app.renderer.end());
        if let Err(e) = res {
            log(&format!("paint failed: {e}"));
        }
    });
}

/// Updates the hovered tile from a pointer position. Returns true when the hovered tile changed.
fn set_hover(hover_at: Option<(f32, f32)>) -> bool {
    APP.with(|a| {
        let Ok(mut slot) = a.try_borrow_mut() else { return false };
        let Some(app) = slot.as_mut() else { return false };
        let rects: Vec<RectF> = app.tiles.iter().map(|(_, r)| *r).collect();
        let next = hover_at.and_then(|(x, y)| dock::hit_test(x, y, &rects));
        let changed = next != app.hover;
        app.hover = next;
        changed
    })
}

fn dock_click(x: f32, y: f32) -> Click {
    APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return Click::None };
        let Some(app) = slot.as_ref() else { return Click::None };
        let Some((id, rect)) = app.tiles.iter().find(|(_, r)| dock::hit_test(x, y, &[*r]).is_some()).copied() else {
            return Click::None;
        };
        let scale = dpi::scale(app.dpi);
        let anchor = RectF {
            x: app.origin.0 + rect.x * scale,
            y: app.origin.1,
            w: rect.w * scale,
            h: DOCK_HEIGHT * scale,
        };
        match id {
            "apps" => apps::hit_app(x, y, rect, &app.apps).map_or(Click::None, Click::Activate),
            "start" => Click::Start,
            "git" => Click::Open(OpenPopup::Data("git"), anchor),
            "claude" | "codex" | "cursor" | "grok" => Click::Open(OpenPopup::Data(id), anchor),
            "media" => Click::Open(OpenPopup::Data("media"), anchor),
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
        "claude" | "codex" | "cursor" | "grok" | "ai" => PopupKind::Ai,
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
                "grok" => Some("Grok"),
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
}

fn shutdown() {
    popup::hide();
    OPEN.with(|o| o.set(None));
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().take() {
            unsafe {
                // SAFETY: hwnd is still valid until DestroyWindow in the caller.
                let _ = KillTimer(app.hwnd, TIMER_REFRESH);
                let _ = KillTimer(app.hwnd, TIMER_GUARD);
                let _ = KillTimer(app.hwnd, TIMER_VISUALIZER);
                let _ = UnregisterHotKey(app.hwnd, HOTKEY_TOGGLE);
            }
            tasks::unregister_shell_hook(app.hwnd);
            appbar::unregister_appbar(app.hwnd);
        }
    });
    appbar::ensure_restored_on_exit();
}

fn on_shell_event(event: ShellEvent) {
    APP.with(|a| {
        let Ok(mut slot) = a.try_borrow_mut() else { return };
        let Some(app) = slot.as_mut() else { return };
        match event {
            ShellEvent::Created(_) | ShellEvent::Destroyed(_) => app.apps.refresh(&app.attention),
            ShellEvent::Flash(h) => app.attention.mark(h),
            ShellEvent::Activated(h) => app.attention.clear(h),
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
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    } else {
                        let _ = ShowWindow(hwnd, SW_HIDE);
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
            let click = dock_click(x, y);
            log(&format!("click at ({x},{y}) -> {}", match &click {
                Click::None => "none",
                Click::Activate(_) => "activate",
                Click::Start => "open Start",
                Click::Open(..) => "open popup",
            }));
            match click {
                Click::None => {}
                Click::Activate(h) => tasks::activate_window(h),
                Click::Start => tasks::open_start_menu(),
                Click::Open(which, anchor) => open_popup(which, anchor),
            }
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
            // The app icons magnify under the pointer, so every move repaints.
            apps::set_pointer(Some(x));
            set_hover(Some((x, y)));
            repaint();
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // Answer hit tests ourselves: DefWindowProc returned HTNOWHERE here, so the dock was
            // skipped by mouse input and clicks reached the desktop instead.
            LRESULT(HTCLIENT as isize)
        }
        WM_MOUSELEAVE => {
            apps::set_pointer(None);
            set_hover(None);
            repaint();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
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
                    }
                }
            });
            appbar::enforce_taskbar_hidden();
            repaint();
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            appbar::enforce_taskbar_hidden();
            LRESULT(0)
        }
        WM_ENDSESSION | WM_CLOSE => {
            shutdown();
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
