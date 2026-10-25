//! App icons: exe / AUMID -> 32x32 RGBA via shell AppsFolder, Appx logos, or SHGetFileInfo.
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{PCWSTR, Result, BSTR};
use windows::Win32::Foundation::{GENERIC_READ, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CLSCTX_INPROC_SERVER,
};
use windows::Win32::UI::Shell::{
    SHCreateItemFromParsingName, SHGetFileInfoW, IShellItemImageFactory, SHFILEINFOW, SHGFI_ICON,
    SHGFI_LARGEICON, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PROPERTYKEY, SHGetPropertyStoreForWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, GetSystemMetrics, HICON, ICONINFO, SM_CXICON, SM_CYICON,
};

/// Requested shell image size: 256 px (jumbo), reduced once when composed, so 38-DIP icons
/// stay crisp at any DPI.
const ICON_PX: i32 = 256;

/// Inbox Settings AUMID used when SystemSettings.exe is resolved but process AUMID is empty.
pub const SETTINGS_AUMID: &str =
    "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

/// PKEY_AppUserModel_ID — defined here to avoid the EnhancedStorage feature.
const PKEY_APPUSERMODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

#[derive(Clone, Debug)]
pub struct IconBitmap {
    pub w: u32,
    pub h: u32,
    /// RGBA, 4 bytes per pixel, top-down rows.
    pub rgba: Arc<Vec<u8>>,
}

static CACHE: OnceLock<Mutex<crate::cache::ByteCache<String, IconBitmap>>> = OnceLock::new();

/// Shell icon of an exe / packaged app as RGBA pixels. Successful results are cached.
pub fn icon_for_exe(exe_path: &str, aumid: &str, package_path: &str) -> Option<IconBitmap> {
    icon_for_window(0, exe_path, aumid, package_path)
}

/// Same as [`icon_for_exe`], but may read `PKEY_AppUserModel_ID` from `hwnd` when AUMID is empty.
pub fn icon_for_window(hwnd: isize, exe_path: &str, aumid: &str, package_path: &str) -> Option<IconBitmap> {
    let cache = CACHE.get_or_init(|| Mutex::new(crate::cache::ByteCache::new(16 * 1024 * 1024)));
    let frame_host = is_frame_host(exe_path);
    let resolved = resolve_aumid(hwnd, exe_path, aumid);
    // Prefer AUMID as cache key. Never key ApplicationFrameHost by its shared host path.
    let key = if !resolved.is_empty() {
        resolved.clone()
    } else if frame_host {
        format!("hwnd-frame:{exe_path}:{package_path}")
    } else {
        exe_path.to_owned()
    };
    if let Some(hit) = cache.lock().ok()?.get_mut(&key).cloned() {
        return Some(hit);
    }
    // Packaged/UWP: AppsFolder (scale/targetsize/plated) first; AppxManifest only after that fails.
    let icon = apps_folder_icon(&resolved)
        .filter(|b| !is_blank_white(b))
        .or_else(|| appx_icon(exe_path, package_path, &resolved).filter(|b| !is_blank_white(b)))
        .or_else(|| if frame_host { None } else { extract(exe_path) })
        .or_else(|| {
            if looks_like_settings(&resolved, exe_path) {
                generic_settings_icon()
            } else if frame_host || !resolved.is_empty() {
                generic_uwp_icon()
            } else {
                None
            }
        })?;
    let mut map = cache.lock().ok()?;
    map.insert(key, icon.clone(), icon.rgba.len());
    Some(icon)
}

