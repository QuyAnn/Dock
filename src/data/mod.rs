//! Pure data sources (no UI). Each returns plain structs from src/model.rs.
pub mod stats;    // CPU/RAM via GetSystemTimes / GlobalMemoryStatusEx
pub mod git;      // git log heatmap (spawn git, parse)
pub mod ai;       // Claude/Codex/Cursor/Grok usage from local credentials + HTTP
pub mod weather;  // Open-Meteo or existing provider
pub mod history;  // rolling 2-minute CPU and RAM samples for sparklines
pub mod http;     // ureq Agent with native-tls connector (required for HTTPS)
