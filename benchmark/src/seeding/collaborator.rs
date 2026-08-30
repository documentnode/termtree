//! Collaborator's session seeder: writes N terminal tiles into
//! `~/.collaborator/`'s canvas JSON (spec FR-9, design §5.6.2).
//!
//! Collaborator spawns a vendored `tmux` and a `node-pty` sidecar to own
//! its PTYs; both are `Orchestrator` under `attribution.rs`'s one
//! partition rule (design §5.2.3) -- Collaborator's own implementation
//! choice for a job TermTree does in-process with `portable-pty`, named
//! here per FR-9's "documented in the harness source, next to that
//! subject's adapter".
//!
//! **This seed format has never been checked against a real Collaborator
//! install** (spec item 4) -- the `{"tiles": [{"id", "cwd", "command"}]}`
//! shape below is this project's best guess at what
//! `~/.collaborator/canvas.json` needs to contain, not a verified
//! contract. `subject.rs`'s `seed_format_verified: false` on the
//! `collaborator` entry reflects this: every N-session/sustained-use
//! sample for this subject reports `invalidReason:
//! "seed-format-unverified"` until someone confirms this format against a
//! real install and flips that flag.

use super::{AgentCliPin, SeedError, SeedPlan, SeededRepo, SessionSeeder};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

const CANVAS_FILENAME: &str = "canvas.json";
const BACKUP_SUFFIX: &str = ".before-resource-benchmark.json";

pub struct CollaboratorSeeder {
  pub state_directory: PathBuf,
}

impl CollaboratorSeeder {
  pub fn production(home: &Path) -> Self {
    Self {
      state_directory: home.join(".collaborator"),
    }
  }

  fn canvas_path(&self) -> PathBuf {
    self.state_directory.join(CANVAS_FILENAME)
  }

  fn backup_path(&self) -> PathBuf {
    self
      .state_directory
      .join(format!("{CANVAS_FILENAME}{BACKUP_SUFFIX}"))
  }
}

impl SessionSeeder for CollaboratorSeeder {
  fn seed(
    &self,
    n: u32,
    repo: &SeededRepo,
    agent: &AgentCliPin,
  ) -> Result<SeedPlan, SeedError> {
    fs::create_dir_all(&self.state_directory)
      .map_err(|e| SeedError(e.to_string()))?;
    let canvas = self.canvas_path();
    let backup = self.backup_path();
    if backup.exists() {
      return Err(SeedError(format!(
        "Backup already exists at {}. Run `resource-benchmark restore` \
         before seeding again.",
        backup.display()
      )));
    }
    if canvas.exists() {
      fs::copy(&canvas, &backup).map_err(|e| SeedError(e.to_string()))?;
    }
    let tiles: Vec<_> = (0..n)
      .map(|i| {
        json!({
          "id": format!("resource-benchmark-tile-{i}"),
          "cwd": repo.local_path,
          "command": agent.executable_path,
        })
      })
      .collect();
    let text = serde_json::to_string_pretty(&json!({ "tiles": tiles }))
      .expect("tile serialization never fails");
    fs::write(&canvas, text).map_err(|e| SeedError(e.to_string()))?;
    Ok(SeedPlan {
      method: "~/.collaborator canvas.json pre-write".to_string(),
    })
  }

  fn restore(&self) -> Result<(), SeedError> {
    let backup = self.backup_path();
    if !backup.exists() {
      return Ok(());
    }
    fs::rename(&backup, self.canvas_path())
      .map_err(|e| SeedError(e.to_string()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::process;

  fn temp_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "resource-benchmark-collaborator-seeder-test-{}-{}",
      process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
  }

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
  fn seed_then_restore_round_trips_prior_canvas() {
    let home = temp_home();
    let seeder = CollaboratorSeeder::production(&home);
    fs::create_dir_all(&seeder.state_directory).unwrap();
    fs::write(seeder.canvas_path(), "{\"tiles\":[{\"id\":\"real\"}]}").unwrap();

    seeder.seed(5, &repo(), &agent()).unwrap();
    let seeded = fs::read_to_string(seeder.canvas_path()).unwrap();
    assert!(seeded.contains("resource-benchmark-tile-0"));

    seeder.restore().unwrap();
    let restored = fs::read_to_string(seeder.canvas_path()).unwrap();
    assert!(restored.contains("\"real\""));

    fs::remove_dir_all(&home).unwrap();
  }
}
