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
    /// Default microphone muted; None without a capture device. Read with the full panel only.
    pub mic_muted: Option<bool>,
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
            mic_muted: system::mic_muted(),
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
            self.mic_muted = system::mic_muted();
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
    /// "Sửa điều khiển" pill / right-click: enter edit mode (UI thread).
    EnterEdit,
    /// Edit mode "−" on a module (UI thread).
    EditRemove(Module),
    /// Edit mode gallery "+" (UI thread).
    EditAdd(Module),
    /// Edit mode "Khôi phục mặc định" (UI thread).
    EditReset,
    /// Edit mode "Xong": keep and save the layout (UI thread).
    EditDone,
    /// Microphone module: toggles mute on the default capture device.
    ToggleMic,
    LockScreen,
    SleepNow,
    /// Win+V.
    ClipboardHistory,
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
const OUTPUT_HEAD_H: f32 = 34.0;
const OUTPUT_INSET: f32 = 10.0;

/// The output picker's glass panel: one Control Center module sized to its rows and anchored to
/// the bottom of the panel rect (just above the dock button, where the main modules end), so the
/// sub-page opens in place instead of floating bare at the top of the popup window.
fn outputs_card(rect: RectF) -> RectF {
    let n = OUTPUTS.with(|o| o.borrow().as_ref().map_or(0, Vec::len)).max(1) as f32;
    let h = (OUTPUT_INSET * 2.0 + OUTPUT_HEAD_H + 6.0 + n * OUTPUT_ROW_H + (n - 1.0) * 6.0).min(rect.h - 2.0 * EDGE);
    // Same footprint edges as the main panel background (it replaces it; no double glass).
    RectF { x: rect.x + EDGE, y: rect.y + rect.h - EDGE - h, w: rect.w - 2.0 * EDGE, h }
}

fn outputs_back(rect: RectF) -> RectF {
    let c = outputs_card(rect);
    RectF { x: c.x + OUTPUT_INSET, y: c.y + OUTPUT_INSET, w: c.w - 2.0 * OUTPUT_INSET, h: OUTPUT_HEAD_H }
}

fn output_row(rect: RectF, i: usize) -> RectF {
    let back = outputs_back(rect);
    RectF { x: back.x, y: back.y + back.h + 6.0 + i as f32 * (OUTPUT_ROW_H + 6.0), w: back.w, h: OUTPUT_ROW_H }
}

fn draw_outputs(r: &mut Renderer, rect: RectF, list: &[system::AudioOutput]) -> Result<()> {
    // Same smoky glass as the main modules (blurred backdrop, bevel rim, shadow).
    draw_module(r, outputs_card(rect), PANEL_R)?;
    let back = outputs_back(rect);
    let chev = RectF { x: back.x, y: back.y, w: 20.0, h: back.h };
    draw_glyph(r, chev, Glyph::Back, 14.0, text())?;
    r.draw_text_styled("Đầu ra âm thanh", RectF { x: back.x + 22.0, w: back.w - 22.0, ..back }, TITLE_PT, text(), Align::Left, TextStyle::Bold)?;
    if list.is_empty() {
        return r.draw_text("Không tìm thấy thiết bị", output_row(rect, 0), SUB_PT, sub(), Align::Center);
    }
    for (i, d) in list.iter().enumerate() {
        let row = output_row(rect, i);
        // Rows: a faint lit well (the selected one brighter), like a CC list row.
        r.fill_rounded_rect(row, 14.0, if d.is_default { 0xFFFF_FF2E } else { 0xFFFF_FF14 })?;
        let dot = RectF { x: row.x + 8.0, y: row.y + (row.h - 28.0) / 2.0, w: 28.0, h: 28.0 };
        if d.is_default {
            r.fill_rounded_rect(dot, 14.0, ACCENT)?;
            draw_glyph(r, dot, speaker(1.0, false), 16.0, WHITE)?;
        } else {
            r.fill_rounded_rect(dot, 14.0, 0xFFFF_FF26)?;
            draw_glyph(r, dot, speaker(1.0, false), 16.0, text())?;
        }
        let label = RectF { x: row.x + 44.0, y: row.y, w: row.w - 44.0 - 32.0, h: row.h };
        r.draw_text_styled(&d.name, label, 12.5, text(), Align::Left, if d.is_default { TextStyle::Bold } else { TextStyle::Ui })?;
        if d.is_default {
            let c = RectF { x: row.x + row.w - 28.0, y: row.y + (row.h - 18.0) / 2.0, w: 18.0, h: 18.0 };
            r.draw_text_styled("✓", c, 13.0, ACCENT, Align::Center, TextStyle::Bold)?;
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
        // Wired link: the Ethernet page; otherwise the network status overview.
        ControlAction::OpenNetworkSettings => system::open_settings(if system::ethernet_connected() {
            "ms-settings:network-ethernet"
        } else {
            "ms-settings:network-status"
        }),
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
        ControlAction::EnterEdit
        | ControlAction::EditRemove(_)
        | ControlAction::EditAdd(_)
        | ControlAction::EditReset
        | ControlAction::EditDone => Ok(()),
        ControlAction::ToggleMic => {
            let muted = system::mic_muted().ok_or_else(|| Error::from(E_NOTIMPL))?;
            system::set_mic_mute(!muted)
        }
        ControlAction::LockScreen => system::lock_screen(),
        ControlAction::SleepNow => system::sleep_now(),
        ControlAction::ClipboardHistory => system::clipboard_history(),
    }
}

// ---------- layout (DIPs, relative to the panel rect) ----------
// .cc: padding 9, gap 8. .cc-grid: 1.35fr / 1fr. Every card has a 1 px border, included in the offsets.

// ---------- layout (DIPs, relative to the panel rect) ----------
// macOS 27 style: independent modules on one strict four-column grid.

/// Nominal popup dimensions. The one-DIP client inset leaves a 60-DIP grid unit (11-DIP gaps) at
/// 100% scale, the macOS 27 proportions: tall capsules, big badges and type.
/// Popup window size: the panel plus `EDGE` of room for its shadow on every side.
pub const WIDTH: f32 = 308.0;
/// Height of the default layout; the live height comes from `height()` (Edit Controls).
pub const HEIGHT: f32 = 489.0;
/// Window edge to the panel background (room for its tight shadow).
const EDGE: f32 = 4.0;
/// Panel background edge to the modules, like macOS's Control Center padding.
const PANEL_PAD: f32 = 13.0;
/// Window edge to the modules.
const PAD: f32 = EDGE + PANEL_PAD;
/// Corner radius of the panel background.
const PANEL_R: f32 = 30.0;
const GAP: f32 = 11.0;
const SLIDER_CARD_H: f32 = 60.0;
const SETTINGS_H: f32 = 28.0;
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

/// White ink on the clear glass (both appearances, as on macOS 27). The light appearance falls
/// back to dark ink only over a near-white backdrop, where white would be unreadable.
fn white_ink() -> bool {
    dark() || !super::glass::control_ink_dark()
}

/// Title text and glyphs.
fn text() -> u32 {
    if white_ink() { 0xFFFF_FFFF } else { 0x1D1D_1FFF }
}

/// Secondary text and slider end icons.
fn sub() -> u32 {
    if white_ink() { 0xFFFF_FFC0 } else { 0x1D1D_1FA8 }
}

/// Slider track: translucent line.
fn track() -> u32 {
    if white_ink() { 0xFFFF_FF3C } else { 0x0000_001F }
}

/// Slider fill: a solid bar.
fn fill() -> u32 {
    if white_ink() { 0xFFFF_FFFF } else { 0x3A3A_3CFF }
}

// ---------- modules and the grid (Edit Controls) ----------

/// One Control Center control. The panel shows an ordered list of them, packed onto the
/// four-column grid; the user can remove, add and reorder them in edit mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    Wifi,
    Bluetooth,
    Network,
    Media,
    Dark,
    Sound,
    Display,
    Brightness,
    Volume,
    Calculator,
    Snipping,
    Timer,
    SettingsApp,
    Airplane,
    NightLight,
    Focus,
    BatterySaver,
    Mic,
    Lock,
    Sleep,
    Clipboard,
    TaskManager,
    Explorer,
    Hotspot,
}

