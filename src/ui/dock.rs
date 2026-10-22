//! Dock geometry, hit testing and drawing, matching the old Electron dock (styles.css / Dock.tsx).
//! Three zones: left (git, AI, media), center (running apps), right (Control Center, settings,
//! CPU, RAM, clock). Coordinates are dock-local DIPs (the dock window's client area).
use windows::core::Result;

use super::liquid::{glass_fill_color, LiquidBitmap, GLASS_SHEEN_TOP};
use super::renderer::{Align, Renderer, TextStyle};
use super::svgpath;

pub const DOCK_H: f32 = 46.0;
const BAR_RADIUS: f32 = 16.0;
const TILE_H: f32 = 34.0;
const TILE_RADIUS: f32 = 10.0;
const ROW_PAD: f32 = 7.0; // equal optical inset at both ends; keeps edge strokes unclipped
const ZONE_GAP: f32 = 5.0; // .zone gap
const ROW_GAP: f32 = 6.0; // .dock-row gap between zones

// Colors from styles.css, 0xRRGGBBAA.
const BAR_BORDER: u32 = 0xFFFF_FF42; // liquid rim top, rgba(255,255,255,.26)
const TILE_FILL: u32 = 0x2C2C_30A1; // rgba(44,44,48,.63): --gt at default --ga .55
const TILE_BORDER: u32 = 0xFFFF_FF1F; // rgba(255,255,255,.12)
const TILE_HOVER_FILL: u32 = 0x1E1E_23A1; // rgba(30,30,35,.63)
const TILE_HOVER_BORDER: u32 = 0xFFFF_FF24; // rgba(255,255,255,.14)
const TEXT: u32 = 0xF1F1_F4FF;
const MUTED: u32 = 0xFFFF_FF8C; // rgba(255,255,255,.55)
const RING_TRACK: u32 = 0xFFFF_FF1F; // rgba(255,255,255,.12)
const BTN_FILL: u32 = 0xFFFF_FF12; // .cc-btn background, rgba(255,255,255,.07)
const BTN_BORDER: u32 = 0xFFFF_FF14; // rgba(255,255,255,.08)
const CLOCK_TOP: u32 = 0xFF5A_3CFF;
const CLOCK_BOTTOM: u32 = 0xE832_1EFF;
const HM_EMPTY: u32 = 0xFFFF_FF14; // .hm.l0
const HM_LEVELS: [u32; 4] = [0x0E44_29FF, 0x006D_32FF, 0x26A6_41FF, 0x39D3_53FF];

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
    /// Rounded card: optional icon square, title (muted, small) above value, optional bottom bar.
    Card,
    /// 34 px card with a circular gauge and the value centered (CPU, RAM).
    Ring,
    /// Red time block above a title/sub row (clock).
    Clock,
    /// Git card: numbers line above a 16x3 mini heatmap (`cells`).
    Git,
    /// Now-playing tile: waveform plus ellipsized title/artist.
    Media,
    /// Control Center button: value holds Segoe MDL2 glyphs drawn side by side.
    Button,
    /// No background (the apps zone, the settings gear). `value` is the glyph or text.
    Bare,
}

/// Plain values for one tile. Caller fills these from model data.
pub struct TileView {
    pub style: Style,
    pub title: String,
    pub value: String,
    /// Second line: weather for `Clock`, numbers for `Git`, reset text for AI.
    pub sub: String,
    pub icon: Option<Icon>,
    /// 0..1: gauge fill for `Ring`, bottom bar for `Card` (AI usage). None hides it.
    pub progress: Option<f32>,
    /// Gauge/bar color, 0xRRGGBBAA. 0 means the default.
    pub accent: u32,
    /// Icon square background for AI tiles, 0xRRGGBBAA. 0 means no square.
    pub bg: u32,
    /// Git heatmap levels 0..=4, column-major (16 columns of 3 rows). Empty when unused.
    pub cells: Vec<u8>,
    /// Vector glyph drawn inside `bg`: SVG path data and an optional stroke width (None = fill).
    pub glyph: Option<(&'static str, Option<f32>)>,
}

impl TileView {
    /// Card with a title and a value, no extras.
    pub fn card(title: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            style: Style::Card,
            title: title.into(),
            value: value.into(),
            sub: String::new(),
            icon: None,
            progress: None,
            accent: 0,
            bg: 0,
            cells: Vec::new(),
            glyph: None,
        }
    }

