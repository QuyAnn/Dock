//! Apps tile: one squircle icon per open window, magnification, running dot, attention/count badge.
//! Mirrors widgets/AppsTile.tsx and the .app/.ico/.dots/.badge rules in styles.css.
//! Window list and attention flags come from win::tasks; icons from win::icons.
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::Result;

use super::dock::{hit_test, RectF};
use super::renderer::{Align, Renderer, TextStyle};
use super::squircle::{rounded_rect_alpha, shadow_pad, soft_shadow, squircle_mask, ICON_N};
use crate::model::WindowEntry;
use crate::win::icons::{icon_for_window, IconBitmap};
use crate::win::tasks::{list_windows, Attention};

/// macOS-style icon size inside the 48-DIP bar (Variant A).
pub const ICON: f32 = 34.0;
/// Height of the centre cluster band (Start, separator zone, apps) = the glass shelf.
/// Centred in the 48-DIP bar: 3 DIP clear above and below, all four shelf corners visible.
pub const CLUSTER_H: f32 = 42.0;
/// Icon top inside the cluster band: equal 4-DIP gaps above and below the 34-DIP icon
/// (icon optically centred in the shelf); the running dot sits inside the bottom gap.
pub const ICON_TOP: f32 = (CLUSTER_H - ICON) / 2.0;
const SLOT: f32 = ICON;
const GAP: f32 = 5.0;
const PITCH: f32 = SLOT + GAP;
const DOT: f32 = 2.5;
/// Space between the icon's bottom edge and the running dot.
const DOT_GAP: f32 = 0.5;
const DOT_RGBA: u32 = 0xFFFF_FFD9; // small white running dot
/// Shadow under each icon (in composed-icon pixels: ICON_N px = ICON DIPs).
const SHADOW_SIGMA_PX: f32 = 2.6;
const SHADOW_OPACITY: f32 = 0.38;
const SHADOW_DY_PX: usize = 2;
const FALLBACK_TOP: u32 = 0x5B6C_FFFF; // .fallback-icon gradient
const FALLBACK_BOTTOM: u32 = 0x7C4D_FFFF;
const FALLBACK_RADIUS: f32 = 0.225; // ~ macOS squircle
const FALLBACK_FONT: f32 = 16.0;
const WHITE: u32 = 0xFFFF_FFFF;
const BADGE_RED: u32 = 0xFF3B_30FF; // #ff3b30
const BADGE_SHADOW: u32 = 0x0000_0066; // box-shadow 0 1px 2px rgba(0,0,0,.4)
const BADGE_H: f32 = 15.0;
const BADGE_FONT: f32 = 9.5;
const ATTN_PERIOD: f32 = 1.4; // animation: attn 1.4s ease-in-out infinite
const MAG_RANGE: f32 = 40.0; // magnify falloff distance, px
// Hover only lifts slightly (scales around the bottom centre, ~2 DIP taller at most) so the
// icon never leaves the 48-DIP bar and nothing else moves.
const MAG_MAX: f32 = 0.05;

thread_local! {
    /// Pointer x in dock-local DIPs (same space as the tile rects), None when not over the dock.
    static POINTER: Cell<Option<f32>> = const { Cell::new(None) };
}

/// Sets the pointer x for magnification. Call with None on mouse leave.
/// Magnification only takes effect on the next draw, so the caller must repaint.
pub fn set_pointer(x: Option<f32>) {
    POINTER.with(|p| p.set(x));
}

/// Icon ready to draw: squircle-composed RGBA plus its drop shadow.
struct AppIcon {
    w: u32,
    h: u32,
    rgba: std::sync::Arc<Vec<u8>>,
    /// Shadow bitmap, padded by `shadow_pad(w)` on each side.
    shadow: (u32, u32, Vec<u8>),
}

impl AppIcon {
    fn new(b: IconBitmap) -> Self {
        Self::from_composed(squircle_mask(b.w, b.h, &b.rgba))
    }

