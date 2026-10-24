//! Focus timer widget: 25 min of work, then a 5 min break, started and paused with a click.
//! State lives here (one timer for all docks); `tick` is called by the UI once a second.
use std::sync::Mutex;
use std::time::Instant;

pub const WORK_SECS: u32 = 25 * 60;
pub const BREAK_SECS: u32 = 5 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Work,
    Break,
}

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub running: bool,
    pub remaining: u32,
    pub total: u32,
}

struct Timer {
    phase: Phase,
    /// Seconds left when last paused or started.
    left: u32,
    started: Option<Instant>,
}

static TIMER: Mutex<Timer> = Mutex::new(Timer { phase: Phase::Work, left: WORK_SECS, started: None });

fn total_of(phase: Phase) -> u32 {
    match phase {
        Phase::Work => WORK_SECS,
        Phase::Break => BREAK_SECS,
    }
}

fn remaining(t: &Timer) -> u32 {
    match t.started {
        Some(at) => t.left.saturating_sub(at.elapsed().as_secs() as u32),
        None => t.left,
    }
}

pub fn snapshot() -> Snapshot {
    let t = TIMER.lock().unwrap();
    Snapshot { phase: t.phase, running: t.started.is_some(), remaining: remaining(&t), total: total_of(t.phase) }
}

/// Starts or pauses the timer.
pub fn toggle() {
    let mut t = TIMER.lock().unwrap();
    match t.started.take() {
        Some(at) => t.left = t.left.saturating_sub(at.elapsed().as_secs() as u32),
        None => t.started = Some(Instant::now()),
    }
}

/// Back to a full, stopped work session.
pub fn reset() {
    let mut t = TIMER.lock().unwrap();
    *t = Timer { phase: Phase::Work, left: WORK_SECS, started: None };
}

/// Moves to the next phase when time ran out. Returns true when that just happened (so the
/// caller can beep). The next phase starts running by itself, like a Pomodoro app.
pub fn tick() -> bool {
    let mut t = TIMER.lock().unwrap();
    if t.started.is_none() || remaining(&t) > 0 {
        return false;
    }
    t.phase = if t.phase == Phase::Work { Phase::Break } else { Phase::Work };
    t.left = total_of(t.phase);
    t.started = Some(Instant::now());
    true
}

/// "24:59".
pub fn mmss(secs: u32) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmss_formats() {
        assert_eq!(mmss(WORK_SECS), "25:00");
        assert_eq!(mmss(65), "01:05");
    }
}
