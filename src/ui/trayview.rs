//! SmartDock's own tray overflow popup ("^"): the notification-area icons learned by
//! `win::traysvc`, drawn as a Liquid Glass grid inside the shared NOACTIVATE popup.
//! Clicks are forwarded to each icon's owner with the shell's own callback messages.
use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::dock::RectF;
use super::glass;
use super::renderer::{Align, Renderer};
use crate::win::traysvc::{self, Action, TrayIcon};

const CELL: f32 = 30.0;
const ICON: f32 = 18.0;
const PAD: f32 = 7.0;
const CAPTION_H: f32 = 20.0;
const MAX_COLS: usize = 6;
const MIN_W: f32 = 120.0;
const TEXT: u32 = 0xF1F1_F4FF;
const TEXT_DIM: u32 = 0xF1F1_F4B0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Idle,
    Repaint,
    Close,
}

#[derive(Default)]
struct View {
    generation: u64,
    icons: Vec<TrayIcon>,
    /// Count of icons Windows would keep in its overflow (not pinned to the taskbar).
    hidden_count: usize,
    cols: usize,
    width: f32,
    height: f32,
    hover: Option<usize>,
    last_click: Option<(usize, Instant)>,
    close_at: Option<Instant>,
}

thread_local! {
    static VIEW: RefCell<View> = RefCell::new(View::default());
}

/// Builds the icon list. Overflow (not pinned) icons first, then the ones Windows pins to the
/// taskbar, which is hidden while SmartDock runs, so they are only reachable here.
fn snapshot(v: &mut View) {
    traysvc::prune_dead();
    let (generation, all) = traysvc::icons();
    let settings = traysvc::icon_settings();
    let (mut overflow, mut pinned): (Vec<TrayIcon>, Vec<TrayIcon>) = (Vec::new(), Vec::new());
    for i in all.into_iter().filter(|i| !i.hidden && (i.icon.is_some() || !i.tip.is_empty())) {
        if traysvc::is_promoted(&i, &settings) {
            pinned.push(i);
        } else {
            overflow.push(i);
        }
    }
    v.hidden_count = overflow.len();
    overflow.extend(pinned);
    v.icons = overflow;
    v.generation = generation;
    if v.hover.is_some_and(|h| h >= v.icons.len()) {
        v.hover = None;
    }
}

fn layout(v: &mut View) {
    let n = v.icons.len();
    v.cols = n.clamp(1, MAX_COLS);
    let rows = n.div_ceil(v.cols).max(1);
    v.width = (PAD * 2.0 + v.cols as f32 * CELL).max(MIN_W);
    v.height = PAD + rows as f32 * CELL + CAPTION_H + 4.0;
}

/// Prepares the view for `anchor` (the ^ key, physical px). Returns popup (width, height) DIPs.
pub fn open(anchor: RectF) -> (f32, f32) {
    traysvc::set_anchor(
        anchor.x.round() as i32,
        anchor.y.round() as i32,
        (anchor.x + anchor.w).round() as i32,
        (anchor.y + anchor.h).round() as i32,
    );
    VIEW.with(|c| {
        let mut v = c.borrow_mut();
        *v = View::default();
        snapshot(&mut v);
        layout(&mut v);
        let names: Vec<String> = v.icons.iter().map(traysvc::short_name).collect();
        crate::log(&format!(
            "overflow: own popup opened with {} icon(s) ({} overflow, {} pinned){}: {}",
            v.icons.len(),
            v.hidden_count,
            v.icons.len() - v.hidden_count,
            if traysvc::failed() { " [tray listener failed]" } else { "" },
            names.join(" | ")
        ));
        (v.width, v.height)
    })
}

fn cell_rect(v: &View, idx: usize) -> RectF {
    let grid_w = v.cols as f32 * CELL;
    let x0 = ((v.width - grid_w) / 2.0).round();
    let (row, col) = (idx / v.cols, idx % v.cols);
    RectF { x: x0 + col as f32 * CELL, y: PAD + row as f32 * CELL, w: CELL, h: CELL }
}

fn hit(v: &View, x: f32, y: f32) -> Option<usize> {
    (0..v.icons.len()).find(|&i| {
        let r = cell_rect(v, i);
        x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
    })
}

