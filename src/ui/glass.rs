//! Dock glass: acrylic backdrop tint and the translucent tile/bar fill with a rim light.
//! Colors are 0xRRGGBBAA for the renderer; the acrylic tint is 0xAARRGGBB.
use windows::core::Result;
use windows::Win32::Foundation::HWND;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use super::dock::RectF;
use super::liquid::LiquidBitmap;
use super::renderer::Renderer;

/// Default Settings > Glass opacity (percent).
pub const DEFAULT_OPACITY_PCT: u8 = 55;
/// Same cap as the CSS `glassVars` (alpha never exceeds 0.95).
const MAX_OPACITY_PCT: u8 = 95;
/// Smoky frosted tint, rgb(40, 40, 44), as in styles.css `.glass-liquid::before`.
const TINT_RGB: u32 = 0x0028_282C;

/// Corner radius of the Start tile (and app icon plates), DIP.
pub const PLATE_RADIUS: f32 = 12.0;
/// Start tile height / app plate edge length, DIP (matches `TILE_H`).
pub const PLATE_SIZE: f32 = 34.0;

/// The dock bar is invisible (the window is truly transparent), so no acrylic is applied; the
/// Glass opacity setting only affects the tiles and popups.
pub fn apply_dock_glass(_hwnd: HWND, _opacity_pct: u8) {}

/// Liquid-glass plate shared by the Start button and centre app icons.
pub fn draw_liquid_plate(r: &mut Renderer, rect: RectF, radius: f32, hovered: bool) -> Result<()> {
    draw_glass(r, rect, radius, Layer::Plate, hovered)
}

// ---------------------------------------------------------------------------------------------
// Liquid Glass (iOS 26 / macOS 26 language) for tiles, plates and panels.
//
// Each piece samples the dock's blurred wallpaper backdrop at its own position, refracts it in an
// edge band (lensing), lifts and slightly saturates it (clear, light material), then applies an
// adaptive dim only as strong as needed for white text. Specular rims, the inner top highlight,
// the bottom inner shade and a soft shadow are vector layers on top. Pieces are cached per
// backdrop + pixel rect, and the renderer caches their GPU bitmaps, so hover redraws are cheap.
// ---------------------------------------------------------------------------------------------

/// Which material recipe to use.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Layer {
    /// Data tiles with text (left widgets, tray, clock): clear but dimmed enough for white text.
    Tile,
    /// Start / app icon plates: brightest, clearest layer.
    Plate,
}

/// Uniform corner radius for tiles and plates (continuous-looking at 34 DIP height).
pub const GLASS_RADIUS: f32 = 12.0;

struct Ctx {
    bg: Arc<LiquidBitmap>,
    /// Backdrop pixels per DIP.
    sx: f32,
    sy: f32,
}

type PieceKey = (usize, i32, i32, i32, i32, i32, Layer);

struct Piece {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
    static PIECES: RefCell<HashMap<PieceKey, Arc<Piece>>> = RefCell::new(HashMap::new());
}

