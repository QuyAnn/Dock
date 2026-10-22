//! App icons: exe -> 32x32 RGBA via the shell icon, cached per path.
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::sync::{Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, GetSystemMetrics, HICON, ICONINFO, SM_CXICON, SM_CYICON,
};

const CACHE_LIMIT: usize = 128;

#[derive(Clone, Debug)]
pub struct IconBitmap {
    pub w: u32,
    pub h: u32,
    /// RGBA, 4 bytes per pixel, top-down rows.
    pub rgba: Vec<u8>,
}

static CACHE: OnceLock<Mutex<HashMap<String, IconBitmap>>> = OnceLock::new();

/// Shell icon of an exe as RGBA pixels. Successful results are cached (bounded to CACHE_LIMIT).
pub fn icon_for_exe(exe_path: &str) -> Option<IconBitmap> {
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok()?.get(exe_path).cloned() {
        return Some(hit);
    }
    let icon = extract(exe_path)?;
    let mut map = cache.lock().ok()?;
    if map.len() >= CACHE_LIMIT {
        map.clear();
    }
    map.insert(exe_path.to_owned(), icon.clone());
    Some(icon)
}

fn extract(exe_path: &str) -> Option<IconBitmap> {
    let wide: Vec<u16> = exe_path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; SHFILEINFOW is plain data.
    unsafe {
        let mut info: SHFILEINFOW = zeroed();
        let ok = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let result = decode(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        result
    }
}

/// Reads the colour and mask bitmaps of `hicon` into RGBA. Does not destroy the icon.
unsafe fn decode(hicon: HICON) -> Option<IconBitmap> {
    let mut ii: ICONINFO = zeroed();
    GetIconInfo(hicon, &mut ii).ok()?;
    let (color, mask) = (ii.hbmColor, ii.hbmMask);
    // Monochrome icons have no colour bitmap; they are skipped.
    let result = if color.is_invalid() {
        None
    } else {
        read_rgba(color, mask)
    };
    if !color.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(color.0));
    }
    let _ = DeleteObject(HGDIOBJ(mask.0));
    result
}

unsafe fn read_rgba(color: HBITMAP, mask: HBITMAP) -> Option<IconBitmap> {
    let w = GetSystemMetrics(SM_CXICON) as u32;
    let h = GetSystemMetrics(SM_CYICON) as u32;
    let hdc = GetDC(HWND::default());
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let mut mask_px = vec![0u8; (w * h * 4) as usize];
    let ok = read_pixels(hdc, color, w, h, &mut bgra) && read_pixels(hdc, mask, w, h, &mut mask_px);
    ReleaseDC(HWND::default(), hdc);
    if !ok {
        return None;
    }
    // Legacy icons have no alpha channel: opacity then comes from the AND mask (black = opaque).
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    let mut rgba = Vec::with_capacity(bgra.len());
    for (p, m) in bgra.chunks_exact(4).zip(mask_px.chunks_exact(4)) {
        let a = match (has_alpha, m[0] == 0) {
            (true, _) => p[3],
            (false, true) => 255,
            (false, false) => 0,
        };
        rgba.extend_from_slice(&[p[2], p[1], p[0], a]);
    }
    Some(IconBitmap { w, h, rgba })
}

/// Copies a bitmap into `out` as top-down 32-bit BGRA.
unsafe fn read_pixels(hdc: HDC, bmp: HBITMAP, w: u32, h: u32, out: &mut [u8]) -> bool {
    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w as i32;
    bmi.bmiHeader.biHeight = -(h as i32);
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    GetDIBits(
        hdc,
        bmp,
        0,
        h,
        Some(out.as_mut_ptr().cast()),
        &mut bmi,
        DIB_RGB_COLORS,
    ) != 0
}
