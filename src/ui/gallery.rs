//! Widget gallery: one big glass sheet (search, categories, live previews of every widget).
//! Clicking a card adds it to or removes it from the dock. Pure geometry + drawing: the caller
//! supplies the entries (with their live `TileView`) and applies the actions.
use std::cell::{Cell, RefCell};

use windows::core::Result;

use super::dock::{draw_tile, RectF, TileView};
use super::renderer::{Align, Renderer, TextStyle};

pub const W: f32 = 720.0;
pub const H: f32 = 470.0;

const SIDE_X: f32 = 14.0;
const SIDE_W: f32 = 176.0;
const MAIN_X: f32 = SIDE_X + SIDE_W + 16.0;
const GRID_TOP: f32 = 66.0;
const CARD_PAD: f32 = 14.0;
const TILE_H: f32 = 42.0;
const CARD_H: f32 = TILE_H + 2.0 * CARD_PAD;
const CELL_H: f32 = CARD_H + 24.0;
const GAP_X: f32 = 12.0;

const TEXT: u32 = 0xFFFF_FFEE;
const MUTED: u32 = 0xFFFF_FF8C;
const ACCENT: u32 = 0x0A84_FFFF;
const CARD_BG: u32 = 0xFFFF_FF1C;

/// Sidebar categories; index 0 shows everything.
pub const CATS: [&str; 9] = ["Tất cả widget", "Đồng hồ", "Năng suất", "Hệ thống", "AI", "Âm nhạc", "Thời tiết", "Ảnh chụp", "Tài chính"];

/// Every widget with its category index into `CATS`.
pub const ITEMS: [(&str, usize); 45] = [
    ("clock", 1),
    ("date", 1),
    ("git", 2),
    ("todo", 2),
    ("focus", 2),
    ("timer", 2),
    ("downloads", 2),
    ("cpu", 3),
    ("ram", 3),
    ("battery", 3),
    ("net", 3),
    ("disk", 3),
    ("gpu", 3),
    ("lang", 3),
    ("cc", 3),
    ("chevron", 3),
    ("claude", 4),
    ("codex", 4),
    ("cursor", 4),
    ("grok", 4),
    ("media", 5),
    ("weather", 6),
    ("pomodoro", 2),
    ("stopwatch", 2),
    ("notes", 2),
    ("dayprog", 1),
    ("monthprog", 1),
    ("yearprog", 1),
    ("theme", 3),
    ("brightness", 3),
    ("recyclebin", 3),
    ("screenshot", 7),
    ("recentshots", 7),
    ("search", 2),
    ("worldclock", 1),
    ("analog", 1),
    ("flipclock", 1),
    ("segclock", 1),
    ("daynum", 1),
    ("sun", 6),
    ("hourly", 6),
    ("forecast", 6),
    ("metric", 8),
    ("currency", 8),
    ("screentime", 2),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_lists_every_widget_once_in_the_right_category() {
        for id in crate::settings::WIDGET_IDS {
            assert_eq!(ITEMS.iter().filter(|(name, _)| *name == id).count(), 1, "{id}");
        }
        assert!(ITEMS.iter().all(|(_,cat)|*cat>0 && *cat<CATS.len()));
        for (id,cat) in [("pomodoro",2),("stopwatch",2),("notes",2),("dayprog",1),("monthprog",1),("yearprog",1),
            ("theme",3),("brightness",3),("recyclebin",3),("screenshot",7),("recentshots",7),("search",2),
            ("worldclock",1),("analog",1),("flipclock",1),("segclock",1),("daynum",1),("sun",6),("hourly",6),("forecast",6),
            ("metric",8),("currency",8),("screentime",2)] {assert!(ITEMS.contains(&(id,cat)),"{id}");}
        for (id, cat) in [("battery", 3), ("net", 3), ("disk", 3), ("gpu", 3), ("timer", 2), ("date", 1)] {
            assert!(ITEMS.contains(&(id, cat)));
        }
    }
}

