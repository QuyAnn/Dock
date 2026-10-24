//! Clock popup: an Itsycal / macOS style month calendar (month navigation, weekends in red,
//! today as a blue disc, neighbouring-month days dimmed) with the weather below.
//! Drawn without the standard popup header so it reads as one calendar sheet.
use std::cell::Cell;

use chrono::{Datelike, Duration, Local, NaiveDate};
use windows::core::Result;

use super::dock::RectF;
use super::renderer::{Align, Renderer, TextStyle};

pub const WIDTH: f32 = 288.0;
const PAD: f32 = 14.0;
const HEAD_Y: f32 = 12.0;
const BTN: f32 = 26.0;
const WEEK_Y: f32 = 50.0;
const GRID_Y: f32 = 70.0;
const ROW_H: f32 = 30.0;
const DISC: f32 = 26.0;


const TEXT: u32 = 0xFFFF_FFF0;
const MUTED: u32 = 0xFFFF_FF8C;
const DIM: u32 = 0xFFFF_FF4D;
const WEEKEND: u32 = 0xFF6B_62FF;
const WEEKEND_DIM: u32 = 0xFF6B_6260;
const ACCENT: u32 = 0x0A84_FFFF;
const SECTION: u32 = 0x6CB4_FFFF;
const DIVIDER: u32 = 0xFFFF_FF1A;
const BTN_BG: u32 = 0xFFFF_FF1A;

const WEEKDAYS: [&str; 7] = ["T2", "T3", "T4", "T5", "T6", "T7", "CN"];


thread_local! {
    /// Months shown away from the current one (0 = this month).
    static OFFSET: Cell<i32> = const { Cell::new(0) };
}

/// Back to the current month (the popup was just opened).
pub fn reset() {
    OFFSET.with(|o| o.set(0));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Prev,
    Next,
    Today,
}

pub fn apply(action: Action) {
    OFFSET.with(|o| match action {
        Action::Prev => o.set(o.get() - 1),
        Action::Next => o.set(o.get() + 1),
        Action::Today => o.set(0),
    });
}

/// Popup height in DIPs (fixed: the grid always has six rows).
pub fn height() -> f32 {
    GRID_Y + 6.0 * ROW_H + 8.0 + 28.0 + 44.0 + 46.0 + 12.0
}

fn buttons(rect: RectF) -> [RectF; 3] {
    let y = rect.y + HEAD_Y;
    let right = rect.x + rect.w - PAD;
    [
        RectF { x: right - 3.0 * BTN - 4.0, y, w: BTN, h: BTN },
        RectF { x: right - 2.0 * BTN - 2.0, y, w: BTN, h: BTN },
        RectF { x: right - BTN, y, w: BTN, h: BTN },
    ]
}

pub fn hit(x: f32, y: f32, rect: RectF) -> Option<Action> {
    let inside = |r: RectF| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
    let [prev, today, next] = buttons(rect);
    if inside(prev) {
        Some(Action::Prev)
    } else if inside(next) {
        Some(Action::Next)
    } else if inside(today) {
        Some(Action::Today)
    } else {
        None
    }
}

/// First day of the shown month.
fn shown_month(today: NaiveDate) -> NaiveDate {
    let total = today.year() * 12 + today.month0() as i32 + OFFSET.with(Cell::get);
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap_or(today)
}

fn disc(r: &mut Renderer, cx: f32, cy: f32, d: f32, rgba: u32) -> Result<()> {
    r.fill_rounded_rect(RectF { x: cx - d / 2.0, y: cy - d / 2.0, w: d, h: d }, d / 2.0, rgba)
}

