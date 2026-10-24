//! Popup body drawing per kind, matching the old Electron popups (Popup.tsx, GitTile, AiTiles,
//! ClockTile, MusicTile, SystemTiles, styles.css `.popup*`). Pure drawing: the caller supplies data and the rect.
//! Layout runs through `Ctx`, either drawing (`Some(renderer)`) or dry (`None`, used by `content_height`),
//! so the window height and the drawn content always agree.
use std::cell::{Cell, RefCell};

use chrono::{Datelike, Local, NaiveDate};
use windows::core::Result;

use super::dock::RectF;
use super::renderer::{Align, Renderer, TextStyle};
use super::svgpath;
use crate::data::git::GitSummary;
use crate::data::weather::Weather;
use crate::model::{AiUsage, GitDay, Media};

pub use super::tiles::DataState;

/// Popup client width minus the 1px border (popup.rs uses 320 px).
const WIDTH: f32 = 319.0;
const PAD_X: f32 = 14.0;
const HEAD_H: f32 = 47.0; // header incl. its 1px divider
const SEC_TITLE_H: f32 = 29.0; // psection-title: 10 top, 13 text, 6 bottom
const SEC_END_H: f32 = 5.0; // psection: 4 bottom padding, 1px divider
const ROW_H: f32 = 29.0; // prow without bar: 7 + 15 + 7
const ROW_BAR_H: f32 = 39.0; // prow with bar: 7 + 15 + 6 + 4 + 7
const VALUE_W: f32 = 48.0;
const BAR_FRAC: f32 = 0.6; // .pbar is 60% wide
const HM_CELL: f32 = 13.0;
const HM_GAP: f32 = 1.5;
const HM_COLS: usize = 17;
const HM_ROWS: usize = 7;
const HM_H: f32 = HM_ROWS as f32 * HM_CELL + (HM_ROWS - 1) as f32 * HM_GAP;
const SPARK_H: f32 = 40.0;
const MEDIA_SIDE_D: f32 = 40.0;
const MEDIA_PLAY_D: f32 = 52.0;
const MEDIA_GAP: f32 = 18.0;
/// Play button top, below the top of the body (fixed, so hit testing needs no data).
const MEDIA_PLAY_Y: f32 = 148.0;
const WAVE_BARS: usize = 38;
const WAVE_H: f32 = 44.0;
const WAVE_BAR_W: f32 = 4.0;
const WAVE_FROM: u32 = 0xA78B_FAFF;
const WAVE_TO: u32 = 0xF472_B6FF;

const TEXT: u32 = 0xFFFF_FFE6;
const MUTED: u32 = 0xFFFF_FF8C; // rgba(255,255,255,.55)
const FAINT: u32 = 0xFFFF_FF66; // weekday headers of the month grid
const EMPTY_TEXT: u32 = 0xFFFF_FF99;
const TRACK: u32 = 0xFFFF_FF14; // bar track, .08
const STRIPE: u32 = 0xFFFF_FF06; // even rows, .025
const DIVIDER: u32 = 0xFFFF_FF0F; // psection border, .06
const HEAD_DIVIDER: u32 = 0xFFFF_FF12; // phead border, .07
const BADGE_BG: u32 = 0xFFFF_FF1A;
const BADGE_TEXT: u32 = 0xFFFF_FFBF;
const WHITE: u32 = 0xFFFF_FFFF;
const BTN: u32 = 0xFFFF_FF14; // .mctl span, .08
const BTN_BIG: u32 = 0xFFFF_FFEB; // .mctl span.big, .92
const BTN_BIG_TEXT: u32 = 0x1111_11FF;
const TODAY: u32 = 0xE832_1EFF; // .month .today
const GREEN: u32 = 0x3FD2_7FFF;
const ORANGE: u32 = 0xFFB3_40FF;
const RED: u32 = 0xFF5D_5DFF;
const BLUE: u32 = 0x4AA8_FFFF;
/// Heatmap levels: empty, then 4 greens (.hm.l0..l4).
const HEAT: [u32; 5] = [0xFFFF_FF14, 0x0E44_29FF, 0x006D_32FF, 0x26A6_41FF, 0x39D3_53FF];

const PREV: &str = "\u{E892}";
const NEXT: &str = "\u{E893}";
const PLAY: &str = "\u{E768}";
const PAUSE: &str = "\u{E769}";

const WEEKDAYS: [&str; 7] = ["T2", "T3", "T4", "T5", "T6", "T7", "CN"];
const AI_NOTE: &str = "Đọc từ đăng nhập cục bộ của bạn trên máy này; token chỉ gửi tới endpoint của chính nhà cung cấp. Claude: tối đa 15 phút/lần do giới hạn API.";
const OPEN_METEO: &str = "Thời tiết: Open-Meteo (không cần key).";
const LOCAL_NOTE: &str = "Đo cục bộ mỗi 2 giây.";

