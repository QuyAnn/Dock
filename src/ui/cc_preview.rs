//! Developer-only Control Center PNG renderer (`--render-cc`).
//! Uses the production Direct2D drawing path on an offscreen WIC bitmap.

use std::path::Path;
use std::sync::Arc;

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_INVALIDARG;

use super::control_center::{self, ControlState};
use super::dock::RectF;
use super::liquid::{self, ScreenGrab};
use super::renderer::Renderer;
use crate::model::Media;
use crate::win::wallpaper;

/// Canvas in DIPs; rendered at 2x (192 DPI) so the glass rims compare fairly with Retina captures.
const CANVAS_W: u32 = 810;
const MIN_CANVAS_H: u32 = 540;
const SCALE: u32 = 2;
const PANEL_RIGHT: f32 = 30.0;
const PANEL_TOP: f32 = 39.5;

/// Renders the production Control Center over `wallpaper_path` with deterministic fake state.
pub fn render(out_path: &Path, wallpaper_path: &Path, dark: bool) -> Result<()> {
    let log_path = out_path.with_extension("log");
    let trace = |msg: &str| {
        use std::io::Write;
        eprintln!("render-cc: {msg}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = writeln!(f, "render-cc: {msg}");
            let _ = f.flush();
        }
    };
    // SMARTDOCK_CC_MODULES=wifi,mic,... renders a customized layout; SMARTDOCK_CC_EDIT=1 renders
    // edit mode; SMARTDOCK_CC_DRAG=from,to also shows `from` mid-drag over `to`.
    let modules: Option<Vec<String>> = std::env::var("SMARTDOCK_CC_MODULES")
        .ok()
        .map(|v| v.split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect());
    control_center::set_modules(modules.as_deref());
    control_center::set_max_height(f32::MAX);
    if std::env::var_os("SMARTDOCK_CC_EDIT").is_some() {
        control_center::begin_edit();
    }
    let panel_h = control_center::height();
    let canvas_h = MIN_CANVAS_H.max((panel_h + 2.0 * PANEL_TOP).ceil() as u32);
        trace(&format!("decoding wallpaper {}", wallpaper_path.display()));
    let (source_w, source_h, source) = wallpaper::thumbnail(wallpaper_path, 1920)?;
    if source_w == 0 || source_h == 0 {
        return Err(Error::from(E_INVALIDARG));
    }
    trace(&format!("wallpaper decoded {source_w}x{source_h} ({} bytes)", source.len()));
    let (px_w, px_h) = (CANVAS_W * SCALE, canvas_h * SCALE);
    let background = cover_crop(source_w, source_h, &source, px_w, px_h);
    trace("cover crop done; rendering frost");
    let grab = ScreenGrab {
        x: 0,
        y: 0,
        w: px_w as i32,
        h: px_h as i32,
        rgba: background.clone(),
    };
    // Same material as the live Control Center popup (`popup_backdrop_for(.., true)`).
    let full = RectF { x: 0.0, y: 0.0, w: px_w as f32, h: px_h as f32 };
    let mut frost = liquid::render_frost_with(&grab, full, SCALE as f32, &liquid::CONTROL_LENS);
    // Live popups measure only the area behind the panel; do the same here.
    let behind = RectF { x: (CANVAS_W as f32 - PANEL_RIGHT - control_center::WIDTH) * SCALE as f32, y: PANEL_TOP * SCALE as f32, w: control_center::WIDTH * SCALE as f32, h: panel_h * SCALE as f32 };
    // Detail frost for every backdrop, like `liquid::add_bright_detail` on the live popup.
    let _ = bright_behind(&frost, behind);
    {
        let detail = liquid::render_frost_with(&grab, full, SCALE as f32, &liquid::BRIGHT_DETAIL_LENS);
        if detail.w == frost.w && detail.h == frost.h {
            frost.plain = detail.rgba;
        }
    }
    let frost = Arc::new(frost);
    trace(&format!("frost rendered {}x{}", frost.w, frost.h));
    let state = ControlState {
        volume: 0.60,
        muted: false,
        brightness: Some(75),
        wifi: Some(true),
        wifi_signal: Some(88),
        ethernet: true,
        ssid: Some("BnK_5G".to_string()),
        bluetooth: Some(true),
        dark,
        mic_muted: Some(true),
    };
    let media = Media {
        title: "Golden Hour".to_string(),
        artist: "SmartDock Demo".to_string(),
        playing: true,
    };
    let panel = RectF {
        x: CANVAS_W as f32 - PANEL_RIGHT - control_center::WIDTH + 0.5,
        y: PANEL_TOP,
        w: control_center::WIDTH - 1.0,
        h: panel_h - 1.0,
    };

    // SMARTDOCK_CC_OUTPUTS=1 renders the sound-output sub-page instead of the main modules.
    if std::env::var_os("SMARTDOCK_CC_OUTPUTS").is_some() {
        control_center::show_outputs(vec![
            crate::win::system::AudioOutput { id: "a".into(), name: "Speakers (Realtek(R) Audio)".into(), is_default: true },
            crate::win::system::AudioOutput { id: "b".into(), name: "Headphones (AirPods Pro)".into(), is_default: false },
            crate::win::system::AudioOutput { id: "c".into(), name: "LG HDR 4K (NVIDIA High Definition Audio)".into(), is_default: false },
        ]);
    }
    if let Ok(spec) = std::env::var("SMARTDOCK_CC_DRAG") {
        if let Some((from, to)) = spec.split_once(',') {
            control_center::preview_drag(panel, from.trim(), to.trim());
        }
    }
    if let Some(scale) = live_scale() {
        return render_live(out_path, &background, px_w, px_h, scale, panel_h, &state, &media);
    }
    trace("starting Direct2D/WIC render");
    let result = Renderer::render_png(out_path, px_w, px_h, 96 * SCALE, |renderer| {
        renderer.draw_bitmap(
            px_w,
            px_h,
            &background,
            RectF { x: 0.0, y: 0.0, w: CANVAS_W as f32, h: canvas_h as f32 },
        )?;
        super::glass::set_backdrop(Some(frost.clone()), CANVAS_W as f32, canvas_h as f32);
        let result = control_center::draw(renderer, panel, &state, Some(&media));
        super::glass::set_backdrop(None, CANVAS_W as f32, canvas_h as f32);
        result
    });
    trace(&format!("render_png returned {result:?}"));
    result
}

