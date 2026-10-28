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
    pub ethernet: bool,         // wired adapter with a cable connected
    /// Connected Wi-Fi network name. Read only while the full panel is open (cached otherwise).
    pub ssid: Option<String>,
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
            ethernet: system::ethernet_connected(),
            ssid: None, // filled by the first full-panel refresh
            bluetooth: system::bluetooth_enabled(),
            dark: system::dark_mode(),
        }
    }

    /// The fixed dock button uses only Wi-Fi and volume. Other hardware reads
    /// are needed while the full panel is visible; keep its last snapshot idle.
    pub fn refresh(&mut self, full_panel: bool) {
        let (volume, muted) = system::get_volume();
        self.volume = volume;
        self.muted = muted;
        self.wifi = system::wifi_enabled();
        self.wifi_signal = system::wifi_signal();
        self.ethernet = system::ethernet_connected();
        if full_panel {
            // SSID: one WLAN query per panel refresh, only while the Control Center is open.
            self.ssid = if self.wifi_signal.is_some() { system::wifi_ssid() } else { None };
            self.brightness = system::get_brightness();
            self.bluetooth = system::bluetooth_enabled();
            self.dark = system::dark_mode();
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
    /// Opens the in-panel list of audio output devices (UI thread).
    PickOutput,
    /// Leaves the output list without changing anything (UI thread).
    CloseOutputs,
    /// Makes output device `n` of the open list the default.
    SetOutput(usize),
}

thread_local! {
    /// Output devices while the picker is open (UI thread only); None shows the normal panel.
    static OUTPUTS: std::cell::RefCell<Option<Vec<system::AudioOutput>>> = const { std::cell::RefCell::new(None) };
}

pub fn outputs_open() -> bool {
    OUTPUTS.with(|o| o.borrow().is_some())
}

pub fn show_outputs(list: Vec<system::AudioOutput>) {
    OUTPUTS.with(|o| *o.borrow_mut() = Some(list));
}

pub fn hide_outputs() {
    OUTPUTS.with(|o| *o.borrow_mut() = None);
}

/// Id of device `n` in the open list.
pub fn output_id(n: usize) -> Option<String> {
    OUTPUTS.with(|o| o.borrow().as_ref().and_then(|l| l.get(n).map(|d| d.id.clone())))
}

const OUTPUT_ROW_H: f32 = 44.0;

fn outputs_back(rect: RectF) -> RectF {
    RectF { x: rect.x + PAD, y: rect.y + PAD, w: rect.w - 2.0 * PAD, h: 30.0 }
}

fn output_row(rect: RectF, i: usize) -> RectF {
    RectF {
        x: rect.x + PAD,
        y: rect.y + PAD + 40.0 + i as f32 * (OUTPUT_ROW_H + GAP),
        w: rect.w - 2.0 * PAD,
        h: OUTPUT_ROW_H,
    }
}

fn draw_outputs(r: &mut Renderer, rect: RectF, list: &[system::AudioOutput]) -> Result<()> {
    let back = outputs_back(rect);
    draw_module(r, back, back.h / 2.0)?;
    r.draw_text_styled("‹  Đầu ra âm thanh", RectF { x: back.x + 12.0, w: back.w - 24.0, ..back }, 12.0, text(), Align::Left, TextStyle::Bold)?;
    if list.is_empty() {
        return r.draw_text("Không tìm thấy thiết bị", output_row(rect, 0), 11.5, sub(), Align::Center);
    }
    for (i, d) in list.iter().enumerate() {
        let row = output_row(rect, i);
        draw_module(r, row, 16.0)?;
        let dot = RectF { x: row.x + 10.0, y: row.y + (row.h - 24.0) / 2.0, w: 24.0, h: 24.0 };
        draw_glyph(r, dot, speaker(1.0, false), 16.0, if d.is_default { text() } else { sub() })?;
        let label = RectF { x: row.x + 42.0, y: row.y, w: row.w - 42.0 - 34.0, h: row.h };
        r.draw_text_styled(&d.name, label, 11.5, text(), Align::Left, if d.is_default { TextStyle::Bold } else { TextStyle::Ui })?;
        if d.is_default {
            let c = RectF { x: row.x + row.w - 30.0, y: row.y + (row.h - 18.0) / 2.0, w: 18.0, h: 18.0 };
            r.fill_rounded_rect(c, 9.0, ACCENT)?;
            r.draw_text_styled("✓", c, 11.0, WHITE, Align::Center, TextStyle::Bold)?;
        }
    }
    Ok(())
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
        // Output picker actions touch UI-thread state; the caller handles them.
        ControlAction::PickOutput | ControlAction::CloseOutputs | ControlAction::SetOutput(_) => Ok(()),
    }
}

