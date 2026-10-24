use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileSettings {
    pub git: bool,
    pub claude: bool,
    pub codex: bool,
    pub cursor: bool,
    pub grok: bool,
    pub music: bool,
    pub cpu: bool,
    pub ram: bool,
    pub battery: bool,
    pub net: bool,
    pub disk: bool,
    pub gpu: bool,
    pub timer: bool,
    pub pomodoro: bool,
    pub stopwatch: bool,
    pub notes: bool,
    pub dayprog: bool,
    pub monthprog: bool,
    pub yearprog: bool,
    pub theme: bool,
    pub brightness: bool,
    pub recyclebin: bool,
    pub screenshot: bool,
    pub recentshots: bool,
    pub search: bool,
    pub worldclock: bool,
    pub analog: bool,
    pub flipclock: bool,
    pub segclock: bool,
    pub daynum: bool,
    pub sun: bool,
    pub hourly: bool,
    pub forecast: bool,
    pub metric: bool,
    pub currency: bool,
    pub screentime: bool,
    pub date: bool,
    pub weather: bool,
    pub focus: bool,
    pub todo: bool,
    pub trash: bool,
    pub downloads: bool,
    pub lang: bool,
}

impl Default for TileSettings {
    fn default() -> Self {
        Self {
            git: true,
            claude: true,
            codex: true,
            cursor: true,
            grok: true,
            music: true,
            cpu: true,
            ram: true,
            battery: false,
            net: false,
            disk: false,
            gpu: false,
            timer: false,
            pomodoro: false,
            stopwatch: false,
            notes: false,
            dayprog: false,
            monthprog: false,
            yearprog: false,
            theme: false,
            brightness: false,
            recyclebin: false,
            screenshot: false,
            recentshots: false,
            search: false,
            worldclock: false,
            analog: false,
            flipclock: false,
            segclock: false,
            daynum: false,
            sun: false,
            hourly: false,
            forecast: false,
            metric: false,
            currency: false,
            screentime: false,
            date: false,
            weather: true,
            focus: false,
            todo: false,
            trash: false,
            downloads: false,
            lang: true,
        }
    }
}

/// Every widget that can sit on the left or right of the dock, with the Settings key it uses.
pub const WIDGET_IDS: [&str; 45] =
    ["git", "claude", "codex", "cursor", "grok", "media", "weather", "cpu", "ram", "focus", "todo", "downloads", "chevron", "cc", "lang", "clock", "battery", "net", "disk", "gpu", "timer", "date", "pomodoro", "stopwatch", "notes", "dayprog", "monthprog", "yearprog", "theme", "brightness", "recyclebin", "screenshot", "recentshots", "search", "worldclock", "analog", "flipclock", "segclock", "daynum", "sun", "hourly", "forecast", "metric", "currency", "screentime"];

/// Always on: these can be moved but not removed (tray chevron, Wi-Fi/volume button, clock).
pub fn is_fixed_widget(id: &str) -> bool {
    matches!(id, "chevron" | "cc" | "clock")
}

/// Which side of the dock each widget is on, and its order (left to right). Widgets missing
/// from both lists (older settings files) are added back on their default side by `normalize`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetLayout {
    pub left: Vec<String>,
    pub right: Vec<String>,
}

impl Default for WidgetLayout {
    fn default() -> Self {
        let own = |ids: &[&str]| -> Vec<String> { ids.iter().map(|s| (*s).to_owned()).collect() };
        let right = own(&["chevron", "cc", "media", "cpu", "ram", "battery", "net", "disk", "gpu", "lang", "date", "clock"]);
        let mut left = own(&["git", "claude", "codex", "cursor", "grok", "weather", "focus", "todo", "downloads", "timer"]);
        for id in WIDGET_IDS {
            if !left.iter().chain(&right).any(|s| s == id) { left.push(id.to_owned()); }
        }
        Self { left, right }
    }
}

