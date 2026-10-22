//! Control Center panel: Wi-Fi/Bluetooth toggles, mute and volume/brightness capsules, and the
//! Mạng / Âm thanh squares. Layout and colors follow the Electron ControlCenter.tsx and styles.css.
//! Pure geometry + drawing; system calls live in `win::system`. Blocking reads go on a worker thread.
use windows::core::{Error, Result};
use windows::Win32::Foundation::E_NOTIMPL;

use super::dock::RectF;
use super::renderer::{Align, Renderer, TextStyle};
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
// .cc: padding 9, gap 8. .cc-grid: 1.35fr / 1fr. Every card has a 1 px border, included in the offsets.

/// Total panel height: 9 + 80 + 8 + 65 + 8 + 65 + 9.
pub const HEIGHT: f32 = 244.0;
const PAD: f32 = 9.0;
const GAP: f32 = 8.0;
const ROW1_H: f32 = 80.0; // .cc-conn and the square column
const CAP_H: f32 = 65.0; // .cc-cap
const CARD_R: f32 = 14.0;
const ROUND: f32 = 28.0; // .cc-round
const CAP_INSET: f32 = 11.0; // 1 px border + 10 px padding
const SLIDER_H: f32 = 22.0; // .cc-slider
const THUMB: f32 = 22.0;

// Colors 0xRRGGBBAA, from the CSS alphas.
const CARD: u32 = 0xFFFF_FF17; // rgba(255,255,255,.09)
const CARD_BORDER: u32 = 0xFFFF_FF1A; // rgba(255,255,255,.10)
const ROUND_BG: u32 = 0xFFFF_FF24; // rgba(255,255,255,.14)
const WHITE: u32 = 0xFFFF_FFFF;
const ACCENT: u32 = 0x1D6F_FFFF; // #1d6fff
const TEXT: u32 = 0xFFFF_FFFF;
const SUB: u32 = 0xFFFF_FF99; // opacity .6
const NA: u32 = 0xFFFF_FF8C; // opacity .55
const SQ_TEXT: u32 = 0xFFFF_FFF2; // opacity .95
const TRACK: u32 = 0xFFFF_FF29; // rgba(255,255,255,.16)
const FILL: u32 = 0xFFFF_FFEB; // rgba(255,255,255,.92)
const GLYPH_DARK: u32 = 0x5555_55FF; // #555 slider icon

// Segoe MDL2 Assets codepoints.
const GLYPH_WIFI: &str = "\u{E701}";
const GLYPH_BT: &str = "\u{E702}";
const GLYPH_BRIGHTNESS: &str = "\u{E706}";
const GLYPH_VOLUME: &str = "\u{E767}";
const GLYPH_MUTE: &str = "\u{E74F}";
const GLYPH_SETTINGS: &str = "\u{E713}";

struct Layout {
    conn: RectF,
    wifi: RectF, // toggle row (round button + label)
    bluetooth: RectF,
    net: RectF,   // Mạng square
    sound: RectF, // Âm thanh square
    brightness: RectF,
    volume: RectF,
}

fn layout(rect: RectF) -> Layout {
    let x0 = rect.x + PAD;
    let w = rect.w - 2.0 * PAD;
    let cols = w - GAP;
    let left_w = cols * 1.35 / 2.35;
    let right_w = cols - left_w;
    let right_x = x0 + left_w + GAP;
    let row_y = rect.y + PAD;
    // .cc-conn padding 8 6 8 8 plus border: toggles start at +9 and +43.
    let toggle_w = left_w - 16.0;
    let toggle = |y: f32| RectF { x: x0 + 9.0, y, w: toggle_w, h: ROUND };
    let sq_w = (right_w - GAP) / 2.0;
    let cap1 = row_y + ROW1_H + GAP;
    let cap2 = cap1 + CAP_H + GAP;
    Layout {
        conn: RectF { x: x0, y: row_y, w: left_w, h: ROW1_H },
        wifi: toggle(row_y + 9.0),
        bluetooth: toggle(row_y + 9.0 + ROUND + 6.0),
        net: RectF { x: right_x, y: row_y, w: sq_w, h: ROW1_H },
        sound: RectF { x: right_x + sq_w + GAP, y: row_y, w: sq_w, h: ROW1_H },
        brightness: RectF { x: x0, y: cap1, w, h: CAP_H },
        volume: RectF { x: x0, y: cap2, w, h: CAP_H },
    }
}