/// Dock style for the tiles: 0 glass, 1 midnight, 2 black.
static STYLE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_style(id: &str) {
    let v = match id {
        "midnight" => 1,
        "black" => 2,
        _ => 0,
    };
    STYLE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Sets the backdrop that glass pieces sample for the dock being painted. `bar_w`/`bar_h` are
/// the bar size in DIPs (the backdrop bitmap covers exactly that). Pass None for surfaces with
/// no wallpaper backdrop (popups use DWM acrylic underneath instead).
pub fn set_backdrop(bg: Option<Arc<LiquidBitmap>>, bar_w: f32, bar_h: f32) {
    CTX.with(|c| {
        *c.borrow_mut() = bg.map(|bg| Ctx {
            sx: bg.w as f32 / bar_w.max(1.0),
            sy: bg.h as f32 / bar_h.max(1.0),
            bg,
        })
    });
}

/// Draws one Liquid Glass element (shadow, refracted backdrop, highlights, rims).
pub fn draw_glass(r: &mut Renderer, rect: RectF, radius: f32, layer: Layer, hovered: bool) -> Result<()> {
    if rect.w < 1.0 || rect.h < 1.0 {
        return Ok(());
    }
    let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
    draw_shadow(r, rect, radius)?;
    match piece_for(rect, radius, layer) {
        Some(p) => r.draw_bitmap(p.w, p.h, &p.rgba, rect)?,
        // No backdrop (yet): light neutral frost rather than a dark slab.
        None => r.fill_rounded_rect(rect, radius, if layer == Layer::Plate { 0xB4AA_A04D } else { 0x8C84_7C59 })?,
    }
    if layer == Layer::Tile {
        // Dock style: a tint over the glass (Glass keeps the clear material as is).
        let tint = match STYLE.load(std::sync::atomic::Ordering::Relaxed) {
            1 => 0x0A10_2CA6, // midnight: deep navy
            2 => 0x0000_00C8, // black
            _ => 0,
        };
        if tint != 0 {
            r.fill_rounded_rect(rect, radius, tint)?;
        }
    }
    if hovered {
        // Glow: the piece lights up a little; no geometry change.
        r.fill_rounded_rect(rect, radius, 0xFFFF_FF1C)?;
    }
    draw_highlights(r, rect, radius, if hovered { 1.35 } else { 1.0 })
}

/// Highlights and rims only (used by popups over DWM acrylic, and by `draw_glass`).
pub fn draw_highlights(r: &mut Renderer, rect: RectF, radius: f32, k: f32) -> Result<()> {
    let a = |base: f32| ((base * k).min(255.0)) as u32;
    let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
    // Soft inner highlight across the top ~55%.
    r.fill_rounded_rect_linear(rect, radius, (x, y), 0xFFFF_FF00 | a(40.0), (x, y + h * 0.55), 0xFFFF_FF00)?;
    // Faint darker inner rim along the bottom edge.
    let inner = RectF { x: x + 1.0, y: y + 1.0, w: (w - 2.0).max(0.0), h: (h - 2.0).max(0.0) };
    r.stroke_rounded_rect_linear(inner, (radius - 1.0).max(0.0), (x, y + h * 0.55), 0x0000_0000, (x, y + h), 0x0000_0038, 1.0)?;
    // Specular rim: brightest at the top-left, fading around the shape.
    let reach = w.min(h * 3.0);
    r.stroke_rounded_rect_linear(rect, radius, (x, y), 0xFFFF_FF00 | a(150.0), (x + reach, y + h), 0xFFFF_FF00 | a(22.0), 1.0)?;
    // Secondary reflection on the opposite (bottom-right) corner, much weaker.
    r.stroke_rounded_rect_linear(
        rect,
        radius,
        (x + w, y + h),
        0xFFFF_FF00 | a(70.0),
        (x + w - reach * 0.6, y),
        0xFFFF_FF00,
        1.0,
    )
}

/// DWM acrylic tint under popups/menus/previews (0xAARRGGBB): light, so the live content
/// behind stays visible like Liquid Glass.
pub const PANEL_ACRYLIC_TINT: u32 = 0x4C1E_1E24;

/// Floating panel (popups, context menus, window previews) over DWM acrylic: a light neutral
/// dim for text legibility, then the same highlights and rims as the dock pieces.
pub fn draw_panel(r: &mut Renderer, rect: RectF, radius: f32) -> Result<()> {
    r.fill_rounded_rect_gradient(rect, radius, 0x2023_2A70, 0x1618_1E8C)?;
    draw_highlights(r, rect, radius, 1.0)
}

/// Very soft drop shadow under a floating piece.
fn draw_shadow(r: &mut Renderer, rect: RectF, radius: f32) -> Result<()> {
    for i in 1..=3 {
        let g = i as f32;
        let sh = RectF { x: rect.x - g * 0.5, y: rect.y + 0.5 + g * 0.6, w: rect.w + g, h: rect.h + g * 0.5 };
        r.fill_rounded_rect(sh, radius + g * 0.5, 0x0000_000B)?;
    }
    Ok(())
}

fn piece_for(rect: RectF, radius: f32, layer: Layer) -> Option<Arc<Piece>> {
    CTX.with(|c| {
        let c = c.borrow();
        let ctx = c.as_ref()?;
        let px = (rect.x * ctx.sx).round() as i32;
        let py = (rect.y * ctx.sy).round() as i32;
        let pw = (rect.w * ctx.sx).round().max(1.0) as i32;
        let ph = (rect.h * ctx.sy).round().max(1.0) as i32;
        let pr = radius * ctx.sy;
        let key: PieceKey = (Arc::as_ptr(&ctx.bg) as usize, px, py, pw, ph, (pr * 4.0).round() as i32, layer);
        if let Some(p) = PIECES.with(|m| m.borrow().get(&key).cloned()) {
            return Some(p);
        }
        let piece = Arc::new(render_piece(&ctx.bg, px, py, pw, ph, pr, ctx.sy, layer));
        PIECES.with(|m| {
            let mut m = m.borrow_mut();
            // Magnified plates make transient sizes; keep the cache bounded.
            if m.len() > 256 {
                let live = key.0;
                m.retain(|k, _| k.0 == live);
                if m.len() > 192 {
                    m.clear();
                }
            }
            m.insert(key, piece.clone());
        });
        Some(piece)
    })
}

/// CPU render of one glass piece (straight RGBA, `pw` x `ph`).
#[allow(clippy::too_many_arguments)]
fn render_piece(bg: &LiquidBitmap, px: i32, py: i32, pw: i32, ph: i32, radius: f32, dpr: f32, layer: Layer) -> Piece {
    let (w, h) = (pw as u32, ph as u32);
    let band = 7.0 * dpr;
    let k = 4.5 * dpr;
    let hx = pw as f32 / 2.0;
    let hy = ph as f32 / 2.0;
    let r = radius.min(hx).min(hy);
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let fx = (x - 0.5).clamp(0.0, bg.w as f32 - 1.001);
        let fy = (y - 0.5).clamp(0.0, bg.h as f32 - 1.001);
        let (ix, iy) = (fx as usize, fy as usize);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let at = |xx: usize, yy: usize, c: usize| bg.rgba[(yy * bg.w as usize + xx) * 4 + c] as f32;
        let ix1 = (ix + 1).min(bg.w as usize - 1);
        let iy1 = (iy + 1).min(bg.h as usize - 1);
        [0, 1, 2].map(|c| {
            let top = at(ix, iy, c) * (1.0 - tx) + at(ix1, iy, c) * tx;
            let bot = at(ix, iy1, c) * (1.0 - tx) + at(ix1, iy1, c) * tx;
            top * (1.0 - ty) + bot * ty
        })
    };
    let (lift, sat, target) = match layer {
        Layer::Tile => (0.10, 1.12, 118.0),
        Layer::Plate => (0.16, 1.10, 168.0),
    };
    let mut rgb = vec![0f32; (w * h * 3) as usize];
    let mut alpha = vec![0u8; (w * h) as usize];
    let (mut lum_sum, mut lum_n) = (0f32, 0f32);
    for j in 0..ph {
        let ly = j as f32 + 0.5 - hy;
        for i in 0..pw {
            let lx = i as f32 + 0.5 - hx;
            let qx = lx.abs() - (hx - r);
            let qy = ly.abs() - (hy - r);
            let (mx, my) = (qx.max(0.0), qy.max(0.0));
            let outside = mx.hypot(my);
            let depth = -(outside + qx.max(qy).min(0.0) - r);
            let cov = (depth + 0.5).clamp(0.0, 1.0);
            let o = (j * pw + i) as usize;
            alpha[o] = (cov * 255.0 + 0.5) as u8;
            if cov <= 0.0 {
                continue;
            }
            let (sx, sy) = (px as f32 + i as f32 + 0.5, py as f32 + j as f32 + 0.5);
            let mut c = if depth >= band {
                sample(sx, sy)
            } else {
                // Lensing: inside the rim band, light bends toward the edge (sample inward along the
                // normal with a quadratic falloff), so the backdrop looks magnified at the border.
                let (mut nx, mut ny) = if qx > 0.0 && qy > 0.0 {
                    (mx / outside.max(1e-3), my / outside.max(1e-3))
                } else if qx > qy {
                    (1.0, 0.0)
                } else {
                    (0.0, 1.0)
                };
                nx *= if lx < 0.0 { -1.0 } else { 1.0 };
                ny *= if ly < 0.0 { -1.0 } else { 1.0 };
                let t = 1.0 - depth.max(0.0) / band;
                let m = k * t * t;
                let rr = sample(sx - nx * m * 1.05, sy - ny * m * 1.05);
                let gg = sample(sx - nx * m, sy - ny * m);
                let bb = sample(sx - nx * m * 0.95, sy - ny * m * 0.95);
                [rr[0], gg[1], bb[2]]
            };
            // Clear material: saturate slightly, then lift toward white.
            let l0 = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            for v in &mut c {
                *v = l0 + (*v - l0) * sat;
                *v += (255.0 - *v) * lift;
            }
            let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            lum_sum += l;
            lum_n += 1.0;
            rgb[o * 3..o * 3 + 3].copy_from_slice(&c);
        }
    }
    // Adaptive dim: only as much as needed to keep white content legible on bright wallpaper.
    let mean = if lum_n > 0.0 { lum_sum / lum_n } else { 0.0 };
    let dim = if mean > target { target / mean } else { 1.0 };
    let mut out = vec![0u8; (w * h * 4) as usize];
    for o in 0..(w * h) as usize {
        for c in 0..3 {
            out[o * 4 + c] = (rgb[o * 3 + c] * dim).round().clamp(0.0, 255.0) as u8;
        }
        out[o * 4 + 3] = alpha[o];
    }
    Piece { w, h, rgba: out }
}