// ---------- layout (DIPs, relative to the panel rect) ----------
// .cc: padding 9, gap 8. .cc-grid: 1.35fr / 1fr. Every card has a 1 px border, included in the offsets.

// ---------- layout (DIPs, relative to the panel rect) ----------
// macOS 27 style: independent modules on one strict four-column grid.

/// Nominal popup dimensions. The one-DIP client inset leaves a 60-DIP grid unit (11-DIP gaps) at
/// 100% scale, the macOS 27 proportions: tall capsules, big badges and type.
pub const WIDTH: f32 = 290.0;
pub const HEIGHT: f32 = 471.0;
const PAD: f32 = 8.0;
const GAP: f32 = 11.0;
const SLIDER_CARD_H: f32 = 60.0;
const SETTINGS_H: f32 = 28.0;
const SETTINGS_W: f32 = 160.0;
const TILE_R: f32 = 30.0;
const CARD_R: f32 = 22.0;
const ROUND: f32 = 40.0; // white badge inside a labelled two-column module
const BADGE_INSET: f32 = 10.0;
const CAP_INSET: f32 = 14.0;
const END_W: f32 = 24.0; // icon slots at both ends of a slider
const SLIDER_H: f32 = 18.0;
const THUMB: f32 = 0.0; // no knob: the fill ends exactly at the value
const TRACK_H: f32 = 4.5;
const TITLE_PT: f32 = 14.0;
const SUB_PT: f32 = 12.5;

// Colors 0xRRGGBBAA.
const WHITE: u32 = 0xFFFF_FFFF;
const ACCENT: u32 = 0x0A84_FFFF; // macOS system blue #0A84FF