    /// From an already composed ICON_N x ICON_N squircle.
    fn from_composed(rgba: Vec<u8>) -> Self {
        let alpha: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
        let shadow = soft_shadow(ICON_N, ICON_N, &alpha, SHADOW_SIGMA_PX, SHADOW_OPACITY, SHADOW_DY_PX);
        Self { w: ICON_N, h: ICON_N, rgba: std::sync::Arc::new(rgba), shadow }
    }

    fn bitmap(&self) -> IconBitmap {
        IconBitmap { w: self.w, h: self.h, rgba: self.rgba.clone() }
    }
}

#[derive(Default)]
pub struct AppsState {
    pub windows: Vec<WindowEntry>,
    /// Composed icons keyed by exe path. Pruned to the exes that are still open.
    icons: HashMap<String, AppIcon>,
    /// Pinned apps (exe path, display name). Pins with no open window get a launcher slot
    /// (`hwnd == 0`) after the running windows.
    pub pins: Vec<(String, String)>,
}

impl AppsState {
    /// Re-reads open windows, sets their attention flags and loads any missing icons.
    pub fn refresh(&mut self, attention: &Attention) {
        // EnumWindows is z-order based, so activating an app otherwise moves its icon. Keep the
        // first-seen order from the TS implementation and append only genuinely new windows.
        let previous: HashMap<isize, usize> =
            self.windows.iter().enumerate().map(|(i, w)| (w.hwnd, i)).collect();
        let mut fresh: Vec<(usize, WindowEntry)> = list_windows().into_iter().enumerate().collect();
        fresh.sort_by_key(|(seen, w)| (previous.get(&w.hwnd).copied().unwrap_or(usize::MAX), *seen));
        self.windows = fresh.into_iter().map(|(_, w)| w).collect();
        for w in &mut self.windows {
            w.attention = attention.contains(w.hwnd);
        }
        let launchers = pinned_launchers(&self.pins, &self.windows);
        self.windows.extend(launchers);
        for w in &self.windows {
            let key = if w.aumid.is_empty() { &w.exe } else { &w.aumid };
            if w.exe.is_empty() || self.icons.contains_key(key) {
                continue;
            }
            if let Some(b) = icon_for_window(w.hwnd, &w.exe, &w.aumid, &w.package_path) {
                self.icons.insert(key.to_owned(), AppIcon::new(b));
            }
        }
        let live: HashSet<&str> = self
            .windows
            .iter()
            .map(|w| if w.aumid.is_empty() { w.exe.as_str() } else { w.aumid.as_str() })
            .collect();
        self.icons.retain(|k, _| live.contains(k.as_str()));
    }
}

/// Launcher entries for pins whose exe has no open window.
fn pinned_launchers(pins: &[(String, String)], open: &[WindowEntry]) -> Vec<WindowEntry> {
    pins.iter()
        .filter(|(exe, _)| !exe.is_empty() && !open.iter().any(|w| w.exe.eq_ignore_ascii_case(exe)))
        .map(|(exe, name)| WindowEntry {
            hwnd: 0,
            title: name.clone(),
            exe: exe.clone(),
            aumid: String::new(),
            package_path: String::new(),
            attention: false,
            minimized: false,
        })
        .collect()
}

/// Draws one slot per window that fits in `rect`, centered. Extra windows are clipped.
/// Reads the pointer set by `set_pointer` for magnification.
pub fn draw_apps(r: &mut Renderer, rect: RectF, state: &AppsState) -> Result<()> {
    let pointer = POINTER.with(Cell::get);
    let t = now_secs();
    for (w, s) in state.windows.iter().zip(slots(rect, state.windows.len())) {
        let cx = s.x + SLOT / 2.0;
        let k = magnify(cx, pointer);
        let size = ICON * k;
        // Scales around the bottom centre; the base position never moves.
        let base_bottom = s.y + ICON_TOP + ICON;
        let ico = RectF { x: cx - size / 2.0, y: base_bottom - size, w: size, h: size };
        let key = if w.aumid.is_empty() { &w.exe } else { &w.aumid };
        match state.icons.get(key) {
            Some(icon) => draw_icon(r, icon, ico)?,
            None => draw_fallback(r, ico, &initial(w))?,
        }
        if w.hwnd == 0 {
            continue; // pinned launcher: no badge, no running dot
        }
        if let Some(badge) = badge_of(w) {
            draw_badge(r, ico, badge, t)?;
        }
        // Small white running dot under the icon.
        let dot = RectF { x: cx - DOT / 2.0, y: base_bottom + DOT_GAP, w: DOT, h: DOT };
        r.fill_rounded_rect(dot, DOT / 2.0, DOT_RGBA)?;
    }
    Ok(())
}

