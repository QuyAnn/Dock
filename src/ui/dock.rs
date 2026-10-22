//! Dock geometry, hit testing and drawing, matching the old Electron dock (styles.css / Dock.tsx).
//! Three zones: left (git, AI, media), center (running apps), right (Control Center, settings,
//! CPU, RAM, clock). Coordinates are dock-local DIPs (the dock window's client area).
use windows::core::Result;

use super::renderer::{Align, Renderer};

pub const DOCK_H: f32 = 56.0;
const BAR_RADIUS: f32 = 8.0;
const TILE_H: f32 = 34.0;
const TILE_RADIUS: f32 = 7.0;
const ROW_PAD: f32 = 5.0; // .dock-row padding
const ZONE_GAP: f32 = 5.0; // .zone gap
const ROW_GAP: f32 = 6.0; // .dock-row gap between zones
const PAD: f32 = 8.0; // .tile horizontal padding
const CLOCK_TIME_H: f32 = 15.0;

// Colors from styles.css, 0xRRGGBBAA.
const BAR_FILL: u32 = 0x2C2E_3838; // linear-gradient top, rgba(44,46,56,.22)
const BAR_BORDER: u32 = 0xFFFF_FF24; // rgba(255,255,255,.14)
const TILE_FILL: u32 = 0x1C1C_20BD; // rgba(28,28,32,.74)
const TILE_BORDER: u32 = 0xFFFF_FF13; // rgba(255,255,255,.075)
const TEXT: u32 = 0xF1F1_F4FF;
const MUTED: u32 = 0xFFFF_FF8C; // rgba(255,255,255,.55)
const RING_TRACK: u32 = 0xFFFF_FF33;
const CLOCK_RED: u32 = 0xF044_1EFF; // .clock-time gradient, flat approximation

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
    /// 0 means the default tile height (34 px). The apps zone uses 44.
    pub height: f32,
}

/// Straight (non-premultiplied) RGBA icon pixels.
pub struct Icon {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// How a tile is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    /// Rounded card: title (muted, small) above value.
    Card,
    /// 34 px card with a circular gauge and the value centered (CPU, RAM).
    Ring,
    /// Red time block above a date/weather row.
    Clock,
    /// No background (the apps zone, the settings gear).
    Bare,
}

/// Plain values for one tile. Caller fills these from model data.
pub struct TileView {
    pub style: Style,
    pub title: String,
    pub value: String,
    /// Second line for `Style::Clock` (weather), empty otherwise.
    pub sub: String,
    pub icon: Option<Icon>,
}

/// Places tiles in three zones. Center group is centered on the bar; left and right hug the edges.
/// Returns one rect per tile, in the order: left, center, right.
pub fn layout(left: &[Tile], center: &[Tile], right: &[Tile], screen_w: f32) -> Vec<RectF> {
    let mut out = Vec::with_capacity(left.len() + center.len() + right.len());
    let mut place = |tiles: &[Tile], x: f32, out: &mut Vec<RectF>| -> f32 {
        let mut x = x;
        for t in tiles {
            let h = if t.height > 0.0 { t.height } else { TILE_H };
            out.push(RectF { x, y: (DOCK_H - h) / 2.0, w: t.width, h });
            x += t.width + ZONE_GAP;
        }
        x
    };
    let zone_w = |tiles: &[Tile]| -> f32 {
        tiles.iter().map(|t| t.width).sum::<f32>() + ZONE_GAP * tiles.len().saturating_sub(1) as f32
    };

    place(left, ROW_PAD, &mut out);

    let center_w = zone_w(center);
    let center_x = (screen_w - center_w) / 2.0;
    place(center, center_x, &mut out);

    let right_w = zone_w(right);
    let right_x = (screen_w - ROW_PAD - right_w).max(center_x + center_w + ROW_GAP);
    place(right, right_x, &mut out);
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
    r.fill_rounded_rect(bar, BAR_RADIUS, BAR_FILL)?;
    r.stroke_rounded_rect(bar, BAR_RADIUS, BAR_BORDER, 1.0)?;

    for (t, rc) in tiles.iter().zip(rects) {
        match t.style {
            Style::Bare => draw_bare(r, t, *rc)?,
            Style::Card => draw_card(r, t, *rc)?,
            Style::Ring => draw_ring(r, t, *rc)?,
            Style::Clock => draw_clock(r, t, *rc)?,
        }
    }
    Ok(())
}

