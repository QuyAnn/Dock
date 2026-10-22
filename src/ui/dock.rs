//! Dock geometry, hit testing and drawing. Tile content is passed in as plain values.
//! Coordinates are dock-local DIPs (the dock window's client area).
use windows::core::Result;

use super::renderer::{Align, Renderer};

pub const DOCK_H: f32 = 56.0;
const BAR_RADIUS: f32 = 14.0;
const TILE_H: f32 = 44.0;
const TILE_RADIUS: f32 = 10.0;
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;
const ICON: f32 = 32.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct Tile {
    pub id: &'static str,
    pub width: f32,
}

/// Straight (non-premultiplied) RGBA icon pixels.
pub struct Icon {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Plain values for one tile. Caller fills these from model data.
pub struct TileView {
    pub title: String,
    pub value: String,
    /// 0..1, drawn as a thin bar along the bottom edge.
    pub progress: Option<f32>,
    pub icon: Option<Icon>,
}

/// Places tiles in one row, vertically centered in `DOCK_H`.
/// `full_width`: tiles spread edge to edge across `screen_w` (space-between).
/// Otherwise: a centered group with fixed gaps (pill). The window is expected to span `screen_w`.
pub fn layout(tiles: &[Tile], full_width: bool, screen_w: i32) -> Vec<RectF> {
    let n = tiles.len();
    let sum: f32 = tiles.iter().map(|t| t.width).sum();
    let sw = screen_w as f32;
    let (mut x, gap) = if full_width {
        let gap = if n > 1 { ((sw - 2.0 * PAD - sum) / (n - 1) as f32).max(GAP) } else { 0.0 };
        (PAD, gap)
    } else {
        let group = sum + GAP * n.saturating_sub(1) as f32;
        ((sw - group).max(0.0) / 2.0, GAP)
    };
    let y = (DOCK_H - TILE_H) / 2.0;
    let mut out = Vec::with_capacity(n);
    for t in tiles {
        out.push(RectF { x, y, w: t.width, h: TILE_H });
        x += t.width + gap;
    }
    out
}

/// Index of the rect containing (x, y), if any.
pub fn hit_test(x: f32, y: f32, rects: &[RectF]) -> Option<usize> {
    rects
        .iter()
        .position(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
}

/// Draws the dock bar (full renderer size), then each tile at `rects[i]`.
/// Call between `Renderer::begin` and `Renderer::end`.
pub fn draw_dock(r: &mut Renderer, tiles: &[TileView], rects: &[RectF]) -> Result<()> {
    let (w, h) = r.size();
    let bar = RectF { x: 0.0, y: 0.0, w, h };
    r.fill_rounded_rect(bar, BAR_RADIUS, 0x2C2E_3838)?;
    r.stroke_rounded_rect(bar, BAR_RADIUS, 0xFFFF_FF24, 1.0)?;

    for (t, rc) in tiles.iter().zip(rects) {
        r.fill_rounded_rect(*rc, TILE_RADIUS, 0xFFFF_FF1F)?;
        r.stroke_rounded_rect(*rc, TILE_RADIUS, 0xFFFF_FF2E, 1.0)?;

        let mut text_x = rc.x + PAD;
        if let Some(icon) = &t.icon {
            let icon_rect = RectF { x: rc.x + PAD, y: rc.y + (rc.h - ICON) / 2.0, w: ICON, h: ICON };
            r.draw_bitmap(icon.w, icon.h, &icon.rgba, icon_rect)?;
            text_x += ICON + 6.0;
        }
        let text_w = (rc.x + rc.w - PAD - text_x).max(0.0);
        let title = RectF { x: text_x, y: rc.y + 5.0, w: text_w, h: 17.0 };
        let value = RectF { x: text_x, y: rc.y + 21.0, w: text_w, h: 18.0 };
        r.draw_text(&t.title, title, 12.0, 0xFFFF_FF99, Align::Left)?;
        r.draw_text(&t.value, value, 13.0, 0xFFFF_FFFF, Align::Left)?;

        if let Some(p) = t.progress {
            let track = RectF { x: text_x, y: rc.y + rc.h - 5.0, w: text_w, h: 2.0 };
            r.fill_rect(track, 0xFFFF_FF33)?;
            let fill = RectF { w: text_w * p.clamp(0.0, 1.0), ..track };
            r.fill_rect(fill, 0xFFFF_FFE6)?;
        }
    }
    Ok(())
}
