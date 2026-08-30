//! The quiesce gate (spec FR-10, design §5.7): five unprivileged signals
//! that must all read nominal before a run starts, checked again before
//! every sample and periodically during N-session measurement.
//!
//! An unreadable safety signal is treated as `unknown`, which **fails** the
//! gate -- an unparseable thermal value is never read as "safe" (design §9).

use crate::exec::run_capture;
use serde::{Deserialize, Serialize};

pub const SYSCTL_PROGRAM: &str = "/usr/sbin/sysctl";
pub const PMSET_PROGRAM: &str = "/usr/bin/pmset";
pub const NOTIFYUTIL_PROGRAM: &str = "/usr/bin/notifyutil";

/// `sysctl -n kern.memorystatus_vm_pressure_level` must read exactly this
/// value for the gate to pass (design §5.7's table).
pub const NOMINAL_MEMORY_PRESSURE_LEVEL: i64 = 1;
pub const NOMINAL_THERMAL_PRESSURE_LEVEL: i64 = 0;
/// Swap usage above this, in bytes, fails the gate even with zero swap
/// *activity* -- a host already deep in swap is not quiesced just because
/// nothing swapped out in the last window.
pub const QUIESCE_SWAP_USED_LIMIT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuiesceVerdict {
  Pass,
  Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuiesceReading {
  pub memory_pressure_level: Option<i64>,
  pub swap_used_bytes: Option<u64>,
  pub swap_free_bytes: Option<u64>,
  pub swapouts_delta: Option<u64>,
  pub on_ac_power: Option<bool>,
  pub thermal_pressure_level: Option<i64>,
  pub verdict: QuiesceVerdict,
  pub failing_signals: Vec<String>,
}

impl QuiesceReading {
  #[cfg(test)]
  pub fn nominal_for_test() -> Self {
    Self {
      memory_pressure_level: Some(NOMINAL_MEMORY_PRESSURE_LEVEL),
      swap_used_bytes: Some(0),
      swap_free_bytes: Some(1),
      swapouts_delta: Some(0),
      on_ac_power: Some(true),
      thermal_pressure_level: Some(NOMINAL_THERMAL_PRESSURE_LEVEL),
      verdict: QuiesceVerdict::Pass,
      failing_signals: Vec::new(),
    }
  }
}

pub fn parse_memory_pressure_level(text: &str) -> Option<i64> {
  text.trim().parse::<i64>().ok()
}

/// `sysctl vm.swapusage` → `vm.swapusage: total = 18432.00M  used =
/// 17377.19M  free = 1054.81M  (encrypted)`.
pub fn parse_swapusage(text: &str) -> Option<(u64, u64)> {
  let used = parse_megabyte_field(text, "used = ")?;
  let free = parse_megabyte_field(text, "free = ")?;
  Some((used, free))
}

fn parse_megabyte_field(text: &str, label: &str) -> Option<u64> {
  let after = text.split(label).nth(1)?;
  let token = after
    .split(|c: char| c == 'M' || c.is_whitespace())
    .next()?;
  let megabytes: f64 = token.parse().ok()?;
  Some((megabytes * 1024.0 * 1024.0) as u64)
}

/// `pmset -g ps` first line: `Now drawing from 'AC Power'` or `'Battery
/// Power'`.
pub fn parse_power_source(text: &str) -> Option<bool> {
  let first_line = text.lines().next()?;
  if first_line.contains("'AC Power'") {
    Some(true)
  } else if first_line.contains("'Battery Power'") {
    Some(false)
  } else {
    None
  }
}

/// `notifyutil -g com.apple.system.thermalpressurelevel` prints
/// `com.apple.system.thermalpressurelevel 0` -- **space-separated**, not
/// colon-separated, unlike most of this crate's other parsers (design §5.7,
/// verified on the host).
pub fn parse_thermal_pressure_level(text: &str) -> Option<i64> {
  let trimmed = text.trim();
  let value = trimmed.rsplit(' ').next()?;
  value.parse::<i64>().ok()
}

/// Reads all five signals and computes the pass/fail verdict. Any signal
/// that fails to parse is `unknown`, which fails the gate rather than
/// passing it by omission.
pub fn read_quiesce_gate(swapouts_delta: Option<u64>) -> QuiesceReading {
  let pressure = run_capture(SYSCTL_PROGRAM, &[
    "-n",
    "kern.memorystatus_vm_pressure_level",
  ])
  .ok()
  .and_then(|o| parse_memory_pressure_level(&o.stdout));

  let swap = run_capture(SYSCTL_PROGRAM, &["vm.swapusage"])
    .ok()
    .and_then(|o| parse_swapusage(&o.stdout));

  let on_ac = run_capture(PMSET_PROGRAM, &["-g", "ps"])
    .ok()
    .and_then(|o| parse_power_source(&o.stdout));

  let thermal = run_capture(NOTIFYUTIL_PROGRAM, &[
    "-g",
    "com.apple.system.thermalpressurelevel",
  ])
  .ok()
  .and_then(|o| parse_thermal_pressure_level(&o.stdout));

  build_reading(
    pressure,
    swap.map(|(used, _)| used),
    swap.map(|(_, free)| free),
    swapouts_delta,
    on_ac,
    thermal,
  )
}

pub fn build_reading(
  memory_pressure_level: Option<i64>,
  swap_used_bytes: Option<u64>,
  swap_free_bytes: Option<u64>,
  swapouts_delta: Option<u64>,
  on_ac_power: Option<bool>,
  thermal_pressure_level: Option<i64>,
) -> QuiesceReading {
  let mut failing = Vec::new();

  match memory_pressure_level {
    Some(level) if level == NOMINAL_MEMORY_PRESSURE_LEVEL => {}
    _ => failing.push("memory-pressure".to_string()),
  }
  match swap_used_bytes {
    Some(used) if used < QUIESCE_SWAP_USED_LIMIT_BYTES => {}
    _ => failing.push("swap-level".to_string()),
  }
  match swapouts_delta {
    Some(0) => {}
    _ => failing.push("swap-activity".to_string()),
  }
  match on_ac_power {
    Some(true) => {}
    _ => failing.push("power-source".to_string()),
  }
  match thermal_pressure_level {
    Some(level) if level == NOMINAL_THERMAL_PRESSURE_LEVEL => {}
    _ => failing.push("thermal-pressure".to_string()),
  }

  let verdict = if failing.is_empty() {
    QuiesceVerdict::Pass
  } else {
    QuiesceVerdict::Fail
  };

  QuiesceReading {
    memory_pressure_level,
    swap_used_bytes,
    swap_free_bytes,
    swapouts_delta,
    on_ac_power,
    thermal_pressure_level,
    verdict,
    failing_signals: failing,
  }
}

/// What a runner must actually do to clear one failing quiesce signal.
///
/// Spec item 6: `doctor` is a third party's front door, so naming a
/// failing signal is not enough -- it must say how to clear it. The
/// signal names are the ones [`build_reading`] pushes.
pub fn remediation_for_signal(signal: &str) -> &'static str {
  match signal {
    "memory-pressure" => {
      "quit other applications and re-check; the gate needs \
       `sysctl kern.memorystatus_vm_pressure_level` to read 1 (nominal)"
    }
    "swap-level" => {
      "reboot to clear swap; the gate needs `sysctl vm.swapusage` to show \
       under 8 GB used"
    }
    "swap-activity" => {
      "the machine is actively swapping -- reboot, then leave it idle until \
       `sysctl vm.swapusage` stops reporting new swapouts"
    }
    "power-source" => "connect the machine to AC power",
    "thermal-pressure" => {
      "let the machine cool until `notifyutil -g \
       com.apple.system.thermalpressurelevel` reads 0 (nominal)"
    }
    _ => "see the harness README's quiesce prerequisites",
  }
}

