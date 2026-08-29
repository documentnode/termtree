//! The subject registry (spec FR-1): a fixed `const` table, not
//! configuration a re-runner can quietly change, so it *is* the published
//! subject set.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFamily {
  WebKitTauri,
  ChromiumElectron,
  GpuiNative,
}

impl RuntimeFamily {
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::WebKitTauri => "webkit-tauri",
      Self::ChromiumElectron => "chromium-electron",
      Self::GpuiNative => "gpui-native",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeederId {
  TermTreeStateJson,
  Collaborator,
  CodeNomad,
  Diri,
}

#[derive(Debug, Clone, Copy)]
pub struct CompanionProcess {
  pub executable_name: &'static str,
  pub kill_between_repetitions: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SubjectSpec {
  pub id: &'static str,
  pub display_name: &'static str,
  pub runtime_family: RuntimeFamily,
  pub bundle_identifier: &'static str,
  pub bundle_path: &'static str,
  pub launch_services_name: &'static str,
  pub helper_bundle_ids: &'static [&'static str],
  pub main_executable_name: &'static str,
  pub companion_processes: &'static [CompanionProcess],
  pub seeder: SeederId,
  pub optional: bool,
  pub expected_version: &'static str,
  /// Whether this subject's seeder format (spec item 4) has been checked
  /// against a real install of the app -- not just unit-tested against the
  /// harness's own assumptions about the format. `true` only for TermTree,
  /// whose seeded `state.json` shape is pinned against the app's own
  /// `AppStateController` source (`seeding/termtree.rs`'s tests). The other
  /// three seeders' formats have never been run against a real install;
  /// see each `seeding/*.rs` module doc for what is unverified and why.
  pub seed_format_verified: bool,
}

/// The webkit-tauri family's helper-process bundle IDs, shared by TermTree
/// (the only WebKitTauri subject in this registry).
const WEBKIT_HELPER_BUNDLE_IDS: &[&str] = &[
  "com.apple.WebKit.Networking",
  "com.apple.WebKit.GPU",
  "com.apple.WebKit.WebContent",
];

pub const SUBJECTS: &[SubjectSpec] = &[
  SubjectSpec {
    id: "termtree",
    display_name: "TermTree",
    runtime_family: RuntimeFamily::WebKitTauri,
    bundle_identifier: "com.termtree.desktop",
    bundle_path: "/Applications/TermTree.app",
    launch_services_name: "TermTree",
    helper_bundle_ids: WEBKIT_HELPER_BUNDLE_IDS,
    main_executable_name: "termtree",
    companion_processes: &[],
    seeder: SeederId::TermTreeStateJson,
    optional: false,
    expected_version: "1.0.0",
    seed_format_verified: true,
  },
  SubjectSpec {
    id: "codenomad-electron",
    display_name: "CodeNomad (Electron)",
    runtime_family: RuntimeFamily::ChromiumElectron,
    bundle_identifier: "com.codenomad.desktop.electron",
    bundle_path: "/Applications/CodeNomad.app",
    launch_services_name: "CodeNomad",
    helper_bundle_ids: &[],
    main_executable_name: "CodeNomad",
    companion_processes: &[CompanionProcess {
      executable_name: "codenomad-server",
      kill_between_repetitions: true,
    }],
    seeder: SeederId::CodeNomad,
    optional: false,
    expected_version: "0.18.0",
    seed_format_verified: false,
  },
  SubjectSpec {
    id: "codenomad-tauri",
    display_name: "CodeNomad (Tauri)",
    runtime_family: RuntimeFamily::WebKitTauri,
    bundle_identifier: "com.codenomad.desktop.tauri",
    bundle_path: "/Applications/CodeNomad Tauri.app",
    launch_services_name: "CodeNomad Tauri",
    helper_bundle_ids: WEBKIT_HELPER_BUNDLE_IDS,
    main_executable_name: "codenomad-tauri",
    companion_processes: &[CompanionProcess {
      executable_name: "codenomad-server",
      kill_between_repetitions: true,
    }],
    seeder: SeederId::CodeNomad,
    optional: false,
    expected_version: "0.18.0",
    seed_format_verified: false,
  },
  SubjectSpec {
    id: "collaborator",
    display_name: "Collaborator",
    runtime_family: RuntimeFamily::ChromiumElectron,
    bundle_identifier: "com.collaborator.desktop",
    bundle_path: "/Applications/Collaborator.app",
    launch_services_name: "Collaborator",
    helper_bundle_ids: &[],
    main_executable_name: "Collaborator",
    companion_processes: &[],
    seeder: SeederId::Collaborator,
    optional: false,
    expected_version: "0.8.4",
    seed_format_verified: false,
  },
  SubjectSpec {
    id: "diri",
    display_name: "diri",
    runtime_family: RuntimeFamily::GpuiNative,
    bundle_identifier: "com.diri.desktop",
    bundle_path: "/Applications/diri.app",
    launch_services_name: "diri",
    helper_bundle_ids: &[],
    main_executable_name: "diri",
    companion_processes: &[CompanionProcess {
      executable_name: "dirijord-rs",
      kill_between_repetitions: true,
    }],
    seeder: SeederId::Diri,
    optional: true,
    expected_version: "0.5.1",
    seed_format_verified: false,
  },
];

/// Excluded subjects and hold-backs (spec FR-1, §8), kept as data next to
/// the registry so `render.rs` can copy the reasons verbatim into the
/// published methodology rather than let them drift from prose.
pub struct Exclusion {
  pub name: &'static str,
  pub reason: &'static str,
}

pub const EXCLUSIONS: &[Exclusion] = &[
  Exclusion {
    name: "Conductor",
    reason: "Terms of Service prohibit use for competitive analysis or a \
             competing product (legal).",
  },
  Exclusion {
    name: "Crystal",
    reason: "Project is dead, renamed Nimbalyst.",
  },
  Exclusion {
    name: "Constellagent",
    reason: "Zero releases, zero tags, no licence file.",
  },
  Exclusion {
    name: "Vibe Kanban",
    reason: "Dual web/desktop mode makes the measurement target ambiguous.",
  },
  Exclusion {
    name: "Claude Squad",
    reason: "Go TUI with no GUI, no webview, no window to measure \
             (category error).",
  },
];

pub const HOLD_BACKS: &[Exclusion] = &[
  Exclusion {
    name: "Nimbalyst",
    reason: "Documented hold-back; may be promoted to a measured subject \
             later.",
  },
  Exclusion {
    name: "Maestri",
    reason: "Documented hold-back; EULA's anti-reverse-engineering clause \
             restricts it to black-box OS-level measurement if ever added.",
  },
];

pub fn find(id: &str) -> Option<&'static SubjectSpec> {
  SUBJECTS.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn registry_includes_the_minimum_required_subjects() {
    for id in [
      "termtree",
      "codenomad-electron",
      "codenomad-tauri",
      "collaborator",
    ] {
      assert!(find(id).is_some(), "missing required subject {id}");
      assert!(!find(id).unwrap().optional);
    }
  }