impl Module {
    /// Default order. First-fit packing of this list gives the classic layout exactly.
    pub const DEFAULT: [Module; 13] = [
        Module::Wifi,
        Module::Media,
        Module::Bluetooth,
        Module::Network,
        Module::Dark,
        Module::Sound,
        Module::Display,
        Module::Brightness,
        Module::Volume,
        Module::Calculator,
        Module::Snipping,
        Module::Timer,
        Module::SettingsApp,
    ];

    /// Every control, in gallery order.
    pub const ALL: [Module; 24] = [
        Module::Wifi,
        Module::Bluetooth,
        Module::Network,
        Module::Media,
        Module::Dark,
        Module::Sound,
        Module::Display,
        Module::Brightness,
        Module::Volume,
        Module::Calculator,
        Module::Snipping,
        Module::Timer,
        Module::SettingsApp,
        Module::Airplane,
        Module::NightLight,
        Module::Focus,
        Module::BatterySaver,
        Module::Mic,
        Module::Lock,
        Module::Sleep,
        Module::Clipboard,
        Module::TaskManager,
        Module::Explorer,
        Module::Hotspot,
    ];

    /// Stable id stored in settings.json (`ccModules`).
    pub fn key(self) -> &'static str {
        match self {
            Module::Wifi => "wifi",
            Module::Bluetooth => "bluetooth",
            Module::Network => "network",
            Module::Media => "media",
            Module::Dark => "dark",
            Module::Sound => "sound",
            Module::Display => "display",
            Module::Brightness => "brightness",
            Module::Volume => "volume",
            Module::Calculator => "calculator",
            Module::Snipping => "snipping",
            Module::Timer => "timer",
            Module::SettingsApp => "settings",
            Module::Airplane => "airplane",
            Module::NightLight => "nightlight",
            Module::Focus => "focus",
            Module::BatterySaver => "batterysaver",
            Module::Mic => "mic",
            Module::Lock => "lock",
            Module::Sleep => "sleep",
            Module::Clipboard => "clipboard",
            Module::TaskManager => "taskmanager",
            Module::Explorer => "explorer",
            Module::Hotspot => "hotspot",
        }
    }

    pub fn from_key(key: &str) -> Option<Module> {
        Module::ALL.into_iter().find(|m| m.key() == key)
    }

    /// Footprint in grid cells (columns, rows): 1x1 buttons, 2x1 labelled tiles, the 2x2 media
    /// tile and full-width sliders.
    fn span(self) -> (usize, usize) {
        match self {
            Module::Wifi | Module::Dark => (2, 1),
            Module::Media => (2, 2),
            Module::Brightness | Module::Volume => (4, 1),
            _ => (1, 1),
        }
    }

    /// Name in the add gallery.
    fn label(self) -> &'static str {
        match self {
            Module::Wifi => "Wi-Fi",
            Module::Bluetooth => "Bluetooth",
            Module::Network => "Mạng",
            Module::Media => "Đang phát",
            Module::Dark => "Chế độ tối",
            Module::Sound => "Đầu ra âm thanh",
            Module::Display => "Màn hình",
            Module::Brightness => "Độ sáng",
            Module::Volume => "Âm lượng",
            Module::Calculator => "Máy tính",
            Module::Snipping => "Chụp màn hình",
            Module::Timer => "Đồng hồ",
            Module::SettingsApp => "Cài đặt",
            Module::Airplane => "Chế độ máy bay",
            Module::NightLight => "Ánh sáng đêm",
            Module::Focus => "Tập trung",
            Module::BatterySaver => "Tiết kiệm pin",
            Module::Mic => "Tắt micro",
            Module::Lock => "Khóa máy",
            Module::Sleep => "Ngủ",
            Module::Clipboard => "Lịch sử bộ nhớ tạm",
            Module::TaskManager => "Trình quản lý tác vụ",
            Module::Explorer => "File Explorer",
            Module::Hotspot => "Điểm phát sóng",
        }
    }

    /// Icon in the gallery (and on the round buttons).
    fn glyph(self) -> Glyph {
        match self {
            Module::Wifi => Glyph::Wifi { bars: 3, off: false },
            Module::Bluetooth => Glyph::Bluetooth,
            Module::Network => Glyph::Network,
            Module::Media => Glyph::Music,
            Module::Dark => Glyph::Moon,
            Module::Sound => speaker(1.0, false),
            Module::Display => Glyph::Display,
            Module::Brightness => Glyph::SunMax,
            Module::Volume => speaker(1.0, false),
            Module::Calculator => Glyph::Calculator,
            Module::Snipping => Glyph::Camera,
            Module::Timer => Glyph::Timer,
            Module::SettingsApp => Glyph::Gear,
            Module::Airplane => Glyph::Airplane,
            Module::NightLight => Glyph::NightLight,
            Module::Focus => Glyph::Focus,
            Module::BatterySaver => Glyph::BatterySaver,
            Module::Mic => Glyph::Mic { muted: false },
            Module::Lock => Glyph::Lock,
            Module::Sleep => Glyph::Sleep,
            Module::Clipboard => Glyph::Clipboard,
            Module::TaskManager => Glyph::TaskManager,
            Module::Explorer => Glyph::Folder,
            Module::Hotspot => Glyph::Hotspot,
        }
    }

    /// URI or program a shortcut button opens.
    fn launch(self) -> Option<&'static str> {
        Some(match self {
            Module::Calculator => "calculator:",
            Module::Snipping => "ms-screenclip:",
            Module::Timer => "ms-clock:",
            Module::SettingsApp => "ms-settings:",
            Module::Airplane => "ms-settings:network-airplanemode",
            Module::NightLight => "ms-settings:nightlight",
            Module::Focus => "ms-settings:quiethours",
            Module::BatterySaver => "ms-settings:batterysaver",
            Module::TaskManager => "taskmgr.exe",
            Module::Explorer => "explorer.exe",
            Module::Hotspot => "ms-settings:network-mobilehotspot",
            _ => return None,
        })
    }
}

