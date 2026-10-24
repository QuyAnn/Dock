//! Win32 platform layer. Each file owns one concern; see docs in each.
pub mod kbd;
pub mod appbar;   // AppBar registration, dock placement, taskbar hide/restore + crash recovery
pub mod dpi;      // per-monitor-v2 awareness and DIP/physical-pixel conversions
pub mod monitors; // EnumDisplayMonitors + primary/secondary selection (unit-tested)
pub mod tasks;    // window list, shell hook, activate window, attention flags
pub mod icons;    // app icons (exe -> RGBA, cached)
pub mod system;   // media (SMTC), Core Audio volume/mute, radios, brightness, Wi-Fi
pub mod wallpaper; // desktop wallpaper path + WIC decode, change poll (Liquid Glass source)
pub mod traysvc;  // our Shell_TrayWnd listener: learns tray icons, forwards everything to Explorer
pub mod grokbot;  // Grok Bot desktop app resolve/launch (never reads %USERPROFILE%\.grokbot)
pub mod shellinfo; // Recycle Bin / Downloads readings, clipboard text, empty bin
pub mod deskglass; // Liquid Glass sheen over the desktop icons (click-through overlay on the desktop list view)
pub mod widget_tools;