#[cfg(test)]
mod tests {

  /// Every signal `build_reading` can push must have a real remedy --
  /// `doctor` is a third party's only guidance for clearing the gate, so a
  /// new signal falling through to the generic message is a regression.
  #[test]
  fn every_failing_signal_has_a_specific_remediation() {
    let reading = build_reading(
      Some(4),        // memory pressure: not nominal
      Some(u64::MAX), // swap used: over the limit
      Some(0),
      Some(9),     // swapouts happened
      Some(false), // on battery
      Some(3),     // thermal pressure: not nominal
    );
    assert_eq!(
      reading.failing_signals.len(),
      5,
      "expected every signal to fail: {:?}",
      reading.failing_signals
    );
    let generic = remediation_for_signal("not-a-real-signal");
    for signal in &reading.failing_signals {
      let remedy = remediation_for_signal(signal);
      assert_ne!(
        remedy, generic,
        "signal {signal} falls through to the generic remediation"
      );
      assert!(
        !remedy.is_empty(),
        "signal {signal} has an empty remediation"
      );
    }
  }

  use super::*;
  use std::fs;

  fn read(name: &str) -> String {
    fs::read_to_string(format!(
      "{}/fixtures/{name}",
      env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
  }

  #[test]
  fn parses_swapusage_into_bytes() {
    let (used, free) = parse_swapusage(&read("sysctl-swapusage.txt")).unwrap();
    assert_eq!(used, (17377.19 * 1024.0 * 1024.0) as u64);
    assert_eq!(free, (1054.81 * 1024.0 * 1024.0) as u64);
  }

  #[test]
  fn parses_ac_power_source() {
    assert_eq!(parse_power_source(&read("pmset-ps-ac.txt")), Some(true));
  }

  /// Live capture from the actual development host (`pmset -g ps`, macOS
  /// 15.7.4/24G517, the same OS build this crate's other fixtures target),
  /// taken while investigating a report that `doctor`'s `power-source`
  /// check disagreed with `pmset -g batt`. Both commands were verified to
  /// print byte-identical output on this host (`diff <(pmset -g ps)
  /// <(pmset -g batt)` -> no difference) at every reading taken during the
  /// investigation, so there is no format divergence between the two
  /// invocations for this parser to handle differently -- the discrepancy
  /// observed at some earlier moment was the host's actual power state
  /// changing (unplugged) between then and when `doctor` was run, not a
  /// parsing or invocation bug. This fixture pins the real captured
  /// battery-power shape (a tab before the percentage, "discharging", a
  /// "present: true" suffix) so a future macOS release that changes it is
  /// caught here rather than only in a live `doctor` run.
  #[test]
  fn parses_a_live_captured_battery_reading_from_this_host() {
    assert_eq!(
      parse_power_source(&read("pmset-ps-battery-live-capture.txt")),
      Some(false)
    );
  }

  #[test]
  fn parses_battery_power_source() {
    assert_eq!(
      parse_power_source(&read("pmset-ps-battery.txt")),
      Some(false)
    );
  }

  #[test]
  fn parses_nominal_thermal_pressure_space_separated() {
    assert_eq!(
      parse_thermal_pressure_level(&read("notifyutil-thermal-nominal.txt")),
      Some(0)
    );
  }

  #[test]
  fn parses_serious_thermal_pressure() {
    assert_eq!(
      parse_thermal_pressure_level(&read("notifyutil-thermal-serious.txt")),
      Some(2)
    );
  }

  #[test]
  fn unparseable_thermal_value_is_unknown_and_fails_the_gate() {
    assert_eq!(parse_thermal_pressure_level("garbage, not a number"), None);
    let reading = build_reading(
      Some(NOMINAL_MEMORY_PRESSURE_LEVEL),
      Some(0),
      Some(1),
      Some(0),
      Some(true),
      None,
    );
    assert_eq!(reading.verdict, QuiesceVerdict::Fail);
    assert!(reading
      .failing_signals
      .contains(&"thermal-pressure".to_string()));
  }

  #[test]
  fn all_five_signals_nominal_passes() {
    let reading = build_reading(
      Some(NOMINAL_MEMORY_PRESSURE_LEVEL),
      Some(0),
      Some(1),
      Some(0),
      Some(true),
      Some(NOMINAL_THERMAL_PRESSURE_LEVEL),
    );
    assert_eq!(reading.verdict, QuiesceVerdict::Pass);
    assert!(reading.failing_signals.is_empty());
  }

  #[test]
  fn memory_pressure_level_two_fails_the_gate() {
    // The host measured `kern.memorystatus_vm_pressure_level: 2` while this
    // design was written (design §5.7) -- the gate must refuse to start.
    let reading = build_reading(
      Some(2),
      Some(0),
      Some(1),
      Some(0),
      Some(true),
      Some(NOMINAL_THERMAL_PRESSURE_LEVEL),
    );
    assert_eq!(reading.verdict, QuiesceVerdict::Fail);
    assert!(reading
      .failing_signals
      .contains(&"memory-pressure".to_string()));
  }
}
