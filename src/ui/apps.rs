//! Apps tile: one squircle icon per **app** (exe / AUMID), magnification, running dot, badge.
//! Multiple windows of the same app (and notification host windows) share one slot; hover peeks
//! and the context menu still list every window. Mirrors widgets/AppsTile.tsx grouping.
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::Result;

use super::dock::{hit_test, RectF};
use super::renderer::{Align, Renderer, TextStyle};
use super::squircle::{rounded_rect_alpha, shadow_pad, soft_shadow, squircle_mask, start_icon_rgba, ICON_N};
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
    rgba: Vec<u8>,
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
        Self { w: ICON_N, h: ICON_N, rgba, shadow }
    }

    fn bitmap(&self) -> IconBitmap {
        IconBitmap { w: self.w, h: self.h, rgba: self.rgba.clone() }
    }
}

#[derive(Default)]
pub struct AppsState {
    /// One entry per icon slot (representative window of each app group), display order.
    pub windows: Vec<WindowEntry>,
    /// Every open taskbar window (including those collapsed into a group). Used by peeks/menus.
    all: Vec<WindowEntry>,
    /// Composed icons keyed by app identity (AUMID or exe). Pruned to live apps.
    icons: HashMap<String, AppIcon>,
    /// Pinned apps (exe path, display name). Pins with no open window get a launcher slot
    /// (`hwnd == 0`) after the running windows.
    pub pins: Vec<(String, String)>,
}

impl AppsState {
    /// Re-reads open windows, groups by app identity, sets attention, loads missing icons.
    pub fn refresh(&mut self, attention: &Attention) {
        // Keep icon order stable: prefer the previous representative's slot for each app key.
        let previous_keys: HashMap<String, usize> = self
            .windows
            .iter()
            .enumerate()
            .map(|(i, w)| (app_identity(w), i))
            .collect();
        let mut fresh: Vec<WindowEntry> = list_windows();
        for w in &mut fresh {
            w.attention = attention.contains(w.hwnd);
        }
        // Stable within a refresh: first-seen z-order from EnumWindows, then prior group order.
        let mut keyed: Vec<(usize, String, WindowEntry)> = fresh
            .into_iter()
            .enumerate()
            .map(|(seen, w)| (seen, app_identity(&w), w))
            .collect();
        keyed.sort_by_key(|(seen, key, _)| (previous_keys.get(key).copied().unwrap_or(usize::MAX), *seen));

        let mut all = Vec::with_capacity(keyed.len());
        let mut groups: Vec<WindowEntry> = Vec::new();
        let mut group_index: HashMap<String, usize> = HashMap::new();
        for (_, key, w) in keyed {
            if let Some(&gi) = group_index.get(&key) {
                // Prefer a non-minimized / attention window as the representative.
                let rep = &mut groups[gi];
                if (w.attention && !rep.attention)
                    || (!w.minimized && rep.minimized)
                    || (rep.hwnd == 0 && w.hwnd != 0)
                {
                    // Keep the better title for badges when the new one carries a count.
                    let keep_title = title_count(&rep.title).unwrap_or(0) >= title_count(&w.title).unwrap_or(0);
                    let title = if keep_title { rep.title.clone() } else { w.title.clone() };
                    let attention = rep.attention || w.attention;
                    *rep = w.clone();
                    rep.title = title;
                    rep.attention = attention;
                } else {
                    rep.attention = rep.attention || w.attention;
                    if title_count(&w.title).unwrap_or(0) > title_count(&rep.title).unwrap_or(0) {
                        rep.title = w.title.clone();
                    }
                }
                all.push(w);
            } else {
                group_index.insert(key, groups.len());
                groups.push(w.clone());
                all.push(w);
            }
        }

        let launchers = pinned_launchers(&self.pins, &groups);
        groups.extend(launchers);
        self.all = all;
        self.windows = groups;

        for w in &self.windows {
            let key = app_identity(w);
            if (w.exe.is_empty() && w.aumid.is_empty()) || self.icons.contains_key(&key) {
                continue;
            }
            if let Some(b) = icon_for_window(w.hwnd, &w.exe, &w.aumid, &w.package_path) {
                self.icons.insert(key, AppIcon::new(b));
            }
        }
        let live: HashSet<String> = self.windows.iter().map(app_identity).collect();
        self.icons.retain(|k, _| live.contains(k));
    }
}

