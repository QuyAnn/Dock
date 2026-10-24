//! Control Center panel: Wi-Fi/Bluetooth toggles, mute and volume/brightness capsules, and the
//! Mạng / Âm thanh squares. Layout and colors follow the Electron ControlCenter.tsx and styles.css.
//! Pure geometry + drawing; system calls live in `win::system`. Blocking reads go on a worker thread.
use windows::core::{Error, Result};
use windows::Win32::Foundation::E_NOTIMPL;

use super::dock::RectF;
use super::renderer::{Align, Renderer, TextStyle};
use crate::model::Media;
use crate::win::system::{self, MediaAction};
use super::sficons;

/// Snapshot of system state shown by the panel.
#[derive(Clone, Debug, Default)]
pub struct ControlState {
    pub volume: f32,            // master volume, 0.0..=1.0
    pub muted: bool,
    pub brightness: Option<u32>, // percent, None when DDC/CI is unsupported
    pub wifi: Option<bool>,     // None when there is no Wi-Fi radio
    pub wifi_signal: Option<u8>, // WLAN signal quality 0..=100 while connected
    pub bluetooth: Option<bool>, // None when there is no Bluetooth radio
    pub dark: bool,
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
            wifi_signal: system::wifi_signal(),
            bluetooth: system::bluetooth_enabled(),
            dark: system::dark_mode(),
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
    ToggleDark,
    OpenNetworkSettings,
    OpenSoundSettings,
    /// Wi-Fi label: the network list (like macOS Wi-Fi module).
    OpenWifiList,
    /// Bluetooth label: Bluetooth devices page.
    OpenBluetoothSettings,
    MediaPrevious,
    MediaPlayPause,
    MediaNext,
    /// Gear in the Control Center: opens SmartDock's own Settings popup (UI thread).
    OpenSmartDockSettings,
    /// Bottom shortcut: opens a URI such as calculator: or ms-screenclip:.
    Launch(&'static str),
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
        ControlAction::ToggleDark => system::set_dark_mode(!system::dark_mode()),
        ControlAction::OpenNetworkSettings => system::open_settings("ms-settings:network-wifi"),
        ControlAction::OpenSoundSettings => system::open_settings("ms-settings:sound"),
        ControlAction::OpenWifiList => system::open_settings("ms-availablenetworks:"),
        ControlAction::OpenBluetoothSettings => system::open_settings("ms-settings:bluetooth"),
        ControlAction::MediaPrevious => system::media_control(MediaAction::Previous),
        ControlAction::MediaPlayPause => system::media_control(MediaAction::PlayPause),
        ControlAction::MediaNext => system::media_control(MediaAction::Next),
        // Handled on the UI thread by the caller (it swaps the popup to Settings).
        ControlAction::OpenSmartDockSettings => Ok(()),
        ControlAction::Launch(uri) => system::open_settings(uri),
    }
}

// ---------- layout (DIPs, relative to the panel rect) ----------
// .cc: padding 9, gap 8. .cc-grid: 1.35fr / 1fr. Every card has a 1 px border, included in the offsets.

// ---------- layout (DIPs, relative to the panel rect) ----------
// macOS 26 style: big rounded glass tiles, stadium pills, thick slider capsules.

/// Total panel height.
pub const HEIGHT: f32 = 454.0;
const PAD: f32 = 12.0;
const GAP: f32 = 10.0;
const TILE_H: f32 = 58.0;
const BOTTOM_H: f32 = 52.0;
const MEDIA_H: f32 = 2.0 * TILE_H + GAP;
const SLIDER_CARD_H: f32 = 58.0;
const TILE_R: f32 = 26.0;
const CARD_R: f32 = 20.0;
const ROUND: f32 = 38.0; // icon circle inside a tile
const CAP_INSET: f32 = 14.0;
const END_W: f32 = 22.0; // icon slots at both ends of a slider
const SLIDER_H: f32 = 16.0;
const THUMB: f32 = 16.0;
const TRACK_H: f32 = 6.0;