fn draw_bare(r: &mut Renderer, t: &TileView, rc: RectF) -> Result<()> {
    if let Some(icon) = &t.icon {
        r.draw_bitmap(icon.w, icon.h, &icon.rgba, rc)?;
    }
    if !t.value.is_empty() {
        r.draw_text(&t.value, rc, 14.0, 0xFFFF_FF73, Align::Center)?;
    }
    Ok(())
}

fn draw_card(r: &mut Renderer, t: &TileView, rc: RectF) -> Result<()> {
    r.fill_rounded_rect(rc, TILE_RADIUS, TILE_FILL)?;
    r.stroke_rounded_rect(rc, TILE_RADIUS, TILE_BORDER, 1.0)?;

    let mut text_x = rc.x + PAD;
    if let Some(icon) = &t.icon {
        let icon_rect = RectF { x: rc.x + PAD, y: rc.y + (rc.h - 16.0) / 2.0, w: 16.0, h: 16.0 };
        r.draw_bitmap(icon.w, icon.h, &icon.rgba, icon_rect)?;
        text_x += 22.0;
    }
    let text_w = (rc.x + rc.w - PAD - text_x).max(0.0);
    if t.value.is_empty() {
        let line = RectF { x: text_x, y: rc.y, w: text_w, h: rc.h };
        r.draw_text(&t.title, line, 9.5, TEXT, Align::Left)?;
    } else {
        let title = RectF { x: text_x, y: rc.y + 4.0, w: text_w, h: 13.0 };
        let value = RectF { x: text_x, y: rc.y + 16.0, w: text_w, h: 16.0 };
        r.draw_text(&t.title, title, 8.5, MUTED, Align::Left)?;
        r.draw_text(&t.value, value, 11.5, TEXT, Align::Left)?;
    }
    Ok(())
}

fn draw_ring(r: &mut Renderer, t: &TileView, rc: RectF) -> Result<()> {
    r.fill_rounded_rect(rc, TILE_RADIUS, TILE_FILL)?;
    r.stroke_rounded_rect(rc, TILE_RADIUS, TILE_BORDER, 1.0)?;
    // Gauge track: a 22 px circle drawn as a fully rounded rect outline.
    let d = 22.0;
    let ring = RectF { x: rc.x + (rc.w - d) / 2.0, y: rc.y + (rc.h - d) / 2.0, w: d, h: d };
    r.stroke_rounded_rect(ring, d / 2.0, RING_TRACK, 2.5)?;
    r.draw_text(&t.value, rc, 9.5, TEXT, Align::Center)
}

fn draw_clock(r: &mut Renderer, t: &TileView, rc: RectF) -> Result<()> {
    r.fill_rounded_rect(rc, TILE_RADIUS, TILE_FILL)?;
    r.stroke_rounded_rect(rc, TILE_RADIUS, TILE_BORDER, 1.0)?;
    let time = RectF { x: rc.x + 2.0, y: rc.y + 2.0, w: rc.w - 4.0, h: CLOCK_TIME_H };
    r.fill_rounded_rect(time, 5.0, CLOCK_RED)?;
    r.draw_text(&t.value, time, 11.0, 0xFFFF_FFFF, Align::Center)?;

    let half = (rc.w - 4.0) / 2.0;
    let row_y = time.y + CLOCK_TIME_H + 1.0;
    let left = RectF { x: rc.x + 4.0, y: row_y, w: half, h: 12.0 };
    let right = RectF { x: rc.x + 4.0 + half, y: row_y, w: half, h: 12.0 };
    r.draw_text(&t.title, left, 8.0, MUTED, Align::Left)?;
    r.draw_text(&t.sub, right, 8.0, 0xFFFF_FFCC, Align::Right)
}
