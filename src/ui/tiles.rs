//! Data tiles: holds fetched data, builds `TileView`s from it, and draws the git heatmap.
//! No threads here: main runs `refresh_slow` on a worker and sends results back via `apply`.
use std::path::PathBuf;

use chrono::{Datelike, Local};
use windows::core::Result;

use super::dock::{RectF, Style, TileView};
use super::renderer::Renderer;
use crate::data::ai::refresh_ai;
use crate::data::git::{refresh_git, GitSummary};
use crate::data::weather::{refresh_weather_at, Weather, DEFAULT_LOCATION};
use crate::model::{AiUsage, Media};
use crate::win::system::current_media;

/// Commits per week that fill the git tile's progress bar.
const WEEK_GOAL: f32 = 10.0;

#[derive(Clone, Debug, Default)]
pub struct DataState {
    pub git: Option<GitSummary>,
    pub ai: Vec<AiUsage>,
    pub weather: Option<Weather>,
    pub media: Option<Media>,
}

/// One result from a background refresh.
pub enum DataUpdate {
    Git(GitSummary),
    Ai(Vec<AiUsage>),
    Weather(Option<Weather>),
    Media(Option<Media>),
}

impl DataState {
    /// Blocking network and disk reads (git, AI usage, weather). Run on a worker thread.
    pub fn refresh_slow(&mut self, repos: &[PathBuf]) {
        self.apply(DataUpdate::Git(refresh_git(repos)));
        self.apply(DataUpdate::Ai(refresh_ai()));
        self.apply(DataUpdate::Weather(refresh_weather_at(DEFAULT_LOCATION)));
    }

    /// Reads the current media session. Blocks on WinRT; keep it off the UI thread.
    pub fn refresh_media(&mut self) {
        self.apply(DataUpdate::Media(current_media()));
    }

    pub fn apply(&mut self, update: DataUpdate) {
        match update {
            DataUpdate::Git(g) => self.git = Some(g),
            DataUpdate::Ai(a) => self.ai = a,
            DataUpdate::Weather(w) => self.weather = w,
            DataUpdate::Media(m) => self.media = m,
        }
    }
}

fn tile(title: &str, value: String, progress: Option<f32>) -> TileView {
    TileView { progress, ..TileView::card(title, value) }
}

/// Git card: today / week / streak numbers and a 16-week mini heatmap (3 rows, Fri-Sun).
pub fn git_tile(d: &DataState) -> TileView {
    match &d.git {
        None => tile("GIT", "–".to_owned(), None),
        Some(g) if g.error.is_some() => tile("GIT", "Lỗi".to_owned(), None),
        Some(g) => TileView {
            style: Style::Git,
            cells: heat_levels(&g.heatmap, 16, 3),
            ..TileView::card("GIT", format!("{} today  {} week  {} streak", g.today, g.week, g.streak))
        },
    }
}

/// Heatmap levels 0..=4, column-major, same bucketing as `Heatmap` in GitTile.tsx.
/// `days` is oldest first and ends with today. Future days are level 0.
fn heat_levels(days: &[crate::model::GitDay], weeks: usize, rows: usize) -> Vec<u8> {
    let today_dow = Local::now().weekday().num_days_from_monday() as i64;
    let level = |n: u32| -> u8 { match n { 0 => 0, 1 => 1, 2..=3 => 2, 4..=6 => 3, _ => 4 } };
    let mut out = Vec::with_capacity(weeks * rows);
    for col in 0..weeks {
        let w = (weeks - 1 - col) as i64; // column 0 is the oldest week
        for row in (7 - rows)..7 {
            let offset = w * 7 + (today_dow - row as i64);
            let lvl = if offset < 0 {
                0
            } else {
                days.len().checked_sub(1 + offset as usize).map_or(0, |i| level(days[i].commits))
            };
            out.push(lvl);
        }
    }
    out
}

/// First provider's 5-hour session usage. Without data, the status text is shown instead.
pub fn ai_tile(d: &DataState) -> TileView {
    match d.ai.first() {
        None => tile("AI", "–".to_owned(), None),
        Some(u) if u.session_pct.is_nan() => tile(&u.name, u.reset_text.clone(), None),
        Some(u) => tile(&u.name, format!("{:.0}%", u.session_pct), Some((u.session_pct / 100.0).clamp(0.0, 1.0))),
    }
}

pub fn weather_tile(d: &DataState) -> TileView {
    let value = match &d.weather {
        None => "–".to_owned(),
        Some(w) => format!("{}° {}", w.current.temp.round(), weather_text(w.current.code)),
    };
    tile("THỜI TIẾT", value, None)
}

pub fn media_tile(d: &DataState) -> TileView {
    let value = match &d.media {
        Some(m) if !m.title.is_empty() && !m.artist.is_empty() => format!("{} – {}", m.title, m.artist),
        Some(m) if !m.title.is_empty() => m.title.clone(),
        _ => "Không phát".to_owned(),
    };
    tile("MEDIA", value, None)
}

/// WMO weather code to short Vietnamese text (same table as the old app, no icons).
fn weather_text(code: u32) -> &'static str {
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

const HM_COLS: usize = 17;
const HM_ROWS: usize = 7;
const HM_GAP: f32 = 2.0;
const HM_EMPTY: u32 = 0xFFFF_FF14;
/// Green 0x40C463 at four alpha levels, lightest first.
const HM_LEVELS: [u32; 4] = [0x40C4_6359, 0x40C4_6399, 0x40C4_63CC, 0x40C4_63FF];

fn heat_color(commits: u32) -> u32 {
    match commits {
        0 => HM_EMPTY,
        1 => HM_LEVELS[0],
        2..=3 => HM_LEVELS[1],
        4..=6 => HM_LEVELS[2],
        _ => HM_LEVELS[3],
    }
}

/// GitHub-style grid: 17 weeks as columns, Monday..Sunday as rows, newest week last.
/// Days after today in the last column are left empty. Cells are centered in `rect`.
pub fn draw_heatmap(r: &mut Renderer, rect: RectF, git: &GitSummary) -> Result<()> {
    let cell = ((rect.w - HM_GAP * (HM_COLS - 1) as f32) / HM_COLS as f32)
        .min((rect.h - HM_GAP * (HM_ROWS - 1) as f32) / HM_ROWS as f32)
        .max(0.0);
    let grid_w = cell * HM_COLS as f32 + HM_GAP * (HM_COLS - 1) as f32;
    let grid_h = cell * HM_ROWS as f32 + HM_GAP * (HM_ROWS - 1) as f32;
    let x0 = rect.x + (rect.w - grid_w) / 2.0;
    let y0 = rect.y + (rect.h - grid_h) / 2.0;

    let days = &git.heatmap;
    let today_row = Local::now().weekday().num_days_from_monday() as i64;
    for col in 0..HM_COLS {
        for row in 0..HM_ROWS {
            // Days before today for this cell; `heatmap` is oldest first and ends today.
            let back = (HM_COLS - 1 - col) as i64 * HM_ROWS as i64 + today_row - row as i64;
            if back < 0 || back as usize >= days.len() {
                continue;
            }
            let commits = days[days.len() - 1 - back as usize].commits;
            let cell_rect = RectF {
                x: x0 + col as f32 * (cell + HM_GAP),
                y: y0 + row as f32 * (cell + HM_GAP),
                w: cell,
                h: cell,
            };
            r.fill_rounded_rect(cell_rect, cell * 0.2, heat_color(commits))?;
        }
    }
    Ok(())
}