// ---------------------------------------------------------------------------------------------
// Popup backdrop: the same wallpaper-based Liquid Glass as the dock, cut at the popup's position.
// ---------------------------------------------------------------------------------------------

static WALLPAPER: std::sync::Mutex<Option<Arc<crate::win::wallpaper::WallpaperPixels>>> = std::sync::Mutex::new(None);

/// Keeps the decoded wallpaper so popups can render their own glass backdrop when they open.
pub fn set_wallpaper(wp: crate::win::wallpaper::WallpaperPixels) {
    if let Ok(mut slot) = WALLPAPER.lock() {
        *slot = Some(Arc::new(wp));
    }
}

/// Liquid Glass backdrop for a popup whose window sits at `rect` (screen px) on `monitor`.
/// None until the wallpaper has been decoded; the popup then falls back to plain acrylic.
pub fn popup_backdrop(rect: windows::Win32::Foundation::RECT, monitor: windows::Win32::Foundation::RECT, dpi: u32) -> Option<Arc<LiquidBitmap>> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    // Live frost, as on macOS: grab what is really behind the popup (plus the blur margin), blur and
    // saturate it. The popup window must not be on screen yet.
    let dpr = dpi as f32 / 96.0;
    let margin = ((super::liquid::FROST_LENS.blur * 3.0 + 8.0) * dpr).ceil() as i32;
    let around = windows::Win32::Foundation::RECT {
        left: rect.left - margin,
        top: rect.top - margin,
        right: rect.right + margin,
        bottom: rect.bottom + margin,
    };
    if let Some(grab) = grab_screen(around) {
        let r = RectF {
            x: rect.left as f32,
            y: rect.top as f32,
            w: (rect.right - rect.left) as f32,
            h: (rect.bottom - rect.top) as f32,
        };
        return Some(Arc::new(super::liquid::render_frost(&grab, r, dpr)));
    }
    let wp = WALLPAPER.lock().ok()?.clone()?;
    // Same placement rules as the dock bar: a spanned wallpaper is addressed in virtual-screen
    // coordinates, every other fit mode per monitor.
    let (origin, screen) = if wp.mode == crate::win::wallpaper::FitMode::Span {
        // SAFETY: plain metric queries.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        ((vx, vy), (vw, vh))
    } else {
        ((monitor.left, monitor.top), (monitor.right - monitor.left, monitor.bottom - monitor.top))
    };
    let r = RectF {
        x: (rect.left - origin.0) as f32,
        y: (rect.top - origin.1) as f32,
        w: (rect.right - rect.left) as f32,
        h: (rect.bottom - rect.top) as f32,
    };
    Some(Arc::new(super::liquid::render_popup(&wp, r, screen, dpi as f32 / 96.0)))
}

