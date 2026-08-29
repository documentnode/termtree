//! TermTree's session seeder: writes `state.json` before launch (spec
//! FR-9, design §5.6.1) under the harness's disposable scratch home (spec
//! item 1) -- never a real TermTree profile.
//!
//! **This is the single highest-consequence module in the harness.** A
//! public build of this harness runs on a stranger's machine, which may
//! have a real TermTree install with real user data at
//! `~/Library/Application Support/DocumentNode/TermTree/state.json`. This
//! seeder must never be able to reach that path.
//!
//! The original (private-repo) version of this module asserted the
//! opposite of today's contract: it *required* a production path and
//! *refused* a `TermTreeDev` one, because at the time the harness always
//! launched subjects against the developer's real `$HOME` and the risk
//! being guarded against was colliding with `scripts/src/bin/
//! sample_mindmap.rs`'s development-only seeder. Now that every subject is
//! launched with `HOME` pointed at a fresh scratch directory (`open -n
//! --env HOME=<scratch>`, `cold_start.rs`), that risk is structurally gone
//! and the actual risk is the opposite one: this seeder reaching a real
//! profile on a third party's machine. [`expected_scratch_state_path`] is
//! the inversion -- it requires the target be under the harness's own
//! scratch root and refuses anything that is not, which now includes a
//! real production profile. The `DocumentNode/TermTree` suffix check is
//! kept as a secondary, defense-in-depth condition (still refuses a
//! `TermTreeDev`-named directory even if one somehow lived under the
//! scratch root), but scratch-root containment is the primary guard.

use super::{AgentCliPin, SeedError, SeedPlan, SeededRepo, SessionSeeder};
use serde::Serialize;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_FILENAME: &str = "state.json";
const BACKUP_FILENAME: &str = "state.json.before-resource-benchmark.json";
const LOCK_DIRECTORY: &str = ".resource-benchmark-seed.lock";
const RECOVERY_PREFIX: &str = "state.json.benchmark-recovery-";
static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Node {
  id: String,
  label: String,
  status: String,
  session_id: Option<()>,
  previous_session_id: Option<()>,
  command: Option<String>,
  cwd: Option<String>,
  exit_code: Option<i32>,
  claude_session_id: Option<()>,
  children: Vec<Node>,
  collapsed: bool,
}

pub struct TermTreeSeeder {
  pub state_directory: PathBuf,
  /// The harness's disposable scratch home (spec item 1). `state_directory`
  /// must live under this root -- [`expected_scratch_state_path`] refuses
  /// to seed anywhere else, including a real production profile.
  pub scratch_root: PathBuf,
}

impl TermTreeSeeder {
  /// `<scratch_home>/Library/Application Support/DocumentNode/TermTree` --
  /// the disposable-scratch-home equivalent of TermTree's real
  /// release-build state path. macOS only, matching this harness's scope
  /// (spec §8: Windows/Linux hosts are out of scope for v1).
  pub fn production(scratch_home: &Path) -> Self {
    Self {
      state_directory: crate::log_marks::app_data_dir(
        &scratch_home.join("Library").join("Application Support"),
      ),
      scratch_root: scratch_home.to_path_buf(),
    }
  }
}

/// Asserts `state_directory/state.json` is both under `scratch_root` and
/// ends in `DocumentNode/TermTree/` -- the **inversion** of this module's
/// original guard (module doc): scratch-root containment is now the
/// primary, affirmative condition, and the suffix check is kept only as
/// defense in depth. A public runner may have a real TermTree profile on
/// this machine; this seeder must never be able to reach it, so everything
/// outside the scratch root is refused, which now includes that real
/// profile.
fn expected_scratch_state_path(
  scratch_root: &Path,
  state_directory: &Path,
) -> Result<PathBuf, SeedError> {
  let path = state_directory.join(STATE_FILENAME);
  let directory = path.parent();
  let application_directory = directory.and_then(Path::parent);
  let under_scratch_root = path.starts_with(scratch_root);
  if !under_scratch_root
    || path.file_name() != Some(OsStr::new(STATE_FILENAME))
    || directory.and_then(Path::file_name) != Some(OsStr::new("TermTree"))
    || application_directory.and_then(Path::file_name)
      != Some(OsStr::new("DocumentNode"))
  {
    return Err(SeedError(format!(
      "Refusing {}. The resource-benchmark seeder only writes state.json \
       under its own disposable scratch home ({}), ending in \
       DocumentNode/TermTree/state.json -- never a real TermTree profile.",
      path.display(),
      scratch_root.display()
    )));
  }
  Ok(path)
}