/// `SMARTDOCK_CC_LIVE=<scale>` (e.g. 1, 1.25, 1.5): render exactly like the live popup on a display
/// at that scale instead of the 2x full-canvas preview.
fn live_scale() -> Option<f32> {
    let v = std::env::var("SMARTDOCK_CC_LIVE").ok()?;
    let s: f32 = v.trim().parse().unwrap_or(1.0);
    Some(s.clamp(1.0, 4.0))
}

/// Live-path replica: the desktop at `scale` (physical px = DIP * scale), a popup window of the
/// Control Center's size whose backdrop is built by the same calls as `glass::popup_backdrop_for`
/// (`render_frost_with(CONTROL_LENS)` over the window rect + `add_bright_detail`), the panel painted
/// into that window at (0.5, 0.5) like `popup::paint`, then composited where the window sits.
#[allow(clippy::too_many_arguments)]
fn render_live(
    out_path: &Path,
    background_2x: &[u8],
    w2: u32,
    h2: u32,
    scale: f32,
    panel_h: f32,
    state: &ControlState,
    media: &Media,
) -> Result<()> {
    let canvas_h = h2 as f32 / SCALE as f32;
    let (px_w, px_h) = ((CANVAS_W as f32 * scale).round() as u32, (canvas_h * scale).round() as u32);
    // The desktop as a screen at this scale would show it (box-downsampled from the 2x crop).
    let mut desk = vec![0u8; (px_w * px_h * 4) as usize];
    let k = SCALE as f32 / scale;
    for y in 0..px_h {
        for x in 0..px_w {
            let (sx0, sy0) = (x as f32 * k, y as f32 * k);
            let mut acc = [0f32; 4];
            let mut n = 0f32;
            let mut yy = sy0;
            while yy < sy0 + k - 1e-3 {
                let mut xx = sx0;
                while xx < sx0 + k - 1e-3 {
                    let (ix, iy) = ((xx as u32).min(w2 - 1), (yy as u32).min(h2 - 1));
                    let o = ((iy * w2 + ix) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += background_2x[o + c] as f32;
                    }
                    n += 1.0;
                    xx += 1.0;
                }
                yy += 1.0;
            }
            let o = ((y * px_w + x) * 4) as usize;
            for c in 0..4 {
                desk[o + c] = (acc[c] / n.max(1.0)).round() as u8;
            }
        }
    }
    let to_px = |dip: f32| (dip * scale).round() as i32;
    let (wx, wy) = (to_px(CANVAS_W as f32 - PANEL_RIGHT - control_center::WIDTH), to_px(PANEL_TOP - 0.5));
    let (ww, wh) = (to_px(control_center::WIDTH), to_px(panel_h));
    let grab = ScreenGrab { x: 0, y: 0, w: px_w as i32, h: px_h as i32, rgba: desk.clone() };
    let win = RectF { x: wx as f32, y: wy as f32, w: ww as f32, h: wh as f32 };
    let mut frost = liquid::render_frost_with(&grab, win, scale, &liquid::CONTROL_LENS);
    liquid::add_bright_detail(&mut frost, &grab, win, scale);
    let frost = Arc::new(frost);
    let dpi = (96.0 * scale).round() as u32;
    let (w, h) = (ww as f32 / scale, wh as f32 / scale);
    let tmp = out_path.with_extension("window.png");
    Renderer::render_png(&tmp, ww as u32, wh as u32, dpi, |r| {
        super::glass::set_backdrop(Some(frost.clone()), w, h);
        let panel = RectF { x: 0.5, y: 0.5, w: w - 1.0, h: h - 1.0 };
        let res = control_center::draw(r, panel, state, Some(media));
        super::glass::set_backdrop(None, w, h);
        res
    })?;
    let (tw, th, win_px) = wallpaper::thumbnail(&tmp, (ww.max(wh)) as u32)?;
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("log"));
    // Composite at 1:1 device pixels (96 DPI target in px units).
    let canvas = (px_w as f32 / scale, px_h as f32 / scale);
    Renderer::render_png(out_path, px_w, px_h, dpi, |r| {
        r.draw_bitmap(px_w, px_h, &desk, RectF { x: 0.0, y: 0.0, w: canvas.0, h: canvas.1 })?;
        r.draw_bitmap(tw, th, &win_px, RectF { x: wx as f32 / scale, y: wy as f32 / scale, w, h })
    })
}

