//! The disposable per-run scratch home (spec item 1): every subject is
//! seeded, launched, and measured with `HOME` pointed at a fresh directory
//! this harness owns, never at the runner's real `$HOME`. A stranger
//! running this harness must never have it touch their real application
//! profiles -- so unlike most of this crate's other configuration knobs,
//! there is deliberately no fallback to the real environment: the default
//! is always a brand-new directory under the OS temp dir, created fresh
//! for this run and never reused across runs unless explicitly overridden.
//!
//! `--home <path>` (highest precedence) and `RESOURCE_BENCHMARK_HOME`
//! follow this crate's existing `RESOURCE_BENCHMARK_*` override convention
//! (`main.rs`'s `RESOURCE_BENCHMARK_REPO_PATH` /
//! `RESOURCE_BENCHMARK_AGENT_CLI_PATH`). An explicit override is on the
//! caller: if they point it at their real `$HOME`, that is no longer this
//! harness silently doing it to them.
//!
//! This is the seam every seeder already keys off of (`seeding/mod.rs`'s
//! `home: &Path` parameter, `seeding/termtree.rs`'s
//! `expected_scratch_state_path`) -- resolving it here, once, is what makes
//! "never the real $HOME by default" structural rather than a convention
//! every call site has to remember.

use std::path::{Path, PathBuf};

pub const HOME_OVERRIDE_ENV: &str = "RESOURCE_BENCHMARK_HOME";

/// The directory name for one run's scratch home -- unique per process
/// invocation (`pid`) and per call (`disambiguator`, normally a
/// nanosecond-resolution timestamp) so two runs, or two calls within the
/// same process, never collide. Pure so the naming rule is testable
/// without touching the filesystem.
pub fn scratch_home_directory_name(pid: u32, disambiguator: u128) -> String {
  format!("resource-benchmark-home-{pid}-{disambiguator}")
}

/// Resolves the scratch home for this run: an explicit `--home` value wins,
/// then `RESOURCE_BENCHMARK_HOME`, then a freshly named directory under
/// `temp_dir`. Pure given already-read inputs, so the precedence order is
/// unit-tested without reading the real environment or touching the real
/// filesystem.
pub fn resolve_scratch_home(
  cli_override: Option<&str>,
  env_override: Option<String>,
  temp_dir: &Path,
  pid: u32,
  disambiguator: u128,
) -> PathBuf {
  if let Some(path) = cli_override {
    return PathBuf::from(path);
  }
  if let Some(path) = env_override {
    return PathBuf::from(path);
  }
  temp_dir.join(scratch_home_directory_name(pid, disambiguator))
}

/// Creates the scratch home directory if it does not already exist. An
/// explicit `--home`/env override may point at a directory that already
/// exists (a re-runner reusing one deliberately), which is not an error.
pub fn ensure_scratch_home_exists(path: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(path)
}

/// Refuses a resolved scratch home that would put the harness back on the
/// runner's real profile.
///
/// The default scratch home can never be the real `$HOME`, but an explicit
/// `--home`/`RESOURCE_BENCHMARK_HOME` can be -- and spec item 1 states the
/// property unconditionally ("the runner's real application state is never
/// read or written"), not "unless you asked for it". A stranger who types
/// `--home $HOME` to see what happens should get a refusal, not silent
/// data loss in the profile they actually use.
///
/// Also refuses an *ancestor* of the real home (`/`, `/Users`, `/Users/x`
/// when the real home is `/Users/x/nested`), since seeding under one still
/// resolves into the real profile's tree.
///
/// Pure: the real home is passed in, never read here, so the rule is
/// unit-testable without touching the environment.
pub fn reject_real_home(
  resolved: &Path,
  real_home: Option<&Path>,
) -> Result<(), String> {
  let Some(real_home) = real_home else {
    return Ok(());
  };
  if real_home.as_os_str().is_empty() {
    return Ok(());
  }
  if real_home.starts_with(resolved) {
    return Err(format!(
      "Refusing to use {} as the scratch home: it is the real home \
       directory ({}) or contains it, so seeding would overwrite the \
       runner's own application profiles. Omit --home/{} to get a fresh \
       disposable directory, or pass one outside the real home.",
      resolved.display(),
      real_home.display(),
      HOME_OVERRIDE_ENV
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {

  #[test]
  fn rejects_the_real_home_itself() {
    let error =
      reject_real_home(Path::new("/Users/dev"), Some(Path::new("/Users/dev")))
        .unwrap_err();
    assert!(error.contains("Refusing"), "{error}");
    assert!(error.contains("/Users/dev"), "{error}");
  }

  #[test]
  fn rejects_an_ancestor_of_the_real_home() {
    assert!(reject_real_home(
      Path::new("/Users"),
      Some(Path::new("/Users/dev"))
    )
    .is_err());
    assert!(
      reject_real_home(Path::new("/"), Some(Path::new("/Users/dev"))).is_err()
    );
  }

  #[test]
  fn allows_a_scratch_directory_outside_the_real_home() {
    assert!(reject_real_home(
      Path::new("/var/folders/tmp/resource-benchmark-home-1-2"),
      Some(Path::new("/Users/dev"))
    )
    .is_ok());
  }

  #[test]
  fn allows_a_directory_nested_inside_the_real_home() {
    // Deliberate: a runner may keep a reusable scratch home under their own
    // home directory. That writes only where they pointed it, never into
    // the real profile's application-support tree above it.
    assert!(reject_real_home(
      Path::new("/Users/dev/benchmark-scratch"),
      Some(Path::new("/Users/dev"))
    )
    .is_ok());
  }

  #[test]
  fn allows_anything_when_the_real_home_is_unknown_or_empty() {
    assert!(reject_real_home(Path::new("/tmp/x"), None).is_ok());
    assert!(reject_real_home(Path::new("/tmp/x"), Some(Path::new(""))).is_ok());
  }

  use super::*;

  #[test]
  fn cli_override_wins_over_everything() {
    let home = resolve_scratch_home(
      Some("/Users/dev/benchmark-home"),
      Some("/tmp/env-home".to_string()),
      Path::new("/tmp"),
      123,
      456,
    );
    assert_eq!(home, PathBuf::from("/Users/dev/benchmark-home"));
  }

  #[test]
  fn env_override_wins_when_no_cli_override() {
    let home = resolve_scratch_home(
      None,
      Some("/tmp/env-home".to_string()),
      Path::new("/tmp"),
      123,
      456,
    );
    assert_eq!(home, PathBuf::from("/tmp/env-home"));
  }

  #[test]
  fn falls_back_to_a_freshly_named_directory_under_temp_dir() {
    let home = resolve_scratch_home(None, None, Path::new("/tmp"), 123, 456);
    assert_eq!(home, PathBuf::from("/tmp/resource-benchmark-home-123-456"));
  }

  #[test]
  fn directory_names_differ_by_pid_and_disambiguator() {
    let a = scratch_home_directory_name(1, 1);
    let b = scratch_home_directory_name(1, 2);
    let c = scratch_home_directory_name(2, 1);
    assert_ne!(a, b);
    assert_ne!(a, c);
  }

  #[test]
  fn never_falls_back_to_a_literal_home_env_reading() {
    // The whole point of this module: there is no code path here that
    // reads `$HOME`. This test exists as a documentation anchor -- if a
    // future edit adds one, it belongs in `main.rs`'s explicit
    // identity-lookup helper, never here.
    let home = resolve_scratch_home(None, None, Path::new("/tmp"), 1, 1);
    assert!(!home.to_string_lossy().is_empty());
  }
}