struct Lock {
  path: PathBuf,
}
impl Lock {
  fn acquire(directory: &Path) -> Result<Self, SeedError> {
    let path = directory.join(LOCK_DIRECTORY);
    fs::create_dir(&path).map_err(|error| {
      if error.kind() == io::ErrorKind::AlreadyExists {
        SeedError(format!(
          "Another resource-benchmark seed/restore is in progress: {}",
          path.display()
        ))
      } else {
        SeedError(error.to_string())
      }
    })?;
    Ok(Self { path })
  }
}
impl Drop for Lock {
  fn drop(&mut self) {
    let _ = fs::remove_dir(&self.path);
  }
}

fn unique_suffix() -> String {
  format!(
    "{}.{}.{}",
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis(),
    process::id(),
    FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
  )
}

fn temporary_path(target: &Path) -> PathBuf {
  target.with_file_name(format!(
    ".{}.{suffix}.tmp",
    target
      .file_name()
      .unwrap_or_else(|| OsStr::new("state"))
      .to_string_lossy(),
    suffix = unique_suffix()
  ))
}

fn atomic_write(target: &Path, text: &str) -> Result<(), SeedError> {
  let temporary = temporary_path(target);
  let result = (|| -> Result<(), SeedError> {
    let mut file = OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temporary)
      .map_err(|e| SeedError(e.to_string()))?;
    file
      .write_all(text.as_bytes())
      .and_then(|()| file.sync_all())
      .map_err(|e| SeedError(e.to_string()))?;
    drop(file);
    fs::rename(&temporary, target).map_err(|e| SeedError(e.to_string()))
  })();
  if result.is_err() {
    let _ = fs::remove_file(&temporary);
  }
  result
}

fn regular_file_or_missing(
  path: &Path,
  description: &str,
) -> Result<bool, SeedError> {
  match fs::symlink_metadata(path) {
    Ok(metadata)
      if metadata.file_type().is_file()
        && !metadata.file_type().is_symlink() =>
    {
      Ok(true)
    }
    Ok(_) => Err(SeedError(format!(
      "{description} must be a regular file: {}",
      path.display()
    ))),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(SeedError(error.to_string())),
  }
}

fn write_recovery(directory: &Path, text: &str) -> Result<PathBuf, SeedError> {
  for _ in 0..1000 {
    let path =
      directory.join(format!("{RECOVERY_PREFIX}{}.json", unique_suffix()));
    match OpenOptions::new().write(true).create_new(true).open(&path) {
      Ok(mut file) => {
        file
          .write_all(text.as_bytes())
          .and_then(|()| file.sync_all())
          .map_err(|e| SeedError(e.to_string()))?;
        return Ok(path);
      }
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
      Err(error) => return Err(SeedError(error.to_string())),
    }
  }
  Err(SeedError(
    "Could not allocate a unique benchmark recovery filename.".into(),
  ))
}

fn node(
  id: &str,
  status: &str,
  command: Option<String>,
  cwd: Option<String>,
) -> Node {
  Node {
    id: id.to_string(),
    label: id.to_string(),
    status: status.to_string(),
    session_id: None,
    previous_session_id: None,
    command,
    cwd,
    exit_code: None,
    claude_session_id: None,
    children: Vec::new(),
    collapsed: false,
  }
}

/// The mind-map root. Its status is deliberately `none` so
/// `restoreSessions` does not queue the root itself for relaunch -- only
/// its `n` children are sessions
/// (`frontend/src/store/AppStateController.ts:855-861`).
const ROOT_NODE_ID: &str = "resource-benchmark-root";

