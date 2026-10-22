//! Right-click context menu for centre app icons (Dock-Rc AppMenu).
//! Rendered inside the shared NOACTIVATE popup so focus stays on the foreign app.
use windows::core::Result;

use super::dock::RectF;
use super::renderer::{Align, Renderer, TextStyle};
use crate::model::WindowEntry;
use crate::settings::{DockPin, Settings};
use crate::win::icons::IconBitmap;
use crate::win::tasks::{close_window, focus_window, launch_exe};

const PAD_X: f32 = 10.0;
const ROW_H: f32 = 32.0;
const HEAD_H: f32 = 40.0;
const GROUP_GAP: f32 = 6.0;
const ICON: f32 = 18.0;
const TEXT: u32 = 0xF1F1_F4FF;
const DANGER: u32 = 0xFF6B_6BFF;
const DIVIDER: u32 = 0xFFFF_FF14;

#[derive(Clone, Debug)]
pub struct AppMenuData {
    pub name: String,
    pub exe: String,
    pub pin_id: Option<String>,
    pub windows: Vec<MenuWindow>,
    pub icon: Option<IconBitmap>,
}

#[derive(Clone, Debug)]
pub struct MenuWindow {
    pub hwnd: isize,
    pub title: String,
    #[allow(dead_code)]
    pub minimized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppMenuAction {
    Launch,
    Focus(isize),
    Pin,
    Unpin,
    CloseAll,
}

thread_local! {
    static MENU: std::cell::RefCell<Option<AppMenuData>> = const { std::cell::RefCell::new(None) };
    static ROWS: std::cell::RefCell<Vec<(AppMenuAction, RectF, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn set_menu(menu: AppMenuData) {
    MENU.with(|m| *m.borrow_mut() = Some(menu));
}

pub fn clear() {
    MENU.with(|m| *m.borrow_mut() = None);
    ROWS.with(|r| r.borrow_mut().clear());
}

pub fn current() -> Option<AppMenuData> {
    MENU.with(|m| m.borrow().clone())
}

pub fn from_group(name: &str, exe: &str, wins: &[WindowEntry], icon: Option<IconBitmap>, settings: &Settings) -> AppMenuData {
    let pin_id = settings
        .dock_pins
        .iter()
        .find(|p| p.exe.eq_ignore_ascii_case(exe))
        .map(|p| format!("dock:{}", p.exe.to_ascii_lowercase()));
    AppMenuData {
        name: name.to_owned(),
        exe: exe.to_owned(),
        pin_id,
        windows: wins
            .iter()
            .map(|w| MenuWindow {
                hwnd: w.hwnd,
                title: w.title.clone(),
                minimized: w.minimized,
            })
            .collect(),
        icon,
    }
}

pub fn content_height(menu: &AppMenuData) -> f32 {
    let mut h = HEAD_H + 8.0;
    if menu.windows.len() > 1 {
        h += menu.windows.len() as f32 * ROW_H + GROUP_GAP;
    }
    // launch + pin/unpin
    h += ROW_H * 2.0 + GROUP_GAP;
    if !menu.windows.is_empty() {
        h += ROW_H + GROUP_GAP;
    }
    h + 8.0
}

pub fn paint(r: &mut Renderer, rect: RectF) -> Result<()> {
    let Some(menu) = current() else { return Ok(()) };
    let mut rows = Vec::new();
    let mut y = rect.y + 8.0;

    // Header: icon + name
    if let Some(icon) = &menu.icon {
        let ico = RectF { x: rect.x + PAD_X, y: y + 4.0, w: ICON, h: ICON };
        r.draw_bitmap(icon.w, icon.h, &icon.rgba, ico)?;
    }
    r.draw_text_styled(
        &menu.name,
        RectF { x: rect.x + PAD_X + ICON + 8.0, y, w: rect.w - PAD_X * 2.0 - ICON - 8.0, h: HEAD_H - 8.0 },
        12.5,
        TEXT,
        Align::Left,
        TextStyle::Bold,
    )?;
    y += HEAD_H - 4.0;
    r.fill_rect(RectF { x: rect.x + 8.0, y, w: rect.w - 16.0, h: 1.0 }, DIVIDER)?;
    y += GROUP_GAP;

    if menu.windows.len() > 1 {
        for w in &menu.windows {
            let row = RectF { x: rect.x + 6.0, y, w: rect.w - 12.0, h: ROW_H };
            let title = truncate(&w.title, 36);
            r.draw_text(&title, row, 11.0, TEXT, Align::Left)?;
            rows.push((AppMenuAction::Focus(w.hwnd), row, false));
            y += ROW_H;
        }
        y += GROUP_GAP;
        r.fill_rect(RectF { x: rect.x + 8.0, y: y - 4.0, w: rect.w - 16.0, h: 1.0 }, DIVIDER)?;
    }

    if !menu.exe.is_empty() {
        let row = RectF { x: rect.x + 6.0, y, w: rect.w - 12.0, h: ROW_H };
        r.draw_text("Open new window", inset_label(row), 11.5, TEXT, Align::Left)?;
        rows.push((AppMenuAction::Launch, row, false));
        y += ROW_H;
    }

    {
        let row = RectF { x: rect.x + 6.0, y, w: rect.w - 12.0, h: ROW_H };
        if menu.pin_id.is_some() {
            r.draw_text("Unpin from dock", inset_label(row), 11.5, TEXT, Align::Left)?;
            rows.push((AppMenuAction::Unpin, row, false));
        } else if !menu.exe.is_empty() {
            r.draw_text("Pin to dock", inset_label(row), 11.5, TEXT, Align::Left)?;
            rows.push((AppMenuAction::Pin, row, false));
        }
        y += ROW_H;
    }

    if !menu.windows.is_empty() {
        y += GROUP_GAP;
        r.fill_rect(RectF { x: rect.x + 8.0, y: y - 4.0, w: rect.w - 16.0, h: 1.0 }, DIVIDER)?;
        let row = RectF { x: rect.x + 6.0, y, w: rect.w - 12.0, h: ROW_H };
        let label = if menu.windows.len() > 1 {
            format!("Close {} windows", menu.windows.len())
        } else {
            "Close window".to_owned()
        };
        r.draw_text(&label, inset_label(row), 11.5, DANGER, Align::Left)?;
        rows.push((AppMenuAction::CloseAll, row, true));
    }

    ROWS.with(|r| *r.borrow_mut() = rows);
    Ok(())
}

fn inset_label(row: RectF) -> RectF {
    RectF { x: row.x + 10.0, y: row.y, w: row.w - 20.0, h: row.h }
}

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_owned()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

pub fn hit_test(x: f32, y: f32) -> Option<AppMenuAction> {
    ROWS.with(|rows| {
        rows.borrow()
            .iter()
            .find(|(_, r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
            .map(|(a, _, _)| *a)
    })
}

pub fn apply(action: AppMenuAction, settings: &mut Settings) {
    let Some(menu) = current() else { return };
    match action {
        AppMenuAction::Launch => launch_exe(&menu.exe),
        AppMenuAction::Focus(hwnd) => focus_window(hwnd),
        AppMenuAction::Pin => {
            if !menu.exe.is_empty()
                && !settings.dock_pins.iter().any(|p| p.exe.eq_ignore_ascii_case(&menu.exe))
            {
                settings.dock_pins.push(DockPin {
                    exe: menu.exe.clone(),
                    name: menu.name.clone(),
                });
            }
        }
        AppMenuAction::Unpin => {
            settings.dock_pins.retain(|p| !p.exe.eq_ignore_ascii_case(&menu.exe));
        }
        AppMenuAction::CloseAll => {
            for w in &menu.windows {
                close_window(w.hwnd);
            }
        }
    }
}