/// Plate rect of the Start / Search button: same size and bottom line as an app icon, growing
/// like one when hovered.
fn button_plate(rc: RectF, hovered: bool) -> RectF {
    let size = ICON * if hovered { 1.0 + MAG_MAX } else { 1.0 };
    let cx = rc.x + rc.w / 2.0;
    let bottom = rc.y + ICON_TOP + ICON;
    RectF { x: cx - size / 2.0, y: bottom - size, w: size, h: size }
}

const BUTTON_RADIUS: f32 = ICON * ICON_GLASS_RADIUS;

/// The dock's Start button in Liquid Glass: a refracting glass plate with a blue tint and the
/// four-pane Windows logo in white.
pub fn draw_start_icon(r: &mut Renderer, rc: RectF, hovered: bool) -> Result<()> {
    use crate::ui::glass::{draw_glass, draw_highlights, Layer};
    let plate = button_plate(rc, hovered);
    draw_glass(r, plate, BUTTON_RADIUS, Layer::Plate, hovered)?;
    // Blue tint, deeper at the bottom, then the rims again so they stay crisp on top of it.
    r.fill_rounded_rect_gradient(plate, BUTTON_RADIUS, 0x4DA8_FFC4, 0x0A62_E6D2)?;
    draw_highlights(r, plate, BUTTON_RADIUS, if hovered { 1.35 } else { 1.0 })?;
    // Windows logo: four panes with a thin gap, centred.
    let logo = plate.w * 0.52;
    let gap = logo * 0.12;
    let pane = (logo - gap) / 2.0;
    let (x0, y0) = (plate.x + (plate.w - logo) / 2.0, plate.y + (plate.h - logo) / 2.0);
    for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let p = RectF { x: x0 + dx * (pane + gap), y: y0 + dy * (pane + gap), w: pane, h: pane };
        r.fill_rounded_rect_gradient(p, pane * 0.16, 0xFFFF_FFFF, 0xE6F2_FFE6)?;
    }
    Ok(())
}

/// Search button in Liquid Glass: a clear glass plate with a white magnifier.
pub fn draw_search_icon(r: &mut Renderer, rc: RectF, hovered: bool) -> Result<()> {
    use crate::ui::glass::{draw_glass, Layer};
    let plate = button_plate(rc, hovered);
    draw_glass(r, plate, BUTTON_RADIUS, Layer::Plate, hovered)?;
    // A faint cool dim keeps the white glyph readable on bright wallpapers.
    r.fill_rounded_rect(plate, BUTTON_RADIUS, 0x1018_2A38)?;
    let inset = plate.w * 0.2;
    let area = RectF { x: plate.x + inset, y: plate.y + inset, w: plate.w - 2.0 * inset, h: plate.h - 2.0 * inset };
    crate::ui::svgpath::draw_path(
        r,
        "M4.3 10.5a6.2 6.2 0 1 0 12.4 0a6.2 6.2 0 1 0 -12.4 0z M15.2 15.2L20.5 20.5",
        area,
        24.0,
        0xFFFF_FFF2,
        Some(2.4),
    )
}

/// Index into `state.windows` for the slot under (x, y).
///
/// Horizontal only: every x inside the apps tile belongs to the nearest-left slot (the 5-DIP gaps
/// are split between neighbours), and any height counts, so a click at the very bottom edge of the
/// screen (where the pointer tends to rest) or between two icons is never swallowed. Callers gate on
/// the dock's own bounds.
pub fn hit_app_index(x: f32, _y: f32, rect: RectF, state: &AppsState) -> Option<usize> {
    if x < rect.x || x >= rect.x + rect.w {
        return None;
    }
    let half_gap = GAP / 2.0;
    slots(rect, state.windows.len()).iter().position(|s| x >= s.x - half_gap && x < s.x + s.w + half_gap)
}

