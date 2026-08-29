//! diri's session seeder (optional subject, spec FR-1, design §5.6.4).
//!
//! diri's `dirijord-rs` daemon owns the PTYs and its sessions **outlive the
//! app** -- unlike every other subject here. The seeder therefore
//! terminates `dirijord-rs` before seeding *and* the orchestrator's
//! teardown terminates it again; without both halves its memory would be
//! attributed to nobody and its sessions would leak into the next
//! repetition. `attribution.rs` includes it via a `CompanionProcess`
//! (`subject.rs`'s registry entry for `diri`).
//!
//! **This seed format has never been checked against a real diri install**
//! (spec item 4) -- the `[[session]]`-table TOML shape below is this
//! project's best guess at what `~/.diri/sessions.toml` needs to contain,
//! not a verified contract. `subject.rs`'s `seed_format_verified: false`
//! on the `diri` entry reflects this: every N-session/sustained-use
//! sample for diri reports `invalidReason: "seed-format-unverified"` until
//! someone confirms this format against a real install and flips that
//! flag.

use super::{AgentCliPin, SeedError, SeedPlan, SeededRepo, SessionSeeder};
use crate::exec::run_capture;
use std::fs;
use std::path::PathBuf;

pub const DIRIJORD_EXECUTABLE_NAME: &str = "dirijord-rs";

pub struct DiriSeeder {
  pub config_path: PathBuf,
}

impl DiriSeeder {
  pub fn production(home: &std::path::Path) -> Self {
    Self {
      config_path: home.join(".diri").join("sessions.toml"),
    }
  }
}

/// Kills every resident `dirijord-rs` process. Called before seeding (so a
/// prior run's sessions cannot leak into this one) and again during
/// teardown (design §5.6.4). Best-effort: a daemon that is already gone is
/// not an error.
pub fn kill_dirijord_daemon() {
  let _ = run_capture("/usr/bin/pkill", &["-x", DIRIJORD_EXECUTABLE_NAME]);
}

impl SessionSeeder for DiriSeeder {
  fn seed(
    &self,
    n: u32,
    repo: &SeededRepo,
    agent: &AgentCliPin,
  ) -> Result<SeedPlan, SeedError> {
    kill_dirijord_daemon();
    if let Some(parent) = self.config_path.parent() {
      fs::create_dir_all(parent).map_err(|e| SeedError(e.to_string()))?;
    }
    let mut sessions = String::new();
    for i in 0..n {
      sessions.push_str(&format!(
        "[[session]]\nid = \"resource-benchmark-session-{i}\"\ncwd = \"{}\"\ncommand = \"{}\"\n\n",
        repo.local_path, agent.executable_path
      ));
    }
    fs::write(&self.config_path, sessions)
      .map_err(|e| SeedError(e.to_string()))?;
    Ok(SeedPlan {
      method: "~/.diri/sessions.toml pre-write".to_string(),
    })
  }

  fn restore(&self) -> Result<(), SeedError> {
    kill_dirijord_daemon();
    if self.config_path.exists() {
      fs::remove_file(&self.config_path)
        .map_err(|e| SeedError(e.to_string()))?;
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn repo() -> SeededRepo {
    SeededRepo {
      url: "https://example.com/repo.git".into(),
      commit: "abc123".into(),
      local_path: "/Users/Shared/benchmark-repo".into(),
    }
  }

  fn agent() -> AgentCliPin {
    AgentCliPin {
      name: "claude".into(),
      version: "1.0.0".into(),
      executable_path: "/Users/dev/.local/bin/claude".into(),
    }
  }

  #[test]
  fn seed_writes_n_sessions_and_restore_removes_the_file() {
    let dir = std::env::temp_dir().join(format!(
      "resource-benchmark-diri-seeder-test-{}",
      std::process::id()
    ));
    let seeder = DiriSeeder {
      config_path: dir.join("sessions.toml"),
    };
    seeder.seed(3, &repo(), &agent()).unwrap();
    let text = fs::read_to_string(&seeder.config_path).unwrap();
    assert_eq!(text.matches("[[session]]").count(), 3);
    seeder.restore().unwrap();
    assert!(!seeder.config_path.exists());
    let _ = fs::remove_dir_all(&dir);
  }
}