  #[test]
  fn diri_is_the_only_optional_subject() {
    let optional: Vec<_> = SUBJECTS
      .iter()
      .filter(|s| s.optional)
      .map(|s| s.id)
      .collect();
    assert_eq!(optional, vec!["diri"]);
  }

  #[test]
  fn every_subject_has_a_pinned_non_empty_version() {
    for subject in SUBJECTS {
      assert!(!subject.expected_version.is_empty(), "{}", subject.id);
    }
  }

  #[test]
  fn codenomad_electron_and_tauri_share_the_pinned_version() {
    let electron = find("codenomad-electron").unwrap();
    let tauri = find("codenomad-tauri").unwrap();
    assert_eq!(electron.expected_version, tauri.expected_version);
  }

  #[test]
  fn only_termtrees_seed_format_is_verified_against_a_real_install() {
    let verified: Vec<_> = SUBJECTS
      .iter()
      .filter(|s| s.seed_format_verified)
      .map(|s| s.id)
      .collect();
    assert_eq!(verified, vec!["termtree"]);
  }

  #[test]
  fn exclusions_and_hold_backs_are_named_with_reasons() {
    assert_eq!(EXCLUSIONS.len(), 5);
    assert_eq!(HOLD_BACKS.len(), 2);
    for e in EXCLUSIONS.iter().chain(HOLD_BACKS) {
      assert!(!e.reason.is_empty());
    }
  }
}
