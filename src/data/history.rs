//! Rolling CPU and RAM samples for the sparkline graphs (2 minutes at 1 Hz).
use std::collections::VecDeque;

use crate::model::Stats;

/// 2 minutes of samples at 1 Hz.
pub const CAPACITY: usize = 120;

/// Both series hold percentages (0..=100), oldest first.
#[derive(Debug, Default)]
pub struct History {
    pub cpu: VecDeque<f32>,
    pub mem: VecDeque<f32>,
}

fn push_capped(series: &mut VecDeque<f32>, v: f32) {
    if series.len() == CAPACITY {
        series.pop_front();
    }
    series.push_back(v);
}

impl History {
    /// Appends one sample. RAM is stored as a percentage of total; 0 when the total is unknown.
    pub fn push(&mut self, s: &Stats) {
        let mem_pct = if s.ram_total_mb > 0 {
            (s.ram_used_mb as f32 / s.ram_total_mb as f32 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        push_capped(&mut self.cpu, s.cpu_pct.clamp(0.0, 100.0));
        push_capped(&mut self.mem, mem_pct);
    }

    /// Copies both series, oldest first, for drawing.
    pub fn snapshot(&self) -> (Vec<f32>, Vec<f32>) {
        (self.cpu.iter().copied().collect(), self.mem.iter().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_120_samples() {
        let mut h = History::default();
        for i in 0..150 {
            h.push(&Stats { cpu_pct: i as f32 % 100.0, ram_used_mb: 50, ram_total_mb: 100 });
        }
        let (cpu, mem) = h.snapshot();
        assert_eq!(cpu.len(), CAPACITY);
        assert_eq!(mem.len(), CAPACITY);
        assert_eq!(cpu[0], 30.0);
        assert_eq!(mem[0], 50.0);
    }

    #[test]
    fn zero_total_ram_is_zero_percent() {
        let mut h = History::default();
        h.push(&Stats { cpu_pct: 150.0, ram_used_mb: 5, ram_total_mb: 0 });
        assert_eq!(h.snapshot(), (vec![100.0], vec![0.0]));
    }
}
