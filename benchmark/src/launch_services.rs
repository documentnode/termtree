//! `lsappinfo list` invocation and its pure parser.
//!
//! WKWebView helper processes are registered with LaunchServices under the
//! owning app's display name even though they are `launchd`-parented
//! (PPID 1), which is what makes this the correct-but-incomplete half of
//! attribution (design §1, §5.2.1) -- `process_tree.rs` supplies the other
//! half.

use crate::exec::{run_capture, ExecError};

pub const LSAPPINFO_PROGRAM: &str = "/usr/bin/lsappinfo";

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchServicesEntry {
  pub display_name: String,
  pub bundle_identifier: Option<String>,
  pub bundle_path: Option<String>,
  pub executable_path: Option<String>,
  pub pid: Option<u32>,
  pub in_front: bool,
}

pub fn invoke_lsappinfo_list() -> Result<String, ExecError> {
  let output = run_capture(LSAPPINFO_PROGRAM, &["list"])?;
  Ok(output.stdout)
}

/// Parse the whole `lsappinfo list` capture into one entry per registered
/// application. Pure and total: every observed real-output shape (§9, §11)
/// is handled without panicking.
pub fn parse_lsappinfo_list(text: &str) -> Vec<LaunchServicesEntry> {
  text.split("\n\n").filter_map(parse_entry).collect()
}

fn parse_entry(block: &str) -> Option<LaunchServicesEntry> {
  let mut lines = block.lines();
  let header = lines.find(|line| !line.trim().is_empty())?;
  let header_trimmed = header.trim_start();
  // Header shape: `NN) "Display Name" ASN:0x0-0xHEX:` optionally followed by
  // ` (in front)`. Display names always appear quoted and may contain
  // spaces, so the name is read between the first and last `"` rather than
  // by splitting on whitespace.
  let first_quote = header_trimmed.find('"')?;
  let rest = &header_trimmed[first_quote + 1..];
  let last_quote = rest.find('"')?;
  let display_name = rest[..last_quote].to_string();
  let in_front = header_trimmed.contains("(in front)");

  let mut bundle_identifier = None;
  let mut bundle_path = None;
  let mut executable_path = None;
  let mut pid = None;

  for line in block.lines() {
    let trimmed = line.trim();
    if let Some(value) = trimmed.strip_prefix("bundleID=") {
      bundle_identifier = parse_quoted_or_null(value);
    } else if let Some(value) = trimmed.strip_prefix("bundle path=") {
      bundle_path = parse_quoted_or_null(value);
    } else if let Some(value) = trimmed.strip_prefix("executable path=") {
      executable_path = parse_quoted_or_null(value);
    } else if let Some(value) = trimmed.strip_prefix("pid = ") {
      // The pid line carries trailing flags (`!signalled`, `sandboxed`,
      // `type="..."`); only the leading integer is the PID.
      pid = value
        .split_whitespace()
        .next()
        .and_then(|token| token.parse::<u32>().ok());
    }
  }

  Some(LaunchServicesEntry {
    display_name,
    bundle_identifier,
    bundle_path,
    executable_path,
    pid,
    in_front,
  })
}

/// Strips a `"quoted string"` value down to its contents, or returns `None`
/// for the literal `[ NULL ]` lsappinfo prints for an absent field (verified
/// on `universalaccessd`'s `bundleID=` and `Version=`).
fn parse_quoted_or_null(value: &str) -> Option<String> {
  let value = value.trim();
  if value.starts_with('[') {
    return None;
  }
  let value = value.strip_prefix('"')?;
  let end = value.find('"')?;
  Some(value[..end].to_string())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  fn fixture() -> String {
    fs::read_to_string(concat!(
      env!("CARGO_MANIFEST_DIR"),
      "/fixtures/lsappinfo-list.txt"
    ))
    .unwrap()
  }

  #[test]
  fn parses_every_termtree_entry_with_distinct_pids() {
    let entries = parse_lsappinfo_list(&fixture());
    let termtree: Vec<_> = entries
      .iter()
      .filter(|e| {
        e.display_name == "TermTree" || e.display_name.starts_with("TermTree ")
      })
      .collect();
    // The main process plus at least the Networking, GPU, and one WebContent
    // helper -- the fixture captures two WebContent entries with the SAME
    // display name, which must yield two distinct PIDs, not be collapsed by
    // a map keyed on name.
    assert!(termtree.len() >= 4, "found: {termtree:?}");
    let web_content: Vec<_> = entries
      .iter()
      .filter(|e| e.display_name == "TermTree Web Content")
      .collect();
    assert_eq!(web_content.len(), 2);
    let pids: std::collections::HashSet<_> =
      web_content.iter().filter_map(|e| e.pid).collect();
    assert_eq!(pids.len(), 2, "expected two distinct WebContent PIDs");

    let main = entries
      .iter()
      .find(|e| e.display_name == "TermTree")
      .unwrap();
    assert_eq!(
      main.bundle_identifier.as_deref(),
      Some("com.termtree.desktop")
    );
    assert_eq!(main.pid, Some(56070));
    assert!(main.in_front);
  }

  #[test]
  fn null_bundle_id_and_version_parse_as_none() {
    let entries = parse_lsappinfo_list(&fixture());
    let entry = entries
      .iter()
      .find(|e| e.display_name == "universalaccessd")
      .unwrap();
    assert_eq!(entry.bundle_identifier, None);
  }

  #[test]
  fn entry_with_no_pid_line_yields_none_not_zero() {
    let entries = parse_lsappinfo_list(&fixture());
    let entry = entries
      .iter()
      .find(|e| e.display_name == "Backup Agent")
      .unwrap();
    assert_eq!(entry.pid, None);
  }

  #[test]
  fn pid_line_trailing_flags_do_not_corrupt_the_pid() {
    let entries = parse_lsappinfo_list(&fixture());
    let entry = entries
      .iter()
      .find(|e| e.display_name == "universalaccessd")
      .unwrap();
    // Real line: `pid = 55129 !signalled type="BackgroundOnly" ...`
    assert_eq!(entry.pid, Some(55129));
  }
}