/// A module placed on the grid (cell coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    m: Module,
    col: usize,
    row: usize,
    w: usize,
    h: usize,
}

/// First-fit packing onto four columns: each module takes the first free spot, scanning rows
/// top to bottom and columns left to right. Returns the placements and the number of rows.
fn pack(order: &[Module]) -> (Vec<Placed>, usize) {
    let mut occ: Vec<[bool; 4]> = Vec::new();
    let mut out = Vec::with_capacity(order.len());
    let mut rows = 0;
    for &m in order {
        let (w, h) = m.span();
        let free = |occ: &Vec<[bool; 4]>, row: usize, col: usize| {
            (row..row + h).all(|r| (col..col + w).all(|c| occ.get(r).map_or(true, |line| !line[c])))
        };
        let mut row = 0;
        let (row, col) = loop {
            if let Some(col) = (0..=4 - w).find(|&col| free(&occ, row, col)) {
                break (row, col);
            }
            row += 1;
        };
        while occ.len() < row + h {
            occ.push([false; 4]);
        }
        for line in &mut occ[row..row + h] {
            for cell in &mut line[col..col + w] {
                *cell = true;
            }
        }
        rows = rows.max(row + h);
        out.push(Placed { m, col, row, w, h });
    }
    (out, rows)
}

/// A module being dragged in edit mode.
#[derive(Clone, Copy, Debug)]
struct Drag {
    m: Module,
    /// Pointer offset inside the module when it was picked up.
    off: (f32, f32),
    at: (f32, f32),
    start: (f32, f32),
    moved: bool,
    /// Module last swapped with (no re-trigger until the pointer leaves it).
    last: Option<Module>,
}

struct Edit {
    /// Order when edit mode began (Esc restores it).
    original: Vec<Module>,
    drag: Option<Drag>,
}

thread_local! {
    static ORDER: std::cell::RefCell<Vec<Module>> = std::cell::RefCell::new(Module::DEFAULT.to_vec());
    static EDIT: std::cell::RefCell<Option<Edit>> = const { std::cell::RefCell::new(None) };
    /// Vertical scroll of the panel content (DIPs) when it is taller than the window.
    static SCROLL: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    /// Tallest window the screen allows above the dock (DIPs).
    static MAX_H: std::cell::Cell<f32> = const { std::cell::Cell::new(f32::MAX) };
}

fn order() -> Vec<Module> {
    ORDER.with(|o| o.borrow().clone())
}

/// Loads the saved order (settings `ccModules`); None or an unreadable list gives the default.
/// Unknown ids are skipped and duplicates dropped.
pub fn set_modules(keys: Option<&[String]>) {
    let list = match keys {
        Some(keys) => {
            let mut v: Vec<Module> = Vec::new();
            for m in keys.iter().filter_map(|k| Module::from_key(k)) {
                if !v.contains(&m) {
                    v.push(m);
                }
            }
            if v.is_empty() && !keys.is_empty() { Module::DEFAULT.to_vec() } else { v }
        }
        None => Module::DEFAULT.to_vec(),
    };
    ORDER.with(|o| *o.borrow_mut() = list);
    SCROLL.with(|s| s.set(0.0));
}

/// The order to save: None while it is the default (settings stay clean).
pub fn module_keys() -> Option<Vec<String>> {
    let o = order();
    if o == Module::DEFAULT { None } else { Some(o.iter().map(|m| m.key().to_string()).collect()) }
}

pub fn editing() -> bool {
    EDIT.with(|e| e.borrow().is_some())
}

/// Caps the window height (DIPs); taller content scrolls.
pub fn set_max_height(h: f32) {
    MAX_H.with(|m| m.set(h.max(200.0)));
}

/// Enters edit mode (pill or right-click).
pub fn begin_edit() {
    if editing() {
        return;
    }
    hide_outputs();
    EDIT.with(|e| *e.borrow_mut() = Some(Edit { original: order(), drag: None }));
    SCROLL.with(|s| s.set(0.0));
}

/// Leaves edit mode keeping the changes ("Xong"). The caller saves `module_keys()`.
pub fn finish_edit() {
    EDIT.with(|e| *e.borrow_mut() = None);
    SCROLL.with(|s| s.set(0.0));
}

/// Leaves edit mode and restores the order it started with (Esc, or the popup closing).
pub fn cancel_edit() {
    if let Some(ed) = EDIT.with(|e| e.borrow_mut().take()) {
        ORDER.with(|o| *o.borrow_mut() = ed.original);
    }
    SCROLL.with(|s| s.set(0.0));
}

/// Applies an edit-mode action on the UI thread. True when `action` was one.
pub fn apply_edit(action: ControlAction) -> bool {
    match action {
        ControlAction::EnterEdit => begin_edit(),
        ControlAction::EditDone => finish_edit(),
        ControlAction::EditReset => ORDER.with(|o| *o.borrow_mut() = Module::DEFAULT.to_vec()),
        ControlAction::EditRemove(m) => ORDER.with(|o| o.borrow_mut().retain(|x| *x != m)),
        ControlAction::EditAdd(m) => ORDER.with(|o| {
            let mut o = o.borrow_mut();
            if !o.contains(&m) {
                o.push(m);
            }
        }),
        _ => return false,
    }
    true
}

const CHIP_H: f32 = 40.0;
const CHIP_GAP: f32 = 8.0;
const SECTION_H: f32 = 18.0;
const MINUS_R: f32 = 11.0;

/// Geometry of everything in the panel for the current order and mode, in window DIPs with the
/// scroll applied.
struct Grid {
    placed: Vec<Placed>,
    x0: f32,
    top: f32,
    unit: f32,
    /// Left and right bottom pills: "Sửa điều khiển" / "Cài đặt SmartDock", or in edit mode
    /// "Khôi phục mặc định" / "Xong".
    pills: (RectF, RectF),
    /// Edit mode: "Thêm điều khiển" heading and the add gallery chips.
    section: RectF,
    chips: Vec<(Module, RectF)>,
    /// Full content height (unscrolled window height that would show everything).
    content_h: f32,
}

fn grid_h(rows: usize, unit: f32) -> f32 {
    if rows == 0 { 0.0 } else { rows as f32 * unit + (rows as f32 - 1.0) * GAP }
}