// Colors 0xRRGGBBAA.
const CARD: u32 = 0xFFFF_FF24;
const ROUND_BG: u32 = 0xFFFF_FF2E;
const WHITE: u32 = 0xFFFF_FFFF;
const ACCENT: u32 = 0x0A84_FFFF; // macOS system blue #0A84FF
const TEXT: u32 = 0xFFFF_FFFF;
const SUB: u32 = 0xFFFF_FFA6;
const TRACK: u32 = 0xFFFF_FF33;
const FILL: u32 = 0xFFFF_FFF2;
const GLYPH_DARK: u32 = 0x5555_55FF;

// Segoe MDL2 Assets codepoints.
const GLYPH_BT: &str = "\u{E702}";
const GLYPH_BRIGHTNESS: &str = "\u{E706}";
const GLYPH_NETWORK: &str = "\u{E968}";
const GLYPH_MOON: &str = "\u{E708}";
const GLYPH_GEAR: &str = "\u{E713}";
const GLYPH_CALC: &str = "\u{E8EF}";
const GLYPH_CAMERA: &str = "\u{E722}";
const GLYPH_CLOCK: &str = "\u{E916}";
const GLYPH_DISPLAY: &str = "\u{E7F4}";

/// Bottom shortcut circles: glyph and the URI they open.
const SHORTCUTS: [(&str, &str); 4] = [
    (GLYPH_CALC, "calculator:"),
    (GLYPH_CAMERA, "ms-screenclip:"),
    (GLYPH_CLOCK, "ms-clock:"),
    (GLYPH_GEAR, "ms-settings:"),
];

struct Layout {
    wifi: RectF,
    /// Bluetooth toggle (round button under the Wi-Fi tile).
    bluetooth: RectF,
    /// Network settings (round button next to Bluetooth).
    net: RectF,
    media: RectF,
    dark: RectF,
    sound: RectF,
    display: RectF,
    brightness: RectF,
    volume: RectF,
    shortcuts: [RectF; 4],
    settings: RectF,
}

fn layout(rect: RectF) -> Layout {
    let x0 = rect.x + PAD;
    let w = rect.w - 2.0 * PAD;
    let col = (w - GAP) / 2.0;
    let right_x = x0 + col + GAP;
    let small = (col - GAP) / 2.0;
    let y1 = rect.y + PAD;
    let y1b = y1 + TILE_H + GAP;
    let y2 = y1 + MEDIA_H + GAP;
    let y3 = y2 + TILE_H + GAP;
    let y4 = y3 + SLIDER_CARD_H + GAP;
    let y5 = y4 + SLIDER_CARD_H + GAP;
    let y6 = y5 + BOTTOM_H + GAP;
    let step = (w - BOTTOM_H) / 3.0;
    Layout {
        wifi: RectF { x: x0, y: y1, w: col, h: TILE_H },
        bluetooth: RectF { x: x0, y: y1b, w: small, h: TILE_H },
        net: RectF { x: x0 + small + GAP, y: y1b, w: small, h: TILE_H },
        media: RectF { x: right_x, y: y1, w: col, h: MEDIA_H },
        dark: RectF { x: x0, y: y2, w: col, h: TILE_H },
        sound: RectF { x: right_x, y: y2, w: small, h: TILE_H },
        display: RectF { x: right_x + small + GAP, y: y2, w: small, h: TILE_H },
        brightness: RectF { x: x0, y: y3, w, h: SLIDER_CARD_H },
        volume: RectF { x: x0, y: y4, w, h: SLIDER_CARD_H },
        shortcuts: [0.0, 1.0, 2.0, 3.0].map(|i| RectF { x: x0 + i * step, y: y5, w: BOTTOM_H, h: BOTTOM_H }),
        settings: RectF { x: rect.x + (rect.w - 150.0) / 2.0, y: y6, w: 150.0, h: 28.0 },
    }
}

/// Slider row inside a slider card (below the title): icon slot, thin track, icon slot.
fn slider_of(card: RectF) -> RectF {
    RectF { x: card.x + CAP_INSET, y: card.y + 33.0, w: card.w - 2.0 * CAP_INSET, h: SLIDER_H }
}

