//! Shared plain types between data, win and ui layers.
#[derive(Clone, Debug, Default)]
pub struct Stats { pub cpu_pct: f32, pub ram_used_mb: u64, pub ram_total_mb: u64 }

#[derive(Clone, Debug, Default)]
pub struct WidgetStats {
    pub battery: Option<Battery>,
    /// Download / upload bytes per second; None until two samples are available.
    pub net: Option<(f64, f64)>,
    /// Free bytes on C: and D:; None for missing or inaccessible drives.
    pub disk_free: [Option<u64>; 2],
    pub gpu_pct: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct Battery {
    pub percent: Option<u8>,
    pub charging: bool,
    pub plugged_in: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Media { pub title: String, pub artist: String, pub playing: bool }

#[derive(Clone, Debug)]
pub struct WindowEntry {
    pub hwnd: isize,
    pub title: String,
    pub exe: String,
    pub aumid: String,
    pub package_path: String,
    pub attention: bool,
    pub minimized: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GitDay { pub date: String, pub commits: u32 }

#[derive(Clone, Debug, Default)]
pub struct AiWindow { pub label: String, pub used_pct: f32, pub reset_text: String }

#[derive(Clone, Debug, Default)]
pub struct AiUsage {
    pub name: String,
    pub session_pct: f32,
    pub reset_text: String,
    pub windows: Vec<AiWindow>,
    /// Month-to-date API spend in USD from the Admin cost report (Claude only).
    pub cost_usd: Option<f64>,
}