fn grid_with(rect: RectF, scroll: f32) -> Grid {
    let order = order();
    let edit = editing();
    let (placed, rows) = pack(&order);
    let x0 = rect.x + PAD;
    let w = rect.w - 2.0 * PAD;
    let unit = (w - 3.0 * GAP) / 4.0;
    let top0 = rect.y + PAD;
    let top = top0 - scroll;
    // Edit mode keeps one row even when everything was removed (a drop target).
    let gh = grid_h(if edit { rows.max(1) } else { rows }, unit);
    let half = (w - GAP) / 2.0;
    let mut y = top + gh + if gh > 0.0 { GAP } else { 0.0 };
    let mut section = RectF::default();
    let mut chips = Vec::new();
    if edit {
        section = RectF { x: x0 + 2.0, y: y + 2.0, w: w - 4.0, h: SECTION_H };
        y = section.y + SECTION_H + 8.0;
        let free: Vec<Module> = Module::ALL.into_iter().filter(|m| !order.contains(m)).collect();
        for (i, m) in free.iter().enumerate() {
            let (c, r) = ((i % 2) as f32, (i / 2) as f32);
            chips.push((*m, RectF { x: x0 + c * (half + GAP), y: y + r * (CHIP_H + CHIP_GAP), w: half, h: CHIP_H }));
        }
        let n = free.len().div_ceil(2) as f32;
        y += if free.is_empty() { 20.0 } else { n * CHIP_H + (n - 1.0) * CHIP_GAP };
        y += GAP + 4.0;
    }
    let pills = (
        RectF { x: x0, y, w: half, h: SETTINGS_H },
        RectF { x: x0 + half + GAP, y, w: half, h: SETTINGS_H },
    );
    let content_h = (y + SETTINGS_H + scroll) - rect.y + PAD + 1.0;
    Grid { placed, x0, top, unit, pills, section, chips, content_h }
}

/// Current scroll, clamped to the content of a window `rect`.
fn scroll_for(rect: RectF) -> f32 {
    let content = grid_with(rect, 0.0).content_h;
    SCROLL.with(|s| s.get()).clamp(0.0, (content - (rect.h + 1.0)).max(0.0))
}

fn grid(rect: RectF) -> Grid {
    grid_with(rect, scroll_for(rect))
}

impl Grid {
    fn cell(&self, p: &Placed) -> RectF {
        let span = |n: usize| n as f32 * self.unit + (n as f32 - 1.0) * GAP;
        RectF {
            x: self.x0 + p.col as f32 * (self.unit + GAP),
            y: self.top + p.row as f32 * (self.unit + GAP),
            w: span(p.w),
            h: if p.m.span().0 == 4 { SLIDER_CARD_H } else { span(p.h) },
        }
    }

    fn rect_of(&self, m: Module) -> Option<RectF> {
        self.placed.iter().find(|p| p.m == m).map(|p| self.cell(p))
    }
}

/// Window height for the current order and mode (capped to the screen; the rest scrolls).
pub fn height() -> f32 {
    let nominal = RectF { x: 0.5, y: 0.5, w: WIDTH - 1.0, h: 10_000.0 };
    let content = grid_with(nominal, 0.0).content_h;
    content.min(MAX_H.with(|m| m.get())).round()
}

/// Scrolls the panel by `dy` DIPs (wheel). False when the content fits.
pub fn scroll_by(dy: f32, rect: RectF) -> bool {
    let content = grid_with(rect, 0.0).content_h;
    let max = (content - (rect.h + 1.0)).max(0.0);
    if max <= 0.0 {
        return false;
    }
    SCROLL.with(|s| s.set((s.get() + dy).clamp(0.0, max)));
    true
}

/// Centre of a module's "−" badge in edit mode.
fn minus_center(rc: RectF) -> (f32, f32) {
    (rc.x + 6.0, rc.y + 6.0)
}

fn on_minus(rc: RectF, x: f32, y: f32) -> bool {
    let (cx, cy) = minus_center(rc);
    (x - cx).hypot(y - cy) <= MINUS_R + 2.0
}

/// Edit-mode drag: picks a module up on mouse-down (not on its "−"), follows the pointer and
/// reorders live as it passes over other modules. Returns true while it owns the drag.
pub fn edit_drag(phase: DragPhaseLite, x: f32, y: f32, rect: RectF) -> bool {
    if !editing() {
        return false;
    }
    match phase {
        DragPhaseLite::Down => {
            let g = grid(rect);
            for p in &g.placed {
                let rc = g.cell(p);
                if on_minus(rc, x, y) {
                    return false;
                }
                if contains(rc, x, y) {
                    let d = Drag { m: p.m, off: (x - rc.x, y - rc.y), at: (x, y), start: (x, y), moved: false, last: None };
                    EDIT.with(|e| {
                        if let Some(ed) = e.borrow_mut().as_mut() {
                            ed.drag = Some(d);
                        }
                    });
                    return true;
                }
            }
            false
        }
        DragPhaseLite::Move => {
            let Some(mut d) = EDIT.with(|e| e.borrow().as_ref().and_then(|ed| ed.drag)) else { return false };
            d.at = (x, y);
            if (x - d.start.0).hypot(y - d.start.1) > 4.0 {
                d.moved = true;
            }
            if d.moved {
                let g = grid(rect);
                let target = g.placed.iter().find(|p| p.m != d.m && contains(g.cell(p), x, y)).map(|p| p.m);
                match target {
                    Some(t) if d.last != Some(t) => {
                        ORDER.with(|o| {
                            let mut o = o.borrow_mut();
                            if let (Some(from), Some(to)) = (o.iter().position(|m| *m == d.m), o.iter().position(|m| *m == t)) {
                                let m = o.remove(from);
                                o.insert(to, m);
                            }
                        });
                        d.last = Some(t);
                    }
                    Some(_) => {}
                    None => {
                        d.last = None;
                        // Below the last row of modules: move to the end.
                        let rows = g.placed.iter().map(|p| p.row + p.h).max().unwrap_or(0);
                        let bottom = g.top + grid_h(rows, g.unit);
                        if y > bottom && y < bottom + g.unit {
                            ORDER.with(|o| {
                                let mut o = o.borrow_mut();
                                if o.last() != Some(&d.m) {
                                    o.retain(|m| *m != d.m);
                                    o.push(d.m);
                                }
                            });
                        }
                    }
                }
            }
            EDIT.with(|e| {
                if let Some(ed) = e.borrow_mut().as_mut() {
                    ed.drag = Some(d);
                }
            });
            true
        }
        DragPhaseLite::Up => EDIT.with(|e| e.borrow_mut().as_mut().map(|ed| ed.drag.take().is_some()).unwrap_or(false)),
    }
}

