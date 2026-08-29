//! `karijini.log` line classification (spec FR-4, design §5.4).
//!
//! `karijini.log`'s own timestamps are millisecond-granularity
//! (`logging.rs`'s `{d(%Y-%m-%d %H:%M:%S%.3f)}`, widened from seconds by
//! taskhub#672) and are **never** parsed for timing here -- only for ordering
//! sanity. The harness times each line's *arrival* on its own monotonic clock
//! (`cold_start.rs`); this module only classifies which mark, if any, a line
//! represents.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMark {
  AppWindowReadyMain,
  SplashClosed,
  SplashTimeout,
}

/// The exact message text (i.e. everything after the `{level} {target} -
/// ` prefix) that identifies each mark. Matching the **whole** message,
/// not a substring search over the raw line, is what keeps a line that
/// merely mentions one of these phrases inside unrelated prose from being
/// misclassified (design §11's explicit false-positive test).
const APP_WINDOW_READY_MAIN_MESSAGE: &str = "app_window_ready: main";
const SPLASH_CLOSED_MESSAGE: &str =
  "splash_monitor - main window ready, closing splashscreen";
const SPLASH_TIMEOUT_MESSAGE: &str =
  "splash_monitor - max timeout reached, forcing transition";

/// Extracts the message portion of one `karijini.log` line in the
/// `{d(%Y-%m-%d %H:%M:%S%.3f)} {l} {t} - {m}` format (`logging.rs`). The
/// date field itself contains a space, so the message is not simply "the
/// text after the 4th space" -- it is everything after the first `" - "`
/// that follows the (date, time, level, target) prefix, i.e. after the 5th
/// whitespace-delimited token.
///
/// The four skipped tokens are date, time, level and target. The seconds
/// field carries the fractional part (`18:00:39.612`), so widening the
/// timestamp's precision does not change the token count -- which is why
/// taskhub#672 could add milliseconds without touching this parser. A
/// timezone suffix or a single-token RFC3339 timestamp would change it, and
/// would make every mark classify as absent rather than fail loudly.
fn message_of(line: &str) -> Option<&str> {
  let mut rest = line;
  for _ in 0..4 {
    let (_, remainder) = rest.split_once(char::is_whitespace)?;
    rest = remainder.trim_start();
  }
  rest.strip_prefix("- ")
}

pub fn classify_log_mark(line: &str) -> Option<LogMark> {
  let message = message_of(line)?;
  if message == APP_WINDOW_READY_MAIN_MESSAGE {
    Some(LogMark::AppWindowReadyMain)
  } else if message == SPLASH_CLOSED_MESSAGE {
    Some(LogMark::SplashClosed)
  } else if message == SPLASH_TIMEOUT_MESSAGE {
    Some(LogMark::SplashTimeout)
  } else {
    None
  }
}

pub fn classify_log_text(text: &str) -> Vec<LogMark> {
  text.lines().filter_map(classify_log_mark).collect()
}

/// Spec item 4: whether the harness read `karijini.log` lines during a
/// TermTree launch's settle window without recognizing a single one of
/// them as a known mark -- a real drift signal (`app_window_ready: main`,
/// the splash-closed message, or the splash-timeout message have all
/// changed) rather than the legitimate cases that also leave every mark
/// `None`: this is not a TermTree launch at all (`is_termtree_launch ==
/// false`), or the log simply had not advanced yet at the settle deadline
/// (`any_log_line_observed == false`). A `splash_timeout_seen` mark still
/// counts as recognized, since it is itself one of the three known
/// messages.
pub fn marks_unrecognized(
  is_termtree_launch: bool,
  any_log_line_observed: bool,
  app_window_ready_ms: Option<u64>,
  splash_close_ms: Option<u64>,
  splash_timeout_seen: bool,
) -> bool {
  is_termtree_launch
    && any_log_line_observed
    && app_window_ready_ms.is_none()
    && splash_close_ms.is_none()
    && !splash_timeout_seen
}

