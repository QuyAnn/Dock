//! Direct2D/DirectComposition rendering. Owns no Win32 state logic.
use std::cell::Cell;

thread_local! {
    static FRAME_TIMER: Cell<Option<fn()>> = const { Cell::new(None) };
}

/// Main registers this so popup/dock surface code can arm the shared ~60 Hz UI timer.
pub fn set_frame_timer_hook(f: Option<fn()>) {
    FRAME_TIMER.with(|c| c.set(f));
}

pub fn arm_frame_timer() {
    if let Some(f) = FRAME_TIMER.with(Cell::get) {
        f();
    }
}

pub mod renderer; // D2D device, acrylic/blur, rounded rect, text (DirectWrite)
pub mod dock;     // dock layout: tiles, pill/full width, hit testing
pub mod popup;    // popup window per tile, created on open, destroyed on close
pub mod popup_content; // popup body drawing per tile kind
pub mod glass; // dock glass: acrylic tint and tile fill/rim
pub mod gallery; // widget gallery sheet
pub mod calendar; // clock popup: month calendar + world clocks
pub mod anim; // easing, interpolation, shared hover lift for dock tiles
pub mod apps; // apps tile: window icons, running dots, attention badge, hit testing
pub mod appmenu; // right-click context menu for centre app icons (NOACTIVATE)
pub mod thumbnails; // DWM live peeks above dock on app-icon hover
pub mod squircle; // macOS squircle icon composition and drop shadow (CPU)
pub mod tiles; // data tiles: DataState, TileView builders, git heatmap
pub mod control_center; // Control Center panel: toggles, volume/brightness sliders, hit testing
pub mod cc_preview; // Windows-only offscreen Control Center PNG verification command
pub mod svgpath; // SVG path subset (M L H V C A Z) drawn as D2D geometry, plus AI logo paths
pub mod ccicons; // Control Center / popup vector icon set (one grid, one stroke weight)
pub mod sficons; // macOS SF-Symbols-style Wi-Fi / speaker status icons (D2D geometry)
pub mod liquid; // Liquid Glass dock background: blur, saturate, lens refraction (CPU, cached)
pub mod trayview; // SmartDock's own tray overflow popup (hidden notification icons)
pub mod more_widgets;