/// Drag phases (mirrors `popup::DragPhase`, kept here so the panel stays UI-framework free).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragPhaseLite {
    Down,
    Move,
    Up,
}

/// `--render-cc` helper: picks `from` up and holds it over `to` (edit mode must be on).
pub fn preview_drag(rect: RectF, from: &str, to: &str) {
    let (Some(a), Some(b)) = (Module::from_key(from), Module::from_key(to)) else { return };
    let g = grid(rect);
    let (Some(ra), Some(rb)) = (g.rect_of(a), g.rect_of(b)) else { return };
    let (x0, y0) = (ra.x + ra.w / 2.0, ra.y + ra.h / 2.0);
    edit_drag(DragPhaseLite::Down, x0, y0, rect);
    edit_drag(DragPhaseLite::Move, rb.x + rb.w / 2.0, rb.y + rb.h / 2.0, rect);
    // Hold it slightly off-centre so the floating copy and the gap read clearly.
    edit_drag(DragPhaseLite::Move, rb.x + rb.w / 2.0 + 10.0, rb.y + rb.h / 2.0 + 8.0, rect);
}

fn dragging() -> Option<Drag> {
    EDIT.with(|e| e.borrow().as_ref().and_then(|ed| ed.drag))
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
    let on_color = ACCENT;
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
    // Over a bright page the active disc is system blue, so its glyph turns white.
    let on_color = if white_ink() { on_color } else { WHITE };
    draw_glyph(r, rect, glyph, size, fade(if on { on_color } else { text() }, icon_k))
}

/// Panel icons: the shared vector icon set (one grid, one stroke weight).
type Glyph = super::ccicons::Icon;

/// Draws `glyph` in a `size`-DIP icon box centred in `rect`. White ink gets the same very
/// subtle drop shadow as the text (macOS lifts white glyphs off the glass).
fn draw_glyph(r: &mut Renderer, rect: RectF, glyph: Glyph, size: f32, rgba: u32) -> Result<()> {
    if super::renderer::text_shadow_for(rgba) {
        let a = ((rgba & 0xFF) * 0x30 / 0xFF).max(1);
        super::ccicons::draw_in(r, glyph, RectF { y: rect.y + 0.6, ..rect }, size, a)?;
    }
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
    r.draw_text_styled(t.label, RectF { x: tx, y: title_y, w: tw, h: 18.0 }, TITLE_PT, fade(text(), k), Align::Left, TextStyle::SemiBold)?;
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
    r.draw_text_styled(title, head, TITLE_PT, text(), Align::Left, TextStyle::SemiBold)?;
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
    super::glass::update_control_ink(rect);
    super::renderer::set_text_shadow(white_ink());
    let result = draw_panel(r, rect, state, media);
    super::renderer::set_text_shadow(false);
    result
}

fn draw_panel(r: &mut Renderer, rect: RectF, state: &ControlState, media: Option<&Media>) -> Result<()> {
    if let Some(done) = OUTPUTS.with(|o| o.borrow().as_ref().map(|l| draw_outputs(r, rect, l))) {
        return done;
    }
    // One large blurred glass panel behind every module (macOS Control Center); the modules
    // keep their thicker glass on top.
    let panel = RectF { x: rect.x + EDGE, y: rect.y + EDGE, w: rect.w - 2.0 * EDGE, h: rect.h - 2.0 * EDGE };
    super::glass::draw_control_panel(r, panel, PANEL_R)?;
    let g = grid(rect);
    // Content taller than the window scrolls inside the panel.
    let clip = g.content_h > rect.h + 1.5;
    if clip {
        r.push_clip(panel)?;
    }
    let result = draw_content(r, rect, &g, state, media);
    if clip {
        let _ = r.pop_clip();
    }
    result
}

fn draw_content(r: &mut Renderer, rect: RectF, g: &Grid, state: &ControlState, media: Option<&Media>) -> Result<()> {
    let edit = editing();
    let drag = dragging().filter(|d| d.moved);
    for p in &g.placed {
        let rc = g.cell(p);
        if drag.is_some_and(|d| d.m == p.m) {
            // Where the dragged module will land: a faint well.
            let radius = module_radius(p.m, rc);
            r.fill_rounded_rect(rc, radius, if dark() { 0xFFFF_FF12 } else { 0x0000_000E })?;
            continue;
        }
        draw_module_at(r, p.m, rc, state, media)?;
        if edit {
            draw_minus(r, rc)?;
        }
    }
    if edit {
        if g.placed.is_empty() {
            let row = RectF { x: g.x0, y: g.top, w: rect.w - 2.0 * PAD, h: g.unit };
            r.fill_rounded_rect(row, TILE_R, if dark() { 0xFFFF_FF10 } else { 0x0000_000C })?;
            r.draw_text("Nhấn + bên dưới để thêm điều khiển", row, SUB_PT, sub(), Align::Center)?;
        }
        r.draw_text_styled("Thêm điều khiển", g.section, 13.0, text(), Align::Left, TextStyle::Bold)?;
        if g.chips.is_empty() {
            let note = RectF { y: g.section.y + SECTION_H + 8.0, h: 20.0, ..g.section };
            r.draw_text("Đã thêm tất cả điều khiển", note, SUB_PT, sub(), Align::Left)?;
        }
        for (m, chip) in &g.chips {
            draw_chip(r, *m, *chip)?;
        }
        let (reset, done) = g.pills;
        draw_module(r, reset, reset.h / 2.0)?;
        r.draw_text_styled("Khôi phục mặc định", reset, 12.5, text(), Align::Center, TextStyle::SemiBold)?;
        r.fill_rounded_rect(done, done.h / 2.0, ACCENT)?;
        r.draw_text_styled("Xong", done, 12.5, WHITE, Align::Center, TextStyle::Bold)?;
    } else {
        let (edit_pill, settings) = g.pills;
        draw_module(r, edit_pill, edit_pill.h / 2.0)?;
        r.draw_text_styled("Sửa điều khiển", edit_pill, 12.5, text(), Align::Center, TextStyle::SemiBold)?;
        // SmartDock Settings pill.
        draw_module(r, settings, settings.h / 2.0)?;
        r.draw_text_styled("Cài đặt SmartDock", settings, 12.5, text(), Align::Center, TextStyle::SemiBold)?;
    }
    // The dragged module floats under the pointer, on top of everything.
    if let Some(d) = drag {
        if let Some(p) = g.placed.iter().find(|p| p.m == d.m) {
            let home = g.cell(p);
            let rc = RectF { x: d.at.0 - d.off.0, y: d.at.1 - d.off.1, ..home };
            super::glass::draw_drag_shadow(r, rc, module_radius(d.m, rc))?;
            draw_module_at(r, d.m, rc, state, media)?;
            draw_minus(r, rc)?;
        }
    }
    Ok(())
}