/// Same test as `liquid::add_bright_detail`, over the panel area of the full-canvas preview frost.
fn bright_behind(frost: &liquid::LiquidBitmap, r: RectF) -> bool {
    let (mut sum, mut n) = (0f32, 0f32);
    let (x0, y0) = (r.x.max(0.0) as u32, r.y.max(0.0) as u32);
    let (x1, y1) = (((r.x + r.w) as u32).min(frost.w), ((r.y + r.h) as u32).min(frost.h));
    for y in (y0..y1).step_by(8) {
        for x in (x0..x1).step_by(8) {
            let o = ((y * frost.w + x) * 4) as usize;
            let c = &frost.rgba[o..o + 3];
            sum += 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32;
            n += 1.0;
        }
    }
    n > 0.0 && sum / n >= 175.0
}

/// Nearest-neighbour cover crop is intentional: Direct2D only sees this as the desktop behind the
/// panel, while the glass path performs its own blur and interpolation.
fn cover_crop(sw: u32, sh: u32, src: &[u8], dw: u32, dh: u32) -> Vec<u8> {
    let scale = (dw as f32 / sw as f32).max(dh as f32 / sh as f32);
    let visible_w = dw as f32 / scale;
    let visible_h = dh as f32 / scale;
    let left = (sw as f32 - visible_w) * 0.5;
    let top = (sh as f32 - visible_h) * 0.5;
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

/// Popups `--render-popup` knows, in workflow order.
pub const POPUPS: &[&str] = &[
    "calendar", "weather", "cpu", "ram", "media", "settings", "todo", "ai", "git", "currency", "notes",
    "stopwatch", "worldclock", "screentime", "forecast", "appmenu", "dockmenu", "gallery",
];

fn sample_weather() -> crate::data::weather::Weather {
    use crate::data::weather::{Current, Day, Hour, Weather};
    let now = chrono::Utc::now();
    let today = (now + chrono::Duration::hours(7)).date_naive();
    let codes = [1u32, 2, 61, 3, 80, 0, 2];
    let daily = (0..7)
        .map(|i| Day {
            date: (today + chrono::Duration::days(i)).format("%Y-%m-%d").to_string(),
            max: 33.0 - (i % 3) as f32,
            min: 25.0 + (i % 2) as f32,
            code: codes[i as usize],
        })
        .collect();
    let start = now.timestamp() - now.timestamp() % 3600;
    let hourly = (0..24)
        .map(|i| Hour { at: start + i * 3600, temp: 27.0 + ((i as f32) * 0.5).sin() * 4.0, code: codes[(i % 7) as usize] })
        .collect();
    Weather {
        location: "Hồ Chí Minh".to_string(),
        current: Current { temp: 31.0, feels: 35.0, code: 2, humidity: 70.0, wind: 11.0 },
        daily,
        hourly,
        sunrise: Some(start - 6 * 3600),
        sunset: Some(start + 6 * 3600),
        utc_offset_seconds: 7 * 3600,
        fetched_at: now,
    }
}

/// Renders one production popup (shared popup glass + its painter) over `wallpaper_path`.
/// `gallery` builds the widget gallery entries (the tile views live in the binary root).
pub fn render_popup(
    name: &str,
    out_path: &Path,
    wallpaper_path: &Path,
    dark: bool,
    gallery: &dyn Fn(&crate::ui::tiles::DataState) -> Vec<super::gallery::Entry>,
) -> Result<()> {
    use super::popup_content::{self as pc, PopupKind};
    let settings = crate::settings::Settings::default();
    let mut data = crate::ui::tiles::DataState { weather: Some(sample_weather()), ..Default::default() };
    data.media = Some(Media { title: "Golden Hour".into(), artist: "SmartDock Demo".into(), playing: true });
    let kind = match name {
        "calendar" => Some(PopupKind::Clock),
        "weather" => Some(PopupKind::Weather),
        "cpu" => Some(PopupKind::Cpu),
        "ram" => Some(PopupKind::Ram),
        "media" => Some(PopupKind::Media),
        "settings" => Some(PopupKind::Settings),
        "todo" => Some(PopupKind::Todo),
        "ai" => Some(PopupKind::Ai),
        "git" => Some(PopupKind::Git),
        _ => None,
    };
    let wave: Vec<f32> = (0..60).map(|i| 0.35 + 0.25 * ((i as f32) * 0.31).sin() + 0.1 * ((i as f32) * 1.7).cos()).collect();
    pc::set_history(PopupKind::Cpu, wave.clone());
    pc::set_history(PopupKind::Ram, wave.iter().map(|v| 0.55 + v * 0.2).collect());
    pc::set_settings(settings.clone());
    let (w, h): (f32, f32) = if let Some(kind) = kind {
        let w = match kind {
            PopupKind::Settings => 460.0,
            PopupKind::Clock => super::calendar::WIDTH,
            _ => 320.0,
        };
        if kind == PopupKind::Clock {
            super::calendar::reset();
        }
        (w, pc::content_height(kind, &data))
    } else {
        match name {
            "appmenu" => {
                let wins = [
                    ("SmartDock – README.md", false),
                    ("control_center.rs — Dock", false),
                    ("Liquid glass notes", true),
                ]
                .map(|(t, m)| crate::model::WindowEntry {
                    hwnd: 0,
                    title: t.to_string(),
                    exe: "Code.exe".into(),
                    aumid: String::new(),
                    package_path: String::new(),
                    attention: false,
                    minimized: m,
                });
                let menu = super::appmenu::from_group("Visual Studio Code", "Code.exe", &wins, None, &settings);
                let h = super::appmenu::content_height(&menu);
                super::appmenu::set_menu(menu);
                (260.0, h)
            }
            "dockmenu" => (super::appmenu::DOCK_MENU_W, super::appmenu::open_dock_menu(Some("weather"), &settings)),
            "gallery" => {
                super::gallery::reset();
                (super::gallery::W, super::gallery::H)
            }
            id if super::more_widgets::has_popup(id) => {
                super::more_widgets::prepare(id, &settings);
                (320.0, super::more_widgets::popup_height(id))
            }
            _ => return Err(Error::from(E_INVALIDARG)),
        }
    };
    let entries = if name == "gallery" { gallery(&data) } else { Vec::new() };
    let (w, h) = (w.round(), h.round());

    let (source_w, source_h, source) = wallpaper::thumbnail(wallpaper_path, 1920)?;
    if source_w == 0 || source_h == 0 {
        return Err(Error::from(E_INVALIDARG));
    }
    let canvas_w = (CANVAS_W as f32).max(w + 2.0 * PANEL_RIGHT);
    let canvas_h = (MIN_CANVAS_H as f32).max(h + 2.0 * PANEL_TOP);
    let (px_w, px_h) = ((canvas_w * SCALE as f32) as u32, (canvas_h * SCALE as f32) as u32);
    let background = cover_crop(source_w, source_h, &source, px_w, px_h);
    let grab = ScreenGrab { x: 0, y: 0, w: px_w as i32, h: px_h as i32, rgba: background.clone() };
    let (x0, y0) = if name == "gallery" {
        ((canvas_w - w) / 2.0, (canvas_h - h) / 2.0)
    } else {
        (canvas_w - PANEL_RIGHT - w, PANEL_TOP.floor())
    };
    let s = SCALE as f32;
    // Popup-local backdrop, exactly like `popup_backdrop_for(.., true)` on the live desktop.
    let frost = Arc::new(liquid::render_frost_with(
        &grab,
        RectF { x: x0 * s, y: y0 * s, w: w * s, h: h * s },
        s,
        &liquid::CONTROL_LENS,
    ));

    // Pass 1: the popup window alone (transparent outside its rounded panel).
    let tmp = out_path.with_extension("popup.png");
    super::glass::set_control_dark(dark);
    super::glass::set_panel_light(matches!(name, "calendar" | "gallery"));
    let radius = if super::glass::panel_light() { 24.0 } else { 18.0 };
    let pw = (w * s) as u32;
    let ph = (h * s) as u32;
    Renderer::render_png(&tmp, pw, ph, 96 * SCALE, |r| {
        let panel = RectF { x: 0.5, y: 0.5, w: w - 1.0, h: h - 1.0 };
        let rect = RectF { x: 0.0, y: 0.0, w, h };
        super::glass::set_backdrop(Some(frost.clone()), w, h);
        let surface = super::glass::draw_popup_surface(r, panel, radius);
        super::renderer::set_light_ink(!dark);
        super::renderer::set_popup_text(true);
        let painted = surface.and_then(|_| match kind {
            Some(kind) => pc::draw_popup(r, kind, rect, &data),
            None => match name {
                "appmenu" => super::appmenu::paint(r, rect),
                "dockmenu" => super::appmenu::paint_dock_menu(r, rect),
                "gallery" => super::gallery::draw(r, rect, &entries),
                id => super::more_widgets::paint_popup(r, id, rect, &data, &settings),
            },
        });
        super::renderer::set_popup_text(false);
        super::renderer::set_light_ink(false);
        super::glass::set_backdrop(None, w, h);
        painted
    })?;
    let (tw, th, popup_px) = wallpaper::thumbnail(&tmp, pw.max(ph))?;
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("log"));

    // Pass 2: desktop + popup window composited where the live popup would sit.
    Renderer::render_png(out_path, px_w, px_h, 96 * SCALE, |r| {
        r.draw_bitmap(px_w, px_h, &background, RectF { x: 0.0, y: 0.0, w: canvas_w, h: canvas_h })?;
        r.draw_bitmap(tw, th, &popup_px, RectF { x: x0, y: y0, w, h })
    })
}

