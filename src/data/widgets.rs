//! Windows widget telemetry. Keep the sampler on one background thread, outside UI/data locks.
use std::collections::HashMap;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Performance::*;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use crate::model::{Battery, WidgetStats};

type Counters = HashMap<u64, (u64, u64)>;

#[derive(Default)]
pub struct Sampler {
    net_previous: Option<(Instant, Counters)>,
    gpu: Option<GpuQuery>,
    gpu_retry: Option<Instant>,
}

impl Sampler {
    pub fn read(&mut self) -> WidgetStats {
        let now = Instant::now();
        let net = match network_counters() {
            Some(current) => {
                let rate = self.net_previous.as_ref().map(|(at, prev)| network_rate(prev, &current, now.duration_since(*at).as_secs_f64()));
                self.net_previous = Some((now, current));
                rate
            }
            None => { self.net_previous = None; None }
        };
        // Missing counters/drivers are normal. Retry occasionally so a device can appear later.
        if self.gpu.is_none() && self.gpu_retry.is_none_or(|at| now.duration_since(at).as_secs() >= 60) {
            self.gpu = GpuQuery::open();
            self.gpu_retry = Some(now);
        }
        let disks = [disk_space(w!("C:\\")), disk_space(w!("D:\\"))];
        WidgetStats {
            battery: battery(),
            net,
            disk_free: disks.map(|d| d.map(|(free, _)| free)),
            disk_total: disks.map(|d| d.map(|(_, total)| total)),
            gpu_pct: self.gpu.as_mut().and_then(GpuQuery::read),
        }
    }
}

fn battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: valid output structure.
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    if s.BatteryFlag == 255 || s.BatteryFlag & 128 != 0 { return None; }
    Some(Battery {
        percent: (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
        charging: s.BatteryFlag & 8 != 0,
        plugged_in: s.ACLineStatus == 1,
    })
}

fn disk_space(root: PCWSTR) -> Option<(u64,u64)> {
    let (mut free, mut total) = (0, 0);
    // SAFETY: root is a static terminated path; the output is a valid local.
    unsafe { GetDiskFreeSpaceExW(root, Some(&mut free), Some(&mut total), None) }.ok()?;
    Some((free, total))
}

fn network_counters() -> Option<Counters> {
    let mut table = std::ptr::null_mut();
    // SAFETY: Windows allocates the table and supplies NumEntries rows. Free it after copying.
    unsafe {
        if GetIfTable2(&mut table).is_err() { return None; }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let out = rows.iter()
            // Hardware interfaces only: avoid counting loopback/VPN/virtual switch traffic twice.
            .filter(|r| r.OperStatus.0 == 1 && r.InterfaceAndOperStatusFlags._bitfield & 1 != 0)
            .map(|r| (r.InterfaceLuid.Value, (r.InOctets, r.OutOctets)))
            .collect();
        FreeMibTable(table.cast());
        Some(out)
    }
}

fn network_rate(previous: &Counters, current: &Counters, seconds: f64) -> (f64, f64) {
    if !seconds.is_finite() || seconds <= 0.0 { return (0.0, 0.0); }
    let (mut down, mut up) = (0.0, 0.0);
    for (id, &(rx, tx)) in current {
        if let Some(&(old_rx, old_tx)) = previous.get(id) {
            down += rx.saturating_sub(old_rx) as f64;
            up += tx.saturating_sub(old_tx) as f64;
        }
    }
    (down / seconds, up / seconds)
}

fn scaled_bytes(bytes: f64) -> String {
    let mut value = if bytes.is_finite() { bytes.max(0.0) } else { 0.0 };
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 { value /= 1024.0; unit += 1; }
    if unit == 0 { format!("{value:.0} {}", units[unit]) } else { format!("{value:.1} {}", units[unit]) }
}

pub fn speed_text(bytes_per_second: f64) -> String { format!("{}/s", scaled_bytes(bytes_per_second)) }
pub fn size_text(bytes: u64) -> String { scaled_bytes(bytes as f64) }

struct GpuQuery { query: isize, counter: isize }

