//! `sysinfo` process-tree snapshot and the pure BFS descendant walk it
//! feeds, modelled deliberately on
//! `src-tauri/src/command/terminal_cmd.rs:1588`'s
//! `get_descendant_pids_and_names` so the harness's idea of "this subject's
//! process tree" matches the app's own (design §5.2.1).
//!
//! This is the half of attribution that resolves Chromium/Electron helper
//! processes (true children of their app) and TermTree's own spawned PTY
//! shells and agent CLI processes -- but resolves **none** of a WebKit
//! subject's `launchd`-parented helpers. `launch_services.rs` supplies the
//! other half; `attribution.rs` takes their union.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRecord {
  pub pid: u32,
  pub ppid: u32,
  pub name: String,
  pub executable_path: Option<String>,
  pub rss_bytes: u64,
}

/// One live `sysinfo` snapshot of the full process table.
pub fn snapshot_processes() -> Vec<ProcessRecord> {
  let mut system = System::new();
  system.refresh_processes_specifics(
    ProcessesToUpdate::All,
    true,
    ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
  );

  system
    .processes()
    .iter()
    .map(|(pid, process)| ProcessRecord {
      pid: pid.as_u32(),
      ppid: process.parent().map(|p| p.as_u32()).unwrap_or(0),
      name: process.name().to_string_lossy().into_owned(),
      executable_path: process.exe().map(|p| p.to_string_lossy().into_owned()),
      rss_bytes: process.memory(),
    })
    .collect()
}

/// BFS over `records` from `root_pid`, returning descendant PIDs in
/// nearest-the-root-first order. Pure and fixture-testable, unlike its
/// model in `terminal_cmd.rs`, which takes a live `sysinfo::System`
/// directly. A `visited` set (not the parent link) is what terminates the
/// walk if the process table ever contains a PPID cycle (design §11).
pub fn descendants_of(records: &[ProcessRecord], root_pid: u32) -> Vec<u32> {
  let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
  for record in records {
    children.entry(record.ppid).or_default().push(record.pid);
  }

  let mut result = Vec::new();
  let mut visited: HashSet<u32> = HashSet::new();
  let mut queue: VecDeque<u32> = VecDeque::new();
  queue.push_back(root_pid);
  visited.insert(root_pid);

  while let Some(pid) = queue.pop_front() {
    if let Some(kids) = children.get(&pid) {
      for &child in kids {
        if visited.insert(child) {
          result.push(child);
          queue.push_back(child);
        }
      }
    }
  }
  result
}

pub fn record_by_pid(
  records: &[ProcessRecord],
  pid: u32,
) -> Option<&ProcessRecord> {
  records.iter().find(|r| r.pid == pid)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  #[derive(serde::Deserialize)]
  struct ProcessTableFixture {
    #[serde(rename = "rootPid")]
    root_pid: u32,
    processes: Vec<ProcessRecord>,
  }

  fn read(name: &str) -> ProcessTableFixture {
    let text = fs::read_to_string(format!(
      "{}/fixtures/{name}",
      env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
  }

  #[test]
  fn chromium_fixture_resolves_41_direct_children_and_58_total() {
    let fixture = read("process-table-chromium.json");
    let direct: Vec<_> = fixture
      .processes
      .iter()
      .filter(|p| p.ppid == fixture.root_pid)
      .collect();
    assert_eq!(direct.len(), 41);

    let all = descendants_of(&fixture.processes, fixture.root_pid);
    assert_eq!(all.len(), 58);
    // The isolated 2-cycle must never be reachable from the real root.
    assert!(!all.contains(&90001));
    assert!(!all.contains(&90002));
  }

  #[test]
  fn a_ppid_cycle_terminates_via_the_visited_set() {
    let fixture = read("process-table-chromium.json");
    // Starting the walk from *inside* the isolated 2-cycle must terminate
    // rather than loop forever -- proving the guard is the `visited` set,
    // not the (broken, circular) parent link.
    let from_cycle = descendants_of(&fixture.processes, 90001);
    assert_eq!(from_cycle, vec![90002]);
  }

  #[test]
  fn webkit_fixture_resolves_19_descendants_none_of_them_webkit_helpers() {
    let fixture = read("process-table-webkit.json");
    let all = descendants_of(&fixture.processes, fixture.root_pid);
    assert_eq!(all.len(), 19);
    assert!(
      !all.contains(&99999),
      "unrelated daemon must not be included"
    );
    for pid in &all {
      let record = record_by_pid(&fixture.processes, *pid).unwrap();
      assert_ne!(record.name, "com.apple.WebKit.WebContent");
    }
  }
}