/// Slot rect for window index `i`, if it fits.
pub fn slot_rect(rect: RectF, state: &AppsState, i: usize) -> Option<RectF> {
    slots(rect, state.windows.len()).get(i).copied()
}

/// Display name for an app icon (exe stem, else window title).
pub fn app_name(w: &WindowEntry) -> String {
    Path::new(&w.exe)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => w.title.clone(),
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| w.title.clone())
}

/// All listed windows that share the same exe (case-insensitive) as `w`.
pub fn group_windows<'a>(state: &'a AppsState, w: &WindowEntry) -> Vec<&'a WindowEntry> {
    if w.exe.is_empty() {
        return state.windows.iter().filter(|o| o.hwnd == w.hwnd).collect();
    }
    state
        .windows
        .iter()
        .filter(|o| o.exe.eq_ignore_ascii_case(&w.exe))
        .collect()
}

/// Icon bitmap pixels for a window's cache key, if loaded.
pub fn icon_pixels(state: &AppsState, w: &WindowEntry) -> Option<IconBitmap> {
    let key = if w.aumid.is_empty() { &w.exe } else { &w.aumid };
    state.icons.get(key).map(AppIcon::bitmap)
}

/// Exact width of `n` app slots (no side padding), so the centre cluster is measured by what
/// is drawn and Start + icons sit symmetrically around the monitor centre.
pub fn cluster_width(n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * SLOT + (n - 1) as f32 * GAP
    }
}

/// Slot rects for the first `n` windows that fit in `rect`, horizontally centered.
fn slots(rect: RectF, n: usize) -> Vec<RectF> {
    let fit = ((rect.w + GAP) / PITCH).floor().max(0.0) as usize;
    let count = n.min(fit);
    let group_w = count as f32 * SLOT + count.saturating_sub(1) as f32 * GAP;
    let x0 = rect.x + (rect.w - group_w).max(0.0) / 2.0;
    (0..count)
        .map(|i| RectF { x: x0 + i as f32 * PITCH, y: rect.y, w: SLOT, h: rect.h })
        .collect()
}

/// Scale of the icon at horizontal centre `cx` for a pointer at `pointer`: 1 + 0.3 * (1 - d/64)^2.
fn magnify(cx: f32, pointer: Option<f32>) -> f32 {
    match pointer {
        None => 1.0,
        Some(x) => {
            let falloff = (1.0 - (cx - x).abs() / MAG_RANGE).max(0.0);
            1.0 + MAG_MAX * falloff * falloff
        }
    }
}

/// Full-bleed squircle icon with its soft shadow; no plate behind it.
/// Liquid Glass finish over a composed icon: a glossy top sheen, a faint inner bottom shade
/// and the specular rims, so every app icon reads as the same glass material as the plates.
fn glass_finish(r: &mut Renderer, ico: RectF) -> Result<()> {
    let radius = ico.w * ICON_GLASS_RADIUS;
    // Gloss: white fading out over the upper half.
    r.fill_rounded_rect_linear(
        ico,
        radius,
        (ico.x, ico.y),
        0xFFFF_FF3A,
        (ico.x, ico.y + ico.h * 0.55),
        0xFFFF_FF00,
    )?;
    crate::ui::glass::draw_highlights(r, ico, radius, 1.0)
}

fn draw_icon(r: &mut Renderer, icon: &AppIcon, ico: RectF) -> Result<()> {
    draw_shadow(r, &icon.shadow, icon.w, ico)?;
    r.draw_bitmap(icon.w, icon.h, &icon.rgba, ico)?;
    glass_finish(r, ico)
}

/// Corner radius of the glass finish, as a share of the icon size (matches the composed squircle).
const ICON_GLASS_RADIUS: f32 = 0.24;

