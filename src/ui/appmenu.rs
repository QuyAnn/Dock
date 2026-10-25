//! Right-click context menu for centre app icons (Dock-Rc AppMenu).
//! Rendered inside the shared NOACTIVATE popup so focus stays on the foreign app.
use windows::core::Result;

use super::dock::RectF;
use super::renderer::{Align, Renderer, TextStyle};
use crate::model::WindowEntry;
use crate::settings::{DockPin, Settings};
use crate::win::icons::IconBitmap;
use crate::win::tasks::{focus_window, launch_exe};

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
    /// Turn auto-pinning off for all apps (Settings › "Tự động ghim app").
    DisableAutoPin,
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
        .map(|p| format!("dock:{}", p.exe.to_ascii_lowercase()))
        .or_else(|| {
            crate::data::usage::is_auto_pinned(exe, settings).then(|| format!("auto:{}", exe.to_ascii_lowercase()))
        });
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
    // launch + pin/unpin (+ "turn off auto-pin" for auto-pinned apps)
    h += ROW_H * 2.0 + GROUP_GAP;
    if is_auto(menu) {
        h += ROW_H;
    }
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

    if is_auto(&menu) {
        let row = RectF { x: rect.x + 6.0, y, w: rect.w - 12.0, h: ROW_H };
        r.draw_text("Tắt tự động ghim app", inset_label(row), 11.5, TEXT, Align::Left)?;
        rows.push((AppMenuAction::DisableAutoPin, row, false));
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

/// The app is on the dock only because of auto-pin (not a manual pin).
fn is_auto(menu: &AppMenuData) -> bool {
    menu.pin_id.as_deref().is_some_and(|id| id.starts_with("auto:"))
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
            settings.auto_pin_hidden.retain(|h| !h.eq_ignore_ascii_case(&menu.exe));
        }
        AppMenuAction::Unpin => {
            let auto = crate::data::usage::is_auto_pinned(&menu.exe, settings);
            settings.dock_pins.retain(|p| !p.exe.eq_ignore_ascii_case(&menu.exe));
            // Remember an unpinned auto pin so it never comes back on its own.
            if auto && !settings.auto_pin_hidden.iter().any(|h| h.eq_ignore_ascii_case(&menu.exe)) {
                settings.auto_pin_hidden.push(menu.exe.clone());
            }
        }
        AppMenuAction::DisableAutoPin => {
            settings.auto_pin = false;
            crate::log("autopin: turned off from the app menu");
        }
        AppMenuAction::CloseAll => {
            crate::log(&format!("appmenu: close {} window(s) of {}", menu.windows.len(), menu.name));
            crate::win::tasks::close_windows(menu.windows.iter().map(|w| w.hwnd).collect());
        }
    }
}

// ---------- dock menu (right-click on the dock, not on an app icon) ----------

pub const DOCK_MENU_W: f32 = 220.0;