/// Head row (title) of a slider card.
fn head_of(card: RectF) -> RectF {
    RectF { x: card.x + CAP_INSET, y: card.y + 9.0, w: card.w - 2.0 * CAP_INSET, h: 16.0 }
}

/// Left icon slot of a slider row (also the mute button on the volume row).
fn icon_of(s: RectF) -> RectF {
    RectF { x: s.x, y: s.y, w: END_W, h: s.h }
}

/// The draggable track between the two icon slots.
fn track_of(s: RectF) -> RectF {
    RectF { x: s.x + END_W, y: s.y, w: s.w - 2.0 * END_W, h: s.h }
}

/// Fraction 0..1 for pointer x on the track. The thumb centre travels from track.x+8 to track.x+w-8.
fn value_at(s: RectF, x: f32) -> f32 {
    let t = track_of(s);
    ((x - (t.x + THUMB / 2.0)) / (t.w - THUMB).max(1.0)).clamp(0.0, 1.0)
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

/// Circle button, macOS Control Center style. ON = filled white with a blue glyph;
/// OFF = translucent liquid-glass circle with a white glyph.
fn draw_round(r: &mut Renderer, rect: RectF, glyph: Glyph, size: f32, on: bool, k: f32, icon_k: f32) -> Result<()> {
    let radius = rect.h / 2.0;
    if on {
        r.fill_rounded_rect(rect, radius, fade(WHITE, k))?;
        super::glass::draw_highlights(r, rect, radius, 0.5 * k)?;
    } else if k >= 0.99 {
        super::glass::draw_card(r, rect, radius, ROUND_BG, k)?;
    } else {
        r.fill_rounded_rect(rect, radius, fade(ROUND_BG, k))?;
        super::glass::draw_highlights(r, rect, radius, k)?;
    }
    draw_glyph(r, rect, glyph, size, fade(if on { ACCENT } else { TEXT }, icon_k))
}

/// A panel icon: a Segoe glyph, or the dock's SF-style vector Wi-Fi / speaker icons.
#[derive(Clone, Copy)]
enum Glyph {
    Text(&'static str),
    Wifi(sficons::Wifi),
    /// Volume 0..1 and muted.
    Speaker(f32, bool),
}

fn draw_glyph(r: &mut Renderer, rect: RectF, glyph: Glyph, size: f32, rgba: u32) -> Result<()> {
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    match glyph {
        Glyph::Text(t) => r.draw_text_styled(t, rect, size, rgba, Align::Center, TextStyle::Icon),
        Glyph::Wifi(w) => sficons::draw_wifi(r, RectF { x: cx - size / 2.0, y: cy - size / 2.0, w: size, h: size }, w, rgba),
        Glyph::Speaker(v, m) => {
            let w = size * 18.0 / 16.0;
            sficons::draw_speaker(r, RectF { x: cx - w / 2.0, y: cy - size / 2.0, w, h: size }, v, m, rgba)
        }
    }
}

/// Liquid-glass tile (same material language as the dock tiles).
fn draw_module(r: &mut Renderer, rect: RectF, radius: f32) -> Result<()> {
    super::glass::draw_card(r, rect, radius, CARD, 0.9)
}

/// One toggle tile: round button, bold label, sub text.
struct Toggle<'a> {
    glyph: Glyph,
    label: &'a str,
    sub: &'a str,
    on: bool,
    enabled: bool,
    /// Glyph drops to 35% opacity when off (Wi-Fi). Bluetooth stays full.
    dim_off: bool,
}

/// Circle button of a toggle tile.
fn round_of(tile: RectF) -> RectF {
    RectF { x: tile.x + 8.0, y: tile.y + (tile.h - ROUND) / 2.0, w: ROUND, h: ROUND }
}

fn draw_toggle(r: &mut Renderer, tile: RectF, t: Toggle) -> Result<()> {
    let k = if t.enabled { 1.0 } else { 0.6 };
    let rk = if t.enabled { k } else { k * 0.45 };
    let icon_k = if t.on || !t.dim_off { 1.0 } else { 0.35 };
    draw_module(r, tile, TILE_R)?;
    draw_round(r, round_of(tile), t.glyph, 18.0, t.on, rk, rk * icon_k)?;
    let tx = tile.x + 8.0 + ROUND + 9.0;
    let tw = (tile.x + tile.w - 8.0 - tx).max(0.0);
    r.draw_text_styled(t.label, RectF { x: tx, y: tile.y + 9.0, w: tw, h: 16.0 }, 12.0, fade(TEXT, k), Align::Left, TextStyle::Bold)?;
    r.draw_text(t.sub, RectF { x: tx, y: tile.y + 26.0, w: tw, h: 15.0 }, 10.5, fade(SUB, k), Align::Left)
}

/// Stadium pill with one centred glyph (network / sound shortcuts).
fn draw_pill_glyph(r: &mut Renderer, rect: RectF, glyph: Glyph) -> Result<()> {
    draw_module(r, rect, TILE_R)?;
    draw_glyph(r, rect, glyph, 18.0, TEXT)
}

/// Slider card background with its title and right-aligned value.
fn draw_cap(r: &mut Renderer, card: RectF, title: &str, value: &str) -> Result<()> {
    draw_module(r, card, CARD_R)?;
    let head = head_of(card);
    r.draw_text_styled(title, head, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(value, head, 10.5, SUB, Align::Right)
}

/// macOS slider row: small icon, thin track with white fill and a round thumb, big icon.
fn draw_capsule(r: &mut Renderer, s: RectF, frac: f32, enabled: bool, left: Glyph, right: Glyph) -> Result<()> {
    let k = if enabled { 1.0 } else { 0.4 };
    let t = track_of(s);
    let bar = RectF { x: t.x, y: t.y + (t.h - TRACK_H) / 2.0, w: t.w, h: TRACK_H };
    let x0 = t.x + THUMB / 2.0;
    let end = x0 + (t.w - THUMB) * frac.clamp(0.0, 1.0);
    r.fill_rounded_rect(bar, TRACK_H / 2.0, fade(TRACK, k))?;
    if end > bar.x {
        r.fill_rounded_rect(RectF { w: end - bar.x, ..bar }, TRACK_H / 2.0, fade(FILL, k))?;
    }
    r.fill_rounded_rect(RectF { x: end - THUMB / 2.0, y: t.y, w: THUMB, h: THUMB }, THUMB / 2.0, fade(WHITE, k))?;
    let slot = icon_of(s);
    draw_glyph(r, RectF { w: END_W - 4.0, ..slot }, left, 11.0, fade(SUB, k))?;
    let rs = RectF { x: s.x + s.w - END_W + 4.0, w: END_W - 4.0, ..slot };
    draw_glyph(r, rs, right, 15.0, fade(SUB, k))
}

fn radio_sub(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "Bật",
        Some(false) => "Tắt",
        None => "Không có",
    }
}

/// Draws the whole panel into `rect`.
pub fn draw(r: &mut Renderer, rect: RectF, state: &ControlState, media: Option<&Media>) -> Result<()> {
    let l = layout(rect);

    draw_toggle(
        r,
        l.wifi,
        Toggle {
            glyph: Glyph::Wifi(match sficons::Wifi::from_state(state.wifi, state.wifi_signal) {
                // The round button already shows on/off; keep the fan full while on.
                sficons::Wifi::Disconnected if state.wifi == Some(true) => sficons::Wifi::Bars(3),
                w => w,
            }),
            label: "Wi-Fi",
            sub: radio_sub(state.wifi),
            on: state.wifi == Some(true),
            enabled: state.wifi.is_some(),
            dim_off: true,
        },
    )?;
    // Bluetooth and network: two round buttons under the Wi-Fi tile.
    let bt_on = state.bluetooth == Some(true);
    let bt_k = if state.bluetooth.is_some() { 1.0 } else { 0.45 };
    draw_round(r, l.bluetooth, Glyph::Text(GLYPH_BT), 20.0, bt_on, bt_k, bt_k)?;
    draw_round(r, l.net, Glyph::Text(GLYPH_NETWORK), 20.0, false, 1.0, 1.0)?;
    draw_now_playing(r, l.media, media)?;

    // Dark mode pill (the "Focus" slot) and the two shortcut tiles.
    draw_module(r, l.dark, TILE_R)?;
    let moon = round_of(l.dark);
    draw_round(r, moon, Glyph::Text(GLYPH_MOON), 16.0, state.dark, 1.0, 1.0)?;
    r.draw_text_styled(
        "Chế độ tối",
        RectF { x: moon.x + ROUND + 9.0, y: l.dark.y, w: l.dark.w - ROUND - 26.0, h: l.dark.h },
        12.0,
        TEXT,
        Align::Left,
        TextStyle::Bold,
    )?;
    draw_pill_glyph(r, l.sound, Glyph::Speaker(1.0, false))?;
    draw_pill_glyph(r, l.display, Glyph::Text(GLYPH_DISPLAY))?;

    // Brightness: unsupported shows the note and a disabled, empty slider.
    let (br_frac, br_text) = match state.brightness {
        Some(p) => (p.min(100) as f32 / 100.0, format!("{}%", p.min(100))),
        None => (0.0, "Không hỗ trợ".to_string()),
    };
    draw_cap(r, l.brightness, "Màn hình", &br_text)?;
    draw_capsule(r, slider_of(l.brightness), br_frac, state.brightness.is_some(), Glyph::Text(GLYPH_BRIGHTNESS), Glyph::Text(GLYPH_BRIGHTNESS))?;

    // Volume: the small icon at left toggles mute.
    let vol = state.volume.clamp(0.0, 1.0);
    let vol_text = if state.muted { "Đã tắt tiếng".to_string() } else { format!("{}%", (vol * 100.0).round() as u32) };
    draw_cap(r, l.volume, "Âm thanh", &vol_text)?;
    draw_capsule(r, slider_of(l.volume), vol, true, Glyph::Speaker(vol, state.muted), Glyph::Speaker(1.0, false))?;

    for (rc, (glyph, _)) in l.shortcuts.into_iter().zip(SHORTCUTS) {
        draw_round(r, rc, Glyph::Text(glyph), 18.0, false, 1.0, 1.0)?;
    }

    // SmartDock Settings pill, like macOS "Edit Controls".
    draw_module(r, l.settings, l.settings.h / 2.0)?;
    r.draw_text_styled("Cài đặt SmartDock", l.settings, 10.5, TEXT, Align::Center, TextStyle::Bold)
}

/// Prev / play / next, centred along the bottom of the Now Playing tile.
fn media_buttons(card: RectF) -> [RectF; 3] {
    let cx = card.x + card.w / 2.0;
    [-1.0, 0.0, 1.0].map(|i| RectF { x: cx + i * 38.0 - 15.0, y: card.y + card.h - 34.0, w: 30.0, h: 26.0 })
}

fn draw_now_playing(r: &mut Renderer, card: RectF, media: Option<&Media>) -> Result<()> {
    draw_module(r, card, CARD_R + 2.0)?;
    let art = RectF { x: card.x + 12.0, y: card.y + 12.0, w: 40.0, h: 40.0 };
    r.fill_rounded_rect_gradient(art, 9.0, 0xFF5F_6DFF, 0xFFC3_71FF)?;
    r.draw_text("♪", art, 18.0, WHITE, Align::Center)?;
    let has = media.is_some_and(|m| !m.title.is_empty());
    let title = media.filter(|_| has).map_or("Không có gì đang phát", |m| m.title.as_str());
    let artist = media.filter(|_| has).map_or("Now Playing", |m| m.artist.as_str());
    let w = card.w - 24.0;
    r.draw_text_styled(title, RectF { x: card.x + 12.0, y: card.y + 56.0, w, h: 16.0 }, 11.5, TEXT, Align::Left, TextStyle::Bold)?;
    r.draw_text(artist, RectF { x: card.x + 12.0, y: card.y + 71.0, w, h: 14.0 }, 10.0, SUB, Align::Left)?;
    let glyphs = ["\u{E892}", if media.is_some_and(|m| m.playing) { "\u{E769}" } else { "\u{E768}" }, "\u{E893}"];
    for (button, glyph) in media_buttons(card).into_iter().zip(glyphs) {
        r.draw_text_styled(glyph, button, 14.0, fade(TEXT, if has { 1.0 } else { 0.4 }), Align::Center, TextStyle::Icon)?;
    }
    Ok(())
}

// ---------- hit testing ----------

/// Which slider a drag controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slider {
    Volume,
    Brightness,
}

