//! Liquid Glass dock background: CPU port of `renderGlass` in glass.ts.
//! Steps: wallpaper region under the bar (plus margin) sampled on a 1/4 grid, gaussian blur
//! (3 box passes), CSS-style saturate, then the rounded-rect lens (edge refraction with an R/B
//! channel split) at full bar resolution. Output is straight RGBA sized to the bar; the corners
//! are alpha-clipped. Render once per wallpaper/position/size change and cache the result.
//! Units: `bar` and `screen` are physical pixels (screen = display size, bar = display-local).
use super::dock::{RectF, DOCK_H};
use crate::win::wallpaper::{FitMode, WallpaperPixels};

/// Same fields as `LensOptions` in LiquidGlass.tsx. CSS px, scaled by the bar's DPI.
#[derive(Clone, Copy, Debug)]
pub struct LensOptions {
    pub radius: f32,
    pub band: f32,
    pub strength: f32,
    pub blur: f32,
    pub saturate: f32,
    pub aberration: f32,
}

/// DOCK_LENS from LiquidGlass.tsx.
pub const DOCK_LENS: LensOptions = LensOptions {
    radius: 16.0,
    band: 9.0,
    strength: 3.0,
    blur: 26.0,
    saturate: 1.35,
    aberration: 0.04,
};

/// Straight RGBA, `w * h * 4` bytes. Draw with `Renderer::draw_bitmap` at the bar rect.
pub struct LiquidBitmap {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    /// Dock: unblurred wallpaper crop of the same size, fully opaque, painted as the bar itself so
    /// the dock looks transparent between the tiles. Control Center over a bright page: a lightly
    /// blurred frost for the pale glass (see `add_bright_detail`). Otherwise empty.
    pub plain: Vec<u8>,
}

/// Blur grid step in physical px. The blur runs on this grid; the lens samples it bilinearly.
const GRID: i32 = 4;
/// Upper bound for the scale used to turn CSS-px lens parameters into physical px.
const MAX_DPR: f32 = 4.0;

/// Renders the refracted wallpaper behind the dock bar with `DOCK_LENS`.
pub fn render_liquid(wallpaper: &WallpaperPixels, bar: RectF, screen: (i32, i32)) -> LiquidBitmap {
    // The bar spans the whole dock height, so its height over DOCK_H is the device scale.
    let dpr = (bar.h / DOCK_H).clamp(1.0, MAX_DPR);
    let place = Placement::new(wallpaper, screen);
    let mut bitmap = render_with(&|x, y| place.sample(wallpaper, x, y), bar, &DOCK_LENS, dpr, true);
    let mut plain = Vec::with_capacity((bitmap.w * bitmap.h * 4) as usize);
    for y in 0..bitmap.h {
        for x in 0..bitmap.w {
            let c = place.sample(wallpaper, bar.x + x as f32 + 0.5, bar.y + y as f32 + 0.5);
            plain.extend_from_slice(&[to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), 255]);
        }
    }
    bitmap.plain = plain;
    bitmap
}

/// Frosted material for floating popups, macOS style: a wide blur of whatever is behind, boosted
/// saturation, no refraction (strength 0). Corner radius matches the DWM clip.
pub const FROST_LENS: LensOptions =
    LensOptions { radius: 8.0, band: 1.0, strength: 0.0, blur: 30.0, saturate: 1.7, aberration: 0.0 };

/// Control Center modules (macOS 26/27 clear glass): only a light blur, so the shapes and highlights
/// behind each module stay readable, and almost no saturation boost (no colour cast).
pub const CONTROL_LENS: LensOptions =
    LensOptions { radius: 8.0, band: 1.0, strength: 0.0, blur: 16.0, saturate: 1.25, aberration: 0.0 };

/// Light frost kept beside `CONTROL_LENS` when the Control Center sits on a bright page: the pale
/// glass samples it so page content (text lines, window edges) stays visible, softly blurred.
pub const BRIGHT_DETAIL_LENS: LensOptions =
    LensOptions { radius: 8.0, band: 1.0, strength: 0.0, blur: 2.5, saturate: 1.15, aberration: 0.0 };

/// Adds the bright-page detail frost (`BRIGHT_DETAIL_LENS`) to a Control Center backdrop as its
/// `plain` layer, but only when the frost is bright on average (otherwise nothing is kept).
pub fn add_bright_detail(frost: &mut LiquidBitmap, grab: &ScreenGrab, rect: RectF, dpr: f32) {
    let n = (frost.w * frost.h) as usize;
    if n == 0 {
        return;
    }
    let step = (n / 4096).max(1);
    let (mut sum, mut cnt) = (0f32, 0f32);
    for i in (0..n).step_by(step) {
        let c = &frost.rgba[i * 4..i * 4 + 3];
        sum += 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32;
        cnt += 1.0;
    }
    if sum / cnt < 175.0 {
        return;
    }
    let detail = render_frost_with(grab, rect, dpr, &BRIGHT_DETAIL_LENS);
    if detail.w == frost.w && detail.h == frost.h {
        frost.plain = detail.rgba;
    }
}

