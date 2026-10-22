//! CPU and RAM usage (Windows only).
use std::mem::size_of;
use std::sync::Mutex;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::model::Stats;

/// Previous (idle, total) system time in 100 ns units; None until the first sample.
static PREV: Mutex<Option<(u64, u64)>> = Mutex::new(None);

fn ticks(t: FILETIME) -> u64 {
    (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64
}

/// Busy CPU % since the previous call. Returns 0 on the first call.
fn cpu_busy_pct() -> f32 {
    const ZERO: FILETIME = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut idle, mut kernel, mut user) = (ZERO, ZERO, ZERO);
    // SAFETY: all pointers refer to valid local FILETIME values.
    if unsafe { GetSystemTimes(Some(&mut idle as *mut _), Some(&mut kernel as *mut _), Some(&mut user as *mut _)) }.is_err() {
        return 0.0;
    }
    // Kernel time already includes idle time.
    let (idle, total) = (ticks(idle), ticks(kernel) + ticks(user));
    let mut prev = PREV.lock().unwrap_or_else(|e| e.into_inner());
    let pct = match *prev {
        Some((pi, pt)) if total > pt => {
            let busy_delta = (total - pt).saturating_sub(idle.saturating_sub(pi));
            busy_delta as f64 / (total - pt) as f64 * 100.0
        }
        _ => 0.0,
    };
    *prev = Some((idle, total));
    pct.clamp(0.0, 100.0) as f32
}

/// (used MB, total MB) of physical memory.
fn ram_mb() -> Option<(u64, u64)> {
    // SAFETY: MEMORYSTATUSEX is plain data; zeroed then dwLength set as the API requires.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    const MB: u64 = 1024 * 1024;
    Some(((m.ullTotalPhys - m.ullAvailPhys) / MB, m.ullTotalPhys / MB))
}

/// Current CPU and RAM usage. Call periodically (e.g. every 1-2 s) so the CPU delta is meaningful.
pub fn read_stats() -> Stats {
    let (ram_used_mb, ram_total_mb) = ram_mb().unwrap_or((0, 0));
    Stats { cpu_pct: cpu_busy_pct(), ram_used_mb, ram_total_mb }
}