pub fn paint(r: &mut Renderer, rect: RectF) -> Result<()> {
    VIEW.with(|c| {
        let v = c.borrow();
        for (i, icon) in v.icons.iter().enumerate() {
            let cell = cell_rect(&v, i);
            if v.hover == Some(i) {
                let g = RectF { x: cell.x + 2.0, y: cell.y + 2.0, w: cell.w - 4.0, h: cell.h - 4.0 };
                r.fill_rounded_rect(g, 8.0, 0xFFFF_FF1E)?;
                glass::draw_highlights(r, g, 8.0, 0.9)?;
            }
            let ir = RectF { x: cell.x + (CELL - ICON) / 2.0, y: cell.y + (CELL - ICON) / 2.0, w: ICON, h: ICON };
            match &icon.icon {
                Some(b) => r.draw_bitmap(b.w, b.h, &b.rgba, ir)?,
                None => {
                    let letter: String = traysvc::short_name(icon).chars().take(1).collect();
                    r.fill_rounded_rect(ir, 6.0, 0xFFFF_FF26)?;
                    r.draw_text(&letter, ir, 12.0, TEXT, Align::Center)?;
                }
            }
        }
        // Caption: the hovered icon's tooltip, else a count.
        let cap = RectF { x: rect.x + PAD, y: v.height - CAPTION_H - 2.0, w: v.width - PAD * 2.0, h: CAPTION_H };
        let (text, color) = match v.hover.and_then(|h| v.icons.get(h)) {
            Some(i) => (traysvc::short_name(i), TEXT),
            None if v.icons.is_empty() => {
                ("Chưa có biểu tượng khay nào".to_owned(), TEXT_DIM)
            }
            None => (format!("{} biểu tượng", v.icons.len()), TEXT_DIM),
        };
        r.draw_text(&text, cap, 11.5, color, Align::Center)
    })
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: out-param is a local.
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

/// Handles a click inside the popup. Returns true when the popup should close now.
pub fn on_click(x: f32, y: f32, button: Button) -> bool {
    VIEW.with(|c| {
        let mut v = c.borrow_mut();
        let Some(idx) = hit(&v, x, y) else { return false };
        let icon = v.icons[idx].clone();
        let (sx, sy) = cursor();
        match button {
            Button::Right => {
                traysvc::click(&icon, Action::Right, sx, sy);
                true
            }
            Button::Left => {
                // SAFETY: plain system query.
                let dbl = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64);
                let now = Instant::now();
                if v.last_click.is_some_and(|(i, t)| i == idx && now.duration_since(t) <= dbl) {
                    v.last_click = None;
                    traysvc::click(&icon, Action::Double, sx, sy);
                    return true;
                }
                v.last_click = Some((idx, now));
                traysvc::click(&icon, Action::Left, sx, sy);
                // Wait out the double-click window, then get out of the way of what opened.
                v.close_at = Some(now + dbl);
                false
            }
        }
    })
}

/// Every popup poll (100 ms): hover, live icon updates, delayed close after a click.
pub fn tick() -> Tick {
    let pointer = super::popup::cursor_in_popup();
    VIEW.with(|c| {
        let mut v = c.borrow_mut();
        if v.close_at.is_some_and(|t| Instant::now() >= t) {
            v.close_at = None;
            return Tick::Close;
        }
        let mut out = Tick::Idle;
        if traysvc::generation() != v.generation {
            let (w, h) = (v.width, v.height);
            snapshot(&mut v);
            layout(&mut v);
            // The popup keeps its size until reopened; extra icons beyond it stay reachable
            // after the next open (rare: an app adding icons while the popup is up).
            v.width = w;
            v.height = h;
            out = Tick::Repaint;
        }
        let hover = pointer.and_then(|(x, y)| hit(&v, x, y));
        if hover != v.hover {
            v.hover = hover;
            out = Tick::Repaint;
        }
        out
    })
}

pub fn closed() {
    VIEW.with(|c| {
        let mut v = c.borrow_mut();
        v.hover = None;
        v.close_at = None;
        v.last_click = None;
    });
    crate::log("overflow: own popup closed");
}
