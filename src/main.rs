//! SmartDock entry point: AppBar dock window with Direct2D tiles and popups.
//! Startup: recover taskbar from a crash -> create dock -> register AppBar -> hide taskbar.
//! Every exit path (WM_CLOSE, panic, Ctrl+C) restores the taskbar.
mod data;
mod model;
mod ui;
mod win;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Local;
use windows::core::w;
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use model::Stats;
use ui::apps::{self, AppsState};
use ui::control_center::{self, ControlState};
use ui::dock::{self, RectF, Tile, TileView};
use ui::glass;
use ui::popup;
use ui::popup_content::{self, MediaAction as PopupMedia, PopupKind};
use ui::renderer::Renderer;
use ui::tiles::{self, DataState};
use win::appbar::{self, AbEdge};
use win::system::{self, MediaAction};
use win::tasks::{self, Attention, ShellEvent};

const DOCK_HEIGHT: i32 = 56;
const POPUP_W: f32 = 320.0;
const POPUP_H: f32 = 260.0;
const GLASS_OPACITY: u8 = 70;
const TIMER_REFRESH: usize = 1; // 1 s: apps, tiles, repaint
const TIMER_GUARD: usize = 2; // 500 ms: keep the Windows taskbar hidden

/// Data shared with the background refresh thread. Blocking calls run there, never on the UI thread.
struct Shared {
    data: Mutex<DataState>,
    control: Mutex<ControlState>,
}

struct App {
    hwnd: HWND,
    /// Dock top-left in screen coordinates, for popup anchors.
    origin: (f32, f32),
    width: f32,
    renderer: Renderer,
    shared: Arc<Shared>,
    apps: AppsState,
    attention: Attention,
    /// Tile id and rect from the last paint, used for hit testing.
    tiles: Vec<(&'static str, RectF)>,
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
    Open(OpenPopup, RectF),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Control Center state while that popup is open; `None` means a data popup.
    static CONTROL_VIEW: RefCell<Option<ControlState>> = const { RefCell::new(None) };
    static OPEN: Cell<Option<OpenPopup>> = const { Cell::new(None) };
    static SHELL_MSG: Cell<u32> = const { Cell::new(0) };
}

fn default_repos() -> Vec<PathBuf> {
    vec![PathBuf::from(r"D:\PAYMENT6"), PathBuf::from(r"D:\SmartDock")]
}

fn main() {
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
        eprintln!("[smartdock] fatal: {e}");
    }
    appbar::ensure_restored_on_exit();
}

unsafe extern "system" fn console_handler(_ctrl_type: u32) -> BOOL {
    appbar::ensure_restored_on_exit();
    std::process::exit(0);
}

fn start_background_refresh(shared: Arc<Shared>) {
    thread::spawn(move || {
        let repos = default_repos();
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
                let mut local = shared.data.lock().unwrap().clone();
                local.refresh_slow(&repos);
                *shared.data.lock().unwrap() = local;
            }
            tick += 1;
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn run() -> windows::core::Result<()> {
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None)?.into() };
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
        RegisterClassExW(&wc);
    }

    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let hwnd = unsafe {
        // SAFETY: class is registered above; the window is never activated (WS_EX_NOACTIVATE).
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            class,
            w!("SmartDock"),
            WS_POPUP,
            0,
            screen_h - DOCK_HEIGHT,
            screen_w,
            DOCK_HEIGHT,
            None,
            None,
            hinst,
            None,
        )?
    };

    // Reserve the work area at the bottom so maximized windows stop above the dock.
    let rect: RECT = appbar::register_appbar(hwnd, AbEdge::Bottom, DOCK_HEIGHT)?;
    let width = (rect.right - rect.left) as f32;
    unsafe {
        // SAFETY: hwnd is our live window.
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, rect.left, rect.top, width as i32, DOCK_HEIGHT, SWP_NOACTIVATE);
    }
    appbar::hide_taskbar();
    glass::apply_dock_glass(hwnd, GLASS_OPACITY);

    let shell_msg = tasks::register_shell_hook(hwnd);
    SHELL_MSG.with(|m| m.set(shell_msg));

    popup::set_painter(paint_popup);
    popup::set_click_handler(on_popup_click);

    let shared = Arc::new(Shared {
        data: Mutex::new(DataState::default()),
        control: Mutex::new(ControlState::read()),
    });
    start_background_refresh(shared.clone());

    let mut apps = AppsState::default();
    let attention = Attention::default();
    apps.refresh(&attention);

    let renderer = Renderer::new(hwnd, width as i32, DOCK_HEIGHT);
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            hwnd,
            origin: (rect.left as f32, rect.top as f32),
            width,
            renderer,
            shared,
            apps,
            attention,
            tiles: Vec::new(),
        })
    });

    unsafe {
        // SAFETY: hwnd is live; timers drive refresh and the taskbar guard.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_REFRESH, 1000, None);
        SetTimer(hwnd, TIMER_GUARD, 500, None);
    }
    repaint();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Dock tiles in display order. The apps tile width follows the number of open windows.