/// Slider under (x, y) for a drag start (the volume glyph is the mute button, not the track).
pub fn slider_at(x: f32, y: f32, rect: RectF, state: &ControlState) -> Option<Slider> {
    let l = layout(rect);
    let vol = slider_of(l.volume);
    if contains(vol, x, y) && !contains(icon_of(vol), x, y) {
        return Some(Slider::Volume);
    }
    let br = slider_of(l.brightness);
    if state.brightness.is_some() && contains(br, x, y) && !contains(icon_of(br), x, y) {
        return Some(Slider::Brightness);
    }
    None
}

/// Slider whose whole card (title row included) is under (x, y): the mouse wheel adjusts it.
pub fn slider_card_at(x: f32, y: f32, rect: RectF) -> Option<Slider> {
    let l = layout(rect);
    if contains(l.volume, x, y) {
        Some(Slider::Volume)
    } else if contains(l.brightness, x, y) {
        Some(Slider::Brightness)
    } else {
        None
    }
}

/// Fraction 0..1 for pointer x on `slider` (clamped, so dragging past the ends pins to 0/100%).
pub fn slider_value(slider: Slider, x: f32, rect: RectF) -> f32 {
    let l = layout(rect);
    let card = match slider {
        Slider::Volume => l.volume,
        Slider::Brightness => l.brightness,
    };
    value_at(slider_of(card), x)
}

