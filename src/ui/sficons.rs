//! macOS menu-bar style status icons (SF Symbols look) drawn as Direct2D geometry:
//! `wifi` (dot + 3 concentric arcs, upward fan) and `speaker.wave` / `speaker.slash`.
//! Strokes are anti-aliased with round caps; sizes are in DIPs so they stay crisp at any DPI.
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D_POINT_2F,
    D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1GeometrySink, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_SWEEP_DIRECTION_CLOCKWISE,
};

use super::dock::RectF;
use super::renderer::Renderer;

/// Wi-Fi state for the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wifi {
    /// Radio off: dimmed fan with a slash.
    Off,
    /// Radio on (or no radio info) but not connected: every arc dimmed.
    Disconnected,
    /// Connected with 1..=3 bars lit (unused arcs dimmed).
    Bars(u8),
}

impl Wifi {
    /// `enabled`: radio state (None = unknown); `signal`: WLAN signal quality 0..=100 when connected.
    pub fn from_state(enabled: Option<bool>, signal: Option<u8>) -> Wifi {
        match (enabled, signal) {
            (Some(false), _) => Wifi::Off,
            (_, Some(q)) => Wifi::Bars(bars_for(q)),
            _ => Wifi::Disconnected,
        }
    }
}

/// Signal quality (0..=100) to lit arcs: <34 one, <67 two, else three (the dot is always lit).
pub fn bars_for(quality: u8) -> u8 {
    match quality {
        0..=33 => 1,
        34..=66 => 2,
        _ => 3,
    }
}

/// Volume to speaker waves: 0 when silent, then 1/2/3 by thirds.
pub fn waves_for(volume: f32) -> u8 {
    let v = volume.clamp(0.0, 1.0);
    if v <= 0.005 {
        0
    } else if v < 0.34 {
        1
    } else if v < 0.67 {
        2
    } else {
        3
    }
}

const DIM: f32 = 0.35;
const STROKE: f32 = 1.6;

fn fade(rgba: u32, k: f32) -> u32 {
    let a = ((rgba & 0xFF) as f32 * k.clamp(0.0, 1.0)).round() as u32;
    (rgba & !0xFF) | a
}

/// Maps a design box of `bw` x `bh` units centred into `rect`.
struct Map {
    ox: f32,
    oy: f32,
    s: f32,
}

impl Map {
    fn new(rect: RectF, bw: f32, bh: f32) -> Map {
        let s = (rect.w / bw).min(rect.h / bh);
        Map { ox: rect.x + (rect.w - bw * s) / 2.0, oy: rect.y + (rect.h - bh * s) / 2.0, s }
    }
    fn p(&self, x: f32, y: f32) -> D2D_POINT_2F {
        D2D_POINT_2F { x: self.ox + x * self.s, y: self.oy + y * self.s }
    }
}

