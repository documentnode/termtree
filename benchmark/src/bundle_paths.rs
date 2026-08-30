//! Per-subject bundle-path overrides (spec item 5): `SubjectSpec::bundle_path`
//! is a compile-time `/Applications/*.app` constant with no override at
//! all, which leaves a third party whose installs live elsewhere simply
//! stuck. [`resolve`] is the one place every live code path reads a
//! subject's actual bundle path, so a CLI or environment override can
//! never drift between `doctor`, the live sweep, and cold-start launch.

use crate::subject::SubjectSpec;
use std::collections::HashMap;

pub type BundlePathOverrides = HashMap<String, String>;

/// `RESOURCE_BENCHMARK_BUNDLE_PATH_<SUBJECT_ID>`, following this crate's
/// existing `RESOURCE_BENCHMARK_*` override convention -- e.g.
/// `codenomad-electron` reads `RESOURCE_BENCHMARK_BUNDLE_PATH_CODENOMAD_ELECTRON`.
pub fn env_var_name(subject_id: &str) -> String {
  format!(
    "RESOURCE_BENCHMARK_BUNDLE_PATH_{}",
    subject_id.to_uppercase().replace('-', "_")
  )
}

/// `subject.id`'s override if one was given (CLI wins over environment),
/// else the registry's compiled-in default.
pub fn resolve<'a>(
  subject: &'a SubjectSpec,
  overrides: &'a BundlePathOverrides,
) -> &'a str {
  overrides
    .get(subject.id)
    .map(String::as_str)
    .unwrap_or(subject.bundle_path)
}

/// Merges environment-sourced overrides with CLI-sourced ones, CLI winning
/// on a collision -- the same precedence [`crate::scratch_home`] uses for
/// `--home` vs `RESOURCE_BENCHMARK_HOME`.
pub fn merge_cli_over_env(
  env_overrides: BundlePathOverrides,
  cli_overrides: BundlePathOverrides,
) -> BundlePathOverrides {
  let mut merged = env_overrides;
  merged.extend(cli_overrides);
  merged
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::subject::find;

  #[test]
  fn env_var_name_uppercases_and_replaces_hyphens() {
    assert_eq!(
      env_var_name("codenomad-electron"),
      "RESOURCE_BENCHMARK_BUNDLE_PATH_CODENOMAD_ELECTRON"
    );
    assert_eq!(
      env_var_name("termtree"),
      "RESOURCE_BENCHMARK_BUNDLE_PATH_TERMTREE"
    );
  }

  #[test]
  fn override_wins_over_the_registry_default() {
    let subject = find("termtree").unwrap();
    let mut overrides = BundlePathOverrides::new();
    overrides.insert(
      "termtree".to_string(),
      "/Users/dev/Apps/TermTree.app".to_string(),
    );
    assert_eq!(resolve(subject, &overrides), "/Users/dev/Apps/TermTree.app");
  }

  #[test]
  fn no_override_falls_back_to_the_registry_default() {
    let subject = find("termtree").unwrap();
    let overrides = BundlePathOverrides::new();
    assert_eq!(resolve(subject, &overrides), subject.bundle_path);
  }

  #[test]
  fn an_unrelated_subjects_override_does_not_leak() {
    let subject = find("collaborator").unwrap();
    let mut overrides = BundlePathOverrides::new();
    overrides.insert("termtree".to_string(), "/tmp/Fake.app".to_string());
    assert_eq!(resolve(subject, &overrides), subject.bundle_path);
  }

  #[test]
  fn cli_override_wins_over_an_env_override_for_the_same_subject() {
    let mut env_overrides = BundlePathOverrides::new();
    env_overrides
      .insert("termtree".to_string(), "/env/TermTree.app".to_string());
    let mut cli_overrides = BundlePathOverrides::new();
    cli_overrides
      .insert("termtree".to_string(), "/cli/TermTree.app".to_string());
    let merged = merge_cli_over_env(env_overrides, cli_overrides);
    assert_eq!(
      merged.get("termtree").map(String::as_str),
      Some("/cli/TermTree.app")
    );
  }

  #[test]
  fn merge_keeps_env_only_entries() {
    let mut env_overrides = BundlePathOverrides::new();
    env_overrides.insert("diri".to_string(), "/env/diri.app".to_string());
    let merged = merge_cli_over_env(env_overrides, BundlePathOverrides::new());
    assert_eq!(
      merged.get("diri").map(String::as_str),
      Some("/env/diri.app")
    );
  }
}
