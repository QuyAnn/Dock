//! Direct2D/DirectComposition rendering. Owns no Win32 state logic.
pub mod renderer; // D2D device, acrylic/blur, rounded rect, text (DirectWrite)
pub mod dock;     // dock layout: tiles, pill/full width, hit testing
pub mod popup;    // popup window per tile, created on open, destroyed on close
pub mod popup_content; // popup body drawing per tile kind
pub mod glass; // dock glass: acrylic tint and tile fill/rim
pub mod apps; // apps tile: window icons, running dots, attention badge, hit testing
pub mod tiles; // data tiles: DataState, TileView builders, git heatmap
pub mod control_center; // Control Center panel: toggles, volume/brightness sliders, hit testing