/// Frosted backdrop from the wallpaper alone (used until a live screen grab is possible).
pub fn render_popup(wallpaper: &WallpaperPixels, rect: RectF, screen: (i32, i32), dpr: f32) -> LiquidBitmap {
    render_popup_with(wallpaper, rect, screen, dpr, &FROST_LENS)
}

/// `render_popup` with an explicit material (the Control Center uses `CONTROL_LENS`).
pub fn render_popup_with(wallpaper: &WallpaperPixels, rect: RectF, screen: (i32, i32), dpr: f32, lens: &LensOptions) -> LiquidBitmap {
    let place = Placement::new(wallpaper, screen);
    render_with(&|x, y| place.sample(wallpaper, x, y), rect, lens, dpr.clamp(1.0, MAX_DPR), false)
}

/// A grab of the screen: straight RGBA, `w * h * 4` bytes, at virtual-screen position (x, y).
pub struct ScreenGrab {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub rgba: Vec<u8>,
}

/// Frosted backdrop of a live screen grab behind the popup. `rect` is the popup in the same
/// pixel space as the grab's (x, y) origin (virtual-screen px).
pub fn render_frost(grab: &ScreenGrab, rect: RectF, dpr: f32) -> LiquidBitmap {
    render_frost_with(grab, rect, dpr, &FROST_LENS)
}

/// `render_frost` with an explicit material (the Control Center uses `CONTROL_LENS`).
pub fn render_frost_with(grab: &ScreenGrab, rect: RectF, dpr: f32, lens: &LensOptions) -> LiquidBitmap {
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let ix = ((x - grab.x as f32).floor() as i32).clamp(0, grab.w - 1);
        let iy = ((y - grab.y as f32).floor() as i32).clamp(0, grab.h - 1);
        let i = ((iy * grab.w + ix) * 4) as usize;
        [grab.rgba[i] as f32, grab.rgba[i + 1] as f32, grab.rgba[i + 2] as f32]
    };
    render_with(&sample, rect, lens, dpr.clamp(1.0, MAX_DPR), false)
}