    /// Tile with no content; used for zones drawn elsewhere.
    pub fn bare(value: impl Into<String>) -> Self {
        Self { style: Style::Bare, ..Self::card("", value) }
    }
}

/// Places tiles in three zones. Center group is centered on the bar; left and right hug the edges.
/// Returns one rect per tile, in the order: left, center, right.
pub fn layout(left: &[Tile], center: &[Tile], right: &[Tile], screen_w: f32) -> Vec<RectF> {
    let mut out = Vec::with_capacity(left.len() + center.len() + right.len());
    let zone_w = |tiles: &[Tile]| -> f32 {
        tiles.iter().map(|t| t.width).sum::<f32>() + ZONE_GAP * tiles.len().saturating_sub(1) as f32
    };
    let place = |tiles: &[Tile], x: f32, max_w: f32, out: &mut Vec<RectF>| -> f32 {
        let mut x = x;
        let gaps = ZONE_GAP * tiles.len().saturating_sub(1) as f32;
        let widths = tiles.iter().map(|t| t.width).sum::<f32>();
        let scale = if widths + gaps > max_w {
            ((max_w - gaps).max(0.0) / widths.max(1.0)).min(1.0)
        } else {
            1.0
        };
        for t in tiles {
            let h = if t.height > 0.0 { t.height } else { TILE_H };
            let w = t.width * scale;
            out.push(RectF { x, y: (DOCK_H - h) / 2.0, w, h });
            x += w + ZONE_GAP;
        }
        x
    };

    let center_w = zone_w(center);
    let center_x = (screen_w - center_w) / 2.0;
    let side_w = (center_x - ROW_PAD - ROW_GAP).max(0.0);
    place(left, ROW_PAD, side_w, &mut out);
    place(center, center_x, center_w, &mut out);

    let right_w = zone_w(right);
    let fitted_right_w = right_w.min(side_w);
    let right_x = screen_w - ROW_PAD - fitted_right_w;
    place(right, right_x, side_w, &mut out);
    out
}