/// Maps a click at (x, y) to an action. None when the click hits nothing actionable.
pub fn hit_test(x: f32, y: f32, rect: RectF, state: &ControlState) -> Option<ControlAction> {
    let l = layout(rect);
    // Only the round icon toggles a radio; the rest of the tile opens its list / settings page.
    if contains(round_of(l.wifi), x, y) {
        return state.wifi.map(|_| ControlAction::ToggleWifi);
    }
    if contains(l.wifi, x, y) {
        return Some(ControlAction::OpenWifiList);
    }
    if contains(l.bluetooth, x, y) {
        return state.bluetooth.map(|_| ControlAction::ToggleBluetooth);
    }
    if contains(l.settings, x, y) {
        return Some(ControlAction::OpenSmartDockSettings);
    }
    if contains(l.dark, x, y) {
        return Some(ControlAction::ToggleDark);
    }
    if contains(l.net, x, y) {
        return Some(ControlAction::OpenNetworkSettings);
    }
    if contains(l.display, x, y) {
        return Some(ControlAction::Launch("ms-settings:display"));
    }
    if contains(l.sound, x, y) {
        return Some(ControlAction::OpenSoundSettings);
    }
    for (rc, (_, uri)) in l.shortcuts.into_iter().zip(SHORTCUTS) {
        if contains(rc, x, y) {
            return Some(ControlAction::Launch(uri));
        }
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
    for (button, action) in media_buttons(l.media).into_iter().zip([
        ControlAction::MediaPrevious,
        ControlAction::MediaPlayPause,
        ControlAction::MediaNext,
    ]) {
        if contains(button, x, y) {
            return Some(action);
        }
    }
    None
}

#[cfg(test)]
mod settings_route_tests {
    use super::*;

    #[test]
    fn dock_layout_cc_settings_pill_opens_smartdock_settings_and_dark_pill_still_toggles() {
        let rect = RectF { x: 0.5, y: 0.5, w: 339.0, h: HEIGHT - 1.0 };
        let l = layout(rect);
        assert!(l.settings.y + l.settings.h <= rect.y + rect.h);
        let state = ControlState::default();
        let s = l.settings;
        let hit = hit_test(s.x + s.w / 2.0, s.y + s.h / 2.0, rect, &state);
        assert!(matches!(hit, Some(ControlAction::OpenSmartDockSettings)), "{hit:?}");
        let other = hit_test(l.dark.x + 20.0, l.dark.y + 20.0, rect, &state);
        assert!(matches!(other, Some(ControlAction::ToggleDark)), "{other:?}");
    }
}