fn render_with(
    sample: &dyn Fn(f32, f32) -> [f32; 3],
    bar: RectF,
    o: &LensOptions,
    dpr: f32,
    flat_bottom: bool,
) -> LiquidBitmap {
    let bw = bar.w.round().max(1.0) as u32;
    let bh = bar.h.round().max(1.0) as u32;
    let blur_px = o.blur * dpr;
    let k = o.strength * dpr;
    let band = o.band * dpr;
    let radius = (o.radius * dpr).min(bw as f32 / 2.0).min(bh as f32 / 2.0);

    // Wallpaper region needed: the bar plus a margin (same formula as glass.ts).
    let margin = ((o.strength + o.blur * 3.0 + 4.0) * dpr).ceil() as i32;
    let x0 = bar.x.floor() as i32 - margin;
    let y0 = bar.y.floor() as i32 - margin;
    let grid_px = grid_for(blur_px, flat_bottom);
    let gw = (bw as i32 + 2 * margin + grid_px - 1) / grid_px;
    let gh = (bh as i32 + 2 * margin + grid_px - 1) / grid_px;

    // 1) Wallpaper on the grid, 2x2 supersamples per cell, straight to float RGB.
    let g = grid_px as f32;
    let mut grid = vec![0f32; (gw * gh * 3) as usize];
    for gy in 0..gh {
        for gx in 0..gw {
            let mut acc = [0f32; 3];
            for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let px = x0 as f32 + (gx as f32 + ox) * g;
                let py = y0 as f32 + (gy as f32 + oy) * g;
                let s = sample(px, py);
                for c in 0..3 {
                    acc[c] += s[c] * 0.25;
                }
            }
            let i = ((gy * gw + gx) * 3) as usize;
            grid[i..i + 3].copy_from_slice(&acc);
        }
    }

    // 2) Gaussian blur as three box passes (variance matches sigma = blur_px). Ping-pong buffers.
    let sigma = blur_px / g;
    let r = box_radius(sigma);
    let mut tmp = vec![0f32; grid.len()];
    for _ in 0..3 {
        box_pass(&grid, &mut tmp, gw, gh, r, true);
        box_pass(&tmp, &mut grid, gw, gh, r, false);
    }

    // 3) Saturate: CSS saturate() matrix.
    let s = o.saturate;
    let m = [
        [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
    ];
    for p in grid.chunks_exact_mut(3) {
        let (r0, g0, b0) = (p[0], p[1], p[2]);
        for c in 0..3 {
            p[c] = (m[c][0] * r0 + m[c][1] * g0 + m[c][2] * b0).clamp(0.0, 255.0);
        }
    }

    // Bilinear lookup in the blurred grid. (x, y) are bar-local pixel centers.
    let grid_at = |x: f32, y: f32, c: usize| -> f32 {
        let fx = ((bar.x + x - x0 as f32) / g - 0.5).clamp(0.0, (gw - 1) as f32 - 0.001);
        let fy = ((bar.y + y - y0 as f32) / g - 0.5).clamp(0.0, (gh - 1) as f32 - 0.001);
        let (ix, iy) = (fx as i32, fy as i32);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let at = |xx: i32, yy: i32| grid[((yy * gw + xx) * 3) as usize + c];
        let top = at(ix, iy) * (1.0 - tx) + at(ix + 1, iy) * tx;
        let bot = at(ix, iy + 1) * (1.0 - tx) + at(ix + 1, iy + 1) * tx;
        top * (1.0 - ty) + bot * ty
    };

    // 4) Lens: rounded-rect SDF. Inside the rim band, sample inward along the normal, with a quadratic
    // falloff (strongest at the edge). R and B are offset by +/- aberration.
    let hx = bw as f32 / 2.0;
    let hy = bh as f32 / 2.0;
    let ab = o.aberration;
    let mut out = vec![0u8; (bw * bh * 4) as usize];
    for y in 0..bh {
        let py = y as f32 + 0.5 - hy;
        for x in 0..bw {
            let px = x as f32 + 0.5 - hx;
            let qx = px.abs() - (hx - radius);
            let qy = py.abs() - (hy - radius);
            let mx = qx.max(0.0);
            let my = qy.max(0.0);
            let outside = mx.hypot(my);
            // Depth inside the rounded rect (>= 0 inside, negative outside).
            let rounded_depth = -(outside + qx.max(qy).min(0.0) - radius);
            // The dock is flush with the lower monitor edge: only its two upper corners are
            // rounded. Keep the lower half rectangular so no wallpaper pinholes appear there.
            let depth = if flat_bottom && py >= 0.0 {
                (hx - px.abs()).min(hy - py).max(0.0)
            } else {
                rounded_depth
            };
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let (cr, cg, cb) = if depth >= band {
                (grid_at(fx, fy, 0), grid_at(fx, fy, 1), grid_at(fx, fy, 2))
            } else {
                let (mut nx, mut ny) = if qx > 0.0 && qy > 0.0 {
                    (mx / outside, my / outside)
                } else if qx > qy {
                    (1.0, 0.0)
                } else {
                    (0.0, 1.0)
                };
                nx *= sign_or_one(px);
                ny *= sign_or_one(py);
                let t = 1.0 - depth.max(0.0) / band;
                let mag = k * t * t;
                (
                    grid_at(fx - nx * mag * (1.0 + ab), fy - ny * mag * (1.0 + ab), 0),
                    grid_at(fx - nx * mag, fy - ny * mag, 1),
                    grid_at(fx - nx * mag * (1.0 - ab), fy - ny * mag * (1.0 - ab), 2),
                )
            };
            let cov = (depth + 0.5).clamp(0.0, 1.0);
            let o4 = ((y * bw + x) * 4) as usize;
            out[o4] = to_u8(cr);
            out[o4 + 1] = to_u8(cg);
            out[o4 + 2] = to_u8(cb);
            out[o4 + 3] = (cov * 255.0 + 0.5) as u8;
        }
    }
    LiquidBitmap { w: bw, h: bh, rgba: out, plain: Vec::new() }
}

/// Wallpaper placement on the display, per Windows fit mode (same math as `placement` in glass.ts).
struct Placement {
    dx: f32,
    dy: f32,
    sx: f32,
    sy: f32,
    tile: bool,
    iw: f32,
    ih: f32,
    sw: f32,
    sh: f32,
    bg: [f32; 3],
}

impl Placement {
    fn new(wp: &WallpaperPixels, screen: (i32, i32)) -> Self {
        let (sw, sh) = (screen.0.max(1) as f32, screen.1.max(1) as f32);
        let iw = wp.w.max(1) as f32;
        let ih = wp.h.max(1) as f32;
        let ow = if wp.orig_w > 0 { wp.orig_w as f32 } else { iw };
        let oh = if wp.orig_h > 0 { wp.orig_h as f32 } else { ih };
        let (sx, sy) = match wp.mode {
            FitMode::Fit => {
                let s = (sw / iw).min(sh / ih);
                (s, s)
            }
            FitMode::Stretch => (sw / iw, sh / ih),
            FitMode::Center | FitMode::Tile => (ow / iw, oh / ih),
            FitMode::Fill | FitMode::Span => {
                let s = (sw / iw).max(sh / ih);
                (s, s)
            }
        };
        Self {
            dx: (sw - iw * sx) / 2.0,
            dy: (sh - ih * sy) / 2.0,
            sx,
            sy,
            tile: wp.mode == FitMode::Tile,
            iw,
            ih,
            sw,
            sh,
            bg: wp.background.map(|c| c as f32),
        }
    }

