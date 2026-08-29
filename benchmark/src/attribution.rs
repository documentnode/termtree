//! The attribution resolver (spec FR-2): a subject's attributable process
//! set is the deduplicated union of its LaunchServices-owned processes
//! (`launch_services.rs`) and its process-tree descendants
//! (`process_tree.rs`). Neither mechanism alone is complete -- using either
//! alone must fail the run rather than silently undercount (design §5.2.2).

use crate::launch_services::LaunchServicesEntry;
use crate::process_tree::ProcessRecord;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoverySource {
  LaunchServices,
  ProcessTree,
  Both,
}

impl DiscoverySource {
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::LaunchServices => "launch-services",
      Self::ProcessTree => "process-tree",
      Self::Both => "both",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessRole {
  Orchestrator,
  AgentCliSession,
}

impl ProcessRole {
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::Orchestrator => "orchestrator",
      Self::AgentCliSession => "agent-cli-session",
    }
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributedProcess {
  pub pid: u32,
  pub name: String,
  pub executable_path: Option<String>,
  pub discovered_by: DiscoverySource,
  pub role: ProcessRole,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributableProcessSet {
  pub main_pid: u32,
  pub processes: Vec<AttributedProcess>,
}

impl AttributableProcessSet {
  pub fn pids(&self) -> Vec<u32> {
    self.processes.iter().map(|p| p.pid).collect()
  }

  pub fn orchestrator_pids(&self) -> Vec<u32> {
    self
      .processes
      .iter()
      .filter(|p| p.role == ProcessRole::Orchestrator)
      .map(|p| p.pid)
      .collect()
  }

  pub fn agent_cli_pids(&self) -> Vec<u32> {
    self
      .processes
      .iter()
      .filter(|p| p.role == ProcessRole::AgentCliSession)
      .map(|p| p.pid)
      .collect()
  }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttributionError {
  /// Spec FR-2: using either mechanism alone is an invalid attribution.
  /// Fired when the LaunchServices set is empty for a subject whose family
  /// expects helper entries, or when the process-tree set is empty for a
  /// subject/tier expected to have descendants.
  Incomplete {
    launch_services_count: usize,
    process_tree_count: usize,
  },
}

/// A subject's LaunchServices process set: every entry whose display name is
/// `launch_services_name` or starts with `"{launch_services_name} "`, gated
/// by `bundle_ids` so an unrelated app whose name happens to share the
/// prefix (design §9) is never attributed.
pub fn resolve_launch_services_pids(
  entries: &[LaunchServicesEntry],
  launch_services_name: &str,
  bundle_ids: &[&str],
) -> Vec<(u32, String)> {
  let prefix = format!("{launch_services_name} ");
  entries
    .iter()
    .filter(|entry| {
      (entry.display_name == launch_services_name
        || entry.display_name.starts_with(&prefix))
        && entry
          .bundle_identifier
          .as_deref()
          .is_some_and(|id| bundle_ids.contains(&id))
    })
    .filter_map(|entry| entry.pid.map(|pid| (pid, entry.display_name.clone())))
    .collect()
}

/// Builds the deduplicated union of the two mechanisms' PIDs into an
/// [`AttributableProcessSet`], defaulting every process's role to
/// `Orchestrator` until [`partition_by_role`] runs. Fails per spec FR-2 if
/// either mechanism's set is empty.
pub fn resolve_union(
  main_pid: u32,
  launch_services_pids: &[(u32, String)],
  tree_records: &[ProcessRecord],
  tree_descendant_pids: &[u32],
) -> Result<AttributableProcessSet, AttributionError> {
  if launch_services_pids.is_empty() || tree_descendant_pids.is_empty() {
    return Err(AttributionError::Incomplete {
      launch_services_count: launch_services_pids.len(),
      process_tree_count: tree_descendant_pids.len(),
    });
  }

  let mut by_pid: BTreeMap<u32, AttributedProcess> = BTreeMap::new();

  for (pid, name) in launch_services_pids {
    by_pid.insert(*pid, AttributedProcess {
      pid: *pid,
      name: name.clone(),
      executable_path: None,
      discovered_by: DiscoverySource::LaunchServices,
      role: ProcessRole::Orchestrator,
    });
  }

  for pid in tree_descendant_pids {
    let record = crate::process_tree::record_by_pid(tree_records, *pid);
    let name = record.map(|r| r.name.clone()).unwrap_or_default();
    let executable_path = record.and_then(|r| r.executable_path.clone());
    by_pid
      .entry(*pid)
      .and_modify(|existing| {
        existing.discovered_by = DiscoverySource::Both;
        if existing.executable_path.is_none() {
          existing.executable_path = executable_path.clone();
        }
      })
      .or_insert(AttributedProcess {
        pid: *pid,
        name,
        executable_path,
        discovered_by: DiscoverySource::ProcessTree,
        role: ProcessRole::Orchestrator,
      });
  }

  Ok(AttributableProcessSet {
    main_pid,
    processes: by_pid.into_values().collect(),
  })
}

/// Applies **one rule to every subject** (spec FR-2's "the published method
/// states this explicitly"): a process is a session root if its executable
/// path equals the pinned agent CLI's resolved path or the host login
/// shell's path; every descendant of a session root is also
/// `AgentCliSession`; everything else is `Orchestrator`. Deliberately not a
/// per-runtime-family rule -- see design §5.2.3 for why `tmux`/`node-pty`
/// sidecars and the login shell land in `Orchestrator`/`AgentCliSession`
/// respectively under this one rule.
pub fn partition_by_role(
  set: &mut AttributableProcessSet,
  tree_records: &[ProcessRecord],
  agent_cli_executable_path: &str,
  login_shell_path: &str,
) {
  let is_session_root = |path: &Option<String>| {
    path.as_deref() == Some(agent_cli_executable_path)
      || path.as_deref() == Some(login_shell_path)
  };

  let session_roots: Vec<u32> = set
    .processes
    .iter()
    .filter(|p| is_session_root(&p.executable_path))
    .map(|p| p.pid)
    .collect();

  let mut session_pids: std::collections::HashSet<u32> =
    session_roots.iter().copied().collect();
  for root in &session_roots {
    for descendant in crate::process_tree::descendants_of(tree_records, *root) {
      session_pids.insert(descendant);
    }
  }

  for process in &mut set.processes {
    process.role = if session_pids.contains(&process.pid) {
      ProcessRole::AgentCliSession
    } else {
      ProcessRole::Orchestrator
    };
  }
}

/// Marks a companion process (e.g. CodeNomad's local server, diri's
/// `dirijord-rs` daemon) as `Orchestrator`, inserting it into the set if the
/// union did not already discover it (spec FR-2: "client/server subjects
/// ... attribute their local server process ... not as a separate,
/// uncounted process").
pub fn attribute_companion_process(
  set: &mut AttributableProcessSet,
  pid: u32,
  name: &str,
  executable_path: Option<String>,
) {
  set
    .processes
    .iter_mut()
    .find(|p| p.pid == pid)
    .map(|p| p.role = ProcessRole::Orchestrator)
    .unwrap_or_else(|| {
      set.processes.push(AttributedProcess {
        pid,
        name: name.to_string(),
        executable_path,
        discovered_by: DiscoverySource::ProcessTree,
        role: ProcessRole::Orchestrator,
      });
    });
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::launch_services::parse_lsappinfo_list;
  use crate::process_tree::descendants_of;
  use std::fs;

  fn read(name: &str) -> String {
    fs::read_to_string(format!(
      "{}/fixtures/{name}",
      env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
  }

  #[derive(serde::Deserialize)]
  struct ProcessTableFixture {
    #[serde(rename = "rootPid")]
    root_pid: u32,
    processes: Vec<ProcessRecord>,
  }

  fn read_process_table(name: &str) -> ProcessTableFixture {
    serde_json::from_str(&read(name)).unwrap()
  }

  #[test]
  fn union_of_zero_overlap_webkit_and_tree_sets_is_deduped_and_labelled() {
    let ls_entries = parse_lsappinfo_list(&read("lsappinfo-list.txt"));
    let ls_pids = resolve_launch_services_pids(&ls_entries, "TermTree", &[
      "com.termtree.desktop",
      "com.apple.WebKit.Networking",
      "com.apple.WebKit.GPU",
      "com.apple.WebKit.WebContent",
    ]);
    assert!(ls_pids.len() >= 4);

    let tree = read_process_table("process-table-webkit.json");
    let descendants = descendants_of(&tree.processes, tree.root_pid);
    assert_eq!(descendants.len(), 19);

    let set =
      resolve_union(tree.root_pid, &ls_pids, &tree.processes, &descendants)
        .unwrap();

    // Zero overlap between the two mechanisms in this fixture -- every PID
    // is discovered by exactly one, never `Both`.
    assert!(set
      .processes
      .iter()
      .all(|p| p.discovered_by != DiscoverySource::Both));
    assert_eq!(set.processes.len(), ls_pids.len() + descendants.len());
  }

  #[test]
  fn union_marks_a_pid_found_by_both_mechanisms_as_both() {
    let ls_pids = vec![(100u32, "Subject".to_string())];
    let tree_records = vec![ProcessRecord {
      pid: 100,
      ppid: 1,
      name: "subject".into(),
      executable_path: Some("/Applications/Subject.app/subject".into()),
      rss_bytes: 0,
    }];
    let set = resolve_union(100, &ls_pids, &tree_records, &[100]).unwrap();
    assert_eq!(set.processes.len(), 1);
    assert_eq!(set.processes[0].discovered_by, DiscoverySource::Both);
  }

  #[test]
  fn empty_launch_services_set_is_an_invalid_attribution() {
    let tree_records = vec![ProcessRecord {
      pid: 2,
      ppid: 1,
      name: "child".into(),
      executable_path: None,
      rss_bytes: 0,
    }];
    let error = resolve_union(1, &[], &tree_records, &[2]).unwrap_err();
    assert!(matches!(error, AttributionError::Incomplete { .. }));
  }

  #[test]
  fn empty_process_tree_set_is_an_invalid_attribution() {
    let ls_pids = vec![(1u32, "Subject".to_string())];
    let error = resolve_union(1, &ls_pids, &[], &[]).unwrap_err();
    assert!(matches!(error, AttributionError::Incomplete { .. }));
  }

  #[test]
  fn partition_puts_agent_cli_and_its_descendants_and_login_shell_in_session() {
    let tree = read_process_table("process-table-webkit.json");
    let descendants = descendants_of(&tree.processes, tree.root_pid);
    let mut set = resolve_union(
      tree.root_pid,
      &[(tree.root_pid, "TermTree".to_string())],
      &tree.processes,
      &descendants,
    )
    .unwrap();

    partition_by_role(
      &mut set,
      &tree.processes,
      "/Users/dev/.local/bin/claude",
      "/bin/zsh",
    );

    let orchestrator = set.orchestrator_pids();
    let session = set.agent_cli_pids();
    assert!(orchestrator.contains(&tree.root_pid));
    // Every zsh (login shell) and every claude (agent CLI) process, plus
    // their descendants, must land in AgentCliSession.
    for record in &tree.processes {
      if record.name == "zsh" || record.name == "claude" {
        assert!(
          session.contains(&record.pid),
          "{} ({}) should be AgentCliSession",
          record.name,
          record.pid
        );
      }
    }
    assert!(!session.is_empty());
  }

  #[test]
  fn partition_puts_tmux_and_node_pty_sidecar_in_orchestrator() {
    let tree = read_process_table("process-table-chromium.json");
    let descendants = descendants_of(&tree.processes, tree.root_pid);
    let ls_pids = vec![(tree.root_pid, "Collaborator".to_string())];
    let mut set =
      resolve_union(tree.root_pid, &ls_pids, &tree.processes, &descendants)
        .unwrap();

    partition_by_role(
      &mut set,
      &tree.processes,
      "/Users/dev/.local/bin/claude",
      "/bin/zsh",
    );

    let tmux = tree.processes.iter().find(|p| p.name == "tmux").unwrap();
    let node = tree.processes.iter().find(|p| p.name == "node").unwrap();
    let orchestrator = set.orchestrator_pids();
    assert!(
      orchestrator.contains(&tmux.pid),
      "tmux is Collaborator's own implementation choice, not a session root"
    );
    assert!(
      orchestrator.contains(&node.pid),
      "the node-pty sidecar is Collaborator's own implementation choice"
    );
    // Its own zsh grandchildren, however, ARE agent CLI sessions under the
    // one rule -- the shell is a per-session cost, not part of Collaborator
    // itself.
    let session = set.agent_cli_pids();
    for record in &tree.processes {
      if record.name == "zsh" {
        assert!(session.contains(&record.pid));
      }
    }
  }

  #[test]
  fn attribute_companion_process_adds_or_reclassifies_as_orchestrator() {
    let mut set = AttributableProcessSet {
      main_pid: 1,
      processes: vec![],
    };
    attribute_companion_process(
      &mut set,
      500,
      "codenomad-server",
      Some("/Applications/CodeNomad.app/server".into()),
    );
    assert_eq!(set.processes.len(), 1);
    assert_eq!(set.processes[0].role, ProcessRole::Orchestrator);
  }
}
