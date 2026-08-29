//! The versioned result schema (design §6) and its crash-safe writer.
//!
//! `schemaVersion: 1`. One file per run at `results/<runId>.json`, rewritten
//! in full after every sample (design §5.8 step 8) so an interrupted sweep
//! loses at most one sample.

use crate::settings::RunSettings;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultFile {
  pub schema_version: u32,
  pub run_id: String,
  pub run_timestamp: String,
  pub harness_ref: HarnessRef,
  pub machine_spec: MachineSpec,
  pub os_build: OsBuild,
  pub agent_cli_version: AgentCliVersion,
  pub repo_ref: RepoRef,
  pub login_shell_path: String,
  pub settings: RunSettings,
  pub subjects: Vec<SubjectProvenance>,
  pub quiesce: RunQuiesce,
  pub samples: Vec<Sample>,
  pub aggregates: Vec<Aggregate>,
  pub fairness_review: FairnessReview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRef {
  pub repo: String,
  pub commit: String,
  pub crate_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MachineSpec {
  pub cpu_brand: String,
  pub logical_cores: u32,
  pub physical_cores: u32,
  pub ram_bytes: u64,
  pub page_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OsBuild {
  pub product_name: String,
  pub product_version: String,
  pub build_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCliVersion {
  pub name: String,
  pub version: String,
  pub executable_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RepoRef {
  pub url: String,
  pub commit: String,
  pub local_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubjectProvenance {
  pub subject_id: String,
  pub display_name: String,
  pub subject_version: String,
  pub runtime_family: String,
  pub bundle_identifier: String,
  pub bundle_path: String,
  pub optional: bool,
  pub seeder: String,
  pub seed_method: String,
  pub calibrated_main_window_area_pt: Option<f64>,
  pub version_drift_accepted: bool,
  /// Whether this subject's seeder format has been confirmed against a
  /// real install (spec item 4). `false` for Collaborator, CodeNomad, and
  /// diri -- their canvas/config-file/CLI-flag formats have never been
  /// checked against a real install (see each `seeding/*.rs` module doc).
  /// Every N-session/sustained-use sample for a subject with
  /// `seedFormatVerified: false` reports
  /// `invalidReason: "seed-format-unverified"` until this is flipped to
  /// `true` after verification.
  pub seed_format_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunQuiesce {
  pub pre_run: crate::quiesce::QuiesceReading,
  pub verdict: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FairnessReview {
  pub reviewer: Option<String>,
  pub reviewed_at: Option<String>,
  pub verdict: Option<String>,
  pub notes: Option<String>,
}

// --- Sample -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
  pub sample_id: String,
  pub subject_id: String,
  pub tier: String,
  pub session_count: u32,
  pub repetition: u32,
  pub is_calibration: bool,
  pub sampled_at: String,
  pub is_valid: bool,
  pub invalid_reason: Option<String>,

  pub attribution: Option<AttributionRecord>,
  pub memory: Option<MemoryRecord>,
  pub cold_start: Option<ColdStartRecord>,
  pub idle_cpu: Option<IdleCpuRecord>,

  pub quiesce: Option<crate::quiesce::QuiesceReading>,
  pub warm_helper_count: Option<u32>,
  pub helper_kill_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttributionRecord {
  pub main_pid: u32,
  pub launch_services_pids: Vec<u32>,
  pub process_tree_pids: Vec<u32>,
  pub vanished_pids: Vec<u32>,
  pub orchestrator_pids: Vec<u32>,
  pub agent_cli_pids: Vec<u32>,
  pub processes: Vec<AttributedProcessRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttributedProcessRecord {
  pub pid: u32,
  pub name: String,
  pub executable_path: Option<String>,
  pub discovered_by: String,
  pub role: String,
  pub phys_footprint_bytes: Option<u64>,
  pub rss_bytes: Option<u64>,
}

/// Field names below intentionally never merge `memPhysFootprintBytes`
/// (footprint's set-level `total footprint`, shared pages counted once) with
/// `memRssBytes` (naive per-process sum, diverges materially under memory
/// compression) under one unlabeled figure -- spec FR-3, design §5.3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
  pub mem_phys_footprint_bytes: u64,
  pub mem_phys_footprint_process_sum_bytes: u64,
  pub shared_page_double_count_bytes: u64,
  pub cross_partition_shared_bytes: u64,
  pub mem_rss_bytes: u64,
  pub mem_rss_method: String,

  pub orchestrator_attributable_bytes: u64,
  pub agent_cli_attributable_bytes: u64,

  pub core_process_bytes: Option<u64>,
  pub render_helper_bytes: Option<u64>,

  pub free_ram_before_bytes: u64,
  pub free_ram_after_bytes: u64,
  pub free_ram_delta_bytes: i64,
  pub free_ram_delta_sign: String,

  pub host_memory_used_before_bytes: u64,
  pub host_memory_used_after_bytes: u64,
  pub host_memory_used_delta_bytes: i64,
  pub compressor_occupied_delta_bytes: i64,
  pub swapouts_delta: u64,

  pub orchestrator_free_ram_delta_bytes: Option<i64>,
  pub agent_cli_free_ram_delta_bytes: Option<i64>,
  pub free_ram_split_derivation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ColdStartRecord {
  pub first_window_visible_ms: Option<u64>,
  pub main_window_visible_ms: Option<u64>,
  pub app_window_ready_ms: Option<u64>,
  pub splash_close_ms: Option<u64>,
  pub mark_source: std::collections::BTreeMap<String, String>,
  pub mark_resolution_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdleCpuRecord {
  pub idle_cpu_percent_of_one_core_median: f64,
  pub idle_cpu_percent_of_one_core_iqr: f64,
  pub sample_count: u32,
  pub window_state: String,
}

// --- Aggregate ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
  pub subject_id: String,
  pub tier: String,
  pub metric: String,
  pub median: f64,
  pub q1: f64,
  pub q3: f64,
  pub iqr: f64,
  pub n: u32,
  pub discarded_count: u32,
  pub discarded_reasons: std::collections::BTreeMap<String, u32>,
  pub derivation: String,
}

/// Reasons a sample is retained but excluded from the aggregate statistic
/// (spec FR-12's three, plus this design's additions -- §6.2).
pub mod invalid_reason {
  pub const SPLASH_TIMEOUT: &str = "splash-timeout";
  pub const WARM_WEBVIEW: &str = "warm-webview";
  pub const QUIESCE_VIOLATION: &str = "quiesce-violation";
  pub const ATTRIBUTION_INCOMPLETE: &str = "attribution-incomplete";
  pub const FOOTPRINT_PID_MISMATCH: &str = "footprint-pid-mismatch";
  pub const SEED_INCOMPLETE: &str = "seed-incomplete";
  pub const CALIBRATION_DISCARD: &str = "calibration-discard";
  /// Spec item 4: this subject's seed format has never been checked
  /// against a real install (`SubjectProvenance.seed_format_verified`),
  /// so any N-session/sustained-use sample it produces is reported
  /// unverified rather than valid, even if the generic session-readiness
  /// check happened to pass.
  pub const SEED_FORMAT_UNVERIFIED: &str = "seed-format-unverified";
  /// Spec item 4: none of this crate's hardcoded `karijini.log` mark
  /// strings (`log_marks.rs`) matched any line the harness actually read
  /// during the settle window, even though the log did advance -- the
  /// TermTree build under test has likely changed its log message text
  /// and `log_marks.rs` needs updating, rather than the run silently
  /// reporting `null` cold-start marks.
  pub const TERMTREE_LOG_MARKS_UNRECOGNIZED: &str =
    "termtree-log-marks-unrecognized";
  /// Spec item 4: TermTree launched (a main pid was discovered) but never
  /// created `DocumentNode/TermTree` under the scratch home -- the app's
  /// data-directory convention has likely changed and
  /// `seeding/termtree.rs` / `log_marks.rs`'s hardcoded path needs
  /// updating, rather than the run silently measuring the wrong directory.
  pub const APP_DATA_DIR_NOT_CREATED: &str = "app-data-dir-not-created";
  /// Spec item 3: a live LaunchServices entry already carried this
  /// subject's bundle identifier before the harness tried to seed/launch
  /// it -- a single-instance plugin would have handed the launch off and
  /// exited within seconds, measuring nothing.
  pub const SUBJECT_ALREADY_RUNNING: &str = "subject-already-running";
}

/// Rewrite the whole result file, atomically, after every sample (design
/// §5.8 step 8). A partial write on crash/power-loss can never leave a
/// corrupt result file behind, since the temp file is renamed into place
/// only after a full successful write.
pub fn write_result_file(path: &Path, result: &ResultFile) -> io::Result<()> {
  let json = serde_json::to_string_pretty(result)
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
  let tmp_path = path.with_extension("json.tmp");
  fs::write(&tmp_path, json)?;
  fs::rename(&tmp_path, path)
}

pub fn read_result_file(path: &Path) -> io::Result<ResultFile> {
  let text = fs::read_to_string(path)?;
  serde_json::from_str(&text)
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::quiesce::QuiesceReading;
  use crate::settings::RunSettings;

  fn sample_result_file() -> ResultFile {
    ResultFile {
      schema_version: SCHEMA_VERSION,
      run_id: "2026-08-25T02-10-44Z-9f3c1a7e".into(),
      run_timestamp: "2026-08-25T02:10:44Z".into(),
      harness_ref: HarnessRef {
        repo: "termtree".into(),
        commit: "f2ce896".into(),
        crate_version: "0.1.0".into(),
      },
      machine_spec: MachineSpec {
        cpu_brand: "Apple M1".into(),
        logical_cores: 8,
        physical_cores: 8,
        ram_bytes: 17_179_869_184,
        page_size_bytes: 16384,
      },
      os_build: OsBuild {
        product_name: "macOS".into(),
        product_version: "15.7.4".into(),
        build_version: "24G517".into(),
      },
      agent_cli_version: AgentCliVersion {
        name: "claude".into(),
        version: "1.0.0".into(),
        executable_path: "/Users/dev/.local/bin/claude".into(),
      },
      repo_ref: RepoRef {
        url: "https://github.com/example/benchmark-repo".into(),
        commit: "abc1234".into(),
        local_path: "/Users/Shared/benchmark-repo".into(),
      },
      login_shell_path: "/bin/zsh".into(),
      settings: RunSettings::default(),
      subjects: vec![],
      quiesce: RunQuiesce {
        pre_run: QuiesceReading::nominal_for_test(),
        verdict: "pass".into(),
      },
      samples: vec![],
      aggregates: vec![],
      fairness_review: FairnessReview::default(),
    }
  }

  #[test]
  fn schema_round_trips_through_json() {
    let result = sample_result_file();
    let json = serde_json::to_string(&result).unwrap();
    let parsed: ResultFile = serde_json::from_str(&json).unwrap();
    assert_eq!(result, parsed);
  }

  #[test]
  fn write_then_read_round_trips_and_is_atomic() {
    let dir = std::env::temp_dir().join(format!(
      "resource-benchmark-result-test-{}",
      std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("run.json");
    let result = sample_result_file();
    write_result_file(&path, &result).unwrap();
    assert!(!path.with_extension("json.tmp").exists());
    let read_back = read_result_file(&path).unwrap();
    assert_eq!(result, read_back);
    fs::remove_dir_all(&dir).unwrap();
  }
}
