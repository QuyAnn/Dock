//! Shared easing and exponential interpolation for dock UI animations (UI thread only).

use std::f32::consts::PI;
use std::time::{Duration, Instant};

/// Nominal animation frame duration (~60 FPS).
pub const FRAME_DT: f32 = 1.0 / 60.0;

pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < f32::EPSILON {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Moves `current` toward `target` with time constant `tau` (seconds).
pub fn approach(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    if dt <= 0.0 {
        return current;
    }
    if tau <= f32::EPSILON {
        return target;
    }
    let k = 1.0 - (-dt / tau).exp();
    current + (target - current) * k
}

pub fn nearly_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

/// Target icon scale at dock-axis position `cx` for a pointer at `pointer` (DIPs along the dock).
pub fn magnify_target(cx: f32, pointer: Option<f32>, range: f32, max_extra: f32) -> f32 {
    match pointer {
        None => 1.0,
        Some(x) => {
            let falloff = (1.0 - (cx - x).abs() / range).max(0.0);
            1.0 + max_extra * falloff * falloff
        }
    }
}

/// True while elapsed time is within `duration`.
pub fn running(elapsed: f32, duration: f32) -> bool {
    elapsed < duration
}

/// Duration of popup open/close and dock show/hide surface motion.
pub const SURFACE_SECS: f32 = 0.22;
/// Dock slides this many DIPs along the screen-edge axis while fading.
pub const DOCK_SLIDE_DIP: f32 = 14.0;

/// Opacity, content scale, and slide progress (0 = offset, 1 = rest) for a floating surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceFrame {
    pub opacity: f32,
    pub scale: f32,
    pub slide: f32,
}

impl SurfaceFrame {
    pub const REST: Self = Self { opacity: 1.0, scale: 1.0, slide: 1.0 };
}

pub fn surface_in(elapsed: f32) -> SurfaceFrame {
    let t = ease_out_cubic((elapsed / SURFACE_SECS).min(1.0));
    SurfaceFrame { opacity: t, scale: 0.94 + 0.06 * t, slide: t }
}

pub fn surface_out(elapsed: f32) -> SurfaceFrame {
    let t = ease_out_cubic((elapsed / SURFACE_SECS).min(1.0));
    SurfaceFrame { opacity: 1.0 - t, scale: 1.0 - 0.06 * t, slide: 1.0 - t }
}

pub fn surface_in_running(elapsed: f32) -> bool {
    running(elapsed, SURFACE_SECS)
}

pub fn surface_out_running(elapsed: f32) -> bool {
    running(elapsed, SURFACE_SECS)
}

/// Slide offset in DIPs from the dock's rest position (positive `slide` = at rest).
pub fn dock_slide_offset(edge: crate::win::edge::AbEdge, slide: f32) -> (f32, f32) {
    use crate::win::edge::AbEdge;
    let d = DOCK_SLIDE_DIP * (1.0 - slide.clamp(0.0, 1.0));
    match edge {
        AbEdge::Bottom => (0.0, d),
        AbEdge::Top => (0.0, -d),
        AbEdge::Left => (d, 0.0),
        AbEdge::Right => (-d, 0.0),
    }
}

/// Limited attention bounce (scale offset), zero after `duration`.
pub fn attn_bounce(elapsed: f32, duration: f32) -> f32 {
    if elapsed >= duration {
        return 0.0;
    }
    let t = elapsed / duration;
    let env = (1.0 - t).max(0.0);
    (2.0 * PI * 2.5 * t).sin() * env * 0.12
}

/// Monotonic frame clock for tests and offscreen renders.
#[derive(Default)]
pub struct FrameClock {
    last: Option<Instant>,
}

