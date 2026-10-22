//! System controls without child processes: now-playing (SMTC), default output volume/mute,
//! primary monitor brightness (DDC/CI), Wi-Fi and Bluetooth radios.
//! WinRT `.get()` and COM calls block; call from a worker thread, not the UI thread.
use std::ptr;

use windows::core::{Error, Result};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::IAsyncOperation;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{E_ACCESSDENIED, E_FAIL, E_NOTIMPL, FALSE, HANDLE, POINT, TRUE};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl, IAudioMeterInformation,
};
use windows::Win32::Media::Audio::AUDIO_VOLUME_NOTIFICATION_DATA;
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD,
};
use windows::core::w;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::model::Media;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAction {
    PlayPause,
    Next,
    Previous,
}

const PERSONALIZE: windows::core::PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");

pub fn dark_mode() -> bool {
    let mut value = 1u32;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut bytes),
        )
    };
    status.is_ok() && value == 0
}

pub fn set_dark_mode(dark: bool) -> Result<()> {
    let value = if dark { 0u32 } else { 1u32 };
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            w!("AppsUseLightTheme"),
            REG_DWORD.0,
            Some((&value as *const u32).cast()),
            std::mem::size_of::<u32>() as u32,
        )
        .ok()
    }
}

pub fn open_settings(page: &str) -> Result<()> {
    let uri = windows::core::HSTRING::from(page);
    let result = unsafe {
        ShellExecuteW(None, w!("open"), &uri, None, None, SW_SHOWNORMAL)
    };
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(E_FAIL.into())
    }
}

pub fn open_path(path: &std::path::Path) -> Result<()> {
    let value = windows::core::HSTRING::from(path.as_os_str());
    let result = unsafe { ShellExecuteW(None, w!("open"), &value, None, None, SW_SHOWNORMAL) };
    if result.0 as isize > 32 { Ok(()) } else { Err(E_FAIL.into()) }
}

thread_local! {
    // Initialize COM/WinRT once per thread. Ignoring the result is fine:
    // RPC_E_CHANGED_MODE means the thread was already initialized.
    static COM_INIT: () = {
        // SAFETY: plain init call with no pointers.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    };
}

fn init_com() {
    COM_INIT.with(|_| ());
}

// ---------- media (SMTC) ----------

/// Now-playing info of the current session, or None when nothing is playing or SMTC fails.
pub fn current_media() -> Option<Media> {
    init_com();
    let mgr = SessionManager::RequestAsync().ok()?.get().ok()?;
    let session = mgr.GetCurrentSession().ok()?;
    let props = session.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let playing = session.GetPlaybackInfo().ok()?.PlaybackStatus().ok()? == PlaybackStatus::Playing;
    Some(Media {
        title: props.Title().ok()?.to_string(),
        artist: props.Artist().ok()?.to_string(),
        playing,
    })
}

/// Sends a transport command to the current media session.
pub fn media_control(action: MediaAction) -> Result<()> {
    init_com();
    let mgr = SessionManager::RequestAsync()?.get()?;
    let session = mgr.GetCurrentSession()?;
    // Each Try* returns false when the session does not support the command; that is not an error.
    let _ok: bool = match action {
        MediaAction::PlayPause => session.TryTogglePlayPauseAsync()?.get()?,
        MediaAction::Next => session.TrySkipNextAsync()?.get()?,
        MediaAction::Previous => session.TrySkipPreviousAsync()?.get()?,
    };
    Ok(())
}

// ---------- volume / mute (default render endpoint) ----------

fn endpoint_volume() -> Result<IAudioEndpointVolume> {
    init_com();
    // SAFETY: COM is initialized on this thread; the GUID pointer is a static.
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
    }
}