/// Slider capsule inside a `.cc-cap` card (top offset: border + padding + head + margin).
fn slider_of(card: RectF) -> RectF {
    RectF { x: card.x + CAP_INSET, y: card.y + 32.0, w: card.w - 2.0 * CAP_INSET, h: SLIDER_H }
}

/// Head row (title left, value right) of a `.cc-cap` card.
fn head_of(card: RectF) -> RectF {
    RectF { x: card.x + CAP_INSET, y: card.y + 9.0, w: card.w - 2.0 * CAP_INSET, h: 16.0 }
}

/// Slider icon: 14 px box at left 5 px, top 4 px inside the capsule.
fn icon_of(s: RectF) -> RectF {
    RectF { x: s.x + 5.0, y: s.y + 4.0, w: 14.0, h: 14.0 }
}

/// Fraction 0..1 for pointer x on the capsule. The thumb center travels from s.x+11 to s.x+w-11.
fn value_at(s: RectF, x: f32) -> f32 {
    ((x - (s.x + THUMB / 2.0)) / (s.w - THUMB)).clamp(0.0, 1.0)
}

fn contains(r: RectF, x: f32, y: f32) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

// ---------- drawing ----------

/// Multiplies the alpha byte of 0xRRGGBBAA by `k` (CSS opacity).
fn fade(rgba: u32, k: f32) -> u32 {
    let a = ((rgba & 0xFF) as f32 * k).round() as u32;
    (rgba & !0xFF) | a
}

/// Circle button (`.cc-round`). `on` = white with blue glyph. `k` fades the circle, `icon_k` the glyph.
fn draw_round(r: &mut Renderer, rect: RectF, glyph: &str, size: f32, on: bool, k: f32, icon_k: f32) -> Result<()> {
    let (bg, fg) = if on { (WHITE, ACCENT) } else { (ROUND_BG, TEXT) };
    r.fill_rounded_rect(rect, rect.h / 2.0, fade(bg, k))?;
    r.draw_text_styled(glyph, rect, size, fade(fg, icon_k), Align::Center, TextStyle::Icon)
}

/// One `.cc-toggle` row: round button, bold label, sub text.
struct Toggle<'a> {
    glyph: &'a str,
    label: &'a str,
    sub: &'a str,
    on: bool,
    enabled: bool,
    /// Glyph drops to 35% opacity when off (Wi-Fi). Bluetooth stays full.
    dim_off: bool,
}

fn draw_toggle(r: &mut Renderer, row: RectF, t: Toggle) -> Result<()> {
    // .cc-toggle.disabled is opacity .6; the disabled .cc-round adds .45 on top.
    let k = if t.enabled { 1.0 } else { 0.6 };
    let rk = if t.enabled { k } else { k * 0.45 };
    let icon_k = if t.on || !t.dim_off { 1.0 } else { 0.35 };
    let round = RectF { x: row.x, y: row.y, w: ROUND, h: ROUND };
    draw_round(r, round, t.glyph, 16.0, t.on, rk, rk * icon_k)?;
    let tx = row.x + ROUND + 8.0;
    let tw = (row.x + row.w - tx).max(0.0);
    r.draw_text_styled(t.label, RectF { x: tx, y: row.y, w: tw, h: 14.0 }, 11.5, fade(TEXT, k), Align::Left, TextStyle::Bold)?;
    r.draw_text(t.sub, RectF { x: tx, y: row.y + 14.0, w: tw, h: 14.0 }, 10.5, fade(SUB, k), Align::Left)
}

/// Square shortcut (`.cc-sq`): round glyph centered above a 10 px label.
fn draw_square(r: &mut Renderer, rect: RectF, glyph: &str, label: &str) -> Result<()> {
    r.fill_rounded_rect(rect, CARD_R, CARD)?;
    r.stroke_rounded_rect(rect, CARD_R, CARD_BORDER, 1.0)?;
    let top = rect.y + (rect.h - 45.0) / 2.0; // 28 circle + 4 gap + 13 text
    let round = RectF { x: rect.x + (rect.w - ROUND) / 2.0, y: top, w: ROUND, h: ROUND };
    draw_round(r, round, glyph, 15.0, false, 1.0, 1.0)?;
    r.draw_text(label, RectF { x: rect.x, y: top + 32.0, w: rect.w, h: 13.0 }, 10.0, SQ_TEXT, Align::Center)
}

