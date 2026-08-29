//! CodeNomad's session seeder, serving **both** the Electron and Tauri
//! builds -- they are the same monorepo with the same seeding contract,
//! which is the whole reason the pair isolates runtime as the only
//! variable (spec FR-1, design §5.6.3).
//!
//! CodeNomad is client-server: its local server process is registered as a
//! `CompanionProcess` in `subject.rs` and attributed via
//! `attribution::attribute_companion_process`, never left uncounted (spec
//! FR-2).
//!
//! **This seed format has never been checked against a real CodeNomad
//! install** (spec item 4) -- `SEED_METHOD` names the pinned v0.18.0's
//! *documented* seeding mechanism, but the env var name, the config-file
//! shape, and the pinned version's actual v0.18.0 behavior are all
//! unverified. `subject.rs` sets `seed_format_verified: false` on both
//! `codenomad-electron` and `codenomad-tauri` (they share this seeder), so
//! every N-session/sustained-use sample for either reports
//! `invalidReason: "seed-format-unverified"` until someone confirms this
//! against a real install and flips that flag.

use super::{AgentCliPin, SeedError, SeedPlan, SeededRepo, SessionSeeder};
use std::env;
use std::path::PathBuf;

/// The pinned v0.18.0's documented seeding mechanism. Recorded so the
/// method actually used ships in provenance (`SeedPlan.method`) rather than
/// being assumed.
const SEED_METHOD: &str = "CODENOMAD_SEED_SESSIONS env var + config file";

pub struct CodeNomadSeeder {
  pub config_path: PathBuf,
}

impl CodeNomadSeeder {
  pub fn production(home: &std::path::Path) -> Self {
    Self {
      config_path: home.join(".codenomad").join("sessions.json"),
    }
  }
}

impl SessionSeeder for CodeNomadSeeder {
  fn seed(
    &self,
    n: u32,
    repo: &SeededRepo,
    agent: &AgentCliPin,
  ) -> Result<SeedPlan, SeedError> {
    // env::set_var is process-global and this harness never runs two
    // subjects concurrently, so a plain env var plus a config file (the
    // documented v0.18.0 mechanism) is sufficient -- no lock/backup
    // discipline is needed because CodeNomad's config file is
    // benchmark-owned, not a pre-existing user file (unlike TermTree's
    // production state.json, §5.6.1).
    let sessions: Vec<_> = (0..n)
      .map(|i| {
        serde_json::json!({
          "id": format!("resource-benchmark-session-{i}"),
          "cwd": repo.local_path,
          "command": agent.executable_path,
        })
      })
      .collect();
    if let Some(parent) = self.config_path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| SeedError(e.to_string()))?;
    }
    std::fs::write(
      &self.config_path,
      serde_json::to_string_pretty(&sessions)
        .expect("session serialization never fails"),
    )
    .map_err(|e| SeedError(e.to_string()))?;
    unsafe {
      env::set_var("CODENOMAD_SEED_SESSIONS", self.config_path.as_os_str());
    }
    Ok(SeedPlan {
      method: SEED_METHOD.to_string(),
    })
  }

  fn restore(&self) -> Result<(), SeedError> {
    unsafe {
      env::remove_var("CODENOMAD_SEED_SESSIONS");
    }
    if self.config_path.exists() {
      std::fs::remove_file(&self.config_path)
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
  fn seed_writes_the_config_file_and_restore_removes_it() {
    let dir = std::env::temp_dir().join(format!(
      "resource-benchmark-codenomad-seeder-test-{}",
      std::process::id()
    ));
    let seeder = CodeNomadSeeder {
      config_path: dir.join("sessions.json"),
    };
    let plan = seeder.seed(4, &repo(), &agent()).unwrap();
    assert_eq!(plan.method, SEED_METHOD);
    assert!(seeder.config_path.exists());
    seeder.restore().unwrap();
    assert!(!seeder.config_path.exists());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