/// Index of the rect containing (x, y), if any.
pub fn hit_test(x: f32, y: f32, rects: &[RectF]) -> Option<usize> {
    rects
        .iter()
        .position(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
}

/// Draws the dock bar (full renderer size), then each tile at `rects[i]`.
/// `hover` is the index of the tile under the pointer, if any.
/// Call between `Renderer::begin` and `Renderer::end`.
pub fn draw_dock(
    r: &mut Renderer,
    tiles: &[TileView],
    rects: &[RectF],
    hover: Option<usize>,
    backdrop: Option<&LiquidBitmap>,
    opacity_pct: u8,
) -> Result<()> {
    let (w, _) = r.size();
    let bar = RectF { x: 0.0, y: 0.0, w, h: DOCK_H };
    // The wallpaper blur is the backdrop. The readable smoky tint must sit above it, not below
    // an opaque bitmap where it would disappear.
    if let Some(b) = backdrop {
        r.draw_bitmap(b.w, b.h, &b.rgba, bar)?;
    }
    let tint = glass_fill_color(opacity_pct);
    r.fill_rounded_rect(bar, BAR_RADIUS, tint)?;
    // A second fill below y=16 doubled the tint alpha and created a hard horizontal seam.
    r.fill_rounded_rect_gradient(bar, BAR_RADIUS, GLASS_SHEEN_TOP, 0xFFFF_FF00)?;
    r.stroke_rounded_rect(bar, BAR_RADIUS, BAR_BORDER, 1.0)?;

    for (i, (t, rc)) in tiles.iter().zip(rects).enumerate() {
        let hovered = hover == Some(i);
        match t.style {
            Style::Bare => draw_bare(r, t, *rc)?,
            Style::Card => draw_card(r, t, *rc, hovered)?,
            Style::Ring => draw_ring(r, t, *rc, hovered)?,
            Style::Clock => draw_clock(r, t, *rc, hovered)?,
            Style::Git => draw_git(r, t, *rc, hovered)?,
            Style::Media => draw_media(r, t, *rc, hovered)?,
            Style::Button => draw_button(r, t, *rc, hovered)?,
        }
    }
    Ok(())
}

/// Card background shared by every boxed tile, with the hover state of `.tile:hover`.
fn card_frame(r: &mut Renderer, rc: RectF, hovered: bool) -> Result<()> {
    let (fill, border) = if hovered { (TILE_HOVER_FILL, TILE_HOVER_BORDER) } else { (TILE_FILL, TILE_BORDER) };
    r.fill_rounded_rect(rc, TILE_RADIUS, fill)?;
    r.fill_rounded_rect_gradient(
        rc,
        TILE_RADIUS,
        if hovered { 0xFFFF_FF24 } else { 0xFFFF_FF12 },
        if hovered { 0xFFFF_FF0D } else { 0xFFFF_FF05 },
    )?;
    r.stroke_rounded_rect(rc, TILE_RADIUS, border, 1.0)
}

fn draw_bare(r: &mut Renderer, t: &TileView, rc: RectF) -> Result<()> {
    if let Some(icon) = &t.icon {
        r.draw_bitmap(icon.w, icon.h, &icon.rgba, rc)?;
    }
    if !t.value.is_empty() {
        // Gear: .gear-btn at 45% opacity.
        r.draw_text_styled(&t.value, rc, 12.0, 0xFFFF_FF73, Align::Center, TextStyle::Icon)?;
    }
    Ok(())
}

fn draw_card(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let mut text_x = rc.x + 7.0;
    if t.bg != 0 {
        // .ai-ico: 22 px square, radius 6, colored background with a glyph.
        let sq = RectF { x: rc.x + 5.0, y: rc.y + (rc.h - 22.0) / 2.0, w: 22.0, h: 22.0 };
        r.fill_rounded_rect(sq, 6.0, t.bg)?;
        if let Some(icon) = &t.icon {
            r.draw_bitmap(icon.w, icon.h, &icon.rgba, RectF { x: sq.x + 4.0, y: sq.y + 4.0, w: 14.0, h: 14.0 })?;
        }
        if let Some((d, stroke)) = t.glyph {
            // Provider logos are 24-unit SVG paths in white.
            let area = RectF { x: sq.x + 4.0, y: sq.y + 4.0, w: 14.0, h: 14.0 };
            svgpath::draw_path(r, d, area, 24.0, 0xFFFF_FFFF, stroke)?;
            if t.title == "Cursor" {
                svgpath::draw_path(r, svgpath::CURSOR_CUBE_INNER, area, 24.0, 0xE8E8_E8FF, Some(1.2))?;
                svgpath::draw_path(r, svgpath::CURSOR_CUBE_TOP, area, 24.0, 0xE8E8_E8D9, None)?;
            }
        }
        text_x = sq.x + 22.0 + 6.0;
    }
    let text_w = (rc.x + rc.w - 7.0 - text_x).max(0.0);
    if t.value.is_empty() {
        r.draw_text(&t.title, RectF { x: text_x, y: rc.y, w: text_w, h: rc.h }, 9.5, TEXT, Align::Left)?;
        return Ok(());
    }
    let title = RectF { x: text_x, y: rc.y + 4.0, w: text_w, h: 11.0 };
    r.draw_text(&t.title, title, 8.5, MUTED, Align::Left)?;
    let value_w = if t.sub.is_empty() { text_w } else { text_w * 0.55 };
    let value = RectF { x: text_x, y: rc.y + 14.0, w: value_w, h: 14.0 };
    r.draw_text_styled(&t.value, value, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    if !t.sub.is_empty() {
        let sub = RectF { x: text_x + value_w, y: rc.y + 14.0, w: text_w - value_w, h: 14.0 };
        r.draw_text(&t.sub, sub, 8.0, MUTED, Align::Right)?;
    }
    if let Some(p) = t.progress {
        // .pbar: 2.5 px track along the bottom, colored fill.
        let track = RectF { x: rc.x + 5.0, y: rc.y + rc.h - 4.0, w: rc.w - 10.0, h: 2.5 };
        r.fill_rounded_rect(track, 1.25, RING_TRACK)?;
        let fill_w = track.w * p.clamp(0.0, 1.0);
        if fill_w > 0.0 {
            let fill = RectF { w: fill_w, ..track };
            r.fill_rounded_rect(fill, 1.25, if t.accent != 0 { t.accent } else { TEXT })?;
        }
    }
    Ok(())
}

fn draw_ring(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    // SVG ring: 28 px box, radius 11.5, stroke 2.4, starts at 12 o'clock.
    let (cx, cy) = (rc.x + rc.w / 2.0, rc.y + rc.h / 2.0);
    let radius = 11.5;
    let width = 2.4;
    r.stroke_arc(cx, cy, radius, 0.0, 360.0, RING_TRACK, width)?;
    if let Some(p) = t.progress {
        let accent = if t.accent != 0 { t.accent } else { 0x4AA8_FFFF };
        r.stroke_arc(cx, cy, radius, -90.0, 360.0 * p.clamp(0.0, 1.0), accent, width)?;
    }
    r.draw_text_styled(&t.value, rc, 7.5, TEXT, Align::Center, TextStyle::Bold)
}

fn draw_clock(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let time = RectF { x: rc.x + 2.0, y: rc.y + 2.0, w: rc.w - 4.0, h: 15.0 };
    r.fill_rounded_rect_gradient(time, 5.0, CLOCK_TOP, CLOCK_BOTTOM)?;
    r.draw_text_styled(&t.value, time, 11.0, 0xFFFF_FFFF, Align::Center, TextStyle::Bold)?;

    let row = RectF { x: rc.x + 4.0, y: time.y + time.h + 1.0, w: rc.w - 8.0, h: 12.0 };
    let date_w = 26.0_f32.min(row.w);
    r.draw_text(&t.title, RectF { w: date_w, ..row }, 8.0, MUTED, Align::Left)?;
    r.draw_text(
        &t.sub,
        RectF { x: row.x + date_w + 1.0, w: (row.w - date_w - 1.0).max(0.0), ..row },
        8.5,
        0xFFFF_FFCC,
        Align::Right,
    )
}

fn draw_git(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    // .git-nums: 10 px line of muted numbers, then the heatmap.
    let nums = RectF { x: rc.x + 7.0, y: rc.y + 3.0, w: rc.w - 14.0, h: 10.0 };
    r.draw_text(&t.value, nums, 8.5, MUTED, Align::Left)?;

    let cell = 5.0;
    let gap = 1.5;
    let rows = 3usize;
    let top = rc.y + 15.0;
    let left = rc.x + 7.0;
    for (i, level) in t.cells.iter().enumerate() {
        let (col, row) = (i / rows, i % rows);
        let x = left + col as f32 * (cell + gap);
        let y = top + row as f32 * (cell + gap);
        let color = match level {
            0 => HM_EMPTY,
            l => HM_LEVELS[(*l as usize - 1).min(3)],
        };
        r.fill_rounded_rect(RectF { x, y, w: cell, h: cell }, 1.5, color)?;
    }
    Ok(())
}

fn draw_media(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    const WAVE: [f32; 14] = [5.0, 9.0, 15.0, 7.0, 18.0, 11.0, 4.0, 13.0, 17.0, 8.0, 14.0, 5.0, 10.0, 16.0];
    for (i, full) in WAVE.iter().enumerate() {
        let h = if t.progress.is_some() { *full } else { 4.0 };
        r.fill_rounded_rect(
            RectF { x: rc.x + 7.0 + i as f32 * 4.0, y: rc.y + (rc.h - h) / 2.0, w: 2.0, h },
            1.0,
            if t.progress.is_some() { 0xC7A6_FFFF } else { 0xC7A6_FF8C },
        )?;
    }
    let x = rc.x + 65.0;
    let w = (rc.x + rc.w - 7.0 - x).max(0.0);
    r.draw_text_styled(&t.title, RectF { x, y: rc.y + 4.0, w, h: 14.0 }, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(&t.sub, RectF { x, y: rc.y + 18.0, w, h: 11.0 }, 8.5, MUTED, Align::Left)
}

fn draw_button(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    // .cc-btn: 34 px, radius 8, faint fill; hover brightens it.
    let fill = if hovered { 0xFFFF_FF26 } else { BTN_FILL };
    r.fill_rounded_rect(rc, 8.0, fill)?;
    r.stroke_rounded_rect(rc, 8.0, BTN_BORDER, 1.0)?;
    // Glyphs sit side by side, 14 px each with a 7 px gap.
    let mut x = rc.x + 9.0;
    for (index, glyph) in t.value.chars().enumerate() {
        let g = RectF { x, y: rc.y, w: 14.0, h: rc.h };
        let color = if index == 0 {
            match t.progress {
                Some(p) if p > 0.5 => 0xFFFF_FFFF,
                Some(_) => 0xFFFF_FF73,
                None => 0xFFFF_FFC4,
            }
        } else {
            0xFFFF_FFE8
        };
        r.draw_text_styled(&glyph.to_string(), g, 13.0, color, Align::Center, TextStyle::Icon)?;
        x += 14.0 + 7.0;
    }
    Ok(())
}
