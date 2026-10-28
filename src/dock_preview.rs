//! Developer-only dock renderer (`--render-dock <out.png> <wallpaper> [dark|light]`).
//! Draws the production dock on the edge named by `SMARTDOCK_DOCK_EDGE` (bottom|top|left|right)
//! over a simulated 1440x900 screen, with one popup (`SMARTDOCK_DOCK_POPUP`: calendar, weather,
//! cpu, ram or control; default calendar) opened from its tile and placed by the live
//! `popup_rect_on` rules. The geometry is appended to `<out>.log`.

use std::path::Path;
use std::sync::Arc;

use windows::core::{Error, Result};
use windows::Win32::Foundation::{E_INVALIDARG, RECT};

use crate::model::{Media, Stats, WindowEntry};
use crate::ui::apps::AppsState;
use crate::ui::control_center::{self, ControlState};
use crate::ui::dock::{self, RectF};
use crate::ui::liquid::{self, ScreenGrab};
use crate::ui::popup_content::{self as pc, PopupKind};
use crate::ui::renderer::Renderer;
use crate::ui::tiles::DataState;
use crate::win::{appbar, edge, wallpaper};

const SCREEN_W: u32 = 1440;
const SCREEN_H: u32 = 900;

pub fn render(out: &Path, wallpaper_path: &Path, dark: bool) -> Result<()> {
    let log_path = out.with_extension("log");
    let note = |msg: &str| {
        use std::io::Write;
        eprintln!("render-dock: {msg}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = writeln!(f, "render-dock: {msg}");
        }
    };
    let side = edge::resolve("auto");
    edge::set_current(side);
    let popup = std::env::var("SMARTDOCK_DOCK_POPUP").unwrap_or_else(|_| "calendar".into());
    let (sw, sh) = (SCREEN_W, SCREEN_H);
    let (src_w, src_h, src) = wallpaper::thumbnail(wallpaper_path, 1920)?;
    if src_w == 0 || src_h == 0 {
        return Err(Error::from(E_INVALIDARG));
    }
    let desk = cover_crop(src_w, src_h, &src, sw, sh);

    // The dock strip, as the AppBar would reserve it on a 96-DPI monitor.
    let monitor = RECT { left: 0, top: 0, right: sw as i32, bottom: sh as i32 };
    let thick = dock::DOCK_H.round() as i32;
    let (_, strip) = appbar::edge_rect(monitor, side, thick);
    let (dw, dh) = (strip.right - strip.left, strip.bottom - strip.top);
    let length = if side.vertical() { dh } else { dw } as f32;
    note(&format!("edge={} strip=({},{}..{},{}) length={length}", side.name(), strip.left, strip.top, strip.right, strip.bottom));

    // Liquid backdrop through the same path as the live dock (transposed for left/right).
    let wp = wallpaper::WallpaperPixels {
        source: wallpaper_path.to_path_buf(),
        w: sw,
        h: sh,
        rgba: desk.clone(),
        orig_w: sw,
        orig_h: sh,
        mode: wallpaper::FitMode::Fill,
        background: [0, 0, 0],
    };
    let bar = RectF { x: strip.left as f32, y: strip.top as f32, w: dw as f32, h: dh as f32 };
    let backdrop = if side.vertical() {
        let turned = crate::transposed_wallpaper(&wp);
        crate::untranspose_liquid(liquid::render_liquid(
            &turned,
            RectF { x: bar.y, y: bar.x, w: bar.h, h: bar.w },
            (sh as i32, sw as i32),
        ))
    } else {
        liquid::render_liquid(&wp, bar, (sw as i32, sh as i32))
    };
    note(&format!("backdrop {}x{}", backdrop.w, backdrop.h));
    let backdrop = Arc::new(backdrop);

    // Deterministic fake state: a few running apps (fallback icons) and default widgets.
    let settings = crate::settings::Settings::default();
    let mut apps = AppsState::default();
    apps.windows = [
        ("README.md - Visual Studio Code", "Code.exe"),
        ("SmartDock - Microsoft Edge", "msedge.exe"),
        ("Downloads", "explorer.exe"),
        ("Windows PowerShell", "WindowsTerminal.exe"),
        ("Spotify Premium", "Spotify.exe"),
        ("Slack", "slack.exe"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (title, exe))| WindowEntry {
        hwnd: 0x100 + i as isize,
        title: title.to_string(),
        exe: exe.to_string(),
        aumid: String::new(),
        package_path: String::new(),
        attention: false,
        minimized: false,
    })
    .collect();
    // SMARTDOCK_DOCK_ACTIVE=<index>: that fake app is the foreground app (blue active pill).
    if let Some(i) = std::env::var("SMARTDOCK_DOCK_ACTIVE").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        if let Some(w) = apps.windows.get(i) {
            note(&format!("active app {i}: {}", w.exe));
            crate::ui::apps::set_foreground_settled(w.hwnd, w.exe.clone());
        }
    }
    let mut data = DataState::default();
    data.media = Some(Media { title: "Golden Hour".into(), artist: "SmartDock Demo".into(), playing: true });
    let stats = Stats { cpu_pct: 23.0, ram_used_mb: 9200, ram_total_mb: 16384 };
    let control = ControlState {
        volume: 0.60,
        muted: false,
        brightness: Some(75),
        wifi: Some(true),
        wifi_signal: Some(88),
        ethernet: false,
        ssid: Some("BnK_5G".to_string()),
        bluetooth: Some(true),
        dark,
        mic_muted: Some(false),
    };

    let mut tiles: Vec<(&'static str, RectF)> = Vec::new();
    let dock_tmp = out.with_extension("dock.png");
    Renderer::render_png(&dock_tmp, dw as u32, dh as u32, 96, |r| {
        crate::draw_surface(r, length, None, &mut tiles, &apps, &stats, &data, &control, 0.0, &settings, Some(&backdrop))
    })?;
    let (tw, th, dock_px) = wallpaper::thumbnail(&dock_tmp, dw.max(dh) as u32)?;
    let _ = std::fs::remove_file(&dock_tmp);
    let _ = std::fs::remove_file(dock_tmp.with_extension("log"));
    for (id, r) in &tiles {
        note(&format!("tile {id} ({:.1},{:.1} {:.1}x{:.1})", r.x, r.y, r.w, r.h));
    }

    // The popup, opened from its tile like a click would.
    let (tile_id, kind) = match popup.as_str() {
        "weather" => ("weather", Some(PopupKind::Weather)),
        "cpu" => ("cpu", Some(PopupKind::Cpu)),
        "ram" => ("ram", Some(PopupKind::Ram)),
        "control" => ("cc", None),
        _ => ("clock", Some(PopupKind::Clock)),
    };
    let tile = tiles
        .iter()
        .find(|(id, r)| *id == tile_id && r.w > 0.0 && r.h > 0.0)
        .or_else(|| tiles.iter().find(|(id, r)| *id == "clock" && r.w > 0.0))
        .map(|(_, r)| *r)
        .ok_or_else(|| Error::from(E_INVALIDARG))?;
    let anchor = crate::dock_anchor((strip.left as f32, strip.top as f32), 1.0, tile);
    if kind.is_none() {
        control_center::set_modules(None);
        control_center::set_max_height(f32::MAX);
    }
    let (w, h) = match kind {
        Some(kind) => {
            pc::set_settings(settings.clone());
            if kind == PopupKind::Clock {
                crate::ui::calendar::reset();
            }
            let w = if kind == PopupKind::Clock { crate::ui::calendar::WIDTH } else { 320.0 };
            (w, pc::content_height(kind, &data))
        }
        None => (control_center::WIDTH, control_center::height()),
    };
    let (w, h) = (w.round(), h.round());
    let rect = crate::ui::popup::popup_rect_on(side, anchor, w as i32, h as i32, 96, monitor);
    note(&format!(
        "popup {popup} anchor=({:.0},{:.0} {:.0}x{:.0}) rect=({},{}..{},{})",
        anchor.x, anchor.y, anchor.w, anchor.h, rect.left, rect.top, rect.right, rect.bottom
    ));
    let grab = ScreenGrab { x: 0, y: 0, w: sw as i32, h: sh as i32, rgba: desk.clone() };
    let win = RectF { x: rect.left as f32, y: rect.top as f32, w, h };
    // Same calls as `glass::popup_backdrop_for(.., true)`.
    let mut frost = liquid::render_frost_with(&grab, win, 1.0, &liquid::CONTROL_LENS);
    liquid::add_bright_detail(&mut frost, &grab, win, 1.0);
    let frost = Arc::new(frost);
    let popup_tmp = out.with_extension("popup.png");
    crate::ui::glass::set_control_dark(dark);
    crate::ui::glass::set_panel_light(kind == Some(PopupKind::Clock));
    let radius = if crate::ui::glass::panel_light() { 24.0 } else { 18.0 };
    Renderer::render_png(&popup_tmp, w as u32, h as u32, 96, |r| {
        let panel = RectF { x: 0.5, y: 0.5, w: w - 1.0, h: h - 1.0 };
        let full = RectF { x: 0.0, y: 0.0, w, h };
        crate::ui::glass::set_backdrop(Some(frost.clone()), w, h);
        let painted = match kind {
            Some(kind) => {
                // The live popup path: Control Center clear glass.
                let mut res = Ok(());
                let surface = crate::ui::popup::paint_clear_glass(r, panel, radius, |r, _| res = pc::draw_popup(r, kind, full, &data));
                surface.and(res)
            }
            None => control_center::draw(r, panel, &control, data.media.as_ref()),
        };
        crate::ui::glass::set_backdrop(None, w, h);
        painted
    })?;
    let (pw, ph, popup_px) = wallpaper::thumbnail(&popup_tmp, (w.max(h)) as u32)?;
    let _ = std::fs::remove_file(&popup_tmp);
    let _ = std::fs::remove_file(popup_tmp.with_extension("log"));

    // Desktop, dock strip and popup window composited where they sit on screen.
    Renderer::render_png(out, sw, sh, 96, |r| {
        r.draw_bitmap(sw, sh, &desk, RectF { x: 0.0, y: 0.0, w: sw as f32, h: sh as f32 })?;
        r.draw_bitmap(tw, th, &dock_px, bar)?;
        r.draw_bitmap(pw, ph, &popup_px, win)
    })
}

/// Nearest-neighbour cover crop of the wallpaper to the simulated screen.
fn cover_crop(sw: u32, sh: u32, src: &[u8], dw: u32, dh: u32) -> Vec<u8> {
    let scale = (dw as f32 / sw as f32).max(dh as f32 / sh as f32);
    let left = (sw as f32 - dw as f32 / scale) * 0.5;
    let top = (sh as f32 - dh as f32 / scale) * 0.5;
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for y in 0..dh {
        for x in 0..dw {
            let sx = (left + x as f32 / scale).floor().clamp(0.0, (sw - 1) as f32) as u32;
            let sy = (top + y as f32 / scale).floor().clamp(0.0, (sh - 1) as f32) as u32;
            let from = ((sy * sw + sx) * 4) as usize;
            let to = ((y * dw + x) * 4) as usize;
            out[to..to + 4].copy_from_slice(&src[from..from + 4]);
            out[to + 3] = 255;
        }
    }
    out
}
