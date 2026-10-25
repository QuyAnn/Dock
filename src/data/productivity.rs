//! Independent monotonic Pomodoro and lap stopwatch. No wall-clock changes affect timers.
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Stopwatch { elapsed: Duration, start: Option<Instant>, laps: Vec<Duration> }
impl Stopwatch {
    fn at(&self, now: Instant) -> Duration {
        self.elapsed + self.start.map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
    }
    fn toggle(&mut self, now: Instant) {
        self.elapsed = self.at(now);
        self.start = if self.start.is_some() { None } else { Some(now) };
    }
    fn lap(&mut self, now: Instant) {
        if self.start.is_some() && self.laps.len() < 100 { self.laps.push(self.at(now)); }
    }
}
static WATCH: Mutex<Stopwatch> = Mutex::new(Stopwatch { elapsed: Duration::ZERO, start: None, laps: Vec::new() });
pub fn watch() -> (Duration, bool, Vec<Duration>) {
    let s = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    (s.at(Instant::now()), s.start.is_some(), s.laps.clone())
}
pub fn watch_summary() -> (Duration, bool, usize) {
    let s = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    (s.at(Instant::now()), s.start.is_some(), s.laps.len())
}
pub fn watch_toggle() { WATCH.lock().unwrap_or_else(|e| e.into_inner()).toggle(Instant::now()); }
pub fn lap() { WATCH.lock().unwrap_or_else(|e| e.into_inner()).lap(Instant::now()); }
pub fn watch_reset() { *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Stopwatch::default(); }
pub fn watch_text(d: Duration) -> String { format!("{}.{:01}", super::timer::time_text(d.as_secs()), d.subsec_millis() / 100) }

struct Pomodoro { rest: bool, round: u64, left: Duration, start: Option<Instant> }
impl Pomodoro {
    const fn new() -> Self { Self { rest: false, round: 1, left: Duration::from_secs(1500), start: None } }
    fn advance(&mut self) {
        if self.rest { self.round += 1; }
        self.rest = !self.rest;
        self.left = Duration::from_secs(if self.rest { 300 } else { 1500 });
    }
    fn settle(&mut self, now: Instant) {
        let Some(start) = self.start else { return };
        let mut elapsed = now.saturating_duration_since(start);
        // Skip whole 30-minute cycles in constant time after sleep/hibernation.
        let cycles = elapsed.as_secs() / 1800;
        self.round += cycles;
        elapsed = elapsed.saturating_sub(Duration::from_secs(cycles * 1800));
        while elapsed >= self.left { elapsed -= self.left; self.advance(); }
        self.left -= elapsed;
        self.start = Some(now);
    }
}
static POMO: Mutex<Pomodoro> = Mutex::new(Pomodoro::new());
pub fn pomodoro() -> (bool, u64, u64, bool) {
    let mut p = POMO.lock().unwrap_or_else(|e| e.into_inner());
    p.settle(Instant::now());
    (p.rest, p.round, p.left.as_secs() + u64::from(p.left.subsec_nanos() > 0), p.start.is_some())
}
pub fn pomo_toggle() {
    let mut p = POMO.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now(); p.settle(now);
    p.start = if p.start.is_some() { None } else { Some(now) };
}
pub fn pomo_skip() {
    let mut p = POMO.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now(); p.settle(now); p.advance();
    if p.start.is_some() { p.start = Some(now); }
}
pub fn pomo_reset() { *POMO.lock().unwrap_or_else(|e| e.into_inner()) = Pomodoro::new(); }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopwatch_laps_pause_and_reset() {
        let n = Instant::now(); let mut s = Stopwatch::default();
        s.lap(n); assert!(s.laps.is_empty());
        s.toggle(n); s.lap(n + Duration::from_millis(1250));
        s.toggle(n + Duration::from_millis(2500));
        assert_eq!(s.laps, [Duration::from_millis(1250)]);
        assert_eq!(s.at(n + Duration::from_secs(99)), Duration::from_millis(2500));
        s.toggle(n + Duration::from_secs(100));
        assert_eq!(s.at(n + Duration::from_secs(101)), Duration::from_millis(3500));
        assert_eq!(watch_text(Duration::from_millis(61234)), "01:01.2");
    }
    #[test]
    fn pomodoro_rolls_through_work_break_and_sleep() {
        let n = Instant::now(); let mut p = Pomodoro::new(); p.start = Some(n);
        p.settle(n + Duration::from_secs(1500));
        assert!(p.rest); assert_eq!(p.left.as_secs(), 300); assert_eq!(p.round, 1);
        p.settle(n + Duration::from_secs(1800));
        assert!(!p.rest); assert_eq!(p.round, 2);
        p.settle(n + Duration::from_secs(18000 + 45));
        assert_eq!(p.round, 11); assert_eq!(p.left.as_secs(), 1455);
    }
    #[test]
    fn pomodoro_preserves_fraction_on_pause_and_skip() {
        let n = Instant::now(); let mut p = Pomodoro::new(); p.start = Some(n);
        p.settle(n + Duration::from_millis(1500)); p.start = None;
        p.settle(n + Duration::from_secs(99)); assert_eq!(p.left.as_millis(), 1498500);
        p.advance(); assert!(p.rest); assert_eq!(p.left.as_secs(), 300);
    }
}