fn layout_tiles(app_count: usize) -> Vec<Tile> {
    let apps_w = (16.0 + app_count as f32 * 40.0).max(72.0);
    vec![
        Tile { id: "clock", width: 150.0 },
        Tile { id: "apps", width: apps_w },
        Tile { id: "git", width: 130.0 },
        Tile { id: "ai", width: 150.0 },
        Tile { id: "weather", width: 150.0 },
        Tile { id: "media", width: 200.0 },
        Tile { id: "cpu", width: 110.0 },
        Tile { id: "ram", width: 150.0 },
        Tile { id: "vol", width: 110.0 },
    ]
}

fn tile_view(id: &str, data: &DataState, control: &ControlState, stats: &Stats) -> TileView {
    let now = Local::now();
    match id {
        "clock" => TileView {
            title: now.format("%a %d/%m").to_string(),
            value: now.format("%H:%M").to_string(),
            progress: None,
            icon: None,
        },
        "git" => tiles::git_tile(data),
        "ai" => tiles::ai_tile(data),
        "weather" => tiles::weather_tile(data),
        "media" => tiles::media_tile(data),
        "cpu" => TileView {
            title: "CPU".into(),
            value: format!("{:.0}%", stats.cpu_pct),
            progress: Some(stats.cpu_pct / 100.0),
            icon: None,
        },
        "ram" => TileView {
            title: "RAM".into(),
            value: format!("{} / {} MB", stats.ram_used_mb, stats.ram_total_mb),
            progress: (stats.ram_total_mb > 0).then(|| stats.ram_used_mb as f32 / stats.ram_total_mb as f32),
            icon: None,
        },
        "vol" => TileView {
            title: "VOL".into(),
            value: if control.muted { "Muted".into() } else { format!("{:.0}%", control.volume * 100.0) },
            progress: Some(control.volume),
            icon: None,
        },
        // The apps tile is drawn by apps::draw_apps on top of its background.
        _ => TileView { title: String::new(), value: String::new(), progress: None, icon: None },
    }
}

fn repaint() {
    APP.with(|a| {
        let mut slot = a.borrow_mut();
        let Some(app) = slot.as_mut() else { return };

        let stats = data::stats::read_stats();
        let data = app.shared.data.lock().unwrap().clone();
        let control = app.shared.control.lock().unwrap().clone();

        let tile_defs = layout_tiles(app.apps.windows.len());
        let views: Vec<TileView> = tile_defs.iter().map(|t| tile_view(t.id, &data, &control, &stats)).collect();
        let rects = dock::layout(&tile_defs, false, app.width as i32);
        app.tiles = tile_defs.iter().zip(rects.iter()).map(|(t, r)| (t.id, *r)).collect();

        let apps_rect = app.tiles.iter().find(|(id, _)| *id == "apps").map(|(_, r)| *r);
        let res = app
            .renderer
            .begin()
            .and_then(|_| dock::draw_dock(&mut app.renderer, &views, &rects))
            .and_then(|_| match apps_rect {
                Some(r) => apps::draw_apps(&mut app.renderer, r, &app.apps),
                None => Ok(()),
            })
            .and_then(|_| app.renderer.end());
        if let Err(e) = res {
            eprintln!("[smartdock] paint failed: {e}");
        }
    });
}