/// Icon contact sheet: every Control Center icon at 1x (24 DIP) and 2x (48 DIP), white on dark and
/// dark on light. Rendered at 96 DPI so each row shows true device pixels.
pub fn render_icons(out_path: &Path) -> Result<()> {
    use super::ccicons::Icon;
    let n = Icon::ALL.len() as f32;
    let cell = 64.0;
    let w = 24.0 + n * cell;
    let rows: [(f32, u32, u32); 4] = [(24.0, 0x1C1C_1EFF, 0xFFFF_FFFF), (48.0, 0x1C1C_1EFF, 0xFFFF_FFFF), (24.0, 0xF2F2_F7FF, 0x1D1D_1FFF), (48.0, 0xF2F2_F7FF, 0x1D1D_1FFF)];
    let row_h = 80.0;
    let h = rows.len() as f32 * row_h;
    Renderer::render_png(out_path, w as u32, h as u32, 96, |r| {
        for (i, (size, bg, fg)) in rows.iter().enumerate() {
            let y = i as f32 * row_h;
            r.fill_rounded_rect(RectF { x: 0.0, y, w, h: row_h }, 0.0, *bg)?;
            for (j, icon) in Icon::ALL.iter().enumerate() {
                let cx = 12.0 + cell * (j as f32 + 0.5);
                // Faint optical grid square behind each icon.
                let g = RectF { x: cx - size / 2.0, y: y + row_h / 2.0 - size / 2.0, w: *size, h: *size };
                r.fill_rounded_rect(g, 2.0, (*fg & 0xFFFF_FF00) | 0x10)?;
                super::ccicons::draw(r, *icon, cx, y + row_h / 2.0, *size, *fg)?;
            }
        }
        Ok(())
    })
}
