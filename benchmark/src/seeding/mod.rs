//! Session seeders (spec FR-9): each subject gets N live agent sessions
//! started the same reproducible way every run. `AgentCliPin` and
//! `SeededRepo` are resolved once per run and shared by every seeder, so
//! "same repository and same agent CLI invocation across every subject" is
//! structural rather than merely checked (design §5.6).

pub mod codenomad;
pub mod collaborator;
pub mod diri;
pub mod termtree;

use crate::process_tree::ProcessRecord;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentCliPin {
  pub name: String,
  pub version: String,
  pub executable_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeededRepo {
  pub url: String,
  pub commit: String,
  pub local_path: String,
}

#[derive(Debug, PartialEq)]
pub struct SeedPlan {
  pub method: String,
}

#[derive(Debug)]
pub struct SeedError(pub String);

impl std::fmt::Display for SeedError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}
impl std::error::Error for SeedError {}

pub trait SessionSeeder {
  /// Write whatever on-disk/CLI state makes the subject start with `n`
  /// sessions.
  fn seed(
    &self,
    n: u32,
    repo: &SeededRepo,
    agent: &AgentCliPin,
  ) -> Result<SeedPlan, SeedError>;
  /// Undo `seed`, restoring any state that existed before it.
  fn restore(&self) -> Result<(), SeedError>;
}

/// Dispatches `subject_id` to its own seeder's [`SessionSeeder::seed`]
/// (design §5.6). The one place that knows the `subject_id` -> seeder
/// mapping, shared by `main.rs`'s `seed` subcommand and `run.rs`'s live
/// orchestration so the two never drift apart.
pub fn seed_subject(
  home: &Path,
  subject_id: &str,
  n: u32,
  repo: &SeededRepo,
  agent: &AgentCliPin,
) -> Result<SeedPlan, SeedError> {
  match subject_id {
    "termtree" => {
      termtree::TermTreeSeeder::production(home).seed(n, repo, agent)
    }
    "collaborator" => {
      collaborator::CollaboratorSeeder::production(home).seed(n, repo, agent)
    }
    "codenomad-electron" | "codenomad-tauri" => {
      codenomad::CodeNomadSeeder::production(home).seed(n, repo, agent)
    }
    "diri" => diri::DiriSeeder::production(home).seed(n, repo, agent),
    other => Err(SeedError(format!("unknown subject: {other}"))),
  }
}

/// Dispatches `subject_id` to its own seeder's [`SessionSeeder::restore`]
/// -- the undo side of [`seed_subject`], shared the same way.
pub fn restore_subject(home: &Path, subject_id: &str) -> Result<(), SeedError> {
  match subject_id {
    "termtree" => termtree::TermTreeSeeder::production(home).restore(),
    "collaborator" => {
      collaborator::CollaboratorSeeder::production(home).restore()
    }
    "codenomad-electron" | "codenomad-tauri" => {
      codenomad::CodeNomadSeeder::production(home).restore()
    }
    "diri" => diri::DiriSeeder::production(home).restore(),
    other => Err(SeedError(format!("unknown subject: {other}"))),
  }
}

/// Generic session-readiness check (NFR-3: checked the same way for every
/// subject, never per-subject). A subject is considered seeded once its
/// process tree contains `n` processes whose executable path is the login
/// shell **and** `n` whose path is the pinned agent CLI.
pub fn count_ready_sessions(
  tree_records: &[ProcessRecord],
  root_pid: u32,
  login_shell_path: &str,
  agent_cli_executable_path: &str,
) -> (u32, u32) {
  let descendants = crate::process_tree::descendants_of(tree_records, root_pid);
  let mut shells = 0u32;
  let mut agents = 0u32;
  for pid in descendants {
    let Some(record) = crate::process_tree::record_by_pid(tree_records, pid)
    else {
      continue;
    };
    match record.executable_path.as_deref() {
      Some(path) if path == login_shell_path => shells += 1,
      Some(path) if path == agent_cli_executable_path => agents += 1,
      _ => {}
    }
  }
  (shells, agents)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn record(pid: u32, ppid: u32, exe: &str) -> ProcessRecord {
    ProcessRecord {
      pid,
      ppid,
      name: "x".into(),
      executable_path: Some(exe.into()),
      rss_bytes: 0,
    }
  }

  #[test]
  fn seed_subject_rejects_an_unknown_subject_id() {
    let repo = SeededRepo {
      url: "https://example.invalid/repo".into(),
      commit: "abc".into(),
      local_path: "/tmp/repo".into(),
    };
    let agent = AgentCliPin {
      name: "claude".into(),
      version: "1.0.0".into(),
      executable_path: "/usr/local/bin/claude".into(),
    };
    let error =
      seed_subject(Path::new("/tmp"), "not-a-subject", 1, &repo, &agent)
        .unwrap_err();
    assert!(error.0.contains("not-a-subject"));
  }

  #[test]
  fn restore_subject_rejects_an_unknown_subject_id() {
    let error =
      restore_subject(Path::new("/tmp"), "not-a-subject").unwrap_err();
    assert!(error.0.contains("not-a-subject"));
  }

  #[test]
  fn counts_shells_and_agents_among_descendants_only() {
    let records = vec![
      record(1, 0, "/Applications/TermTree.app/termtree"),
      record(2, 1, "/bin/zsh"),
      record(3, 2, "/Users/dev/.local/bin/claude"),
      record(4, 1, "/bin/zsh"),
      // Not a descendant of root (1): must not be counted.
      record(5, 99, "/bin/zsh"),
    ];
    let (shells, agents) = count_ready_sessions(
      &records,
      1,
      "/bin/zsh",
      "/Users/dev/.local/bin/claude",
    );
    assert_eq!(shells, 2);
    assert_eq!(agents, 1);
  }
}
