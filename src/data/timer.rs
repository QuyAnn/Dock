//! Monotonic stopwatch shared by every dock and the gallery. Pausing keeps fractions of a second.
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Timer {
    elapsed: Duration,
    started: Option<Instant>,
}

impl Timer {
    fn elapsed_at(&self, now: Instant) -> Duration {
        self.elapsed + self.started.map_or(Duration::ZERO, |at| now.saturating_duration_since(at))
    }

    fn toggle_at(&mut self, now: Instant) {
        self.elapsed = self.elapsed_at(now);
        self.started = if self.started.is_some() { None } else { Some(now) };
    }
}

static TIMER: Mutex<Timer> = Mutex::new(Timer { elapsed: Duration::ZERO, started: None });

pub fn snapshot() -> (u64, bool) {
    let t = TIMER.lock().unwrap_or_else(|e| e.into_inner());
    (t.elapsed_at(Instant::now()).as_secs(), t.started.is_some())
}

pub fn toggle() {
    TIMER.lock().unwrap_or_else(|e| e.into_inner()).toggle_at(Instant::now());
}

pub fn reset() {
    *TIMER.lock().unwrap_or_else(|e| e.into_inner()) = Timer::default();
}

pub fn time_text(secs: u64) -> String {
    if secs < 3600 {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    } else {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_pause_resume_preserves_elapsed_and_subseconds() {
        let now = Instant::now();
        let mut t = Timer::default();
        assert_eq!(t.elapsed_at(now), Duration::ZERO);
        t.toggle_at(now);
        t.toggle_at(now + Duration::from_millis(1500));
        assert!(t.started.is_none());
        assert_eq!(t.elapsed_at(now + Duration::from_secs(10)), Duration::from_millis(1500));
        t.toggle_at(now + Duration::from_secs(10));
        assert_eq!(t.elapsed_at(now + Duration::from_millis(10500)), Duration::from_secs(2));
        t = Timer::default();
        assert!(t.started.is_none());
        assert_eq!(t.elapsed_at(now), Duration::ZERO);
    }

    #[test]
    fn formats_minutes_and_hours() {
        assert_eq!(time_text(0), "00:00");
        assert_eq!(time_text(65), "01:05");
        assert_eq!(time_text(3599), "59:59");
        assert_eq!(time_text(3601), "1:00:01");
        assert_eq!(time_text(360000), "100:00:00");
    }
}