/// Stable app identity for grouping: AUMID when present, else lowercase exe, else hwnd.
fn app_identity(w: &WindowEntry) -> String {
    if !w.aumid.is_empty() {
        w.aumid.to_ascii_lowercase()
    } else if !w.exe.is_empty() {
        w.exe.to_ascii_lowercase()
    } else {
        format!("hwnd:{}", w.hwnd)
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
        match state.icons.get(&app_identity(w)) {
            Some(icon) => draw_icon(r, icon, ico)?,
            None => draw_fallback(r, ico, &initial(w))?,
        }
        if w.hwnd == 0 {
            continue; // pinned launcher: no badge, no running dot
        }
        let members = group_windows(state, w);
        if let Some(badge) = badge_of_group(&members) {
            draw_badge(r, ico, badge, t)?;
        }
        // Small white running dot under the icon.
        let dot = RectF { x: cx - DOT / 2.0, y: base_bottom + DOT_GAP, w: DOT, h: DOT };
        r.fill_rounded_rect(dot, DOT / 2.0, DOT_RGBA)?;
    }
    Ok(())
}

/// The dock's Start button: white Windows logo on a blue squircle, same size/shadow as the apps.
pub fn draw_start_icon(r: &mut Renderer, rc: RectF, hovered: bool) -> Result<()> {
    static START: OnceLock<AppIcon> = OnceLock::new();
    let icon = START.get_or_init(|| AppIcon::from_composed(start_icon_rgba()));
    let k = if hovered { 1.0 + MAG_MAX } else { 1.0 };
    let size = ICON * k;
    let cx = rc.x + rc.w / 2.0;
    let bottom = rc.y + ICON_TOP + ICON;
    draw_icon(r, icon, RectF { x: cx - size / 2.0, y: bottom - size, w: size, h: size })
}

/// Index into `state.windows` for the slot under (x, y).
pub fn hit_app_index(x: f32, y: f32, rect: RectF, state: &AppsState) -> Option<usize> {
    let slots = slots(rect, state.windows.len());
    hit_test(x, y, &slots)
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

/// All open windows that belong to the same app as `w` (AUMID / exe identity).
pub fn group_windows<'a>(state: &'a AppsState, w: &WindowEntry) -> Vec<&'a WindowEntry> {
    let key = app_identity(w);
    let pool = if state.all.is_empty() { &state.windows } else { &state.all };
    pool.iter().filter(|o| app_identity(o) == key).collect()
}