fn module_radius(m: Module, rc: RectF) -> f32 {
    match m {
        Module::Media => CARD_R + 4.0,
        Module::Brightness | Module::Volume => CARD_R,
        _ => TILE_R.min(rc.h / 2.0),
    }
}

/// Edit-mode "−" badge on a module's top-left corner.
fn draw_minus(r: &mut Renderer, rc: RectF) -> Result<()> {
    let (cx, cy) = minus_center(rc);
    let disc = RectF { x: cx - MINUS_R, y: cy - MINUS_R, w: 2.0 * MINUS_R, h: 2.0 * MINUS_R };
    let (bg, ink) = if dark() { (0x5A5A_5EFF, WHITE) } else { (0xF2F2_F5FF, 0x1D1D_1FFF) };
    r.fill_rounded_rect(RectF { x: disc.x - 0.5, y: disc.y - 0.5, w: disc.w + 1.0, h: disc.h + 1.0 }, MINUS_R + 0.5, 0x0000_0030)?;
    r.fill_rounded_rect(disc, MINUS_R, bg)?;
    r.fill_rounded_rect(RectF { x: cx - 5.0, y: cy - 1.0, w: 10.0, h: 2.0 }, 1.0, ink)
}

/// Add-gallery chip: glyph, name and a blue "+".
fn draw_chip(r: &mut Renderer, m: Module, chip: RectF) -> Result<()> {
    draw_module(r, chip, chip.h / 2.0)?;
    let icon = RectF { x: chip.x + 6.0, y: chip.y, w: 24.0, h: chip.h };
    draw_glyph(r, icon, m.glyph(), 17.0, text())?;
    // Long names wrap onto a second line instead of being cut.
    let (x, w) = (chip.x + 31.0, chip.w - 31.0 - 28.0);
    let (pt, line) = (11.5, 13.5);
    match wrap_two(m.label(), w, pt) {
        (one, None) => r.draw_text_styled(&one, RectF { x, y: chip.y, w, h: chip.h }, pt, text(), Align::Left, TextStyle::SemiBold)?,
        (one, Some(two)) => {
            let y = chip.y + (chip.h - 2.0 * line) / 2.0;
            r.draw_text_styled(&one, RectF { x, y, w, h: line }, pt, text(), Align::Left, TextStyle::SemiBold)?;
            r.draw_text_styled(&two, RectF { x, y: y + line, w, h: line }, pt, text(), Align::Left, TextStyle::SemiBold)?;
        }
    }
    let plus = RectF { x: chip.x + chip.w - 25.0, y: chip.y + (chip.h - 19.0) / 2.0, w: 19.0, h: 19.0 };
    r.fill_rounded_rect(plus, 9.5, ACCENT)?;
    let (cx, cy) = (plus.x + 9.5, plus.y + 9.5);
    r.fill_rounded_rect(RectF { x: cx - 5.0, y: cy - 1.0, w: 10.0, h: 2.0 }, 1.0, WHITE)?;
    r.fill_rounded_rect(RectF { x: cx - 1.0, y: cy - 5.0, w: 2.0, h: 10.0 }, 1.0, WHITE)
}

