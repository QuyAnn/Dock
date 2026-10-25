//! Demand-driven polling. One small scheduler owns timing; blocking jobs are temporary,
//! independent threads so a slow network/COM call cannot stall other widgets.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{data, settings::Settings, tiles, win, Shared};

pub const BATTERY: u32 = 1 << 0;
pub const NET: u32 = 1 << 1;
pub const DISK: u32 = 1 << 2;
pub const GPU: u32 = 1 << 3;
pub const BRIGHTNESS: u32 = 1 << 4;
pub const SHOTS: u32 = 1 << 5;
pub const SHELL: u32 = 1 << 6;
pub const MEDIA: u32 = 1 << 7;
pub const WEATHER: u32 = 1 << 8;
pub const GIT: u32 = 1 << 9;
pub const CLAUDE: u32 = 1 << 10;
pub const CODEX: u32 = 1 << 11;
pub const CURSOR: u32 = 1 << 12;
pub const GROK: u32 = 1 << 13;
pub const BTC: u32 = 1 << 14;
pub const RATES: u32 = 1 << 15;
pub const CONTROL: u32 = 1 << 16;
pub const STATS: u32 = 1 << 17;
pub const CONTROL_PANEL: u32 = 1 << 18;
const AI: u32 = CLAUDE | CODEX | CURSOR | GROK;
pub const GALLERY: u32 = (1 << 19) - 1;
pub static PREVIEW: AtomicU32 = AtomicU32::new(0);

pub fn for_widget(id: &str) -> u32 {
    match id {
        "battery" => BATTERY, "net" => NET, "disk" => DISK, "gpu" => GPU,
        "cpu" | "ram" => STATS,
        "brightness" => BRIGHTNESS, "recentshots" => SHOTS,
        "trash" | "recyclebin" | "downloads" => SHELL,
        "media" => MEDIA, "cc" => CONTROL | CONTROL_PANEL | MEDIA,
        "weather" | "sun" | "hourly" | "forecast" => WEATHER,
        "git" => GIT, "claude" => CLAUDE, "codex" => CODEX,
        "cursor" => CURSOR, "grok" => GROK, "metric" => BTC, "currency" => RATES,
        _ => 0,
    }
}

pub fn placed(s: &Settings) -> u32 {
    // Weather in the fixed clock still needs its worker. CC's dock button needs
    // radio/volume state, but its media panel only needs polling while open.
    crate::settings::WIDGET_IDS.iter().filter(|id| s.tiles.is_on(id))
        .fold(0, |mask, id| mask | if *id == "cc" { CONTROL } else { for_widget(id) })
        | if s.tiles.trash { SHELL } else { 0 }
}

/// Small Rust-only loops can use 256 KiB. Blocking jobs retain 512 KiB of headroom
/// for Windows COM, TLS, shell extensions and image codecs.
pub fn spawn(name: &str, stack: usize, f: impl FnOnce() + Send + 'static) -> bool {
    match std::thread::Builder::new().name(name.into()).stack_size(stack).spawn(f) {
        Ok(_) => true,
        Err(e) => { crate::log(&format!("worker {name}: {e}")); false }
    }
}

#[derive(Default)]
struct Job {
    last: Option<Instant>,
    running: Arc<AtomicBool>,
}