/// AUMID from the window property store (works on ApplicationFrameHost frames).
pub fn aumid_from_hwnd(hwnd: isize) -> Option<String> {
    if hwnd == 0 {
        return None;
    }
    unsafe {
        let _com = super::com::Apartment::mta();
        let store: IPropertyStore = SHGetPropertyStoreForWindow(HWND(hwnd as _)).ok()?;
        let value = store.GetValue(&PKEY_APPUSERMODEL_ID).ok()?;
        let bstr = BSTR::try_from(&value).ok()?;
        let s = bstr.to_string().trim().to_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

fn resolve_aumid(hwnd: isize, exe_path: &str, aumid: &str) -> String {
    if !aumid.is_empty() {
        return aumid.to_owned();
    }
    if let Some(from_hwnd) = aumid_from_hwnd(hwnd) {
        return from_hwnd;
    }
    if looks_like_settings("", exe_path) {
        return SETTINGS_AUMID.to_owned();
    }
    String::new()
}

fn is_frame_host(exe_path: &str) -> bool {
    Path::new(exe_path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("ApplicationFrameHost.exe"))
}

fn looks_like_settings(aumid: &str, exe_path: &str) -> bool {
    let a = aumid.to_ascii_lowercase();
    a.contains("windows.immersivecontrolpanel")
        || a.contains("systemsettings")
        || Path::new(exe_path)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("SystemSettings.exe"))
}

/// `shell:AppsFolder\<AUMID>` via IShellItemImageFactory — preferred path for all UWPs.
fn apps_folder_icon(aumid: &str) -> Option<IconBitmap> {
    if aumid.is_empty() {
        return None;
    }
    shell_image(&format!("shell:AppsFolder\\{aumid}"))
}

/// IShellItemImageFactory icon for any parsing name (exe path, .lnk, AppsFolder item) at
/// `ICON_PX` (jumbo image list size); smaller native sizes are accepted.
fn shell_image(path: &str) -> Option<IconBitmap> {
    let wide: Vec<u16> = path.encode_utf16().chain(once(0)).collect();
    unsafe {
        let _com = super::com::Apartment::mta();
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(
            PCWSTR(wide.as_ptr()),
            None::<&windows::Win32::System::Com::IBindCtx>,
        )
        .ok()?;
        let hbmp = factory
            .GetImage(
                SIZE {
                    cx: ICON_PX,
                    cy: ICON_PX,
                },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
            .ok()?;
        let result = hbitmap_to_rgba(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        result
    }
}

/// Simple gear glyph as RGBA when package logo discovery fails for Settings.
fn generic_settings_icon() -> Option<IconBitmap> {
    let (w, h) = (32u32, 32u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let cx = 15.5f32;
    let cy = 15.5f32;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let r = (dx * dx + dy * dy).sqrt();
            let on = (r > 8.0 && r < 12.5) || r < 4.0 || (dx.abs() < 2.2 && r < 13.0) || (dy.abs() < 2.2 && r < 13.0);
            let i = ((y * w + x) * 4) as usize;
            if on {
                rgba[i] = 0xE8;
                rgba[i + 1] = 0xE8;
                rgba[i + 2] = 0xEC;
                rgba[i + 3] = 0xFF;
            }
        }
    }
    Some(IconBitmap { w, h, rgba: Arc::new(rgba) })
}

/// Rounded square with a window glyph when a packaged app's logo cannot be resolved.
fn generic_uwp_icon() -> Option<IconBitmap> {
    let (w, h) = (32u32, 32u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let in_round = x >= 3 && x < 29 && y >= 3 && y < 29;
            let edge = x == 3 || x == 28 || y == 3 || y == 28;
            let bar = y >= 5 && y <= 9 && x >= 5 && x <= 26;
            let pane = y >= 11 && y <= 26 && x >= 5 && x <= 26;
            if in_round && (edge || bar || pane) {
                let (r, g, b, a) = if edge || bar {
                    (0xC8u8, 0xCAu8, 0xD0u8, 0xFFu8)
                } else {
                    (0x5A, 0x5E, 0x6A, 0xE0)
                };
                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = a;
            }
        }
    }
    Some(IconBitmap { w, h, rgba: Arc::new(rgba) })
}

fn appx_icon(exe_path: &str, package_path: &str, aumid: &str) -> Option<IconBitmap> {
    let exe = Path::new(exe_path);
    let has_pkg = !package_path.is_empty() && Path::new(package_path).join("AppxManifest.xml").is_file();
    let packaged = has_pkg
        || !aumid.is_empty()
        || exe_path.to_ascii_lowercase().contains("\\windowsapps\\")
        || exe.file_name().is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("SystemSettings.exe"));
    if !packaged {
        return None;
    }
    let manifest = if has_pkg {
        Path::new(package_path).join("AppxManifest.xml")
    } else {
        let mut dir = exe.parent()?;
        loop {
            let candidate = dir.join("AppxManifest.xml");
            if candidate.is_file() {
                break candidate;
            }
            dir = dir.parent()?;
        }
    };
    let xml = std::fs::read_to_string(&manifest).ok()?;
    let logo = xml_value(&xml, "Square44x44Logo")
        .or_else(|| xml_value(&xml, "Square150x150Logo"))
        .or_else(|| xml_value(&xml, "Logo"))?;
    let root = manifest.parent()?;
    let configured = root.join(logo.replace('/', "\\"));
    let selected = best_logo(&configured).or_else(|| configured.is_file().then_some(configured))?;
    decode_image(&selected).ok()
}

