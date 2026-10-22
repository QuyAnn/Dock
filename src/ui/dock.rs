//! Dock geometry, hit testing and drawing, matching the old Electron dock (styles.css / Dock.tsx).
//! Three zones: left (git, AI, media), center (running apps), right (Control Center, settings,
//! CPU, RAM, clock). Coordinates are dock-local DIPs (the dock window's client area).
use windows::core::Result;
use std::sync::OnceLock;
use std::time::Instant;

use super::liquid::{glass_fill_color, LiquidBitmap, GLASS_SHEEN_TOP};
use super::renderer::{Align, Renderer, TextStyle};
use super::svgpath;

pub const DOCK_H: f32 = 46.0;
const BAR_RADIUS: f32 = 16.0;
const TILE_H: f32 = 34.0;
const TILE_RADIUS: f32 = 10.0;
const ROW_PAD: f32 = 5.0; // styles.css .dock-row padding: 0 5px
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
const RING_TRACK: u32 = 0xFFFF_FF33; // rgba(255,255,255,.20) — visible grey track behind accent arc
const BTN_FILL: u32 = 0xFFFF_FF12; // .cc-btn background, rgba(255,255,255,.07)
const BTN_BORDER: u32 = 0xFFFF_FF14; // rgba(255,255,255,.08)
const NO_BACKDROP_TOP: u32 = 0x6E68_62FF;
const NO_BACKDROP_BOTTOM: u32 = 0x5A55_50FF;
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
    /// Windows logo button that opens Start.
    Start,
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
    /// Live endpoint peak for the media visualizer, 0..1.
    pub audio_peak: f32,
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
            audio_peak: 0.0,
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

/// Minimum clear space between a side zone and the centre cluster.
pub const CENTER_GAP: f32 = 12.0;
/// Media keeps at least this width before other left tiles are squeezed.
const MEDIA_MIN_W: f32 = 150.0;

/// Places tiles in three zones. Returns one rect per tile, in the order: left, center, right.
///
/// The centre cluster (Start + app icons) is centred on `screen_w / 2` exactly and never moves
/// for the side zones: left/right tiles are clamped to stop `CENTER_GAP` short of it. When a
/// side zone has spare room, the flexible tile (`media`) grows toward the cluster; when it is
/// too wide, `media` shrinks first (down to `MEDIA_MIN_W`), then every tile scales down.
pub fn layout(left: &[Tile], center: &[Tile], right: &[Tile], screen_w: f32) -> Vec<RectF> {
    let mut out = Vec::with_capacity(left.len() + center.len() + right.len());
    let gaps = |n: usize| ZONE_GAP * n.saturating_sub(1) as f32;
    let zone_w = |tiles: &[Tile]| -> f32 { tiles.iter().map(|t| t.width).sum::<f32>() + gaps(tiles.len()) };
    let place = |tiles: &[Tile], widths: &[f32], x: f32, out: &mut Vec<RectF>| {
        let mut x = x;
        for (t, &w) in tiles.iter().zip(widths) {
            let h = if t.height > 0.0 { t.height } else { TILE_H };
            out.push(RectF { x, y: (DOCK_H - h) / 2.0, w, h });
            x += w + ZONE_GAP;
        }
    };

    let center_w = zone_w(center);
    let center_x = (screen_w - center_w) / 2.0;
    let center_right = center_x + center_w;
    let (left_max, right_max) = if center.is_empty() {
        let half = (screen_w / 2.0 - ROW_PAD - CENTER_GAP / 2.0).max(0.0);
        (half, half)
    } else {
        (
            (center_x - CENTER_GAP - ROW_PAD).max(0.0),
            (screen_w - ROW_PAD - CENTER_GAP - center_right).max(0.0),
        )
    };

    let left_widths = side_widths(left, left_max, true);
    place(left, &left_widths, ROW_PAD, &mut out);
    place(center, &center.iter().map(|t| t.width).collect::<Vec<_>>(), center_x, &mut out);
    // Right packs to the trailing edge (clock / rings stay put); only media would grow there.
    let right_widths = side_widths(right, right_max, false);
    let fitted_right_w: f32 = right_widths.iter().sum::<f32>() + gaps(right.len());
    place(right, &right_widths, screen_w - ROW_PAD - fitted_right_w, &mut out);
    out
}