    /// Wallpaper colour at a display pixel, composited over the background colour.
    fn sample(&self, wp: &WallpaperPixels, x: f32, y: f32) -> [f32; 3] {
        // Blur requests pixels beyond the physical display at the bottom/side margins. Canvas
        // effectively extends edge pixels there; clamping avoids blending the desktop background
        // (usually near-black) into the dock's lower rim.
        let (x, y) = if self.tile {
            (x, y)
        } else {
            (x.clamp(0.0, self.sw - 1.0), y.clamp(0.0, self.sh - 1.0))
        };
        let mut u = (x - self.dx) / self.sx;
        let mut v = (y - self.dy) / self.sy;
        if self.tile {
            u = (x - self.dx).rem_euclid(self.iw * self.sx) / self.sx;
            v = (y - self.dy).rem_euclid(self.ih * self.sy) / self.sy;
        }
        if u < 0.0 || v < 0.0 || u >= self.iw || v >= self.ih {
            return self.bg;
        }
        let i = (v as usize * wp.w as usize + u as usize) * 4;
        let Some(p) = wp.rgba.get(i..i + 4) else { return self.bg };
        let a = p[3] as f32 / 255.0;
        [0, 1, 2].map(|c| p[c] as f32 * a + self.bg[c] * (1.0 - a))
    }
}

/// Box radius for three passes that approximate a gaussian of `sigma` (variance 3*(w^2-1)/12).
/// Blur grid step for a blur of `blur_px` physical px. Wide blurs keep the coarse `GRID`; light
/// blurs (the bright-page detail frost) use a finer grid so the blur stays the same size in DIPs at
/// any display scale. With a fixed 4 px grid the smallest possible blur (box radius 1, 3 passes)
/// is ~5.7 px: ~2.8 DIP at 200% but ~5.7 DIP at 100%, which smeared page text on real screens.
/// The dock (`flat_bottom`) always keeps `GRID`.
fn grid_for(blur_px: f32, flat_bottom: bool) -> i32 {
    if flat_bottom || blur_px >= 4.0 * GRID as f32 {
        GRID
    } else {
        ((blur_px / 3.0).round() as i32).clamp(1, GRID)
    }
}

fn box_radius(sigma: f32) -> i32 {
    let r = ((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0;
    r.round().max(1.0) as i32
}

/// One box blur pass over 3-channel float pixels, along rows or columns. Edges are clamped.
fn box_pass(src: &[f32], dst: &mut [f32], w: i32, h: i32, r: i32, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let idx = |i: i32| -> usize {
            let i = i.clamp(0, len - 1);
            let p = if horizontal { line * w + i } else { i * w + line };
            p as usize * 3
        };
        for c in 0..3 {
            let mut sum = 0f32;
            for k in -r..=r {
                sum += src[idx(k) + c];
            }
            for i in 0..len {
                dst[idx(i) + c] = sum * norm;
                sum += src[idx(i + r + 1) + c] - src[idx(i - r) + c];
            }
        }
    }
}

/// `Math.sign(v) || 1`: never returns 0.
fn sign_or_one(v: f32) -> f32 {
    if v < 0.0 {
        -1.0
    } else {
        1.0
    }
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

// Glass colours from styles.css, 0xRRGGBBAA.

/// Tint over the liquid: rgb(40,40,44) at min(0.95, pct/100) alpha (`.glass-liquid::before`, `--ga`).
/// Tiles use `--gt` = that alpha + 0.08 (cap 0.95).
pub fn glass_fill_color(opacity_pct: u8) -> u32 {
    let alpha = (opacity_pct.min(95) as u32 * 255 + 50) / 100;
    0x2828_2C00 | alpha
}

/// Top sheen of `.glass-liquid::before`: white 6%, fading to 0 at 40%. Drawn over the tint.
pub const GLASS_SHEEN_TOP: u32 = 0xFFFF_FF0F;
/// `.glass-rim` 1px gradient, top stop (white 26%).
pub const GLASS_RIM_TOP: u32 = 0xFFFF_FF42;
/// `.glass-rim` middle stop at 40% (white 16%). The renderer draws 2-stop gradients only.
pub const GLASS_RIM_MID: u32 = 0xFFFF_FF29;
/// `.glass-rim` bottom stop (white 13%).
pub const GLASS_RIM_BOTTOM: u32 = 0xFFFF_FF21;