impl Drop for GpuQuery {
    fn drop(&mut self) {
        // SAFETY: this object exclusively owns the query, including its counters.
        unsafe { PdhCloseQuery(self.query); }
    }
}

impl GpuQuery {
    fn open() -> Option<Self> {
        let mut query = 0;
        // SAFETY: valid output handles and a static, English counter path (locale independent).
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 { return None; }
            let mut gpu = Self { query, counter: 0 };
            if PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut gpu.counter) != 0 { return None; }
            // Prime rate counters. First sample remains unavailable until the next collection.
            PdhCollectQueryData(query);
            Some(gpu)
        }
    }

    fn read(&mut self) -> Option<f32> {
        // SAFETY: query/counter are live. The aligned buffer holds both items and their names;
        // no borrowed pointer escapes it. PDH errors (including topology changes) yield None.
        unsafe {
            if PdhCollectQueryData(self.query) != 0 { return None; }
            let (mut bytes, mut count) = (0, 0);
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None) != PDH_MORE_DATA { return None; }
            if bytes == 0 || bytes > 4 * 1024 * 1024 { return None; }
            // Allocate in item-sized units to guarantee the API's alignment on every target.
            let mut buffer = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (bytes as usize).div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>())];
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, Some(buffer.as_mut_ptr())) != 0 { return None; }
            if count as usize > buffer.len() { return None; }
            let mut engines = HashMap::new();
            for item in &buffer[..count as usize] {
                if item.FmtValue.CStatus > PDH_CSTATUS_NEW_DATA || item.szName.is_null() { continue; }
                let value = item.FmtValue.Anonymous.doubleValue;
                let name = item.szName.to_string().ok()?;
                add_engine(&mut engines, &name, value);
            }
            engines.values().copied().reduce(f64::max).map(|v| v.clamp(0.0, 100.0) as f32)
        }
    }
}

fn add_engine(engines: &mut HashMap<String, f64>, name: &str, value: f64) {
    // Sum processes on each physical engine, then use the busiest engine, like Task Manager.
    if !value.is_finite() || value < 0.0 { return; }
    if let Some((_, engine)) = name.split_once("_luid_") {
        *engines.entry(engine.to_owned()).or_default() += value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_speeds_and_sizes() {
        assert_eq!(speed_text(0.0), "0 B/s");
        assert_eq!(speed_text(1023.0), "1023 B/s");
        assert_eq!(speed_text(1024.0), "1.0 KB/s");
        assert_eq!(speed_text(1.5 * 1024.0 * 1024.0), "1.5 MB/s");
        assert_eq!(speed_text(f64::NAN), "0 B/s");
        assert_eq!(speed_text(-1.0), "0 B/s");
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(1024_u64.pow(3) * 42), "42.0 GB");
        assert_eq!(size_text(1024_u64.pow(4)), "1.0 TB");
        assert_eq!(size_text(u64::MAX), "16.0 EB");
    }

    #[test]
    fn rates_handle_elapsed_time_reset_and_adapter_changes() {
        let old = HashMap::from([(1, (1000, 1000)), (2, (9999, 9999))]);
        let new = HashMap::from([(1, (3048, 900)), (3, (99999, 99999))]);
        assert_eq!(network_rate(&old, &new, 2.0), (1024.0, 0.0));
        assert_eq!(network_rate(&old, &new, 0.0), (0.0, 0.0));
    }

    #[test]
    fn gpu_sums_processes_without_summing_independent_engines() {
        let mut e = HashMap::new();
        add_engine(&mut e, "pid_1_luid_0x1_phys_0_eng_0_engtype_3D", 20.0);
        add_engine(&mut e, "pid_2_luid_0x1_phys_0_eng_0_engtype_3D", 30.0);
        add_engine(&mut e, "pid_1_luid_0x1_phys_0_eng_1_engtype_Copy", 10.0);
        add_engine(&mut e, "pid_3_luid_0x2_phys_0_eng_0_engtype_3D", 15.0);
        add_engine(&mut e, "invalid", f64::NAN);
        assert_eq!(e.len(), 3);
        assert_eq!(e.values().copied().reduce(f64::max), Some(50.0));
    }
}
