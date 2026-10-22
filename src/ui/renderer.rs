//! Direct2D HWND render target + DirectWrite text.
//! Factories are created once per thread and shared. The render target and brush are
//! created on first draw and dropped by `release()` (call it when the window hides).
//! Colors are 0xRRGGBBAA.
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{s, w, Result, HRESULT};

/// D2DERR_RECREATE_TARGET: the render target was lost and must be rebuilt.
const D2DERR_RECREATE_TARGET: HRESULT = HRESULT(0x8899_000C_u32 as i32);
use windows::Win32::Foundation::{BOOL, E_INVALIDARG, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_POINT_2F,
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE,
    D2D1_ARC_SIZE_SMALL, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE, ID2D1Geometry,
    ID2D1PathGeometry,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use super::dock::RectF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

thread_local! {
    static D2D: RefCell<Option<ID2D1Factory1>> = const { RefCell::new(None) };
    static DWRITE: RefCell<Option<IDWriteFactory>> = const { RefCell::new(None) };
    // Cache key: (font size * 10, alignment, style). Few combinations are used, so this stays tiny.
    static FORMATS: RefCell<Vec<((u32, Align, TextStyle), IDWriteTextFormat)>> = const { RefCell::new(Vec::new()) };
}

fn d2d_factory() -> Result<ID2D1Factory1> {
    D2D.with(|c| -> Result<ID2D1Factory1> {
        if let Some(f) = c.borrow().as_ref() {
            return Ok(f.clone());
        }
        // SAFETY: plain factory creation with default options.
        let f: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        *c.borrow_mut() = Some(f.clone());
        Ok(f)
    })
}

fn dwrite_factory() -> Result<IDWriteFactory> {
    DWRITE.with(|c| -> Result<IDWriteFactory> {
        if let Some(f) = c.borrow().as_ref() {
            return Ok(f.clone());
        }
        // SAFETY: plain factory creation.
        let f: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        *c.borrow_mut() = Some(f.clone());
        Ok(f)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    /// Segoe UI Variable Text, regular.
    Ui,
    /// Segoe UI Variable Text, semibold-bold, like `b` in the old CSS.
    Bold,
    /// Segoe MDL2 Assets glyphs (Windows system icons).
    Icon,
}

/// Cached text format for (size, align, style). Vertically centered, single line.
fn text_format(size: f32, align: Align, style: TextStyle) -> Result<IDWriteTextFormat> {
    let key = ((size * 10.0).round() as u32, align, style);
    let cached = FORMATS.with(|c| c.borrow().iter().find(|(k, _)| *k == key).map(|(_, f)| f.clone()));
    if let Some(f) = cached {
        return Ok(f);
    }
    let dw = dwrite_factory()?;
    let f = unsafe {
        // SAFETY: family name and locale are static UTF-16 literals.
        let (family, weight) = match style {
            TextStyle::Ui => (w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_NORMAL),
            TextStyle::Bold => (w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_BOLD),
            TextStyle::Icon => (w!("Segoe MDL2 Assets"), DWRITE_FONT_WEIGHT_NORMAL),
        };
        dw.CreateTextFormat(
            family,
            None::<&windows::Win32::Graphics::DirectWrite::IDWriteFontCollection>,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?
    };
    let text_align: DWRITE_TEXT_ALIGNMENT = match align {
        Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
        Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
    };
    unsafe {
        f.SetTextAlignment(text_align)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    }
    FORMATS.with(|c| c.borrow_mut().push((key, f.clone())));
    Ok(f)
}

fn color(rgba: u32) -> D2D1_COLOR_F {
    let c = |shift: u32| ((rgba >> shift) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: c(24), g: c(16), b: c(8), a: c(0) }
}

fn d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
}

fn rounded(r: RectF, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: d2d_rect(r), radiusX: radius, radiusY: radius }
}

pub struct Renderer {
    hwnd: HWND,
    w: u32,
    h: u32,
    target: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
}

impl Renderer {
    /// Lightweight: no Direct2D resources are created until the first draw.
    pub fn new(hwnd: HWND, w: i32, h: i32) -> Self {
        Self { hwnd, w: w.max(1) as u32, h: h.max(1) as u32, target: None, brush: None }
    }

    pub fn size(&self) -> (f32, f32) {
        (self.w as f32, self.h as f32)
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        self.w = w.max(1) as u32;
        self.h = h.max(1) as u32;
        if let Some(rt) = &self.target {
            let size = D2D_SIZE_U { width: self.w, height: self.h };
            // SAFETY: `size` lives for the call.
            if unsafe { rt.Resize(&size) }.is_err() {
                self.release();
            }
        }
    }

    /// Drops the render target and brush. Call when the window is hidden.
    pub fn release(&mut self) {
        self.brush = None;
        self.target = None;
    }

    fn parts(&mut self) -> Result<(&ID2D1HwndRenderTarget, &ID2D1SolidColorBrush)> {
        if self.target.is_none() {
            let factory = d2d_factory()?;
            let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                ..Default::default()
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: self.w, height: self.h },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = unsafe {
                // SAFETY: both property structs live for the call.
                factory.CreateHwndRenderTarget(&rt_props, &hwnd_props)?
            };
            let brush = unsafe {
                // SAFETY: null brush properties are the documented default.
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                rt.CreateSolidColorBrush(&color(0), None)?
            };
            self.target = Some(rt);
            self.brush = Some(brush);
        }
        Ok((
            self.target.as_ref().expect("created above"),
            self.brush.as_ref().expect("created above"),
        ))
    }

    fn set_color(brush: &ID2D1SolidColorBrush, rgba: u32) {
        let c = color(rgba);
        // SAFETY: `c` lives for the call.
        unsafe { brush.SetColor(&c) };
    }

    /// Starts a frame and clears to transparent.
    pub fn begin(&mut self) -> Result<()> {
        let (rt, _) = self.parts()?;
        let clear = color(0);
        // SAFETY: called on the render target owned by this renderer.
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&clear as *const D2D1_COLOR_F));
        }
        Ok(())
    }

    /// Ends the frame. If the device was lost, the target is dropped and rebuilt on next `begin`.
    pub fn end(&mut self) -> Result<()> {
        let Some(rt) = self.target.as_ref() else { return Ok(()) };
        // SAFETY: paired with `begin`.
        match unsafe { rt.EndDraw(None, None) } {
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.release();
                Ok(())
            }
            r => r,
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: RectF, radius: f32, rgba: u32) -> Result<()> {
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let rr = rounded(rect, radius);
        // SAFETY: `rr` lives for the call.
        unsafe { rt.FillRoundedRectangle(&rr, brush) };
        Ok(())
    }

    /// Stroke inside the rect (inset by half the width to avoid clipping at the edge).
    pub fn stroke_rounded_rect(&mut self, rect: RectF, radius: f32, rgba: u32, width: f32) -> Result<()> {
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let half = width / 2.0;
        let inset = RectF { x: rect.x + half, y: rect.y + half, w: rect.w - width, h: rect.h - width };
        let rr = rounded(inset, radius);
        // SAFETY: `rr` lives for the call; default stroke style.
        unsafe { rt.DrawRoundedRectangle(&rr, brush, width, None::<&ID2D1StrokeStyle>) };
        Ok(())
    }

    pub fn fill_rect(&mut self, rect: RectF, rgba: u32) -> Result<()> {
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let r = d2d_rect(rect);
        // SAFETY: `r` lives for the call.
        unsafe { rt.FillRectangle(&r, brush) };
        Ok(())
    }

    /// Single-line text, vertically centered in `rect`, clipped to it.
    pub fn draw_text(&mut self, text: &str, rect: RectF, size: f32, rgba: u32, align: Align) -> Result<()> {
        self.draw_text_styled(text, rect, size, rgba, align, TextStyle::Ui)
    }

    pub fn draw_text_styled(
        &mut self,
        text: &str,
        rect: RectF,
        size: f32,
        rgba: u32,
        align: Align,
        style: TextStyle,
    ) -> Result<()> {
        let format = text_format(size, align, style)?;
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let r = d2d_rect(rect);
        // SAFETY: slices and rect live for the call.
        unsafe {
            rt.DrawText(&wide, &format, &r, brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL)
        };
        Ok(())
    }

    /// Linear top-to-bottom gradient inside a rounded rect (`.clock-time`, `.dock` tint).
    pub fn fill_rounded_rect_gradient(&mut self, rect: RectF, radius: f32, top: u32, bottom: u32) -> Result<()> {
        let (rt, _) = self.parts()?;
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: color(top) },
            D2D1_GRADIENT_STOP { position: 1.0, color: color(bottom) },
        ];
        // SAFETY: `stops` lives for the call; the brush is local and dropped at the end.
        let brush = unsafe {
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: D2D_POINT_2F { x: rect.x, y: rect.y },
                endPoint: D2D_POINT_2F { x: rect.x, y: rect.y + rect.h },
            };
            rt.CreateLinearGradientBrush(&props, None, &coll)?
        };
        let rr = rounded(rect, radius);
        // SAFETY: `rr` lives for the call.
        unsafe { rt.FillRoundedRectangle(&rr, &brush) };
        Ok(())
    }

    /// Arc stroke centered at (cx, cy), angles in degrees clockwise from 3 o'clock.
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, radius: f32, start_deg: f32, sweep_deg: f32, rgba: u32, width: f32) -> Result<()> {
        if sweep_deg.abs() < 0.5 {
            return Ok(());
        }
        let factory = d2d_factory()?;
        let geo: ID2D1PathGeometry = unsafe { factory.CreatePathGeometry()?.into() };
        let pt = |deg: f32| {
            let a = deg.to_radians();
            D2D_POINT_2F { x: cx + radius * a.cos(), y: cy + radius * a.sin() }
        };
        // SAFETY: the sink is closed before the geometry is used.
        unsafe {
            let sink = geo.Open()?;
            sink.BeginFigure(pt(start_deg), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(start_deg + sweep_deg),
                size: D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if sweep_deg.abs() > 180.0 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
        }
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let g: &ID2D1Geometry = &geo;
        // SAFETY: `g` lives for the call; default stroke style.
        unsafe { rt.DrawGeometry(g, brush, width, None::<&ID2D1StrokeStyle>) };
        Ok(())
    }

    /// Draws straight (non-premultiplied) RGBA pixels scaled into `rect`.
    /// The bitmap is created per call and released right after drawing.
    pub fn draw_bitmap(&mut self, w: u32, h: u32, rgba: &[u8], rect: RectF) -> Result<()> {
        if rgba.len() != w as usize * h as usize * 4 {
            return Err(E_INVALIDARG.into());
        }
        // D2D wants premultiplied BGRA.
        let mut bgra = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            let a = px[3] as u32;
            let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            bgra.extend_from_slice(&[pm(px[2]), pm(px[1]), pm(px[0]), px[3]]);
        }
        let (rt, _) = self.parts()?;
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = D2D_SIZE_U { width: w, height: h };
        // SAFETY: `bgra` holds w*h*4 bytes and lives for the call; `props` likewise.
        let bitmap: ID2D1Bitmap =
            unsafe { rt.CreateBitmap(size, Some(bgra.as_ptr().cast::<c_void>()), w * 4, &props)? };
        let r = d2d_rect(rect);
        // SAFETY: `r` lives for the call.
        unsafe {
            rt.DrawBitmap(
                &bitmap,
                Some(&r as *const D2D_RECT_F),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            )
        };
        Ok(())
    }
}