thread_local! {
    static HOVER_BLEND: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    static HOVER_TARGET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a dock tile is hovered (drives the shared hover lift animation).
pub fn set_hover_target(active: bool) {
    HOVER_TARGET.with(|c| c.set(active));
}

pub fn hover_lift() -> f32 {
    HOVER_BLEND.with(std::cell::Cell::get)
}

pub fn hover_animating() -> bool {
    let target = if HOVER_TARGET.with(std::cell::Cell::get) { 1.0 } else { 0.0 };
    !nearly_equal(HOVER_BLEND.with(std::cell::Cell::get), target)
}

pub fn tick_hover(dt: f32) -> bool {
    let target = if HOVER_TARGET.with(std::cell::Cell::get) { 1.0 } else { 0.0 };
    let next = approach(HOVER_BLEND.with(std::cell::Cell::get), target, dt, 0.07);
    HOVER_BLEND.with(|c| c.set(next));
    !nearly_equal(next, target)
}

impl FrameClock {
    pub fn tick(&mut self) -> f32 {
        let now = Instant::now();
        let dt = self
            .last
            .replace(now)
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(FRAME_DT)
            .clamp(0.0, 0.05);
        dt
    }

    pub fn advance(&mut self, dt: f32) {
        let base = self.last.unwrap_or_else(Instant::now);
        self.last = Some(base + Duration::from_secs_f32(dt.max(0.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anim_easing_smoothstep_endpoints() {
        assert!(nearly_equal(smoothstep(0.0, 1.0, 0.0), 0.0));
        assert!(nearly_equal(smoothstep(0.0, 1.0, 1.0), 1.0));
        assert!(smoothstep(0.0, 1.0, 0.5) > 0.4 && smoothstep(0.0, 1.0, 0.5) < 0.6);
    }

    #[test]
    fn anim_easing_out_cubic_finishes_at_one() {
        assert!(nearly_equal(ease_out_cubic(1.0), 1.0));
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn anim_approach_reaches_target_after_time() {
        let mut v = 0.0;
        let mut t = 0.0;
        while t < 2.0 {
            v = approach(v, 1.0, FRAME_DT, 0.08);
            t += FRAME_DT;
        }
        assert!(nearly_equal(v, 1.0), "v={v}");
    }

    #[test]
    fn anim_magnify_never_exceeds_max() {
        let max_extra = 0.05;
        for cx in 0..40 {
            let cx = cx as f32;
            for ptr in 0..40 {
                let s = magnify_target(cx, Some(ptr as f32), 40.0, max_extra);
                assert!(s <= 1.0 + max_extra + 1e-5, "cx={cx} ptr={ptr} s={s}");
                assert!(s >= 1.0);
            }
        }
    }

    #[test]
    fn anim_magnify_vertical_axis_same_formula() {
        // Vertical docks use the same falloff along the stack axis (y treated like x).
        let h = magnify_target(120.0, Some(100.0), 40.0, 0.05);
        let v = magnify_target(120.0, Some(100.0), 40.0, 0.05);
        assert!(nearly_equal(h, v));
    }

    #[test]
    fn anim_attention_bounce_stops() {
        assert!(nearly_equal(attn_bounce(2.0, 1.4), 0.0));
        assert!(attn_bounce(0.1, 1.4).abs() > 0.0);
    }

    #[test]
    fn anim_running_window() {
        assert!(running(0.1, 0.18));
        assert!(!running(0.2, 0.18));
    }

    #[test]
    fn anim_popup_open_progress_endpoints() {
        let start = surface_in(0.0);
        assert!(start.opacity < 0.05);
        assert!(start.scale < 0.95);
        let end = surface_in(SURFACE_SECS);
        assert!(nearly_equal(end.opacity, 1.0));
        assert!(nearly_equal(end.scale, 1.0));
        assert!(nearly_equal(end.slide, 1.0));
    }

    #[test]
    fn anim_popup_close_progress_fades_out() {
        let end = surface_out(SURFACE_SECS);
        assert!(nearly_equal(end.opacity, 0.0));
        assert!(nearly_equal(end.slide, 0.0));
    }

    #[test]
    fn anim_dock_slide_zero_at_rest() {
        use crate::win::edge::AbEdge;
        for edge in [AbEdge::Bottom, AbEdge::Top, AbEdge::Left, AbEdge::Right] {
            let (x, y) = dock_slide_offset(edge, 1.0);
            assert!(nearly_equal(x, 0.0) && nearly_equal(y, 0.0));
        }
    }

    #[test]
    fn anim_hover_timer_stops_at_rest() {
        set_hover_target(true);
        for _ in 0..90 {
            tick_hover(FRAME_DT);
        }
        set_hover_target(false);
        for _ in 0..120 {
            tick_hover(FRAME_DT);
        }
        assert!(!hover_animating());
        assert!(nearly_equal(hover_lift(), 0.0));
    }
}
