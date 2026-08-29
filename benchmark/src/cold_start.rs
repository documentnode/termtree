//! Cold-start / time-to-interactive measurement (spec FR-4, FR-6, design
//! §5.4): wall-clock time from launch invocation to the window-visible and
//! (TermTree-only) log-mark readiness signals, timed by an external
//! monotonic clock -- never `hyperfine` (it measures process exit; a
//! documented 500x artifact applies to long-running GUI apps that never
//! exit) and never a log timestamp. Log lines are arrival-timestamped here
//! instead, which is what keeps the timing independent of the log's own
//! precision (millisecond-granularity since taskhub#672, see
//! `log_marks.rs`) and of its flush latency.
//!
//! The live launch-and-poll loop is not unit-testable (design §11); the
//! pure decision functions it calls -- PID discovery from a process-table
//! delta, and the calibrated main-window-area rule -- are.

use crate::exec::{run_capture, ExecError};
use crate::process_tree::ProcessRecord;
use crate::window_probe::OnScreenWindow;
use std::collections::HashSet;
use std::path::Path;

pub const OPEN_PROGRAM: &str = "/usr/bin/open";

/// `open -n -F --env HOME=<scratch_home> -a <bundle_path>` (spec item 1):
///
/// - `-n` always launches a **new** instance rather than activating one
///   already running -- required so a subject launched with one scratch
///   `HOME` never gets silently handed off to an instance still resident
///   from a different `HOME`. `-n` does **not** bypass a subject's own
///   single-instance guard (see [`crate::run::find_already_running_subject`]
///   for the separate check that does matter for that).
/// - `--env HOME=<scratch_home>` is what actually isolates the subject:
///   every seeder writes under this same directory
///   (`seeding/termtree.rs`'s `expected_scratch_state_path`, `seeding/
///   collaborator.rs`, `seeding/codenomad.rs`, `seeding/diri.rs`), so the
///   launched process must read its state from the identical path.
/// - `-F` suppresses window/session restoration (spec FR-4's last
///   acceptance criterion). A fresh scratch `HOME` has no saved window
///   state of its own to restore, so `-F` is no longer load-bearing the way
///   it was against a real profile -- kept anyway as a no-cost guard
///   against any restoration state macOS itself might keep outside the
///   app's `HOME` (e.g. a `bundle_identifier`-keyed system record), and
///   because it does not conflict with `-n`.
pub fn launch_suppressing_restoration(
  bundle_path: &str,
  scratch_home: &Path,
) -> Result<(), ExecError> {
  let home_env_arg = format!("HOME={}", scratch_home.display());
  run_capture(OPEN_PROGRAM, &[
    "-n",
    "-F",
    "--env",
    &home_env_arg,
    "-a",
    bundle_path,
  ])?;
  Ok(())
}

/// Whether this machine's `/usr/bin/open` documents `--env` at all. `man
/// open`'s minimum-macOS-version note could not be established reliably on
/// this development machine, so rather than hardcode an unverified version
/// constant, this probes the tool's own `--help` text -- side-effect-free,
/// consistent with `doctor`'s "changes nothing" contract -- and the caller
/// fails loudly, naming the missing flag, instead of silently launching
/// every subject against the runner's real `$HOME` (spec item 1).
pub fn supports_env_flag() -> bool {
  run_capture(OPEN_PROGRAM, &["--help"])
    .map(|output| {
      output.stdout.contains("--env") || output.stderr.contains("--env")
    })
    .unwrap_or(false)
}

/// Finds the PID of a newly-appeared process whose executable path is under
/// `bundle_path`, given the process table from before and after launch.
/// `open` does not report the launched PID itself, so discovery is by
/// process-table delta (design §5.4 step 2). Pure and fixture-testable.
pub fn find_newly_launched_pid(
  before: &[ProcessRecord],
  after: &[ProcessRecord],
  bundle_path: &str,
) -> Option<u32> {
  let before_pids: HashSet<u32> = before.iter().map(|r| r.pid).collect();
  after
    .iter()
    .find(|record| {
      !before_pids.contains(&record.pid)
        && record
          .executable_path
          .as_deref()
          .is_some_and(|path| path.starts_with(bundle_path))
    })
    .map(|record| record.pid)
}