/// Panel over a Liquid Glass backdrop bitmap: a neutral dim so white text stays legible on bright
/// wallpaper, then the same highlights and rims as the dock pieces.
pub fn draw_panel_tint(r: &mut Renderer, rect: RectF, radius: f32) -> Result<()> {
    r.fill_rounded_rect_gradient(rect, radius, 0x1C1C_2260, 0x1414_1A74)?;
    draw_highlights(r, rect, radius, 1.0)
}

/// A card inside a popup: a Liquid Glass piece when the popup has a wallpaper backdrop, else the
/// plain translucent `fallback` fill with highlights.
pub fn draw_card(r: &mut Renderer, rect: RectF, radius: f32, fallback: u32, k: f32) -> Result<()> {
    if CTX.with(|c| c.borrow().is_some()) {
        draw_glass(r, rect, radius, Layer::Tile, false)
    } else {
        r.fill_rounded_rect(rect, radius, fallback)?;
        draw_highlights(r, rect, radius, k)
    }
}

/// Grabs the composed desktop (everything visible, including other apps) in `rect` (virtual-screen
/// px), clipped to the virtual screen. None if GDI fails.
fn grab_screen(rect: windows::Win32::Foundation::RECT) -> Option<super::liquid::ScreenGrab> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    // SAFETY: plain metric queries.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let x0 = rect.left.max(vx);
    let y0 = rect.top.max(vy);
    let x1 = rect.right.min(vx + vw);
    let y1 = rect.bottom.min(vy + vh);
    let (w, h) = (x1 - x0, y1 - y0);
    if w < 2 || h < 2 {
        return None;
    }
    // SAFETY: every GDI object created here is deselected and released before returning.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let copied = BitBlt(mem, 0, 0, w, h, screen, x0, y0, SRCCOPY | CAPTUREBLT).is_ok();
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = if copied {
            GetDIBits(mem, bmp, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut bmi, DIB_RGB_COLORS)
        } else {
            0
        };
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if lines != h {
            return None;
        }
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA -> RGBA
            px[3] = 255;
        }
        Some(super::liquid::ScreenGrab { x: x0, y: y0, w, h, rgba: bgra })
    }
}
