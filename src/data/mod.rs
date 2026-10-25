//! Pure data sources (no UI). Each returns plain structs from src/model.rs.
pub mod stats;    // CPU/RAM via GetSystemTimes / GlobalMemoryStatusEx
pub mod git;      // git log heatmap (spawn git, parse)
pub mod ai;       // Claude/Codex/Cursor usage from local credentials + HTTP
pub mod weather;  // Open-Meteo or existing provider
pub mod history;  // rolling 2-minute CPU and RAM samples for sparklines
pub mod http;     // ureq Agent with native-tls connector (required for HTTPS)
pub mod usage;    // foreground-time / UserAssist ranking for auto-pinned launchers
pub mod focus;    // Pomodoro-style focus timer (work / break), advanced once a second
pub mod widgets;  // Battery, network, disk and GPU samples on a background worker
pub mod timer;    // Click-to-start/pause stopwatch shared by all docks
pub mod productivity;
pub mod calendar_progress;
pub mod finance;
pub mod screentime;