impl WidgetLayout {
    /// Drops unknown and duplicate ids, and appends missing ones to their default side.
    pub fn normalize(&mut self) {
        let mut seen = Vec::new();
        for list in [&mut self.left, &mut self.right] {
            list.retain(|id| {
                let ok = WIDGET_IDS.contains(&id.as_str()) && !seen.contains(id);
                if ok {
                    seen.push(id.clone());
                }
                ok
            });
        }
        let defaults = WidgetLayout::default();
        for id in WIDGET_IDS {
            if !seen.iter().any(|s| s == id) {
                if defaults.right.iter().any(|d| d == id) {
                    // Fixed buttons keep their usual spot: chevron and Control Center first.
                    match id {
                        "chevron" => self.right.insert(0, id.to_owned()),
                        "cc" => {
                            let at = usize::from(self.right.first().is_some_and(|s| s == "chevron"));
                            self.right.insert(at, id.to_owned());
                        }
                        _ => self.right.push(id.to_owned()),
                    }
                } else {
                    self.left.push(id.to_owned());
                }
            }
        }
    }

    pub fn is_left(&self, id: &str) -> bool {
        self.left.iter().any(|s| s == id)
    }


    /// Puts `id` on `left`/right so it lands at slot `k` among the widgets currently shown on
    /// that side (`shown`, in order, without `id`); hidden widgets keep their relative place.
    pub fn move_to(&mut self, id: &str, left: bool, shown: &[&str], k: usize) {
        self.left.retain(|s| s != id);
        self.right.retain(|s| s != id);
        let list = if left { &mut self.left } else { &mut self.right };
        let pos = |name: &str, list: &Vec<String>| list.iter().position(|s| s == name);
        let idx = if let Some(next) = shown.get(k).and_then(|n| pos(n, list)) {
            next
        } else if let Some(last) = shown.last().and_then(|n| pos(n, list)) {
            last + 1
        } else if left {
            list.len()
        } else {
            0
        };
        list.insert(idx.min(list.len()), id.to_owned());
    }
    /// Moves `id` to `side` (left = true) at `index` within that side (clamped).
    pub fn place(&mut self, id: &str, left: bool, index: usize) {
        self.left.retain(|s| s != id);
        self.right.retain(|s| s != id);
        let list = if left { &mut self.left } else { &mut self.right };
        list.insert(index.min(list.len()), id.to_owned());
    }
}

impl TileSettings {
    pub fn is_on(&self, id: &str) -> bool {
        match id {
            "git" => self.git,
            "claude" => self.claude,
            "codex" => self.codex,
            "cursor" => self.cursor,
            "grok" => self.grok,
            "media" => self.music,
            "weather" => self.weather,
            "cpu" => self.cpu,
            "ram" => self.ram,
            "battery" => self.battery,
            "net" => self.net,
            "disk" => self.disk,
            "gpu" => self.gpu,
            "timer" => self.timer,
            "pomodoro" => self.pomodoro,
            "stopwatch" => self.stopwatch,
            "notes" => self.notes,
            "dayprog" => self.dayprog,
            "monthprog" => self.monthprog,
            "yearprog" => self.yearprog,
            "theme" => self.theme,
            "brightness" => self.brightness,
            "recyclebin" => self.recyclebin,
            "screenshot" => self.screenshot,
            "recentshots" => self.recentshots,
            "search" => self.search,
            "worldclock" => self.worldclock,
            "analog" => self.analog,
            "flipclock" => self.flipclock,
            "segclock" => self.segclock,
            "daynum" => self.daynum,
            "sun" => self.sun,
            "hourly" => self.hourly,
            "forecast" => self.forecast,
            "metric" => self.metric,
            "currency" => self.currency,
            "screentime" => self.screentime,
            "date" => self.date,
            "focus" => self.focus,
            "lang" => self.lang,
            "todo" => self.todo,
            "trash" => self.trash,
            "downloads" => self.downloads,
            _ => is_fixed_widget(id),
        }
    }

    pub fn set(&mut self, id: &str, on: bool) {
        match id {
            "git" => self.git = on,
            "claude" => self.claude = on,
            "codex" => self.codex = on,
            "cursor" => self.cursor = on,
            "grok" => self.grok = on,
            "media" => self.music = on,
            "weather" => self.weather = on,
            "cpu" => self.cpu = on,
            "ram" => self.ram = on,
            "battery" => self.battery = on,
            "net" => self.net = on,
            "disk" => self.disk = on,
            "gpu" => self.gpu = on,
            "timer" => self.timer = on,
            "pomodoro" => self.pomodoro = on,
            "stopwatch" => self.stopwatch = on,
            "notes" => self.notes = on,
            "dayprog" => self.dayprog = on,
            "monthprog" => self.monthprog = on,
            "yearprog" => self.yearprog = on,
            "theme" => self.theme = on,
            "brightness" => self.brightness = on,
            "recyclebin" => self.recyclebin = on,
            "screenshot" => self.screenshot = on,
            "recentshots" => self.recentshots = on,
            "search" => self.search = on,
            "worldclock" => self.worldclock = on,
            "analog" => self.analog = on,
            "flipclock" => self.flipclock = on,
            "segclock" => self.segclock = on,
            "daynum" => self.daynum = on,
            "sun" => self.sun = on,
            "hourly" => self.hourly = on,
            "forecast" => self.forecast = on,
            "metric" => self.metric = on,
            "currency" => self.currency = on,
            "screentime" => self.screentime = on,
            "date" => self.date = on,
            "focus" => self.focus = on,
            "lang" => self.lang = on,
            "todo" => self.todo = on,
            "trash" => self.trash = on,
            "downloads" => self.downloads = on,
            _ => {}
        }
    }
}