/// `n` session nodes with status `running`/`idle`/`waiting`, cycled
/// round-robin beneath a single non-relaunching root, all pointing at
/// `repo.local_path` and `agent`'s invocation -- the tree shape does not
/// vary with `n` (design §5.6.1).
///
/// The document shape is TermTree's own persisted schema, produced by
/// `AppStateController.getPersistedState()`
/// (`frontend/src/store/AppStateController.ts:941-990`): a single recursive
/// `tree` root plus the sibling preference fields. The app restores sessions
/// by reading `persisted.tree` (`:883` `restoreSessions(persisted.tree)`,
/// `:886`), so a document keyed on anything else relaunches **zero**
/// sessions and the N-session tier would silently measure an empty app
/// (spec FR-8, FR-9).
///
/// `sessionId` is left null on purpose. `restoreSessions` copies it into
/// `previousSessionId`, which is the scrollback buffer key; a fabricated id
/// would point at `.buf`/`.snap` files that do not exist. With no saved
/// `agentKind`/`agentSessionId` the relaunch path issues no provider resume
/// command, so each seeded pane starts the agent CLI **fresh** via its
/// `command` rather than resuming a prior conversation. The tier therefore
/// measures `n` freshly started agent sessions, not `n` resumed ones -- a
/// distinction the published method must state (spec FR-14).
fn fixture_for_n_sessions(
  n: u32,
  repo: &SeededRepo,
  agent: &AgentCliPin,
) -> String {
  const STATUSES: [&str; 3] = ["running", "idle", "waiting"];
  let sessions: Vec<Node> = (0..n)
    .map(|i| {
      node(
        &format!("resource-benchmark-session-{i}"),
        STATUSES[i as usize % STATUSES.len()],
        Some(agent.executable_path.clone()),
        Some(repo.local_path.clone()),
      )
    })
    .collect();
  let mut root = node(ROOT_NODE_ID, "none", None, None);
  root.children = sessions;
  serde_json::to_string_pretty(&serde_json::json!({
    "tree": root,
    "panelPosition": "right",
    "panelSplitPercent": 60,
    "panelCollapsed": false,
    "panelCollapsedPreferenceVersion": 1,
    "editorPanelSplitPercent": 65,
    "editorOutlineVisible": false,
    "editorOutlineSplitPercent": 78,
    "editorRecentFiles": [],
    "themeKey": "midnight",
    "lastThemeByMode": { "dark": "midnight", "light": "daylight" },
    "layoutType": "Rightward",
    "minimapVisible": false,
    "viewportZoom": 1,
    "viewportX": 0,
    "viewportY": 0,
    "onboardingSeen": true,
    "lastUsedCwd": repo.local_path,
    "fontPreference": "bundled",
    "mindmapLineStyle": "curved",
    "mindmapLineWidthPx": 2,
    "mindmapMarginParental": 18,
    "mindmapMarginSibling": 8,
    "mindmapAnimateConnections": true,
    "scrollbackNodeIds": [],
  }))
  .expect("Node serialization never fails")
}

impl SessionSeeder for TermTreeSeeder {
  fn seed(
    &self,
    n: u32,
    repo: &SeededRepo,
    agent: &AgentCliPin,
  ) -> Result<SeedPlan, SeedError> {
    let state =
      expected_scratch_state_path(&self.scratch_root, &self.state_directory)?;
    let directory = state.parent().expect("state has a parent");
    let backup = directory.join(BACKUP_FILENAME);
    fs::create_dir_all(directory).map_err(|e| SeedError(e.to_string()))?;
    let _lock = Lock::acquire(directory)?;
    if regular_file_or_missing(&backup, "Resource-benchmark seed backup")? {
      return Err(SeedError(format!(
        "Backup already exists at {}. Run `resource-benchmark restore` \
         before seeding again so it cannot be overwritten.",
        backup.display()
      )));
    }
    let fixture = fixture_for_n_sessions(n, repo, agent);
    if regular_file_or_missing(&state, "Production TermTree state")? {
      fs::copy(&state, &backup).map_err(|e| SeedError(e.to_string()))?;
    }
    atomic_write(&state, &fixture)?;
    Ok(SeedPlan {
      method: "production state.json pre-write".to_string(),
    })
  }