/// A window qualifies as the **main** window once its area is at least
/// `fraction` of the subject's calibrated main-window area (design §5.4
/// step 3) -- a per-subject constant from the calibration launch, not a
/// hard-coded pixel threshold, so the rule is identical across subjects
/// with different splash/main window geometry.
pub fn is_main_window(
  window: &OnScreenWindow,
  calibrated_main_window_area_pt: f64,
  fraction: f64,
) -> bool {
  window.layer == 0
    && window.area_pt >= calibrated_main_window_area_pt * fraction
}

/// The largest on-screen, layer-0 window area observed during the
/// calibration launch becomes `calibratedMainWindowAreaPt`, reused by every
/// counted repetition (design §5.4 step 3, spec FR-12's mandatory first-run
/// discard doubling as the calibration launch).
pub fn calibrate_main_window_area(windows: &[OnScreenWindow]) -> Option<f64> {
  windows
    .iter()
    .filter(|w| w.layer == 0)
    .map(|w| w.area_pt)
    .fold(None, |max, area| {
      Some(max.map_or(area, |m: f64| m.max(area)))
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn record(pid: u32, exe: &str) -> ProcessRecord {
    ProcessRecord {
      pid,
      ppid: 1,
      name: "x".into(),
      executable_path: Some(exe.into()),
      rss_bytes: 0,
    }
  }

  #[test]
  fn finds_the_newly_launched_pid_under_the_bundle_path() {
    let before = vec![record(1, "/Applications/Other.app/other")];
    let after = vec![
      record(1, "/Applications/Other.app/other"),
      record(2, "/Applications/TermTree.app/Contents/MacOS/termtree"),
    ];
    let found =
      find_newly_launched_pid(&before, &after, "/Applications/TermTree.app");
    assert_eq!(found, Some(2));
  }

  #[test]
  fn ignores_a_new_pid_under_a_different_bundle() {
    let before = vec![];
    let after = vec![record(2, "/Applications/Other.app/other")];
    let found =
      find_newly_launched_pid(&before, &after, "/Applications/TermTree.app");
    assert_eq!(found, None);
  }

  #[test]
  fn main_window_rule_uses_the_calibrated_area_not_a_fixed_pixel_count() {
    let splash = OnScreenWindow {
      owner_pid: 1,
      layer: 0,
      area_pt: 400.0,
    };
    let main = OnScreenWindow {
      owner_pid: 1,
      layer: 0,
      area_pt: 1_310_720.0,
    };
    let calibrated = 1_310_720.0;
    assert!(!is_main_window(&splash, calibrated, 0.5));
    assert!(is_main_window(&main, calibrated, 0.5));
  }

  #[test]
  fn non_layer_zero_windows_never_qualify_as_main() {
    let overlay = OnScreenWindow {
      owner_pid: 1,
      layer: 3,
      area_pt: 2_000_000.0,
    };
    assert!(!is_main_window(&overlay, 1_310_720.0, 0.5));
  }

  #[test]
  fn calibration_picks_the_largest_layer_zero_window() {
    let windows = vec![
      OnScreenWindow {
        owner_pid: 1,
        layer: 0,
        area_pt: 400.0,
      },
      OnScreenWindow {
        owner_pid: 1,
        layer: 3,
        area_pt: 9_999_999.0,
      },
      OnScreenWindow {
        owner_pid: 1,
        layer: 0,
        area_pt: 1_310_720.0,
      },
    ];
    assert_eq!(calibrate_main_window_area(&windows), Some(1_310_720.0));
  }

  #[test]
  fn calibration_of_no_windows_is_none() {
    assert_eq!(calibrate_main_window_area(&[]), None);
  }
}