const SETTING_TILES: [(&str, &str); 9] = [
    ("git", "Git"),
    ("claude", "Claude"),
    ("codex", "ChatGPT"),
    ("cursor", "Cursor"),
    ("grok", "Grok Bot"),
    ("music", "Media"),
    ("cpu", "CPU"),
    ("ram", "RAM"),
    ("weather", "Thời tiết"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupKind {
    Git,
    Ai,
    Weather,
    Media,
    Clock,
    Cpu,
    Ram,
    Settings,
    Todo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAction {
    Previous,
    PlayPause,
    Next,
}

#[derive(Clone, Debug)]
pub enum SettingsAction {
    GlassOpacity(u8),
    HiddenTaskbar(bool),
    ToggleWidget(&'static str),
    ToggleWeatherPosition,
    ToggleStartup,
    OpenConfig,
    ToggleAutoPin,
    CycleAutoPinCount,
    ToggleClaudeSource,
    RefreshUsage,
    CycleClaudeBudget,
}


thread_local! {
    static CONTENT: RefCell<Option<(PopupKind, DataState)>> = const { RefCell::new(None) };
    static AI_PROVIDER: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    /// Sample history per kind (Cpu, Ram), percent 0..=100, oldest first.
    static HISTORY: RefCell<Vec<(PopupKind, Vec<f32>)>> = const { RefCell::new(Vec::new()) };
    static SETTINGS_STATE: RefCell<crate::settings::Settings> = RefCell::new(crate::settings::Settings::default());
}

pub fn set_ai_provider(provider: Option<&'static str>) {
    AI_PROVIDER.with(|p| *p.borrow_mut() = provider);
}

/// Stores the samples for a sparkline (Cpu or Ram). Call `popup::invalidate` after updating while open.
pub fn set_history(kind: PopupKind, samples: Vec<f32>) {
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        match h.iter().position(|(k, _)| *k == kind) {
            Some(i) => h[i].1 = samples,
            None => h.push((kind, samples)),
        }
    });
}

/// Stores what `paint_content` draws. Call before `popup::show` and after each data refresh.
pub fn set_content(kind: PopupKind, data: DataState) {
    CONTENT.with(|c| *c.borrow_mut() = Some((kind, data)));
}

pub fn set_settings(settings: crate::settings::Settings) {
    SETTINGS_STATE.with(|state| *state.borrow_mut() = settings);
}

/// Painter with the signature `popup::set_painter` expects. Draws the content stored by `set_content`.
pub fn paint_content(r: &mut Renderer, rect: RectF) {
    CONTENT.with(|c| {
        if let Some((kind, data)) = c.borrow().as_ref() {
            // Errors are per draw call; a failed frame is retried on the next WM_PAINT.
            let _ = draw_popup(r, *kind, rect, data);
        }
    });
}

/// Draws the whole popup (header, body, footer) into `rect`. Call between `Renderer::begin` and `end`.
pub fn draw_popup(r: &mut Renderer, kind: PopupKind, rect: RectF, data: &DataState) -> Result<()> {
    let mut c = Ctx { r: Some(r), rect };
    draw_all(&mut c, kind, data).map(|_| ())
}

/// Total popup height for this content, in DIPs. Use it as the window height (and in the click hit test).
pub fn content_height(kind: PopupKind, data: &DataState) -> f32 {
    let width = if kind == PopupKind::Settings { 459.0 } else { WIDTH };
    let mut c = Ctx { r: None, rect: RectF { x: 0.0, y: 0.0, w: width, h: 0.0 } };
    draw_all(&mut c, kind, data).unwrap_or(0.0)
}

pub fn hit_settings(x: f32, y: f32, rect: RectF) -> Option<SettingsAction> {
    let top = rect.y + HEAD_H;
    let inner_x = rect.x + PAD_X;
    let inner_w = rect.w - PAD_X * 2.0;
    let slider = rect_at(inner_x, top + 33.0, inner_w, 18.0);
    if contains(slider, x, y) {
        let pct = ((x - slider.x) / slider.w * 95.0).round().clamp(0.0, 95.0) as u8;
        return Some(SettingsAction::GlassOpacity(pct));
    }
    let taskbar = rect_at(inner_x, top + 76.0, inner_w, 30.0);
    if contains(taskbar, x, y) {
        return Some(SettingsAction::HiddenTaskbar(x >= taskbar.x + taskbar.w / 2.0));
    }
    let grid_y = top + 137.0;
    let cell_w = (inner_w - 8.0) / 3.0;
    for (index, (id, _)) in SETTING_TILES.iter().enumerate() {
        let cell = rect_at(
            inner_x + (index % 3) as f32 * (cell_w + 4.0),
            grid_y + (index / 3) as f32 * 31.0,
            cell_w,
            27.0,
        );
        if contains(cell, x, y) {
            return Some(SettingsAction::ToggleWidget(id));
        }
    }
    if contains(rect_at(inner_x, top + 257.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::ToggleWeatherPosition);
    }
    if contains(rect_at(inner_x, top + 295.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::OpenConfig);
    }
    if contains(rect_at(inner_x, top + 333.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::OpenConfig);
    }
    if contains(rect_at(inner_x, top + 371.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::ToggleStartup);
    }
    let auto = rect_at(inner_x, top + 447.0, inner_w, 30.0);
    if contains(auto, x, y) {
        // The count chip (left of the switch) cycles the count; anywhere else toggles on/off.
        let chip = autopin_chip(auto);
        let on = SETTINGS_AUTO_ON.with(Cell::get);
        return Some(if on && contains(chip, x, y) { SettingsAction::CycleAutoPinCount } else { SettingsAction::ToggleAutoPin });
    }
    if contains(rect_at(inner_x, top + 485.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::ToggleClaudeSource);
    }
    if contains(rect_at(inner_x, top + 523.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::RefreshUsage);
    }
    if contains(rect_at(inner_x, top + 561.0, inner_w, 30.0), x, y) {
        return Some(SettingsAction::CycleClaudeBudget);
    }
    None
}

fn contains(rect: RectF, x: f32, y: f32) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

/// Hit test for the media buttons. `rect` must match the one passed to `draw_popup`.
pub fn hit_media_button(x: f32, y: f32, rect: RectF) -> Option<MediaAction> {
    let [prev, play, next] = media_buttons(rect);
    let actions = [(prev, MediaAction::Previous), (play, MediaAction::PlayPause), (next, MediaAction::Next)];
    actions
        .into_iter()
        .find(|(b, _)| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
        .map(|(_, a)| a)
}

// ---------------------------------------------------------------- drawing context

/// Drawing target. With `r == None` it only advances the layout (dry run for `content_height`).
struct Ctx<'a> {
    r: Option<&'a mut Renderer>,
    rect: RectF,
}

#[derive(Clone, Copy)]
struct Font {
    size: f32,
    rgba: u32,
    align: Align,
    style: TextStyle,
}

fn font(size: f32, rgba: u32) -> Font {
    Font { size, rgba, align: Align::Left, style: TextStyle::Ui }
}

impl Font {
    fn bold(self) -> Self {
        Font { style: TextStyle::Bold, ..self }
    }
    fn center(self) -> Self {
        Font { align: Align::Center, ..self }
    }
    fn right(self) -> Self {
        Font { align: Align::Right, ..self }
    }
    fn icon(self) -> Self {
        Font { style: TextStyle::Icon, ..self }
    }
}

fn rect_at(x: f32, y: f32, w: f32, h: f32) -> RectF {
    RectF { x, y, w, h }
}

impl Ctx<'_> {
    fn inner_x(&self) -> f32 {
        self.rect.x + PAD_X
    }

    fn inner_w(&self) -> f32 {
        self.rect.w - 2.0 * PAD_X
    }

    fn fill(&mut self, area: RectF, rgba: u32) -> Result<()> {
        match self.r.as_deref_mut() {
            Some(r) => r.fill_rect(area, rgba),
            None => Ok(()),
        }
    }

    fn round(&mut self, area: RectF, radius: f32, rgba: u32) -> Result<()> {
        match self.r.as_deref_mut() {
            Some(r) => r.fill_rounded_rect(area, radius, rgba),
            None => Ok(()),
        }
    }

    fn text(&mut self, s: &str, area: RectF, f: Font) -> Result<()> {
        match self.r.as_deref_mut() {
            Some(r) => r.draw_text_styled(s, area, f.size, f.rgba, f.align, f.style),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------- shared pieces

struct Meta {
    glyph: &'static str,
    color: u32,
    title: &'static str,
    badge: Option<String>,
    note: &'static str,
}

fn meta(kind: PopupKind) -> Meta {
    match kind {
        PopupKind::Git => Meta {
            glyph: "⎇",
            color: 0x2EA0_43FF,
            title: "Git activity",
            badge: None,
            note: "Đếm commit theo git user.email (hoặc Git / email trong Cài đặt); tự tìm repo nếu chưa cấu hình thư mục.",
        },
        PopupKind::Ai => Meta { glyph: "✦", color: 0xD977_57FF, title: "AI usage", badge: None, note: AI_NOTE },
        PopupKind::Weather => Meta { glyph: "☀", color: 0xF5A5_24FF, title: "Thời tiết", badge: None, note: "Open-Meteo." },
        PopupKind::Media => Meta {
            glyph: "♪",
            color: 0xE548_4DFF,
            title: "Đang phát",
            badge: None,
            note: "Từ Windows Media Controls (Spotify, trình duyệt, …).",
        },
        PopupKind::Clock => {
            let t = Local::now().date_naive();
            Meta {
                glyph: "◷",
                color: 0xE54D_2EFF,
                title: "Hôm nay",
                badge: Some(format!("{:02}/{:02}/{}", t.day(), t.month(), t.year())),
                note: OPEN_METEO,
            }
        }
        PopupKind::Cpu => Meta { glyph: "◔", color: 0x30A4_6CFF, title: "CPU", badge: None, note: LOCAL_NOTE },
        PopupKind::Ram => Meta { glyph: "◔", color: 0x3E63_DDFF, title: "RAM", badge: None, note: LOCAL_NOTE },
        PopupKind::Settings => Meta { glyph: "⚙", color: 0x6B6B_75FF, title: "Cài đặt SmartDock", badge: None, note: "" },
        PopupKind::Todo => Meta {
            glyph: "✓",
            color: 0x3FD2_7FFF,
            title: "Ghi chú",
            badge: None,
            note: "Bấm vào ghi chú để đánh dấu, bấm dấu × để xoá.",
        },
    }
}

/// Provider logo on its own tile background, the same artwork as the dock tile. False for an unknown provider.
fn ai_logo(c: &mut Ctx<'_>, x: f32, y: f32, provider: &str) -> Result<bool> {
    let bg = match provider {
        "Claude" => 0xD977_57FF,
        "Codex" => 0x0D0D_0DFF,
        "Cursor" => 0x1414_14FF,
        "Grok" => 0x0000_00FF,
        _ => return Ok(false),
    };
    c.round(rect_at(x, y, 24.0, 24.0), 7.0, bg)?;
    let Some(r) = c.r.as_deref_mut() else { return Ok(true) };
    let icon = rect_at(x + 4.0, y + 4.0, 16.0, 16.0);
    match provider {
        "Claude" => svgpath::draw_path(r, svgpath::CLAUDE_SPARK, icon, 24.0, 0xFFFF_FFFF, None)?,
        "Codex" => svgpath::draw_path(r, svgpath::CODEX_ORBITS, icon, 24.0, 0xFFFF_FFFF, Some(1.7))?,
        "Grok" => svgpath::draw_path(r, svgpath::GROK_SLASH, icon, 24.0, 0xFFFF_FFFF, Some(2.0))?,
        _ => {
            svgpath::draw_path(r, svgpath::CURSOR_CUBE, icon, 24.0, 0xE8E8_E8FF, Some(1.5))?;
            svgpath::draw_path(r, svgpath::CURSOR_CUBE_INNER, icon, 24.0, 0xE8E8_E8FF, Some(1.2))?;
            svgpath::draw_path(r, svgpath::CURSOR_CUBE_TOP, icon, 24.0, 0xE8E8_E8D9, None)?;
        }
    }
    Ok(true)
}

/// .phead: 24px colored icon, 13.5px title, optional badge at the right edge.
/// The AI popup shows the selected provider's logo instead of the generic glyph.
fn header(c: &mut Ctx<'_>, m: &Meta, provider: Option<&str>) -> Result<()> {
    let (x, y) = (c.rect.x + PAD_X, c.rect.y + 12.0);
    let logo_drawn = match provider {
        Some(name) => ai_logo(c, x, y, name)?,
        None => false,
    };
    if !logo_drawn {
        c.round(rect_at(x, y, 24.0, 24.0), 7.0, m.color)?;
        c.text(m.glyph, rect_at(x, y, 24.0, 24.0), font(13.0, WHITE).center())?;
    }

    // Badge width is estimated: the renderer cannot measure text.
    let badge_w = m.badge.as_ref().map(|b| b.chars().count() as f32 * 5.4 + 14.0);
    let title_w = c.rect.w - 47.0 - PAD_X - badge_w.unwrap_or(0.0);
    c.text(m.title, rect_at(c.rect.x + 47.0, y, title_w, 24.0), font(13.5, TEXT).bold())?;
    if let (Some(b), Some(bw)) = (&m.badge, badge_w) {
        let bx = c.rect.x + c.rect.w - PAD_X - bw;
        c.round(rect_at(bx, y + 4.0, bw, 16.0), 6.0, BADGE_BG)?;
        c.text(b, rect_at(bx, y + 4.0, bw, 16.0), font(10.0, BADGE_TEXT).center())?;
    }
    c.fill(rect_at(c.rect.x, c.rect.y + HEAD_H - 1.0, c.rect.w, 1.0), HEAD_DIVIDER)
}

/// .pfoot: wrapped note text. Returns the bottom of the popup.
fn footer(c: &mut Ctx<'_>, y: f32, note: &str) -> Result<f32> {
    if note.is_empty() {
        return Ok(y);
    }
    let max_chars = (c.inner_w() / 5.0) as usize;
    let top = y + 10.0;
    let lines = wrap(note, max_chars);
    for (i, line) in lines.iter().enumerate() {
        c.text(line, rect_at(c.inner_x(), top + i as f32 * 13.0, c.inner_w(), 13.0), font(9.5, MUTED))?;
    }
    Ok(top + lines.len() as f32 * 13.0 + 12.0)
}

/// Greedy word wrap by character count.
fn wrap(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max_chars {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// .psection-title. Returns the y where the section content starts.
fn section(c: &mut Ctx<'_>, y: f32, title: &str) -> Result<f32> {
    c.text(title, rect_at(c.inner_x(), y + 10.0, c.inner_w(), 13.0), font(9.5, MUTED).bold())?;
    Ok(y + SEC_TITLE_H)
}

/// Bottom padding and divider of a section. Returns the y after the section.
fn section_end(c: &mut Ctx<'_>, y: f32) -> Result<f32> {
    c.fill(rect_at(c.rect.x, y + 4.0, c.rect.w, 1.0), DIVIDER)?;
    Ok(y + SEC_END_H)
}

/// .pempty: centered-left message block with no section.
fn empty(c: &mut Ctx<'_>, y: f32, text: &str) -> Result<f32> {
    c.text(text, rect_at(c.inner_x(), y + 16.0, c.inner_w(), 16.0), font(11.0, EMPTY_TEXT))?;
    Ok(y + 48.0)
}

/// .prow: label left, optional hint right of it, bold value at the right, optional 60% bar below.
/// Even rows get the zebra stripe, like `.prow:nth-child(even)`.
fn row(
    c: &mut Ctx<'_>,
    y: f32,
    i: usize,
    label: &str,
    hint: Option<&str>,
    value: Option<&str>,
    bar: Option<(f32, u32)>,
) -> Result<f32> {
    let h = if bar.is_some() { ROW_BAR_H } else { ROW_H };
    if i % 2 == 0 {
        c.fill(rect_at(c.rect.x, y, c.rect.w, h), STRIPE)?;
    }
    let (x, w) = (c.inner_x(), c.inner_w());
    c.text(label, rect_at(x, y + 7.0, w * 0.5, 15.0), font(12.5, TEXT))?;
    if let Some(hint) = hint {
        let hint_w = if value.is_some() { w - VALUE_W } else { w };
        c.text(hint, rect_at(x, y + 7.0, hint_w, 15.0), font(10.5, MUTED).right())?;
    }
    if let Some(v) = value {
        c.text(v, rect_at(x + w - VALUE_W, y + 7.0, VALUE_W, 15.0), font(12.5, TEXT).bold().right())?;
    }
    if let Some((frac, color)) = bar {
        let track = rect_at(x, y + 28.0, w * BAR_FRAC, 4.0);
        c.round(track, 2.0, TRACK)?;
        let fill_w = track.w * frac.clamp(0.0, 1.0);
        if fill_w > 0.0 {
            c.round(rect_at(track.x, track.y, fill_w, 4.0), 2.0, color)?;
        }
    }
    Ok(y + h)
}

/// Thresholds of the system panels (pctColor in common.tsx).
fn pct_color(v: f32) -> u32 {
    if v > 85.0 {
        RED
    } else if v > 65.0 {
        ORANGE
    } else {
        GREEN
    }
}

/// Thresholds of the AI usage bars (barColor in AiTiles.tsx).
fn ai_color(v: f32) -> u32 {
    if v >= 90.0 {
        RED
    } else if v >= 70.0 {
        ORANGE
    } else {
        GREEN
    }
}

// ---------------------------------------------------------------- kinds

fn draw_all(c: &mut Ctx<'_>, kind: PopupKind, data: &DataState) -> Result<f32> {
    let m = meta(kind);
    let provider = match kind {
        PopupKind::Ai => AI_PROVIDER.with(|p| *p.borrow()),
        _ => None,
    };
    header(c, &m, provider)?;
    let top = c.rect.y + HEAD_H;
    let hist = history(kind);
    let y = match kind {
        PopupKind::Git => draw_git(c, top, data.git.as_ref())?,
        PopupKind::Ai => draw_ai(c, top, &data.ai)?,
        PopupKind::Weather => draw_weather(c, top, data.weather.as_ref())?,
        PopupKind::Media => draw_media(c, top, data.media.as_ref())?,
        PopupKind::Clock => {
            let y = draw_month(c, top)?;
            draw_weather(c, y, data.weather.as_ref())?
        }
        PopupKind::Cpu | PopupKind::Ram => draw_system(c, top, kind, &hist)?,
        PopupKind::Settings => draw_settings(c, top, data)?,
        PopupKind::Todo => draw_todo(c, top)?,
    };
    footer(c, y, m.note)
}

fn history(kind: PopupKind) -> Vec<f32> {
    HISTORY.with(|h| h.borrow().iter().find(|(k, _)| *k == kind).map(|(_, v)| v.clone()).unwrap_or_default())
}

fn draw_git(c: &mut Ctx<'_>, y: f32, git: Option<&GitSummary>) -> Result<f32> {
    let Some(g) = git else { return empty(c, y, "Đang đọc git log…") };
    if let Some(err) = &g.error {
        return empty(c, y, err);
    }
    let mut y = section(c, y, "ACTIVITY · 17 TUẦN")?;
    // .hm-big: padding 4px 14px 8px around the heatmap.
    heatmap(c, rect_at(c.inner_x(), y + 4.0, c.inner_w(), HM_H), &g.heatmap)?;
    y += 4.0 + HM_H + 8.0;

    // .stat-grid: 4 equal columns, bold value over a muted label.
    let total: u32 = g.heatmap.iter().map(|d| d.commits).sum();
    let stats = [(g.today, "hôm nay"), (g.week, "7 ngày"), (g.streak, "streak"), (total, "119 ngày")];
    let cw = c.inner_w() / 4.0;
    for (i, (value, label)) in stats.iter().enumerate() {
        let x = c.inner_x() + i as f32 * cw;
        c.text(&value.to_string(), rect_at(x, y + 2.0, cw, 19.0), font(16.0, TEXT).bold())?;
        c.text(label, rect_at(x, y + 21.0, cw, 13.0), font(9.5, MUTED))?;
    }
    y += 42.0;
    // No REPOSITORIES section: GitSummary has no per-repo list yet.
    section_end(c, y)
}

/// 17 week columns of 7 weekday rows, Monday on top, newest week last. Future days are not drawn.
fn heatmap(c: &mut Ctx<'_>, area: RectF, days: &[GitDay]) -> Result<()> {
    let today_row = Local::now().weekday().num_days_from_monday() as i64;
    for col in 0..HM_COLS {
        for row in 0..HM_ROWS {
            // Days back from today for this cell; negative means a future day.
            let back = ((HM_COLS - 1 - col) as i64) * 7 + today_row - row as i64;
            if back < 0 {
                continue;
            }
            let level = if (back as usize) < days.len() {
                heat_level(days[days.len() - 1 - back as usize].commits)
            } else {
                0
            };
            let x = area.x + col as f32 * (HM_CELL + HM_GAP);
            let y = area.y + row as f32 * (HM_CELL + HM_GAP);
            c.round(rect_at(x, y, HM_CELL, HM_CELL), 1.5, HEAT[level])?;
        }
    }
    Ok(())
}

/// Same buckets as `level()` in GitTile.tsx.
fn heat_level(n: u32) -> usize {
    match n {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=6 => 3,
        _ => 4,
    }
}

fn draw_ai(c: &mut Ctx<'_>, y: f32, ai: &[AiUsage]) -> Result<f32> {
    if ai.is_empty() {
        return empty(c, y, "Đang tải…");
    }
    let mut y = section(c, y, "USAGE")?;
    let selected = AI_PROVIDER.with(|p| *p.borrow());
    let providers = ai.iter().filter(|u| selected.is_none_or(|name| u.name == name));
    let mut row_index = 0usize;
    for u in providers {
        if u.windows.is_empty() && u.cost_usd.is_none() {
            y = row(c, y, row_index, &u.name, Some(u.reset_text.as_str()), None, None)?;
            row_index += 1;
        }
        for window in &u.windows {
            let value = format!("{:.0}%", window.used_pct);
            let hint = (!window.reset_text.is_empty()).then_some(window.reset_text.as_str());
            y = row(
                c,
                y,
                row_index,
                &window.label,
                hint,
                Some(value.as_str()),
                Some(((window.used_pct / 100.0).clamp(0.0, 1.0), ai_color(window.used_pct))),
            )?;
            row_index += 1;
        }
        if let Some(cost) = u.cost_usd {
            let budget = SETTINGS_STATE.with(|s| s.borrow().claude_monthly_budget).max(1) as f32;
            let pct = cost as f32 / budget * 100.0;
            let value = format!("${cost:.2} / ${budget:.0}");
            let bar = Some(((pct / 100.0).clamp(0.0, 1.0), ai_color(pct)));
            y = row(c, y, row_index, "Đã dùng tháng này", None, Some(value.as_str()), bar)?;
            row_index += 1;
        }
    }
    // No SUBSCRIPTION section: AiUsage carries no plan info yet.
    section_end(c, y)
}

fn draw_weather(c: &mut Ctx<'_>, y: f32, w: Option<&Weather>) -> Result<f32> {
    let Some(w) = w else {
        let y = section(c, y, "THỜI TIẾT")?;
        let y = empty(c, y, "Đang tải…")?;
        return section_end(c, y);
    };
    let mut y = section(c, y, &format!("THỜI TIẾT · {}", w.location.to_uppercase()))?;
    let cur = &w.current;
    let x = c.inner_x();
    // .wx-now: big temperature, then condition and details.
    c.text(&format!("{:.0}°", cur.temp), rect_at(x, y + 2.0, 74.0, 30.0), font(26.0, TEXT).bold())?;
    let dx = x + 80.0;
    let dw = c.inner_w() - 80.0;
    c.text(wx_text(cur.code), rect_at(dx, y + 6.0, dw, 16.0), font(11.0, MUTED))?;
    let details = format!("Cảm giác {:.0}° · Ẩm {:.0}% · Gió {:.0} km/h", cur.feels, cur.humidity, cur.wind);
    c.text(&details, rect_at(dx, y + 22.0, dw, 16.0), font(11.0, MUTED))?;
    y += 44.0;

    // .wx-days: one slot per day, up to 7.
    let slot = (c.inner_w() - 20.0) / 7.0;
    for (i, d) in w.daily.iter().take(7).enumerate() {
        let sx = x + 10.0 + i as f32 * slot;
        c.text(&weekday_label(&d.date), rect_at(sx, y, slot, 13.0), font(9.5, MUTED).center())?;
        c.text(&format!("{:.0}°", d.max), rect_at(sx, y + 15.0, slot, 15.0), font(11.0, TEXT).bold().center())?;
        c.text(&format!("{:.0}°", d.min), rect_at(sx, y + 31.0, slot, 13.0), font(9.5, MUTED).center())?;
    }
    y += 52.0;
    section_end(c, y)
}

/// Month title and a Monday-first grid with today highlighted.
fn draw_month(c: &mut Ctx<'_>, y: f32) -> Result<f32> {
    let today = Local::now().date_naive();
    let (year, month) = (today.year(), today.month());
    let y = section(c, y, &format!("THÁNG {month} {year}"))?;

    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(today);
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .unwrap_or(first);
    let days = (next - first).num_days() as u32;
    let start = first.weekday().num_days_from_monday() as usize;
    let weeks = (start + days as usize).div_ceil(7);

    // .month: 7 columns, 2px gap, 24px cells (26px pitch with the row gap).
    const PITCH: f32 = 26.0;
    const GAP: f32 = 2.0;
    let x0 = c.rect.x + 12.0;
    let col_w = (c.rect.w - 24.0 - 6.0 * GAP) / 7.0;
    for (i, d) in WEEKDAYS.iter().enumerate() {
        let area = rect_at(x0 + i as f32 * (col_w + GAP), y, col_w, 24.0);
        c.text(d, area, font(10.0, FAINT).center())?;
    }
    for day in 1..=days {
        let idx = start + day as usize - 1;
        let area = rect_at(
            x0 + (idx % 7) as f32 * (col_w + GAP),
            y + (1 + idx / 7) as f32 * PITCH,
            col_w,
            24.0,
        );
        let is_today = day == today.day();
        if is_today {
            c.round(area, 6.0, TODAY)?;
        }
        let f = if is_today { font(11.5, TEXT).bold() } else { font(11.5, TEXT) };
        c.text(&day.to_string(), area, f.center())?;
    }
    let y = y + (1 + weeks) as f32 * PITCH - GAP + 8.0;
    section_end(c, y)
}

thread_local! {
    /// Start of the wave animation clock, and the latest audio peak (0..1) while media is open.
    static WAVE_START: std::time::Instant = std::time::Instant::now();
    static MEDIA_PEAK: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// Updates the open media popup in place (called ~25x per second): the session state and the
/// audio level that drives the wave.
pub fn update_media(media: Option<Media>, peak: f32) {
    MEDIA_PEAK.with(|p| p.set(peak));
    CONTENT.with(|c| {
        if let Some((_, data)) = c.borrow_mut().as_mut() {
            data.media = media;
        }
    });
}

fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t).round() as u32) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// Media buttons: previous, play/pause (bigger), next, centred under the title.
fn media_buttons(rect: RectF) -> [RectF; 3] {
    let total = MEDIA_SIDE_D * 2.0 + MEDIA_PLAY_D + MEDIA_GAP * 2.0;
    let x0 = rect.x + (rect.w - total) / 2.0;
    let play_y = rect.y + HEAD_H + MEDIA_PLAY_Y;
    let side_y = play_y + (MEDIA_PLAY_D - MEDIA_SIDE_D) / 2.0;
    [
        rect_at(x0, side_y, MEDIA_SIDE_D, MEDIA_SIDE_D),
        rect_at(x0 + MEDIA_SIDE_D + MEDIA_GAP, play_y, MEDIA_PLAY_D, MEDIA_PLAY_D),
        rect_at(x0 + MEDIA_SIDE_D + MEDIA_PLAY_D + MEDIA_GAP * 2.0, side_y, MEDIA_SIDE_D, MEDIA_SIDE_D),
    ]
}

fn draw_media(c: &mut Ctx<'_>, top: f32, m: Option<&Media>) -> Result<f32> {
    let playing = m.is_some_and(|m| m.playing);
    let (x, w) = (c.inner_x(), c.inner_w());
    let t = WAVE_START.with(|s| s.elapsed().as_secs_f32());
    let peak = MEDIA_PEAK.with(|p| p.get()).clamp(0.0, 1.0);

    // Equalizer: bars follow the audio level while playing, and rest as small dots when paused.
    let wave_y = top + 16.0;
    let n = WAVE_BARS;
    let pitch = w / n as f32;
    let amp = (0.35 + peak * 1.9).min(1.0);
    for i in 0..n {
        let k = i as f32;
        let env = 0.5 + 0.5 * (std::f32::consts::PI * (k + 0.5) / n as f32).sin();
        let s = 0.5 + 0.28 * (t * 6.0 + k * 0.7).sin() + 0.22 * (t * 3.7 + k * 1.3).sin();
        let h = if playing { 5.0 + (WAVE_H - 5.0) * (s * amp * env).clamp(0.0, 1.0) } else { 4.0 };
        let base = lerp_color(WAVE_FROM, WAVE_TO, k / (n - 1) as f32);
        let color = if playing { base } else { (base & 0xFFFF_FF00) | 0x70 };
        c.round(rect_at(x + k * pitch + (pitch - WAVE_BAR_W) / 2.0, wave_y + (WAVE_H - h) / 2.0, WAVE_BAR_W, h), WAVE_BAR_W / 2.0, color)?;
    }

    let title = m
        .map(|m| m.title.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or("Không có ứng dụng nào đang phát");
    c.text(title, rect_at(x, top + 70.0, w, 22.0), font(15.5, TEXT).bold().center())?;
    c.text(m.map_or("", |m| m.artist.as_str()), rect_at(x, top + 94.0, w, 16.0), font(11.5, MUTED).center())?;

    // Status pill with a coloured dot.
    if m.is_some() {
        let (label, dot) = if playing { ("Đang phát", GREEN) } else { ("Tạm dừng", ORANGE) };
        let pill_w = 24.0 + label.chars().count() as f32 * 5.6;
        let px = x + (w - pill_w) / 2.0;
        c.round(rect_at(px, top + 116.0, pill_w, 18.0), 9.0, BADGE_BG)?;
        c.round(rect_at(px + 8.0, top + 116.0 + 6.0, 6.0, 6.0), 3.0, dot)?;
        c.text(label, rect_at(px + 18.0, top + 116.0, pill_w - 22.0, 18.0), font(9.5, BADGE_TEXT))?;
    }

    // Buttons use the same rects as `hit_media_button`.
    let [prev, play, next] = media_buttons(c.rect);
    let play_glyph = if playing { PAUSE } else { PLAY };
    for (b, glyph, big) in [(prev, PREV, false), (play, play_glyph, true), (next, NEXT, false)] {
        c.round(b, b.w / 2.0, if big { BTN_BIG } else { BTN })?;
        c.text(glyph, b, font(if big { 18.0 } else { 14.0 }, if big { BTN_BIG_TEXT } else { TEXT }).icon().center())?;
    }
    section_end(c, top + MEDIA_PLAY_Y + MEDIA_PLAY_D + 16.0)
}

// ---------------------------------------------------------------- to-do

const TODO_ROW_H: f32 = 32.0;
const TODO_MAX_SHOWN: usize = 8;

/// Click targets in the To-do popup; indexes are positions in the settings list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoAction {
    Toggle(usize),
    Delete(usize),
    AddFromClipboard,
    FocusInput,
    ClearDone,
}

fn todo_state() -> (Vec<crate::settings::TodoItem>, usize) {
    let items = SETTINGS_STATE.with(|s| s.borrow().todos.clone());
    let shown = items.len().min(TODO_MAX_SHOWN);
    (items, shown)
}

/// Rect of the list row `i` (body top = `top`).
fn todo_row(top: f32, rect: RectF, i: usize) -> RectF {
    rect_at(rect.x + 6.0, top + 8.0 + i as f32 * TODO_ROW_H, rect.w - 12.0, TODO_ROW_H)
}

thread_local! {
    /// Text typed into the note input, and whether the input has keyboard focus.
    static TODO_INPUT: RefCell<String> = const { RefCell::new(String::new()) };
    static TODO_FOCUS: Cell<bool> = const { Cell::new(false) };
}

pub fn todo_input_focused() -> bool {
    TODO_FOCUS.with(Cell::get)
}

pub fn todo_input_set_focus(on: bool) {
    TODO_FOCUS.with(|f| f.set(on));
}

/// Feeds one typed character to the note input. Returns the finished note on Enter.
pub fn todo_input_char(ch: char) -> Option<String> {
    TODO_INPUT.with(|t| {
        let mut t = t.borrow_mut();
        match ch {
            '\u{8}' => {
                t.pop();
            }
            '\r' | '\n' => {
                let text = t.trim().to_owned();
                t.clear();
                return (!text.is_empty()).then_some(text);
            }
            c if !c.is_control() && t.chars().count() < 200 => t.push(c),
            _ => {}
        }
        None
    })
}

/// Input box, add-from-clipboard and clear buttons below the list.
fn todo_buttons(top: f32, rect: RectF, shown: usize, any_done: bool) -> (RectF, RectF, Option<RectF>) {
    // An empty list still reserves one row for its hint.
    let list_h = shown.max(1) as f32 * TODO_ROW_H;
    let y = top + 8.0 + list_h + 10.0;
    let input = rect_at(rect.x + 6.0, y, rect.w - 12.0, 32.0);
    let add = rect_at(rect.x + 6.0, y + 38.0, rect.w - 12.0, 32.0);
    let clear = any_done.then(|| rect_at(rect.x + 6.0, y + 38.0 + 34.0, rect.w - 12.0, 32.0));
    (input, add, clear)
}

pub fn hit_todo(x: f32, y: f32, rect: RectF) -> Option<TodoAction> {
    let top = rect.y + HEAD_H;
    let (items, shown) = todo_state();
    for i in 0..shown {
        let row = todo_row(top, rect, i);
        if contains(row, x, y) {
            return Some(if x >= row.x + row.w - 30.0 { TodoAction::Delete(i) } else { TodoAction::Toggle(i) });
        }
    }
    let (input, add, clear) = todo_buttons(top, rect, shown, items.iter().any(|t| t.done));
    if contains(input, x, y) {
        return Some(TodoAction::FocusInput);
    }
    if contains(add, x, y) {
        return Some(TodoAction::AddFromClipboard);
    }
    clear.filter(|c| contains(*c, x, y)).map(|_| TodoAction::ClearDone)
}

fn draw_todo(c: &mut Ctx<'_>, top: f32) -> Result<f32> {
    let (items, shown) = todo_state();
    let rect = c.rect;
    if items.is_empty() {
        c.text("Chưa có việc nào. Sao chép một dòng chữ rồi bấm nút bên dưới.", rect_at(c.inner_x(), top + 12.0, c.inner_w(), 28.0), font(10.5, EMPTY_TEXT))?;
    }
    for (i, item) in items.iter().take(shown).enumerate() {
        let row = todo_row(top, rect, i);
        if i % 2 == 1 {
            c.round(row, 8.0, STRIPE)?;
        }
        let ring = rect_at(row.x + 8.0, row.y + 7.0, 18.0, 18.0);
        if item.done {
            c.round(ring, 9.0, GREEN)?;
            c.text("✓", ring, font(11.0, 0x0B1A_10FF).bold().center())?;
        } else {
            c.round(ring, 9.0, 0xFFFF_FF55)?;
            c.round(rect_at(ring.x + 1.5, ring.y + 1.5, 15.0, 15.0), 7.5, 0x1C1E_24FF)?;
        }
        let color = if item.done { MUTED } else { TEXT };
        c.text(&item.text, rect_at(row.x + 34.0, row.y, row.w - 34.0 - 34.0, row.h), font(11.5, color))?;
        c.text("\u{E711}", rect_at(row.x + row.w - 28.0, row.y, 22.0, row.h), font(10.0, MUTED).icon().center())?;
    }
    let (input, add, clear) = todo_buttons(top, rect, shown, items.iter().any(|t| t.done));
    let focused = todo_input_focused();
    c.round(input, 9.0, if focused { 0xFFFF_FF55 } else { 0xFFFF_FF22 })?;
    c.round(rect_at(input.x + 1.0, input.y + 1.0, input.w - 2.0, input.h - 2.0), 8.0, 0x1A1C_22FF)?;
    let typed = TODO_INPUT.with(|t| t.borrow().clone());
    let text_area = rect_at(input.x + 10.0, input.y, input.w - 20.0, input.h);
    if typed.is_empty() && !focused {
        c.text("Nhập ghi chú rồi nhấn Enter…", text_area, font(11.0, MUTED))?;
    } else {
        // Show the tail of long text so the caret end stays visible.
        let chars: Vec<char> = typed.chars().collect();
        let tail: String = chars[chars.len().saturating_sub(34)..].iter().collect();
        let shown_text = if focused { format!("{tail}|") } else { tail };
        c.text(&shown_text, text_area, font(11.5, TEXT))?;
    }
    c.round(add, 9.0, BTN)?;
    c.text("＋  Thêm từ clipboard", add, font(11.0, TEXT).center())?;
    let mut bottom = add.y + add.h;
    if let Some(clear) = clear {
        c.round(clear, 9.0, BTN)?;
        c.text("Xoá việc đã xong", clear, font(11.0, MUTED).center())?;
        bottom = clear.y + clear.h;
    }
    if items.len() > shown {
        c.text(&format!("+{} việc nữa", items.len() - shown), rect_at(c.inner_x(), bottom + 4.0, c.inner_w(), 14.0), font(9.5, MUTED))?;
        bottom += 16.0;
    }
    section_end(c, bottom + 8.0)
}

/// CPU or RAM: one usage row, then a sparkline of the sample history.
fn draw_system(c: &mut Ctx<'_>, y: f32, kind: PopupKind, hist: &[f32]) -> Result<f32> {
    let Some(&cur) = hist.last() else { return empty(c, y, "Đang đo…") };
    let mut y = section(c, y, "SỬ DỤNG")?;
    let color = if kind == PopupKind::Cpu { pct_color(cur) } else { BLUE };
    let value = format!("{cur:.0}%");
    y = row(c, y, 0, "Sử dụng", None, Some(value.as_str()), Some((cur / 100.0, color)))?;
    spark(c, rect_at(c.inner_x(), y, c.inner_w(), SPARK_H), hist, color)?;
    y += SPARK_H + 6.0;
    section_end(c, y)
}

/// Sparkline on a 0..=100 scale. The renderer has no polyline, so the line and its faint
/// area are drawn as 1px columns.
fn spark(c: &mut Ctx<'_>, area: RectF, data: &[f32], color: u32) -> Result<()> {
    if data.len() < 2 || area.w <= 0.0 {
        return Ok(());
    }
    let base = area.y + area.h;
    let y_of = |v: f32| area.y + area.h - 2.0 - (v.clamp(0.0, 100.0) / 100.0) * (area.h - 4.0);
    let step = area.w / (data.len() - 1) as f32;
    let fill = (color & 0xFFFF_FF00) | 0x1F; // ~12% alpha
    for (i, pair) in data.windows(2).enumerate() {
        let x0 = area.x + i as f32 * step;
        let x1 = x0 + step;
        let (y0, y1) = (y_of(pair[0]), y_of(pair[1]));
        let mut px = x0.floor();
        while px < x1 {
            let t0 = ((px - x0) / step).clamp(0.0, 1.0);
            let t1 = ((px + 1.0 - x0) / step).clamp(0.0, 1.0);
            let ya = y0 + (y1 - y0) * t0;
            let yb = y0 + (y1 - y0) * t1;
            let top = ya.min(yb);
            c.fill(rect_at(px, top, 1.0, base - top), fill)?;
            c.fill(rect_at(px, top, 1.0, ya.max(yb) - top + 1.5), color)?;
            px += 1.0;
        }
    }
    Ok(())
}

fn draw_settings(c: &mut Ctx<'_>, y: f32, data: &DataState) -> Result<f32> {
    let settings = SETTINGS_STATE.with(|state| state.borrow().clone());
    let x = c.inner_x();
    let w = c.inner_w();

    c.text(
        &format!("ĐỘ ĐỤC KÍNH  ·  {}%", settings.glass_opacity),
        rect_at(x, y + 10.0, w, 15.0),
        font(9.5, MUTED).bold(),
    )?;
    let track = rect_at(x, y + 38.0, w, 5.0);
    c.round(track, 2.5, 0xFFFF_FF1F)?;
    c.round(RectF { w: track.w * settings.glass_opacity as f32 / 95.0, ..track }, 2.5, BLUE)?;
    let knob_x = track.x + track.w * settings.glass_opacity as f32 / 95.0;
    c.round(rect_at(knob_x - 7.0, track.y - 5.0, 14.0, 14.0), 7.0, WHITE)?;

    c.text("TASKBAR WINDOWS", rect_at(x, y + 57.0, w, 15.0), font(9.5, MUTED).bold())?;
    let taskbar = rect_at(x, y + 76.0, w, 30.0);
    c.round(taskbar, 8.0, 0xFFFF_FF0F)?;
    let half = taskbar.w / 2.0;
    let hidden = settings.taskbar_mode == "hidden";
    c.round(
        rect_at(taskbar.x + if hidden { half } else { 0.0 }, taskbar.y, half, taskbar.h),
        8.0,
        0x4AA8_FF45,
    )?;
    c.text("Tự ẩn", rect_at(taskbar.x, taskbar.y, half, taskbar.h), font(10.5, if hidden { MUTED } else { WHITE }).center())?;
    c.text("Ẩn hoàn toàn", rect_at(taskbar.x + half, taskbar.y, half, taskbar.h), font(10.5, if hidden { WHITE } else { MUTED }).center())?;

    c.text("WIDGET HIỂN THỊ", rect_at(x, y + 118.0, w, 15.0), font(9.5, MUTED).bold())?;
    let cell_w = (w - 8.0) / 3.0;
    for (index, (id, label)) in SETTING_TILES.iter().enumerate() {
        let on = match *id {
            "git" => settings.tiles.git,
            "claude" => settings.tiles.claude,
            "codex" => settings.tiles.codex,
            "cursor" => settings.tiles.cursor,
            "grok" => settings.tiles.grok,
            "music" => settings.tiles.music,
            "cpu" => settings.tiles.cpu,
            "ram" => settings.tiles.ram,
            _ => settings.tiles.weather,
        };
        let cell = rect_at(
            x + (index % 3) as f32 * (cell_w + 4.0),
            y + 137.0 + (index / 3) as f32 * 31.0,
            cell_w,
            27.0,
        );
        c.round(cell, 7.0, if on { 0x4AA8_FF35 } else { 0xFFFF_FF0B })?;
        c.text(if on { "✓" } else { "○" }, rect_at(cell.x + 8.0, cell.y, 16.0, cell.h), font(10.5, if on { BLUE } else { MUTED }).center())?;
        c.text(label, rect_at(cell.x + 27.0, cell.y, cell.w - 32.0, cell.h), font(10.0, if on { TEXT } else { MUTED }))?;
    }

    setting_value(
        c,
        rect_at(x, y + 257.0, w, 30.0),
        "Vị trí thời tiết",
        if settings.weather_position == "tile" { "Widget riêng" } else { "Trong đồng hồ" },
    )?;
    setting_value(c, rect_at(x, y + 295.0, w, 30.0), "Địa điểm", &settings.weather_location)?;
    let repo = if settings.git_folders.is_empty() {
        if settings.git_author.is_empty() {
            "Tự tìm repo · user.email".to_owned()
        } else {
            format!("Tự tìm repo · {}", settings.git_author)
        }
    } else {
        format!("{} thư mục · {}", settings.git_folders.len(), if settings.git_author.is_empty() { "email Git" } else { &settings.git_author })
    };
    setting_value(c, rect_at(x, y + 333.0, w, 30.0), "Git / email", &repo)?;
    setting_value(
        c,
        rect_at(x, y + 371.0, w, 30.0),
        "Khởi động cùng Windows",
        if settings.launch_at_startup { "Bật" } else { "Tắt" },
    )?;
    setting_value(c, rect_at(x, y + 409.0, w, 30.0), "Phím tắt ẩn / hiện", &settings.shortcut)?;
    autopin_row(c, rect_at(x, y + 447.0, w, 30.0), settings.auto_pin, settings.auto_pin_count)?;
    setting_value(
        c,
        rect_at(x, y + 485.0, w, 30.0),
        "Nguồn hạn mức Claude",
        if settings.claude_source == "apikey" { "API key · chi phí tháng" } else { "Đăng nhập Claude Code" },
    )?;
    let git_hint = data
        .git
        .as_ref()
        .and_then(|g| g.error.as_deref())
        .unwrap_or("Bấm Địa điểm hoặc Git / email để sửa settings.json.");
    setting_value(c, rect_at(x, y + 523.0, w, 30.0), "Hạn mức AI", "Làm mới ngay")?;
    setting_value(
        c,
        rect_at(x, y + 561.0, w, 30.0),
        "Hạn mức tháng (Claude)",
        &format!("{} US$", settings.claude_monthly_budget),
    )?;
    c.text(git_hint, rect_at(x, y + 599.0, w, 28.0), font(9.0, MUTED))?;
    Ok(y + 636.0)
}

thread_local! {
    /// Auto-pin state as last drawn (the hit test needs to know whether the chip is shown).
    static SETTINGS_AUTO_ON: Cell<bool> = const { Cell::new(true) };
}

/// Switch geometry inside the auto-pin row (right side, iOS-style pill).
fn autopin_switch(area: RectF) -> RectF {
    rect_at(area.x + area.w - 10.0 - 34.0, area.y + (area.h - 18.0) / 2.0, 34.0, 18.0)
}

/// "N app" chip left of the switch; click cycles 4/6/8/10/12.
fn autopin_chip(area: RectF) -> RectF {
    let sw = autopin_switch(area);
    rect_at(sw.x - 8.0 - 54.0, area.y + (area.h - 20.0) / 2.0, 54.0, 20.0)
}

/// "Tự động ghim app" with a clear on/off switch and the count chip.
fn autopin_row(c: &mut Ctx<'_>, area: RectF, on: bool, count: u32) -> Result<()> {
    SETTINGS_AUTO_ON.with(|s| s.set(on));
    c.round(area, 8.0, 0xFFFF_FF0B)?;
    c.text("Tự động ghim app", rect_at(area.x + 10.0, area.y, area.w * 0.5, area.h), font(10.0, TEXT))?;
    let sw = autopin_switch(area);
    c.round(sw, sw.h / 2.0, if on { 0x34C7_59FF } else { 0xFFFF_FF2E })?;
    let knob = 14.0;
    let kx = if on { sw.x + sw.w - 2.0 - knob } else { sw.x + 2.0 };
    c.round(rect_at(kx, sw.y + 2.0, knob, knob), knob / 2.0, 0xFFFF_FFFF)?;
    if on {
        let chip = autopin_chip(area);
        c.round(chip, 6.0, 0xFFFF_FF14)?;
        c.text(&format!("{count} app ▾"), chip, font(9.5, TEXT).center())?;
    } else {
        let label = rect_at(sw.x - 8.0 - 54.0, area.y, 54.0, area.h);
        c.text("Tắt", label, font(9.5, MUTED).right())?;
    }
    Ok(())
}

fn setting_value(c: &mut Ctx<'_>, area: RectF, label: &str, value: &str) -> Result<()> {
    c.round(area, 8.0, 0xFFFF_FF0B)?;
    c.text(label, rect_at(area.x + 10.0, area.y, area.w * 0.48, area.h), font(10.0, TEXT))?;
    c.text(value, rect_at(area.x + area.w * 0.46, area.y, area.w * 0.50 - 10.0, area.h), font(9.5, MUTED).right())
}

// ---------------------------------------------------------------- labels

/// Vietnamese short label for a WMO weather code (same buckets as weatherInfo in common.tsx).
fn wx_text(code: u32) -> &'static str {
    match code {
        0 => "Trời quang",
        1..=2 => "Ít mây",
        3 => "Nhiều mây",
        4..=48 => "Sương mù",
        49..=57 => "Mưa phùn",
        58..=67 => "Mưa",
        68..=77 => "Tuyết",
        78..=82 => "Mưa rào",
        83..=86 => "Mưa tuyết",
        _ => "Dông",
    }
}

/// "T2".."CN" from a YYYY-MM-DD date; falls back to MM-DD.
fn weekday_label(date: &str) -> String {
    match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => WEEKDAYS[d.weekday().num_days_from_monday() as usize].to_owned(),
        Err(_) => date.get(5..).unwrap_or(date).to_owned(),
    }
}
