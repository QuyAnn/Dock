//! Dock glass: acrylic backdrop tint and the translucent tile/bar fill with a rim light.
//! Colors are 0xRRGGBBAA for the renderer; the acrylic tint is 0xAARRGGBB.
use windows::core::Result;
use windows::Win32::Foundation::HWND;

use super::dock::RectF;
use super::renderer::{apply_acrylic, Renderer};

/// Default Settings > Glass opacity (percent).
pub const DEFAULT_OPACITY_PCT: u8 = 70;
/// Same cap as the CSS `glassVars` (alpha never exceeds 0.95).
const MAX_OPACITY_PCT: u8 = 95;
/// Smoky frosted tint, rgb(40, 40, 44), as in styles.css `.glass-liquid::before`.
const TINT_RGB: u32 = 0x0028_282C;
/// White at ~10% over the acrylic.
const FILL_RGBA: u32 = 0xFFFF_FF1A;
/// White at ~18% rim light.
const BORDER_RGBA: u32 = 0xFFFF_FF2E;
const BORDER_WIDTH: f32 = 1.0;

/// Enables acrylic blur on the dock window, tinted by `opacity_pct` (0..=95 effective).
/// If the API is missing, the window keeps only the CSS fallback.
pub fn apply_dock_glass(hwnd: HWND, opacity_pct: u8) {
    let pct = opacity_pct.min(MAX_OPACITY_PCT) as u32;
    // Scale 0..=100 to 0..=255 with rounding.
    let alpha = (pct * 255 + 50) / 100;
    let tint_argb = (alpha << 24) | TINT_RGB;
    // Returns false on pre-1809 Windows; nothing else to do in that case.
    let _ = apply_acrylic(hwnd, tint_argb);
}

/// Draws the translucent fill and 1px light border for a tile or the dock bar.
/// Call between `Renderer::begin` and `Renderer::end`, after the backdrop is set.
pub fn draw_glass_fill(r: &mut Renderer, rect: RectF, radius: f32) -> Result<()> {
    r.fill_rounded_rect(rect, radius, FILL_RGBA)?;
    r.stroke_rounded_rect(rect, radius, BORDER_RGBA, BORDER_WIDTH)
}
