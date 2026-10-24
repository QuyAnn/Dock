//! Opt-in foreground sampling, separate from all-time auto-pin ranking. Local file only.
use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;
use chrono::{Local, Timelike};
use serde::{Serialize, Deserialize};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::System::SystemInformation::GetTickCount64;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Today { pub date: String, pub apps: BTreeMap<String, f64> }
impl Today {
    fn add(&mut self, date: &str, exe: &str, seconds: f64) {
        if self.date != date { self.date = date.into(); self.apps.clear(); }
        if !exe.is_empty() && seconds.is_finite() && (0.0..=5.0).contains(&seconds) {
            *self.apps.entry(exe.to_owned()).or_default() += seconds;
        }
    }
    pub fn ranked(&self) -> Vec<(String,f64)> {
        if self.date != Local::now().format("%Y-%m-%d").to_string() { return Vec::new(); }
        let mut rows: Vec<_> = self.apps.iter().map(|(k,v)| (k.clone(),*v)).collect();
        rows.sort_by(|a,b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))); rows
    }
}
fn path() -> Option<std::path::PathBuf> {
    if cfg!(test) { return None; }
    crate::settings::path().map(|p| p.with_file_name("screentime.json"))
}
static TODAY: LazyLock<Mutex<Today>> = LazyLock::new(|| Mutex::new(path().and_then(|p| std::fs::read(p).ok())
    .and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()));
pub fn snapshot() -> Today { TODAY.lock().unwrap_or_else(|e| e.into_inner()).clone() }
pub fn flush() {
    static WRITER:Mutex<()> = Mutex::new(());
    let _writer=WRITER.lock().unwrap_or_else(|e|e.into_inner());
    let today=snapshot();
    if let Some(p) = path() {
        if let Some(parent) = p.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(bytes) = serde_json::to_vec(&today) {
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp,bytes).is_ok() { let _ = std::fs::rename(tmp,p); }
        }
    }
}
#[derive(Default)]
pub struct Tracker { last: Option<(Instant,String)>, save_at: Option<Instant> }
impl Tracker {
    pub fn sample(&mut self, enabled: bool) {
        let now = Instant::now();
        let local = Local::now(); let date = local.format("%Y-%m-%d").to_string();
        let mut today = TODAY.lock().unwrap_or_else(|e| e.into_inner());
        today.add(&date,"",0.0);
        let mut input = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, ..Default::default() };
        // SAFETY: valid output and plain foreground/process queries. Idle after five minutes
        // and gaps from sleep/suspend do not count as foreground use.
        let active = enabled && unsafe { GetLastInputInfo(&mut input) }.as_bool()
            && (unsafe { GetTickCount64() } as u32).wrapping_sub(input.dwTime) < 300_000;
        if active {
            if let Some((at,exe)) = self.last.take() {
                let seconds = now.duration_since(at).as_secs_f64();
                let since_midnight = local.num_seconds_from_midnight() as f64 + local.nanosecond() as f64 / 1e9;
                if seconds <= 5.0 { today.add(&date,&exe,seconds.min(since_midnight)); }
            }
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)); }
            self.last = Some((now, super::usage::process_exe(pid)));
        } else { self.last = None; }
        if enabled && self.save_at.is_none_or(|at| now.duration_since(at).as_secs() >= 15) {
            self.save_at = Some(now);
            drop(today); flush();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daily_totals_reset_and_reject_suspend_gaps() {
        let mut t = Today::default();
        t.add("2026-10-24","editor.exe",1.0); t.add("2026-10-24","editor.exe",2.0);
        t.add("2026-10-24","editor.exe",1000.0); t.add("2026-10-24","editor.exe",f64::NAN);
        assert_eq!(t.apps["editor.exe"],3.0);
        t.add("2026-10-25","browser.exe",0.5);
        assert!(!t.apps.contains_key("editor.exe")); assert_eq!(t.apps["browser.exe"],0.5);
        assert_eq!(serde_json::from_slice::<Today>(&serde_json::to_vec(&t).unwrap()).unwrap().apps,t.apps);
    }
}