/// `.cc-cap` card background with its title and right-aligned value.
fn draw_cap(r: &mut Renderer, card: RectF, title: &str, value: &str) -> Result<()> {
    r.fill_rounded_rect(card, CARD_R, CARD)?;
    r.stroke_rounded_rect(card, CARD_R, CARD_BORDER, 1.0)?;
    let head = head_of(card);
    r.draw_text_styled(title, head, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(value, head, 10.5, NA, Align::Right)
}

/// Thumb-style capsule (`.cc-slider`): white fill to `frac`, white thumb circle, glyph at left.
fn draw_capsule(r: &mut Renderer, s: RectF, frac: f32, enabled: bool, glyph: &str) -> Result<()> {
    let k = if enabled { 1.0 } else { 0.4 };
    let x0 = s.x + THUMB / 2.0;
    let span = s.w - THUMB;
    let end = x0 + span * frac.clamp(0.0, 1.0);
    r.fill_rect(RectF { x: x0, y: s.y, w: span, h: s.h }, fade(TRACK, k))?;
    if end > x0 {
        r.fill_rect(RectF { x: x0, y: s.y, w: end - x0, h: s.h }, fade(FILL, k))?;
    }
    // Thumb: no drop shadow (the CSS box-shadow is not drawn).
    r.fill_rounded_rect(RectF { x: end - THUMB / 2.0, y: s.y, w: THUMB, h: THUMB }, THUMB / 2.0, fade(WHITE, k))?;
    r.draw_text_styled(glyph, icon_of(s), 14.0, fade(GLYPH_DARK, k), Align::Center, TextStyle::Icon)
}

fn radio_sub(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "Bật",
        Some(false) => "Tắt",
        None => "Không có",
    }
}

/// Draws the whole panel into `rect`.
pub fn draw(r: &mut Renderer, rect: RectF, state: &ControlState) -> Result<()> {
    let l = layout(rect);

    // Connectivity card.
    r.fill_rounded_rect(l.conn, CARD_R, CARD)?;
    r.stroke_rounded_rect(l.conn, CARD_R, CARD_BORDER, 1.0)?;
    draw_toggle(
        r,
        l.wifi,
        Toggle {
            glyph: GLYPH_WIFI,
            label: "Wi-Fi",
            sub: radio_sub(state.wifi),
            on: state.wifi == Some(true),
            enabled: state.wifi.is_some(),
            dim_off: true,
        },
    )?;
    draw_toggle(
        r,
        l.bluetooth,
        Toggle {
            glyph: GLYPH_BT,
            label: "Bluetooth",
            sub: radio_sub(state.bluetooth),
            on: state.bluetooth == Some(true),
            enabled: state.bluetooth.is_some(),
            dim_off: false,
        },
    )?;

    draw_square(r, l.net, GLYPH_SETTINGS, "Mạng")?;
    draw_square(r, l.sound, GLYPH_VOLUME, "Âm thanh")?;

    // Brightness: unsupported shows the note and a disabled, empty capsule.
    let (br_frac, br_text) = match state.brightness {
        Some(p) => (p.min(100) as f32 / 100.0, format!("{}%", p.min(100))),
        None => (0.0, "Không hỗ trợ".to_string()),
    };
    draw_cap(r, l.brightness, "Màn hình", &br_text)?;
    draw_capsule(r, slider_of(l.brightness), br_frac, state.brightness.is_some(), GLYPH_BRIGHTNESS)?;

    // Volume: the glyph at left toggles mute, as in the TSX speaker button.
    let vol = state.volume.clamp(0.0, 1.0);
    let vol_text = if state.muted { "Đã tắt tiếng".to_string() } else { format!("{}%", (vol * 100.0).round() as u32) };
    draw_cap(r, l.volume, "Âm thanh", &vol_text)?;
    let glyph = if state.muted { GLYPH_MUTE } else { GLYPH_VOLUME };
    draw_capsule(r, slider_of(l.volume), vol, true, glyph)
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
    let vol = slider_of(l.volume);
    if contains(icon_of(vol), x, y) {
        return Some(ControlAction::ToggleMute);
    }
    if contains(vol, x, y) {
        return Some(ControlAction::SetVolume(value_at(vol, x)));
    }
    let br = slider_of(l.brightness);
    if state.brightness.is_some() && contains(br, x, y) {
        let p = value_at(br, x) * 100.0;
        return Some(ControlAction::SetBrightness(p.round() as u32));
    }
    None
}
