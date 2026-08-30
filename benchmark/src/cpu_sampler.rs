//! Idle-CPU sampling (spec FR-5, design §5.5): `sysinfo`'s two-refresh
//! **interval** delta, never `ps -o %cpu`'s lifetime average.
//!
//! `idleCpuPercentOfOneCore`: `sysinfo` reports CPU usage relative to a
//! single core, so 100 means one fully saturated core -- the host's logical
//! core count lives in provenance, not in this field, so a reader is never
//! left to guess what "100" means on an 8-core machine.

use crate::stats::{self, Summary};
use std::thread;
use std::time::Duration;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// Samples the CPU usage of `pids` `sample_count` times, `interval_ms`
/// apart, and returns the median/IQR of the per-sample **summed** interval
/// readings across the attributable set (design §5.5).
pub fn sample_idle_cpu(
  pids: &[u32],
  interval_ms: u64,
  sample_count: u32,
) -> Option<Summary> {
  let mut system = System::new();
  let mut readings = Vec::with_capacity(sample_count as usize);

  for _ in 0..sample_count {
    system.refresh_processes_specifics(
      ProcessesToUpdate::All,
      true,
      ProcessRefreshKind::nothing().with_cpu(),
    );
    thread::sleep(Duration::from_millis(interval_ms));
    system.refresh_processes_specifics(
      ProcessesToUpdate::All,
      true,
      ProcessRefreshKind::nothing().with_cpu(),
    );
    let total: f32 = pids
      .iter()
      .filter_map(|pid| {
        system
          .process(sysinfo::Pid::from_u32(*pid))
          .map(|p| p.cpu_usage())
      })
      .sum();
    readings.push(total as f64);
  }

  stats::summarize(&readings)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sampling_zero_pids_returns_all_zero_readings() {
    let summary = sample_idle_cpu(&[], 5, 3).unwrap();
    assert_eq!(summary.median, 0.0);
    assert_eq!(summary.n, 3);
  }
}