/// Gradient squircle-ish tile with the uppercase initial, used until the icon is loaded.
fn draw_fallback(r: &mut Renderer, ico: RectF, label: &str) -> Result<()> {
    draw_shadow(r, fallback_shadow(), FALLBACK_SRC, ico)?;
    let k = ico.w / ICON;
    r.fill_rounded_rect_gradient(ico, FALLBACK_RADIUS * ico.w, FALLBACK_TOP, FALLBACK_BOTTOM)?;
    r.draw_text_styled(label, ico, FALLBACK_FONT * k, WHITE, Align::Center, TextStyle::Bold)?;
    glass_finish(r, ico)
}

/// Source size used for the fallback shadow bitmap.
const FALLBACK_SRC: u32 = 32;

/// Shadow of the fallback tile, computed once.
fn fallback_shadow() -> &'static (u32, u32, Vec<u8>) {
    static SHADOW: OnceLock<(u32, u32, Vec<u8>)> = OnceLock::new();
    SHADOW.get_or_init(|| {
        let alpha = rounded_rect_alpha(FALLBACK_SRC, FALLBACK_SRC, FALLBACK_RADIUS * FALLBACK_SRC as f32);
        soft_shadow(FALLBACK_SRC, FALLBACK_SRC, &alpha, 0.9, SHADOW_OPACITY, 1)
    })
}

/// Draws a padded shadow bitmap so that its unpadded box lands on `ico`.
fn draw_shadow(r: &mut Renderer, shadow: &(u32, u32, Vec<u8>), src_w: u32, ico: RectF) -> Result<()> {
    let k = ico.w / src_w as f32;
    let pad = shadow_pad(src_w) as f32 * k;
    let rect = RectF {
        x: ico.x - pad,
        y: ico.y - pad,
        w: shadow.0 as f32 * k,
        h: shadow.1 as f32 * k,
    };
    r.draw_bitmap(shadow.0, shadow.1, &shadow.2, rect)
}

/// What the badge shows: a count from the window title, or a pulsing dot for attention.
enum Badge {
    Count(String),
    Dot,
}

fn badge_of(w: &WindowEntry) -> Option<Badge> {
    match title_count(&w.title) {
        Some(n) if n > 0 => Some(Badge::Count(if n > 99 { "99+".to_owned() } else { n.to_string() })),
        _ if w.attention => Some(Badge::Dot),
        _ => None,
    }
}

/// Badge geometry is relative to .ico (top -3 px, right -5 px for counts; top -1, right -2 for dots).
/// It scales with the icon, so every offset and size is multiplied by `k`.
fn draw_badge(r: &mut Renderer, ico: RectF, badge: Badge, t: f32) -> Result<()> {
    let k = ico.w / ICON;
    let right = ico.x + ico.w;
    match badge {
        Badge::Count(text) => {
            // min-width 15 px, 4 px side padding; digits are about 5.3 px wide at 9.5 px bold.
            let bw = (8.0 + 5.3 * text.chars().count() as f32).max(BADGE_H) * k;
            let bh = BADGE_H * k;
            let rect = RectF { x: right + 5.0 * k - bw, y: ico.y - 3.0 * k, w: bw, h: bh };
            r.fill_rounded_rect(RectF { y: rect.y + k, ..rect }, bh / 2.0, BADGE_SHADOW)?;
            r.fill_rounded_rect(rect, bh / 2.0, BADGE_RED)?;
            r.draw_text_styled(&text, rect, BADGE_FONT * k, WHITE, Align::Center, TextStyle::Bold)
        }
        Badge::Dot => {
            // .badge.dot animation: attn 1.4s ease-in-out infinite (scale 1 -> .6 -> 1, opacity 1 -> .5 -> 1).
            let (scale, opacity) = attn(t);
            let base = 9.0 * k;
            let cx = right + 2.0 * k - base / 2.0;
            let cy = ico.y - 1.0 * k + base / 2.0;
            let d = base * scale;
            let rect = RectF { x: cx - d / 2.0, y: cy - d / 2.0, w: d, h: d };
            let shadow = RectF { y: rect.y + k, ..rect };
            r.fill_rounded_rect(shadow, d / 2.0, with_alpha(BADGE_SHADOW, opacity))?;
            r.fill_rounded_rect(rect, d / 2.0, with_alpha(BADGE_RED, opacity))
        }
    }
}

