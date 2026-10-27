//! Developer-only Control Center PNG renderer (`--render-cc`).
//! Uses the production Direct2D drawing path on an offscreen WIC bitmap.

use std::path::Path;
use std::sync::Arc;
use std::io::Write;

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_INVALIDARG;

use super::control_center::{self, ControlState};
use super::dock::RectF;
use super::liquid::{self, ScreenGrab};
use super::renderer::Renderer;
use crate::model::Media;
use crate::win::wallpaper;

const CANVAS_W: u32 = 720;
const CANVAS_H: u32 = 480;
const PANEL_RIGHT: f32 = 30.0;
const PANEL_TOP: f32 = 39.5;

/// Renders the production Control Center over `wallpaper_path` with deterministic fake state.
pub fn render(out_path: &Path, wallpaper_path: &Path) -> Result<()> {
    let mut trace = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_path.with_extension("log"))
        .ok();
    macro_rules! trace {
        ($message:expr) => {
            if let Some(file) = trace.as_mut() {
                let _ = writeln!(file, "render-cc: {}", $message);
                let _ = file.flush();
            }
        };
    }
    trace!("decoding wallpaper");
    let (source_w, source_h, source) = wallpaper::thumbnail(wallpaper_path, 1920)?;
    trace!("wallpaper decoded");
    if source_w == 0 || source_h == 0 {
        return Err(Error::from(E_INVALIDARG));
    }
    let background = cover_crop(
        source_w,
        source_h,
        &source,
        CANVAS_W,
        CANVAS_H,
    );
    trace!("wallpaper cover crop complete");
    let grab = ScreenGrab {
        x: 0,
        y: 0,
        w: CANVAS_W as i32,
        h: CANVAS_H as i32,
        rgba: background.clone(),
    };
    let frost = Arc::new(liquid::render_frost(
        &grab,
        RectF { x: 0.0, y: 0.0, w: CANVAS_W as f32, h: CANVAS_H as f32 },
        1.0,
    ));
    trace!("frost backdrop complete");
    let state = ControlState {
        volume: 0.60,
        muted: false,
        brightness: Some(75),
        wifi: Some(true),
        wifi_signal: Some(88),
        ethernet: false,
        bluetooth: Some(true),
        dark: true,
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
        h: control_center::HEIGHT - 1.0,
    };

    trace!("starting WIC render");
    Renderer::render_png(out_path, CANVAS_W, CANVAS_H, 96, |renderer| {
        renderer.draw_bitmap(
            CANVAS_W,
            CANVAS_H,
            &background,
            RectF { x: 0.0, y: 0.0, w: CANVAS_W as f32, h: CANVAS_H as f32 },
        )?;
        super::glass::set_backdrop(Some(frost.clone()), CANVAS_W as f32, CANVAS_H as f32);
        let result = control_center::draw(renderer, panel, &state, Some(&media));
        super::glass::set_backdrop(None, CANVAS_W as f32, CANVAS_H as f32);
        result
    })
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