/// Draws one module into its grid rect.
fn draw_module_at(r: &mut Renderer, m: Module, rc: RectF, state: &ControlState, media: Option<&Media>) -> Result<()> {
    match m {
        Module::Wifi => {
            // Wi-Fi tile: always the Wi-Fi radio, subtitled with the network name like macOS.
            let wifi_sub = match state.wifi {
                Some(true) => state.ssid.clone().unwrap_or_else(|| "Chưa kết nối".to_string()),
                Some(false) => "Tắt".to_string(),
                None => "Không có".to_string(),
            };
            draw_toggle(
                r,
                rc,
                Toggle {
                    // Signal strength while connected; a full fan otherwise.
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
            )
        }
        Module::Bluetooth => {
            let bt_k = if state.bluetooth.is_some() { 1.0 } else { 0.45 };
            draw_round(r, rc, Glyph::Bluetooth, 24.0, state.bluetooth == Some(true), bt_k, bt_k)
        }
        // Lights up while a wired adapter is connected.
        Module::Network => draw_round_with(r, rc, Glyph::Network, 24.0, state.ethernet, ACCENT, 1.0, 1.0),
        Module::Media => draw_now_playing(r, rc, media),
        Module::Dark => {
            draw_module(r, rc, TILE_R)?;
            let moon = round_of(rc);
            draw_round(r, moon, Glyph::Moon, 22.0, state.dark, 1.0, 1.0)?;
            let tx = moon.x + ROUND + 10.0;
            let label = RectF { x: tx, y: rc.y, w: (rc.x + rc.w) - tx - 4.0, h: rc.h };
            r.draw_text_styled("Chế độ tối", label, 12.5, text(), Align::Left, TextStyle::SemiBold)
        }
        Module::Sound => draw_pill_glyph(r, rc, speaker(1.0, false)),
        Module::Display => draw_pill_glyph(r, rc, Glyph::Display),
        Module::Brightness => {
            // Unsupported shows the note and a disabled, empty slider.
            let (br_frac, br_text) = match state.brightness {
                Some(p) => (p.min(100) as f32 / 100.0, format!("{}%", p.min(100))),
                None => (0.0, "Không hỗ trợ".to_string()),
            };
            draw_cap(r, rc, "Màn hình", &br_text)?;
            draw_capsule(r, slider_of(rc), br_frac, state.brightness.is_some(), Glyph::SunMin, Glyph::SunMax)
        }
        Module::Volume => {
            // The small icon at left toggles mute.
            let vol = state.volume.clamp(0.0, 1.0);
            let vol_text = if state.muted { "Đã tắt tiếng".to_string() } else { format!("{}%", (vol * 100.0).round() as u32) };
            draw_cap(r, rc, "Âm thanh", &vol_text)?;
            // "Âm thanh ›": the chevron sits right after the measured title.
            let head = head_of(rc);
            let title_w = r.measure_text("Âm thanh", TITLE_PT, TextStyle::SemiBold).unwrap_or(62.0);
            draw_glyph(r, RectF { x: head.x + title_w + 3.0, y: head.y, w: 12.0, h: head.h }, Glyph::Chevron, 12.0, sub())?;
            draw_capsule(r, slider_of(rc), vol, true, speaker(0.0, state.muted), speaker(1.0, false))
        }
        // Microphone: lit (blue) with a slashed glyph while muted.
        Module::Mic => {
            let muted = state.mic_muted == Some(true);
            let k = if state.mic_muted.is_some() { 1.0 } else { 0.45 };
            draw_round(r, rc, Glyph::Mic { muted }, 24.0, muted, k, k)
        }
        other => draw_round(r, rc, other.glyph(), 24.0, false, 1.0, 1.0),
    }
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
    r.draw_text_styled(title, RectF { x: card.x + 14.0, y: card.y + 60.0, w, h: 18.0 }, TITLE_PT, text(), Align::Left, TextStyle::SemiBold)?;
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

fn slider_card(slider: Slider, rect: RectF) -> Option<RectF> {
    if editing() {
        return None;
    }
    grid(rect).rect_of(match slider {
        Slider::Volume => Module::Volume,
        Slider::Brightness => Module::Brightness,
    })
}

/// Slider under (x, y) for a drag start (the volume glyph is the mute button, not the track).
pub fn slider_at(x: f32, y: f32, rect: RectF, state: &ControlState) -> Option<Slider> {
    if outputs_open() {
        return None;
    }
    if let Some(card) = slider_card(Slider::Volume, rect) {
        let vol = slider_of(card);
        if contains(vol, x, y) && !contains(icon_of(vol), x, y) {
            return Some(Slider::Volume);
        }
    }
    if let Some(card) = slider_card(Slider::Brightness, rect) {
        let br = slider_of(card);
        if state.brightness.is_some() && contains(br, x, y) && !contains(icon_of(br), x, y) {
            return Some(Slider::Brightness);
        }
    }
    None
}

/// Slider whose whole card (title row included) is under (x, y): the mouse wheel adjusts it.
pub fn slider_card_at(x: f32, y: f32, rect: RectF) -> Option<Slider> {
    [Slider::Volume, Slider::Brightness]
        .into_iter()
        .find(|s| slider_card(*s, rect).is_some_and(|c| contains(c, x, y)))
}

/// Fraction 0..1 for pointer x on `slider` (clamped, so dragging past the ends pins to 0/100%).
pub fn slider_value(slider: Slider, x: f32, rect: RectF) -> f32 {
    slider_card(slider, rect).map_or(0.0, |card| value_at(slider_of(card), x))
}

/// Maps a click at (x, y) to an action. None when the click hits nothing actionable.
pub fn hit_test(x: f32, y: f32, rect: RectF, state: &ControlState) -> Option<ControlAction> {
    if let Some(hit) = OUTPUTS.with(|o| {
        o.borrow().as_ref().map(|l| {
            // The header, or anywhere outside the sub-page card, goes back to the panel.
            if contains(outputs_back(rect), x, y) || !contains(outputs_card(rect), x, y) {
                return Some(ControlAction::CloseOutputs);
            }
            (0..l.len()).find(|&i| contains(output_row(rect, i), x, y)).map(ControlAction::SetOutput)
        })
    }) {
        return hit;
    }
    let g = grid(rect);
    if editing() {
        // Edit mode: modules don't act; only "−", the gallery and the two pills.
        if let Some(p) = g.placed.iter().find(|p| on_minus(g.cell(p), x, y)) {
            return Some(ControlAction::EditRemove(p.m));
        }
        if let Some((m, _)) = g.chips.iter().find(|(_, c)| contains(*c, x, y)) {
            return Some(ControlAction::EditAdd(*m));
        }
        if contains(g.pills.0, x, y) {
            return Some(ControlAction::EditReset);
        }
        if contains(g.pills.1, x, y) {
            return Some(ControlAction::EditDone);
        }
        return None;
    }
    if contains(g.pills.0, x, y) {
        return Some(ControlAction::EnterEdit);
    }
    if contains(g.pills.1, x, y) {
        return Some(ControlAction::OpenSmartDockSettings);
    }
    let p = g.placed.iter().find(|p| contains(g.cell(p), x, y))?;
    module_hit(p.m, g.cell(p), x, y, state)
}

fn module_hit(m: Module, rc: RectF, x: f32, y: f32, state: &ControlState) -> Option<ControlAction> {
    match m {
        // Only the round icon toggles the radio; the rest of the tile opens the network list.
        Module::Wifi if contains(round_of(rc), x, y) => state.wifi.map(|_| ControlAction::ToggleWifi),
        Module::Wifi => Some(ControlAction::OpenWifiList),
        Module::Bluetooth => state.bluetooth.map(|_| ControlAction::ToggleBluetooth),
        Module::Network => Some(ControlAction::OpenNetworkSettings),
        Module::Media => media_buttons(rc)
            .into_iter()
            .zip([ControlAction::MediaPrevious, ControlAction::MediaPlayPause, ControlAction::MediaNext])
            .find(|(b, _)| contains(*b, x, y))
            .map(|(_, a)| a),
        Module::Dark => Some(ControlAction::ToggleDark),
        Module::Sound => Some(ControlAction::PickOutput),
        Module::Display => Some(ControlAction::Launch("ms-settings:display")),
        Module::Brightness => {
            let br = slider_of(rc);
            (state.brightness.is_some() && contains(br, x, y))
                .then(|| ControlAction::SetBrightness((value_at(br, x) * 100.0).round() as u32))
        }
        Module::Volume => {
            // The "Âm thanh ›" title row opens the output device list.
            let vol = slider_of(rc);
            if contains(head_of(rc), x, y) {
                Some(ControlAction::PickOutput)
            } else if contains(icon_of(vol), x, y) {
                Some(ControlAction::ToggleMute)
            } else if contains(vol, x, y) {
                Some(ControlAction::SetVolume(value_at(vol, x)))
            } else {
                None
            }
        }
        Module::Mic => state.mic_muted.map(|_| ControlAction::ToggleMic),
        Module::Lock => Some(ControlAction::LockScreen),
        Module::Sleep => Some(ControlAction::SleepNow),
        Module::Clipboard => Some(ControlAction::ClipboardHistory),
        other => other.launch().map(ControlAction::Launch),
    }
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

    fn reset() {
        set_modules(None);
        EDIT.with(|e| *e.borrow_mut() = None);
        MAX_H.with(|m| m.set(f32::MAX));
    }

    fn at(m: Module) -> RectF {
        grid(nominal_rect()).rect_of(m).unwrap()
    }

    fn center(r: RectF) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn default_packing_reproduces_the_classic_layout() {
        reset();
        let rect = nominal_rect();
        close(height(), HEIGHT);
        // The fixed layout before Edit Controls.
        let x0 = rect.x + PAD;
        let col = (rect.w - 2.0 * PAD - 3.0 * GAP) / 4.0;
        close(col, 60.0);
        let x = |c: f32| x0 + c * (col + GAP);
        let y1 = rect.y + PAD;
        let y1b = y1 + col + GAP;
        let y2 = y1b + col + GAP;
        let y3 = y2 + col + GAP;
        let y4 = y3 + SLIDER_CARD_H + GAP;
        let y5 = y4 + SLIDER_CARD_H + GAP;
        let d = 2.0 * col + GAP;
        let full = rect.w - 2.0 * PAD;
        let expect = [
            (Module::Wifi, RectF { x: x(0.0), y: y1, w: d, h: col }),
            (Module::Bluetooth, RectF { x: x(0.0), y: y1b, w: col, h: col }),
            (Module::Network, RectF { x: x(1.0), y: y1b, w: col, h: col }),
            (Module::Media, RectF { x: x(2.0), y: y1, w: d, h: d }),
            (Module::Dark, RectF { x: x(0.0), y: y2, w: d, h: col }),
            (Module::Sound, RectF { x: x(2.0), y: y2, w: col, h: col }),
            (Module::Display, RectF { x: x(3.0), y: y2, w: col, h: col }),
            (Module::Brightness, RectF { x: x0, y: y3, w: full, h: SLIDER_CARD_H }),
            (Module::Volume, RectF { x: x0, y: y4, w: full, h: SLIDER_CARD_H }),
            (Module::Calculator, RectF { x: x(0.0), y: y5, w: col, h: col }),
            (Module::Snipping, RectF { x: x(1.0), y: y5, w: col, h: col }),
            (Module::Timer, RectF { x: x(2.0), y: y5, w: col, h: col }),
            (Module::SettingsApp, RectF { x: x(3.0), y: y5, w: col, h: col }),
        ];
        for (m, e) in expect {
            let a = at(m);
            close(a.x, e.x);
            close(a.y, e.y);
            close(a.w, e.w);
            close(a.h, e.h);
        }
        let g = grid(rect);
        close(g.pills.0.y, y5 + col + GAP);
        assert!(g.pills.1.y + g.pills.1.h <= rect.y + rect.h);
        assert_eq!(module_keys(), None);
    }

    #[test]
    fn hit_testing_tracks_the_module_rects() {
        reset();
        let rect = nominal_rect();
        let state = ControlState { wifi: Some(true), bluetooth: Some(true), brightness: Some(75), ..Default::default() };
        let (x, y) = center(at(Module::Bluetooth));
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::ToggleBluetooth)));
        let (x, y) = center(at(Module::Network));
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::OpenNetworkSettings)));
        let (x, y) = center(at(Module::Dark));
        assert!(matches!(hit_test(x, y, rect, &state), Some(ControlAction::ToggleDark)));
        let (x, y) = center(at(Module::Snipping));
        assert_eq!(hit_test(x, y, rect, &state), Some(ControlAction::Launch("ms-screenclip:")));
        let g = grid(rect);
        let (x, y) = center(g.pills.1);
        assert_eq!(hit_test(x, y, rect, &state), Some(ControlAction::OpenSmartDockSettings));
        let (x, y) = center(g.pills.0);
        assert_eq!(hit_test(x, y, rect, &state), Some(ControlAction::EnterEdit));
    }

    #[test]
    fn edit_mode_removes_adds_resets_and_cancels() {
        reset();
        let rect = nominal_rect();
        let state = ControlState::default();
        begin_edit();
        // Modules don't act; their "−" removes them.
        let (x, y) = center(at(Module::Dark));
        assert_eq!(hit_test(x, y, rect, &state), None);
        let (cx, cy) = minus_center(at(Module::Media));
        let hit = hit_test(cx, cy, rect, &state).unwrap();
        assert_eq!(hit, ControlAction::EditRemove(Module::Media));
        assert!(apply_edit(hit));
        assert!(grid(rect).rect_of(Module::Media).is_none());
        // The gallery offers it back (plus every new control).
        let g = grid_with(RectF { h: 2000.0, ..rect }, 0.0);
        assert_eq!(g.chips.len(), Module::ALL.len() - Module::DEFAULT.len() + 1);
        let (m, chip) = g.chips.iter().find(|(m, _)| *m == Module::Lock).copied().unwrap();
        let tall = RectF { h: 2000.0, ..rect };
        assert_eq!(hit_test(chip.x + 5.0, chip.y + 5.0, tall, &state), Some(ControlAction::EditAdd(m)));
        apply_edit(ControlAction::EditAdd(Module::Lock));
        assert_eq!(order().last(), Some(&Module::Lock));
        // Esc restores the order edit mode started with.
        cancel_edit();
        assert_eq!(order(), Module::DEFAULT.to_vec());
        // Done keeps the edit; reset goes back to the default.
        begin_edit();
        apply_edit(ControlAction::EditRemove(Module::Timer));
        apply_edit(ControlAction::EditDone);
        assert!(!editing());
        assert!(module_keys().unwrap().iter().all(|k| k != "timer"));
        begin_edit();
        apply_edit(ControlAction::EditReset);
        apply_edit(ControlAction::EditDone);
        assert_eq!(module_keys(), None);
    }

    #[test]
    fn dragging_reorders_live() {
        reset();
        let rect = nominal_rect();
        begin_edit();
        let (x0, y0) = center(at(Module::Calculator));
        assert!(edit_drag(DragPhaseLite::Down, x0, y0, rect));
        let (x1, y1) = center(at(Module::Timer));
        assert!(edit_drag(DragPhaseLite::Move, x1, y1, rect));
        assert!(edit_drag(DragPhaseLite::Up, x1, y1, rect));
        let o = order();
        let pos = |m| o.iter().position(|x| *x == m).unwrap();
        assert!(pos(Module::Calculator) > pos(Module::Timer));
        cancel_edit();
        assert_eq!(order(), Module::DEFAULT.to_vec());
    }

    #[test]
    fn saved_layouts_load_skip_unknown_ids_and_scroll_when_tall() {
        reset();
        let keys: Vec<String> = ["mic", "wifi", "bogus", "mic", "lock", "volume"].iter().map(|s| s.to_string()).collect();
        set_modules(Some(&keys));
        assert_eq!(order(), vec![Module::Mic, Module::Wifi, Module::Lock, Module::Volume]);
        // mic | wifi wifi | lock  /  volume
        let g = grid(nominal_rect());
        let mic = g.rect_of(Module::Mic).unwrap();
        let lock = g.rect_of(Module::Lock).unwrap();
        close(lock.y, mic.y);
        assert!(height() < HEIGHT);
        // Everything: taller than a small screen, so it is capped and scrolls.
        let all: Vec<String> = Module::ALL.iter().map(|m| m.key().to_string()).collect();
        set_modules(Some(&all));
        set_max_height(500.0);
        let h = height();
        close(h, 500.0);
        let rect = RectF { x: 0.5, y: 0.5, w: WIDTH - 1.0, h: h - 1.0 };
        assert!(scroll_by(40.0, rect));
        close(scroll_for(rect), 40.0);
        reset();
    }

    #[test]
    fn wifi_subtitle_wraps_long_ssids_onto_two_lines() {
        assert_eq!(wrap_two("BnK_5G", 70.0, 12.5), ("BnK_5G".to_string(), None));
        let (a, b) = wrap_two("Slice Of Heaven Guest", 70.0, 12.5);
        assert_eq!(a, "Slice Of");
        assert_eq!(b.as_deref(), Some("Heaven Guest"));
    }
}