/// Tile widths for one side zone so the zone fits in `max_w`. `grow` lets `media` soak spare room.
fn side_widths(tiles: &[Tile], max_w: f32, grow: bool) -> Vec<f32> {
    let mut widths: Vec<f32> = tiles.iter().map(|t| t.width).collect();
    if tiles.is_empty() {
        return widths;
    }
    let gaps = ZONE_GAP * tiles.len().saturating_sub(1) as f32;
    let natural = widths.iter().sum::<f32>() + gaps;
    let media = tiles.iter().position(|t| t.id == "media");
    if natural <= max_w {
        if let (true, Some(i)) = (grow, media) {
            widths[i] += max_w - natural;
        }
        return widths;
    }
    let mut over = natural - max_w;
    if let Some(i) = media {
        let give = (widths[i] - MEDIA_MIN_W).max(0.0).min(over);
        widths[i] -= give;
        over -= give;
    }
    if over > 0.0 {
        let total: f32 = widths.iter().sum();
        let scale = ((total - over).max(0.0) / total.max(1.0)).clamp(0.0, 1.0);
        for w in &mut widths {
            *w *= scale;
        }
    }
    widths
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
    match backdrop {
        Some(b) => r.draw_bitmap(b.w, b.h, &b.rgba, bar)?,
        // No wallpaper crop yet: the HWND target has no real alpha, so a transparent clear
        // shows as black. Paint a neutral mid-tone glass base instead of a near-black bar.
        None => r.fill_top_rounded_rect_gradient(bar, BAR_RADIUS, NO_BACKDROP_TOP, NO_BACKDROP_BOTTOM)?,
    }
    let tint = glass_fill_color(opacity_pct);
    // Flush to the monitor edge: round only the top corners. Rounding the bottom lets a few
    // sharp wallpaper pixels show through at the lower left/right.
    r.fill_top_rounded_rect(bar, BAR_RADIUS, tint)?;
    // A second fill below y=16 doubled the tint alpha and created a hard horizontal seam.
    r.fill_top_rounded_rect_gradient(bar, BAR_RADIUS, GLASS_SHEEN_TOP, 0xFFFF_FF00)?;
    r.stroke_top_rounded_rect(bar, BAR_RADIUS, BAR_BORDER, 1.0)?;

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
            Style::Start => draw_start(r, *rc, hovered)?,
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
    // Dock-Rc Ring: 28 px box, r=11.5, stroke 2.4, track + accent; tiny label above the percent.
    let (cx, cy) = (rc.x + rc.w / 2.0, rc.y + rc.h / 2.0);
    let radius = 11.5;
    let width = 2.4;
    r.stroke_arc(cx, cy, radius, 0.0, 360.0, RING_TRACK, width)?;
    if let Some(p) = t.progress {
        let accent = if t.accent != 0 { t.accent } else { 0x4AA8_FFFF };
        let sweep = 360.0 * p.clamp(0.0, 1.0);
        if sweep >= 0.5 {
            r.stroke_arc(cx, cy, radius, -90.0, sweep, accent, width)?;
        }
    }
    let inset = radius - width;
    let inner = RectF { x: cx - inset, y: cy - inset, w: inset * 2.0, h: inset * 2.0 };
    if !t.title.is_empty() {
        // Label above the number so "CPU"/"RAM" never collides with the percent.
        r.draw_text(
            &t.title,
            RectF { x: inner.x, y: inner.y + 0.5, w: inner.w, h: 7.5 },
            6.0,
            MUTED,
            Align::Center,
        )?;
        r.draw_text_styled(
            &t.value,
            RectF { x: inner.x, y: cy - 3.0, w: inner.w, h: inset },
            8.5,
            TEXT,
            Align::Center,
            TextStyle::Bold,
        )?;
    } else {
        r.draw_text_styled(&t.value, inner, 9.0, TEXT, Align::Center, TextStyle::Bold)?;
    }
    Ok(())
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
    let elapsed = {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_secs_f32()
    };
    let peak = t.audio_peak.sqrt();
    let has_peak = peak > 0.04;
    let playing = t.progress.is_some();
    for i in 0..14 {
        let pulse = ((elapsed * 10.0 + i as f32 * 1.73).sin() * 0.5 + 0.5).powf(0.7);
        let shape = 0.45 + 0.55 * ((i as f32 - 6.5).abs() / 6.5 * std::f32::consts::PI).cos().abs();
        // Peak drives the bars when audio is audible; otherwise a soft idle pulse while "playing".
        let h = if playing && has_peak {
            3.0 + peak * (5.0 + 18.0 * pulse * shape)
        } else if playing {
            3.0 + (0.2 + 0.35 * pulse * shape) * 10.0
        } else {
            4.0
        };
        r.fill_rounded_rect(
            RectF { x: rc.x + 7.0 + i as f32 * 4.0, y: rc.y + (rc.h - h) / 2.0, w: 2.0, h },
            1.0,
            if playing { 0xC7A6_FFFF } else { 0xC7A6_FF8C },
        )?;
    }
    let x = rc.x + 65.0;
    let w = (rc.x + rc.w - 7.0 - x).max(0.0);
    // Full strings (Vietnamese diacritics intact); DirectWrite ellipsizes only when they overflow.
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

fn draw_start(r: &mut Renderer, rc: RectF, hovered: bool) -> Result<()> {
    // Liquid-glass plate — same helper / radius as centre app icon plates.
    super::glass::draw_liquid_plate(r, rc, super::glass::PLATE_RADIUS, hovered)?;
    let d = 8.0;
    let gap = 1.5;
    let total = d * 2.0 + gap;
    let x = rc.x + (rc.w - total) / 2.0;
    let y = rc.y + (rc.h - total) / 2.0;
    for row in 0..2 {
        for col in 0..2 {
            let tile = RectF {
                x: x + col as f32 * (d + gap),
                y: y + row as f32 * (d + gap),
                w: d,
                h: d,
            };
            r.fill_rounded_rect_gradient(tile, 1.3, 0x65C7_FFFF, 0x1688_FFFF)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &'static str, width: f32) -> Tile {
        Tile { id, width, height: 0.0 }
    }

    fn home_zones(apps: usize) -> (Vec<Tile>, Vec<Tile>, Vec<Tile>) {
        let left = vec![t("git", 118.0), t("claude", 100.0), t("codex", 100.0), t("cursor", 155.0), t("grok", 100.0), t("media", 200.0)];
        let center = vec![t("start", 38.0), Tile { id: "apps", width: super::super::apps::cluster_width(apps), height: 44.0 }];
        let right = vec![t("cc", 56.0), t("gear", 22.0), t("cpu", 42.0), t("ram", 42.0), t("clock", 64.0)];
        (left, center, right)
    }

    #[test]
    fn dock_layout_center_cluster_is_screen_centred() {
        for &(w, apps) in &[(1920.0, 5), (1920.0, 1), (1536.0, 9), (2560.0, 12), (1280.0, 30)] {
            let (l, c, r) = home_zones(apps);
            let rects = layout(&l, &c, &r, w);
            let first = rects[l.len()];
            let last = rects[l.len() + c.len() - 1];
            let mid = (first.x + last.x + last.w) / 2.0;
            assert!((mid - w / 2.0).abs() < 0.01, "w={w} apps={apps} mid={mid}");
        }
    }

    #[test]
    fn dock_layout_sides_never_overlap_cluster() {
        for &(w, apps) in &[(1920.0, 5), (1280.0, 12), (1024.0, 8)] {
            let (l, c, r) = home_zones(apps);
            let rects = layout(&l, &c, &r, w);
            let left_end = rects[l.len() - 1].x + rects[l.len() - 1].w;
            let c0 = rects[l.len()].x;
            let c1 = rects[l.len() + c.len() - 1].x + rects[l.len() + c.len() - 1].w;
            let right_start = rects[l.len() + c.len()].x;
            assert!(c0 - left_end >= CENTER_GAP - 0.01, "w={w} left_end={left_end} c0={c0}");
            assert!(right_start - c1 >= CENTER_GAP - 0.01, "w={w} c1={c1} right={right_start}");
        }
    }

    #[test]
    fn dock_layout_media_grows_up_to_cluster_gap() {
        let (l, c, r) = home_zones(5);
        let rects = layout(&l, &c, &r, 1920.0);
        let media = rects[l.len() - 1];
        let c0 = rects[l.len()].x;
        assert!((c0 - (media.x + media.w) - CENTER_GAP).abs() < 0.01);
        assert!(media.w >= 200.0);
    }
}
