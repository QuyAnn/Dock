//! Data tiles: holds fetched data, builds `TileView`s from it, and draws the git heatmap.
//! No threads here: main runs `refresh_slow` on a worker and sends results back via `apply`.
use std::path::PathBuf;

use chrono::{Datelike, Local};
use windows::core::Result;

use super::dock::{RectF, Style, TileView};
use super::renderer::Renderer;
use crate::data::ai::refresh_ai;
use crate::data::git::{refresh_git, GitSummary};
use crate::data::weather::{refresh_weather_at, Weather};
use crate::model::{AiUsage, Media, WidgetStats};
use crate::win::system::current_media;

/// Commits per week that fill the git tile's progress bar.
const WEEK_GOAL: f32 = 10.0;

#[derive(Clone, Debug, Default)]
pub struct DataState {
    pub widgets: WidgetStats,
    pub git: Option<GitSummary>,
    pub ai: Vec<AiUsage>,
    pub weather: Option<Weather>,
    pub media: Option<Media>,
}

/// One result from a background refresh.
pub enum DataUpdate {
    Widgets(WidgetStats),
    Git(GitSummary),
    Ai(Vec<AiUsage>),
    Weather(Option<Weather>),
    Media(Option<Media>),
}

impl DataState {
    /// Blocking network and disk reads (git, AI usage, weather). Run on a worker thread.
    pub fn refresh_slow(&mut self, repos: &[PathBuf], git_author: &str, weather_location: &str) {
        for update in Self::fetch_slow(repos, git_author, weather_location) {
            self.apply(update);
        }
    }

    /// The slow (network/disk) pass as updates, so callers can fetch without holding any lock.
    pub fn fetch_slow(repos: &[PathBuf], git_author: &str, weather_location: &str) -> [DataUpdate; 3] {
        [
            DataUpdate::Git(refresh_git(repos, git_author)),
            DataUpdate::Ai(refresh_ai()),
            DataUpdate::Weather(refresh_weather_at(weather_location)),
        ]
    }

    /// The current media session as an update. Blocks on WinRT; keep it off the UI thread.
    pub fn fetch_media() -> DataUpdate {
        DataUpdate::Media(current_media())
    }

    /// Reads the current media session. Blocks on WinRT; keep it off the UI thread.
    pub fn refresh_media(&mut self) {
        self.apply(DataUpdate::Media(current_media()));
    }

