//! Control Center panel: radio/mute tiles and volume/brightness sliders.
//! Pure geometry + drawing; system calls live in `win::system`. Blocking reads go on a worker thread.
use windows::core::{Error, Result};
use windows::Win32::Foundation::E_NOTIMPL;

use super::dock::RectF;
use super::renderer::{Align, Renderer};
use crate::win::system;

/// Snapshot of system state shown by the panel.
#[derive(Clone, Debug, Default)]
pub struct ControlState {
    pub volume: f32,            // master volume, 0.0..=1.0
    pub muted: bool,
    pub brightness: Option<u32>, // percent, None when DDC/CI is unsupported
    pub wifi: Option<bool>,     // None when there is no Wi-Fi radio
    pub bluetooth: Option<bool>, // None when there is no Bluetooth radio
}

impl ControlState {
    /// Blocking: reads every value from the system. Call from a background thread.
    pub fn read() -> ControlState {
        let (volume, muted) = system::get_volume();
        ControlState {
            volume,
            muted,
            brightness: system::get_brightness(),
            wifi: system::wifi_enabled(),
            bluetooth: system::bluetooth_enabled(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlAction {
    ToggleMute,
    SetVolume(f32),
    SetBrightness(u32),
    ToggleWifi,
    ToggleBluetooth,
}

/// Runs the system call for `action`. Blocking. Toggles re-read the live radio/mute state
/// so a stale panel cannot flip the wrong way.
pub fn apply(action: ControlAction) -> Result<()> {
    match action {
        ControlAction::ToggleMute => {
            let (_, muted) = system::get_volume();
            system::set_mute(!muted)
        }
        ControlAction::SetVolume(v) => system::set_volume(v),
        ControlAction::SetBrightness(p) => system::set_brightness(p),
        ControlAction::ToggleWifi => {
            let on = system::wifi_enabled().ok_or_else(|| Error::from(E_NOTIMPL))?;
            system::set_wifi(!on)
        }
        ControlAction::ToggleBluetooth => {
            let on = system::bluetooth_enabled().ok_or_else(|| Error::from(E_NOTIMPL))?;
            system::set_bluetooth(!on)
        }
    }
}

// ---------- layout (DIPs, relative to the panel rect) ----------

pub const HEIGHT: f32 = 236.0; // total panel height for the popup window
const PAD: f32 = 10.0;
const GAP: f32 = 8.0;
const TILE_H: f32 = 52.0;
const TILE_R: f32 = 12.0;
const SLIDER_H: f32 = 44.0;
const SLIDER_PAD: f32 = 12.0;
const TRACK_H: f32 = 6.0;

// Colors 0xRRGGBBAA.
const TILE_ON: u32 = 0x0A84FFE6;
const CARD: u32 = 0xFFFFFF1F;
const TRACK: u32 = 0xFFFFFF33;
const FILL: u32 = 0xFFFFFFF2;
const TEXT: u32 = 0xFFFFFFFF;
const SUB: u32 = 0xFFFFFFB3;
const DIM: u32 = 0xFFFFFF66;

struct Layout {
    wifi: RectF,
    bluetooth: RectF,
    mute: RectF,
    volume: RectF,
    brightness: RectF,
}

fn layout(rect: RectF) -> Layout {
    let x = rect.x + PAD;
    let w = rect.w - 2.0 * PAD;
    let half = (w - GAP) / 2.0;
    let row1 = rect.y + PAD;
    let row2 = row1 + TILE_H + GAP;
    let row3 = row2 + TILE_H + GAP;
    let row4 = row3 + SLIDER_H + GAP;
    Layout {
        wifi: RectF { x, y: row1, w: half, h: TILE_H },
        bluetooth: RectF { x: x + half + GAP, y: row1, w: half, h: TILE_H },
        mute: RectF { x, y: row2, w, h: TILE_H },
        volume: RectF { x, y: row3, w, h: SLIDER_H },
        brightness: RectF { x, y: row4, w, h: SLIDER_H },
    }
}

/// Track rect inside a slider card.
fn track_of(block: RectF) -> RectF {
    RectF {
        x: block.x + SLIDER_PAD,
        y: block.y + block.h - 14.0,
        w: block.w - 2.0 * SLIDER_PAD,
        h: TRACK_H,
    }
}

fn contains(r: RectF, x: f32, y: f32) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

// ---------- drawing ----------

/// Two-line toggle tile: label on top, state below.
fn draw_tile(r: &mut Renderer, rect: RectF, label: &str, sub: &str, on: bool, enabled: bool) -> Result<()> {
    let bg = if on { TILE_ON } else { CARD };
    r.fill_rounded_rect(rect, TILE_R, bg)?;
    let text = if enabled { TEXT } else { DIM };
    let sub_col = if enabled { SUB } else { DIM };
    let half = rect.h / 2.0;
    let x = rect.x + SLIDER_PAD;
    let w = rect.w - 2.0 * SLIDER_PAD;
    r.draw_text(label, RectF { x, y: rect.y + 4.0, w, h: half - 4.0 }, 13.0, text, Align::Left)?;
    r.draw_text(sub, RectF { x, y: rect.y + half, w, h: half - 4.0 }, 11.0, sub_col, Align::Left)
}

/// Slider card: label + value on top, track below. `value` is None when unsupported.
fn draw_slider(r: &mut Renderer, block: RectF, label: &str, value_text: &str, frac: Option<f32>) -> Result<()> {
    r.fill_rounded_rect(block, TILE_R, CARD)?;
    let head = RectF { x: block.x + SLIDER_PAD, y: block.y + 4.0, w: block.w - 2.0 * SLIDER_PAD, h: 18.0 };
    r.draw_text(label, head, 12.0, TEXT, Align::Left)?;
    r.draw_text(value_text, head, 12.0, SUB, Align::Right)?;
    let track = track_of(block);
    match frac {
        Some(f) => {
            r.fill_rounded_rect(track, TRACK_H / 2.0, TRACK)?;
            let fw = (track.w * f.clamp(0.0, 1.0)).max(0.0);
            if fw > 0.0 {
                r.fill_rounded_rect(RectF { w: fw, ..track }, TRACK_H / 2.0, FILL)?;
            }
        }
        None => {
            let note = RectF { x: track.x, y: track.y - 6.0, w: track.w, h: 18.0 };
            r.draw_text("Not supported", note, 11.0, DIM, Align::Left)?;
        }
    }
    Ok(())
}

/// Draws the whole panel into `rect`.
pub fn draw(r: &mut Renderer, rect: RectF, state: &ControlState) -> Result<()> {
    let l = layout(rect);
    let on_off = |v: Option<bool>| match v {
        Some(true) => "On",
        Some(false) => "Off",
        None => "Not available",
    };
    draw_tile(r, l.wifi, "Wi-Fi", on_off(state.wifi), state.wifi == Some(true), state.wifi.is_some())?;
    draw_tile(r, l.bluetooth, "Bluetooth", on_off(state.bluetooth), state.bluetooth == Some(true), state.bluetooth.is_some())?;
    let mute_sub = if state.muted { "Muted" } else { "Sound on" };
    draw_tile(r, l.mute, "Mute", mute_sub, state.muted, true)?;

    let vol_pct = (state.volume.clamp(0.0, 1.0) * 100.0).round() as u32;
    let vol_text = if state.muted { "Muted".to_string() } else { format!("{vol_pct}%") };
    draw_slider(r, l.volume, "Volume", &vol_text, Some(state.volume))?;

    match state.brightness {
        Some(p) => draw_slider(r, l.brightness, "Brightness", &format!("{}%", p.min(100)), Some(p.min(100) as f32 / 100.0))?,
        None => draw_slider(r, l.brightness, "Brightness", "", None)?,
    }
    Ok(())
}

// ---------- hit testing ----------

/// Maps a click at (x, y) to an action. None when the click hits nothing actionable.
pub fn hit_test(x: f32, y: f32, rect: RectF, state: &ControlState) -> Option<ControlAction> {
    let l = layout(rect);
    if contains(l.wifi, x, y) {
        return state.wifi.map(|_| ControlAction::ToggleWifi);
    }
    if contains(l.bluetooth, x, y) {
        return state.bluetooth.map(|_| ControlAction::ToggleBluetooth);
    }
    if contains(l.mute, x, y) {
        return Some(ControlAction::ToggleMute);
    }
    if contains(l.volume, x, y) {
        let t = track_of(l.volume);
        return Some(ControlAction::SetVolume(((x - t.x) / t.w).clamp(0.0, 1.0)));
    }
    if contains(l.brightness, x, y) && state.brightness.is_some() {
        let t = track_of(l.brightness);
        let p = ((x - t.x) / t.w).clamp(0.0, 1.0) * 100.0;
        return Some(ControlAction::SetBrightness(p.round() as u32));
    }
    None
}
