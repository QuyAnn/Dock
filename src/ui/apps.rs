//! Apps tile: one squircle icon per open window, magnification, running dot, attention/count badge.
//! Mirrors widgets/AppsTile.tsx and the .app/.ico/.dots/.badge rules in styles.css.
//! Window list and attention flags come from win::tasks; icons from win::icons.
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::Result;

use super::dock::{hit_test, RectF};
use super::renderer::{Align, Renderer, TextStyle};
use super::squircle::{drop_shadow, rounded_rect_alpha, shadow_pad, squircle_mask};
use crate::model::WindowEntry;
use crate::win::icons::{icon_for_exe, IconBitmap};
use crate::win::tasks::{list_windows, Attention};

const ICON: f32 = 30.0; // .ico / .sq size
const ICO_MARGIN: f32 = 4.0; // .ico margin-bottom
const SLOT: f32 = 36.0; // .app width
const GAP: f32 = 2.0; // .apps gap
const PITCH: f32 = SLOT + GAP;
const DOT: f32 = 3.0; // .dots i
const DOT_RGBA: u32 = 0xFFFF_FFB3; // rgba(255,255,255,.7)
const FALLBACK_TOP: u32 = 0x5B6C_FFFF; // .fallback-icon gradient
const FALLBACK_BOTTOM: u32 = 0x7C4D_FFFF;
const FALLBACK_RADIUS: f32 = 0.27; // border-radius: 27%
const FALLBACK_FONT: f32 = 13.0;
const WHITE: u32 = 0xFFFF_FFFF;
const BADGE_RED: u32 = 0xFF3B_30FF; // #ff3b30
const BADGE_SHADOW: u32 = 0x0000_0066; // box-shadow 0 1px 2px rgba(0,0,0,.4)
const BADGE_H: f32 = 15.0;
const BADGE_FONT: f32 = 9.5;
const ATTN_PERIOD: f32 = 1.4; // animation: attn 1.4s ease-in-out infinite
const MAG_RANGE: f32 = 64.0; // magnify falloff distance, px
// A 46-DIP AppBar has room for ~35.5 DIP above the running dot. Cap magnification so the
// squircle never crosses the HWND's top edge and gets clipped.
const MAG_MAX: f32 = 0.18;

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
        let rgba = squircle_mask(b.w, b.h, &b.rgba);
        let alpha: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
        let shadow = drop_shadow(b.w, b.h, &alpha);
        Self { w: b.w, h: b.h, rgba, shadow }
    }
}

#[derive(Default)]
pub struct AppsState {
    pub windows: Vec<WindowEntry>,
    /// Composed icons keyed by exe path. Pruned to the exes that are still open.
    icons: HashMap<String, AppIcon>,
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
        for w in &self.windows {
            if w.exe.is_empty() || self.icons.contains_key(&w.exe) {
                continue;
            }
            if let Some(b) = icon_for_exe(&w.exe) {
                self.icons.insert(w.exe.clone(), AppIcon::new(b));
            }
        }
        let live: HashSet<&str> = self.windows.iter().map(|w| w.exe.as_str()).collect();
        self.icons.retain(|k, _| live.contains(k.as_str()));
    }
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
        // .ico is centred in the slot above its margin, and scales around its bottom centre.
        let base_top = s.y + (s.h - (ICON + ICO_MARGIN)) / 2.0;
        let ico = RectF {
            x: cx - size / 2.0,
            y: base_top + ICON - size,
            w: size,
            h: size,
        };
        match state.icons.get(&w.exe) {
            Some(icon) => draw_icon(r, icon, ico)?,
            None => draw_fallback(r, ico, &initial(w))?,
        }
        if let Some(badge) = badge_of(w) {
            draw_badge(r, ico, badge, t)?;
        }
        // Running dot: .dots sits 1 px above the slot bottom. One dot per window slot.
        let dot = RectF {
            x: s.x + (SLOT - DOT) / 2.0,
            y: s.y + s.h - 1.0 - DOT,
            w: DOT,
            h: DOT,
        };
        r.fill_rounded_rect(dot, DOT / 2.0, DOT_RGBA)?;
    }
    Ok(())
}

/// HWND of the window whose slot contains (x, y), if any.
pub fn hit_app(x: f32, y: f32, rect: RectF, state: &AppsState) -> Option<isize> {
    let slots = slots(rect, state.windows.len());
    hit_test(x, y, &slots).map(|i| state.windows[i].hwnd)
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

/// Icon colour and shadow, drawn at the (possibly magnified) box `ico`.
fn draw_icon(r: &mut Renderer, icon: &AppIcon, ico: RectF) -> Result<()> {
    draw_shadow(r, &icon.shadow, icon.w, ico)?;
    r.draw_bitmap(icon.w, icon.h, &icon.rgba, ico)
}

/// Gradient tile with the uppercase initial, used until the icon is loaded.
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
        drop_shadow(FALLBACK_SRC, FALLBACK_SRC, &alpha)
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
