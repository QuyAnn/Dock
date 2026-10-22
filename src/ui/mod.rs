//! Direct2D/DirectComposition rendering. Owns no Win32 state logic.
pub mod renderer; // D2D device, acrylic/blur, rounded rect, text (DirectWrite)
pub mod dock;     // dock layout: tiles, pill/full width, hit testing
pub mod popup;    // popup window per tile, created on open, destroyed on close
