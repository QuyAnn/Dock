//! SmartDock entry point: AppBar dock window with Direct2D tiles.
//! Startup: recover taskbar from a crash -> create dock -> register AppBar -> hide taskbar.
//! Every exit path (WM_DESTROY, panic, Ctrl+C) restores the taskbar.
mod data;
mod model;
mod ui;
mod win;

use std::cell::RefCell;

use chrono::Local;
use windows::core::w;
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use ui::dock::{self, RectF, Tile, TileView};
use ui::renderer::Renderer;
use win::appbar::{self, AbEdge};

const DOCK_HEIGHT: i32 = 56;
const TIMER_ID: usize = 1;
const WM_APP_APPBAR: u32 = WM_APP + 0x5d;

struct App {
    hwnd: HWND,
    renderer: Renderer,
    tiles: Vec<Tile>,
    rects: Vec<RectF>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
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
    unsafe {
        // SAFETY: hwnd is our live window.
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rect.left,
            rect.bottom,
            rect.right - rect.left,
            DOCK_HEIGHT,
            SWP_NOACTIVATE,
        );
    }
    appbar::hide_taskbar();

    let tiles = vec![
        Tile { id: "clock", width: 150.0 },
        Tile { id: "cpu", width: 120.0 },
        Tile { id: "ram", width: 150.0 },
        Tile { id: "vol", width: 120.0 },
    ];
    let renderer = Renderer::new(hwnd, rect.right - rect.left, DOCK_HEIGHT);
    let rects = dock::layout(&tiles, false, rect.right - rect.left);
    APP.with(|a| *a.borrow_mut() = Some(App { hwnd, renderer, tiles, rects }));

    unsafe {
        // SAFETY: hwnd is live; timer drives the 1 s refresh.
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_ID, 1000, None);
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

fn tile_views(tiles: &[Tile]) -> Vec<TileView> {
    let now = Local::now();
    let stats = data::stats::read_stats();
    let (vol, muted) = win::system::get_volume();
    tiles
        .iter()
        .map(|t| match t.id {
            "clock" => TileView {
                title: now.format("%a %d/%m").to_string(),
                value: now.format("%H:%M").to_string(),
                progress: None,
                icon: None,
            },
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
            _ => TileView {
                title: "VOL".into(),
                value: if muted { "Muted".into() } else { format!("{:.0}%", vol * 100.0) },
                progress: Some(vol),
                icon: None,
            },
        })
        .collect()
}

fn repaint() {
    APP.with(|a| {
        let mut slot = a.borrow_mut();
        let Some(app) = slot.as_mut() else { return };
        let views = tile_views(&app.tiles);
        let res = app
            .renderer
            .begin()
            .and_then(|_| dock::draw_dock(&mut app.renderer, &views, &app.rects))
            .and_then(|_| app.renderer.end());
        if let Err(e) = res {
            eprintln!("[smartdock] paint failed: {e}");
        }
    });
}

fn shutdown() {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().take() {
            unsafe {
                // SAFETY: hwnd is still valid until DestroyWindow below.
                let _ = KillTimer(app.hwnd, TIMER_ID);
            }
        }
    });
    appbar::ensure_restored_on_exit();
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            repaint();
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
        WM_APP_APPBAR | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            // The shell may move the taskbar back; keep it hidden while we own the bottom edge.
            appbar::enforce_taskbar_hidden();
            LRESULT(0)
        }
        WM_ENDSESSION | WM_CLOSE => {
            shutdown();
            appbar::unregister_appbar(hwnd);
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