fn xml_value(xml: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let rest = xml.split_once(&marker)?.1;
    Some(rest.split('"').next()?.to_owned())
}

fn best_logo(configured: &Path) -> Option<PathBuf> {
    let dir = configured.parent()?;
    let stem = configured.file_stem()?.to_string_lossy();
    let mut candidates: Vec<(i32, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
            if !name.starts_with(&stem.to_ascii_lowercase()) || path.extension()?.to_string_lossy().to_ascii_lowercase() != "png" {
                return None;
            }
            let score = if name.contains("targetsize-48") && name.contains("unplated") {
                300
            } else if name.contains("targetsize-48") {
                250
            } else if name.contains("scale-200") {
                200
            } else {
                100
            };
            Some((score, path))
        })
        .collect();
    candidates.sort_by_key(|(score, _)| *score);
    candidates.pop().map(|(_, path)| path)
}

/// Reject opaque near-white bitmaps (bad Square44x44Logo / white plate) so callers can fall through.
fn is_blank_white(icon: &IconBitmap) -> bool {
    let mut opaque = 0u32;
    let mut bright = 0u32;
    for px in icon.rgba.chunks_exact(4) {
        if px[3] < 200 {
            continue;
        }
        opaque += 1;
        let lum = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 255.0;
        if lum > 0.92 {
            bright += 1;
        }
    }
    if opaque == 0 {
        return true;
    }
    let coverage = opaque as f32 / (icon.w * icon.h) as f32;
    let bright_share = bright as f32 / opaque as f32;
    coverage > 0.85 && bright_share > 0.9
}

fn decode_image(path: &Path) -> Result<IconBitmap> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    unsafe {
        let _com = super::com::Apartment::mta();
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory.CreateDecoderFromFilename(
            PCWSTR(wide.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0, 0);
        frame.GetSize(&mut w, &mut h)?;
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut rgba = vec![0; (w * h * 4) as usize];
        converter.CopyPixels(std::ptr::null(), w * 4, &mut rgba)?;
        Ok(IconBitmap { w, h, rgba: Arc::new(rgba) })
    }
}

fn extract(exe_path: &str) -> Option<IconBitmap> {
    if let Some(big) = shell_image(exe_path).filter(|b| b.w >= 48 && !is_blank_white(b)) {
        return Some(big);
    }
    let wide: Vec<u16> = exe_path.encode_utf16().chain(Some(0)).collect();
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

unsafe fn decode(hicon: HICON) -> Option<IconBitmap> {
    let mut ii: ICONINFO = zeroed();
    GetIconInfo(hicon, &mut ii).ok()?;
    let (color, mask) = (ii.hbmColor, ii.hbmMask);
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

unsafe fn hbitmap_to_rgba(hbmp: HBITMAP) -> Option<IconBitmap> {
    let mut bm: BITMAP = zeroed();
    if GetObjectW(HGDIOBJ(hbmp.0), size_of::<BITMAP>() as i32, Some((&mut bm as *mut BITMAP).cast())) == 0 {
        return None;
    }
    let w = bm.bmWidth.max(0) as u32;
    let h = bm.bmHeight.max(0) as u32;
    if w == 0 || h == 0 {
        return None;
    }
    let hdc = GetDC(HWND::default());
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let ok = read_pixels(hdc, hbmp, w, h, &mut bgra);
    ReleaseDC(HWND::default(), hdc);
    if !ok {
        return None;
    }
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    let mut rgba = Vec::with_capacity(bgra.len());
    for p in bgra.chunks_exact(4) {
        let a = if has_alpha { p[3] } else { 255 };
        rgba.extend_from_slice(&[p[2], p[1], p[0], a]);
    }
    Some(IconBitmap { w, h, rgba: Arc::new(rgba) })
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
    Some(IconBitmap { w, h, rgba: Arc::new(rgba) })
}

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