/// Count shown in the badge: a number in the title like "(3) Inbox" or "[12] Chat". None if absent.
fn title_count(title: &str) -> Option<u32> {
    let t = title.trim_start();
    if let Some(rest) = t.strip_prefix(['(', '[']) {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let after = &rest[digits.len()..];
        let after = after.strip_prefix('+').unwrap_or(after);
        if (1..=4).contains(&digits.len()) && (after.starts_with(')') || after.starts_with(']')) {
            return digits.parse().ok();
        }
    }
    // Suffix form: "Inbox (3)".
    let inner = title.trim_end().strip_suffix(')')?;
    let inner = inner.strip_suffix('+').unwrap_or(inner);
    let digits = &inner[inner.rfind('(')? + 1..];
    if (1..=4).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

/// Uppercase first letter of the window title (the label the TSX uses for the fallback).
fn initial(w: &WindowEntry) -> String {
    w.title
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "?".to_owned())
}

/// (scale, opacity) of the attention dot at time `t`. Keyframes: 0/100% = 1, 50% = .6 scale, .5 opacity.
fn attn(t: f32) -> (f32, f32) {
    let p = (t % ATTN_PERIOD) / ATTN_PERIOD;
    // Triangle 0..1..0 over one period, eased per half like ease-in-out.
    let tri = 1.0 - (2.0 * p - 1.0).abs();
    let e = 0.5 - 0.5 * (PI * tri).cos();
    (1.0 - 0.4 * e, 1.0 - 0.5 * e)
}

/// Replaces the alpha byte of a 0xRRGGBBAA colour with `a` * the original alpha.
fn with_alpha(rgba: u32, a: f32) -> u32 {
    let base = (rgba & 0xFF) as f32;
    (rgba & 0xFFFF_FF00) | ((base * a.clamp(0.0, 1.0)).round() as u32)
}