thread_local! {
    /// Appearance used by the palette below; set at the start of `draw` from `ControlState::dark`.
    static DARK: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

fn dark() -> bool {
    DARK.with(|d| d.get())
}

/// Title text and glyphs: white in dark mode, near-black in light mode (macOS).
fn text() -> u32 {
    if dark() { 0xFFFF_FFFF } else { 0x1D1D_1FFF }
}

/// Secondary text and slider end icons.
fn sub() -> u32 {
    if dark() { 0xFFFF_FFB0 } else { 0x1D1D_1FA0 }
}

/// Slider track: translucent light line (darker translucent in light mode).
fn track() -> u32 {
    if dark() { 0xFFFF_FF40 } else { 0x0000_002A }
}

/// Slider fill: a thin solid bar.
fn fill() -> u32 {
    if dark() { 0xFFFF_FFFF } else { 0x2C2C_2EE6 }
}

/// Bottom shortcut circles: glyph and the URI they open.
const SHORTCUTS: [(Glyph, &str); 4] = [
    (Glyph::Calculator, "calculator:"),
    (Glyph::Camera, "ms-screenclip:"),
    (Glyph::Timer, "ms-clock:"),
    (Glyph::Gear, "ms-settings:"),
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
    let col = (w - 3.0 * GAP) / 4.0;
    let span = |n: f32| n * col + (n - 1.0) * GAP;
    let x = |column: f32| x0 + column * (col + GAP);
    let double = span(2.0);
    let y1 = rect.y + PAD;
    let y1b = y1 + col + GAP;
    let y2 = y1b + col + GAP;
    let y3 = y2 + col + GAP;
    let y4 = y3 + SLIDER_CARD_H + GAP;
    let y5 = y4 + SLIDER_CARD_H + GAP;
    let y6 = y5 + col + GAP;
    Layout {
        wifi: RectF { x: x(0.0), y: y1, w: double, h: col },
        bluetooth: RectF { x: x(0.0), y: y1b, w: col, h: col },
        net: RectF { x: x(1.0), y: y1b, w: col, h: col },
        media: RectF { x: x(2.0), y: y1, w: double, h: span(2.0) },
        dark: RectF { x: x(0.0), y: y2, w: double, h: col },
        sound: RectF { x: x(2.0), y: y2, w: col, h: col },
        display: RectF { x: x(3.0), y: y2, w: col, h: col },
        brightness: RectF { x: x0, y: y3, w, h: SLIDER_CARD_H },
        volume: RectF { x: x0, y: y4, w, h: SLIDER_CARD_H },
        shortcuts: [0.0, 1.0, 2.0, 3.0].map(|i| RectF { x: x(i), y: y5, w: col, h: col }),
        settings: RectF { x: rect.x + (rect.w - SETTINGS_W) / 2.0, y: y6, w: SETTINGS_W, h: SETTINGS_H },
    }
}

/// Slider row inside a slider card (below the title): icon slot, thin track, icon slot.
fn slider_of(card: RectF) -> RectF {
    RectF { x: card.x + CAP_INSET, y: card.y + 33.0, w: card.w - 2.0 * CAP_INSET, h: SLIDER_H }
}

/// Head row (title) of a slider card.
fn head_of(card: RectF) -> RectF {
    RectF { x: card.x + CAP_INSET, y: card.y + 9.0, w: card.w - 2.0 * CAP_INSET, h: 20.0 }
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
/// A rect no wider than `ROUND` is a badge inside a labelled module; a larger one is a
/// free-standing round module: smoky glass when off, a solid white disc when on (macOS 27).
/// Either way the "on" glyph is system blue.
fn draw_round(r: &mut Renderer, rect: RectF, glyph: Glyph, size: f32, on: bool, k: f32, icon_k: f32) -> Result<()> {
    // Badges and indicators glow system blue; a free-standing toggle shows a dark glyph on white.
    let on_color = if rect.w <= ROUND + 1.0 { ACCENT } else { 0x1C1C_1EFF };
    draw_round_with(r, rect, glyph, size, on, on_color, k, icon_k)
}

#[allow(clippy::too_many_arguments)]
fn draw_round_with(r: &mut Renderer, rect: RectF, glyph: Glyph, size: f32, on: bool, on_color: u32, k: f32, icon_k: f32) -> Result<()> {
    let radius = rect.h / 2.0;
    let badge_only = rect.w <= ROUND + 1.0;
    match (on, badge_only) {
        (true, _) => super::glass::draw_control_active(r, rect, radius, k)?,
        (false, true) => super::glass::draw_control_badge(r, rect, radius)?,
        (false, false) => super::glass::draw_control_card(r, rect, radius)?,
    }
    draw_glyph(r, rect, glyph, size, fade(if on { on_color } else { text() }, icon_k))
}

/// Panel icons: the shared vector icon set (one grid, one stroke weight).
type Glyph = super::ccicons::Icon;

/// Draws `glyph` in a `size`-DIP icon box centred in `rect`.
fn draw_glyph(r: &mut Renderer, rect: RectF, glyph: Glyph, size: f32, rgba: u32) -> Result<()> {
    super::ccicons::draw_in(r, glyph, rect, size, rgba)
}

/// Speaker icon for a volume level.
fn speaker(volume: f32, muted: bool) -> Glyph {
    Glyph::Speaker { waves: if muted { 0 } else { sficons::waves_for(volume) }, muted }
}

/// Liquid-glass tile (same material language as the dock tiles).
fn draw_module(r: &mut Renderer, rect: RectF, radius: f32) -> Result<()> {
    super::glass::draw_control_card(r, rect, radius)
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
    RectF { x: tile.x + BADGE_INSET, y: tile.y + (tile.h - ROUND) / 2.0, w: ROUND, h: ROUND }
}

fn draw_toggle(r: &mut Renderer, tile: RectF, t: Toggle) -> Result<()> {
    let k = if t.enabled { 1.0 } else { 0.6 };
    let rk = if t.enabled { k } else { k * 0.45 };
    let icon_k = if t.on || !t.dim_off { 1.0 } else { 0.35 };
    draw_module(r, tile, TILE_R)?;
    // The filled Wi-Fi fan spans ~57% of the badge (0.72 of its 32-DIP box), like macOS.
    let size = if matches!(t.glyph, Glyph::Wifi { .. }) { 32.0 } else { 20.0 };
    draw_round(r, round_of(tile), t.glyph, size, t.on, rk, rk * icon_k)?;
    let tx = tile.x + BADGE_INSET + ROUND + 10.0;
    let tw = (tile.x + tile.w - 8.0 - tx).max(0.0);
    let (line1, line2) = wrap_two(t.sub, tw, SUB_PT);
    let title_y = if line2.is_some() { tile.y + 7.0 } else { tile.y + 12.0 };
    r.draw_text_styled(t.label, RectF { x: tx, y: title_y, w: tw, h: 18.0 }, TITLE_PT, fade(text(), k), Align::Left, TextStyle::Bold)?;
    r.draw_text(&line1, RectF { x: tx, y: title_y + 18.0, w: tw, h: 16.0 }, SUB_PT, fade(sub(), k), Align::Left)?;
    if let Some(line2) = line2 {
        r.draw_text(&line2, RectF { x: tx, y: title_y + 32.0, w: tw, h: 16.0 }, SUB_PT, fade(sub(), k), Align::Left)?;
    }
    Ok(())
}

/// Splits a subtitle into at most two lines for a `width`-DIP column (macOS wraps the SSID).
/// Breaks at the last space that fits, else mid-word; the renderer ellipsizes line two.
fn wrap_two(text: &str, width: f32, size: f32) -> (String, Option<String>) {
    let cap = ((width / (size * 0.55)).floor() as usize).max(4);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= cap {
        return (text.to_string(), None);
    }
    let cut = chars[..=cap.min(chars.len() - 1)]
        .iter()
        .rposition(|c| *c == ' ')
        .filter(|&i| i > 0)
        .unwrap_or(cap);
    let first: String = chars[..cut].iter().collect();
    let rest: String = chars[cut..].iter().collect();
    (first.trim_end().to_string(), Some(rest.trim_start().to_string()))
}

/// Stadium pill with one centred glyph (network / sound shortcuts).
fn draw_pill_glyph(r: &mut Renderer, rect: RectF, glyph: Glyph) -> Result<()> {
    draw_module(r, rect, TILE_R)?;
    draw_glyph(r, rect, glyph, 24.0, text())
}

/// Slider card background with its title and right-aligned value.
fn draw_cap(r: &mut Renderer, card: RectF, title: &str, value: &str) -> Result<()> {
    draw_module(r, card, CARD_R)?;
    let head = head_of(card);
    r.draw_text_styled(title, head, TITLE_PT, text(), Align::Left, TextStyle::Bold)?;
    r.draw_text(value, head, SUB_PT, sub(), Align::Right)
}

/// macOS slider row: small icon, thin track with white fill and a round thumb, big icon.
fn draw_capsule(r: &mut Renderer, s: RectF, frac: f32, enabled: bool, left: Glyph, right: Glyph) -> Result<()> {
    let k = if enabled { 1.0 } else { 0.4 };
    let t = track_of(s);
    let bar = RectF { x: t.x, y: t.y + (t.h - TRACK_H) / 2.0, w: t.w, h: TRACK_H };
    let x0 = t.x + THUMB / 2.0;
    let end = x0 + (t.w - THUMB) * frac.clamp(0.0, 1.0);
    // macOS 26/27: a thin solid fill on a translucent track, no knob.
    let end = end + THUMB / 2.0 * frac.clamp(0.0, 1.0);
    r.fill_rounded_rect(bar, TRACK_H / 2.0, fade(track(), k))?;
    if end > bar.x + TRACK_H {
        r.fill_rounded_rect(RectF { w: end - bar.x, ..bar }, TRACK_H / 2.0, fade(fill(), k))?;
    }
    let slot = icon_of(s);
    draw_glyph(r, RectF { w: END_W - 6.0, ..slot }, left, 16.0, fade(text(), k))?;
    let rs = RectF { x: s.x + s.w - END_W + 6.0, w: END_W - 6.0, ..slot };
    draw_glyph(r, rs, right, 20.0, fade(text(), k))
}

/// Draws the whole panel into `rect`.
pub fn draw(r: &mut Renderer, rect: RectF, state: &ControlState, media: Option<&Media>) -> Result<()> {
    DARK.with(|d| d.set(state.dark));
    super::glass::set_control_dark(state.dark);
    if let Some(done) = OUTPUTS.with(|o| o.borrow().as_ref().map(|l| draw_outputs(r, rect, l))) {
        return done;
    }
    let l = layout(rect);

    // Wi-Fi tile: always the Wi-Fi radio, subtitled with the network name like macOS.
    let wifi_sub = match state.wifi {
        Some(true) => state.ssid.clone().unwrap_or_else(|| "Chưa kết nối".to_string()),
        Some(false) => "Tắt".to_string(),
        None => "Không có".to_string(),
    };
    draw_toggle(
        r,
        l.wifi,
        Toggle {
            // Signal strength while connected; a full fan otherwise (the badge already shows on/off).
            glyph: Glyph::Wifi {
                bars: match (state.wifi, state.wifi_signal) {
                    (Some(true), Some(q)) => sficons::bars_for(q),
                    _ => 3,
                },
                off: false,
            },
            label: "Wi-Fi",
            sub: &wifi_sub,
            on: state.wifi == Some(true),
            enabled: state.wifi.is_some(),
            dim_off: true,
        },
    )?;
    // Bluetooth and network: two round buttons under the Wi-Fi tile. The network button lights up
    // (white disc, blue Ethernet glyph) while a wired adapter is connected.
    let bt_on = state.bluetooth == Some(true);
    let bt_k = if state.bluetooth.is_some() { 1.0 } else { 0.45 };
    draw_round(r, l.bluetooth, Glyph::Bluetooth, 24.0, bt_on, bt_k, bt_k)?;
    draw_round_with(r, l.net, Glyph::Network, 24.0, state.ethernet, ACCENT, 1.0, 1.0)?;
    draw_now_playing(r, l.media, media)?;

    // Dark mode pill (the "Focus" slot) and the two shortcut tiles.
    draw_module(r, l.dark, TILE_R)?;
    let moon = round_of(l.dark);
    draw_round(r, moon, Glyph::Moon, 22.0, state.dark, 1.0, 1.0)?;
    r.draw_text_styled(
        "Chế độ tối",
        RectF {
            x: moon.x + ROUND + 10.0,
            y: l.dark.y,
            w: (l.dark.x + l.dark.w) - (moon.x + ROUND + 10.0) - 4.0,
            h: l.dark.h,
        },
        12.5,
        text(),
        Align::Left,
        TextStyle::Bold,
    )?;
    draw_pill_glyph(r, l.sound, speaker(1.0, false))?;
    draw_pill_glyph(r, l.display, Glyph::Display)?;

    // Brightness: unsupported shows the note and a disabled, empty slider.
    let (br_frac, br_text) = match state.brightness {
        Some(p) => (p.min(100) as f32 / 100.0, format!("{}%", p.min(100))),
        None => (0.0, "Không hỗ trợ".to_string()),
    };
    draw_cap(r, l.brightness, "Màn hình", &br_text)?;
    draw_capsule(r, slider_of(l.brightness), br_frac, state.brightness.is_some(), Glyph::SunMin, Glyph::SunMax)?;

    // Volume: the small icon at left toggles mute.
    let vol = state.volume.clamp(0.0, 1.0);
    let vol_text = if state.muted { "Đã tắt tiếng".to_string() } else { format!("{}%", (vol * 100.0).round() as u32) };
    draw_cap(r, l.volume, "Âm thanh", &vol_text)?;
    // "Âm thanh ›": the chevron sits right after the measured title.
    let head = head_of(l.volume);
    let title_w = r.measure_text("Âm thanh", TITLE_PT, TextStyle::Bold).unwrap_or(62.0);
    draw_glyph(r, RectF { x: head.x + title_w + 3.0, y: head.y, w: 12.0, h: head.h }, Glyph::Chevron, 12.0, sub())?;
    draw_capsule(r, slider_of(l.volume), vol, true, speaker(0.0, state.muted), speaker(1.0, false))?;

    for (rc, (glyph, _)) in l.shortcuts.into_iter().zip(SHORTCUTS) {
        draw_round(r, rc, glyph, 24.0, false, 1.0, 1.0)?;
    }

    // SmartDock Settings pill, like macOS "Edit Controls".
    draw_module(r, l.settings, l.settings.h / 2.0)?;
    r.draw_text_styled("Cài đặt SmartDock", l.settings, 12.5, text(), Align::Center, TextStyle::Bold)
}

/// Prev / play / next, centred along the bottom of the Now Playing tile.
fn media_buttons(card: RectF) -> [RectF; 3] {
    let cx = card.x + card.w / 2.0;
    [-1.0, 0.0, 1.0].map(|i| RectF { x: cx + i * 38.0 - 16.0, y: card.y + card.h - 40.0, w: 32.0, h: 30.0 })
}

fn draw_now_playing(r: &mut Renderer, card: RectF, media: Option<&Media>) -> Result<()> {
    draw_module(r, card, CARD_R + 4.0)?;
    let art = RectF { x: card.x + 14.0, y: card.y + 14.0, w: 40.0, h: 40.0 };
    r.fill_rounded_rect_gradient(art, 8.0, 0xFF5F_6DFF, 0xFFC3_71FF)?;
    draw_glyph(r, art, Glyph::Music, 22.0, WHITE)?;
    let has = media.is_some_and(|m| !m.title.is_empty());
    let title = media.filter(|_| has).map_or("Không có gì đang phát", |m| m.title.as_str());
    let artist = media.filter(|_| has).map_or("Now Playing", |m| m.artist.as_str());
    let w = card.w - 26.0;
    r.draw_text_styled(title, RectF { x: card.x + 14.0, y: card.y + 60.0, w, h: 18.0 }, TITLE_PT, text(), Align::Left, TextStyle::Bold)?;
    r.draw_text(artist, RectF { x: card.x + 14.0, y: card.y + 77.0, w, h: 16.0 }, SUB_PT, sub(), Align::Left)?;
    let glyphs = [Glyph::Prev, if media.is_some_and(|m| m.playing) { Glyph::Pause } else { Glyph::Play }, Glyph::Next];
    for (button, glyph) in media_buttons(card).into_iter().zip(glyphs) {
        let size = if matches!(glyph, Glyph::Prev | Glyph::Next) { 22.0 } else { 20.0 };
        draw_glyph(r, button, glyph, size, fade(sub(), if has { 1.0 } else { 0.45 }))?;
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
    if outputs_open() {
        return None;
    }
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
    if let Some(hit) = OUTPUTS.with(|o| {
        o.borrow().as_ref().map(|l| {
            if contains(outputs_back(rect), x, y) {
                return Some(ControlAction::CloseOutputs);
            }
            (0..l.len()).find(|&i| contains(output_row(rect, i), x, y)).map(ControlAction::SetOutput)
        })
    }) {
        return hit;
    }
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
        return Some(ControlAction::PickOutput);
    }
    for (rc, (_, uri)) in l.shortcuts.into_iter().zip(SHORTCUTS) {
        if contains(rc, x, y) {
            return Some(ControlAction::Launch(uri));
        }
    }
    // The "Âm thanh ›" title row opens the output device list.
    if contains(head_of(l.volume), x, y) {
        return Some(ControlAction::PickOutput);
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

    fn nominal_rect() -> RectF {
        RectF { x: 0.5, y: 0.5, w: WIDTH - 1.0, h: HEIGHT - 1.0 }
    }

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "{a} != {b}");
    }

    #[test]
    fn control_center_uses_an_even_four_column_grid() {
        let rect = nominal_rect();
        let l = layout(rect);
        let unit = l.shortcuts[0].w;

        for circle in [l.bluetooth, l.net, l.sound, l.display, l.shortcuts[0], l.shortcuts[1], l.shortcuts[2], l.shortcuts[3]] {
            close(circle.w, unit);
            close(circle.h, unit);
        }
        for pair in [l.wifi, l.dark] {
            close(pair.w, 2.0 * unit + GAP);
            close(pair.h, unit);
        }
        close(l.media.w, 2.0 * unit + GAP);
        close(l.media.h, 2.0 * unit + GAP);

        for pair in l.shortcuts.windows(2) {
            close(pair[1].x - (pair[0].x + pair[0].w), GAP);
        }
        close(l.net.x - (l.bluetooth.x + l.bluetooth.w), GAP);
        close(l.media.x - (l.wifi.x + l.wifi.w), GAP);
        close(l.brightness.y - (l.dark.y + l.dark.h), GAP);
        close(l.volume.y - (l.brightness.y + l.brightness.h), GAP);
        close(l.shortcuts[0].y - (l.volume.y + l.volume.h), GAP);
        close(l.wifi.x, l.brightness.x);
        close(l.display.x + l.display.w, l.brightness.x + l.brightness.w);
        assert!(l.settings.y + l.settings.h <= rect.y + rect.h);
    }

    #[test]
    fn hit_testing_tracks_the_new_module_rects() {
        let rect = nominal_rect();
        let l = layout(rect);
        let state = ControlState { wifi: Some(true), bluetooth: Some(true), brightness: Some(75), ..Default::default() };
        let center = |r: RectF| (r.x + r.w / 2.0, r.y + r.h / 2.0);

        let (x, y) = center(l.bluetooth);
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::ToggleBluetooth)));
        let (x, y) = center(l.net);
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::OpenNetworkSettings)));
        let (x, y) = center(l.dark);
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::ToggleDark)));
        let (x, y) = center(l.settings);
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::OpenSmartDockSettings)));
    }

    #[test]
    fn dock_layout_cc_settings_pill_opens_smartdock_settings_and_dark_pill_still_toggles() {
        let rect = nominal_rect();
        let l = layout(rect);
        assert!(l.settings.y + l.settings.h <= rect.y + rect.h);
        let state = ControlState::default();
        let s = l.settings;
        let hit = hit_test(s.x + s.w / 2.0, s.y + s.h / 2.0, rect, &state);
        assert!(matches!(hit, Some(ControlAction::OpenSmartDockSettings)), "{hit:?}");
        let other = hit_test(l.dark.x + 20.0, l.dark.y + 20.0, rect, &state);
        assert!(matches!(other, Some(ControlAction::ToggleDark)), "{other:?}");
    }

    #[test]
    fn wifi_subtitle_wraps_long_ssids_onto_two_lines() {
        assert_eq!(wrap_two("BnK_5G", 70.0, 12.5), ("BnK_5G".to_string(), None));
        let (a, b) = wrap_two("Slice Of Heaven Guest", 70.0, 12.5);
        assert_eq!(a, "Slice Of");
        assert_eq!(b.as_deref(), Some("Heaven Guest"));
    }
}
