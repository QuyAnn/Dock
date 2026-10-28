//! Dock geometry, hit testing and drawing, matching the old Electron dock (styles.css / Dock.tsx).
//! Three zones: left (git, AI, media), center (running apps), right (Control Center, settings,
//! CPU, RAM, clock). Coordinates are dock-local DIPs (the dock window's client area).
use windows::core::Result;
use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

use super::liquid::{glass_fill_color, LiquidBitmap, GLASS_SHEEN_TOP};
use super::renderer::{Align, Renderer, TextStyle};
use super::svgpath;
use crate::win::edge::AbEdge;

/// Bar height in DIPs. 52 leaves a 5-DIP clear margin above and below the 42-DIP centre
/// cluster (shelf + 38 px icons + running dots), so the shelf's top rim is never clipped.
pub const DOCK_H: f32 = 52.0;
const BAR_RADIUS: f32 = 18.0;
const TILE_H: f32 = 42.0; // same height as the centre cluster
const ROW_PAD: f32 = 5.0; // styles.css .dock-row padding: 0 5px
/// Every tile (widgets and centre cluster) sits this many DIPs above the bar's vertical centre.
pub const CENTER_RISE: f32 = 2.0;
const ZONE_GAP: f32 = 5.0; // .zone gap
const ROW_GAP: f32 = 6.0; // .dock-row gap between zones

// Colors from styles.css, 0xRRGGBBAA.
const BAR_BORDER: u32 = 0xFFFF_FF42; // liquid rim top, rgba(255,255,255,.26)
const TEXT: u32 = 0xF1F1_F4FF;
const MUTED: u32 = 0xFFFF_FFC0; // rgba(255,255,255,.75) — brighter on clear glass
const RING_TRACK: u32 = 0xFFFF_FF33; // rgba(255,255,255,.20) — visible grey track behind accent arc
const NO_BACKDROP_TOP: u32 = 0x6E68_62FF;
const NO_BACKDROP_BOTTOM: u32 = 0x5A55_50FF;
const CLOCK_TOP: u32 = 0xFF5A_3CFF;
const CLOCK_BOTTOM: u32 = 0xE832_1EFF;

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
    pub rgba: std::sync::Arc<Vec<u8>>,
}

/// How a tile is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    /// Original extended widgets; the view's `extra` identifies its geometry.
    Widget,
    /// AI usage: a big icon plate on the left, then one line per limit (label, bar, reset, percent).
    Usage,
    /// Rounded card: optional icon square, title (muted, small) above value, optional bottom bar.
    Card,
    /// Compact telemetry card: small heading over two full-width data rows.
    Metrics,
    /// 34 px card with a circular gauge and the value centered (CPU, RAM).
    Ring,
    /// Glass square with one bold centred label (keyboard layout).
    Label,
    /// Note card: yellow notebook icon, bold title, muted preview line, edit button.
    Note,
    /// Wide clock: weekday and date on the left, time and weather on the right.
    ClockWide,
    /// Red time block above a title/sub row (clock).
    Clock,
    /// Git card: numbers line above a 16x3 mini heatmap (`cells`).
    Git,
    /// Now-playing tile: waveform plus ellipsized title/artist.
    Media,
    /// Control Center button: value holds Segoe MDL2 glyphs drawn side by side.
    Button,
    /// Search icon in the centre cluster, next to Start.
    Search,
    /// Recycle Bin icon at the end of the centre cluster (`progress` set = it holds items).
    Trash,
    /// Windows logo button that opens Start.
    Start,
    /// Thin vertical divider in the centre cluster (Start | apps).
    Separator,
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
    /// Extra small usage bars (0..1, color) drawn top-right on a card, e.g. Cursor's
    /// "Auto + Composer" and "API models" limits next to the main $ bar.
    pub mini: Vec<(f32, u32)>,
    /// Control Center button: SF-style Wi-Fi + speaker icons (None draws `value` glyphs).
    pub status: Option<StatusIcons>,
    /// Tray-overflow chevron; `Some(open)` (points down while the flyout is open).
    pub chevron: Option<bool>,
    /// `Usage` style: one entry per limit, top to bottom.
    pub rows: Vec<UsageRow>,
    /// Second line of the wide clock's left block (the date).
    pub extra: String,
}

/// State for the dock's Wi-Fi / speaker status icons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusIcons {
    pub wifi: super::sficons::Wifi,
    pub volume: f32,
    pub muted: bool,
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
            mini: Vec::new(),
            status: None,
            chevron: None,
            rows: Vec::new(),
            extra: String::new(),
        }
    }

    /// Tile with no content; used for zones drawn elsewhere.
    pub fn bare(value: impl Into<String>) -> Self {
        Self { style: Style::Bare, ..Self::card("", value) }
    }
}

/// Fixed left-tile widths (Dock-Rc: .ai-tile 100px, .music-tile 142px, scaled up a little so
/// "$52.59/$20.00" and Vietnamese titles fit). Every usage tile shares one width.
/// Git: one stat line ("6 today  37 week  🔥 23 streak") over a 3-row strip of recent days.
pub const GIT_W: f32 = 188.0;
pub const USAGE_W: f32 = 112.0;
pub const MEDIA_W: f32 = 200.0;

/// Minimum clear space between a side zone and the centre cluster.
pub const CENTER_GAP: f32 = 12.0;
/// Media keeps at least this width before other left tiles are squeezed.
const MEDIA_MIN_W: f32 = 150.0;
/// Provider cards keep at least this width before every tile is scaled down.
const USAGE_MIN_W: f32 = 140.0;