/// Seconds since the first call. Driven by wall-clock time, so the caller must redraw to animate.
fn now_secs() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::dock::{layout, Tile};

    fn entry(hwnd: isize, exe: &str) -> WindowEntry {
        WindowEntry {
            hwnd,
            title: String::new(),
            exe: exe.to_owned(),
            aumid: String::new(),
            package_path: String::new(),
            attention: false,
            minimized: false,
        }
    }

    fn t(id: &'static str, width: f32) -> Tile {
        Tile { id, width, height: 0.0 }
    }

    /// Lays out the home-PC dock (1080p) with `n` apps; returns (rects, start index, apps index).
    fn centred(n: usize, screen_w: f32) -> (Vec<RectF>, usize, usize) {
        let left = vec![t("git", 118.0), t("cursor", 112.0), t("media", 200.0)];
        let center = vec![
            Tile { id: "start", width: ICON, height: CLUSTER_H },
            Tile { id: "sep", width: 1.0, height: 24.0 },
            Tile { id: "apps", width: cluster_width(n), height: CLUSTER_H },
        ];
        let right = vec![t("cpu", 42.0), t("ram", 42.0), t("clock", 64.0)];
        (layout(&left, &center, &right, screen_w), left.len(), left.len() + 2)
    }

    fn mid(r: RectF) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn dock_layout_shelf_icons_and_dots_vertically_centred_with_margin() {
        use crate::ui::dock::{shelf_for, DOCK_H};
        let (rects, start, apps) = centred(5, 1920.0);
        let shelf = shelf_for(rects[start], rects[apps]);
        // Clear margin above the shelf rim and below it; shelf centred in the bar.
        assert!(shelf.y >= 3.0, "shelf top {}", shelf.y);
        // Rounded bottom corners stay inside the visible bar (the window extends below the screen).
        assert!(shelf.y + shelf.h <= DOCK_H - 3.0, "shelf bottom {shelf:?}");
        assert!(((DOCK_H - shelf.y - shelf.h) - shelf.y - 2.0 * crate::ui::dock::CENTER_RISE).abs() < 0.01, "shelf not raised: {shelf:?}");
        let icon_top = rects[apps].y + ICON_TOP;
        let icon_bottom = icon_top + ICON;
        // A hovered icon grows upward by MAG_MAX but stays inside the bar.
        assert!(icon_bottom - ICON * (1.0 + MAG_MAX) >= 0.0);
        assert!(icon_top >= shelf.y && icon_bottom <= shelf.y + shelf.h);
        // Icon optically centred in the shelf: equal gaps to the top and bottom rims.
        let (gap_top, gap_bottom) = (icon_top - shelf.y, shelf.y + shelf.h - icon_bottom);
        assert!((gap_top - gap_bottom).abs() < 0.01, "gaps {gap_top} vs {gap_bottom}");
        let dot_bottom = icon_bottom + DOT_GAP + DOT;
        // At least 1 DIP between the dot and the shelf's bottom rim.
        assert!(dot_bottom <= shelf.y + shelf.h - 1.0, "dot {dot_bottom} touches shelf rim {shelf:?}");
        // Side tiles sit at the same raised height as the cluster.
        for (i, r) in rects.iter().enumerate() {
            if i >= start && i <= apps {
                continue;
            }
            assert!(((DOCK_H - r.y - r.h) - r.y - 2.0 * crate::ui::dock::CENTER_RISE).abs() < 0.01, "tile {i} not raised like the cluster: {r:?}");
        }
    }

    #[test]
    fn dock_layout_hit_start_button_on_centred_layout() {
        for &(n, w) in &[(1usize, 1920.0f32), (5, 1920.0), (9, 2560.0)] {
            let (rects, start, _) = centred(n, w);
            let (x, y) = mid(rects[start]);
            assert_eq!(hit_test(x, y, &rects), Some(start), "n={n} w={w}");
            // The cluster straddles the screen centre, so Start sits left of it.
            assert!(rects[start].x < w / 2.0);
            // Variant A: Start is a 38-DIP icon, the cluster (Start .. last app) is centred.
            assert_eq!(rects[start].w, ICON);
            let last = rects[start + 2];
            let mid = (rects[start].x + last.x + last.w) / 2.0;
            assert!((mid - w / 2.0).abs() < 0.01, "n={n} mid={mid}");
        }
    }

    #[test]
    fn dock_layout_hit_every_app_slot_on_centred_layout() {
        for &(n, w) in &[(1usize, 1920.0f32), (5, 1920.0), (12, 1920.0), (9, 2560.0)] {
            let (rects, _, apps) = centred(n, w);
            let mut state = AppsState::default();
            state.windows = (0..n).map(|i| entry(100 + i as isize, "x.exe")).collect();
            let rect = rects[apps];
            for i in 0..n {
                let slot = slot_rect(rect, &state, i).expect("slot fits");
                let (x, y) = mid(slot);
                assert_eq!(hit_test(x, y, &rects), Some(apps), "tile n={n} i={i}");
                assert_eq!(hit_app_index(x, y, rect, &state), Some(i), "slot n={n} i={i}");
                assert_eq!(state.windows[hit_app_index(x, y, rect, &state).unwrap()].hwnd, 100 + i as isize);
            }
            // The 5-DIP gaps between icons are split between the neighbours: no dead zone.
            if n > 1 {
                let s0 = slot_rect(rect, &state, 0).unwrap();
                assert_eq!(s0.w, ICON);
                let mid_y = s0.y + s0.h / 2.0;
                assert_eq!(hit_app_index(s0.x + s0.w + GAP / 2.0 - 0.1, mid_y, rect, &state), Some(0));
                assert_eq!(hit_app_index(s0.x + s0.w + GAP / 2.0, mid_y, rect, &state), Some(1));
            }
            // Any height counts: the bottom screen edge must not be a dead strip.
            let s0 = slot_rect(rect, &state, 0).unwrap();
            assert_eq!(hit_app_index(s0.x + s0.w / 2.0, rect.y + rect.h + 6.0, rect, &state), Some(0));
            // Outside the cluster nothing is hit.
            let y = rect.y + rect.h / 2.0;
            assert_eq!(hit_app_index(rect.x - 1.0, y, rect, &state), None);
            assert_eq!(hit_app_index(rect.x + rect.w + 1.0, y, rect, &state), None);
        }
    }

    #[test]
    fn dock_layout_pinned_not_running_gets_launcher_slot() {
        let pins = vec![
            ("C:\\Apps\\Code.exe".to_owned(), "Code".to_owned()),
            ("C:\\Apps\\Notes.exe".to_owned(), "Notes".to_owned()),
        ];
        let open = vec![entry(7, "c:\\apps\\code.exe")];
        let launchers = pinned_launchers(&pins, &open);
        assert_eq!(launchers.len(), 1, "running pin is not duplicated");
        assert_eq!(launchers[0].hwnd, 0);
        assert_eq!(launchers[0].exe, "C:\\Apps\\Notes.exe");
    }
}

