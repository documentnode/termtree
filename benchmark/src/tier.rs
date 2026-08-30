//! The measurement tiers (spec §3's terminology table; FR-6, FR-7, FR-8).

use crate::settings::RunSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
  FreshLaunch,
  SustainedUse,
  NSession(u32),
}

impl Tier {
  pub fn as_str(&self) -> String {
    match self {
      Self::FreshLaunch => "fresh-launch".to_string(),
      Self::SustainedUse => "sustained-use".to_string(),
      Self::NSession(n) => format!("n-session-{n}"),
    }
  }

  pub fn session_count(&self) -> u32 {
    match self {
      Self::FreshLaunch => 0,
      // The sustained-use tier seeds the same 5-session configuration as
      // the N=5 tier (design §5.6.5).
      Self::SustainedUse => 5,
      Self::NSession(n) => *n,
    }
  }

  /// The disclosed, tiered repetition count for this tier (design §2.3's
  /// FR-12 deviation) -- cheap tiers get the full count, expensive tiers
  /// get fewer, and every count is printed in `settings` and the rendered
  /// table.
  pub fn repetitions(&self, settings: &RunSettings) -> u32 {
    match self {
      Self::FreshLaunch => settings.repetitions.fresh_launch,
      Self::SustainedUse => settings.repetitions.sustained_use,
      Self::NSession(_) => settings.repetitions.n_session,
    }
  }

  pub fn parse(text: &str) -> Option<Self> {
    match text {
      "fresh-launch" => Some(Self::FreshLaunch),
      "sustained-use" => Some(Self::SustainedUse),
      other => other
        .strip_prefix("n-session-")
        .and_then(|n| n.parse::<u32>().ok())
        .map(Self::NSession),
    }
  }
}

/// The default tier sweep for `just benchmark run` with no `--tiers`
/// override -- "a full subject/tier sweep" (spec FR-16).
pub const DEFAULT_TIERS: &[Tier] = &[
  Tier::FreshLaunch,
  Tier::NSession(5),
  Tier::NSession(10),
  Tier::NSession(20),
  Tier::SustainedUse,
];

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tier_names_round_trip_through_parse() {
    for tier in DEFAULT_TIERS {
      assert_eq!(Tier::parse(&tier.as_str()), Some(*tier));
    }
  }

  #[test]
  fn n_session_tiers_carry_their_session_count() {
    assert_eq!(Tier::NSession(20).session_count(), 20);
    assert_eq!(Tier::FreshLaunch.session_count(), 0);
    assert_eq!(Tier::SustainedUse.session_count(), 5);
  }

  #[test]
  fn repetitions_use_the_tiered_counts() {
    let settings = RunSettings::default();
    assert_eq!(
      Tier::FreshLaunch.repetitions(&settings),
      settings.repetitions.fresh_launch
    );
    assert_eq!(
      Tier::NSession(20).repetitions(&settings),
      settings.repetitions.n_session
    );
    assert_eq!(
      Tier::SustainedUse.repetitions(&settings),
      settings.repetitions.sustained_use
    );
  }

  #[test]
  fn parse_rejects_garbage() {
    assert_eq!(Tier::parse("not-a-tier"), None);
    assert_eq!(Tier::parse("n-session-abc"), None);
  }
}