fn dock_click(x: f32, y: f32) -> Click {
    APP.with(|a| {
        let Ok(slot) = a.try_borrow() else { return Click::None };
        let Some(app) = slot.as_ref() else { return Click::None };
        let Some((id, rect)) = app.tiles.iter().find(|(_, r)| dock::hit_test(x, y, &[*r]).is_some()).copied() else {
            return Click::None;
        };
        let anchor = RectF { x: app.origin.0 + rect.x, y: app.origin.1, w: rect.w, h: DOCK_HEIGHT as f32 };
        match id {
            "apps" => apps::hit_app(x, y, rect, &app.apps).map_or(Click::None, Click::Activate),
            "git" => Click::Open(OpenPopup::Data("git"), anchor),
            "ai" => Click::Open(OpenPopup::Data("ai"), anchor),
            "weather" => Click::Open(OpenPopup::Data("weather"), anchor),
            "media" => Click::Open(OpenPopup::Data("media"), anchor),
            "vol" => Click::Open(OpenPopup::Control, anchor),
            _ => Click::None,
        }
    })
}

fn open_popup(which: OpenPopup, anchor: RectF) {
    if OPEN.with(Cell::get) == Some(which) {
        popup::hide();
        OPEN.with(|o| o.set(None));
        return;
    }
    let height = match which {
        OpenPopup::Data(id) => {
            let kind = match id {
                "git" => PopupKind::Git,
                "ai" => PopupKind::Ai,
                "weather" => PopupKind::Weather,
                _ => PopupKind::Media,
            };
            let data = APP.with(|a| a.borrow().as_ref().map(|app| app.shared.data.lock().unwrap().clone()));
            if let Some(data) = data {
                popup_content::set_content(kind, data);
            }
            CONTROL_VIEW.with(|c| *c.borrow_mut() = None);
            POPUP_H
        }
        OpenPopup::Control => {
            let state = APP.with(|a| a.borrow().as_ref().map(|app| app.shared.control.lock().unwrap().clone()));
            CONTROL_VIEW.with(|c| *c.borrow_mut() = state);
            control_center::HEIGHT
        }
    };
    if popup::show(anchor, height as i32).is_ok() {
        OPEN.with(|o| o.set(Some(which)));
    }
}

fn paint_popup(r: &mut Renderer, rect: RectF) {
    let control = CONTROL_VIEW.with(|c| c.borrow().clone());
    match control {
        Some(state) => {
            let _ = control_center::draw(r, rect, &state);
        }
        None => popup_content::paint_content(r, rect),
    }
}

fn popup_rect() -> RectF {
    let h = if CONTROL_VIEW.with(|c| c.borrow().is_some()) { control_center::HEIGHT } else { POPUP_H };
    RectF { x: 0.5, y: 0.5, w: POPUP_W - 1.0, h: h - 1.0 }
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

fn shutdown() {
    popup::hide();
    OPEN.with(|o| o.set(None));
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().take() {
            unsafe {
                // SAFETY: hwnd is still valid until DestroyWindow in the caller.
                let _ = KillTimer(app.hwnd, TIMER_REFRESH);
                let _ = KillTimer(app.hwnd, TIMER_GUARD);
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

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
                            }
                        }
                    });
                    repaint();
                }
                TIMER_GUARD => appbar::enforce_taskbar_hidden(),
                _ => {}
            }
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
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            match dock_click(x, y) {
                Click::None => {}
                Click::Activate(h) => tasks::activate_window(h),
                Click::Open(which, anchor) => open_popup(which, anchor),
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            // The shell may move the taskbar back; keep it hidden while we own the bottom edge.
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