#[cfg(test)]
mod icon_tests {
    use crate::ui::squircle::{choose_backing, squircle_mask, Backing, ICON_N};

    fn solid(n: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity(n * n * 4);
        for y in 0..n {
            for x in 0..n {
                v.extend_from_slice(&f(x, y));
            }
        }
        v
    }

    #[test]
    fn dock_layout_icon_backing_choice() {
        let n = 64;
        // Full square art -> full-bleed.
        let full = solid(n, |_, _| [200, 40, 40, 255]);
        assert_eq!(choose_backing(n, &full), Backing::FullBleed);
        // A circle (Chrome-like, colourful) -> light backing.
        let circle = solid(n, |x, y| {
            let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
            if dx * dx + dy * dy < 31.0 * 31.0 { [220, 60, 50, 255] } else { [0, 0, 0, 0] }
        });
        assert_eq!(choose_backing(n, &circle), Backing::Light);
        // Blue monochrome wide art (Task Manager-like) -> dark backing.
        let blue = solid(n, |_, y| if (8..56).contains(&y) { [40, 140, 230, 255] } else { [0, 0, 0, 0] });
        assert_eq!(choose_backing(n, &blue), Backing::Dark);
        // Composed output is always ICON_N square with transparent corners.
        let out = squircle_mask(n as u32, n as u32, &circle);
        assert_eq!(out.len(), (ICON_N * ICON_N * 4) as usize);
        assert_eq!(out[3], 0, "corner outside the squircle is transparent");
        let c = ((ICON_N / 2 * ICON_N + ICON_N / 2) * 4) as usize;
        assert_eq!(out[c + 3], 255, "centre is opaque (backing)");
    }
}

/// Recycle Bin icon: a neutral glass plate, a faint round well and an outlined bin; when the
/// bin holds items the two slats inside show.
pub fn draw_trash_icon(r: &mut Renderer, rc: RectF, hovered: bool, full: bool) -> Result<()> {
    use crate::ui::glass::{draw_glass, Layer};
    let plate = button_plate(rc, hovered);
    draw_glass(r, plate, BUTTON_RADIUS, Layer::Plate, hovered)?;
    r.fill_rounded_rect(plate, BUTTON_RADIUS, 0x1018_2A38)?;
    // Round well behind the glyph.
    let well_d = plate.w * 0.68;
    let well = RectF { x: plate.x + (plate.w - well_d) / 2.0, y: plate.y + (plate.h - well_d) / 2.0, w: well_d, h: well_d };
    r.fill_rounded_rect(well, well_d / 2.0, 0xFFFF_FF18)?;
    let inset = plate.w * 0.27;
    let area = RectF { x: plate.x + inset, y: plate.y + inset, w: plate.w - 2.0 * inset, h: plate.h - 2.0 * inset };
    let d = if full {
        "M4 7h16 M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2 M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12 M10 11v6 M14 11v6"
    } else {
        "M4 7h16 M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2 M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12"
    };
    crate::ui::svgpath::draw_path(r, d, area, 24.0, 0xFFFF_FFD6, Some(1.8))
}