/// What a dock menu row does. Widget ids are the `settings::WIDGET_IDS` constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockMenuAction {
    Settings,
    /// Open the widget gallery.
    Gallery,
    /// Switch the widget to the other side of the dock.
    MoveSide(&'static str),
    /// Hide the widget (its switch in Settings goes off).
    Remove(&'static str),
    /// Toggle compact / normal widget width.
    CycleSize,
    /// Next dock material (glass, midnight, black).
    CycleStyle,
    /// Liquid Glass sheen over the desktop icons on / off.
    ToggleDesktopGlass,
    /// Clock widget: compact or wide layout.
    CycleClock,
    /// A widget-specific action run by `run_widget_action` in main.
    Extra(&'static str),
    /// Show a hidden widget again.
    Add(&'static str),
}

enum MenuItem {
    Row(DockMenuAction, String, &'static str),
    Header(&'static str),
    Divider,
}

thread_local! {
    static DOCK_MENU: std::cell::RefCell<Vec<MenuItem>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn item_h(item: &MenuItem) -> f32 {
    match item {
        MenuItem::Row(..) => ROW_H,
        MenuItem::Header(_) => 22.0,
        MenuItem::Divider => GROUP_GAP,
    }
}

/// Builds the menu for a right-click: `target` is the widget under the pointer, if any. Returns
/// its height in DIPs.
pub fn open_dock_menu(target: Option<&'static str>, settings: &Settings) -> f32 {
    use crate::settings::{widget_label, WIDGET_IDS};
    let mut items = Vec::new();
    if let Some(id) = target {
        let to = if settings.layout.is_left(id) { "phải" } else { "trái" };
        items.push(MenuItem::Header(widget_label(id)));
        match id {
            "pomodoro" => {
                items.push(MenuItem::Row(DockMenuAction::Extra("pomodoro_skip"), "Bỏ qua giai đoạn".to_owned(), "\u{E893}"));
                items.push(MenuItem::Row(DockMenuAction::Extra("pomodoro_reset"), "Đặt lại Pomodoro".to_owned(), "\u{E777}"));
            }
            "stopwatch" => items.push(MenuItem::Row(DockMenuAction::Extra("stopwatch_reset"), "Đặt lại bấm giờ".to_owned(), "\u{E777}")),
            "timer" => items.push(MenuItem::Row(DockMenuAction::Extra("timer_reset"), "Đặt lại bấm giờ".to_owned(), "\u{E777}")),
            "focus" => items.push(MenuItem::Row(DockMenuAction::Extra("focus_reset"), "Đặt lại hẹn giờ".to_owned(), "\u{E777}")),
            "trash" => {
                items.push(MenuItem::Row(DockMenuAction::Extra("trash"), "Mở thùng rác".to_owned(), "\u{E8B7}"));
                items.push(MenuItem::Row(DockMenuAction::Extra("trash_empty"), "Dọn thùng rác".to_owned(), "\u{E74D}"));
            }
            _ => {}
        }
        // The Recycle Bin is a fixed dock icon, not a widget: nothing to move or remove.
        if crate::settings::WIDGET_IDS.contains(&id) {
            items.push(MenuItem::Row(DockMenuAction::MoveSide(id), format!("Chuyển sang bên {to}"), "\u{E8AB}"));
        }
        if crate::settings::WIDGET_IDS.contains(&id) && !crate::settings::is_fixed_widget(id) {
            items.push(MenuItem::Row(DockMenuAction::Remove(id), "Gỡ widget".to_owned(), "\u{E74D}"));
        }
        items.push(MenuItem::Divider);
    }
    let hidden: Vec<&'static str> = WIDGET_IDS.iter().copied().filter(|id| !settings.widget_shown(id)).collect();
    if !hidden.is_empty() {
        items.push(MenuItem::Header("Thêm widget"));
        // The gallery holds the complete catalog. Keep this shortcut menu within a screen.
        for id in hidden.into_iter().take(6) {
            items.push(MenuItem::Row(DockMenuAction::Add(id), widget_label(id).to_owned(), "\u{E710}"));
        }
        items.push(MenuItem::Divider);
    }
    items.push(MenuItem::Header("Giao diện"));
    let size = if settings.widget_size == "compact" { "Gọn" } else { "Vừa" };
    items.push(MenuItem::Row(DockMenuAction::CycleSize, format!("Cỡ widget: {size}"), "\u{E8A3}"));
    items.push(MenuItem::Row(DockMenuAction::CycleStyle, format!("Kiểu dock: {}", settings.style_label()), "\u{E771}"));
    let desk = if settings.desktop_glass { "Bật" } else { "Tắt" };
    items.push(MenuItem::Row(DockMenuAction::ToggleDesktopGlass, format!("Icon desktop kính: {desk}"), "\u{E7F4}"));
    let clock = if settings.clock_style == "wide" { "Rộng" } else { "Gọn" };
    items.push(MenuItem::Row(DockMenuAction::CycleClock, format!("Kiểu đồng hồ: {clock}"), "\u{E823}"));
    items.push(MenuItem::Divider);
    items.push(MenuItem::Row(DockMenuAction::Gallery, "Thư viện widget…".to_owned(), "\u{E8A9}"));
    items.push(MenuItem::Row(DockMenuAction::Settings, "Cài đặt SmartDock".to_owned(), "\u{E713}"));
    let h = 12.0 + items.iter().map(item_h).sum::<f32>();
    DOCK_MENU.with(|m| *m.borrow_mut() = items);
    h
}

fn dock_menu_rows() -> Vec<(DockMenuAction, RectF)> {
    DOCK_MENU.with(|m| {
        let mut y = 6.0;
        let mut out = Vec::new();
        for item in m.borrow().iter() {
            if let MenuItem::Row(a, _, _) = item {
                out.push((*a, RectF { x: 6.0, y, w: DOCK_MENU_W - 12.0, h: ROW_H }));
            }
            y += item_h(item);
        }
        out
    })
}

pub fn paint_dock_menu(r: &mut Renderer, _rect: RectF) -> Result<()> {
    DOCK_MENU.with(|m| -> Result<()> {
        let mut y = 6.0;
        for item in m.borrow().iter() {
            match item {
                MenuItem::Row(action, label, glyph) => {
                    let row = RectF { x: 6.0, y, w: DOCK_MENU_W - 12.0, h: ROW_H };
                    let color = if matches!(action, DockMenuAction::Remove(_)) { DANGER } else { TEXT };
                    r.draw_text_styled(glyph, RectF { x: row.x + 8.0, y, w: 18.0, h: ROW_H }, 13.0, color, Align::Center, TextStyle::Icon)?;
                    r.draw_text(label, RectF { x: row.x + 32.0, y, w: row.w - 40.0, h: ROW_H }, 11.5, color, Align::Left)?;
                }
                MenuItem::Header(text) => {
                    r.draw_text_styled(text, RectF { x: 14.0, y, w: DOCK_MENU_W - 28.0, h: 22.0 }, 10.0, 0xFFFF_FFA0, Align::Left, TextStyle::Bold)?;
                }
                MenuItem::Divider => {
                    r.fill_rect(RectF { x: 8.0, y: y + GROUP_GAP / 2.0, w: DOCK_MENU_W - 16.0, h: 1.0 }, DIVIDER)?;
                }
            }
            y += item_h(item);
        }
        Ok(())
    })
}

/// The row under (x, y), if any.
pub fn dock_menu_hit(x: f32, y: f32) -> Option<DockMenuAction> {
    dock_menu_rows()
        .into_iter()
        .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
        .map(|(a, _)| a)
}

/// Applies a widget action to the settings. `Settings` is handled by the caller.
pub fn apply_dock_menu(action: DockMenuAction, settings: &mut Settings) {
    match action {
        DockMenuAction::Settings | DockMenuAction::Gallery | DockMenuAction::Extra(_) => {}
        DockMenuAction::CycleSize => {
            settings.widget_size = if settings.widget_size == "compact" { "normal" } else { "compact" }.to_owned();
        }
        DockMenuAction::CycleStyle => settings.cycle_style(),
        DockMenuAction::ToggleDesktopGlass => settings.desktop_glass = !settings.desktop_glass,
        DockMenuAction::CycleClock => {
            settings.clock_style = if settings.clock_style == "wide" { "compact" } else { "wide" }.to_owned();
        }
        DockMenuAction::MoveSide(id) => {
            let to_left = !settings.layout.is_left(id);
            // Joins the new side next to the centre: end of the left list, start of the right one.
            let index = if to_left { usize::MAX } else { 0 };
            settings.layout.place(id, to_left, index);
        }
        DockMenuAction::Remove(id) => {
            if id == "weather" {
                settings.weather_position = "clock".to_owned();
            } else {
                settings.tiles.set(id, false);
            }
        }
        DockMenuAction::Add(id) => {
            settings.tiles.set(id, true);
            if id == "weather" {
                settings.weather_position = "tile".to_owned();
            }
        }
    }
}

#[cfg(test)]
mod dock_menu_tests {
    use super::*;

    #[test]
    fn dock_layout_dock_menu_settings_row_hit() {
        let mut settings = Settings::default();
        for id in crate::settings::WIDGET_IDS {
            settings.tiles.set(id, true);
        }
        settings.weather_position = "tile".to_owned();
        let h = open_dock_menu(None, &settings);
        // Everything is shown: no "add widget" rows; Settings is the last row and is hit in its middle.
        let (last, rect) = dock_menu_rows().pop().unwrap();
        assert_eq!(last, DockMenuAction::Settings);
        assert_eq!(dock_menu_hit(DOCK_MENU_W / 2.0, rect.y + rect.h / 2.0), Some(DockMenuAction::Settings));
        assert_eq!(dock_menu_hit(DOCK_MENU_W / 2.0, h - 1.0), None);
        assert_eq!(dock_menu_hit(1.0, rect.y + rect.h / 2.0), None);
    }

    #[test]
    fn dock_menu_widget_rows_and_actions() {
        let mut settings = Settings::default();
        settings.tiles.set("ram", false);
        let h = open_dock_menu(Some("media"), &settings);
        let rows = dock_menu_rows();
        let actions: Vec<_> = rows.iter().map(|(a, _)| *a).collect();
        assert!(actions.contains(&DockMenuAction::MoveSide("media")));
        assert!(actions.contains(&DockMenuAction::Remove("media")));
        assert!(actions.contains(&DockMenuAction::Add("ram")));
        assert!(rows.iter().all(|(_, r)| r.y + r.h <= h));
        apply_dock_menu(DockMenuAction::MoveSide("media"), &mut settings);
        assert!(settings.layout.is_left("media"));
        apply_dock_menu(DockMenuAction::Remove("media"), &mut settings);
        assert!(!settings.tiles.is_on("media"));
        apply_dock_menu(DockMenuAction::Add("ram"), &mut settings);
        assert!(settings.tiles.is_on("ram"));
    }
}