  fn restore(&self) -> Result<(), SeedError> {
    let state =
      expected_scratch_state_path(&self.scratch_root, &self.state_directory)?;
    let directory = state.parent().expect("state has a parent");
    if !directory.exists() {
      // Nothing was ever seeded; restoring is a safe no-op.
      return Ok(());
    }
    let backup = directory.join(BACKUP_FILENAME);
    let _lock = Lock::acquire(directory)?;
    if !regular_file_or_missing(&backup, "Resource-benchmark seed backup")? {
      // No seed is in progress; restoring is a safe no-op (design §9: the
      // manual `benchmark restore` escape hatch must always be safe).
      return Ok(());
    }
    if regular_file_or_missing(&state, "Production TermTree state")? {
      let current =
        fs::read_to_string(&state).map_err(|e| SeedError(e.to_string()))?;
      write_recovery(directory, &current)?;
    }
    fs::rename(&backup, &state).map_err(|e| SeedError(e.to_string()))?;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "resource-benchmark-termtree-seeder-test-{}-{}",
      process::id(),
      unique_suffix()
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

  /// The primary guard (spec item 2's inversion): a `state_directory` that
  /// is otherwise shaped correctly but lives **outside** the scratch root
  /// -- e.g. a real user's actual home directory, which is exactly what a
  /// public runner of this harness may have on their machine -- must be
  /// refused. This is the case the original (pre-inversion) guard could
  /// not catch at all: it only ever inspected the directory's own name.
  #[test]
  fn refuses_a_directory_outside_the_scratch_root() {
    let scratch_root = temp_home();
    let real_home = temp_home();
    let outside_directory = real_home
      .join("Library")
      .join("Application Support")
      .join("DocumentNode")
      .join("TermTree");
    let seeder = TermTreeSeeder {
      state_directory: outside_directory,
      scratch_root: scratch_root.clone(),
    };
    let error = seeder.seed(5, &repo(), &agent()).unwrap_err();
    assert!(error.0.contains("Refusing"));
    assert!(error.0.contains("scratch home"));
    fs::remove_dir_all(&scratch_root).unwrap();
    fs::remove_dir_all(&real_home).unwrap();
  }

  /// Secondary, defense-in-depth condition: even a `TermTreeDev`-named
  /// directory is refused when it lives *under* the scratch root, so a
  /// future edit cannot accidentally reintroduce the pre-inversion
  /// TermTree/TermTreeDev collision this module's history warns about.
  #[test]
  fn refuses_a_termtreedev_directory_even_under_the_scratch_root() {
    let home = temp_home();
    let dev_directory = home
      .join("Library")
      .join("Application Support")
      .join("DocumentNode")
      .join("TermTreeDev");
    let seeder = TermTreeSeeder {
      state_directory: dev_directory,
      scratch_root: home.clone(),
    };
    let error = seeder.seed(5, &repo(), &agent()).unwrap_err();
    assert!(error.0.contains("Refusing"));
    fs::remove_dir_all(&home).unwrap();
  }

  #[test]
  fn seed_then_restore_round_trips_prior_state() {
    let home = temp_home();
    let seeder = TermTreeSeeder::production(&home);
    fs::create_dir_all(&seeder.state_directory).unwrap();
    let state_path = seeder.state_directory.join(STATE_FILENAME);
    fs::write(&state_path, "{\"nodes\":[{\"id\":\"real-user-state\"}]}")
      .unwrap();

    let plan = seeder.seed(5, &repo(), &agent()).unwrap();
    assert_eq!(plan.method, "production state.json pre-write");
    let seeded = fs::read_to_string(&state_path).unwrap();
    assert!(seeded.contains("resource-benchmark-session-0"));
    assert!(seeded.contains(&agent().executable_path));

    seeder.restore().unwrap();
    let restored = fs::read_to_string(&state_path).unwrap();
    assert!(restored.contains("real-user-state"));

    fs::remove_dir_all(&home).unwrap();
  }

  #[test]
  fn seeding_twice_without_restoring_refuses_to_overwrite_the_backup() {
    let home = temp_home();
    let seeder = TermTreeSeeder::production(&home);
    fs::create_dir_all(&seeder.state_directory).unwrap();
    fs::write(
      seeder.state_directory.join(STATE_FILENAME),
      "{\"nodes\":[]}",
    )
    .unwrap();

    seeder.seed(3, &repo(), &agent()).unwrap();
    let error = seeder.seed(3, &repo(), &agent()).unwrap_err();
    assert!(error.0.contains("Backup already exists"));

    seeder.restore().unwrap();
    fs::remove_dir_all(&home).unwrap();
  }

  #[test]
  fn restore_with_nothing_seeded_is_a_safe_no_op() {
    let home = temp_home();
    let seeder = TermTreeSeeder::production(&home);
    assert!(seeder.restore().is_ok());
    fs::remove_dir_all(&home).unwrap();
  }

  #[test]
  fn seeding_when_no_prior_state_exists_seeds_without_a_backup() {
    let home = temp_home();
    let seeder = TermTreeSeeder::production(&home);
    seeder.seed(2, &repo(), &agent()).unwrap();
    let state_path = seeder.state_directory.join(STATE_FILENAME);
    assert!(state_path.exists());
    let backup_path = seeder.state_directory.join(BACKUP_FILENAME);
    assert!(!backup_path.exists());
    seeder.restore().unwrap();
    fs::remove_dir_all(&home).unwrap();
  }

  /// The defect this pins: the seeder once emitted `{"nodes": [...]}`.
  /// `AppStateController.loadPersistedState` reads `persisted.tree`
  /// (`frontend/src/store/AppStateController.ts:883`), so that document
  /// restored zero sessions and the N-session tier measured an empty app
  /// while every unit test still passed.
  #[test]
  fn seeded_document_uses_the_apps_tree_schema_not_a_flat_node_list() {
    let document: serde_json::Value =
      serde_json::from_str(&fixture_for_n_sessions(5, &repo(), &agent()))
        .unwrap();
    assert!(
      document
        .get("tree")
        .is_some_and(serde_json::Value::is_object),
      "persisted state must carry a `tree` root object"
    );
    assert!(
      document.get("nodes").is_none(),
      "`nodes` is not a key TermTree ever reads"
    );
  }

  #[test]
  fn only_the_session_nodes_are_relaunch_eligible() {
    const RELAUNCHED: [&str; 3] = ["running", "idle", "waiting"];
    let document: serde_json::Value =
      serde_json::from_str(&fixture_for_n_sessions(5, &repo(), &agent()))
        .unwrap();
    let root = &document["tree"];
    assert_eq!(root["id"], ROOT_NODE_ID);
    assert!(
      !RELAUNCHED.contains(&root["status"].as_str().unwrap()),
      "the root must not be queued for relaunch"
    );
    let children = root["children"].as_array().unwrap();
    assert_eq!(children.len(), 5);
    for child in children {
      assert!(RELAUNCHED.contains(&child["status"].as_str().unwrap()));
      assert!(child["children"].as_array().unwrap().is_empty());
    }
  }

  /// A first-run onboarding overlay would change both the cold-start and the
  /// memory numbers, so the seeded app must be the one users actually run.
  #[test]
  fn onboarding_is_marked_seen() {
    let document: serde_json::Value =
      serde_json::from_str(&fixture_for_n_sessions(1, &repo(), &agent()))
        .unwrap();
    assert_eq!(document["onboardingSeen"], serde_json::Value::Bool(true));
  }

  /// Pins the contract against `getPersistedState()`'s return type
  /// (`frontend/src/store/AppStateController.ts:941-966`). A field the app
  /// expects but the seeder omits falls back to a default that may not be
  /// the state we intend to measure.
  #[test]
  fn seeded_top_level_keys_match_the_apps_persisted_schema() {
    const EXPECTED: [&str; 26] = [
      "editorOutlineSplitPercent",
      "editorOutlineVisible",
      "editorPanelSplitPercent",
      "editorRecentFiles",
      "fontPreference",
      "lastThemeByMode",
      "lastUsedCwd",
      "layoutType",
      "mindmapAnimateConnections",
      "mindmapLineStyle",
      "mindmapLineWidthPx",
      "mindmapMarginParental",
      "mindmapMarginSibling",
      "minimapVisible",
      "onboardingSeen",
      "panelCollapsed",
      "panelCollapsedPreferenceVersion",
      "panelPosition",
      "panelSplitPercent",
      "scrollbackNodeIds",
      "themeKey",
      "tree",
      "viewportX",
      "viewportY",
      "viewportZoom",
      "splitGroup",
    ];
    let document: serde_json::Value =
      serde_json::from_str(&fixture_for_n_sessions(1, &repo(), &agent()))
        .unwrap();
    let mut seeded: Vec<&str> = document
      .as_object()
      .unwrap()
      .keys()
      .map(String::as_str)
      .collect();
    seeded.sort_unstable();
    let mut expected = EXPECTED.to_vec();
    expected.sort_unstable();
    // `splitGroup` is merged in by `main.ts` only when a terminal manager
    // exists, so the seeder does not write it.
    expected.retain(|key| *key != "splitGroup");
    assert_eq!(seeded, expected);
  }
}
