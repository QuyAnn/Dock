//! macOS menu-bar style status icons for the dock's Control Center button. Wi-Fi and the speaker
//! reuse the Control Center icon paths (`ccicons`) so the dock and the panel draw identical glyphs;
//! the chevron and `switch.2` are drawn here as Direct2D geometry. Sizes are in DIPs.
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D_POINT_2F};

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
    /// Wired connection only (no Wi-Fi link): an RJ45 plug.
    Ethernet,
}

impl Wifi {
    /// `enabled`: radio state (None = unknown); `signal`: WLAN signal quality 0..=100 when connected.
    /// A live Wi-Fi link wins; otherwise a cable shows the plug; otherwise radio state.
    pub fn from_net(enabled: Option<bool>, signal: Option<u8>, ethernet: bool) -> Wifi {
        match Wifi::from_state(enabled, signal) {
            Wifi::Off | Wifi::Disconnected if ethernet => Wifi::Ethernet,
            w => w,
        }
    }

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
/// Volume at or below this counts as silent: the slider there mutes (as on macOS), and the
/// speaker shows the muted glyph even when the endpoint itself is not muted.
pub const MUTE_AT: f32 = 0.005;

/// Whether the speaker should read as muted: muted endpoint, or volume at zero.
pub fn shows_muted(volume: f32, muted: bool) -> bool {
    muted || volume <= MUTE_AT
}

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

/// The dock's Wi-Fi status icon: the same filled fan as the Control Center (`ccicons`: a rounded
/// wedge plus two rounded bands, unlit bands at 30%), scaled so the fan spans the icon box. Off adds
/// the bold slash, a radio with no link dims the whole fan, and a wired-only link shows the
/// Control Center's network glyph.
pub fn draw_wifi(r: &mut Renderer, rect: RectF, state: Wifi, rgba: u32) -> Result<()> {
    use super::ccicons::{self, Icon};
    let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    if state == Wifi::Ethernet {
        // Chevrons span 18 of 24 grid units: fit them to the box width.
        return ccicons::draw(r, Icon::Network, cx, cy, rect.w * 24.0 / 18.0, rgba);
    }
    // The fan spans 16.2 grid units wide and ~13.5 tall, centred at y 11.35 (before the 0.6-unit
    // lift `ccicons` gives it for round badges): undo both so the fan sits centred in the box.
    let size = (rect.w * 24.0 / 16.2).min(rect.h * 24.0 / 13.5);
    let unit = size / 24.0;
    let cy = cy + (12.0 - 11.35 + 0.6) * unit;
    let (icon, k) = match state {
        Wifi::Off => (Icon::Wifi { bars: 3, off: true }, 1.0),
        Wifi::Disconnected => (Icon::Wifi { bars: 3, off: false }, 0.3),
        Wifi::Bars(n) => (Icon::Wifi { bars: n.clamp(1, 3), off: false }, 1.0),
        Wifi::Ethernet => unreachable!(),
    };
    ccicons::draw(r, icon, cx, cy, size, fade(rgba, k))
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

/// The dock's speaker: the Control Center speaker (`ccicons`: rounded cone, 0..=3 waves by
/// `volume`, a cross when muted) scaled to fill the box width.
pub fn draw_speaker(r: &mut Renderer, rect: RectF, volume: f32, muted: bool, rgba: u32) -> Result<()> {
    use super::ccicons::{self, Icon};
    // Cone plus three waves span ~19.2 of 24 grid units wide and 14.1 tall.
    let size = (rect.w * 24.0 / 19.2).min(rect.h * 24.0 / 14.1);
    let muted = shows_muted(volume, muted);
    let icon = Icon::Speaker { waves: if muted { 0 } else { waves_for(volume) }, muted };
    ccicons::draw(r, icon, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0, size, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_volume_reads_as_muted() {
        assert!(shows_muted(0.0, false));
        assert!(shows_muted(0.004, false));
        assert!(!shows_muted(0.01, false));
        assert!(shows_muted(0.5, true));
        assert!(!shows_muted(0.34, false));
    }

    #[test]
    fn dock_layout_status_icon_levels() {
        assert_eq!(Wifi::from_state(Some(false), Some(90)), Wifi::Off);
        assert_eq!(Wifi::from_state(Some(true), None), Wifi::Disconnected);
        assert_eq!(Wifi::from_state(Some(true), Some(20)), Wifi::Bars(1));
        assert_eq!(Wifi::from_state(Some(true), Some(50)), Wifi::Bars(2));
        assert_eq!(Wifi::from_state(None, Some(99)), Wifi::Bars(3));
        assert_eq!(Wifi::from_net(Some(true), None, true), Wifi::Ethernet);
        assert_eq!(Wifi::from_net(Some(false), None, true), Wifi::Ethernet);
        assert_eq!(Wifi::from_net(Some(true), Some(80), true), Wifi::Bars(3));
        assert_eq!(Wifi::from_net(Some(true), None, false), Wifi::Disconnected);
        assert_eq!(waves_for(0.0), 0);
        assert_eq!(waves_for(0.2), 1);
        assert_eq!(waves_for(0.5), 2);
        assert_eq!(waves_for(1.0), 3);
    }
}