pub fn draw(r: &mut Renderer, rect: RectF, weather: Option<&crate::data::weather::Weather>) -> Result<()> {
    let today = Local::now().date_naive();
    let first = shown_month(today);

    // Header: month and year, then previous / today / next.
    let title = format!("Tháng {}, {}", first.month(), first.year());
    r.draw_text_styled(&title, RectF { x: rect.x + PAD, y: rect.y + HEAD_Y, w: rect.w - 2.0 * PAD - 3.0 * BTN, h: BTN }, 15.0, TEXT, Align::Left, TextStyle::Bold)?;
    let [prev, now, next] = buttons(rect);
    for (b, glyph) in [(prev, "\u{E76B}"), (next, "\u{E76C}")] {
        r.fill_rounded_rect(b, BTN / 2.0, BTN_BG)?;
        r.draw_text_styled(glyph, b, 10.0, TEXT, Align::Center, TextStyle::Icon)?;
    }
    r.fill_rounded_rect(now, BTN / 2.0, BTN_BG)?;
    disc(r, now.x + BTN / 2.0, now.y + BTN / 2.0, 8.0, TEXT)?;

    // Weekday letters.
    let col_w = (rect.w - 2.0 * PAD) / 7.0;
    for (i, d) in WEEKDAYS.iter().enumerate() {
        let area = RectF { x: rect.x + PAD + i as f32 * col_w, y: rect.y + WEEK_Y, w: col_w, h: 16.0 };
        r.draw_text_styled(d, area, 10.0, if i >= 5 { WEEKEND_DIM } else { MUTED }, Align::Center, TextStyle::Bold)?;
    }

    // Six weeks starting on the Monday on or before the 1st.
    let start = first - Duration::days(first.weekday().num_days_from_monday() as i64);
    for i in 0..42 {
        let date = start + Duration::days(i);
        let (col, row) = ((i % 7) as f32, (i / 7) as f32);
        let area = RectF { x: rect.x + PAD + col * col_w, y: rect.y + GRID_Y + row * ROW_H, w: col_w, h: ROW_H };
        let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h / 2.0);
        let in_month = date.month() == first.month();
        let weekend = date.weekday().num_days_from_monday() >= 5;
        let is_today = date == today;
        if is_today {
            disc(r, cx, cy, DISC, ACCENT)?;
        }
        let color = match (is_today, in_month, weekend) {
            (true, _, _) => 0xFFFF_FFFF,
            (_, true, true) => WEEKEND,
            (_, true, false) => TEXT,
            (_, false, true) => WEEKEND_DIM,
            (_, false, false) => DIM,
        };
        let style = if is_today { TextStyle::Bold } else { TextStyle::Ui };
        r.draw_text_styled(&date.day().to_string(), area, 12.5, color, Align::Center, style)?;
    }

    // Weather: current conditions, then the week ahead.
    let list_y = rect.y + GRID_Y + 6.0 * ROW_H + 8.0;
    r.fill_rect(RectF { x: rect.x + PAD, y: list_y, w: rect.w - 2.0 * PAD, h: 1.0 }, DIVIDER)?;
    let Some(w) = weather else {
        r.draw_text_styled("THỜI TIẾT", RectF { x: rect.x + PAD, y: list_y + 8.0, w: rect.w - 2.0 * PAD, h: 14.0 }, 9.5, SECTION, Align::Left, TextStyle::Bold)?;
        return r.draw_text("Đang tải…", RectF { x: rect.x + PAD, y: list_y + 30.0, w: rect.w - 2.0 * PAD, h: 16.0 }, 11.0, MUTED, Align::Left);
    };
    let inner_w = rect.w - 2.0 * PAD;
    r.draw_text_styled(&format!("THỜI TIẾT · {}", w.location.to_uppercase()), RectF { x: rect.x + PAD, y: list_y + 8.0, w: inner_w, h: 14.0 }, 9.5, SECTION, Align::Left, TextStyle::Bold)?;
    let y = list_y + 28.0;
    let cur = &w.current;
    r.draw_text_styled(&format!("{:.0}°", cur.temp), RectF { x: rect.x + PAD, y, w: 72.0, h: 32.0 }, 28.0, TEXT, Align::Left, TextStyle::Bold)?;
    let dx = rect.x + PAD + 76.0;
    let dw = inner_w - 76.0;
    r.draw_text_styled(super::popup_content::weather_text(cur.code), RectF { x: dx, y: y + 2.0, w: dw, h: 16.0 }, 12.0, TEXT, Align::Left, TextStyle::Bold)?;
    let details = format!("Cảm giác {:.0}° · Ẩm {:.0}% · Gió {:.0} km/h", cur.feels, cur.humidity, cur.wind);
    r.draw_text(&details, RectF { x: dx, y: y + 19.0, w: dw, h: 14.0 }, 10.0, MUTED, Align::Left)?;
    let days_y = y + 44.0;
    let slot = inner_w / w.daily.len().clamp(1, 7) as f32;
    for (i, d) in w.daily.iter().take(7).enumerate() {
        let sx = rect.x + PAD + i as f32 * slot;
        r.draw_text(&super::popup_content::weekday_short(&d.date), RectF { x: sx, y: days_y, w: slot, h: 13.0 }, 9.5, MUTED, Align::Center)?;
        r.draw_text_styled(&format!("{:.0}°", d.max), RectF { x: sx, y: days_y + 15.0, w: slot, h: 15.0 }, 11.5, TEXT, Align::Center, TextStyle::Bold)?;
        r.draw_text(&format!("{:.0}°", d.min), RectF { x: sx, y: days_y + 31.0, w: slot, h: 13.0 }, 9.5, MUTED, Align::Center)?;
    }
    Ok(())
}
