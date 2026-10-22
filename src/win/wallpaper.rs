//! Desktop wallpaper for Liquid Glass. Port of the path/mode lookup in wallpaper.ts:
//! HKCU\Control Panel\Desktop (WallPaper, WallpaperStyle, TileWallpaper, Background), falling back to
//! %APPDATA%\Microsoft\Windows\Themes\TranscodedWallpaper. Decoded with WIC into straight RGBA.
//! Units: the decoded image is in physical pixels; `orig_w/orig_h` is the file's full size.
use std::ffi::c_void;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, GENERIC_READ};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

/// Fallback when Background is missing: the Electron default #1d2430.
const DEFAULT_BG: [u8; 3] = [0x1d, 0x24, 0x30];

/// Windows fit modes (WallpaperStyle + TileWallpaper).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitMode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
    Span,
}

/// Decoded wallpaper. `w`/`h` can be smaller than `orig_w`/`orig_h` (width is capped at load time),
/// so placement must scale by `orig / w` for center/tile.
pub struct WallpaperPixels {
    pub source: PathBuf,
    pub w: u32,
    pub h: u32,
    /// Straight (non-premultiplied) RGBA, `w * h * 4` bytes.
    pub rgba: Vec<u8>,
    pub orig_w: u32,
    pub orig_h: u32,
    pub mode: FitMode,
    /// Desktop Background colour (letterbox fill for fit/center).
    pub background: [u8; 3],
}

/// Identity of the wallpaper file and settings; used to detect changes without decoding.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    path: PathBuf,
    mtime: Option<SystemTime>,
    len: u64,
    mode: FitMode,
    background: [u8; 3],
}

/// Signature of the last `load` attempt (even a failed one, so polling does not retry in a loop).
static LAST: Mutex<Option<Source>> = Mutex::new(None);

/// Cheap poll: true when the wallpaper path, file mtime/size, fit mode or background differ from
/// the last `load`. Reads four registry values and one stat. Explorer writes the transcoded file
/// slightly after WM_SETTINGCHANGE, so callers should poll again a few seconds later.
pub fn wallpaper_changed() -> bool {
    let current = current_source();
    let seen = match LAST.lock() {
        Ok(g) => g.clone(),
        Err(_) => None,
    };
    current != seen
}

/// Path selected by the same discovery order as `load`, for diagnostics even if WIC decode fails.
pub fn source_path() -> Option<PathBuf> {
    current_source().map(|source| source.path)
}

/// Reads and decodes the current wallpaper. Width is capped to `max_w` (use the primary display
/// width, at least 1920). Records the signature so `wallpaper_changed` reports false afterwards.
/// Errors: no wallpaper file, or WIC cannot decode it.
pub fn load(max_w: u32) -> Result<WallpaperPixels> {
    let source = current_source();
    match LAST.lock() {
        Ok(mut g) => *g = source.clone(),
        Err(p) => *p.into_inner() = source.clone(),
    }
    let Some(src) = source else { return Err(E_FAIL.into()) };
    let (w, h, orig_w, orig_h, rgba) = decode(&src.path, max_w)?;
    Ok(WallpaperPixels {
        source: src.path,
        w,
        h,
        rgba,
        orig_w,
        orig_h,
        mode: src.mode,
        background: src.background,
    })
}

fn current_source() -> Option<Source> {
    let themes = std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join("Microsoft").join("Windows").join("Themes"));
    let desktop = reg_string(w!("WallPaper")).filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| p.is_file());
    let history = reg_string_at(
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Wallpapers"),
        w!("BackgroundHistoryPath0"),
    )
    .filter(|p| !p.is_empty())
    .map(PathBuf::from)
    .filter(|p| p.is_file());
    let transcoded = themes.as_ref().map(|d| d.join("TranscodedWallpaper")).filter(|p| p.is_file());
    let generated = themes.as_ref().and_then(|d| newest_file(d, Some("Transcoded_")));
    let cached = themes
        .as_ref()
        .map(|d| d.join("CachedFiles"))
        .and_then(|d| newest_file(&d, Some("CachedImage_")));
    // BackgroundHistoryPath0 is not guaranteed to be the image currently on screen. Prefer the
    // transcoded/cached output that Windows updates for slideshow and Spotlight sessions.
    let path = desktop.or(transcoded).or(generated).or(cached).or(history)?;
    let meta = std::fs::metadata(&path).ok()?;
    let style = reg_string(w!("WallpaperStyle")).unwrap_or_else(|| "10".into());
    let tile = reg_string(w!("TileWallpaper")).unwrap_or_else(|| "0".into());
    let background = reg_string(w!("Background")).and_then(|s| parse_rgb(&s)).unwrap_or(DEFAULT_BG);
    Some(Source {
        path,
        mtime: meta.modified().ok(),
        len: meta.len(),
        mode: mode_of(&style, &tile),
        background,
    })
}

/// WallpaperStyle/TileWallpaper -> fit mode (same mapping as wallpaper.ts).
fn mode_of(style: &str, tile: &str) -> FitMode {
    if tile == "1" {
        return FitMode::Tile;
    }
    match style {
        "0" => FitMode::Center,
        "2" => FitMode::Stretch,
        "6" => FitMode::Fit,
        "22" => FitMode::Span,
        _ => FitMode::Fill, // 10
    }
}

fn parse_rgb(s: &str) -> Option<[u8; 3]> {
    let mut it = s.split_whitespace().map(|p| p.parse::<u8>());
    let r = it.next()?.ok()?;
    let g = it.next()?.ok()?;
    let b = it.next()?.ok()?;
    Some([r, g, b])
}

/// Reads a REG_SZ value from HKCU\Control Panel\Desktop. None if missing or not a string.
fn reg_string(name: PCWSTR) -> Option<String> {
    reg_string_at(w!("Control Panel\\Desktop"), name)
}

fn reg_string_at(subkey: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: the subkey and value name are static literals; `buf` holds `bytes` bytes and
    // `bytes` is a local that lives for the call.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    if err.is_err() {
        return None;
    }
    let n = (bytes as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string())
}

fn newest_file(dir: &Path, prefix: Option<&str>) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| prefix.is_none_or(|p| e.file_name().to_string_lossy().starts_with(p)))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Decodes the first frame, scaled down to at most `max_w` wide. Returns (w, h, orig_w, orig_h, rgba).
fn decode(path: &Path, max_w: u32) -> Result<(u32, u32, u32, u32, Vec<u8>)> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    // SAFETY: CoInitializeEx on this thread is idempotent (an already-initialized thread returns an
    // ignorable HRESULT). `wide` is NUL-terminated and lives for the calls. Output pointers are locals.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory.CreateDecoderFromFilename(
            PCWSTR(wide.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut ow, mut oh) = (0u32, 0u32);
        frame.GetSize(&mut ow, &mut oh)?;
        if ow == 0 || oh == 0 {
            return Err(E_FAIL.into());
        }

        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;

        let (dw, dh) = if max_w > 0 && ow > max_w {
            (max_w, ((oh as u64 * max_w as u64) / ow as u64).max(1) as u32)
        } else {
            (ow, oh)
        };
        let mut buf = vec![0u8; dw as usize * dh as usize * 4];
        if (dw, dh) != (ow, oh) {
            let scaler = factory.CreateBitmapScaler()?;
            scaler.Initialize(&conv, dw, dh, WICBitmapInterpolationModeFant)?;
            scaler.CopyPixels(std::ptr::null(), dw * 4, &mut buf)?;
        } else {
            conv.CopyPixels(std::ptr::null(), ow * 4, &mut buf)?;
        }
        Ok((dw, dh, ow, oh, buf))
    }
}