/// Applies the undocumented acrylic accent (same as the old Electron build).
/// `tint_argb` is 0xAARRGGBB; it is converted to the ABGR order the API expects.
/// Returns false if the API is missing (pre-Win10 1809).
pub fn apply_acrylic(hwnd: HWND, tint_argb: u32) -> bool {
    #[repr(C)]
    struct AccentPolicy {
        state: i32,
        flags: u32,
        gradient: u32,
        anim_id: u32,
    }
    #[repr(C)]
    struct WcaData {
        attrib: u32,
        data: *mut c_void,
        size: usize,
    }
    type SetWcaFn = unsafe extern "system" fn(HWND, *mut WcaData) -> BOOL;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;
    const WCA_ACCENT_POLICY: u32 = 19;

    // SAFETY: user32 is always loaded; the looked-up symbol is called with its documented signature.
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return false };
        let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
            return false;
        };
        let set: SetWcaFn = std::mem::transmute(proc);
        let abgr = (tint_argb & 0xFF00_FF00)
            | ((tint_argb & 0xFF) << 16)
            | ((tint_argb >> 16) & 0xFF);
        let mut accent = AccentPolicy {
            state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            flags: 2, // draw all borders / use gradient color
            gradient: abgr,
            anim_id: 0,
        };
        let mut data = WcaData {
            attrib: WCA_ACCENT_POLICY,
            data: &mut accent as *mut AccentPolicy as *mut c_void,
            size: size_of::<AccentPolicy>(),
        };
        set(hwnd, &mut data).as_bool()
    }
}