/// TermTree's own data directory under the app-support root -- the parent
/// of [`karijini_log_path`]'s log file and the exact directory
/// `seeding/termtree.rs`'s writer targets (`TermTreeSeeder::production`).
/// Factored out to one join point so it, `karijini_log_path`, and
/// `run.rs`'s post-launch "did the app actually create its data
/// directory" drift check (spec item 4) can never independently drift
/// from each other.
pub fn app_data_dir(app_support_dir: &Path) -> std::path::PathBuf {
  app_support_dir.join("DocumentNode").join("TermTree")
}

pub fn karijini_log_path(app_support_dir: &Path) -> std::path::PathBuf {
  app_data_dir(app_support_dir)
    .join("logs")
    .join("karijini.log")
}

#[cfg(test)]
mod tests {
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
  fn cold_start_log_carries_both_marks_in_order() {
    let marks = classify_log_text(&read("karijini-cold-start.log"));
    assert_eq!(marks, vec![
      LogMark::AppWindowReadyMain,
      LogMark::SplashClosed
    ]);
  }

  #[test]
  fn splash_timeout_log_is_classified_and_no_false_positive_fires() {
    let marks = classify_log_text(&read("karijini-splash-timeout.log"));
    // The fixture contains a WARN line whose message merely *mentions* the
    // splash-timeout phrase inside different prose; it must not be
    // misclassified as the real mark. Only the genuine
    // `splash_monitor - max timeout reached, forcing transition` line
    // should classify.
    assert_eq!(marks, vec![LogMark::SplashTimeout]);
  }

  /// taskhub#672 widened `logging.rs`'s timestamp to milliseconds. The
  /// fractional part rides the *seconds* token, so the prefix `message_of`
  /// skips is still exactly four tokens (date, time, level, target). A
  /// timezone suffix -- or a single-token RFC3339 timestamp -- would change
  /// that count, and every mark would then classify as absent instead of
  /// failing loudly. This pins the boundary from both sides.
  #[test]
  fn millisecond_timestamp_keeps_the_four_token_prefix() {
    let millisecond_timestamp =
      "2026-08-24 18:00:39.612 INFO termtree_lib::command::window_cmd - app_window_ready: main";
    assert_eq!(
      classify_log_mark(millisecond_timestamp),
      Some(LogMark::AppWindowReadyMain)
    );

    let timezone_suffixed_timestamp =
      "2026-08-24 18:00:39.612 +10:00 INFO termtree_lib::command::window_cmd - app_window_ready: main";
    assert_eq!(classify_log_mark(timezone_suffixed_timestamp), None);
  }

  #[test]
  fn app_window_ready_for_splashscreen_label_is_not_the_main_mark() {
    let line =
      "2026-08-24 18:00:39.612 INFO termtree_lib::command::window_cmd - app_window_ready: splashscreen";
    assert_eq!(classify_log_mark(line), None);
  }

  #[test]
  fn unrecognized_when_lines_advanced_but_none_classified() {
    assert!(marks_unrecognized(true, true, None, None, false));
  }

  #[test]
  fn not_unrecognized_when_the_log_never_advanced() {
    // No new lines were seen yet -- too early to call this drift.
    assert!(!marks_unrecognized(true, false, None, None, false));
  }

  #[test]
  fn not_unrecognized_for_a_non_termtree_subject() {
    assert!(!marks_unrecognized(false, true, None, None, false));
  }

  #[test]
  fn not_unrecognized_once_any_known_mark_is_present() {
    assert!(!marks_unrecognized(true, true, Some(1500), None, false));
    assert!(!marks_unrecognized(true, true, None, Some(2100), false));
    assert!(!marks_unrecognized(true, true, None, None, true));
  }

  #[test]
  fn karijini_log_path_is_nested_under_app_data_dir() {
    let app_support = Path::new("/scratch/Library/Application Support");
    assert_eq!(
      karijini_log_path(app_support),
      app_data_dir(app_support).join("logs").join("karijini.log")
    );
    assert_eq!(
      app_data_dir(app_support),
      Path::new("/scratch/Library/Application Support/DocumentNode/TermTree")
    );
  }

  #[test]
  fn unrelated_log_line_classifies_as_none() {
    let line =
      "2026-08-24 18:00:38.244 INFO termtree_lib - splash_monitor - started (max=15s)";
    assert_eq!(classify_log_mark(line), None);
  }
}
