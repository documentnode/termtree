//! `RunSettings`: every settle time, repetition count, poll interval, and
//! sample count the harness uses. Serialised into the result file so a
//! published number carries the conditions it was taken under (spec FR-6,
//! FR-11).

use serde::{Deserialize, Serialize};

/// Repetitions for tiers whose per-repetition cost is low enough to afford
/// the spec's literal "at least 20" (fresh-launch, cold start, idle CPU).
/// See design §2.3's FR-12 deviation for why the heavier tiers do not share
/// this count.
pub const CHEAP_TIER_REPETITIONS: u32 = 20;
/// Disclosed lower repetition count for the sustained-use tier (~11 min per
/// repetition makes 20 repetitions ~31 h of exclusive machine time; design
/// §2.3, §10).
pub const SUSTAINED_USE_REPETITIONS: u32 = 5;
/// Disclosed lower repetition count for each N-session tier (~4 min per
/// repetition; design §2.3, §10).
pub const N_SESSION_REPETITIONS: u32 = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunSettings {
  pub repetitions: TierRepetitions,
  pub discard_first: bool,
  pub fresh_launch_settle_ms: u64,
  pub sustained_use_duration_s: u64,
  pub n_session_settle_s: u64,
  pub seed_timeout_s: u64,
  pub idle_cpu_sample_interval_ms: u64,
  pub idle_cpu_sample_count: u32,
  pub window_visible_poll_ms: u64,
  pub log_tail_poll_ms: u64,
  pub helper_drain_timeout_ms: u64,
  pub quiesce_window_s: u64,
  pub main_window_area_fraction: f64,
}

/// Per-tier repetition counts (design §2.3's FR-12 deviation): cheap tiers
/// take the full count, expensive tiers take a disclosed lower count. Every
/// count named here is printed in the result file's `settings` block and in
/// the rendered table -- never an undisclosed shortfall (NFR-2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TierRepetitions {
  pub fresh_launch: u32,
  pub sustained_use: u32,
  pub n_session: u32,
}

impl Default for TierRepetitions {
  fn default() -> Self {
    Self {
      fresh_launch: CHEAP_TIER_REPETITIONS,
      sustained_use: SUSTAINED_USE_REPETITIONS,
      n_session: N_SESSION_REPETITIONS,
    }
  }
}

impl Default for RunSettings {
  fn default() -> Self {
    Self {
      repetitions: TierRepetitions::default(),
      discard_first: true,
      fresh_launch_settle_ms: 15_000,
      sustained_use_duration_s: 600,
      n_session_settle_s: 120,
      seed_timeout_s: 300,
      idle_cpu_sample_interval_ms: 1_000,
      idle_cpu_sample_count: 30,
      window_visible_poll_ms: 20,
      log_tail_poll_ms: 20,
      helper_drain_timeout_ms: 10_000,
      quiesce_window_s: 5,
      main_window_area_fraction: 0.5,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_settings_round_trip_through_json() {
    let settings = RunSettings::default();
    let json = serde_json::to_string(&settings).unwrap();
    let parsed: RunSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(settings, parsed);
  }

  #[test]
  fn tiered_repetitions_are_disclosed_and_unequal() {
    // The whole point of the tiered-repetition decision (design §2.3) is
    // that the heavy tiers take fewer, disclosed repetitions rather than
    // silently falling short of a uniform count.
    let repetitions = TierRepetitions::default();
    assert_eq!(repetitions.fresh_launch, CHEAP_TIER_REPETITIONS);
    assert!(repetitions.sustained_use < CHEAP_TIER_REPETITIONS);
    assert!(repetitions.n_session < CHEAP_TIER_REPETITIONS);
  }
}
