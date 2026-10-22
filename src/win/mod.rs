//! Win32 platform layer. Each file owns one concern; see docs in each.
pub mod appbar;   // AppBar registration, dock placement, taskbar hide/restore + crash recovery
pub mod tasks;    // window list, shell hook, activate window, attention flags
pub mod icons;    // app icons (exe -> RGBA, cached)
pub mod system;   // media (SMTC), Core Audio volume/mute, radios, brightness, Wi-Fi
pub mod wallpaper; // desktop wallpaper path + WIC decode, change poll (Liquid Glass source)