/// Icon bitmap pixels for a window's cache key, if loaded.
pub fn icon_pixels(state: &AppsState, w: &WindowEntry) -> Option<IconBitmap> {
    state.icons.get(&app_identity(w)).map(AppIcon::bitmap)
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
fn draw_icon(r: &mut Renderer, icon: &AppIcon, ico: RectF) -> Result<()> {
    draw_shadow(r, &icon.shadow, icon.w, ico)?;
    r.draw_bitmap(icon.w, icon.h, &icon.rgba, ico)
}

/// Gradient squircle-ish tile with the uppercase initial, used until the icon is loaded.
fn draw_fallback(r: &mut Renderer, ico: RectF, label: &str) -> Result<()> {
    draw_shadow(r, fallback_shadow(), FALLBACK_SRC, ico)?;
    let k = ico.w / ICON;
    r.fill_rounded_rect_gradient(ico, FALLBACK_RADIUS * ico.w, FALLBACK_TOP, FALLBACK_BOTTOM)?;
    r.draw_text_styled(label, ico, FALLBACK_FONT * k, WHITE, Align::Center, TextStyle::Bold)
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

fn badge_of_group(members: &[&WindowEntry]) -> Option<Badge> {
    let notif = members.iter().filter_map(|w| title_count(&w.title)).max().unwrap_or(0);
    let n = members.iter().filter(|w| w.hwnd != 0).count() as u32;
    // Prefer an unread count from the title; otherwise show how many windows are grouped.
    let count = notif.max(if n > 1 { n } else { 0 });
    if count > 0 {
        Some(Badge::Count(if count > 99 { "99+".to_owned() } else { count.to_string() }))
    } else if members.iter().any(|w| w.attention) {
        Some(Badge::Dot)
    } else {
        None
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
    use std::collections::HashMap;

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
        assert!((shelf.y - (DOCK_H - shelf.y - shelf.h)).abs() < 0.01, "shelf not centred: {shelf:?}");
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
        // Side tiles are vertically centred too.
        for (i, r) in rects.iter().enumerate() {
            if i == start + 1 {
                continue;
            }
            assert!((r.y - (DOCK_H - r.y - r.h)).abs() < 0.01, "tile {i} not centred: {r:?}");
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
            // Distinct exes → one slot each (same exe would collapse into a single group).
            state.windows = (0..n)
                .map(|i| entry(100 + i as isize, &format!("app{i}.exe")))
                .collect();
            let rect = rects[apps];
            for i in 0..n {
                let slot = slot_rect(rect, &state, i).expect("slot fits");
                let (x, y) = mid(slot);
                assert_eq!(hit_test(x, y, &rects), Some(apps), "tile n={n} i={i}");
                assert_eq!(hit_app_index(x, y, rect, &state), Some(i), "slot n={n} i={i}");
                assert_eq!(state.windows[hit_app_index(x, y, rect, &state).unwrap()].hwnd, 100 + i as isize);
            }
            // The 5-DIP gaps between icons are not part of any slot.
            if n > 1 {
                let s0 = slot_rect(rect, &state, 0).unwrap();
                assert_eq!(s0.w, ICON);
                assert_eq!(hit_app_index(s0.x + s0.w + GAP / 2.0, s0.y + s0.h / 2.0, rect, &state), None);
            }
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

    #[test]
    fn same_exe_windows_collapse_to_one_icon() {
        let mut state = AppsState::default();
        state.all = vec![
            entry(1, "C:\\Apps\\Zalo.exe"),
            {
                let mut e = entry(2, "c:\\apps\\zalo.exe");
                e.title = "(3) Chat".into();
                e
            },
            entry(3, "C:\\Apps\\idea64.exe"),
            entry(4, "C:\\Apps\\idea64.exe"),
        ];
        // Simulate refresh collapse: one representative per identity.
        let mut groups = Vec::new();
        let mut index = HashMap::new();
        for w in state.all.clone() {
            let key = app_identity(&w);
            if let Some(&i) = index.get(&key) {
                let rep = &mut groups[i];
                rep.attention |= w.attention;
                if title_count(&w.title).unwrap_or(0) > title_count(&rep.title).unwrap_or(0) {
                    rep.title = w.title.clone();
                }
            } else {
                index.insert(key, groups.len());
                groups.push(w);
            }
        }
        state.windows = groups;
        assert_eq!(state.windows.len(), 2, "Zalo + IDEA only");
        let zalo = state.windows.iter().find(|w| w.exe.to_ascii_lowercase().contains("zalo")).unwrap();
        let idea = state.windows.iter().find(|w| w.exe.to_ascii_lowercase().contains("idea")).unwrap();
        assert_eq!(group_windows(&state, zalo).len(), 2);
        assert_eq!(group_windows(&state, idea).len(), 2);
        let badge = badge_of_group(&group_windows(&state, zalo));
        assert!(matches!(badge, Some(Badge::Count(ref s)) if s == "3"), "unread from title wins: {badge:?}");
        let badge = badge_of_group(&group_windows(&state, idea));
        assert!(matches!(badge, Some(Badge::Count(ref s)) if s == "2"), "window count: {badge:?}");
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
