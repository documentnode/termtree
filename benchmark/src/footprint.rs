//! `footprint -j` invocation, its pure parser, and the pid-set verification
//! that guards against a dead/recycled PID silently producing a wrong
//! number (design §5.3.1).
//!
//! # Never `-p`
//!
//! `footprint -h` documents `-p, --proc <name>` and `-p, --pid <pid>` on
//! separate lines sharing one short flag; name resolution wins, and it is a
//! **partial** match (verified: `footprint -j out.json -p 1` measured four
//! unrelated 1Password processes). This module always builds the long form,
//! `--pid <pid>`, once per requested PID, and never the short form. See also
//! `exec.rs`'s module doc for the separate zsh-word-splitting hazard this
//! crate is immune to by construction but a re-runner is not.

use crate::exec::{run_capture, ExecError};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub const FOOTPRINT_PROGRAM: &str = "/usr/bin/footprint";

#[derive(Debug, Clone, PartialEq)]
pub struct FootprintReport {
  /// `footprint`'s set-level `total footprint` -- pages shared across the
  /// measured set are counted once. This, not the naive per-process sum, is
  /// what the harness publishes as `memPhysFootprintBytes` (design §2.3).
  pub total_footprint_bytes: u64,
  pub processes: Vec<FootprintProcess>,
  pub shared: Vec<SharedRegionGroup>,
  pub errors: Vec<String>,
  pub warnings: Vec<String>,
  pub page_size_bytes: u64,
  pub start_time_iso: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FootprintProcess {
  pub pid: u32,
  pub name: String,
  pub footprint_bytes: u64,
  pub phys_footprint_bytes: Option<u64>,
  pub has_categories: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedRegionGroup {
  pub pids: Vec<u32>,
  pub specific_to_pid: Option<u32>,
  pub is_shared_cache: bool,
}

#[derive(Debug)]
pub enum FootprintParseError {
  MissingField(&'static str),
  WrongType(&'static str),
}

impl std::fmt::Display for FootprintParseError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::MissingField(field) => write!(f, "missing field: {field}"),
      Self::WrongType(field) => write!(f, "wrong type for field: {field}"),
    }
  }
}

impl std::error::Error for FootprintParseError {}

/// Build the argument vector for one `footprint -j` invocation over
/// `requested_pids`. A `#[test]` below asserts this vector contains exactly
/// one `--pid` per PID, no `-p`, and no argument containing a space --
/// the cheap structural guard against reintroducing the flag ambiguity or a
/// string-interpolated PID list (design §5.3.1, §11).
pub fn build_argument_vector(
  output_path: &str,
  requested_pids: &[u32],
) -> Vec<String> {
  let mut args = vec!["-j".to_string(), output_path.to_string()];
  for pid in requested_pids {
    args.push("--pid".to_string());
    args.push(pid.to_string());
  }
  args
}

pub fn invoke_footprint(
  output_path: &str,
  requested_pids: &[u32],
) -> Result<(), ExecError> {
  let args = build_argument_vector(output_path, requested_pids);
  let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
  run_capture(FOOTPRINT_PROGRAM, &arg_refs)?;
  Ok(())
}

pub fn parse_footprint_json(
  text: &str,
) -> Result<FootprintReport, FootprintParseError> {
  let root: Value = serde_json::from_str(text)
    .map_err(|_| FootprintParseError::WrongType("root"))?;

  let total_footprint_bytes = root
    .get("total footprint")
    .and_then(Value::as_u64)
    .ok_or(FootprintParseError::MissingField("total footprint"))?;
  let page_size_bytes = root
    .get("page size")
    .and_then(Value::as_u64)
    .ok_or(FootprintParseError::MissingField("page size"))?;

  let processes_value = root
    .get("processes")
    .and_then(Value::as_array)
    .ok_or(FootprintParseError::MissingField("processes"))?;
  let mut processes = Vec::with_capacity(processes_value.len());
  for entry in processes_value {
    processes.push(parse_process(entry)?);
  }

  let shared_value = root
    .get("shared")
    .and_then(Value::as_array)
    .ok_or(FootprintParseError::MissingField("shared"))?;
  let shared = shared_value.iter().map(parse_shared_group).collect();

  let errors = string_array(&root, "errors");
  let warnings = string_array(&root, "warnings");
  let start_time_iso = root
    .get("start_time")
    .and_then(|v| v.get("date"))
    .and_then(Value::as_str)
    .map(str::to_string);

  Ok(FootprintReport {
    total_footprint_bytes,
    processes,
    shared,
    errors,
    warnings,
    page_size_bytes,
    start_time_iso,
  })
}

fn parse_process(
  entry: &Value,
) -> Result<FootprintProcess, FootprintParseError> {
  let pid = entry
    .get("pid")
    .and_then(Value::as_u64)
    .map(|p| p as u32)
    .ok_or(FootprintParseError::MissingField("processes[].pid"))?;
  let name = entry
    .get("name")
    .and_then(Value::as_str)
    .ok_or(FootprintParseError::MissingField("processes[].name"))?
    .to_string();
  let footprint_bytes = entry
    .get("footprint")
    .and_then(Value::as_u64)
    .ok_or(FootprintParseError::MissingField("processes[].footprint"))?;
  let phys_footprint_bytes = entry
    .get("auxiliary")
    .and_then(|aux| aux.get("phys_footprint"))
    .and_then(Value::as_u64);
  // Small processes may omit `categories` entirely; its absence is not a
  // parse error (design §9, §11).
  let has_categories = entry
    .get("categories")
    .and_then(Value::as_object)
    .map(|obj| !obj.is_empty())
    .unwrap_or(false);

  Ok(FootprintProcess {
    pid,
    name,
    footprint_bytes,
    phys_footprint_bytes,
    has_categories,
  })
}

fn parse_shared_group(entry: &Value) -> SharedRegionGroup {
  let pids = entry
    .get("pids")
    .and_then(Value::as_array)
    .map(|arr| {
      arr
        .iter()
        .filter_map(|p| p.as_u64().map(|p| p as u32))
        .collect()
    })
    .unwrap_or_default();
  let specific_to_pid = entry
    .get("specific_to_pid")
    .and_then(Value::as_u64)
    .map(|p| p as u32);
  let is_shared_cache = entry
    .get("shared-cache")
    .and_then(Value::as_bool)
    .unwrap_or(false);
  SharedRegionGroup {
    pids,
    specific_to_pid,
    is_shared_cache,
  }
}

fn string_array(root: &Value, key: &str) -> Vec<String> {
  root
    .get(key)
    .and_then(Value::as_array)
    .map(|arr| {
      arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
    })
    .unwrap_or_default()
}

/// `memPhysFootprintProcessSumBytes` -- the naive per-process sum a
/// `pidusage`-style tool would report. Never presented as
/// `memPhysFootprintBytes`'s equal; the difference is
/// `sharedPageDoubleCountBytes` (design §2.3, §6.2).
pub fn process_sum_bytes(report: &FootprintReport) -> u64 {
  report.processes.iter().map(|p| p.footprint_bytes).sum()
}

pub fn shared_page_double_count_bytes(report: &FootprintReport) -> u64 {
  process_sum_bytes(report).saturating_sub(report.total_footprint_bytes)
}

#[derive(Debug, PartialEq)]
pub struct PidSetMismatch {
  pub requested: HashSet<u32>,
  pub returned: HashSet<u32>,
}

/// Asserts the PIDs `footprint` actually measured equal exactly the PIDs
/// requested. This is not a guard against a parser bug -- it defends
/// against a process exiting or a PID being recycled between attribution and
/// measurement, both of which otherwise produce a plausible-looking
/// undercount with an empty `errors` array (design §5.3.1, §9).
pub fn verify_pid_set(
  requested: &[u32],
  report: &FootprintReport,
) -> Result<(), PidSetMismatch> {
  let requested_set: HashSet<u32> = requested.iter().copied().collect();
  let returned_set: HashSet<u32> =
    report.processes.iter().map(|p| p.pid).collect();
  if requested_set == returned_set {
    Ok(())
  } else {
    Err(PidSetMismatch {
      requested: requested_set,
      returned: returned_set,
    })
  }
}

/// A companion assertion beside [`verify_pid_set`]: a PID can be recycled
/// between attribution and measurement, so the returned `name` for a PID no
/// longer matches what was attributed even though the PID set itself is
/// unchanged. `attributed_names` maps PID to the name the attribution
/// resolver (`attribution.rs`) recorded for it.
pub fn verify_process_names(
  attributed_names: &BTreeMap<u32, String>,
  report: &FootprintReport,
) -> Result<(), u32> {
  for process in &report.processes {
    if let Some(expected) = attributed_names.get(&process.pid) {
      if expected != &process.name {
        return Err(process.pid);
      }
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  fn read_fixture(name: &str) -> String {
    fs::read_to_string(format!(
      "{}/fixtures/{name}",
      env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
  }

  // --- Argument-vector test: written first, per design §12 Phase 2 ------

  #[test]
  fn argument_vector_uses_long_form_pid_only() {
    let args = build_argument_vector("/tmp/out.json", &[56070, 56072, 56074]);
    let pid_flag_count = args.iter().filter(|a| *a == "--pid").count();
    assert_eq!(pid_flag_count, 3, "one --pid per requested PID: {args:?}");
    assert!(
      !args.iter().any(|a| a == "-p"),
      "must never use the ambiguous short flag: {args:?}"
    );
    assert!(
      !args.iter().any(|a| a.contains(' ')),
      "no argument may be a space-joined PID list: {args:?}"
    );
    assert_eq!(args, vec![
      "-j",
      "/tmp/out.json",
      "--pid",
      "56070",
      "--pid",
      "56072",
      "--pid",
      "56074"
    ]);
  }

  #[test]
  fn argument_vector_is_empty_pid_safe() {
    let args = build_argument_vector("/tmp/out.json", &[]);
    assert_eq!(args, vec!["-j", "/tmp/out.json"]);
  }

  // --- parse_footprint_json ----------------------------------------------

  #[test]
  fn parses_termtree_6proc_capture() {
    let report =
      parse_footprint_json(&read_fixture("footprint-termtree-6proc.json"))
        .unwrap();
    assert_eq!(report.page_size_bytes, 16384);
    assert!(report.errors.is_empty());
    assert_eq!(report.processes.len(), 5);
    assert!(report.start_time_iso.is_some());

    let sum = process_sum_bytes(&report);
    assert_eq!(
      sum,
      report.total_footprint_bytes + shared_page_double_count_bytes(&report)
    );
    assert!(
      report.total_footprint_bytes < sum,
      "the set total must be lower than the naive sum \
       (shared pages counted once): total={}, sum={sum}",
      report.total_footprint_bytes
    );

    // Every process in this capture is large enough to carry categories.
    assert!(report.processes.iter().all(|p| p.has_categories));

    let names: Vec<_> =
      report.processes.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"termtree"));
    assert_eq!(
      names
        .iter()
        .filter(|n| **n == "com.apple.WebKit.WebContent")
        .count(),
      2
    );
  }

  #[test]
  fn parses_shared_set_with_shared_cache_and_missing_categories() {
    let report =
      parse_footprint_json(&read_fixture("footprint-shared-set.json")).unwrap();
    assert_eq!(report.processes.len(), 3);
    let sum = process_sum_bytes(&report);
    assert_ne!(
      sum, report.total_footprint_bytes,
      "shared-set fixture must show sum != total"
    );
    let double_count = shared_page_double_count_bytes(&report);
    assert_eq!(sum, report.total_footprint_bytes + double_count);

    // pid 56074 (Networking) was stripped of `categories` to prove a small
    // process without a breakdown does not error the parse.
    let networking = report.processes.iter().find(|p| p.pid == 56074).unwrap();
    assert!(!networking.has_categories);

    assert!(report.shared.iter().any(|s| s.specific_to_pid.is_some()));
    assert!(report.shared.iter().any(|s| s.is_shared_cache));
  }

  #[test]
  fn verify_pid_set_detects_a_dead_pid() {
    let report =
      parse_footprint_json(&read_fixture("footprint-dead-pid.json")).unwrap();
    // Requested one PID (56074) that exited before measurement.
    let requested = [64305u32, 83276, 56070, 56072, 56074];
    let mismatch = verify_pid_set(&requested, &report).unwrap_err();
    assert!(mismatch.requested.contains(&56074));
    assert!(!mismatch.returned.contains(&56074));
  }

  #[test]
  fn verify_pid_set_passes_when_sets_match() {
    let report =
      parse_footprint_json(&read_fixture("footprint-termtree-6proc.json"))
        .unwrap();
    let requested: Vec<u32> = report.processes.iter().map(|p| p.pid).collect();
    assert!(verify_pid_set(&requested, &report).is_ok());
  }

  #[test]
  fn verify_process_names_detects_a_recycled_pid() {
    let report =
      parse_footprint_json(&read_fixture("footprint-termtree-6proc.json"))
        .unwrap();
    let mut attributed = BTreeMap::new();
    attributed.insert(56070u32, "some_other_process".to_string());
    let mismatch = verify_process_names(&attributed, &report).unwrap_err();
    assert_eq!(mismatch, 56070);
  }
}
