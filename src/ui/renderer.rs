//! Direct2D HWND render target + DirectWrite text.
//! Factories are created once per thread and shared. The render target and brush are
//! created on first draw and dropped by `release()` (call it when the window hides).
//! Colors are 0xRRGGBBAA.
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{s, w, Interface, Result, HRESULT};

/// D2DERR_RECREATE_TARGET: the render target was lost and must be rebuilt.
const D2DERR_RECREATE_TARGET: HRESULT = HRESULT(0x8899_000C_u32 as i32);
use windows::Win32::Foundation::{BOOL, E_INVALIDARG, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_POINT_2F,
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    ID2D1RenderTarget,
    ID2D1StrokeStyle, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE,
    D2D1_ARC_SIZE_SMALL, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE, ID2D1Geometry,
    ID2D1PathGeometry,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWM_WINDOW_CORNER_PREFERENCE, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
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
    /// Segoe UI Variable Text, semibold (600): popup labels and secondary text.
    SemiBold,
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
            TextStyle::SemiBold => (w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_SEMI_BOLD),
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
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        f.SetTrimming(&trimming, &sign)?;
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
    pixel_w: u32,
    pixel_h: u32,
    dpi: u32,
    target: Option<ID2D1HwndRenderTarget>,
    /// Base target used by both the normal HWND surface and the Control Center PNG renderer.
    base_target: Option<ID2D1RenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    /// GPU bitmaps reused across frames (liquid backdrop, app icons). Keyed by the source
    /// buffer address/size plus a content fingerprint; dropped with the render target.
    bitmaps: crate::cache::ByteCache<(usize, usize, u32, u32, u64), (ID2D1Bitmap, u64)>,
    frame: u64,
}

impl Renderer {
    /// Lightweight: no Direct2D resources are created until the first draw.
    pub fn new(hwnd: HWND, pixel_w: i32, pixel_h: i32, dpi: u32) -> Self {
        Self {
            hwnd,
            pixel_w: pixel_w.max(1) as u32,
            pixel_h: pixel_h.max(1) as u32,
            dpi: dpi.max(96),
            target: None,
            base_target: None,
            brush: None,
            bitmaps: crate::cache::ByteCache::new(32 * 1024 * 1024),
            frame: 0,
        }
    }