/// Places tiles in three zones. Returns one rect per tile, in the order: left, center, right.
///
/// The centre cluster (Start + app icons) is centred on `screen_w / 2` exactly and never moves
/// for the side zones. Side tiles keep their fixed widths; only if a zone would come closer
/// than `CENTER_GAP` to the cluster (small monitor, many apps) does `media` shrink first
/// (down to `MEDIA_MIN_W`), then every tile scales down. Spare room is left as glass.
pub fn layout(left: &[Tile], center: &[Tile], right: &[Tile], screen_w: f32) -> Vec<RectF> {
    let mut out = Vec::with_capacity(left.len() + center.len() + right.len());
    let gaps = |n: usize| ZONE_GAP * n.saturating_sub(1) as f32;
    let zone_w = |tiles: &[Tile]| -> f32 { tiles.iter().map(|t| t.width).sum::<f32>() + gaps(tiles.len()) };
    let place = |tiles: &[Tile], widths: &[f32], x: f32, rise: f32, out: &mut Vec<RectF>| {
        let mut x = x;
        for (t, &w) in tiles.iter().zip(widths) {
            let h = if t.height > 0.0 { t.height } else { TILE_H };
            out.push(RectF { x, y: (DOCK_H - h) / 2.0 - rise, w, h });
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

    let left_widths = side_widths(left, left_max);
    place(left, &left_widths, ROW_PAD, CENTER_RISE, &mut out);
    place(center, &center.iter().map(|t| t.width).collect::<Vec<_>>(), center_x, CENTER_RISE, &mut out);
    // Right packs to the trailing edge (clock / rings stay put).
    let right_widths = side_widths(right, right_max);
    let fitted_right_w: f32 = right_widths.iter().sum::<f32>() + gaps(right.len());
    place(right, &right_widths, screen_w - ROW_PAD - fitted_right_w, CENTER_RISE, &mut out);
    out
}

/// Tile widths for one side zone: the fixed widths, squeezed only when they exceed `max_w`.
fn side_widths(tiles: &[Tile], max_w: f32) -> Vec<f32> {
    let mut widths: Vec<f32> = tiles.iter().map(|t| t.width).collect();
    if tiles.is_empty() {
        return widths;
    }
    let gaps = ZONE_GAP * tiles.len().saturating_sub(1) as f32;
    let natural = widths.iter().sum::<f32>() + gaps;
    let media = tiles.iter().position(|t| t.id == "media");
    if natural <= max_w {
        return widths;
    }
    let mut over = natural - max_w;
    if let Some(i) = media {
        let give = (widths[i] - MEDIA_MIN_W).max(0.0).min(over);
        widths[i] -= give;
        over -= give;
    }
    // Provider cards give up width next (down to a readable minimum) so the Git heatmap and the
    // other fixed widgets keep theirs.
    for (i, t) in tiles.iter().enumerate() {
        if over > 0.0 && matches!(t.id, "claude" | "codex" | "cursor" | "grok") {
            let give = (widths[i] - USAGE_MIN_W).max(0.0).min(over);
            widths[i] -= give;
            over -= give;
        }
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

/// Length of a widget along a vertical (left/right) dock; its thickness is the usual 42 DIP.
/// Wide widgets collapse to a compact square there (see `draw_tile_vertical`).
pub fn vertical_len(id: &str) -> f32 {
    match id {
        "cc" => 62.0,
        "chevron" => 30.0,
        "clock" => 46.0,
        _ => TILE_H,
    }
}

/// `layout` for a dock on any screen edge. Bottom is exactly `layout`; top is the same row
/// mirrored (tiles rise away from the screen edge, i.e. downwards); left/right stack the zones
/// top to bottom along `length` (the dock window's height in DIPs).
pub fn layout_edge(edge: AbEdge, left: &[Tile], center: &[Tile], right: &[Tile], length: f32) -> Vec<RectF> {
    match edge {
        AbEdge::Bottom => layout(left, center, right, length),
        AbEdge::Top => layout(left, center, right, length)
            .into_iter()
            .map(|r| RectF { y: DOCK_H - r.y - r.h, ..r })
            .collect(),
        AbEdge::Left | AbEdge::Right => layout_vertical(edge, left, center, right, length),
    }
}

/// Vertical dock: the centre cluster (Start, Search, apps, Trash) is centred on `length / 2`,
/// the "left" zone starts at the top and the "right" zone packs to the bottom. Widgets keep a
/// 42-DIP square (taller for Control Center / clock); side tiles that do not fit are hidden
/// (zero-size rect) rather than squeezed. Cells sit `CENTER_RISE` away from the screen edge.
fn layout_vertical(edge: AbEdge, left: &[Tile], center: &[Tile], right: &[Tile], length: f32) -> Vec<RectF> {
    let away = if edge == AbEdge::Left { CENTER_RISE } else { -CENTER_RISE };
    // (thickness across the dock, length along it)
    let center_cell = |t: &Tile| (if t.height > 0.0 { t.height } else { TILE_H }, t.width);
    let side_cell = |t: &Tile| (TILE_H, vertical_len(t.id));
    let rect = |(thick, len): (f32, f32), y: f32| RectF { x: (DOCK_H - thick) / 2.0 + away, y, w: thick, h: len };
    let zone_len = |cells: &[(f32, f32)]| cells.iter().map(|c| c.1).sum::<f32>() + ZONE_GAP * cells.len().saturating_sub(1) as f32;

    let center_cells: Vec<_> = center.iter().map(center_cell).collect();
    let center_len = zone_len(&center_cells);
    // Room the Control Center, tray chevron and clock need at the bottom. When the centred
    // cluster would crowd them out, the cluster slides up (left widgets give way first).
    let essential = |t: &Tile| matches!(t.id, "cc" | "chevron" | "clock");
    let essentials: Vec<f32> = right.iter().filter(|t| essential(t)).map(|t| side_cell(t).1).collect();
    let essential_len = zone_len(&essentials.iter().map(|&l| (0.0, l)).collect::<Vec<_>>());
    let mut center_y = ((length - center_len) / 2.0).max(ROW_PAD);
    if !center.is_empty() && essential_len > 0.0 {
        let max_end = length - ROW_PAD - essential_len - CENTER_GAP;
        if center_y + center_len > max_end {
            center_y = (max_end - center_len).max(ROW_PAD);
        }
    }
    let center_end = center_y + center_len;
    let (top_limit, bottom_limit) = if center.is_empty() {
        (length / 2.0 - CENTER_GAP / 2.0, length / 2.0 + CENTER_GAP / 2.0)
    } else {
        (center_y - CENTER_GAP, center_end + CENTER_GAP)
    };

    let mut out = Vec::with_capacity(left.len() + center.len() + right.len());
    let mut y = ROW_PAD;
    for t in left {
        let cell = side_cell(t);
        if y + cell.1 <= top_limit {
            out.push(rect(cell, y));
            y += cell.1 + ZONE_GAP;
        } else {
            out.push(RectF { x: rect(cell, y).x, y, w: 0.0, h: 0.0 });
        }
    }
    let mut y = center_y;
    for cell in center_cells {
        out.push(rect(cell, y));
        y += cell.1 + ZONE_GAP;
    }
    // Right zone: packed to the bottom. When it does not all fit, the Control Center, tray
    // chevron and clock keep their place first, then the tiles nearest the end.
    let budget = (length - ROW_PAD) - bottom_limit;
    let mut keep = vec![false; right.len()];
    let mut used = 0.0;
    let order = (0..right.len()).rev().filter(|&i| essential(&right[i])).chain((0..right.len()).rev().filter(|&i| !essential(&right[i])));
    for i in order {
        let need = side_cell(&right[i]).1 + if used > 0.0 { ZONE_GAP } else { 0.0 };
        if used + need <= budget {
            keep[i] = true;
            used += need;
        }
    }
    let mut placed = vec![RectF::default(); right.len()];
    let mut end = length - ROW_PAD;
    for (i, t) in right.iter().enumerate().rev() {
        let cell = side_cell(t);
        if keep[i] {
            placed[i] = rect(cell, end - cell.1);
            end -= cell.1 + ZONE_GAP;
        } else {
            placed[i] = RectF { x: rect(cell, end).x, y: end, w: 0.0, h: 0.0 };
        }
    }
    out.extend(placed);
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
    let (w, h) = r.size();
    let edge = crate::win::edge::current();
    let bar = if edge.vertical() { RectF { x: 0.0, y: 0.0, w: DOCK_H, h } } else { RectF { x: 0.0, y: 0.0, w, h: DOCK_H } };
    // The wallpaper blur is the backdrop. The readable smoky tint must sit above it, not below
    // an opaque bitmap where it would disappear.
    match backdrop {
        // True transparency: nothing is painted behind the tiles, so no seam or edge can show.
        _ if super::renderer::dwm_transparent() => {}
        // The bar itself is invisible: the plain, unblurred wallpaper crop is painted so only the
        // tiles (which sample the blurred `rgba` in glass.rs) read as glass pieces.
        Some(b) if !b.plain.is_empty() => r.draw_bitmap(b.w, b.h, &b.plain, bar)?,
        Some(b) => r.draw_bitmap(b.w, b.h, &b.rgba, bar)?,
        // No wallpaper crop yet: the HWND target has no real alpha, so a transparent clear
        // shows as black. Paint a neutral mid-tone glass base instead of a near-black bar.
        None if edge == AbEdge::Bottom => r.fill_top_rounded_rect_gradient(bar, BAR_RADIUS, NO_BACKDROP_TOP, NO_BACKDROP_BOTTOM)?,
        // Other edges: round only the inner corners by pushing the outer ones past the window.
        None => {
            let ext = match edge {
                AbEdge::Top => RectF { y: -BAR_RADIUS, h: DOCK_H + BAR_RADIUS, ..bar },
                AbEdge::Left => RectF { x: -BAR_RADIUS, w: DOCK_H + BAR_RADIUS, ..bar },
                _ => RectF { w: DOCK_H + BAR_RADIUS, ..bar },
            };
            r.fill_rounded_rect_gradient(ext, BAR_RADIUS, NO_BACKDROP_TOP, NO_BACKDROP_BOTTOM)?
        }
    }
    let _ = (glass_fill_color(opacity_pct), GLASS_SHEEN_TOP, BAR_BORDER, w);

    if let Some(shelf) = SHELF.with(Cell::get) {
        draw_shelf(r, shelf)?;
    }
    for (i, (t, rc)) in tiles.iter().zip(rects).enumerate() {
        let hovered = hover == Some(i);
        if edge.vertical() {
            // Tiles that did not fit the vertical dock are laid out with zero size.
            if rc.w > 0.0 && rc.h > 0.0 {
                draw_tile_vertical(r, t, *rc, hovered)?;
            }
        } else {
            draw_tile(r, t, *rc, hovered)?;
        }
    }
    Ok(())
}

/// Draws one tile in its style (the dock bar and the widget gallery previews share this).
pub fn draw_tile(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
        match t.style {
            Style::Widget => super::more_widgets::draw(r, t, rc, hovered)?,
            Style::Bare => draw_bare(r, t, rc)?,
            Style::Card => draw_card(r, t, rc, hovered)?,
            Style::Metrics => draw_metrics(r, t, rc, hovered)?,
            Style::Usage => draw_usage(r, t, rc, hovered)?,
            Style::Note => draw_note(r, t, rc, hovered)?,
            Style::Label => draw_label(r, t, rc, hovered)?,
            Style::Ring => draw_ring(r, t, rc, hovered)?,
            Style::Clock => draw_clock(r, t, rc, hovered)?,
            Style::ClockWide => draw_clock_wide(r, t, rc, hovered)?,
            Style::Git => draw_git(r, t, rc, hovered)?,
            Style::Media => draw_media(r, t, rc, hovered)?,
            Style::Button => draw_button(r, t, rc, hovered)?,
            Style::Start => draw_start(r, rc, hovered)?,
            Style::Trash => super::apps::draw_trash_icon(r, rc, hovered, t.progress.is_some())?,
            Style::Search => super::apps::draw_search_icon(r, rc, hovered)?,
            Style::Separator => draw_separator(r, rc)?,
        }
    Ok(())
}

/// One tile of a vertical (left/right) dock: square widgets draw as usual, wide ones collapse
/// to a compact card (small title over a bold value), the status button stacks its icons.
fn draw_tile_vertical(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    match t.style {
        Style::Ring | Style::Label | Style::Bare | Style::Start | Style::Search | Style::Trash => draw_tile(r, t, rc, hovered),
        Style::Git => draw_git_compact(r, t, rc, hovered),
        Style::Widget if t.extra == "analog" => draw_tile(r, t, rc, hovered),
        Style::Separator => r.fill_rect(RectF { x: rc.x, y: rc.y + (rc.h - 1.0) / 2.0, w: rc.w, h: 1.0 }, 0xFFFF_FF4D),
        Style::Button if t.chevron.is_some() => draw_button(r, t, rc, hovered),
        Style::Button => {
            card_frame(r, rc, hovered)?;
            let cx = rc.x + rc.w / 2.0;
            if let Some(st) = t.status {
                // Wi-Fi, speaker and Control Center icons stacked top to bottom.
                let wifi = RectF { x: cx - 7.5, y: rc.y + 6.0, w: 15.0, h: 15.0 };
                let spk = RectF { x: cx - 8.5, y: wifi.y + 18.0, w: 17.0, h: 15.0 };
                let ccr = RectF { x: cx - 7.5, y: spk.y + 18.0, w: 15.0, h: 15.0 };
                super::sficons::draw_wifi(r, wifi, st.wifi, 0xFFFF_FFFF)?;
                super::sficons::draw_speaker(r, spk, st.volume, st.muted, 0xFFFF_FFF0)?;
                return super::sficons::draw_control_center(r, ccr, 0xFFFF_FFF0);
            }
            let n = t.value.chars().count().max(1) as f32;
            let pitch = ((rc.h - 8.0) / n).min(18.0);
            let mut y = rc.y + (rc.h - pitch * n) / 2.0;
            for glyph in t.value.chars() {
                r.draw_text_styled(&glyph.to_string(), RectF { x: rc.x, y, w: rc.w, h: pitch }, 12.0, 0xFFFF_FFE8, Align::Center, TextStyle::Icon)?;
                y += pitch;
            }
            Ok(())
        }
        Style::Clock | Style::ClockWide => {
            card_frame(r, rc, hovered)?;
            let time = RectF { x: rc.x + 3.0, y: rc.y + 5.0, w: rc.w - 6.0, h: 17.0 };
            r.fill_rounded_rect_gradient(time, 5.0, CLOCK_TOP, CLOCK_BOTTOM)?;
            r.draw_text_styled(&t.value, time, 10.5, 0xFFFF_FFFF, Align::Center, TextStyle::Bold)?;
            let below = RectF { x: rc.x + 2.0, y: time.y + time.h + 2.0, w: rc.w - 4.0, h: (rc.y + rc.h - time.y - time.h - 4.0).max(0.0) };
            r.draw_text(&t.title, below, 8.0, MUTED, Align::Center)
        }
        Style::Media => {
            card_frame(r, rc, hovered)?;
            let playing = t.progress.is_some();
            let peak = t.audio_peak.sqrt();
            for i in 0..7 {
                let shape = 0.45 + 0.55 * ((i as f32 - 3.0).abs() / 3.0 * std::f32::consts::PI).cos().abs();
                let h = if playing { 4.0 + (6.0 + 14.0 * peak) * shape } else { 4.0 };
                r.fill_rounded_rect(
                    RectF { x: rc.x + (rc.w - 26.0) / 2.0 + i as f32 * 4.0, y: rc.y + (rc.h - h) / 2.0, w: 2.0, h },
                    1.0,
                    if playing { 0xC7A6_FFFF } else { 0xC7A6_FF8C },
                )?;
            }
            Ok(())
        }
        _ => {
            card_frame(r, rc, hovered)?;
            let inner = RectF { x: rc.x + 2.0, w: rc.w - 4.0, ..rc };
            if t.bg != 0 {
                // Provider square (AI usage) above the value.
                let sq = RectF { x: rc.x + (rc.w - 18.0) / 2.0, y: rc.y + 4.0, w: 18.0, h: 18.0 };
                r.fill_rounded_rect(sq, 5.0, t.bg)?;
                if let Some((d, stroke)) = t.glyph {
                    svgpath::draw_path(r, d, RectF { x: sq.x + 3.0, y: sq.y + 3.0, w: 12.0, h: 12.0 }, 24.0, 0xFFFF_FFFF, stroke)?;
                } else if let Some(icon) = &t.icon {
                    r.draw_bitmap(icon.w, icon.h, &icon.rgba, RectF { x: sq.x + 3.0, y: sq.y + 3.0, w: 12.0, h: 12.0 })?;
                }
                let value = if t.value.is_empty() { t.rows.first().map(|row| row.value.clone()).unwrap_or_default() } else { t.value.clone() };
                return r.draw_text_styled(&value, RectF { y: sq.y + 19.0, h: 12.0, ..inner }, 8.5, TEXT, Align::Center, TextStyle::Bold);
            }
            if t.value.is_empty() {
                return r.draw_text_styled(&t.title, inner, 8.5, TEXT, Align::Center, TextStyle::Bold);
            }
            r.draw_text(&t.title, RectF { y: rc.y + rc.h / 2.0 - 12.0, h: 10.0, ..inner }, 6.5, MUTED, Align::Center)?;
            r.draw_text_styled(&t.value, RectF { y: rc.y + rc.h / 2.0 - 2.0, h: 13.0, ..inner }, 9.5, TEXT, Align::Center, TextStyle::Bold)
        }
    }
}

/// Card background shared by every boxed tile: a Liquid Glass piece sampled from the bar's
/// backdrop at the tile's own position. Hover glows brighter; geometry never changes.
fn card_frame(r: &mut Renderer, rc: RectF, hovered: bool) -> Result<()> {
    super::glass::draw_glass(r, rc, super::glass::GLASS_RADIUS, super::glass::Layer::Tile, hovered)
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

fn draw_label(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    r.draw_text_styled(&t.value, rc, 12.0, TEXT, Align::Center, TextStyle::Bold)?;
    Ok(())
}

fn draw_note(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    // Notebook icon: white sheet, yellow header band, faint ruled lines.
    let s = (rc.h - 12.0).min(30.0);
    let ico = RectF { x: rc.x + 7.0, y: rc.y + (rc.h - s) / 2.0, w: s, h: s };
    r.fill_rounded_rect(ico, 6.0, 0xF4F4_F2FF)?;
    r.fill_rounded_rect(RectF { h: s * 0.3, ..ico }, 6.0, 0xFFC8_0AFF)?;
    r.fill_rect(RectF { y: ico.y + s * 0.2, h: s * 0.1, ..ico }, 0xFFC8_0AFF)?;
    for i in 0..3 {
        let y = ico.y + s * (0.46 + 0.17 * i as f32);
        r.fill_rect(RectF { x: ico.x + 3.0, y, w: s - 6.0, h: 1.0 }, 0x0000_0026)?;
    }
    // Edit button on the right.
    let b = 22.0;
    let btn = RectF { x: rc.x + rc.w - 7.0 - b, y: rc.y + (rc.h - b) / 2.0, w: b, h: b };
    r.fill_rounded_rect(btn, b / 2.0, if hovered { 0xFFFF_FF2E } else { 0xFFFF_FF1A })?;
    r.draw_text_styled("\u{E70F}", btn, 10.5, TEXT, Align::Center, TextStyle::Icon)?;
    let x = ico.x + s + 8.0;
    let w = (btn.x - 6.0 - x).max(0.0);
    r.draw_text_styled(&t.title, RectF { x, y: rc.y + rc.h / 2.0 - 14.0, w, h: 14.0 }, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(&t.sub, RectF { x, y: rc.y + rc.h / 2.0 + 1.0, w, h: 12.0 }, 9.0, MUTED, Align::Left)?;
    Ok(())
}

fn draw_card(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    // The text blocks were laid out for a 34 DIP tile: centre them in taller ones.
    let off = (rc.h - 34.0) / 2.0;
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
    let title = RectF { x: text_x, y: rc.y + off + 4.0, w: text_w, h: 11.0 };
    r.draw_text(&t.title, title, 8.5, MUTED, Align::Left)?;
    let value_w = if t.sub.is_empty() { text_w } else { text_w * 0.55 };
    let value = RectF { x: text_x, y: rc.y + off + 14.0, w: value_w, h: 14.0 };
    // Fixed tile width: long values ("$52.59/$20.00") step down a size before ellipsizing.
    let value_px = if t.value.chars().count() as f32 * 11.5 * 0.56 > value_w { 10.0 } else { 11.5 };
    r.draw_text_styled(&t.value, value, value_px, TEXT, Align::Left, TextStyle::Bold)?;
    if !t.sub.is_empty() {
        let sub = RectF { x: text_x + value_w, y: rc.y + off + 14.0, w: text_w - value_w, h: 14.0 };
        r.draw_text(&t.sub, sub, 8.0, MUTED, Align::Right)?;
    }
    // Extra limits: short stacked bars in the free top-right corner beside the title.
    for (i, (p, c)) in t.mini.iter().enumerate().take(2) {
        let track = RectF { x: rc.x + rc.w - 7.0 - 28.0, y: rc.y + off + 5.0 + i as f32 * 4.0, w: 28.0, h: 2.0 };
        r.fill_rounded_rect(track, 1.0, RING_TRACK)?;
        let fill_w = track.w * p.clamp(0.0, 1.0);
        if fill_w > 0.0 {
            r.fill_rounded_rect(RectF { w: fill_w.max(2.0), ..track }, 1.0, *c)?;
        }
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

fn draw_metrics(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let area = RectF { x: rc.x + 7.0, y: rc.y + (rc.h - 34.0) / 2.0, w: (rc.w - 14.0).max(0.0), h: 10.0 };
    r.draw_text(&t.title, area, 7.5, MUTED, Align::Left)?;
    r.draw_text_styled(&t.value, RectF { y: area.y + 10.0, h: 12.0, ..area }, 10.0, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(&t.sub, RectF { y: area.y + 22.0, h: 12.0, ..area }, 10.0, TEXT, Align::Left)?;
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
    let off = (rc.h - 34.0) / 2.0;
    let time = RectF { x: rc.x + 2.0, y: rc.y + off + 2.0, w: rc.w - 4.0, h: 15.0 };
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

/// Git tile cell colours (0xRRGGBBAA): empty days dark grey, then GitHub-style greens.
const GIT_EMPTY: u32 = 0x8A8A_9259;
const GIT_LEVELS: [u32; 4] = [0x1F6F_3AFF, 0x2EA0_43FF, 0x39D3_53FF, 0x6CEB_83FF];
const GIT_ROWS: usize = 3;
const GIT_CELL: f32 = 6.0;
const GIT_GAP: f32 = 1.5;
/// Side padding of the stat line and the grid inside the wide (horizontal) tile.
const GIT_PAD: f32 = 9.0;

/// Where the recent-days grid sits in a git tile: (left, top, columns). Columns fill the width
/// left by `GIT_PAD` on both sides; the grid is centred horizontally and sits under the stat
/// line, clear of the 12 DIP rounded corners.
pub fn git_grid(rc: RectF, compact: bool) -> (f32, f32, usize) {
    let pad = if compact { 6.0 } else { GIT_PAD };
    let pitch = GIT_CELL + GIT_GAP;
    let cols = (((rc.w - 2.0 * pad + GIT_GAP) / pitch).floor() as usize).max(1);
    let grid_w = cols as f32 * pitch - GIT_GAP;
    let grid_h = GIT_ROWS as f32 * pitch - GIT_GAP;
    let top = rc.y + rc.h - 4.0 - grid_h;
    (rc.x + (rc.w - grid_w) / 2.0, top, cols)
}

/// Colour for one day's heat level.
fn git_cell_color(level: u8) -> u32 {
    match level {
        0 => GIT_EMPTY,
        l => GIT_LEVELS[(l as usize - 1).min(3)],
    }
}

/// The most recent `GIT_ROWS * cols` days, column-major (top to bottom, then left to right),
/// so time runs left to right and today is the bottom-right cell.
fn draw_git_grid(r: &mut Renderer, cells: &[u8], left: f32, top: f32, cols: usize) -> Result<()> {
    let n = GIT_ROWS * cols;
    let pitch = GIT_CELL + GIT_GAP;
    for i in 0..n {
        // Index from the end: the last cell drawn is the newest day.
        let back = n - 1 - i;
        let level = cells.len().checked_sub(1 + back).map_or(0, |k| cells[k]);
        let (col, row) = (i / GIT_ROWS, i % GIT_ROWS);
        let cell = RectF { x: left + col as f32 * pitch, y: top + row as f32 * pitch, w: GIT_CELL, h: GIT_CELL };
        r.fill_rounded_rect(cell, 1.5, git_cell_color(level))?;
    }
    Ok(())
}

/// Wide git tile: "6 today   37 week   🔥 23 streak" over a 3-row strip of recent days.
fn draw_git(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let (left, top, cols) = git_grid(rc, false);
    draw_git_grid(r, &t.cells, left, top, cols)?;
    // Stat line: bold numbers, medium labels; three groups spread over the grid's width.
    // When the dock squeezes the tile, labels shrink, then the streak label goes, then all
    // labels go (numbers and the flame always stay).
    let span_w = cols as f32 * (GIT_CELL + GIT_GAP) - GIT_GAP;
    let num = 12.0;
    let flame_w = 13.0;
    let nums = [t.title.as_str(), t.value.as_str(), t.sub.as_str()];
    let mut nw = [0.0f32; 3];
    for (i, n) in nums.iter().enumerate() {
        nw[i] = r.measure_text(n, num, TextStyle::Bold)?;
    }
    let mut chosen = (10.0f32, [" today", " week", ""], [0.0f32; 3], f32::MAX);
    for (lab, labels) in [
        (10.0f32, [" today", " week", " streak"]),
        (9.0, [" today", " week", " streak"]),
        (9.0, [" today", " week", ""]),
        (9.0, ["", "", ""]),
    ] {
        let mut lw = [0.0f32; 3];
        for (i, l) in labels.iter().enumerate() {
            lw[i] = if l.is_empty() { 0.0 } else { r.measure_text(l, lab, TextStyle::SemiBold)? };
        }
        let total = flame_w + nw.iter().sum::<f32>() + lw.iter().sum::<f32>();
        chosen = (lab, labels, lw, total);
        if total + 2.0 * 6.0 <= span_w {
            break;
        }
    }
    let (lab, labels, lw, total) = chosen;
    let gap = ((span_w - total) / 2.0).max(3.0);
    let line = RectF { x: 0.0, y: rc.y + 3.5, w: 0.0, h: (top - rc.y - 3.5 - 1.0).max(12.0) };
    let mut x = left;
    for i in 0..3 {
        if i == 2 {
            r.draw_text("\u{1F525}", RectF { x: x - 1.0, w: flame_w + 2.0, ..line }, 9.5, 0xFFFF_FFFF, Align::Left)?;
            x += flame_w;
        }
        r.draw_text_styled(nums[i], RectF { x, w: nw[i] + 1.0, ..line }, num, TEXT, Align::Left, TextStyle::Bold)?;
        if !labels[i].is_empty() {
            r.draw_text_styled(labels[i], RectF { x: x + nw[i], w: lw[i] + 1.0, ..line }, lab, GIT_LABEL, Align::Left, TextStyle::SemiBold)?;
        }
        x += nw[i] + lw[i] + gap;
    }
    Ok(())
}

/// Labels next to the bold numbers: white at ~70% so they read as secondary but stay bold-ish.
const GIT_LABEL: u32 = 0xFFFF_FFB8;

/// Square git tile on a left/right dock (42x42): today's count on top, last days below.
fn draw_git_compact(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let (left, top, cols) = git_grid(rc, true);
    draw_git_grid(r, &t.cells, left, top, cols)?;
    let line = RectF { x: rc.x, y: rc.y + 3.5, w: rc.w, h: (top - rc.y - 4.5).max(11.0) };
    // "6 today" centred when it fits the 42 DIP square, else just the bold number.
    let (num, lab) = (11.0, 8.0);
    let nw = r.measure_text(&t.title, num, TextStyle::Bold)?;
    let lw = r.measure_text(" today", lab, TextStyle::SemiBold)?;
    if nw + lw <= rc.w - 6.0 {
        let x = rc.x + (rc.w - nw - lw) / 2.0;
        r.draw_text_styled(&t.title, RectF { x, w: nw + 1.0, ..line }, num, TEXT, Align::Left, TextStyle::Bold)?;
        r.draw_text_styled(" today", RectF { x: x + nw, w: lw + 1.0, ..line }, lab, GIT_LABEL, Align::Left, TextStyle::SemiBold)
    } else {
        r.draw_text_styled(&t.title, line, num, TEXT, Align::Center, TextStyle::Bold)
    }
}

fn draw_media(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let off = (rc.h - 34.0) / 2.0;
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
    r.draw_text_styled(&t.title, RectF { x, y: rc.y + off + 4.0, w, h: 14.0 }, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(&t.sub, RectF { x, y: rc.y + off + 18.0, w, h: 11.0 }, 8.5, MUTED, Align::Left)
}

fn draw_button(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    // .cc-btn: 34 px, radius 8, faint fill; hover brightens it.
    if let Some(open) = t.chevron {
        // Square liquid-glass key; while the flyout is open it stays lit (pressed look)
        // and the SF-style chevron flips from up to down.
        card_frame(r, rc, hovered || open)?;
        if open {
            r.fill_rounded_rect(rc, super::glass::GLASS_RADIUS, 0xFFFF_FF26)?;
        }
        let g = RectF { x: rc.x + (rc.w - 12.0) / 2.0, y: rc.y + (rc.h - 12.0) / 2.0, w: 12.0, h: 12.0 };
        let color = if hovered || open { 0xFFFF_FFFF } else { 0xFFFF_FFD9 };
        return super::sficons::draw_chevron(r, g, !open, color);
    }
    card_frame(r, rc, hovered)?;
    if let Some(st) = t.status {
        // macOS menu-bar look: Wi-Fi fan (15 px) then speaker (17x15), white, vector.
        let cy = rc.y + rc.h / 2.0;
        let wifi = RectF { x: rc.x + 7.0, y: cy - 7.5, w: 15.0, h: 15.0 };
        let spk = RectF { x: wifi.x + wifi.w + 6.0, y: cy - 7.5, w: 17.0, h: 15.0 };
        super::sficons::draw_wifi(r, wifi, st.wifi, 0xFFFF_FFFF)?;
        super::sficons::draw_speaker(r, spk, st.volume, st.muted, 0xFFFF_FFF0)?;
        let ccr = RectF { x: spk.x + spk.w + 6.0, y: cy - 7.5, w: 15.0, h: 15.0 };
        return super::sficons::draw_control_center(r, ccr, 0xFFFF_FFF0);
    }
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
    // macOS-style: white Windows logo on a blue squircle, same size and shadow as app icons.
    super::apps::draw_start_icon(r, rc, hovered)
}

fn draw_separator(r: &mut Renderer, rc: RectF) -> Result<()> {
    let line = RectF { x: rc.x + (rc.w - 1.0) / 2.0, y: rc.y, w: 1.0, h: rc.h };
    r.fill_rect(line, 0xFFFF_FF4D)
}

thread_local! {
    /// Glass shelf behind the centre cluster (Start + apps), set by the caller each frame.
    static SHELF: Cell<Option<RectF>> = const { Cell::new(None) };
}

/// Sets (or clears) the shelf drawn behind the centre cluster on the next `draw_dock`.
pub fn set_shelf(rect: Option<RectF>) {
    SHELF.with(|s| s.set(rect));
}

/// Shelf geometry for a centre cluster spanning `first..last` (dock-local DIPs): symmetric
/// horizontal padding keeps it centred with the cluster; vertically it is exactly the
/// cluster's 42-DIP band, which the layout centres in the bar (3 DIP clear above and below),
/// so icons and running dots sit inside it.
pub fn shelf_for(first: RectF, last: RectF) -> RectF {
    let pad = SHELF_PAD;
    RectF { x: first.x - pad, y: first.y, w: last.x + last.w - first.x + 2.0 * pad, h: first.h }
}

/// `shelf_for` on any edge: along a vertical dock the padding runs top/bottom instead.
pub fn shelf_for_edge(edge: AbEdge, first: RectF, last: RectF) -> RectF {
    if edge.vertical() {
        let pad = SHELF_PAD;
        RectF { x: first.x, y: first.y - pad, w: first.w, h: last.y + last.h - first.y + 2.0 * pad }
    } else {
        shelf_for(first, last)
    }
}

const SHELF_PAD: f32 = 7.0;
/// The centre cluster's capsule uses the same glass, radius and rims as every other dock
/// section (widgets, tray, clock), so all sections read as one height: the old very light
/// shelf had near-invisible rims and looked shorter than its 42 DIP (only the icons showed).
fn draw_shelf(r: &mut Renderer, rc: RectF) -> Result<()> {
    let scale = r.dpi() as f32 / 96.0;
    let rc = snap_to_pixels(rc, scale);
    super::glass::draw_glass(r, rc, super::glass::GLASS_RADIUS, super::glass::Layer::Tile, false)
}

/// Rounds a DIP rect's edges to whole device pixels at `scale` (DPI / 96).
pub fn snap_to_pixels(rc: RectF, scale: f32) -> RectF {
    let s = scale.max(0.01);
    let (x0, y0) = ((rc.x * s).round(), (rc.y * s).round());
    let (x1, y1) = (((rc.x + rc.w) * s).round(), ((rc.y + rc.h) * s).round());
    RectF { x: x0 / s, y: y0 / s, w: (x1 - x0) / s, h: (y1 - y0) / s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &'static str, width: f32) -> Tile {
        Tile { id, width, height: 0.0 }
    }

    fn home_zones(apps: usize) -> (Vec<Tile>, Vec<Tile>, Vec<Tile>) {
        // Default layout: Media lives on the right side.
        let left = vec![t("git", GIT_W), t("claude", USAGE_W), t("codex", USAGE_W), t("cursor", USAGE_W), t("grok", USAGE_W)];
        let center = vec![
            Tile { id: "start", width: super::super::apps::ICON, height: super::super::apps::CLUSTER_H },
            Tile { id: "sep", width: 1.0, height: 24.0 },
            Tile { id: "apps", width: super::super::apps::cluster_width(apps), height: super::super::apps::CLUSTER_H },
        ];
        let right = vec![t("cc", 78.0), t("media", MEDIA_W), t("cpu", 42.0), t("ram", 42.0), t("clock", 64.0)];
        (left, center, right)
    }

    #[test]
    fn dock_layout_shelf_snaps_to_device_pixels_with_equal_margins() {
        let shelf = RectF { x: 650.0, y: (DOCK_H - super::super::apps::CLUSTER_H) / 2.0, w: 600.0, h: super::super::apps::CLUSTER_H };
        for &scale in &[1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let s = snap_to_pixels(shelf, scale);
            let (top, bottom) = (s.y * scale, (s.y + s.h) * scale);
            assert!((top - top.round()).abs() < 1e-3 && (bottom - bottom.round()).abs() < 1e-3);
            // Margins inside the visible bar (rounded to whole pixels) differ by at most 1 px
            // and are equal when the bar height in px is even-split.
            let bar = (DOCK_H * scale).round();
            let (mt, mb) = (top, bar - bottom);
            assert!((mt - mb).abs() <= 1.0, "scale {scale}: {mt} vs {mb}");
        }
        assert_eq!(snap_to_pixels(shelf, 1.0).y, (DOCK_H - super::super::apps::CLUSTER_H) / 2.0);
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
    fn dock_layout_left_tiles_keep_fixed_widths_on_1080p() {
        for apps in [1, 5, 6] {
            let (l, c, r) = home_zones(apps);
            let rects = layout(&l, &c, &r, 1920.0);
            for (tile, rc) in l.iter().zip(&rects) {
                assert_eq!(rc.w, tile.width, "{} apps={apps}", tile.id);
            }
        }
    }

    #[test]
    fn dock_layout_edge_bottom_is_unchanged_and_top_is_mirrored() {
        for &(w, apps) in &[(1920.0, 5), (1280.0, 12)] {
            let (l, c, r) = home_zones(apps);
            let bottom = layout(&l, &c, &r, w);
            assert_eq!(layout_edge(AbEdge::Bottom, &l, &c, &r, w), bottom);
            let top = layout_edge(AbEdge::Top, &l, &c, &r, w);
            for (b, t) in bottom.iter().zip(&top) {
                assert_eq!((b.x, b.w, b.h), (t.x, t.w, t.h));
                assert!((t.y - (DOCK_H - b.y - b.h)).abs() < 1e-4);
                // Rises away from the top screen edge: more clearance above than below.
                assert!(t.y >= DOCK_H - t.y - t.h - 0.01, "{t:?}");
            }
        }
    }

    #[test]
    fn dock_layout_vertical_stacks_zones_without_overlap() {
        for edge in [AbEdge::Left, AbEdge::Right] {
            for &(len, apps) in &[(1032.0, 5), (864.0, 9), (700.0, 12)] {
                let (l, c, r) = home_zones(apps);
                let rects = layout_edge(edge, &l, &c, &r, len);
                assert_eq!(rects.len(), l.len() + c.len() + r.len());
                let shown: Vec<RectF> = rects.iter().copied().filter(|r| r.w > 0.0 && r.h > 0.0).collect();
                for r in &shown {
                    assert!(r.x >= 0.0 && r.x + r.w <= DOCK_H, "{edge:?} {r:?}");
                    assert!(r.y >= 0.0 && r.y + r.h <= len + 0.01, "{edge:?} len={len} {r:?}");
                }
                // Visible tiles run strictly top to bottom.
                for pair in shown.windows(2) {
                    assert!(pair[1].y >= pair[0].y + pair[0].h - 0.01, "{edge:?} len={len} {pair:?}");
                }
                // Centre cluster centred along the dock when it fits; on a short dock it may
                // only slide up to make room for the Control Center and clock.
                let first = rects[l.len()];
                let last = rects[l.len() + c.len() - 1];
                let mid = (first.y + last.y + last.h) / 2.0;
                if len >= 864.0 {
                    assert!((mid - len / 2.0).abs() < 0.01, "{edge:?} len={len} mid={mid}");
                } else {
                    assert!(mid <= len / 2.0 + 0.01 && first.y >= ROW_PAD - 0.01, "{edge:?} len={len} mid={mid}");
                }
                // Clock (last right tile) is kept at the bottom.
                let clock = rects[rects.len() - 1];
                assert!(clock.h > 0.0 && (clock.y + clock.h - (len - ROW_PAD)).abs() < 0.01);
                // The Control Center / tray chevron keep their place before optional widgets.
                for id in ["cc", "chevron"] {
                    if let Some(i) = r.iter().position(|t| t.id == id) {
                        assert!(rects[l.len() + c.len() + i].h > 0.0, "{edge:?} len={len} {id} dropped");
                    }
                }
            }
        }
        // Tiles rise away from the screen edge.
        let (l, c, r) = home_zones(5);
        let left = layout_edge(AbEdge::Left, &l, &c, &r, 1000.0);
        let right = layout_edge(AbEdge::Right, &l, &c, &r, 1000.0);
        assert!(left[l.len()].x > right[l.len()].x);
    }

    #[test]
    fn dock_layout_all_sections_share_one_height_on_every_edge() {
        // Widgets, the centre capsule (Start .. apps) and tray/status/clock tiles all have the
        // same thickness and the same position across the dock, on every edge.
        for edge in [AbEdge::Bottom, AbEdge::Top, AbEdge::Left, AbEdge::Right] {
            let (l, c, r) = home_zones(6);
            let len = if edge.vertical() { 1032.0 } else { 1920.0 };
            let rects = layout_edge(edge, &l, &c, &r, len);
            let ids: Vec<&str> = l.iter().chain(&c).chain(&r).map(|t| t.id).collect();
            let across = |rc: &RectF| if edge.vertical() { (rc.x, rc.w) } else { (rc.y, rc.h) };
            let shelf = shelf_for_edge(edge, rects[l.len()], rects[l.len() + c.len() - 1]);
            let want = across(&shelf);
            assert!((want.1 - TILE_H).abs() < 1e-4, "{edge:?} shelf {shelf:?}");
            for (id, rc) in ids.iter().zip(&rects) {
                if id.starts_with("sep") || rc.w <= 0.0 || rc.h <= 0.0 {
                    continue;
                }
                let got = across(rc);
                assert!((got.0 - want.0).abs() < 1e-4 && (got.1 - want.1).abs() < 1e-4, "{edge:?} {id} {rc:?} vs shelf {shelf:?}");
            }
        }
    }

    #[test]
    fn dock_layout_git_grid_fits_inside_the_tile() {
        for (rc, compact) in [
            (RectF { x: 10.0, y: 5.0, w: GIT_W, h: TILE_H }, false),
            (RectF { x: 5.0, y: 300.0, w: TILE_H, h: TILE_H }, true),
        ] {
            let (left, top, cols) = git_grid(rc, compact);
            let pitch = GIT_CELL + GIT_GAP;
            let (w, h) = (cols as f32 * pitch - GIT_GAP, GIT_ROWS as f32 * pitch - GIT_GAP);
            // Same 42 DIP height as every section; the grid stays clear of the tile edges and
            // leaves at least 12 DIP above it for the stat line.
            assert_eq!(rc.h, TILE_H);
            assert!(left >= rc.x + 6.0 && left + w <= rc.x + rc.w - 6.0, "{rc:?} {left} {w}");
            assert!(top >= rc.y + 15.0 && top + h <= rc.y + rc.h - 3.0, "{rc:?} {top} {h}");
            // Bottom corner cells stay inside the 12 DIP rounded corners.
            let (cx, cy) = (rc.x + 12.0, rc.y + rc.h - 12.0);
            let (dx, dy) = (cx - left, top + h - cy);
            assert!(dx <= 0.0 || dy <= 0.0 || (dx * dx + dy * dy).sqrt() <= 12.0 - 1.0, "{rc:?}");
            if compact {
                assert!(cols >= 4);
            } else {
                assert!(cols >= 20, "wide tile shows at least ~2 months: {cols}");
            }
        }
    }

    #[test]
    fn dock_layout_vertical_shelf_wraps_cluster() {
        let first = RectF { x: 5.0, y: 400.0, w: 42.0, h: 34.0 };
        let last = RectF { x: 5.0, y: 600.0, w: 42.0, h: 100.0 };
        let shelf = shelf_for_edge(AbEdge::Left, first, last);
        assert_eq!(shelf, RectF { x: 5.0, y: 393.0, w: 42.0, h: 314.0 });
        assert_eq!(shelf_for_edge(AbEdge::Bottom, first, last), shelf_for(first, last));
    }
}

/// One limit line of a `Style::Usage` tile.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    pub label: String,
    /// 0..1 fill of the bar.
    pub frac: f32,
    /// Bar colour, 0xRRGGBBAA.
    pub color: u32,
    /// Reset time or note, right of the bar ("1h 25m", "Wed 28").
    pub right: String,
    /// Percentage text at the far right ("39%").
    pub value: String,
}

/// Fixed pieces of a usage line, in DIPs.
const USAGE_LABEL_W: f32 = 32.0;
const USAGE_RESET_W: f32 = 30.0;
const USAGE_PCT_W: f32 = 26.0;
const USAGE_ROW_H: f32 = 11.0;
const USAGE_ROW_GAP: f32 = 1.5;

/// Provider card in the LumaBar style: icon plate, then label / bar / reset / percent per limit.
fn draw_usage(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let side = rc.h - 8.0;
    let plate = RectF { x: rc.x + 6.0, y: rc.y + (rc.h - side) / 2.0, w: side, h: side };
    if t.icon.is_none() {
        r.fill_rounded_rect(plate, 8.0, if t.bg != 0 { t.bg } else { 0x2A2A_30FF })?;
    }
    let inset = side * 0.06;
    let area = RectF { x: plate.x + inset, y: plate.y + inset, w: side - 2.0 * inset, h: side - 2.0 * inset };
    if let Some(icon) = &t.icon {
        // Bitmap logos (Grok Bot) fill the plate area with no backing plate.
        r.draw_bitmap(icon.w, icon.h, &icon.rgba, plate)?;
    } else if let Some((d, stroke)) = t.glyph {
        if t.title == "Cursor" {
            // Cursor's cube: lit top face, mid left face, darker right face.
            svgpath::draw_path(r, "M12 12 3.5 7.3v9.4L12 21.5Z", area, 24.0, 0xB4B4_B4FF, None)?;
            svgpath::draw_path(r, "M12 12l8.5-4.7v9.4L12 21.5Z", area, 24.0, 0x7E7E_7EFF, None)?;
            svgpath::draw_path(r, svgpath::CURSOR_CUBE_TOP, area, 24.0, 0xF4F4_F4FF, None)?;
        } else {
            svgpath::draw_path(r, d, area, 24.0, 0xFFFF_FFFF, stroke)?;
        }
    }
    let left = plate.x + plate.w + 7.0;
    let right = rc.x + rc.w - 7.0;
    let n = t.rows.len().max(1) as f32;
    let total = n * USAGE_ROW_H + (n - 1.0) * USAGE_ROW_GAP;
    let top = rc.y + (rc.h - total) / 2.0;
    for (i, row) in t.rows.iter().enumerate() {
        let y = top + i as f32 * (USAGE_ROW_H + USAGE_ROW_GAP);
        r.draw_text_styled(&row.label, RectF { x: left, y, w: USAGE_LABEL_W, h: USAGE_ROW_H }, 8.5, TEXT, Align::Left, TextStyle::Bold)?;
        let pct = RectF { x: right - USAGE_PCT_W, y, w: USAGE_PCT_W, h: USAGE_ROW_H };
        r.draw_text_styled(&row.value, pct, 9.0, TEXT, Align::Right, TextStyle::Bold)?;
        let mut bar_right = pct.x - 4.0;
        // Narrow card (dock squeezed): drop the reset text so the bar keeps a usable length.
        let bar_left = left + USAGE_LABEL_W + 4.0;
        let room_with_reset = pct.x - 4.0 - USAGE_RESET_W - 4.0 - bar_left;
        if !row.right.is_empty() && room_with_reset >= 30.0 {
            let reset = RectF { x: pct.x - 4.0 - USAGE_RESET_W, y, w: USAGE_RESET_W, h: USAGE_ROW_H };
            r.draw_text(&row.right, reset, 7.5, MUTED, Align::Right)?;
            bar_right = reset.x - 4.0;
        }
        let track = RectF { x: bar_left, y: y + (USAGE_ROW_H - 3.0) / 2.0, w: (bar_right - bar_left).max(8.0), h: 3.0 };
        r.fill_rounded_rect(track, 1.5, RING_TRACK)?;
        let fill_w = track.w * row.frac.clamp(0.0, 1.0);
        if fill_w > 0.0 {
            r.fill_rounded_rect(RectF { w: fill_w.max(3.0), ..track }, 1.5, row.color)?;
        }
    }
    Ok(())
}

/// Wide clock: weekday (red caps) over the date, a divider, then the time over the weather.
fn draw_clock_wide(r: &mut Renderer, t: &TileView, rc: RectF, hovered: bool) -> Result<()> {
    card_frame(r, rc, hovered)?;
    let mid = rc.x + rc.w * 0.52;
    let left = RectF { x: rc.x + 10.0, y: rc.y, w: mid - rc.x - 16.0, h: rc.h };
    r.draw_text_styled(&t.title, RectF { y: rc.y + (rc.h - 30.0) / 2.0, h: 11.0, ..left }, 8.5, 0xFF5D_4EFF, Align::Left, TextStyle::Bold)?;
    r.draw_text_styled(&t.extra, RectF { y: rc.y + (rc.h - 30.0) / 2.0 + 12.0, h: 18.0, ..left }, 13.0, TEXT, Align::Left, TextStyle::Bold)?;
    r.fill_rect(RectF { x: mid, y: rc.y + 8.0, w: 1.0, h: rc.h - 16.0 }, 0xFFFF_FF26)?;
    let right = RectF { x: mid + 10.0, y: rc.y, w: rc.x + rc.w - 10.0 - (mid + 10.0), h: rc.h };
    let two_lines = !t.sub.is_empty();
    let time_y = if two_lines { rc.y + (rc.h - 32.0) / 2.0 } else { rc.y + (rc.h - 18.0) / 2.0 };
    r.draw_text_styled(&t.value, RectF { y: time_y, h: 18.0, ..right }, 14.0, TEXT, Align::Left, TextStyle::Bold)?;
    if two_lines {
        r.draw_text(&t.sub, RectF { y: time_y + 18.0, h: 13.0, ..right }, 9.5, MUTED, Align::Left)?;
    }
    Ok(())
}
