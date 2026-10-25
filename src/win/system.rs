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
    CoCreateInstance, CLSCTX_ALL,
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
    // Drop the apartment when temporary polling threads exit.
    static COM_INIT: super::com::Apartment = super::com::Apartment::mta();
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
    crate::background::spawn("volume-watch", 256 * 1024, || {
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

/// Signal quality (0..=100) of the best connected WLAN interface; None when not connected.
pub fn wifi_signal() -> Option<u8> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::WiFi::{
        wlan_interface_state_connected, wlan_intf_opcode_current_connection, WlanCloseHandle, WlanEnumInterfaces,
        WlanFreeMemory, WlanOpenHandle, WlanQueryInterface, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
    };
    // SAFETY: every WLAN allocation is freed with WlanFreeMemory and the handle is closed;
    // the slice covers exactly dwNumberOfItems entries of the returned list.
    unsafe {
        let mut version = 0u32;
        let mut h = HANDLE::default();
        if WlanOpenHandle(2, None, &mut version, &mut h) != 0 {
            return None;
        }
        let mut best: Option<u8> = None;
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, None, &mut list) == 0 && !list.is_null() {
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            for it in items {
                if it.isState != wlan_interface_state_connected {
                    continue;
                }
                let mut size = 0u32;
                let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
                if WlanQueryInterface(h, &it.InterfaceGuid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None) == 0
                    && !data.is_null()
                {
                    let attr = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    let q = attr.wlanAssociationAttributes.wlanSignalQuality.min(100) as u8;
                    best = Some(best.map_or(q, |b| b.max(q)));
                    WlanFreeMemory(data);
                }
            }
            WlanFreeMemory(list as *const std::ffi::c_void);
        }
        let _ = WlanCloseHandle(h, None);
        best
    }
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

// ---------- audio output devices ----------

/// An active playback endpoint.
#[derive(Clone, Debug)]
pub struct AudioOutput {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Undocumented but stable COM interface (used by every "audio switcher" tool); only
/// `SetDefaultEndpoint` is called, the other slots just keep the vtable layout right.
#[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: windows::core::IUnknown {
    fn GetMixFormat(&self, a: *const std::ffi::c_void, b: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn GetDeviceFormat(&self, a: *const std::ffi::c_void, b: i32, c: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn ResetDeviceFormat(&self, a: *const std::ffi::c_void) -> windows::core::HRESULT;
    fn SetDeviceFormat(&self, a: *const std::ffi::c_void, b: *mut std::ffi::c_void, c: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn GetProcessingPeriod(&self, a: *const std::ffi::c_void, b: i32, c: *mut i64, d: *mut i64) -> windows::core::HRESULT;
    fn SetProcessingPeriod(&self, a: *const std::ffi::c_void, b: *mut i64) -> windows::core::HRESULT;
    fn GetShareMode(&self, a: *const std::ffi::c_void, b: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn SetShareMode(&self, a: *const std::ffi::c_void, b: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn GetPropertyValue(&self, a: *const std::ffi::c_void, b: i32, c: *const std::ffi::c_void, d: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn SetPropertyValue(&self, a: *const std::ffi::c_void, b: i32, c: *const std::ffi::c_void, d: *mut std::ffi::c_void) -> windows::core::HRESULT;
    fn SetDefaultEndpoint(&self, id: windows::core::PCWSTR, role: i32) -> windows::core::HRESULT;
    fn SetEndpointVisibility(&self, a: *const std::ffi::c_void, b: i32) -> windows::core::HRESULT;
}

const CLSID_POLICY_CONFIG: windows::core::GUID = windows::core::GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Active output devices with the default one flagged. Empty on failure.
pub fn list_outputs() -> Vec<AudioOutput> {
    use windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE;
    use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};
    use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
    init_com();
    let read = || -> Result<Vec<AudioOutput>> {
        let key = PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
            pid: 14,
        };
        // SAFETY: COM is initialized; every interface/pointer below is used only while live.
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let default_id = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .and_then(|d| d.GetId().ok())
                .map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.as_ptr().cast()));
                    s
                })
                .unwrap_or_default();
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut out = Vec::new();
            for i in 0..devices.GetCount()? {
                let dev = devices.Item(i)?;
                let id_p = dev.GetId()?;
                let id = id_p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id_p.as_ptr().cast()));
                let name = dev
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|store| store.GetValue(&key))
                    .map(|pv| {
                        let s = pv.to_string();
                        s
                    })
                    .unwrap_or_else(|_| "Thiết bị âm thanh".to_string());
                out.push(AudioOutput { is_default: id == default_id, id, name });
            }
            Ok(out)
        }
    };
    read().unwrap_or_default()
}

/// Makes `id` the default output for all roles (console, multimedia, communications).
pub fn set_default_output(id: &str) -> Result<()> {
    init_com();
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the calls.
    unsafe {
        let cfg: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)?;
        for role in 0..3 {
            cfg.SetDefaultEndpoint(windows::core::PCWSTR(wide.as_ptr()), role).ok()?;
        }
    }
    Ok(())
}

/// True when a wired Ethernet adapter is up with a cable connected (hardware interfaces only,
/// so virtual switches, VPNs and loopback do not count).
pub fn ethernet_connected() -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2};
    let mut table = ptr::null_mut();
    // SAFETY: Windows allocates the table with NumEntries rows; it is freed after the scan.
    unsafe {
        if GetIfTable2(&mut table).is_err() {
            return false;
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        // Type 6 = IF_TYPE_ETHERNET_CSMACD (Wi-Fi is 71); MediaConnectState 1 = connected.
        let found = rows
            .iter()
            .any(|r| r.Type == 6 && r.OperStatus.0 == 1 && r.MediaConnectState.0 == 1 && r.InterfaceAndOperStatusFlags._bitfield & 1 != 0);
        FreeMibTable(table.cast());
        found
    }
}