/// Name shown in the widget menus.
pub fn widget_label(id: &str) -> &'static str {
    match id {
        "git" => "Git",
        "claude" => "Claude",
        "codex" => "ChatGPT",
        "cursor" => "Cursor",
        "grok" => "Grok Bot",
        "media" => "Nhạc",
        "weather" => "Thời tiết",
        "cpu" => "CPU",
        "ram" => "RAM",
        "battery" => "Pin",
        "net" => "Tốc độ mạng",
        "disk" => "Dung lượng trống",
        "gpu" => "GPU",
        "timer" => "Bấm giờ",
        "pomodoro" => "Pomodoro",
        "stopwatch" => "Đồng hồ bấm giờ",
        "notes" => "Sổ ghi chú",
        "dayprog" => "Tiến độ ngày",
        "monthprog" => "Tiến độ tháng",
        "yearprog" => "Tiến độ năm",
        "theme" => "Sáng / tối",
        "brightness" => "Độ sáng màn hình",
        "recyclebin" => "Thùng rác",
        "screenshot" => "Chụp màn hình",
        "recentshots" => "Ảnh chụp gần đây",
        "search" => "Tìm kiếm",
        "worldclock" => "Giờ thế giới",
        "analog" => "Đồng hồ kim",
        "flipclock" => "Đồng hồ lật",
        "segclock" => "Đồng hồ LED",
        "daynum" => "Lịch ngày",
        "sun" => "Mặt trời",
        "hourly" => "Thời tiết theo giờ",
        "forecast" => "Dự báo bốn ngày",
        "metric" => "Bitcoin / USD",
        "currency" => "Đổi tiền",
        "screentime" => "Thời gian ứng dụng",
        "date" => "Ngày tháng",
        "focus" => "Hẹn giờ tập trung",
        "lang" => "Ngôn ngữ bàn phím",
        "todo" => "Ghi chú",
        "trash" => "Thùng rác",
        "downloads" => "Tải xuống",
        "chevron" => "Khay hệ thống",
        "cc" => "Wi-Fi và âm lượng",
        "clock" => "Đồng hồ",
        _ => "",
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn extended_widgets_default_off_and_roundtrip_with_local_content() {
        let mut s:Settings=serde_json::from_str("{}").unwrap();
        for id in &WIDGET_IDS[22..] {
            assert!(!s.widget_shown(id));
            assert!(!widget_label(id).is_empty());
            assert_eq!(s.layout.left.iter().chain(&s.layout.right).filter(|v|v.as_str()==*id).count(),1,"{id}");
            s.tiles.set(id,true);
            assert!(s.widget_shown(id));
        }
        s.note="Ghi chú tiếng Việt 📝".into();
        s.world_cities=vec![WorldCity::new("Delhi",330)];
        s.currency_from="EUR".into();s.currency_to="VND".into();s.currency_amount=100.5;
        let roundtrip:Settings=serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(s,roundtrip);
        for id in &WIDGET_IDS[22..] {s.tiles.set(id,false);assert!(!s.widget_shown(id));}
    }

    #[test]
    fn new_widgets_load_from_old_settings_and_persist_switches_and_positions() {
        let mut s: Settings = serde_json::from_str(r#"{"tiles":{"cpu":false},"layout":{"left":["git"],"right":["clock"]}}"#).unwrap();
        s.layout.normalize();
        for id in ["battery", "net", "disk", "gpu", "timer", "date"] {
            assert!(!s.widget_shown(id), "{id} defaults off for old settings");
            assert!(!widget_label(id).is_empty());
            assert!(!is_fixed_widget(id));
            s.tiles.set(id, true);
            assert!(s.widget_shown(id));
            s.layout.place(id, true, 0);
        }
        let loaded: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(loaded, s);
        assert!(!loaded.tiles.cpu);
        for id in ["battery", "net", "disk", "gpu", "timer", "date"] {
            assert!(loaded.layout.is_left(id));
            s.tiles.set(id, false);
            assert!(!s.widget_shown(id));
        }
    }

    #[test]
    fn widget_layout_normalize_fills_missing_and_drops_unknown() {
        let mut l = WidgetLayout { left: vec!["git".into(), "bogus".into(), "git".into()], right: vec![] };
        l.normalize();
        assert_eq!(l.left.iter().filter(|s| *s == "git").count(), 1);
        assert!(!l.left.iter().chain(&l.right).any(|s| s == "bogus"));
        assert!(l.right.iter().any(|s| s == "media"));
        for id in WIDGET_IDS {
            assert_eq!(l.left.iter().chain(&l.right).filter(|s| *s == id).count(), 1, "{id}");
        }
    }

    #[test]
    fn widget_layout_place_moves_between_sides() {
        let mut l = WidgetLayout::default();
        l.place("media", true, 1);
        assert_eq!(l.left[1], "media");
        assert!(!l.right.iter().any(|s| s == "media"));
        l.place("git", false, 99);
        assert_eq!(l.right.last().map(String::as_str), Some("git"));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DockPin {
    pub exe: String,
    pub name: String,
}

impl Default for DockPin {
    fn default() -> Self {
        Self { exe: String::new(), name: String::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub glass_opacity: u8,
    /// "hidden" or "autohide".
    pub taskbar_mode: String,
    pub launch_at_startup: bool,
    /// SmartDock's own Shell_TrayWnd listener (tray icons in the ^ popup). Kill switch.
    pub tray_service: bool,
    pub tiles: TileSettings,
    /// Side and order of the optional widgets (drag them or use the right-click menu).
    pub layout: WidgetLayout,
    pub weather_location: String,
    /// "clock" or "tile".
    pub weather_position: String,
    pub git_folders: Vec<PathBuf>,
    pub git_author: String,
    pub shortcut: String,
    /// SmartDock-owned pins (never written to the Windows taskbar pins folder).
    pub dock_pins: Vec<DockPin>,
    /// Show the most-used apps as pinned launchers (merged after manual pins).
    pub auto_pin: bool,
    /// How many auto pins to show.
    pub auto_pin_count: u32,
    /// Auto pins the user removed with Unpin (exe paths); never auto-pinned again.
    pub auto_pin_hidden: Vec<String>,
    /// Where the Claude usage comes from: "login" (Claude Code sign-in) or "apikey" (ANTHROPIC_API_KEY).
    pub claude_source: String,
    /// Monthly API budget in USD shown next to the Claude spend (chosen in Settings).
    pub claude_monthly_budget: u32,
    /// "normal" or "compact": compact narrows the widgets.
    pub widget_size: String,
    /// "glass", "midnight" or "black": material of the widget tiles.
    pub dock_style: String,
    /// Liquid Glass sheen over the desktop icons (a click-through overlay).
    pub desktop_glass: bool,
    /// "compact" (red time block) or "wide" (weekday + date, time + weather).
    pub clock_style: String,
    /// Items of the To-do widget.
    pub todos: Vec<TodoItem>,
    /// Latest local note, edited directly in its popup.
    pub note: String,
    /// City labels and fixed UTC offsets in minutes (editable in settings.json).
    pub world_cities: Vec<WorldCity>,
    pub currency_from: String,
    pub currency_to: String,
    pub currency_amount: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            glass_opacity: 55,
            taskbar_mode: "hidden".to_owned(),
            launch_at_startup: false,
            tray_service: true,
            tiles: TileSettings::default(),
            layout: WidgetLayout::default(),
            weather_location: "Hanoi".to_owned(),
            weather_position: "clock".to_owned(),
            git_folders: Vec::new(),
            git_author: String::new(),
            shortcut: "Ctrl+Alt+D".to_owned(),
            dock_pins: Vec::new(),
            auto_pin: true,
            auto_pin_count: 8,
            auto_pin_hidden: Vec::new(),
            claude_source: "login".to_owned(),
            claude_monthly_budget: 100,
            widget_size: "normal".to_owned(),
            dock_style: "glass".to_owned(),
            desktop_glass: false,
            clock_style: "compact".to_owned(),
            todos: Vec::new(),
            note: String::new(),
            world_cities: vec![WorldCity::new("Tokyo", 540), WorldCity::new("London", 0), WorldCity::new("New York", -300)],
            currency_from: "USD".into(),
            currency_to: "VND".into(),
            currency_amount: 1.0,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from).map(|dir| dir.join("SmartDock").join("settings.json"))
}

/// Reloads the settings only when the file's modified time differs from `*seen` (starts as `None`,
/// so the first call always loads). Saves the per-second read + JSON parse when nothing changed.
pub fn load_if_changed(seen: &mut Option<Option<std::time::SystemTime>>) -> Option<Settings> {
    let modified = path().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok());
    if *seen == Some(modified) {
        return None;
    }
    *seen = Some(modified);
    Some(load())
}

pub fn load() -> Settings {
    let Some(path) = path() else { return Settings::default() };
    let Ok(text) = std::fs::read_to_string(path) else { return Settings::default() };
    let mut settings: Settings = serde_json::from_str(&text).unwrap_or_default();
    settings.glass_opacity = settings.glass_opacity.min(95);
    settings.layout.normalize();
    if settings.taskbar_mode != "autohide" && settings.taskbar_mode != "hidden" {
        settings.taskbar_mode = "hidden".to_owned();
    }
    if settings.weather_location.trim().is_empty() {
        settings.weather_location = "Hanoi".to_owned();
    }
    if settings.weather_position != "clock" && settings.weather_position != "tile" {
        settings.weather_position = "clock".to_owned();
    }
    settings
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("APPDATA is unavailable"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(settings)?)
}

pub fn sync_startup(enabled: bool) {
    let value_name: Vec<u16> = "SmartDock".encode_utf16().chain(Some(0)).collect();
    unsafe {
        if enabled {
            let Ok(exe) = std::env::current_exe() else { return };
            let command = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                PCWSTR(value_name.as_ptr()),
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                PCWSTR(value_name.as_ptr()),
            );
        }
    }
}

impl Settings {
    /// Whether a widget is drawn on the dock: switched on and, for weather, set to be its own tile.
    pub fn widget_shown(&self, id: &str) -> bool {
        self.tiles.is_on(id) && (id != "weather" || self.weather_position == "tile")
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;

    #[test]
    fn widget_layout_move_to_slots_among_shown() {
        let mut l = WidgetLayout::default(); // left: git claude codex cursor grok weather
        // weather is hidden: shown = git claude codex cursor grok. Drop "media" at slot 2.
        l.move_to("media", true, &["git", "claude", "codex", "cursor", "grok"], 2);
        assert_eq!(&l.left[..4], ["git", "claude", "media", "codex"]);
        // Past the last shown widget: lands right after it, before the hidden weather.
        l.move_to("cpu", true, &["git", "claude", "media", "codex", "cursor", "grok"], 6);
        let g = l.left.iter().position(|s| s == "grok").unwrap();
        assert_eq!(l.left[g + 1], "cpu");
        // Empty right side: goes first.
        let mut r = WidgetLayout { left: vec!["git".into()], right: vec![] };
        r.move_to("ram", false, &[], 0);
        assert_eq!(r.right, ["ram"]);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

pub const DOCK_STYLES: [(&str, &str); 3] = [("glass", "Kính"), ("midnight", "Midnight"), ("black", "Đen")];

impl Settings {
    /// Width factor of the widgets for the chosen size.
    pub fn widget_scale(&self) -> f32 {
        if self.widget_size == "compact" { 0.86 } else { 1.0 }
    }

    pub fn style_label(&self) -> &'static str {
        DOCK_STYLES.iter().find(|(id, _)| *id == self.dock_style).map_or("Kính", |(_, l)| l)
    }

    pub fn cycle_style(&mut self) {
        let i = DOCK_STYLES.iter().position(|(id, _)| *id == self.dock_style).unwrap_or(0);
        self.dock_style = DOCK_STYLES[(i + 1) % DOCK_STYLES.len()].0.to_owned();
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldCity { pub name: String, pub offset_minutes: i32 }
impl WorldCity {
    pub fn new(name: &str, offset_minutes: i32) -> Self { Self { name: name.into(), offset_minutes } }
}