/// Current peak level of the default output endpoint (WASAPI endpoint meter), 0.0..=1.0.
/// Tries `eMultimedia` first (music / media sessions), then `eConsole`.
pub fn audio_peak() -> f32 {
    let read = |role| -> Result<f32> {
        init_com();
        let meter: IAudioMeterInformation = unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, role)?;
            device.Activate(CLSCTX_ALL, None)?
        };
        unsafe { meter.GetPeakValue() }
    };
    read(eMultimedia)
        .or_else(|_| read(eConsole))
        .unwrap_or(0.0)
        .clamp(0.0, 1.0)
}

/// Master volume as a scalar 0.0..=1.0 and mute state. Returns (0.0, false) when no device.
pub fn get_volume() -> (f32, bool) {
    let read = || -> Result<(f32, bool)> {
        let vol = endpoint_volume()?;
        // SAFETY: vol is a live IAudioEndpointVolume.
        unsafe { Ok((vol.GetMasterVolumeLevelScalar()?, vol.GetMute()?.0 != 0)) }
    };
    read().unwrap_or((0.0, false))
}

// ---------- live volume notifications ----------

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Latest master volume (f32 bits) and mute from IAudioEndpointVolumeCallback.
static NOTIFIED_VOLUME: AtomicU32 = AtomicU32::new(0);
static NOTIFIED_MUTED: AtomicBool = AtomicBool::new(false);
/// Set by the callback, cleared by `take_volume_change`.
static VOLUME_DIRTY: AtomicBool = AtomicBool::new(false);

#[windows::core::implement(IAudioEndpointVolumeCallback)]
struct VolumeWatcher;

impl IAudioEndpointVolumeCallback_Impl for VolumeWatcher_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if data.is_null() {
            return Ok(());
        }
        // SAFETY: the audio service passes a valid struct for the duration of the call.
        let d = unsafe { &*data };
        NOTIFIED_VOLUME.store(d.fMasterVolume.to_bits(), Ordering::SeqCst);
        NOTIFIED_MUTED.store(d.bMuted.as_bool(), Ordering::SeqCst);
        VOLUME_DIRTY.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Registers an IAudioEndpointVolumeCallback on the default output once (dedicated thread that
/// keeps the endpoint + callback alive). Volume/mute changes from anywhere (keyboard keys,
/// other apps, our own slider) are then available through `take_volume_change`.
pub fn watch_volume() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let Ok(vol) = endpoint_volume() else { return };
        let cb: IAudioEndpointVolumeCallback = VolumeWatcher.into();
        // SAFETY: vol is live; cb stays alive with this thread (parked forever).
        if unsafe { vol.RegisterControlChangeNotify(&cb) }.is_err() {
            return;
        }
        loop {
            std::thread::park();
        }
    });
}

/// (volume, muted) if it changed since the last call.
pub fn take_volume_change() -> Option<(f32, bool)> {
    VOLUME_DIRTY.swap(false, Ordering::SeqCst).then(|| {
        (f32::from_bits(NOTIFIED_VOLUME.load(Ordering::SeqCst)), NOTIFIED_MUTED.load(Ordering::SeqCst))
    })
}

/// Sets master volume, scalar 0.0..=1.0 (clamped).
pub fn set_volume(level: f32) -> Result<()> {
    let vol = endpoint_volume()?;
    // SAFETY: vol is live; a null event context is allowed.
    unsafe { vol.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), ptr::null()) }
}

pub fn set_mute(mute: bool) -> Result<()> {
    let vol = endpoint_volume()?;
    // SAFETY: vol is live; a null event context is allowed.
    unsafe { vol.SetMute(if mute { TRUE } else { FALSE }, ptr::null()) }
}

// ---------- brightness (DDC/CI, primary monitor) ----------