impl Job {
    fn run(&mut self, enabled: bool, seconds: u64, f: impl FnOnce() + Send + 'static) {
        if !enabled { self.last = None; return; }
        if self.last.is_some_and(|at| at.elapsed() < Duration::from_secs(seconds)) { return; }
        if self.running.swap(true, Ordering::AcqRel) { return; }
        let running = self.running.clone();
        if spawn("widget-job", 512 * 1024, move || {
            // Also clear in-flight state if unwinding in a debug build.
            struct Done(Arc<AtomicBool>);
            impl Drop for Done { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
            let _done = Done(running);
            f();
        }) {
            self.last = Some(Instant::now());
        } else {
            self.running.store(false, Ordering::Release);
        }
    }
}

pub fn start(shared: Arc<Shared>) {
    spawn("widget-scheduler", 256 * 1024, move || {
        let mut tracker = data::screentime::Tracker::default();
        let mut sampler = data::widgets::Sampler::default();
        let mut sample_at = Instant::now() - Duration::from_secs(1);
        let (mut brightness, mut shots, mut shell, mut media, mut control) =
            (Job::default(), Job::default(), Job::default(), Job::default(), Job::default());
        let (mut git, mut ai, mut weather, mut btc, mut rates) =
            (Job::default(), Job::default(), Job::default(), Job::default(), Job::default());
        let mut previous_ai = 0;
        let mut had_shots = false;
        let mut previous_telemetry = 0;
        let mut previous_panel = false;
        loop {
            let s = shared.settings.lock().unwrap().clone();
            let mask = placed(&s) | PREVIEW.load(Ordering::Relaxed);
            let needed = |bits| mask & bits != 0;
            if sample_at.elapsed() >= Duration::from_secs(1) {
                // Screen-time is opt-in even when the gallery previews it.
                tracker.sample(s.tiles.screentime);
                let sample = sampler.read_selected(needed(BATTERY), needed(NET), needed(DISK), needed(GPU));
                let telemetry = mask & (BATTERY | NET | DISK | GPU);
                if telemetry != 0 || previous_telemetry != 0 {
                    Arc::make_mut(&mut shared.data.lock().unwrap()).apply(tiles::DataUpdate::Widgets(sample));
                }
                previous_telemetry = telemetry;
                sample_at = Instant::now();
            }
            let pending = win::widget_tools::brightness_pending();
            if pending { brightness.last = None; }
            brightness.run(needed(BRIGHTNESS) || pending, 15, win::widget_tools::poll_brightness);
            shots.run(needed(SHOTS), 5, win::widget_tools::refresh_shots);
            if had_shots && !needed(SHOTS) && !shots.running.load(Ordering::Acquire) {
                win::widget_tools::clear_shots();
                had_shots = false;
            }
            had_shots |= needed(SHOTS);
            shell.run(needed(SHELL), 5, win::shellinfo::refresh);
            let sh = shared.clone();
            media.run(needed(MEDIA), 1, move || {
                let update = tiles::DataState::fetch_media();
                Arc::make_mut(&mut sh.data.lock().unwrap()).apply(update);
            });
            let sh = shared.clone();
            let full_control = needed(CONTROL_PANEL);
            if full_control != previous_panel {
                control.last = None;
                previous_panel = full_control;
            }
            control.run(needed(CONTROL), 2, move || {
                let mut state = sh.control.lock().unwrap().clone();
                state.refresh(full_control);
                *sh.control.lock().unwrap() = state;
            });
            if shared.refresh_slow.swap(false, Ordering::SeqCst) {
                git.last = None; ai.last = None; weather.last = None;
            }
            if previous_ai != mask & AI { ai.last = None; previous_ai = mask & AI; }
            let sh = shared.clone();
            let config = s.clone();
            git.run(needed(GIT), 600, move || {
                let repos = if config.git_folders.is_empty() { crate::default_repos() } else { config.git_folders.clone() };
                let update = tiles::DataUpdate::Git(data::git::refresh_git(&repos, &config.git_author));
                Arc::make_mut(&mut sh.data.lock().unwrap()).apply(update);
            });
            let sh = shared.clone();
            ai.run(needed(AI), 600, move || {
                let update = tiles::DataUpdate::Ai(data::ai::refresh_ai_selected([
                    mask & CLAUDE != 0, mask & CODEX != 0, mask & CURSOR != 0, mask & GROK != 0,
                ]));
                Arc::make_mut(&mut sh.data.lock().unwrap()).apply(update);
            });
            let sh = shared.clone();
            weather.run(needed(WEATHER), 600, move || {
                let update = tiles::DataUpdate::Weather(data::weather::refresh_weather_at(&s.weather_location));
                Arc::make_mut(&mut sh.data.lock().unwrap()).apply(update);
            });
            btc.run(needed(BTC), 60, data::finance::refresh_btc);
            rates.run(needed(RATES), 3600, data::finance::refresh_rates);
            std::thread::sleep(Duration::from_millis(250));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weather_clock_and_shared_consumers_keep_their_worker() {
        let mut s = Settings::default();
        s.weather_position = "clock".into();
        assert!(!s.widget_shown("weather"));
        assert_ne!(placed(&s) & WEATHER, 0);
        s.tiles.weather = false;
        assert_eq!(placed(&s) & WEATHER, 0);
        s.tiles.hourly = true;
        assert_ne!(placed(&s) & WEATHER, 0);
        assert_eq!(for_widget("screentime"), 0);
        assert_eq!(placed(&Settings::default()) & GPU, 0);
        assert_eq!(for_widget("cc") & MEDIA, MEDIA);
        assert_eq!(crate::popup_demand(Some(crate::OpenPopup::Data("clock"))), WEATHER);
    }
    #[test]
    fn disabling_a_job_resets_its_deadline_without_starting_it() {
        let mut job = Job { last: Some(Instant::now()), ..Job::default() };
        job.run(false, 600, || panic!("disabled job ran"));
        assert!(job.last.is_none());
        assert!(!job.running.load(Ordering::Relaxed));
    }

    #[test]
    fn a_slow_job_cannot_overlap_after_disable_and_reenable() {
        let mut job = Job::default();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        job.run(true, 60, move || {
            started_tx.send(()).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        job.run(false, 60, || panic!("disabled job"));
        job.run(true, 60, || panic!("overlapping job"));
        assert!(job.running.load(Ordering::Acquire));
        assert!(job.last.is_none()); // re-enable refresh remains pending
        finish_tx.send(()).unwrap();
    }
}
