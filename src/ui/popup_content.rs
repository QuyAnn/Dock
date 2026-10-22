//! Popup body drawing per tile kind. Pure drawing: the caller supplies data and the rect.
//! `rect` is the whole popup area; content is inset by PAD.
use std::cell::RefCell;

use windows::core::Result;

use super::dock::RectF;
use super::renderer::{Align, Renderer};
use crate::data::git::GitSummary;
use crate::data::weather::Weather;
use crate::model::{AiUsage, Media};

const PAD: f32 = 14.0;
const ROW_H: f32 = 22.0;
const CELL: f32 = 12.0;
const CELL_GAP: f32 = 3.0;
const BTN_H: f32 = 28.0;
const BTN_GAP: f32 = 6.0;

const TEXT: u32 = 0xFFFF_FFE6;
const DIM: u32 = 0xFFFF_FF99;
const FAINT: u32 = 0xFFFF_FF55;
const TRACK: u32 = 0xFFFF_FF33;
const BTN: u32 = 0xFFFF_FF1F;
const BTN_STROKE: u32 = 0xFFFF_FF2E;

/// Heatmap levels: 0 (none) then 4 green shades for 1, 2, 3, 4+ commits.
const HEAT: [u32; 5] = [0xFFFF_FF14, 0x4ADE_8055, 0x4ADE_8099, 0x4ADE_80CC, 0x4ADE_80FF];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupKind {
    Git,
    Ai,
    Weather,
    Media,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAction {
    Previous,
    PlayPause,
    Next,
}

pub use super::tiles::DataState;

/// Settings placeholder rows: (label, on).
const SETTINGS: [(&str, bool); 4] = [
    ("Start with Windows", true),
    ("Full-width dock", false),
    ("Dark mode", true),
    ("Show attention dots", true),
];

/// Draws the body of one popup into `rect`. Call between `Renderer::begin` and `end`.
pub fn draw_popup(r: &mut Renderer, kind: PopupKind, rect: RectF, data: &DataState) -> Result<()> {
    let inner = RectF { x: rect.x + PAD, y: rect.y + PAD, w: (rect.w - 2.0 * PAD).max(0.0), h: (rect.h - 2.0 * PAD).max(0.0) };
    match kind {
        PopupKind::Git => draw_git(r, inner, data.git.as_ref()),
        PopupKind::Ai => draw_ai(r, inner, &data.ai),
        PopupKind::Weather => draw_weather(r, inner, data.weather.as_ref()),
        PopupKind::Media => draw_media(r, inner, data.media.as_ref()),
        PopupKind::Settings => draw_settings(r, inner),
    }
}

/// Hit test for the media buttons. `rect` must match the one passed to `draw_popup`.
pub fn hit_media_button(x: f32, y: f32, rect: RectF) -> Option<MediaAction> {
    let inner = inset(rect);
    let [prev, play, next] = media_buttons(inner);
    let actions = [(prev, MediaAction::Previous), (play, MediaAction::PlayPause), (next, MediaAction::Next)];
    actions
        .into_iter()
        .find(|(b, _)| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
        .map(|(_, a)| a)
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

thread_local! {
    static CONTENT: RefCell<Option<(PopupKind, DataState)>> = const { RefCell::new(None) };
}

/// Stores what `paint_content` draws. Call before `popup::show` and after each data refresh.
pub fn set_content(kind: PopupKind, data: DataState) {
    CONTENT.with(|c| *c.borrow_mut() = Some((kind, data)));
}

fn inset(rect: RectF) -> RectF {
    RectF { x: rect.x + PAD, y: rect.y + PAD, w: (rect.w - 2.0 * PAD).max(0.0), h: (rect.h - 2.0 * PAD).max(0.0) }
}

/// Row `i` of the inner area, full width.
fn row(inner: RectF, i: f32) -> RectF {
    RectF { x: inner.x, y: inner.y + i * ROW_H, w: inner.w, h: ROW_H }
}

fn title(r: &mut Renderer, rc: RectF, text: &str) -> Result<()> {
    r.draw_text(text, rc, 14.0, TEXT, Align::Left)
}

/// Label on the left, value on the right.
fn kv(r: &mut Renderer, rc: RectF, key: &str, value: &str) -> Result<()> {
    r.draw_text(key, rc, 13.0, DIM, Align::Left)?;
    r.draw_text(value, rc, 13.0, TEXT, Align::Right)
}

fn draw_git(r: &mut Renderer, inner: RectF, git: Option<&GitSummary>) -> Result<()> {
    title(r, row(inner, 0.0), "Git activity")?;
    let Some(g) = git else {
        return r.draw_text("Loading...", row(inner, 1.0), 13.0, DIM, Align::Left);
    };
    if let Some(err) = &g.error {
        r.draw_text(err, row(inner, 1.0), 12.0, DIM, Align::Left)?;
    } else {
        // Three numbers across one row: value then label.
        let col_w = inner.w / 3.0;
        for (i, (value, label)) in [(g.today, "today"), (g.week, "week"), (g.streak, "streak")].iter().enumerate() {
            let x = inner.x + i as f32 * col_w;
            let rc = row(inner, 1.0);
            r.draw_text(&value.to_string(), RectF { x, w: 36.0, ..rc }, 14.0, TEXT, Align::Left)?;
            r.draw_text(label, RectF { x: x + 36.0, w: col_w - 36.0, ..rc }, 12.0, DIM, Align::Left)?;
        }
    }
    let top = inner.y + 2.0 * ROW_H + 8.0;
    draw_heatmap(r, inner.x, top, &g.heatmap)
}

/// 7 rows (weekdays) by N columns (weeks), oldest day first, drawn column by column.
fn draw_heatmap(r: &mut Renderer, x: f32, y: f32, days: &[crate::model::GitDay]) -> Result<()> {
    let max = days.iter().map(|d| d.commits).max().unwrap_or(0).max(1);
    for (i, day) in days.iter().enumerate() {
        let col = (i / 7) as f32;
        let row_i = (i % 7) as f32;
        let level = match day.commits {
            0 => 0,
            c => {
                // Scale against the busiest day, then map to 1..=4.
                let s = (c as f32 / max as f32 * 4.0).ceil() as usize;
                s.clamp(1, 4)
            }
        };
        let rc = RectF { x: x + col * (CELL + CELL_GAP), y: y + row_i * (CELL + CELL_GAP), w: CELL, h: CELL };
        r.fill_rounded_rect(rc, 2.0, HEAT[level])?;
    }
    Ok(())
}

fn draw_ai(r: &mut Renderer, inner: RectF, ai: &[AiUsage]) -> Result<()> {
    title(r, row(inner, 0.0), "AI usage")?;
    if ai.is_empty() {
        return r.draw_text("No providers", row(inner, 1.0), 13.0, DIM, Align::Left);
    }
    const NAME_W: f32 = 88.0;
    const RESET_W: f32 = 96.0;
    for (i, u) in ai.iter().enumerate() {
        let rc = row(inner, i as f32 + 1.0);
        r.draw_text(&u.name, RectF { w: NAME_W, ..rc }, 13.0, TEXT, Align::Left)?;
        let bar_x = inner.x + NAME_W + 4.0;
        let bar_w = (inner.w - NAME_W - RESET_W - 8.0).max(0.0);
        if u.session_pct.is_nan() {
            // No data: reset_text holds the status (sign-in needed, error, ...).
            let status = RectF { x: bar_x, w: inner.w - NAME_W - 4.0, ..rc };
            r.draw_text(&u.reset_text, status, 12.0, DIM, Align::Left)?;
            continue;
        }
        let track = RectF { x: bar_x, y: rc.y + (ROW_H - 4.0) / 2.0, w: bar_w, h: 4.0 };
        r.fill_rect(track, TRACK)?;
        let frac = (u.session_pct / 100.0).clamp(0.0, 1.0);
        r.fill_rect(RectF { w: bar_w * frac, ..track }, TEXT)?;
        r.draw_text(&u.reset_text, RectF { x: inner.x + inner.w - RESET_W, w: RESET_W, ..rc }, 12.0, DIM, Align::Right)?;
    }
    Ok(())
}

fn draw_weather(r: &mut Renderer, inner: RectF, w: Option<&Weather>) -> Result<()> {
    let Some(w) = w else {
        return r.draw_text("Weather unavailable", row(inner, 0.0), 13.0, DIM, Align::Left);
    };
    title(r, row(inner, 0.0), &w.location)?;
    let c = &w.current;
    kv(r, row(inner, 1.0), "Now", &format!("{:.0}°C  {}", c.temp, wmo_text(c.code)))?;
    kv(r, row(inner, 2.0), "Feels like", &format!("{:.0}°C", c.feels))?;
    kv(r, row(inner, 3.0), "Humidity", &format!("{:.0}%", c.humidity))?;
    kv(r, row(inner, 4.0), "Wind", &format!("{:.0} km/h", c.wind))?;
    for (i, d) in w.daily.iter().take(5).enumerate() {
        let rc = row(inner, 5.0 + i as f32);
        // Dates are YYYY-MM-DD; show MM-DD.
        let short = d.date.get(5..).unwrap_or(&d.date);
        r.draw_text(short, RectF { w: 52.0, ..rc }, 12.0, DIM, Align::Left)?;
        r.draw_text(wmo_text(d.code), RectF { x: rc.x + 52.0, w: inner.w - 52.0 - 80.0, ..rc }, 12.0, DIM, Align::Left)?;
        r.draw_text(&format!("{:.0}° / {:.0}°", d.max, d.min), RectF { x: rc.x + inner.w - 80.0, w: 80.0, ..rc }, 12.0, TEXT, Align::Right)?;
    }
    Ok(())
}

/// Short label for a WMO weather code (Open-Meteo).
fn wmo_text(code: u32) -> &'static str {
    match code {
        0 => "Clear",
        1..=3 => "Cloudy",
        45 | 48 => "Fog",
        51..=67 | 80..=82 => "Rain",
        71..=77 | 85 | 86 => "Snow",
        95..=99 => "Storm",
        _ => "-",
    }
}

fn media_buttons(inner: RectF) -> [RectF; 3] {
    let y = inner.y + 2.0 * ROW_H + 8.0;
    let bw = ((inner.w - 2.0 * BTN_GAP) / 3.0).max(0.0);
    [0.0, 1.0, 2.0].map(|i| RectF { x: inner.x + i * (bw + BTN_GAP), y, w: bw, h: BTN_H })
}

fn draw_media(r: &mut Renderer, inner: RectF, m: Option<&Media>) -> Result<()> {
    let Some(m) = m else {
        return r.draw_text("Nothing playing", row(inner, 0.0), 13.0, DIM, Align::Left);
    };
    title(r, row(inner, 0.0), &m.title)?;
    r.draw_text(&m.artist, row(inner, 1.0), 12.0, DIM, Align::Left)?;
    let play_label = if m.playing { "Pause" } else { "Play" };
    let labels = ["Previous", play_label, "Next"];
    for (b, label) in media_buttons(inner).iter().zip(labels) {
        r.fill_rounded_rect(*b, 8.0, BTN)?;
        r.stroke_rounded_rect(*b, 8.0, BTN_STROKE, 1.0)?;
        r.draw_text(label, *b, 12.0, TEXT, Align::Center)?;
    }
    Ok(())
}

fn draw_settings(r: &mut Renderer, inner: RectF) -> Result<()> {
    title(r, row(inner, 0.0), "Settings")?;
    for (i, (label, on)) in SETTINGS.iter().enumerate() {
        kv(r, row(inner, i as f32 + 1.0), label, if *on { "On" } else { "Off" })?;
    }
    Ok(())
}