    /// Renders one deterministic frame into an offscreen WIC bitmap and saves it as PNG.
    /// This is used only by the Control Center developer preview command.
    pub fn render_png(
        path: &std::path::Path,
        pixel_w: u32,
        pixel_h: u32,
        dpi: u32,
        draw: impl FnOnce(&mut Renderer) -> Result<()>,
    ) -> Result<()> {
        use std::io::Write;
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::GENERIC_WRITE;
        use windows::Win32::Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA,
            IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapCacheOnLoad,
            WICBitmapEncoderNoCache,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;

        let mut trace = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.with_extension("log"))
            .ok();
        macro_rules! trace {
            ($message:expr) => {
                if let Some(file) = trace.as_mut() {
                    let _ = writeln!(file, "render-wic: {}", $message);
                    let _ = file.flush();
                }
            };
        }
        trace!("initializing COM");
        let _com = crate::win::com::Apartment::mta();
        trace!("creating WIC factory");
        let wic: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        trace!("creating WIC PBGRA bitmap");
        let bitmap = unsafe {
            wic.CreateBitmap(
                pixel_w,
                pixel_h,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )?
        };
        trace!("creating Direct2D WIC render target");
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi.max(96) as f32,
            dpiY: dpi.max(96) as f32,
            ..Default::default()
        };
        let target = unsafe { d2d_factory()?.CreateWicBitmapRenderTarget(&bitmap, &props)? };
        trace!("creating Direct2D brush");
        let brush = unsafe { target.CreateSolidColorBrush(&color(0), None)? };
        unsafe {
            target.SetDpi(dpi.max(96) as f32, dpi.max(96) as f32);
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        let mut renderer = Renderer {
            hwnd: HWND::default(),
            pixel_w,
            pixel_h,
            dpi: dpi.max(96),
            target: None,
            base_target: Some(target),
            brush: Some(brush),
            bitmaps: crate::cache::ByteCache::new(32 * 1024 * 1024),
            frame: 0,
        };
        trace!("beginning Direct2D frame");
        renderer.begin()?;
        trace!("drawing Control Center");
        let drawn = draw(&mut renderer);
        trace!(format!("draw returned {drawn:?}; ending Direct2D frame"));
        // The offscreen renderer only has `base_target` (no HWND target), so `Renderer::end`
        // would skip EndDraw and leave the WIC bitmap write-locked. Always pair BeginDraw here.
        let ended = match renderer.base_target.as_ref() {
            Some(rt) => unsafe { rt.EndDraw(None, None) },
            None => Ok(()),
        };
        drawn?;
        ended?;
        // A WIC bitmap render target owns a write lock until every D2D reference is released.
        // Drop the target and its brush before handing the bitmap to the PNG encoder.
        renderer.release();
        trace!("released Direct2D target");

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
        unsafe {
            trace!("creating output stream");
            let stream = wic.CreateStream()?;
            stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0)?;
            trace!("creating PNG encoder");
            let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            trace!("creating PNG frame");
            let mut frame: Option<IWICBitmapFrameEncode> = None;
            let mut options: Option<IPropertyBag2> = None;
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame = frame.ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
            frame.Initialize(options.as_ref())?;
            trace!("configuring PNG frame");
            frame.SetSize(pixel_w, pixel_h)?;
            frame.SetResolution(dpi as f64, dpi as f64)?;
            let mut format = GUID_WICPixelFormat32bppPBGRA;
            frame.SetPixelFormat(&mut format)?;
            trace!("writing WIC bitmap to PNG frame");
            frame.WriteSource(&bitmap, std::ptr::null())?;
            trace!("committing PNG frame");
            frame.Commit()?;
            trace!("committing PNG encoder");
            encoder.Commit()
        }
    }

    /// Client size in device-independent pixels, matching all drawing coordinates.
    pub fn size(&self) -> (f32, f32) {
        let scale = self.dpi as f32 / 96.0;
        (self.pixel_w as f32 / scale, self.pixel_h as f32 / scale)
    }

    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    pub fn resize(&mut self, pixel_w: i32, pixel_h: i32, dpi: u32) {
        self.pixel_w = pixel_w.max(1) as u32;
        self.pixel_h = pixel_h.max(1) as u32;
        self.dpi = dpi.max(96);
        if let Some(rt) = &self.target {
            let size = D2D_SIZE_U { width: self.pixel_w, height: self.pixel_h };
            // SAFETY: `size` lives for the call.
            if unsafe { rt.Resize(&size) }.is_err() {
                self.release();
            } else {
                unsafe { rt.SetDpi(self.dpi as f32, self.dpi as f32) };
            }
        }
    }

    /// Drops the render target and brush. Call when the window is hidden.
    pub fn release(&mut self) {
        self.bitmaps.clear();
        self.brush = None;
        self.base_target = None;
        self.target = None;
    }

    #[cfg(test)]
    pub fn is_released(&self) -> bool {
        self.target.is_none() && self.base_target.is_none() && self.brush.is_none()
    }

    fn parts(&mut self) -> Result<(&ID2D1RenderTarget, &ID2D1SolidColorBrush)> {
        if self.base_target.is_none() {
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
                pixelSize: D2D_SIZE_U { width: self.pixel_w, height: self.pixel_h },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = unsafe {
                // SAFETY: both property structs live for the call.
                factory.CreateHwndRenderTarget(&rt_props, &hwnd_props)?
            };
            let brush = unsafe {
                // SAFETY: null brush properties are the documented default.
                rt.SetDpi(self.dpi as f32, self.dpi as f32);
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                rt.CreateSolidColorBrush(&color(0), None)?
            };
            self.base_target = Some(rt.cast()?);
            self.target = Some(rt);
            self.brush = Some(brush);
        }
        Ok((
            self.base_target.as_ref().expect("created above"),
            self.brush.as_ref().expect("created above"),
        ))
    }

    fn set_color(brush: &ID2D1SolidColorBrush, rgba: u32) {
        let c = color(light_ink(rgba));
        // SAFETY: `c` lives for the call.
        unsafe { brush.SetColor(&c) };
    }

    /// Area fills: like `set_color`, but in light-ink mode opaque white surfaces (switch knobs,
    /// slider thumbs) stay white and dark wells (inputs, tracks) turn into milky white wells.
    fn set_fill_color(brush: &ID2D1SolidColorBrush, rgba: u32) {
        let c = color(light_fill(rgba));
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
        self.frame += 1;
        // Forget bitmaps not drawn in the last 120 frames (closed apps, old wallpaper, temp buffers).
        if self.frame % 32 == 0 {
            let f = self.frame;
            self.bitmaps.retain(|_, (_, used)| f.saturating_sub(*used) < 120);
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
        Self::set_fill_color(brush, rgba);
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

    /// Clips later drawing to `rect` until `pop_clip` (axis aligned, anti-aliased off).
    pub fn push_clip(&mut self, rect: RectF) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
        let (rt, _) = self.parts()?;
        let r = d2d_rect(rect);
        // SAFETY: `r` lives for the call; paired with `pop_clip`.
        unsafe { rt.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED) };
        Ok(())
    }

    pub fn pop_clip(&mut self) -> Result<()> {
        let (rt, _) = self.parts()?;
        // SAFETY: paired with `push_clip`.
        unsafe { rt.PopAxisAlignedClip() };
        Ok(())
    }

    pub fn fill_rect(&mut self, rect: RectF, rgba: u32) -> Result<()> {
        let (rt, brush) = self.parts()?;
        Self::set_fill_color(brush, rgba);
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
        let (style, rgba) = popup_text(style, rgba);
        let format = text_format(size, align, style)?;
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let r = d2d_rect(rect);
        // SAFETY: slices and rect live for the call.
        unsafe {
            rt.DrawText(
                &wide,
                &format,
                &r,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
        Ok(())
    }

    /// Width in DIPs of `text` laid out on one line with the given size and style.
    pub fn measure_text(&self, text: &str, size: f32, style: TextStyle) -> Result<f32> {
        let (style, _) = popup_text(style, 0xFFFF_FFFF);
        let format = text_format(size, Align::Left, style)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let dw = dwrite_factory()?;
        // SAFETY: the slice and format live for the call; metrics is a plain out-struct.
        unsafe {
            let layout = dw.CreateTextLayout(&wide, &format, 10_000.0, 1_000.0)?;
            let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok(m.widthIncludingTrailingWhitespace)
        }
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
    /// A full 360° sweep is drawn as two semicircles — a single 360° arc is degenerate in D2D
    /// (start == end) and draws nothing, which made CPU/RAM track rings invisible.
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, radius: f32, start_deg: f32, sweep_deg: f32, rgba: u32, width: f32) -> Result<()> {
        if sweep_deg.abs() < 0.5 {
            return Ok(());
        }
        // Split near-full circles so each AddArc has distinct endpoints.
        if sweep_deg.abs() >= 359.5 {
            self.stroke_arc(cx, cy, radius, start_deg, 180.0, rgba, width)?;
            return self.stroke_arc(cx, cy, radius, start_deg + 180.0, 180.0, rgba, width);
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

    /// Straight line segment, default (flat) caps.
    pub fn stroke_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, rgba: u32, width: f32) -> Result<()> {
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        // SAFETY: points are plain values; brush belongs to this target.
        unsafe {
            rt.DrawLine(
                D2D_POINT_2F { x: x0, y: y0 },
                D2D_POINT_2F { x: x1, y: y1 },
                brush,
                width,
                None::<&ID2D1StrokeStyle>,
            )
        };
        Ok(())
    }

    /// Fills a rect with only the top two corners rounded (bottom edge flush / square).
    /// Used for the dock bar when it sits on the monitor edge so wallpaper cannot peek
    /// through lower rounded corners.
    pub fn fill_top_rounded_rect(&mut self, rect: RectF, radius: f32, rgba: u32) -> Result<()> {
        self.with_top_rounded_path(rect, radius, |rt, brush, geo| {
            // SAFETY: geo and brush live for the call.
            unsafe { rt.FillGeometry(geo, brush, None::<&windows::Win32::Graphics::Direct2D::ID2D1Brush>) };
            Ok(())
        }, rgba)
    }

    /// Stroke for a top-rounded / square-bottom rect (inset by half the stroke width).
    pub fn stroke_top_rounded_rect(&mut self, rect: RectF, radius: f32, rgba: u32, width: f32) -> Result<()> {
        let half = width / 2.0;
        let inset = RectF { x: rect.x + half, y: rect.y + half, w: rect.w - width, h: rect.h - width };
        let r = radius.max(0.0).min(inset.w / 2.0).min(inset.h);
        self.with_top_rounded_path(inset, r, move |rt, brush, geo| {
            // SAFETY: geo and brush live for the call.
            unsafe { rt.DrawGeometry(geo, brush, width, None::<&ID2D1StrokeStyle>) };
            Ok(())
        }, rgba)
    }

    fn with_top_rounded_path(
        &mut self,
        rect: RectF,
        radius: f32,
        draw: impl FnOnce(&ID2D1RenderTarget, &ID2D1SolidColorBrush, &ID2D1Geometry) -> Result<()>,
        rgba: u32,
    ) -> Result<()> {
        let r = radius.max(0.0).min(rect.w / 2.0).min(rect.h);
        let factory = d2d_factory()?;
        let geo: ID2D1PathGeometry = unsafe { factory.CreatePathGeometry()?.into() };
        let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
        // SAFETY: sink closed before draw.
        unsafe {
            let sink = geo.Open()?;
            use windows::Win32::Graphics::Direct2D::Common::{
                D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
            };
            // Top-left after the corner arc start, going clockwise: top edge → top-right arc →
            // right edge → bottom-right (square) → bottom edge → bottom-left (square) → left → TL arc.
            sink.BeginFigure(
                D2D_POINT_2F { x: x + r, y },
                D2D1_FIGURE_BEGIN_FILLED,
            );
            sink.AddLine(D2D_POINT_2F { x: x + w - r, y });
            if r > 0.0 {
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: D2D_POINT_2F { x: x + w, y: y + r },
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                });
            }
            sink.AddLine(D2D_POINT_2F { x: x + w, y: y + h });
            sink.AddLine(D2D_POINT_2F { x, y: y + h });
            sink.AddLine(D2D_POINT_2F { x, y: y + r });
            if r > 0.0 {
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: D2D_POINT_2F { x: x + r, y },
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                });
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
        }
        let (rt, brush) = self.parts()?;
        Self::set_fill_color(brush, rgba);
        let g: &ID2D1Geometry = &geo;
        draw(rt, brush, g)
    }

    /// Vertical gradient fill clipped to a top-rounded / square-bottom path.
    pub fn fill_top_rounded_rect_gradient(&mut self, rect: RectF, radius: f32, top: u32, bottom: u32) -> Result<()> {
        let r = radius.max(0.0).min(rect.w / 2.0).min(rect.h);
        let factory = d2d_factory()?;
        let geo: ID2D1PathGeometry = unsafe { factory.CreatePathGeometry()?.into() };
        let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
        unsafe {
            let sink = geo.Open()?;
            use windows::Win32::Graphics::Direct2D::Common::{
                D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
            };
            sink.BeginFigure(D2D_POINT_2F { x: x + r, y }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(D2D_POINT_2F { x: x + w - r, y });
            if r > 0.0 {
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: D2D_POINT_2F { x: x + w, y: y + r },
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                });
            }
            sink.AddLine(D2D_POINT_2F { x: x + w, y: y + h });
            sink.AddLine(D2D_POINT_2F { x, y: y + h });
            sink.AddLine(D2D_POINT_2F { x, y: y + r });
            if r > 0.0 {
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: D2D_POINT_2F { x: x + r, y },
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                });
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
        }
        let (rt, _) = self.parts()?;
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: color(top) },
            D2D1_GRADIENT_STOP { position: 1.0, color: color(bottom) },
        ];
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: D2D_POINT_2F { x: rect.x, y: rect.y },
            endPoint: D2D_POINT_2F { x: rect.x, y: rect.y + rect.h },
        };
        // SAFETY: stops live for CreateGradientStopCollection / CreateLinearGradientBrush.
        let brush = unsafe {
            let collection = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            rt.CreateLinearGradientBrush(&props, None, &collection)?
        };
        let g: &ID2D1Geometry = &geo;
        unsafe { rt.FillGeometry(g, &brush, None::<&windows::Win32::Graphics::Direct2D::ID2D1Brush>) };
        Ok(())
    }

    /// Draws straight (non-premultiplied) RGBA pixels scaled into `rect`.
    /// The GPU bitmap is cached per source buffer, so redraws (hover, magnify) do not
    /// re-convert and re-upload the 1920x46 backdrop on every mouse move.
    pub fn draw_bitmap(&mut self, w: u32, h: u32, rgba: &[u8], rect: RectF) -> Result<()> {
        if rgba.len() != w as usize * h as usize * 4 {
            return Err(E_INVALIDARG.into());
        }
        let key = (rgba.as_ptr() as usize, rgba.len(), w, h, fingerprint(rgba));
        let frame = self.frame;
        let cached = self.bitmaps.get_mut(&key).map(|(b, used)| {
            *used = frame;
            b.clone()
        });
        let bitmap = match cached {
            Some(b) => b,
            None => {
                let b = self.create_bitmap(w, h, rgba)?;
                self.bitmaps.insert(key, (b.clone(), frame), rgba.len());
                b
            }
        };
        let (rt, _) = self.parts()?;
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

impl Renderer {
    fn create_bitmap(&mut self, w: u32, h: u32, rgba: &[u8]) -> Result<ID2D1Bitmap> {
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
        unsafe { rt.CreateBitmap(size, Some(bgra.as_ptr().cast::<c_void>()), w * 4, &props) }
    }
}

/// Cheap content hash over ~256 evenly spaced bytes: a freed buffer reused at the same
/// address with other pixels gets a new cache entry instead of a stale image.
fn fingerprint(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let step = (data.len() / 256).max(1);
    let mut i = 0;
    while i < data.len() {
        h = (h ^ data[i] as u64).wrapping_mul(0x0100_0000_01b3);
        i += step;
    }
    h
}

/// Set once a window's frame was extended over its whole client area, so DWM honours the
/// per-pixel alpha of the Direct2D target (a cleared pixel then shows the desktop through).
static DWM_TRANSPARENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Lets a WS_POPUP window be truly transparent where nothing is drawn.
pub fn apply_dwm_transparency(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::UI::Controls::MARGINS;
    let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
    // SAFETY: `m` lives for the call.
    let ok = unsafe { DwmExtendFrameIntoClientArea(hwnd, &m) }.is_ok();
    if ok {
        DWM_TRANSPARENT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    ok
}

/// Dock windows: square corners and no DWM border line, so nothing outlines the empty bar.
pub fn apply_dock_no_chrome(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::DWMWCP_DONOTROUND;
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
    // SAFETY: documented attributes with u32 payloads.
    unsafe {
        let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&pref as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&DWMWA_COLOR_NONE as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

pub fn dwm_transparent() -> bool {
    DWM_TRANSPARENT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Asks the Windows 11 compositor for anti-aliased rounded corners.
pub fn apply_dwm_rounding(hwnd: HWND) -> bool {
    unsafe {
        let preference: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        )
        .is_ok()
    }
}

/// Corner radius (DIPs) of a DWMWCP_ROUND window on Windows 11. Popup panels draw their D2D
/// shape with exactly this radius so the DWM clip and the drawn rims coincide.
pub const POPUP_CORNER_RADIUS: f32 = 8.0;

/// Rounded DWM corners for floating panels, with the 1 px DWM border turned off
/// (DWMWA_COLOR_NONE) so no dark/square outline shows outside the curve.
pub fn apply_popup_corners(hwnd: HWND) -> bool {
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
    let rounded = apply_dwm_rounding(hwnd);
    // SAFETY: documented attribute with a u32 COLORREF payload.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&DWMWA_COLOR_NONE as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
    rounded
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
            // A zero tint turns the accent off (fully transparent popup window).
            state: if tint_argb == 0 { 0 } else { ACCENT_ENABLE_ACRYLICBLURBEHIND },
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

/// Arbitrary-direction gradients for Liquid Glass rims and highlights.
impl Renderer {
    fn linear_brush(
        &mut self,
        p0: (f32, f32),
        c0: u32,
        p1: (f32, f32),
        c1: u32,
    ) -> Result<windows::Win32::Graphics::Direct2D::ID2D1LinearGradientBrush> {
        let (rt, _) = self.parts()?;
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: color(c0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: color(c1) },
        ];
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: D2D_POINT_2F { x: p0.0, y: p0.1 },
            endPoint: D2D_POINT_2F { x: p1.0, y: p1.1 },
        };
        // SAFETY: stops and props live for the calls.
        unsafe {
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            rt.CreateLinearGradientBrush(&props, None, &coll)
        }
    }

    /// Fills a rounded rect with a gradient from `p0` (`c0`) to `p1` (`c1`).
    pub fn fill_rounded_rect_linear(
        &mut self,
        rect: RectF,
        radius: f32,
        p0: (f32, f32),
        c0: u32,
        p1: (f32, f32),
        c1: u32,
    ) -> Result<()> {
        let brush = self.linear_brush(p0, c0, p1, c1)?;
        let (rt, _) = self.parts()?;
        let rr = rounded(rect, radius);
        // SAFETY: `rr` and the brush live for the call.
        unsafe { rt.FillRoundedRectangle(&rr, &brush) };
        Ok(())
    }

    /// Strokes a rounded rect (centred on `rect` inset by half the width) with a gradient.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rounded_rect_linear(
        &mut self,
        rect: RectF,
        radius: f32,
        p0: (f32, f32),
        c0: u32,
        p1: (f32, f32),
        c1: u32,
        width: f32,
    ) -> Result<()> {
        let brush = self.linear_brush(p0, c0, p1, c1)?;
        let (rt, _) = self.parts()?;
        let h = width / 2.0;
        let inset = RectF { x: rect.x + h, y: rect.y + h, w: (rect.w - width).max(0.0), h: (rect.h - width).max(0.0) };
        let rr = rounded(inset, (radius - h).max(0.0));
        // SAFETY: `rr` and the brush live for the call.
        unsafe { rt.DrawRoundedRectangle(&rr, &brush, width, None::<&ID2D1StrokeStyle>) };
        Ok(())
    }

    /// Strokes the dock bar outline (top corners rounded) with a gradient.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_top_rounded_rect_linear(
        &mut self,
        rect: RectF,
        radius: f32,
        p0: (f32, f32),
        c0: u32,
        p1: (f32, f32),
        c1: u32,
        width: f32,
    ) -> Result<()> {
        let brush = self.linear_brush(p0, c0, p1, c1)?;
        let half = width / 2.0;
        let inset = RectF { x: rect.x + half, y: rect.y + half, w: rect.w - width, h: rect.h - width };
        let r = radius.max(0.0).min(inset.w / 2.0).min(inset.h);
        self.with_top_rounded_path(
            inset,
            r,
            move |rt, _, geo| {
                // SAFETY: geo and brush live for the call.
                unsafe { rt.DrawGeometry(geo, &brush, width, None::<&ID2D1StrokeStyle>) };
                Ok(())
            },
            0,
        )
    }
}

thread_local! {
    /// Set while popup content is painted on the light (milky) glass: neutral white ink becomes
    /// near-black so every popup keeps its contrast without per-popup colour tables.
    static LIGHT_INK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    static POPUP_TEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Enables the popup text weights (see `popup_text`) for subsequent text draws and measurements.
/// Set only while popup panels paint their content, so the dock bar keeps its own type.
pub fn set_popup_text(on: bool) {
    POPUP_TEXT.with(|c| c.set(on));
}

/// Popup content reads on bright glass: regular text is set semibold (labels and secondary text
/// alike; Segoe UI Variable has no medium instance, so 500 would fall back to regular), and
/// translucent secondary text gains ~30% of its missing opacity. Bold and icon text pass through.
fn popup_text(style: TextStyle, rgba: u32) -> (TextStyle, u32) {
    if style != TextStyle::Ui || !POPUP_TEXT.with(std::cell::Cell::get) {
        return (style, rgba);
    }
    let a = rgba & 0xFF;
    let a = if a < 0xE0 { a + (0xFF - a) * 3 / 10 } else { a };
    (TextStyle::SemiBold, (rgba & !0xFF) | a)
}

/// Enables the light-appearance ink remap (see `light_ink`) for subsequent solid-colour draws.
pub fn set_light_ink(on: bool) {
    LIGHT_INK.with(|c| c.set(on));
}

/// In light-ink mode, near-white neutral colours (text, glyphs, hairlines, subtle white fills)
/// map to their dark counterparts: opaque ink to #1D1D1F, translucent fills to black at the same
/// alpha. Coloured accents pass through unchanged.
pub fn light_ink(rgba: u32) -> u32 {
    if !LIGHT_INK.with(std::cell::Cell::get) {
        return rgba;
    }
    let (r, g, b, a) = (rgba >> 24, (rgba >> 16) & 0xFF, (rgba >> 8) & 0xFF, rgba & 0xFF);
    let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
    if lo < 0xB0 || hi - lo > 0x30 {
        return rgba;
    }
    if a >= 0x60 { 0x1D1D_1F00 | a } else { a }
}

/// Fill variant of `light_ink` (see `Renderer::set_fill_color`).
pub fn light_fill(rgba: u32) -> u32 {
    if !LIGHT_INK.with(std::cell::Cell::get) {
        return rgba;
    }
    let (r, g, b, a) = (rgba >> 24, (rgba >> 16) & 0xFF, (rgba >> 8) & 0xFF, rgba & 0xFF);
    let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
    if hi - lo <= 0x18 && lo >= 0xE8 && a >= 0xE0 {
        return rgba;
    }
    if hi <= 0x40 && hi - lo <= 0x18 && a < 0xA0 {
        return 0xFFFF_FF00 | (a * 2).min(0xC0);
    }
    light_ink(rgba)
}

/// Path-geometry drawing used by `svgpath`. Kept in its own block so it does not touch the core API.
impl Renderer {
    /// Like `draw_geometry_with`, but strokes use round caps and joins (SF Symbols look).
    pub fn draw_geometry_round(
        &mut self,
        build: impl FnOnce(&windows::Win32::Graphics::Direct2D::ID2D1GeometrySink) -> Result<()>,
        rgba: u32,
        stroke: Option<f32>,
    ) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::{
            D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_LINE_JOIN_ROUND, D2D1_STROKE_STYLE_PROPERTIES1,
            D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
        };
        let factory = d2d_factory()?;
        let geo: ID2D1PathGeometry = unsafe { factory.CreatePathGeometry()?.into() };
        // SAFETY: the sink is closed before the geometry is used, even when `build` fails.
        unsafe {
            let sink = geo.Open()?;
            let built = build(&sink);
            let closed = sink.Close();
            built?;
            closed?;
        }
        let props = D2D1_STROKE_STYLE_PROPERTIES1 {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
            transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
        };
        // SAFETY: plain factory call with a valid properties struct.
        let style: ID2D1StrokeStyle = unsafe { factory.CreateStrokeStyle(&props, None)?.into() };
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let g: &ID2D1Geometry = &geo;
        // SAFETY: `g` and `style` live for the call.
        unsafe {
            match stroke {
                Some(width) => rt.DrawGeometry(g, brush, width, &style),
                None => rt.FillGeometry(g, brush, None::<&windows::Win32::Graphics::Direct2D::ID2D1Brush>),
            }
        };
        Ok(())
    }

    /// Builds a path geometry through `build` (which drives an open sink), then fills it
    /// (`stroke` is None) or strokes it with the given width in DIPs.
    pub fn draw_geometry_with(
        &mut self,
        build: impl FnOnce(&windows::Win32::Graphics::Direct2D::ID2D1GeometrySink) -> Result<()>,
        rgba: u32,
        stroke: Option<f32>,
    ) -> Result<()> {
        let factory = d2d_factory()?;
        let geo: ID2D1PathGeometry = unsafe { factory.CreatePathGeometry()?.into() };
        // SAFETY: the sink is closed before the geometry is used, even when `build` fails.
        unsafe {
            let sink = geo.Open()?;
            let built = build(&sink);
            let closed = sink.Close();
            built?;
            closed?;
        }
        let (rt, brush) = self.parts()?;
        Self::set_color(brush, rgba);
        let g: &ID2D1Geometry = &geo;
        // SAFETY: `g` lives for the call; default stroke style; no opacity brush.
        unsafe {
            match stroke {
                Some(width) => rt.DrawGeometry(g, brush, width, None::<&ID2D1StrokeStyle>),
                None => rt.FillGeometry(g, brush, None::<&windows::Win32::Graphics::Direct2D::ID2D1Brush>),
            }
        };
        Ok(())
    }
}

/// Undoes `apply_dwm_transparency` on one window (popups switch between a glass panel and
/// free-floating tiles). Does not touch the global flag the dock uses.
pub fn reset_dwm_transparency(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::UI::Controls::MARGINS;
    let m = MARGINS::default();
    // SAFETY: `m` lives for the call.
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    #[test]
    fn all_tiles_and_popups_render_after_target_release_and_bitmap_eviction() {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let hwnd = unsafe { CreateWindowExW(
            WS_EX_TOOLWINDOW, w!("STATIC"), w!("Renderer lifecycle test"), WS_POPUP,
            0, 0, 1000, 800, None, None, None, None,
        ) }.unwrap();
        struct Window(HWND);
        impl Drop for Window { fn drop(&mut self) { unsafe { let _ = DestroyWindow(self.0); } } }
        let _window = Window(hwnd);
        let mut r = Renderer::new(hwnd, 1000, 800, 96);
        let settings = crate::settings::Settings::default();
        let data = crate::tiles::DataState::default();
        let control = crate::ControlState::default();
        let stats = crate::Stats::default();
        // Force continuous evictions without changing any drawing paths or pixels.
        r.bitmaps = crate::cache::ByteCache::new(8);
        for _ in 0..2 {
            for id in crate::settings::WIDGET_IDS {
                r.begin().unwrap();
                let v = crate::tile_view(id, &data, &control, &stats, 0.0, &settings);
                crate::dock::draw_tile(&mut r, &v, RectF { x: 5.0, y: 5.0, w: 250.0, h: 42.0 }, false).unwrap();
                // Each upload exceeds the test budget and must still draw successfully.
                r.draw_bitmap(2, 2, &[255; 16], RectF { x: 5.0, y: 60.0, w: 20.0, h: 20.0 }).unwrap();
                r.end().unwrap();
            }
            use crate::popup_content::PopupKind;
            for kind in [PopupKind::Git, PopupKind::Ai, PopupKind::Weather, PopupKind::Media,
                PopupKind::Clock, PopupKind::Cpu, PopupKind::Ram, PopupKind::Settings, PopupKind::Todo] {
                r.begin().unwrap();
                crate::popup_content::draw_popup(&mut r, kind, RectF { x: 0.0, y: 0.0, w: 460.0, h: 700.0 }, &data).unwrap();
                r.end().unwrap();
            }
            r.begin().unwrap();
            crate::control_center::draw(&mut r, RectF { x: 0.0, y: 0.0, w: 296.0, h: 500.0 }, &control, None).unwrap();
            r.end().unwrap();
            r.release();
            assert!(r.target.is_none());
            assert!(r.brush.is_none());
            r.resize(1000, 800, 144);
        }
    }
}
