//! Machine spec, OS build, subject/agent version probes, and repo ref
//! capture (spec FR-11). Every probe is a thin wrapper over `exec.rs`; the
//! parsing each does is intentionally trivial (single-value `sysctl -n` /
//! `sw_vers -productVersion` reads), so it is exercised through `doctor`
//! rather than fixture-tested line by line.

use crate::exec::run_capture;
use crate::result::{AgentCliVersion, HarnessRef, MachineSpec, OsBuild};

pub fn probe_machine_spec() -> MachineSpec {
  let cpu_brand =
    run_capture("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"])
      .map(|o| o.stdout.trim().to_string())
      .unwrap_or_else(|_| "unknown".to_string());
  let logical_cores = run_capture("/usr/sbin/sysctl", &["-n", "hw.logicalcpu"])
    .ok()
    .and_then(|o| o.stdout.trim().parse().ok())
    .unwrap_or(0);
  let physical_cores =
    run_capture("/usr/sbin/sysctl", &["-n", "hw.physicalcpu"])
      .ok()
      .and_then(|o| o.stdout.trim().parse().ok())
      .unwrap_or(0);
  let ram_bytes = run_capture("/usr/sbin/sysctl", &["-n", "hw.memsize"])
    .ok()
    .and_then(|o| o.stdout.trim().parse().ok())
    .unwrap_or(0);
  let page_size_bytes = run_capture("/usr/bin/getconf", &["PAGESIZE"])
    .ok()
    .and_then(|o| o.stdout.trim().parse().ok())
    .unwrap_or(16384);

  MachineSpec {
    cpu_brand,
    logical_cores,
    physical_cores,
    ram_bytes,
    page_size_bytes,
  }
}

pub fn probe_os_build() -> OsBuild {
  let product_version = run_capture("/usr/bin/sw_vers", &["-productVersion"])
    .map(|o| o.stdout.trim().to_string())
    .unwrap_or_else(|_| "unknown".to_string());
  let build_version = run_capture("/usr/bin/sw_vers", &["-buildVersion"])
    .map(|o| o.stdout.trim().to_string())
    .unwrap_or_else(|_| "unknown".to_string());
  OsBuild {
    product_name: "macOS".to_string(),
    product_version,
    build_version,
  }
}

/// Reads `CFBundleShortVersionString` from `<bundle_path>/Contents/Info.plist`
/// via `defaults read` -- no bundle-parsing dependency needed.
pub fn probe_subject_version(bundle_path: &str) -> Option<String> {
  let info_plist_path = format!("{bundle_path}/Contents/Info");
  let output = run_capture("/usr/bin/defaults", &[
    "read",
    &info_plist_path,
    "CFBundleShortVersionString",
  ])
  .ok()?;
  if !output.success() {
    return None;
  }
  let version = output.stdout.trim();
  if version.is_empty() {
    None
  } else {
    Some(version.to_string())
  }
}

pub fn probe_login_shell_path(home: &str) -> String {
  run_capture("/usr/bin/dscl", &[".", "-read", home, "UserShell"])
    .ok()
    .and_then(|o| {
      o.stdout
        .trim()
        .strip_prefix("UserShell: ")
        .map(str::to_string)
    })
    .unwrap_or_else(|| "/bin/zsh".to_string())
}

pub fn probe_agent_cli_version(executable_path: &str) -> AgentCliVersion {
  let version = run_capture(executable_path, &["--version"])
    .map(|o| o.stdout.trim().to_string())
    .unwrap_or_else(|_| "unknown".to_string());
  AgentCliVersion {
    name: executable_path
      .rsplit('/')
      .next()
      .unwrap_or(executable_path)
      .to_string(),
    version,
    executable_path: executable_path.to_string(),
  }
}

pub fn harness_ref(commit: &str) -> HarnessRef {
  HarnessRef {
    // The public repo this harness ships from (documentnode/termtree,
    // spec item 5) -- not `termtree-app`, the private repo it was moved
    // out of. Every published result's provenance must name the repo a
    // reader can actually go clone.
    repo: "termtree".to_string(),
    commit: commit.to_string(),
    crate_version: env!("CARGO_PKG_VERSION").to_string(),
  }
}

/// `/bin/date -u +%Y-%m-%dT%H:%M:%SZ` -- the one ISO timestamp source this
/// crate uses instead of a `chrono`/`time` dependency (design §4.3).
pub fn iso_timestamp_now() -> String {
  run_capture("/bin/date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
    .map(|o| o.stdout.trim().to_string())
    .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn iso_timestamp_has_the_expected_shape() {
    let timestamp = iso_timestamp_now();
    assert_eq!(timestamp.len(), 20, "{timestamp}");
    assert!(timestamp.ends_with('Z'), "{timestamp}");
    assert!(timestamp.contains('T'), "{timestamp}");
  }

  #[test]
  fn harness_ref_carries_the_crate_version() {
    let reference = harness_ref("deadbeef");
    assert_eq!(reference.commit, "deadbeef");
    assert_eq!(reference.repo, "termtree");
    assert!(!reference.crate_version.is_empty());
  }

  #[test]
  fn missing_bundle_returns_none_not_a_placeholder_version() {
    assert_eq!(
      probe_subject_version("/Applications/Definitely Not Installed.app"),
      None
    );
  }
}