/// Arc around (cx, cy) of radius `rad`, from `start` degrees (clockwise from 3 o'clock) by `sweep`.
fn arc(sink: &ID2D1GeometrySink, m: &Map, cx: f32, cy: f32, rad: f32, start: f32, sweep: f32) {
    let at = |deg: f32| {
        let a = deg.to_radians();
        m.p(cx + rad * a.cos(), cy + rad * a.sin())
    };
    // SAFETY: the caller's sink is open for the duration of the build closure.
    unsafe {
        sink.BeginFigure(at(start), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: at(start + sweep),
            size: D2D_SIZE_F { width: rad * m.s, height: rad * m.s },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
}

fn line(sink: &ID2D1GeometrySink, m: &Map, a: (f32, f32), b: (f32, f32)) {
    // SAFETY: the caller's sink is open for the duration of the build closure.
    unsafe {
        sink.BeginFigure(m.p(a.0, a.1), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddLine(m.p(b.0, b.1));
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
}

/// SF `chevron.up` / `chevron.down` (regular weight) in a 12x12 design box: a shallow
/// 10x5 caret with round caps/join, optically centred.
pub fn draw_chevron(r: &mut Renderer, rect: RectF, up: bool, rgba: u32) -> Result<()> {
    let m = Map::new(rect, 12.0, 12.0);
    let (tip, base) = if up { (3.6, 8.6) } else { (8.4, 3.4) };
    r.draw_geometry_round(
        |s| {
            // SAFETY: the sink is open for the duration of this closure.
            unsafe {
                s.BeginFigure(m.p(1.0, base), D2D1_FIGURE_BEGIN_HOLLOW);
                s.AddLine(m.p(6.0, tip));
                s.AddLine(m.p(11.0, base));
                s.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            Ok(())
        },
        rgba,
        Some(1.5 * m.s),
    )
}

/// SF `wifi`: a rounded wedge at the bottom plus two concentric 90° arcs opening upward, in a
/// 16x16 design box (the wedge counts as the first bar, as on macOS).
pub fn draw_wifi(r: &mut Renderer, rect: RectF, state: Wifi, rgba: u32) -> Result<()> {
    let m = Map::new(rect, 16.0, 16.0);
    let (cx, cy) = (8.0, 13.4);
    let lit = match state {
        Wifi::Bars(n) => n.min(3),
        _ => 0,
    };
    let width = 1.9 * m.s;
    for (i, rad) in [7.2f32, 10.8].into_iter().enumerate() {
        let k = if (i as u8 + 1) < lit { 1.0 } else { DIM };
        r.draw_geometry_round(|s| Ok(arc(s, &m, cx, cy, rad, 225.0, 90.0)), fade(rgba, k), Some(width))?;
    }
    // Wedge: apex at the centre, flaring up to a curved top edge.
    let wedge_k = if matches!(state, Wifi::Bars(_)) { 1.0 } else { DIM };
    let pt = |deg: f32, rad: f32| {
        let a = deg.to_radians();
        (cx + rad * a.cos(), cy + rad * a.sin())
    };
    let wedge = |s: &ID2D1GeometrySink| {
        let pts = [(cx, cy + 0.3), pt(225.0, 4.4), pt(247.5, 4.8), pt(270.0, 5.0), pt(292.5, 4.8), pt(315.0, 4.4)];
        // SAFETY: the sink is open for this closure.
        unsafe {
            s.BeginFigure(m.p(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_FILLED);
            for &(x, y) in &pts[1..] {
                s.AddLine(m.p(x, y));
            }
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        Ok(())
    };
    r.draw_geometry_round(wedge, fade(rgba, wedge_k), None)?;
    r.draw_geometry_round(wedge, fade(rgba, wedge_k), Some(1.0 * m.s))?;
    if state == Wifi::Off {
        r.draw_geometry_round(|s| Ok(line(s, &m, (2.6, 2.2), (13.8, 15.0))), rgba, Some(width))?;
    }
    Ok(())
}

/// SF `switch.2` (the macOS Control Center menu-bar icon): two pill outlines, each with a knob,
/// in a 16x16 design box.
pub fn draw_control_center(r: &mut Renderer, rect: RectF, rgba: u32) -> Result<()> {
    let m = Map::new(rect, 16.0, 16.0);
    let w = 1.25 * m.s;
    for (cy, knob_x) in [(3.7f32, 11.2f32), (12.3, 4.8)] {
        let p = m.p(1.4, cy - 2.5);
        r.stroke_rounded_rect(RectF { x: p.x, y: p.y, w: 13.2 * m.s, h: 5.0 * m.s }, 2.5 * m.s, rgba, w)?;
        let c = m.p(knob_x, cy);
        let d = 1.5 * m.s;
        r.fill_rounded_rect(RectF { x: c.x - d, y: c.y - d, w: 2.0 * d, h: 2.0 * d }, d, rgba)?;
    }
    Ok(())
}

/// SF `speaker.wave.N` (0..=3 waves by `volume`) or `speaker.slash` when muted, 18x16 box.
pub fn draw_speaker(r: &mut Renderer, rect: RectF, volume: f32, muted: bool, rgba: u32) -> Result<()> {
    let m = Map::new(rect, 18.0, 16.0);
    // Rounded speaker cone: fill plus a round-joined stroke of the same colour softens corners.
    let cone = |s: &ID2D1GeometrySink| {
        let pts = [(1.4, 6.0), (4.2, 6.0), (7.8, 2.9), (7.8, 13.1), (4.2, 10.0), (1.4, 10.0)];
        // SAFETY: the sink is open for this closure.
        unsafe {
            s.BeginFigure(m.p(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_FILLED);
            for &(x, y) in &pts[1..] {
                s.AddLine(m.p(x, y));
            }
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        Ok(())
    };
    r.draw_geometry_round(cone, rgba, None)?;
    r.draw_geometry_round(cone, rgba, Some(1.1 * m.s))?;
    let width = STROKE * m.s;
    if muted {
        r.draw_geometry_round(|s| Ok(line(s, &m, (1.6, 1.8), (14.6, 14.6))), rgba, Some(width))?;
        return Ok(());
    }
    let n = waves_for(volume);
    for (i, rad) in [3.4f32, 6.0, 8.6].into_iter().enumerate() {
        if (i as u8) < n {
            r.draw_geometry_round(|s| Ok(arc(s, &m, 8.0, 8.0, rad, 320.0, 80.0)), rgba, Some(width))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_layout_status_icon_levels() {
        assert_eq!(Wifi::from_state(Some(false), Some(90)), Wifi::Off);
        assert_eq!(Wifi::from_state(Some(true), None), Wifi::Disconnected);
        assert_eq!(Wifi::from_state(Some(true), Some(20)), Wifi::Bars(1));
        assert_eq!(Wifi::from_state(Some(true), Some(50)), Wifi::Bars(2));
        assert_eq!(Wifi::from_state(None, Some(99)), Wifi::Bars(3));
        assert_eq!(waves_for(0.0), 0);
        assert_eq!(waves_for(0.2), 1);
        assert_eq!(waves_for(0.5), 2);
        assert_eq!(waves_for(1.0), 3);
    }
}
