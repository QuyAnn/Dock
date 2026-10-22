use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub weather: bool,
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
            weather: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub glass_opacity: u8,
    /// "hidden" or "autohide".
    pub taskbar_mode: String,
    pub launch_at_startup: bool,
    pub tiles: TileSettings,
    pub weather_location: String,
    /// "clock" or "tile".
    pub weather_position: String,
    pub git_folders: Vec<PathBuf>,
    pub git_author: String,
    pub shortcut: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            glass_opacity: 55,
            taskbar_mode: "hidden".to_owned(),
            launch_at_startup: false,
            tiles: TileSettings::default(),
            weather_location: "Hanoi".to_owned(),
            weather_position: "clock".to_owned(),
            git_folders: Vec::new(),
            git_author: String::new(),
            shortcut: "Ctrl+Alt+D".to_owned(),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from).map(|dir| dir.join("SmartDock").join("settings.json"))
}

pub fn load() -> Settings {
    let Some(path) = path() else { return Settings::default() };
    let Ok(text) = std::fs::read_to_string(path) else { return Settings::default() };
    let mut settings: Settings = serde_json::from_str(&text).unwrap_or_default();
    settings.glass_opacity = settings.glass_opacity.min(95);
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