/// Runs `f` with the first physical monitor of the primary display, then destroys the handles.
fn with_primary_monitor<T>(f: impl FnOnce(HANDLE) -> Option<T>) -> Option<T> {
    // SAFETY: Win32 call; the monitor handle is only used by the API itself.
    let hm = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut count = 0u32;
    // SAFETY: count is a live out-pointer.
    let _ = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hm, &mut count) }.ok()?;
    if count == 0 {
        return None;
    }
    let mut mons = vec![
        PHYSICAL_MONITOR { hPhysicalMonitor: HANDLE(ptr::null_mut()), szPhysicalMonitorDescription: [0; 128] };
        count as usize
    ];
    // SAFETY: the slice has exactly the length the monitor count was read with.
    let _ = unsafe { GetPhysicalMonitorsFromHMONITOR(hm, &mut mons) }.ok()?;
    let out = f(mons[0].hPhysicalMonitor);
    // SAFETY: mons was filled by GetPhysicalMonitorsFromHMONITOR and is destroyed exactly once.
    let _ = unsafe { DestroyPhysicalMonitors(&mons) };
    out
}

/// Brightness in percent (0..=100), or None when the monitor does not support DDC/CI.
pub fn get_brightness() -> Option<u32> {
    with_primary_monitor(|h| {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        // SAFETY: h is a live physical monitor handle; out-pointers are locals.
        // Returns nonzero on success.
        let ok = unsafe { GetMonitorBrightness(h, &mut min, &mut cur, &mut max) } != 0;
        (ok && max > min).then(|| (cur.saturating_sub(min) * 100 / (max - min)).min(100))
    })
}

/// Sets brightness in percent (clamped to 0..=100). Err when unsupported or refused.
pub fn set_brightness(percent: u32) -> Result<()> {
    let level = percent.min(100);
    let done = with_primary_monitor(|h| {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        // SAFETY: h is a live physical monitor handle; out-pointers are locals.
        // Returns nonzero on success.
        let ok = unsafe { GetMonitorBrightness(h, &mut min, &mut cur, &mut max) } != 0;
        if !ok || max <= min {
            return None;
        }
        let raw = min + (max - min) * level / 100;
        // SAFETY: h is a live physical monitor handle.
        Some(unsafe { SetMonitorBrightness(h, raw) } != 0)
    });
    match done {
        Some(true) => Ok(()),
        None => Err(Error::from(E_NOTIMPL)),
        Some(false) => Err(Error::from(E_FAIL)),
    }
}

// ---------- radios (Wi-Fi / Bluetooth) ----------

fn find_radio(kind: RadioKind) -> Result<Option<Radio>> {
    init_com();
    // Access prompt/status is not fatal here: GetRadios still works and reports states.
    let _status: RadioAccessStatus = Radio::RequestAccessAsync()?.get()?;
    let radios = Radio::GetRadiosAsync()?.get()?;
    for i in 0..radios.Size()? {
        let radio = radios.GetAt(i)?;
        if radio.Kind()? == kind {
            return Ok(Some(radio));
        }
    }
    Ok(None)
}

fn radio_enabled(kind: RadioKind) -> Option<bool> {
    let radio = find_radio(kind).ok()??;
    Some(radio.State().ok()? == RadioState::On)
}

fn set_radio(kind: RadioKind, on: bool) -> Result<()> {
    let radio = find_radio(kind)?.ok_or_else(|| Error::from(E_NOTIMPL))?;
    let state = if on { RadioState::On } else { RadioState::Off };
    let op: IAsyncOperation<RadioAccessStatus> = radio.SetStateAsync(state)?;
    if op.get()? == RadioAccessStatus::Allowed {
        Ok(())
    } else {
        Err(Error::from(E_ACCESSDENIED))
    }
}

/// None when there is no Wi-Fi radio (or its state cannot be read).
pub fn wifi_enabled() -> Option<bool> {
    radio_enabled(RadioKind::WiFi)
}

pub fn set_wifi(on: bool) -> Result<()> {
    set_radio(RadioKind::WiFi, on)
}

/// None when there is no Bluetooth radio (or its state cannot be read).
pub fn bluetooth_enabled() -> Option<bool> {
    radio_enabled(RadioKind::Bluetooth)
}

pub fn set_bluetooth(on: bool) -> Result<()> {
    set_radio(RadioKind::Bluetooth, on)
}