    pub fn apply(&mut self, update: DataUpdate) {
        match update {
            DataUpdate::Widgets(s) => self.widgets = s,
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

pub fn battery_tile(s: &WidgetStats) -> TileView {
    let Some(b) = &s.battery else { return TileView::card("PIN", "Không có pin") };
    let state = if b.charging { "ĐANG SẠC" } else if b.plugged_in { "CẮM ĐIỆN" } else { "DÙNG PIN" };
    TileView {
        progress: b.percent.map(|p| p as f32 / 100.0),
        accent: if b.charging { 0x3FD2_7FFF } else if b.percent.is_some_and(|p| p <= 20) { 0xFF5D_5DFF } else { 0x4AA8_FFFF },
        ..TileView::card(format!("PIN · {state}"), b.percent.map_or("--".into(), |p| format!("{p}%")))
    }
}

pub fn net_tile(s: &WidgetStats) -> TileView {
    let (down, up) = s.net.map_or(("--".into(), "--".into()), |(d, u)| (crate::data::widgets::speed_text(d), crate::data::widgets::speed_text(u)));
    TileView { style: Style::Metrics, sub: format!("↑ {up}"), ..TileView::card("TỐC ĐỘ MẠNG", format!("↓ {down}")) }
}

pub fn disk_tile(s: &WidgetStats) -> TileView {
    let free = |i: usize| s.disk_free[i].map_or("--".into(), crate::data::widgets::size_text);
    TileView { style: Style::Metrics, sub: format!("D: {}", free(1)), ..TileView::card("DUNG LƯỢNG TRỐNG", format!("C: {}", free(0))) }
}

pub fn gpu_tile(s: &WidgetStats) -> TileView {
    TileView {
        progress: s.gpu_pct.map(|p| p / 100.0),
        accent: 0x4AA8_FFFF,
        ..TileView::card("GPU", s.gpu_pct.map_or("--".into(), |p| format!("{p:.0}%")))
    }
}

pub fn timer_tile() -> TileView {
    let (secs, running) = crate::data::timer::snapshot();
    TileView::card(if running { "BẤM GIỜ · CHẠY" } else { "BẤM GIỜ · DỪNG" }, crate::data::timer::time_text(secs))
}

pub fn date_tile(date: chrono::NaiveDate) -> TileView {
    const DAYS: [&str; 7] = ["CHỦ NHẬT", "THỨ HAI", "THỨ BA", "THỨ TƯ", "THỨ NĂM", "THỨ SÁU", "THỨ BẢY"];
    TileView::card(DAYS[date.weekday().num_days_from_sunday() as usize], date.format("%d/%m/%Y").to_string())
}

#[cfg(test)]
mod widget_tests {
    use super::*;

    #[test]
    fn telemetry_tiles_show_live_values_and_unavailable_states() {
        let mut s = WidgetStats::default();
        assert_eq!(gpu_tile(&s).value, "--");
        assert_eq!(net_tile(&s).value, "↓ --");
        assert_eq!(disk_tile(&s).sub, "D: --");
        assert_eq!(battery_tile(&s).value, "Không có pin");
        s.battery = Some(crate::model::Battery { percent: Some(73), charging: true, plugged_in: true });
        s.net = Some((2048.0, 1024.0));
        s.disk_free = [Some(1024_u64.pow(3)), None];
        s.gpu_pct = Some(42.0);
        let battery = battery_tile(&s);
        assert_eq!(battery.value, "73%");
        assert!(battery.title.contains("ĐANG SẠC"));
        assert_eq!(battery.progress, Some(0.73));
        assert_eq!(net_tile(&s).value, "↓ 2.0 KB/s");
        assert_eq!(net_tile(&s).sub, "↑ 1.0 KB/s");
        assert_eq!(disk_tile(&s).value, "C: 1.0 GB");
        assert_eq!(gpu_tile(&s).value, "42%");
        s.battery.as_mut().unwrap().charging = false;
        assert!(battery_tile(&s).title.contains("CẮM ĐIỆN"));
        s.battery.as_mut().unwrap().plugged_in = false;
        assert!(battery_tile(&s).title.contains("DÙNG PIN"));
    }

    #[test]
    fn date_tile_uses_vietnamese_weekdays() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(date_tile(date).title, "CHỦ NHẬT");
        assert_eq!(date_tile(date).value, "25/10/2026");
        assert_eq!(date_tile(date.succ_opt().unwrap()).title, "THỨ HAI");
    }
}

/// Git card: today / week / streak numbers over a strip of recent days.
/// `title`, `value`, `sub` carry the today, week and streak counts; `cells` holds one level
/// (0..=4) per day, oldest first, ending with today (the tile draws as many as fit).
pub fn git_tile(d: &DataState) -> TileView {
    match &d.git {
        None => tile("GIT", "–".to_owned(), None),
        Some(g) if g.error.is_some() => {
            // Hint belongs in Settings / Git popup; tile stays quiet dashes + an empty grid.
            TileView {
                style: Style::Git,
                sub: "–".to_owned(),
                cells: day_levels(&g.heatmap),
                ..TileView::card("–", "–".to_owned())
            }
        }
        Some(g) => TileView {
            style: Style::Git,
            sub: g.streak.to_string(),
            cells: day_levels(&g.heatmap),
            ..TileView::card(g.today.to_string(), g.week.to_string())
        },
    }
}

/// Commit count -> heat level 0..=4 (same bucketing as `Heatmap` in GitTile.tsx).
pub fn heat_level(n: u32) -> u8 {
    match n {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=6 => 3,
        _ => 4,
    }
}

/// One level per day, oldest first, ending with today.
pub fn day_levels(days: &[crate::model::GitDay]) -> Vec<u8> {
    days.iter().map(|d| heat_level(d.commits)).collect()
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
    match &d.media {
        Some(m) if !m.title.is_empty() => TileView {
            style: Style::Media,
            title: m.title.clone(),
            sub: m.artist.clone(),
            progress: m.playing.then_some(1.0),
            ..TileView::card("", "")
        },
        _ => TileView {
            style: Style::Media,
            title: "Không phát".to_owned(),
            sub: "Media controls".to_owned(),
            ..TileView::card("", "")
        },
    }
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