pub struct Entry {
    pub id: &'static str,
    pub name: &'static str,
    pub cat: usize,
    pub view: TileView,
    /// Dock width of the widget.
    pub width: f32,
    /// Currently on the bar.
    pub on: bool,
    /// Cannot be removed (Control Center button, tray chevron, clock).
    pub fixed: bool,
    /// Hardware unavailable; an already-enabled widget can still be removed.
    pub disabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Close,
    Category(usize),
    FocusSearch,
    Toggle(&'static str),
}

thread_local! {
    static CAT: Cell<usize> = const { Cell::new(0) };
    static QUERY: RefCell<String> = const { RefCell::new(String::new()) };
    static SCROLL: Cell<f32> = const { Cell::new(0.0) };
    static FOCUS: Cell<bool> = const { Cell::new(false) };
}

/// Fresh state for a newly opened gallery.
pub fn reset() {
    CAT.with(|c| c.set(0));
    QUERY.with(|q| q.borrow_mut().clear());
    SCROLL.with(|s| s.set(0.0));
    FOCUS.with(|f| f.set(false));
}

pub fn set_focus(on: bool) {
    FOCUS.with(|f| f.set(on));
}

pub fn focused() -> bool {
    FOCUS.with(Cell::get)
}

pub fn select(cat: usize) {
    CAT.with(|c| c.set(cat.min(CATS.len() - 1)));
    SCROLL.with(|s| s.set(0.0));
}

/// One typed character for the search box (Backspace edits).
pub fn input_char(ch: char) {
    QUERY.with(|q| {
        let mut q = q.borrow_mut();
        match ch {
            '\u{8}' => {
                q.pop();
            }
            c if !c.is_control() && q.chars().count() < 30 => q.push(c),
            _ => {}
        }
    });
    SCROLL.with(|s| s.set(0.0));
}

pub fn scroll(notches: f32) {
    SCROLL.with(|s| s.set((s.get() - notches * 48.0).max(0.0)));
}

fn shown(e: &Entry) -> bool {
    let cat = CAT.with(Cell::get);
    if cat != 0 && e.cat != cat {
        return false;
    }
    let q = QUERY.with(|q| q.borrow().to_lowercase());
    q.is_empty() || e.name.to_lowercase().contains(&q) || e.id.contains(&q)
}

struct Placed {
    index: usize,
    card: RectF,
    tile: RectF,
}

fn grid_area(rect: RectF) -> RectF {
    RectF { x: rect.x + MAIN_X, y: rect.y + GRID_TOP, w: rect.w - MAIN_X - 18.0, h: rect.h - GRID_TOP - 14.0 }
}

/// Flow layout of the visible cards (before scrolling). Returns the cards and the content height.
fn flow(entries: &[Entry], area: RectF) -> (Vec<Placed>, f32) {
    let mut out = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    for (i, e) in entries.iter().enumerate().filter(|(_, e)| shown(e)) {
        let w = (e.width + 2.0 * CARD_PAD).max(104.0);
        if x > 0.0 && x + w > area.w {
            x = 0.0;
            y += CELL_H;
        }
        let card = RectF { x: area.x + x, y: area.y + y, w, h: CARD_H };
        let tile = RectF { x: card.x + (w - e.width) / 2.0, y: card.y + CARD_PAD, w: e.width, h: TILE_H };
        out.push(Placed { index: i, card, tile });
        x += w + GAP_X;
    }
    let content = if out.is_empty() { 0.0 } else { y + CELL_H };
    (out, content)
}

fn scrolled(entries: &[Entry], area: RectF) -> Vec<Placed> {
    let (mut placed, content) = flow(entries, area);
    let max = (content - area.h).max(0.0);
    let off = SCROLL.with(|s| {
        let v = s.get().min(max);
        s.set(v);
        v
    });
    for p in &mut placed {
        p.card.y -= off;
        p.tile.y -= off;
    }
    placed
}

fn close_btn(rect: RectF) -> RectF {
    RectF { x: rect.x + rect.w - 18.0 - 28.0, y: rect.y + 16.0, w: 28.0, h: 28.0 }
}

fn search_box(rect: RectF) -> RectF {
    RectF { x: rect.x + SIDE_X, y: rect.y + 16.0, w: SIDE_W, h: 34.0 }
}

fn cat_row(rect: RectF, i: usize) -> RectF {
    RectF { x: rect.x + SIDE_X, y: rect.y + 64.0 + i as f32 * 36.0, w: SIDE_W, h: 32.0 }
}

pub fn hit(x: f32, y: f32, rect: RectF, entries: &[Entry]) -> Option<Action> {
    let inside = |r: RectF| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
    if inside(close_btn(rect)) {
        return Some(Action::Close);
    }
    if inside(search_box(rect)) {
        return Some(Action::FocusSearch);
    }
    for i in 0..CATS.len() {
        if inside(cat_row(rect, i)) {
            return Some(Action::Category(i));
        }
    }
    let area = grid_area(rect);
    if inside(area) {
        for p in scrolled(entries, area) {
            if inside(p.card) {
                let e = &entries[p.index];
                return (!e.fixed && !e.disabled).then_some(Action::Toggle(e.id));
            }
        }
    }
    None
}

pub fn draw(r: &mut Renderer, rect: RectF, entries: &[Entry]) -> Result<()> {
    // Sidebar: search box and categories.
    let sb = search_box(rect);
    r.fill_rounded_rect(sb, sb.h / 2.0, CARD_BG)?;
    if focused() {
        r.stroke_rounded_rect(sb, sb.h / 2.0, 0xFFFF_FF66, 1.0)?;
    }
    r.draw_text_styled("\u{E721}", RectF { x: sb.x + 10.0, w: 16.0, ..sb }, 11.0, MUTED, Align::Center, TextStyle::Icon)?;
    let query = QUERY.with(|q| q.borrow().clone());
    let label_rect = RectF { x: sb.x + 32.0, w: sb.w - 42.0, ..sb };
    if query.is_empty() && !focused() {
        r.draw_text("Tìm widget", label_rect, 12.0, MUTED, Align::Left)?;
    } else {
        let tail: String = query.chars().rev().take(18).collect::<Vec<_>>().into_iter().rev().collect();
        r.draw_text(&format!("{tail}{}", if focused() { "|" } else { "" }), label_rect, 12.0, TEXT, Align::Left)?;
    }
    let cat = CAT.with(Cell::get);
    for (i, name) in CATS.iter().enumerate() {
        let row = cat_row(rect, i);
        if i == cat {
            r.fill_rounded_rect(row, 10.0, ACCENT)?;
        }
        r.draw_text_styled(name, RectF { x: row.x + 14.0, w: row.w - 20.0, ..row }, 12.5, TEXT, Align::Left, if i == cat { TextStyle::Bold } else { TextStyle::Ui })?;
    }

    // Header.
    let title = CATS[cat];
    let hx = rect.x + MAIN_X;
    r.draw_text_styled(title, RectF { x: hx, y: rect.y + 16.0, w: 260.0, h: 28.0 }, 17.0, TEXT, Align::Left, TextStyle::Bold)?;
    let on_bar = entries.iter().filter(|e| e.on).count();
    let total = entries.len();
    let count_x = hx + 14.0 + title.chars().count() as f32 * 9.6;
    r.draw_text(&format!("{total} widget · {on_bar} trên thanh"), RectF { x: count_x, y: rect.y + 16.0, w: 220.0, h: 28.0 }, 11.0, MUTED, Align::Left)?;
    let cb = close_btn(rect);
    r.fill_rounded_rect(cb, cb.h / 2.0, CARD_BG)?;
    r.draw_text_styled("\u{E711}", cb, 10.0, TEXT, Align::Center, TextStyle::Icon)?;

    // Cards (clipped to the grid area so scrolling can cut them off).
    let area = grid_area(rect);
    r.push_clip(RectF { y: area.y - 4.0, h: area.h + 4.0, ..area })?;
    let result = (|| -> Result<()> {
        let placed = scrolled(entries, area);
        if placed.is_empty() {
            r.draw_text("Không có widget nào khớp.", RectF { x: area.x, y: area.y + 10.0, w: area.w, h: 20.0 }, 12.0, MUTED, Align::Left)?;
        }
        for p in placed {
            let e = &entries[p.index];
            r.fill_rounded_rect(p.card, 16.0, CARD_BG)?;
            if e.on {
                let color = if e.fixed { 0xFFFF_FF55 } else { ACCENT };
                r.stroke_rounded_rect(p.card, 16.0, color, 1.5)?;
                let badge = RectF { x: p.card.x + p.card.w - 22.0, y: p.card.y + 6.0, w: 16.0, h: 16.0 };
                r.fill_rounded_rect(badge, 8.0, color)?;
                let (bx, by) = (badge.x, badge.y);
                if e.fixed {
                    // Padlock: body and shackle.
                    r.fill_rounded_rect(RectF { x: bx + 4.5, y: by + 7.5, w: 7.0, h: 5.5 }, 1.2, 0xFFFF_FFFF)?;
                    r.stroke_arc(bx + 8.0, by + 7.5, 2.3, 180.0, 180.0, 0xFFFF_FFFF, 1.2)?;
                } else {
                    r.stroke_line(bx + 4.0, by + 8.4, bx + 7.0, by + 11.2, 0xFFFF_FFFF, 1.7)?;
                    r.stroke_line(bx + 7.0, by + 11.2, bx + 12.2, by + 5.2, 0xFFFF_FFFF, 1.7)?;
                }
            }
            draw_tile(r, &e.view, p.tile, false)?;
            let name = e.name.to_owned();
            r.draw_text(&name, RectF { x: p.card.x - 6.0, y: p.card.y + CARD_H + 3.0, w: p.card.w + 12.0, h: 16.0 }, 10.5, MUTED, Align::Center)?;
        }
        Ok(())
    })();
    r.pop_clip()?;
    result
}
