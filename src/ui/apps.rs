//! Apps tile: one icon per open window, running dot, pulsing attention badge.
//! Window list and attention flags come from win::tasks; icons from win::icons.
use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::Result;

use super::dock::{hit_test, Icon, RectF};
use super::renderer::{Align, Renderer};
use crate::model::WindowEntry;
use crate::win::icons::icon_for_exe;
use crate::win::tasks::{list_windows, Attention};

const ICON: f32 = 36.0;
const GAP: f32 = 6.0;
const PITCH: f32 = ICON + GAP;
/// Icon is lifted a little so the running dot fits below it.
const ICON_LIFT: f32 = 2.0;
const DOT: f32 = 4.0;
const FALLBACK_BG: u32 = 0x3A3F_4DFF;
const BADGE: f32 = 10.0;
const BADGE_RED: u32 = 0xFF4D_4D00;
const PULSE_PERIOD: f32 = 1.2;

#[derive(Default)]
pub struct AppsState {
    pub windows: Vec<WindowEntry>,
    /// Icons keyed by exe path. Pruned to the exes that are still open.
    icons: HashMap<String, Icon>,
}

impl AppsState {
    /// Re-reads open windows, sets their attention flags and loads any missing icons.
    pub fn refresh(&mut self, attention: &Attention) {
        self.windows = list_windows();
        for w in &mut self.windows {
            w.attention = attention.contains(w.hwnd);
        }
        for w in &self.windows {
            if w.exe.is_empty() || self.icons.contains_key(&w.exe) {
                continue;
            }
            if let Some(b) = icon_for_exe(&w.exe) {
                self.icons.insert(w.exe.clone(), Icon { w: b.w, h: b.h, rgba: b.rgba });
            }
        }
        let live: HashSet<&str> = self.windows.iter().map(|w| w.exe.as_str()).collect();
        self.icons.retain(|k, _| live.contains(k.as_str()));
    }
}

/// Draws one slot per window that fits in `rect`, centered. Extra windows are clipped.
pub fn draw_apps(r: &mut Renderer, rect: RectF, state: &AppsState) -> Result<()> {
    for (w, s) in state.windows.iter().zip(slots(rect, state.windows.len())) {
        let icon_rect = RectF {
            x: s.x,
            y: s.y + (s.h - ICON) / 2.0 - ICON_LIFT,
            w: ICON,
            h: ICON,
        };
        match state.icons.get(&w.exe) {
            Some(icon) => r.draw_bitmap(icon.w, icon.h, &icon.rgba, icon_rect)?,
            None => {
                r.fill_rounded_rect(icon_rect, 8.0, FALLBACK_BG)?;
                r.draw_text(&letter(w), icon_rect, 16.0, 0xFFFF_FFFF, Align::Center)?;
            }
        }
        // Running dot, below the icon.
        let dot = RectF {
            x: s.x + (ICON - DOT) / 2.0,
            y: s.y + s.h - DOT - 1.0,
            w: DOT,
            h: DOT,
        };
        r.fill_rounded_rect(dot, DOT / 2.0, 0xFFFF_FFE6)?;
        if w.attention {
            draw_badge(r, icon_rect)?;
        }
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
    let group_w = count as f32 * ICON + count.saturating_sub(1) as f32 * GAP;
    let x0 = rect.x + (rect.w - group_w).max(0.0) / 2.0;
    (0..count)
        .map(|i| RectF { x: x0 + i as f32 * PITCH, y: rect.y, w: ICON, h: rect.h })
        .collect()
}

/// Uppercase first letter of the exe name, or of the title if there is no exe.
fn letter(w: &WindowEntry) -> String {
    Path::new(&w.exe)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(w.title.as_str())
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned())
}

/// Red dot at the icon's top-right corner with a halo that grows and fades.
fn draw_badge(r: &mut Renderer, icon: RectF) -> Result<()> {
    let p = pulse();
    let dot = RectF { x: icon.x + ICON - BADGE + 2.0, y: icon.y - 2.0, w: BADGE, h: BADGE };
    let grow = 1.0 + 3.0 * p;
    let halo = RectF {
        x: dot.x - grow,
        y: dot.y - grow,
        w: dot.w + 2.0 * grow,
        h: dot.h + 2.0 * grow,
    };
    let alpha = ((1.0 - p) * 110.0) as u32;
    r.fill_rounded_rect(halo, halo.w / 2.0, BADGE_RED | alpha)?;
    r.fill_rounded_rect(dot, BADGE / 2.0, BADGE_RED | 0xFF)?;
    Ok(())
}

/// 0..1 oscillation for the badge. Driven by wall-clock time, so the caller must redraw to animate.
fn pulse() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    0.5 + 0.5 * (t * TAU / PULSE_PERIOD).sin()
}
