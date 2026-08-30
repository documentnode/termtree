//! `RunOrchestrator` (spec FR-6, FR-12, design §5.8): repetitions,
//! calibration launch, warm-helper check, teardown, invalid-sample
//! handling, and crash-safe append.
//!
//! Sequencing, tier expansion, repetition accounting, resume-state
//! reconciliation, validity classification, and record assembly are all
//! pure functions in this module, each unit-tested below. Only the actual
//! spawn-and-wait glue in [`RunOrchestrator::run`] and its private
//! `measure_one`/`measure_cold_start`/`teardown` methods is live,
//! untestable orchestration (design §11: "not unit-tested, by design:
//! launching subjects ... live `footprint`/`vm_stat` invocation") --
//! everything it decides, it decides by calling one of the pure functions
//! below.

use crate::attribution::{self, AttributableProcessSet, DiscoverySource};
use crate::bundle_paths::{self, BundlePathOverrides};
use crate::cold_start;
use crate::cpu_sampler;
use crate::exec::run_capture;
use crate::footprint::{self, FootprintReport};
use crate::host_memory::{self, HostMemorySample};
use crate::launch_services::{self, LaunchServicesEntry};
use crate::log_marks::{self, LogMark};
use crate::process_tree::{self, ProcessRecord};
use crate::provenance;
use crate::quiesce::{self, QuiesceReading, QuiesceVerdict};
use crate::result::{
  self, invalid_reason, AttributedProcessRecord, AttributionRecord,
  ColdStartRecord, FairnessReview, IdleCpuRecord, MemoryRecord, ResultFile,
  RunQuiesce, Sample, SubjectProvenance,
};
use crate::seeding::{self, AgentCliPin, SeededRepo};
use crate::settings::{RunSettings, TierRepetitions};
use crate::stats;
use crate::subject::{self, RuntimeFamily, SubjectSpec, SUBJECTS};
use crate::tier::Tier;
use crate::window_probe;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// A subject is **warm** (not eligible for a fresh-launch sample, spec
/// FR-6) if any of its helper processes are still resident from a prior
/// run. For `WebKitTauri` subjects that means a LaunchServices entry under
/// the subject's display name with a bundle ID in `helper_bundle_ids`; for
/// `ChromiumElectron` subjects it means a resident process whose
/// executable path is under `bundle_path` (checked via `process_tree.rs`
/// upstream of this function -- passed in as `chromium_helper_present`).
pub fn has_warm_helpers(
  subject: &SubjectSpec,
  ls_entries: &[LaunchServicesEntry],
  chromium_helper_present: bool,
) -> bool {
  match subject.runtime_family {
    RuntimeFamily::WebKitTauri => {
      let prefix = format!("{} ", subject.launch_services_name);
      ls_entries.iter().any(|entry| {
        entry.pid.is_some()
          && entry.display_name.starts_with(&prefix)
          && entry
            .bundle_identifier
            .as_deref()
            .is_some_and(|id| subject.helper_bundle_ids.contains(&id))
      })
    }
    RuntimeFamily::ChromiumElectron | RuntimeFamily::GpuiNative => {
      chromium_helper_present
    }
  }
}

/// The invalid reason a sample should carry, in priority order, given the
/// signals the orchestrator collected for it (design §5.8, §9). Pure so the
/// decision logic itself is testable even though the signals it reads come
/// from live measurement.
#[allow(clippy::too_many_arguments)]
pub fn classify_sample_invalidity(
  is_calibration: bool,
  subject_already_running: bool,
  warm_helper_count: u32,
  splash_timeout_seen: bool,
  quiesce_violation_seen: bool,
  attribution_incomplete: bool,
  footprint_pid_mismatch: bool,
  seed_incomplete: bool,
  seed_format_unverified: bool,
  log_marks_unrecognized: bool,
  app_data_dir_missing: bool,
) -> Option<&'static str> {
  if is_calibration {
    return Some(invalid_reason::CALIBRATION_DISCARD);
  }
  if subject_already_running {
    return Some(invalid_reason::SUBJECT_ALREADY_RUNNING);
  }
  if seed_incomplete {
    return Some(invalid_reason::SEED_INCOMPLETE);
  }
  if seed_format_unverified {
    return Some(invalid_reason::SEED_FORMAT_UNVERIFIED);
  }
  if app_data_dir_missing {
    return Some(invalid_reason::APP_DATA_DIR_NOT_CREATED);
  }
  if log_marks_unrecognized {
    return Some(invalid_reason::TERMTREE_LOG_MARKS_UNRECOGNIZED);
  }
  if warm_helper_count > 0 {
    return Some(invalid_reason::WARM_WEBVIEW);
  }
  if splash_timeout_seen {
    return Some(invalid_reason::SPLASH_TIMEOUT);
  }
  if attribution_incomplete {
    return Some(invalid_reason::ATTRIBUTION_INCOMPLETE);
  }
  if footprint_pid_mismatch {
    return Some(invalid_reason::FOOTPRINT_PID_MISMATCH);
  }
  if quiesce_violation_seen {
    return Some(invalid_reason::QUIESCE_VIOLATION);
  }
  None
}

// --- Refusals (spec: an unimplemented or refused run must never exit 0) --

/// Why a run refused to start, or refused to include a subject. Every
/// variant carries what a re-runner needs to act on it -- `main.rs` prints
/// [`std::fmt::Display`] to stderr and exits non-zero.
#[derive(Debug)]
pub enum RunRefusal {
  QuiesceGateFailed(Vec<String>),
  UnknownSubject(String),
  SubjectNotInstalled {
    display_name: String,
    bundle_path: String,
  },
  VersionDrift {
    display_name: String,
    expected: String,
    found: String,
  },
  SubjectVersionUnprobeable {
    display_name: String,
    bundle_path: String,
  },
  NoSubjectsSelected,
  ResumeFileUnreadable {
    path: String,
    message: String,
  },
  SubjectAlreadyRunning {
    display_name: String,
    bundle_identifier: String,
  },
  OpenEnvFlagUnsupported,
}

impl std::fmt::Display for RunRefusal {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::QuiesceGateFailed(signals) => write!(
        f,
        "quiesce gate failed, refusing to start: {}",
        signals.join(", ")
      ),
      Self::UnknownSubject(id) => write!(f, "unknown subject: {id}"),
      Self::SubjectNotInstalled {
        display_name,
        bundle_path,
      } => write!(f, "{display_name} is not installed at {bundle_path}"),
      Self::VersionDrift {
        display_name,
        expected,
        found,
      } => write!(
        f,
        "{display_name}: version drift, expected {expected} found {found} \
         (pass --allow-version-drift to proceed anyway)"
      ),
      Self::SubjectVersionUnprobeable {
        display_name,
        bundle_path,
      } => write!(
        f,
        "{display_name}: could not probe installed version at {bundle_path}"
      ),
      Self::NoSubjectsSelected => {
        write!(f, "no subjects selected for this run")
      }
      Self::ResumeFileUnreadable { path, message } => {
        write!(f, "--resume {path}: {message}")
      }
      Self::SubjectAlreadyRunning {
        display_name,
        bundle_identifier,
      } => write!(
        f,
        "{display_name} ({bundle_identifier}) is already running -- quit \
         it before running the benchmark (a same-bundle-identifier \
         instance would silently absorb the launch and exit, measuring \
         nothing)"
      ),
      Self::OpenEnvFlagUnsupported => write!(
        f,
        "/usr/bin/open on this machine does not document --env; this \
         harness requires it to launch every subject with an isolated \
         scratch HOME (see this crate's README, \"Prerequisites\")"
      ),
    }
  }
}

impl std::error::Error for RunRefusal {}

/// The subject registry entries a run should measure: every `--subjects`
/// id resolved against [`subject::find`], or (with no override) every
/// non-optional subject plus every optional one when
/// `allow_optional_subjects` is set (spec FR-1's "diri ... marked optional
/// ... unless --allow-optional-subjects"). Pure -- installed-ness and
/// version drift are checked live, downstream of this selection.
pub fn select_subjects(
  requested_ids: Option<&[String]>,
  allow_optional_subjects: bool,
) -> Result<Vec<&'static SubjectSpec>, RunRefusal> {
  let selected: Vec<&'static SubjectSpec> = match requested_ids {
    Some(ids) => {
      let mut specs = Vec::with_capacity(ids.len());
      for id in ids {
        let spec = subject::find(id)
          .ok_or_else(|| RunRefusal::UnknownSubject(id.clone()))?;
        specs.push(spec);
      }
      specs
    }
    None => SUBJECTS
      .iter()
      .filter(|s| !s.optional || allow_optional_subjects)
      .collect(),
  };
  if selected.is_empty() {
    return Err(RunRefusal::NoSubjectsSelected);
  }
  Ok(selected)
}

/// The first selected subject whose **bundle identifier** already has a
/// live LaunchServices entry (spec item 3) -- keyed on bundle identifier,
/// not display name or app name, because two differently named bundles
/// can declare the same identifier, and a single-instance plugin then
/// hands a new launch off to the already-running instance, which exits
/// within seconds. Left undetected, the harness would seed a profile,
/// launch, measure nothing, and report an invalid tier for a reason
/// nobody could diagnose -- `open -n` does not bypass this, because `-n`
/// only tells LaunchServices to spawn a new process; it says nothing about
/// the app's own single-instance guard. Pure over an already-captured
/// `lsappinfo list`, so the matching rule is unit-tested independent of
/// the live invocation that feeds it.
pub fn find_already_running_subject<'a>(
  subjects: &[&'a SubjectSpec],
  ls_entries: &[LaunchServicesEntry],
) -> Option<&'a SubjectSpec> {
  subjects.iter().copied().find(|subject| {
    ls_entries.iter().any(|entry| {
      entry.pid.is_some()
        && entry.bundle_identifier.as_deref() == Some(subject.bundle_identifier)
    })
  })
}

/// `RunSettings::default()` with `--repetitions N` applied uniformly to
/// every tier's count when given -- the CLI surface has one repetition
/// flag (spec §5.9), not a per-tier one, so an override sets all three
/// (this is what the design's smoke sweep, `--repetitions 2`, relies on).
pub fn build_settings(repetitions_override: Option<u32>) -> RunSettings {
  let mut settings = RunSettings::default();
  if let Some(n) = repetitions_override {
    settings.repetitions = TierRepetitions {
      fresh_launch: n,
      sustained_use: n,
      n_session: n,
    };
  }
  settings
}

// --- Repetition planning (tier expansion + resume reconciliation) -------

/// One planned (subject, tier, repetition) launch. `repetition == 0` is
/// always the calibration launch (spec FR-12's mandatory first-run
/// discard, design §5.4's calibration launch) -- [`plan_repetitions`]
/// includes it, on top of the tier's disclosed repetition count.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRepetition {
  pub subject_id: String,
  pub tier: Tier,
  pub repetition: u32,
  pub is_calibration: bool,
  pub sample_id: String,
}

/// The `sampleId` a repetition's [`Sample`] carries, e.g.
/// `"termtree/fresh-launch/003"` (design §6.2) -- computed once here so
/// every planner, resumer, and writer agrees on the same identifier.
pub fn sample_id(subject_id: &str, tier: Tier, repetition: u32) -> String {
  format!("{subject_id}/{}/{repetition:03}", tier.as_str())
}

/// Expands every (subject, tier) pair into its full repetition list --
/// `0..=tier.repetitions(settings)`, i.e. the disclosed count plus the
/// mandatory calibration launch at repetition 0 (design §5.8's `for
/// repetition in 0..=settings.repetitions`, spec FR-12).
pub fn plan_repetitions(
  subjects: &[&SubjectSpec],
  tiers: &[Tier],
  settings: &RunSettings,
) -> Vec<PlannedRepetition> {
  let mut plan = Vec::new();
  for subject in subjects {
    for &tier in tiers {
      let repetitions = tier.repetitions(settings);
      for repetition in 0..=repetitions {
        plan.push(PlannedRepetition {
          subject_id: subject.id.to_string(),
          tier,
          repetition,
          is_calibration: repetition == 0,
          sample_id: sample_id(subject.id, tier, repetition),
        });
      }
    }
  }
  plan
}

/// `--resume <file>`'s reconciliation step (design §5.8): drops every
/// planned repetition whose `sampleId` already has a sample in the
/// resumed file, so a re-invocation after a crash or interrupt continues
/// where it left off instead of re-measuring (or re-seeding, re-launching)
/// work that already landed safely on disk.
pub fn pending_repetitions(
  planned: &[PlannedRepetition],
  completed_sample_ids: &HashSet<String>,
) -> Vec<PlannedRepetition> {
  planned
    .iter()
    .filter(|p| !completed_sample_ids.contains(&p.sample_id))
    .cloned()
    .collect()
}

pub fn completed_sample_ids(result: &ResultFile) -> HashSet<String> {
  result.samples.iter().map(|s| s.sample_id.clone()).collect()
}

/// How many times to re-check the quiesce gate during one repetition's
/// settle/measurement window (spec FR-10: "re-read ... at the N-session
/// tiers, every 30 s during measurement") -- zero for tiers/durations
/// shorter than the interval, so a fresh-launch's 15 s settle does not
/// spuriously re-check.
pub fn quiesce_recheck_count(duration_s: u64, interval_s: u64) -> u64 {
  duration_s.checked_div(interval_s).unwrap_or(0)
}

// --- Warm-helper ownership (also used by teardown) -----------------------

/// Whether any process whose executable path is under `bundle_path`, other
/// than `main_pid` itself, is currently resident (design §5.8 step 2's
/// Chromium/Electron half of the warm-helper check, and the signal
/// [`has_warm_helpers`] takes as `chromium_helper_present`).
pub fn chromium_helper_resident(
  tree_records: &[ProcessRecord],
  main_pid: u32,
  bundle_path: &str,
) -> bool {
  tree_records.iter().any(|record| {
    record.pid != main_pid
      && record
        .executable_path
        .as_deref()
        .is_some_and(|path| path.starts_with(bundle_path))
  })
}

/// The PIDs of a `WebKitTauri` subject's own helper processes, by the same
/// ownership test [`has_warm_helpers`] uses to detect warmth. Teardown
/// kills only these PIDs (design §5.8 step 9, design §9: "a shared-name
/// helper owned by a different app is never touched").
pub fn owned_webkit_helper_pids(
  subject: &SubjectSpec,
  ls_entries: &[LaunchServicesEntry],
) -> Vec<u32> {
  let prefix = format!("{} ", subject.launch_services_name);
  ls_entries
    .iter()
    .filter(|entry| {
      entry.display_name.starts_with(&prefix)
        && entry
          .bundle_identifier
          .as_deref()
          .is_some_and(|id| subject.helper_bundle_ids.contains(&id))
    })
    .filter_map(|entry| entry.pid)
    .collect()
}

// --- Record assembly (pure: takes already-invoked/parsed results) --------

/// Builds the [`AttributionRecord`] published on a [`Sample`] from an
/// already-resolved [`AttributableProcessSet`] and the union `footprint`
/// invocation's report -- pure, so the union/partition/footprint-merge
/// logic stays testable even though resolving and invoking are live.
pub fn build_attribution_record(
  set: &AttributableProcessSet,
  vanished_pids: &[u32],
  union_report: &FootprintReport,
) -> AttributionRecord {
  let processes = set
    .processes
    .iter()
    .map(|p| {
      let footprint_row =
        union_report.processes.iter().find(|fp| fp.pid == p.pid);
      AttributedProcessRecord {
        pid: p.pid,
        name: p.name.clone(),
        executable_path: p.executable_path.clone(),
        discovered_by: p.discovered_by.as_str().to_string(),
        role: p.role.as_str().to_string(),
        phys_footprint_bytes: footprint_row.map(|fp| fp.footprint_bytes),
        rss_bytes: None,
      }
    })
    .collect();
  let launch_services_pids = set
    .processes
    .iter()
    .filter(|p| {
      matches!(
        p.discovered_by,
        DiscoverySource::LaunchServices | DiscoverySource::Both
      )
    })
    .map(|p| p.pid)
    .collect();
  let process_tree_pids = set
    .processes
    .iter()
    .filter(|p| {
      matches!(
        p.discovered_by,
        DiscoverySource::ProcessTree | DiscoverySource::Both
      )
    })
    .map(|p| p.pid)
    .collect();
  AttributionRecord {
    main_pid: set.main_pid,
    launch_services_pids,
    process_tree_pids,
    vanished_pids: vanished_pids.to_vec(),
    orchestrator_pids: set.orchestrator_pids(),
    agent_cli_pids: set.agent_cli_pids(),
    processes,
  }
}

/// Builds the [`MemoryRecord`] published on a [`Sample`] from the three
/// `footprint` invocations (union/orchestrator/session, design §5.3.1's
/// table) and the before/after `vm_stat` samples (design §5.3.2) -- pure,
/// so the derived-quantity formulas (the free-RAM sign convention, the
/// shared-page double count, the cross-partition overlap) are unit-tested
/// independent of the live invocations that feed them.
#[allow(clippy::too_many_arguments)]
pub fn build_memory_record(
  union_report: &FootprintReport,
  orchestrator_report: &FootprintReport,
  session_report: &FootprintReport,
  mem_rss_bytes: u64,
  core_process_bytes: Option<u64>,
  render_helper_bytes: Option<u64>,
  before: &HostMemorySample,
  after: &HostMemorySample,
) -> MemoryRecord {
  let free_before = before.free_ram_bytes();
  let free_after = after.free_ram_bytes();
  let used_before = before.host_memory_used_bytes();
  let used_after = after.host_memory_used_bytes();
  // orchestrator + session >= union whenever the two partitions share
  // pages; the excess over the union total is published rather than
  // hidden by forcing the parts to sum (design §5.3.1).
  let cross_partition_shared_bytes = orchestrator_report
    .total_footprint_bytes
    .saturating_add(session_report.total_footprint_bytes)
    .saturating_sub(union_report.total_footprint_bytes);

  MemoryRecord {
    mem_phys_footprint_bytes: union_report.total_footprint_bytes,
    mem_phys_footprint_process_sum_bytes: footprint::process_sum_bytes(
      union_report,
    ),
    shared_page_double_count_bytes: footprint::shared_page_double_count_bytes(
      union_report,
    ),
    cross_partition_shared_bytes,
    mem_rss_bytes,
    mem_rss_method: "naive-per-process-sum".to_string(),
    orchestrator_attributable_bytes: orchestrator_report.total_footprint_bytes,
    agent_cli_attributable_bytes: session_report.total_footprint_bytes,
    core_process_bytes,
    render_helper_bytes,
    free_ram_before_bytes: free_before,
    free_ram_after_bytes: free_after,
    // Sign convention: positive means the subject consumed RAM (design
    // §5.3.2) -- `before - after`, not `after - before`.
    free_ram_delta_bytes: free_before as i64 - free_after as i64,
    free_ram_delta_sign: "positive-means-consumed".to_string(),
    host_memory_used_before_bytes: used_before,
    host_memory_used_after_bytes: used_after,
    host_memory_used_delta_bytes: used_after as i64 - used_before as i64,
    compressor_occupied_delta_bytes: after.compressor_occupied_bytes() as i64
      - before.compressor_occupied_bytes() as i64,
    swapouts_delta: after.swapouts().saturating_sub(before.swapouts()),
    // The free-RAM split at N-session tiers is derived cross-tier from
    // this same subject's fresh-launch median (design §5.3.2) -- left
    // unset here; a post-processing pass over the written result file is
    // required to fill it in once a fresh-launch aggregate exists, which
    // is out of scope for this pass (see final report).
    orchestrator_free_ram_delta_bytes: None,
    agent_cli_free_ram_delta_bytes: None,
    free_ram_split_derivation: None,
  }
}

pub fn build_cold_start_record(
  first_window_visible_ms: Option<u64>,
  main_window_visible_ms: Option<u64>,
  app_window_ready_ms: Option<u64>,
  splash_close_ms: Option<u64>,
  mark_resolution_ms: u64,
) -> ColdStartRecord {
  let mut mark_source = std::collections::BTreeMap::new();
  if first_window_visible_ms.is_some() {
    mark_source.insert(
      "firstWindowVisibleMs".to_string(),
      "cg-window-list".to_string(),
    );
  }
  if main_window_visible_ms.is_some() {
    mark_source.insert(
      "mainWindowVisibleMs".to_string(),
      "cg-window-list".to_string(),
    );
  }
  if app_window_ready_ms.is_some() {
    mark_source.insert(
      "appWindowReadyMs".to_string(),
      "karijini-log-arrival".to_string(),
    );
  }
  if splash_close_ms.is_some() {
    mark_source.insert(
      "splashCloseMs".to_string(),
      "karijini-log-arrival".to_string(),
    );
  }
  ColdStartRecord {
    first_window_visible_ms,
    main_window_visible_ms,
    app_window_ready_ms,
    splash_close_ms,
    mark_source,
    mark_resolution_ms,
  }
}

pub fn build_idle_cpu_record(
  summary: &stats::Summary,
  window_state: &str,
) -> IdleCpuRecord {
  IdleCpuRecord {
    idle_cpu_percent_of_one_core_median: summary.median,
    idle_cpu_percent_of_one_core_iqr: summary.iqr,
    sample_count: summary.n,
    window_state: window_state.to_string(),
  }
}

// --- Live orchestration (design §5.8, §7) --------------------------------
//
// Everything below this line spawns a process, polls the window server, or
// sleeps a wall-clock duration, and is therefore the crate's one
// deliberately untested surface (design §11). It decides nothing on its
// own -- every branch calls one of the pure functions above (plan,
// classify, build_*_record) or a thin `invoke_*`/parse pair owned by
// another module. See the final report for exactly which lines these are.

/// Set by [`install_interrupt_handler`]'s `SIGINT` handler; polled between
/// repetitions so an interrupted sweep restores every seeder's backed-up
/// state (most importantly TermTree's production `state.json`, design
/// §5.6.1) before exiting, rather than abandoning it mid-seed.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

type RawSignalHandler = extern "C" fn(i32);
const SIGINT: i32 = 2;

extern "C" {
  fn signal(signum: i32, handler: RawSignalHandler) -> usize;
}

extern "C" fn handle_interrupt(_signum: i32) {
  INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Installs the `SIGINT` handler above. `main.rs` calls this once before
/// [`RunOrchestrator::run`]. Idempotent.
pub fn install_interrupt_handler() {
  // SAFETY: `signal` is libSystem's standard POSIX `signal(2)`, always
  // linked into a macOS Rust binary; `handle_interrupt` matches its
  // required `extern "C" fn(i32)` signature and does nothing but store to
  // an atomic, which is signal-safe.
  unsafe {
    signal(SIGINT, handle_interrupt);
  }
}

fn interrupted() -> bool {
  INTERRUPTED.load(Ordering::SeqCst)
}

/// Restores every seeder's backed-up state, best-effort (design §5.6.1,
/// §9's "the sweep is interrupted"). Run at the end of every sweep and
/// again if [`interrupted`] fires mid-sweep; `resource-benchmark restore`
/// is the same operation available as a manual escape hatch.
fn restore_all_seeders(home: &Path, subjects: &[&SubjectSpec]) {
  for subject in subjects {
    if let Err(error) = seeding::restore_subject(home, subject.id) {
      eprintln!("warning: restore {} failed: {error}", subject.id);
    }
  }
}

/// One measured sample's cold-start half (design §5.4) -- the launch, PID
/// discovery, and concurrent window/log polling this crate cannot unit
/// test, packaged so [`RunOrchestrator::measure_one`] can hand its result
/// straight to the pure [`build_cold_start_record`].
struct ColdStartOutcome {
  first_window_visible_ms: Option<u64>,
  main_window_visible_ms: Option<u64>,
  app_window_ready_ms: Option<u64>,
  splash_close_ms: Option<u64>,
  splash_timeout_seen: bool,
  main_pid: Option<u32>,
  /// The largest layer-0 window area observed this launch -- becomes
  /// `calibratedMainWindowAreaPt` when this is the calibration launch
  /// (design §5.4 step 3).
  observed_main_window_area_pt: Option<f64>,
  /// Spec item 4: `karijini.log` advanced during this launch but none of
  /// `log_marks.rs`'s hardcoded messages matched -- a drift signal, not the
  /// legitimate "log rolled/shrank" case this module already handles by
  /// reopening from the start.
  log_marks_unrecognized: bool,
}

fn invoke_and_parse_footprint(
  output_path: &Path,
  pids: &[u32],
) -> Option<FootprintReport> {
  if pids.is_empty() {
    return Some(FootprintReport {
      total_footprint_bytes: 0,
      processes: Vec::new(),
      shared: Vec::new(),
      errors: Vec::new(),
      warnings: Vec::new(),
      page_size_bytes: 0,
      start_time_iso: None,
    });
  }
  footprint::invoke_footprint(&output_path.to_string_lossy(), pids).ok()?;
  let text = std::fs::read_to_string(output_path).ok()?;
  let _ = std::fs::remove_file(output_path);
  footprint::parse_footprint_json(&text).ok()
}

pub struct RunOrchestrator {
  pub subjects: Vec<&'static SubjectSpec>,
  pub tiers: Vec<Tier>,
  pub settings: RunSettings,
  /// The disposable per-run scratch home (spec item 1) -- never the
  /// runner's real `$HOME`. Every seeder, the karijini log path, and the
  /// `HOME` a subject is launched with all key off this.
  pub home: PathBuf,
  pub repo: SeededRepo,
  pub agent: AgentCliPin,
  pub allow_version_drift: bool,
  pub out_path: PathBuf,
  /// Per-subject `/Applications/*.app` overrides (spec item 5), resolved
  /// once in `main.rs` from `--bundle-path`/`RESOURCE_BENCHMARK_BUNDLE_PATH_*`.
  pub bundle_path_overrides: BundlePathOverrides,
  /// The real logged-in user's login shell path (`dscl -read <real
  /// $HOME> UserShell`), used only to classify a process as a session
  /// root (design §5.2.3) -- resolved once in `main.rs` against the real
  /// `$HOME`, deliberately **not** the scratch home, since `dscl` needs an
  /// actual Directory Services record to read.
  pub login_shell_path: String,
}

impl RunOrchestrator {
  /// Builds a fresh [`ResultFile`] envelope (design §6.1): probes the
  /// machine spec, OS build, and every selected subject's installed
  /// version, refusing per subject on version drift unless
  /// `allow_version_drift` was passed (spec FR-1; this fix's "an
  /// unimplemented or refused run must never exit 0").
  fn build_envelope(
    &self,
    quiesce_reading: &QuiesceReading,
  ) -> Result<ResultFile, RunRefusal> {
    let mut subjects_provenance = Vec::with_capacity(self.subjects.len());
    for subject in &self.subjects {
      let bundle_path =
        bundle_paths::resolve(subject, &self.bundle_path_overrides);
      if !Path::new(bundle_path).exists() {
        if subject.optional {
          continue;
        }
        return Err(RunRefusal::SubjectNotInstalled {
          display_name: subject.display_name.to_string(),
          bundle_path: bundle_path.to_string(),
        });
      }
      let found_version = provenance::probe_subject_version(bundle_path)
        .ok_or_else(|| RunRefusal::SubjectVersionUnprobeable {
          display_name: subject.display_name.to_string(),
          bundle_path: bundle_path.to_string(),
        })?;
      let version_drift_accepted = found_version != subject.expected_version;
      if version_drift_accepted && !self.allow_version_drift {
        return Err(RunRefusal::VersionDrift {
          display_name: subject.display_name.to_string(),
          expected: subject.expected_version.to_string(),
          found: found_version,
        });
      }
      subjects_provenance.push(SubjectProvenance {
        subject_id: subject.id.to_string(),
        display_name: subject.display_name.to_string(),
        subject_version: found_version,
        runtime_family: subject.runtime_family.as_str().to_string(),
        bundle_identifier: subject.bundle_identifier.to_string(),
        bundle_path: bundle_path.to_string(),
        optional: subject.optional,
        seeder: format!("{:?}", subject.seeder),
        seed_method: String::new(),
        calibrated_main_window_area_pt: None,
        version_drift_accepted,
        seed_format_verified: subject.seed_format_verified,
      });
    }
    if subjects_provenance.is_empty() {
      return Err(RunRefusal::NoSubjectsSelected);
    }

    let machine_spec = provenance::probe_machine_spec();
    let os_build = provenance::probe_os_build();
    let run_timestamp = provenance::iso_timestamp_now();
    let run_id = format!(
      "{}-{:08x}",
      run_timestamp.replace(':', "-"),
      std::process::id()
    );

    Ok(ResultFile {
      schema_version: result::SCHEMA_VERSION,
      run_id,
      run_timestamp,
      harness_ref: provenance::harness_ref("unknown"),
      machine_spec,
      os_build,
      agent_cli_version: result::AgentCliVersion {
        name: self.agent.name.clone(),
        version: self.agent.version.clone(),
        executable_path: self.agent.executable_path.clone(),
      },
      repo_ref: result::RepoRef {
        url: self.repo.url.clone(),
        commit: self.repo.commit.clone(),
        local_path: self.repo.local_path.clone(),
      },
      login_shell_path: self.login_shell_path.clone(),
      settings: self.settings.clone(),
      subjects: subjects_provenance,
      quiesce: RunQuiesce {
        pre_run: quiesce_reading.clone(),
        verdict: if quiesce_reading.verdict == QuiesceVerdict::Pass {
          "pass".to_string()
        } else {
          "fail".to_string()
        },
      },
      samples: Vec::new(),
      aggregates: Vec::new(),
      fairness_review: FairnessReview::default(),
    })
  }

  /// The full sweep (design §5.8, §7): pre-run quiesce gate, envelope
  /// construction (or `--resume` reload), tier expansion, and then one
  /// `measure_one` + crash-safe append per pending repetition.
  pub fn run(
    &self,
    resume_path: Option<&Path>,
  ) -> Result<ResultFile, RunRefusal> {
    let quiesce_reading = quiesce::read_quiesce_gate(None);
    if quiesce_reading.verdict != QuiesceVerdict::Pass {
      return Err(RunRefusal::QuiesceGateFailed(
        quiesce_reading.failing_signals.clone(),
      ));
    }

    // Spec item 1: every subject is launched with `HOME` isolated via
    // `open --env`; refuse up front, loudly, rather than silently falling
    // back to the runner's real `$HOME` if this machine's `open` predates
    // it.
    if !cold_start::supports_env_flag() {
      return Err(RunRefusal::OpenEnvFlagUnsupported);
    }

    // Spec item 3: refuse before seeding/launching anything if any
    // selected subject's bundle identifier is already running.
    let ls_text = launch_services::invoke_lsappinfo_list().unwrap_or_default();
    let ls_entries = launch_services::parse_lsappinfo_list(&ls_text);
    if let Some(subject) =
      find_already_running_subject(&self.subjects, &ls_entries)
    {
      return Err(RunRefusal::SubjectAlreadyRunning {
        display_name: subject.display_name.to_string(),
        bundle_identifier: subject.bundle_identifier.to_string(),
      });
    }

    let out_path = resume_path
      .map(Path::to_path_buf)
      .unwrap_or_else(|| self.out_path.clone());

    let mut result = match resume_path {
      Some(path) => result::read_result_file(path).map_err(|error| {
        RunRefusal::ResumeFileUnreadable {
          path: path.display().to_string(),
          message: error.to_string(),
        }
      })?,
      None => self.build_envelope(&quiesce_reading)?,
    };

    if let Err(error) = result::write_result_file(&out_path, &result) {
      eprintln!("error writing {}: {error}", out_path.display());
    }

    let subjects_for_plan: Vec<&SubjectSpec> = result
      .subjects
      .iter()
      .filter_map(|sp| subject::find(&sp.subject_id))
      .collect();
    let plan =
      plan_repetitions(&subjects_for_plan, &self.tiers, &result.settings);
    let already_done = completed_sample_ids(&result);
    let pending = pending_repetitions(&plan, &already_done);

    let mut calibrated_areas: HashMap<String, f64> = result
      .subjects
      .iter()
      .filter_map(|sp| {
        sp.calibrated_main_window_area_pt
          .map(|area| (sp.subject_id.clone(), area))
      })
      .collect();
    let login_shell_path = result.login_shell_path.clone();

    for planned in &pending {
      if interrupted() {
        eprintln!(
          "resource-benchmark run: interrupted, restoring seeders and stopping"
        );
        break;
      }
      let Some(subject) = subjects_for_plan
        .iter()
        .find(|s| s.id == planned.subject_id)
      else {
        continue;
      };

      // Re-check the quiesce gate before every repetition, not only at
      // the start (spec FR-10) -- a multi-hour sweep can drift into
      // memory/thermal pressure the pre-run check never saw.
      let repetition_quiesce = quiesce::read_quiesce_gate(None);
      let quiesce_violation =
        repetition_quiesce.verdict != QuiesceVerdict::Pass;

      let sample = self.measure_one(
        subject,
        planned,
        &mut calibrated_areas,
        quiesce_violation,
        &repetition_quiesce,
        &login_shell_path,
      );

      if planned.is_calibration {
        if let Some(area) = calibrated_areas.get(&planned.subject_id) {
          if let Some(sp) = result
            .subjects
            .iter_mut()
            .find(|s| s.subject_id == planned.subject_id)
          {
            sp.calibrated_main_window_area_pt = Some(*area);
          }
        }
      }

      result.samples.push(sample);
      result.aggregates = stats::compute_aggregates(&result.samples);
      // Crash-safe append (design §5.8 step 8): rewrite the whole file
      // after every sample, so an interrupted sweep loses at most one.
      if let Err(error) = result::write_result_file(&out_path, &result) {
        eprintln!("error writing {}: {error}", out_path.display());
      }
    }

    restore_all_seeders(&self.home, &subjects_for_plan);
    Ok(result)
  }

  /// The live launch-and-poll loop for one cold-start measurement (design
  /// §5.4). Untested by design; every decision it makes about *whether* a
  /// window qualifies as the main window, or a log line is a mark, calls
  /// [`cold_start::is_main_window`] / [`log_marks::classify_log_mark`],
  /// both of which are fixture-tested in their own modules.
  fn measure_cold_start(
    &self,
    subject: &SubjectSpec,
    calibrated_area: Option<f64>,
  ) -> ColdStartOutcome {
    let bundle_path =
      bundle_paths::resolve(subject, &self.bundle_path_overrides);
    let before = process_tree::snapshot_processes();
    let karijini_path = (subject.id == "termtree").then(|| {
      log_marks::karijini_log_path(
        &self.home.join("Library").join("Application Support"),
      )
    });
    let mut log_offset: u64 = karijini_path
      .as_deref()
      .and_then(|p| std::fs::metadata(p).ok())
      .map(|m| m.len())
      .unwrap_or(0);

    let start = Instant::now();
    let _ = cold_start::launch_suppressing_restoration(bundle_path, &self.home);

    let discovery_deadline = start + Duration::from_secs(2);
    let mut main_pid = None;
    while main_pid.is_none() && Instant::now() < discovery_deadline {
      let after = process_tree::snapshot_processes();
      main_pid =
        cold_start::find_newly_launched_pid(&before, &after, bundle_path);
      if main_pid.is_none() {
        thread::sleep(Duration::from_millis(
          self.settings.window_visible_poll_ms,
        ));
      }
    }

    let mut first_window_visible_ms = None;
    let mut main_window_visible_ms = None;
    let mut app_window_ready_ms = None;
    let mut splash_close_ms = None;
    let mut splash_timeout_seen = false;
    let mut observed_main_window_area_pt: Option<f64> = None;
    // Spec item 4: whether the harness ever read a non-blank new
    // `karijini.log` line during this launch's settle window, regardless
    // of whether it classified as a known mark. Distinguishes "the log
    // never advanced" (too early, or not TermTree) from "the log advanced
    // but none of `log_marks.rs`'s hardcoded messages matched a single
    // line" -- the latter is the drift signal
    // [`log_marks::marks_unrecognized`] turns into a loud failure.
    let mut any_log_line_observed = false;

    let settle_deadline =
      start + Duration::from_millis(self.settings.fresh_launch_settle_ms);
    let poll_interval = Duration::from_millis(
      self
        .settings
        .window_visible_poll_ms
        .min(self.settings.log_tail_poll_ms)
        .max(1),
    );
    while Instant::now() < settle_deadline {
      if let Some(pid) = main_pid {
        let windows = window_probe::on_screen_windows_owned_by(pid);
        if first_window_visible_ms.is_none() && !windows.is_empty() {
          first_window_visible_ms = Some(start.elapsed().as_millis() as u64);
        }
        if let Some(area) = cold_start::calibrate_main_window_area(&windows) {
          observed_main_window_area_pt = Some(
            observed_main_window_area_pt.map_or(area, |m: f64| m.max(area)),
          );
        }
        if main_window_visible_ms.is_none() {
          if let Some(reference_area) = calibrated_area {
            if windows.iter().any(|w| {
              cold_start::is_main_window(
                w,
                reference_area,
                self.settings.main_window_area_fraction,
              )
            }) {
              main_window_visible_ms = Some(start.elapsed().as_millis() as u64);
            }
          }
        }
      }
      if let Some(path) = &karijini_path {
        if let Ok(text) = std::fs::read_to_string(path) {
          let len = text.len() as u64;
          if len >= log_offset {
            for line in text[log_offset as usize..].lines() {
              if !line.trim().is_empty() {
                any_log_line_observed = true;
              }
              match log_marks::classify_log_mark(line) {
                Some(LogMark::AppWindowReadyMain) => {
                  app_window_ready_ms
                    .get_or_insert(start.elapsed().as_millis() as u64);
                }
                Some(LogMark::SplashClosed) => {
                  splash_close_ms
                    .get_or_insert(start.elapsed().as_millis() as u64);
                }
                Some(LogMark::SplashTimeout) => splash_timeout_seen = true,
                None => {}
              }
            }
            log_offset = len;
          } else {
            // The log rolled/shrank mid-measurement (design §9): reopen
            // from the start rather than treat the shrink as an error.
            log_offset = 0;
          }
        }
      }
      let done = main_window_visible_ms.is_some()
        && (karijini_path.is_none()
          || splash_close_ms.is_some()
          || splash_timeout_seen);
      if done || interrupted() {
        break;
      }
      thread::sleep(poll_interval);
    }

    let log_marks_unrecognized = log_marks::marks_unrecognized(
      karijini_path.is_some(),
      any_log_line_observed,
      app_window_ready_ms,
      splash_close_ms,
      splash_timeout_seen,
    );

    ColdStartOutcome {
      first_window_visible_ms,
      main_window_visible_ms,
      app_window_ready_ms,
      splash_close_ms,
      splash_timeout_seen,
      main_pid,
      observed_main_window_area_pt,
      log_marks_unrecognized,
    }
  }

  /// Quits `subject` (graceful `osascript` quit, design §5.8 step 9), kills
  /// its registered companion processes, and waits up to
  /// `helper_drain_timeout_ms` for its own helpers to exit -- **only**
  /// helpers [`owned_webkit_helper_pids`]/[`chromium_helper_resident`]
  /// attribute to this subject are ever targeted (design §9: "a
  /// shared-name helper owned by a different app is never touched").
  /// Returns the number of survivors killed past the drain timeout.
  fn teardown(&self, subject: &SubjectSpec) -> u32 {
    let _ = run_capture("/usr/bin/osascript", &[
      "-e",
      &format!("quit app id \"{}\"", subject.bundle_identifier),
    ]);
    for companion in subject.companion_processes {
      if companion.kill_between_repetitions {
        let _ =
          run_capture("/usr/bin/pkill", &["-x", companion.executable_name]);
      }
    }

    let deadline = Instant::now()
      + Duration::from_millis(self.settings.helper_drain_timeout_ms);
    loop {
      let ls_text =
        launch_services::invoke_lsappinfo_list().unwrap_or_default();
      let ls_entries = launch_services::parse_lsappinfo_list(&ls_text);
      let tree = process_tree::snapshot_processes();
      let webkit_survivors = owned_webkit_helper_pids(subject, &ls_entries);
      let chromium_survivor = chromium_helper_resident(
        &tree,
        0,
        bundle_paths::resolve(subject, &self.bundle_path_overrides),
      );
      if (webkit_survivors.is_empty() && !chromium_survivor)
        || Instant::now() >= deadline
      {
        for pid in &webkit_survivors {
          let _ = run_capture("/bin/kill", &["-TERM", &pid.to_string()]);
        }
        return webkit_survivors.len() as u32;
      }
      thread::sleep(Duration::from_millis(200));
    }
  }

  /// Everything design §5.8 does for one planned repetition: warm-helper
  /// check, seed, `vm_stat` before, launch + cold start, settle,
  /// attribute, `footprint` x3, `vm_stat` after, idle CPU, teardown --
  /// assembled into one [`Sample`] via [`classify_sample_invalidity`] and
  /// the pure `build_*_record` functions above.
  #[allow(clippy::too_many_arguments)]
  fn measure_one(
    &self,
    subject: &SubjectSpec,
    planned: &PlannedRepetition,
    calibrated_areas: &mut HashMap<String, f64>,
    quiesce_violation_seen: bool,
    quiesce_reading: &QuiesceReading,
    login_shell_path: &str,
  ) -> Sample {
    let sampled_at = provenance::iso_timestamp_now();

    let ls_text = launch_services::invoke_lsappinfo_list().unwrap_or_default();
    let ls_entries = launch_services::parse_lsappinfo_list(&ls_text);

    // Spec item 3: refuse before seeding if this subject's bundle
    // identifier is already running -- checked per repetition, not only
    // at the start of the sweep, because a multi-hour sweep can outlive a
    // machine state the pre-run check saw. Skip seeding, launching, *and*
    // teardown entirely: the running instance may be the operator's own
    // real usage of the app, and `teardown`'s graceful quit must never
    // touch a process this harness did not itself launch.
    let subject_already_running =
      find_already_running_subject(std::slice::from_ref(&subject), &ls_entries)
        .is_some();
    if subject_already_running {
      let invalid_reason_value = classify_sample_invalidity(
        planned.is_calibration,
        true,
        0,
        false,
        quiesce_violation_seen,
        false,
        false,
        false,
        false,
        false,
        false,
      );
      return Sample {
        sample_id: planned.sample_id.clone(),
        subject_id: planned.subject_id.clone(),
        tier: planned.tier.as_str(),
        session_count: planned.tier.session_count(),
        repetition: planned.repetition,
        is_calibration: planned.is_calibration,
        sampled_at,
        is_valid: invalid_reason_value.is_none(),
        invalid_reason: invalid_reason_value.map(str::to_string),
        attribution: None,
        memory: None,
        cold_start: None,
        idle_cpu: None,
        quiesce: Some(quiesce_reading.clone()),
        warm_helper_count: None,
        helper_kill_count: None,
      };
    }

    // Step 2: warm-helper check (spec FR-6, design §5.8 step 2).
    let bundle_path =
      bundle_paths::resolve(subject, &self.bundle_path_overrides);
    let tree_before = process_tree::snapshot_processes();
    let warm_helper_count =
      if matches!(subject.runtime_family, RuntimeFamily::WebKitTauri) {
        owned_webkit_helper_pids(subject, &ls_entries).len() as u32
      } else if chromium_helper_resident(&tree_before, 0, bundle_path) {
        1
      } else {
        0
      };

    // Step 3: seed (N-session and sustained-use tiers only).
    let session_count = planned.tier.session_count();
    let mut seed_incomplete = false;
    if session_count > 0 {
      if let Err(error) = seeding::seed_subject(
        &self.home,
        subject.id,
        session_count,
        &self.repo,
        &self.agent,
      ) {
        eprintln!("seed {} failed: {error}", subject.id);
        seed_incomplete = true;
      }
    }

    // Step 4: vm_stat before.
    let before_memory = host_memory::invoke_vm_stat()
      .ok()
      .and_then(|text| host_memory::parse_vm_stat(&text));

    // Step 5: launch + cold start.
    let calibrated_area = calibrated_areas.get(&planned.subject_id).copied();
    let cold_start_outcome = self.measure_cold_start(subject, calibrated_area);
    if planned.is_calibration {
      if let Some(area) = cold_start_outcome.observed_main_window_area_pt {
        calibrated_areas.insert(planned.subject_id.clone(), area);
      }
    }

    // Spec item 4: TermTree launched but never created its own data
    // directory under the scratch home -- the app's data-directory
    // convention has likely changed (`seeding/termtree.rs`,
    // `log_marks::karijini_log_path` both hardcode
    // `DocumentNode/TermTree`).
    let app_data_dir_missing = subject.id == "termtree"
      && cold_start_outcome.main_pid.is_some()
      && !log_marks::app_data_dir(
        &self.home.join("Library").join("Application Support"),
      )
      .exists();

    // Spec item 4: this subject's seeder format has never been checked
    // against a real install (`subject.seed_format_verified`), so any
    // tier that relies on seeding reports unverified rather than valid.
    let seed_format_unverified =
      session_count > 0 && !subject.seed_format_verified;

    // Step 6: settle. `measure_cold_start` already spends up to
    // `freshLaunchSettleMs`; the heavier tiers wait out their own fixed
    // duration on top of it.
    let extra_settle_s = match planned.tier {
      Tier::FreshLaunch => 0,
      Tier::SustainedUse => self.settings.sustained_use_duration_s,
      Tier::NSession(_) => self.settings.n_session_settle_s,
    };
    let recheck_interval_s = self.settings.quiesce_window_s.max(30);
    for _ in 0..quiesce_recheck_count(extra_settle_s, recheck_interval_s) {
      thread::sleep(Duration::from_secs(recheck_interval_s));
      if interrupted() {
        break;
      }
    }
    let remaining_settle_s = extra_settle_s % recheck_interval_s;
    if remaining_settle_s > 0 {
      thread::sleep(Duration::from_secs(remaining_settle_s));
    }

    // Session readiness (N-session / sustained-use tiers): generic check,
    // the same way for every subject (design §5.6, NFR-3).
    if session_count > 0 && !seed_incomplete {
      match cold_start_outcome.main_pid {
        Some(pid) => {
          let tree = process_tree::snapshot_processes();
          let (shells, agents) = seeding::count_ready_sessions(
            &tree,
            pid,
            login_shell_path,
            &self.agent.executable_path,
          );
          if shells < session_count || agents < session_count {
            seed_incomplete = true;
          }
        }
        None => seed_incomplete = true,
      }
    }

    // Step 7: attribute -> footprint x3 -> vm_stat after -> idle CPU.
    let mut attribution_incomplete = cold_start_outcome.main_pid.is_none();
    let mut footprint_pid_mismatch = false;
    let mut attribution_record = None;
    let mut memory_record = None;
    let mut idle_cpu_record = None;

    if let Some(main_pid) = cold_start_outcome.main_pid {
      let tree = process_tree::snapshot_processes();
      let ls_text_now =
        launch_services::invoke_lsappinfo_list().unwrap_or_default();
      let ls_entries_now = launch_services::parse_lsappinfo_list(&ls_text_now);
      let ls_pids = attribution::resolve_launch_services_pids(
        &ls_entries_now,
        subject.launch_services_name,
        subject.helper_bundle_ids,
      );
      let descendants = process_tree::descendants_of(&tree, main_pid);

      match attribution::resolve_union(main_pid, &ls_pids, &tree, &descendants)
      {
        Err(_) => attribution_incomplete = true,
        Ok(mut set) => {
          attribution::partition_by_role(
            &mut set,
            &tree,
            &self.agent.executable_path,
            login_shell_path,
          );
          for companion in subject.companion_processes {
            if let Some(record) =
              tree.iter().find(|r| r.name == companion.executable_name)
            {
              attribution::attribute_companion_process(
                &mut set,
                record.pid,
                &record.name,
                record.executable_path.clone(),
              );
            }
          }

          let scratch = std::env::temp_dir();
          let slug = planned.sample_id.replace('/', "-");
          let union_report = invoke_and_parse_footprint(
            &scratch.join(format!("fp-{slug}-union.json")),
            &set.pids(),
          );
          let orchestrator_report = invoke_and_parse_footprint(
            &scratch.join(format!("fp-{slug}-orchestrator.json")),
            &set.orchestrator_pids(),
          );
          let session_report = invoke_and_parse_footprint(
            &scratch.join(format!("fp-{slug}-session.json")),
            &set.agent_cli_pids(),
          );

          match (union_report, orchestrator_report, session_report) {
            (Some(union), Some(orchestrator), Some(session)) => {
              let union_pids = set.pids();
              if footprint::verify_pid_set(&union_pids, &union).is_err() {
                footprint_pid_mismatch = true;
              }
              let vanished: Vec<u32> = union_pids
                .iter()
                .copied()
                .filter(|pid| !union.processes.iter().any(|p| p.pid == *pid))
                .collect();
              attribution_record =
                Some(build_attribution_record(&set, &vanished, &union));

              let rss_sum: u64 = tree
                .iter()
                .filter(|r| union_pids.contains(&r.pid))
                .map(|r| r.rss_bytes)
                .sum();
              let core_process_bytes = union
                .processes
                .iter()
                .find(|p| p.name == subject.main_executable_name)
                .map(|p| p.footprint_bytes);
              let render_helper_bytes = Some(
                union
                  .processes
                  .iter()
                  .filter(|p| p.name != subject.main_executable_name)
                  .map(|p| p.footprint_bytes)
                  .sum(),
              );

              if let (Some(before), Ok(after_text)) =
                (&before_memory, host_memory::invoke_vm_stat())
              {
                if let Some(after) = host_memory::parse_vm_stat(&after_text) {
                  memory_record = Some(build_memory_record(
                    &union,
                    &orchestrator,
                    &session,
                    rss_sum,
                    core_process_bytes,
                    render_helper_bytes,
                    before,
                    &after,
                  ));
                }
              }

              idle_cpu_record = cpu_sampler::sample_idle_cpu(
                &union_pids,
                self.settings.idle_cpu_sample_interval_ms,
                self.settings.idle_cpu_sample_count,
              )
              .map(|summary| {
                build_idle_cpu_record(&summary, "foreground-unoccluded")
              });
            }
            _ => attribution_incomplete = true,
          }
        }
      }
    }

    // Step 9/10: teardown, then seeder restore for tiers that seeded.
    let helper_kill_count = self.teardown(subject);
    if session_count > 0 {
      if let Err(error) = seeding::restore_subject(&self.home, subject.id) {
        eprintln!("restore {} failed: {error}", subject.id);
      }
    }

    let invalid_reason_value = classify_sample_invalidity(
      planned.is_calibration,
      false,
      warm_helper_count,
      cold_start_outcome.splash_timeout_seen,
      quiesce_violation_seen,
      attribution_incomplete,
      footprint_pid_mismatch,
      seed_incomplete,
      seed_format_unverified,
      cold_start_outcome.log_marks_unrecognized,
      app_data_dir_missing,
    );

    Sample {
      sample_id: planned.sample_id.clone(),
      subject_id: planned.subject_id.clone(),
      tier: planned.tier.as_str(),
      session_count,
      repetition: planned.repetition,
      is_calibration: planned.is_calibration,
      sampled_at,
      is_valid: invalid_reason_value.is_none(),
      invalid_reason: invalid_reason_value.map(str::to_string),
      attribution: attribution_record,
      memory: memory_record,
      cold_start: Some(build_cold_start_record(
        cold_start_outcome.first_window_visible_ms,
        cold_start_outcome.main_window_visible_ms,
        cold_start_outcome.app_window_ready_ms,
        cold_start_outcome.splash_close_ms,
        self
          .settings
          .window_visible_poll_ms
          .max(self.settings.log_tail_poll_ms),
      )),
      idle_cpu: idle_cpu_record,
      quiesce: Some(quiesce_reading.clone()),
      warm_helper_count: Some(warm_helper_count),
      helper_kill_count: Some(helper_kill_count),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::subject::find;

  #[test]
  fn webkit_subject_is_warm_when_a_helper_entry_survives() {
    let subject = find("termtree").unwrap();
    let entries = vec![LaunchServicesEntry {
      display_name: "TermTree Web Content".into(),
      bundle_identifier: Some("com.apple.WebKit.WebContent".into()),
      bundle_path: None,
      executable_path: None,
      pid: Some(123),
      in_front: false,
    }];
    assert!(has_warm_helpers(subject, &entries, false));
  }

  #[test]
  fn webkit_subject_is_not_warm_with_no_surviving_entries() {
    let subject = find("termtree").unwrap();
    assert!(!has_warm_helpers(subject, &[], false));
  }

  #[test]
  fn chromium_subject_warmth_comes_from_the_process_tree_signal() {
    let subject = find("collaborator").unwrap();
    assert!(has_warm_helpers(subject, &[], true));
    assert!(!has_warm_helpers(subject, &[], false));
  }

  #[test]
  fn calibration_discard_takes_priority_over_everything() {
    let reason = classify_sample_invalidity(
      true, true, 5, true, true, true, true, true, true, true, true,
    );
    assert_eq!(reason, Some(invalid_reason::CALIBRATION_DISCARD));
  }

  #[test]
  fn warm_webview_is_reported_when_present_and_not_calibration() {
    let reason = classify_sample_invalidity(
      false, false, 1, false, false, false, false, false, false, false, false,
    );
    assert_eq!(reason, Some(invalid_reason::WARM_WEBVIEW));
  }

  #[test]
  fn no_signals_is_valid() {
    let reason = classify_sample_invalidity(
      false, false, 0, false, false, false, false, false, false, false, false,
    );
    assert_eq!(reason, None);
  }

  #[test]
  fn seed_incomplete_takes_priority_over_warm_webview() {
    let reason = classify_sample_invalidity(
      false, false, 1, false, false, false, false, true, false, false, false,
    );
    assert_eq!(reason, Some(invalid_reason::SEED_INCOMPLETE));
  }

  #[test]
  fn subject_already_running_takes_priority_over_seed_incomplete() {
    let reason = classify_sample_invalidity(
      false, true, 0, false, false, false, false, true, false, false, false,
    );
    assert_eq!(reason, Some(invalid_reason::SUBJECT_ALREADY_RUNNING));
  }

  #[test]
  fn seed_format_unverified_is_reported_when_no_stronger_reason_applies() {
    let reason = classify_sample_invalidity(
      false, false, 0, false, false, false, false, false, true, false, false,
    );
    assert_eq!(reason, Some(invalid_reason::SEED_FORMAT_UNVERIFIED));
  }

  #[test]
  fn seed_incomplete_takes_priority_over_seed_format_unverified() {
    let reason = classify_sample_invalidity(
      false, false, 0, false, false, false, false, true, true, false, false,
    );
    assert_eq!(reason, Some(invalid_reason::SEED_INCOMPLETE));
  }

  #[test]
  fn app_data_dir_missing_is_reported_when_no_stronger_reason_applies() {
    let reason = classify_sample_invalidity(
      false, false, 0, false, false, false, false, false, false, false, true,
    );
    assert_eq!(reason, Some(invalid_reason::APP_DATA_DIR_NOT_CREATED));
  }

  #[test]
  fn log_marks_unrecognized_is_reported_when_no_stronger_reason_applies() {
    let reason = classify_sample_invalidity(
      false, false, 0, false, false, false, false, false, false, true, false,
    );
    assert_eq!(
      reason,
      Some(invalid_reason::TERMTREE_LOG_MARKS_UNRECOGNIZED)
    );
  }

  // --- select_subjects / build_settings ----------------------------------

  #[test]
  fn default_selection_excludes_the_optional_subject() {
    let selected = select_subjects(None, false).unwrap();
    assert!(!selected.iter().any(|s| s.id == "diri"));
    assert!(selected.iter().any(|s| s.id == "termtree"));
  }

  #[test]
  fn allow_optional_subjects_includes_diri_in_the_default_selection() {
    let selected = select_subjects(None, true).unwrap();
    assert!(selected.iter().any(|s| s.id == "diri"));
  }

  #[test]
  fn explicit_selection_of_an_unknown_id_is_refused() {
    let error =
      select_subjects(Some(&["not-a-subject".to_string()]), false).unwrap_err();
    assert!(
      matches!(error, RunRefusal::UnknownSubject(id) if id == "not-a-subject")
    );
  }

  #[test]
  fn explicit_selection_of_an_optional_subject_is_honoured() {
    let selected = select_subjects(Some(&["diri".to_string()]), false).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, "diri");
  }

  #[test]
  fn repetitions_override_applies_uniformly_to_every_tier() {
    let settings = build_settings(Some(2));
    assert_eq!(settings.repetitions.fresh_launch, 2);
    assert_eq!(settings.repetitions.sustained_use, 2);
    assert_eq!(settings.repetitions.n_session, 2);
  }

  #[test]
  fn no_override_keeps_the_tiered_defaults() {
    let settings = build_settings(None);
    assert_eq!(settings, RunSettings::default());
  }

  // --- plan_repetitions / pending_repetitions -----------------------------

  #[test]
  fn plan_includes_the_calibration_launch_plus_the_disclosed_count() {
    let subject = find("termtree").unwrap();
    let settings = build_settings(Some(3));
    let plan = plan_repetitions(&[subject], &[Tier::FreshLaunch], &settings);
    // repetitions 0..=3 inclusive == 4 planned launches.
    assert_eq!(plan.len(), 4);
    assert!(plan[0].is_calibration);
    assert!(plan[1..].iter().all(|p| !p.is_calibration));
    assert_eq!(plan[0].sample_id, "termtree/fresh-launch/000");
    assert_eq!(plan[3].sample_id, "termtree/fresh-launch/003");
  }

  #[test]
  fn plan_expands_every_subject_and_tier_combination() {
    let termtree = find("termtree").unwrap();
    let collaborator = find("collaborator").unwrap();
    let settings = build_settings(Some(1));
    let plan = plan_repetitions(
      &[termtree, collaborator],
      &[Tier::FreshLaunch, Tier::NSession(5)],
      &settings,
    );
    // 2 subjects * 2 tiers * (0..=1 == 2 repetitions each) == 8.
    assert_eq!(plan.len(), 8);
  }

  #[test]
  fn pending_repetitions_drops_sample_ids_already_completed() {
    let subject = find("termtree").unwrap();
    let settings = build_settings(Some(2));
    let plan = plan_repetitions(&[subject], &[Tier::FreshLaunch], &settings);
    let completed: HashSet<String> =
      [plan[0].sample_id.clone(), plan[1].sample_id.clone()]
        .into_iter()
        .collect();
    let pending = pending_repetitions(&plan, &completed);
    assert_eq!(pending.len(), plan.len() - 2);
    assert!(pending.iter().all(|p| !completed.contains(&p.sample_id)));
  }

  #[test]
  fn pending_repetitions_with_nothing_completed_is_the_whole_plan() {
    let subject = find("termtree").unwrap();
    let settings = build_settings(Some(1));
    let plan = plan_repetitions(&[subject], &[Tier::FreshLaunch], &settings);
    let pending = pending_repetitions(&plan, &HashSet::new());
    assert_eq!(pending, plan);
  }

  #[test]
  fn quiesce_recheck_count_is_zero_below_the_interval() {
    assert_eq!(quiesce_recheck_count(15, 30), 0);
  }

  #[test]
  fn quiesce_recheck_count_divides_a_long_settle_window() {
    assert_eq!(quiesce_recheck_count(120, 30), 4);
  }

  // --- chromium_helper_resident / owned_webkit_helper_pids ----------------

  #[test]
  fn chromium_helper_resident_ignores_the_main_pid_itself() {
    let records = vec![ProcessRecord {
      pid: 1,
      ppid: 0,
      name: "Collaborator".into(),
      executable_path: Some("/Applications/Collaborator.app/main".into()),
      rss_bytes: 0,
    }];
    assert!(!chromium_helper_resident(
      &records,
      1,
      "/Applications/Collaborator.app"
    ));
  }

  #[test]
  fn chromium_helper_resident_true_for_a_surviving_helper() {
    let records = vec![
      ProcessRecord {
        pid: 1,
        ppid: 0,
        name: "Collaborator".into(),
        executable_path: Some("/Applications/Collaborator.app/main".into()),
        rss_bytes: 0,
      },
      ProcessRecord {
        pid: 2,
        ppid: 1,
        name: "Collaborator Helper".into(),
        executable_path: Some("/Applications/Collaborator.app/helper".into()),
        rss_bytes: 0,
      },
    ];
    assert!(chromium_helper_resident(
      &records,
      1,
      "/Applications/Collaborator.app"
    ));
  }

  #[test]
  fn owned_webkit_helper_pids_only_returns_this_subjects_helpers() {
    let subject = find("termtree").unwrap();
    let entries = vec![
      LaunchServicesEntry {
        display_name: "TermTree Web Content".into(),
        bundle_identifier: Some("com.apple.WebKit.WebContent".into()),
        bundle_path: None,
        executable_path: None,
        pid: Some(101),
        in_front: false,
      },
      // A same-named helper owned by a different app -- must never be
      // returned (design §9's "shared-name helper" case).
      LaunchServicesEntry {
        display_name: "Other App Web Content".into(),
        bundle_identifier: Some("com.apple.WebKit.WebContent".into()),
        bundle_path: None,
        executable_path: None,
        pid: Some(202),
        in_front: false,
      },
    ];
    let pids = owned_webkit_helper_pids(subject, &entries);
    assert_eq!(pids, vec![101]);
  }

  // --- find_already_running_subject ---------------------------------------

  #[test]
  fn finds_a_subject_already_running_by_bundle_identifier() {
    let termtree = find("termtree").unwrap();
    let collaborator = find("collaborator").unwrap();
    let entries = vec![LaunchServicesEntry {
      display_name: "TermTree".into(),
      bundle_identifier: Some("com.termtree.desktop".into()),
      bundle_path: None,
      executable_path: None,
      pid: Some(4242),
      in_front: false,
    }];
    let found =
      find_already_running_subject(&[termtree, collaborator], &entries);
    assert_eq!(found.map(|s| s.id), Some("termtree"));
  }

  #[test]
  fn a_registered_but_not_running_entry_does_not_count() {
    let termtree = find("termtree").unwrap();
    // Registered with LaunchServices but not currently running: no `pid`
    // line (design §5.2.1's "registered but not running" case).
    let entries = vec![LaunchServicesEntry {
      display_name: "TermTree".into(),
      bundle_identifier: Some("com.termtree.desktop".into()),
      bundle_path: None,
      executable_path: None,
      pid: None,
      in_front: false,
    }];
    assert!(find_already_running_subject(&[termtree], &entries).is_none());
  }

  #[test]
  fn a_same_named_different_bundle_identifier_does_not_count() {
    let termtree = find("termtree").unwrap();
    // Same display name, different bundle identifier -- must not match
    // (this is exactly the "differently-named bundles can share an
    // identifier" case inverted: here the names collide but the
    // identifiers do not, so it must not be treated as the same subject).
    let entries = vec![LaunchServicesEntry {
      display_name: "TermTree".into(),
      bundle_identifier: Some("com.example.unrelated".into()),
      bundle_path: None,
      executable_path: None,
      pid: Some(1),
      in_front: false,
    }];
    assert!(find_already_running_subject(&[termtree], &entries).is_none());
  }

  // --- build_attribution_record / build_memory_record ---------------------

  fn footprint_report(
    total: u64,
    processes: Vec<(u32, &str, u64)>,
  ) -> FootprintReport {
    FootprintReport {
      total_footprint_bytes: total,
      processes: processes
        .into_iter()
        .map(|(pid, name, bytes)| crate::footprint::FootprintProcess {
          pid,
          name: name.to_string(),
          footprint_bytes: bytes,
          phys_footprint_bytes: None,
          has_categories: false,
        })
        .collect(),
      shared: vec![],
      errors: vec![],
      warnings: vec![],
      page_size_bytes: 16384,
      start_time_iso: None,
    }
  }

  #[test]
  fn attribution_record_carries_footprint_bytes_per_process() {
    let set = AttributableProcessSet {
      main_pid: 1,
      processes: vec![attribution::AttributedProcess {
        pid: 1,
        name: "termtree".into(),
        executable_path: None,
        discovered_by: DiscoverySource::Both,
        role: attribution::ProcessRole::Orchestrator,
      }],
    };
    let report = footprint_report(1000, vec![(1, "termtree", 1000)]);
    let record = build_attribution_record(&set, &[99], &report);
    assert_eq!(record.main_pid, 1);
    assert_eq!(record.vanished_pids, vec![99]);
    assert_eq!(record.launch_services_pids, vec![1]);
    assert_eq!(record.process_tree_pids, vec![1]);
    assert_eq!(record.processes[0].phys_footprint_bytes, Some(1000));
    assert_eq!(record.processes[0].discovered_by, "both");
  }

  fn host_memory_sample(
    free_pages: u64,
    used_pages: u64,
    compressor_pages: u64,
    swapouts: u64,
  ) -> HostMemorySample {
    let mut counters = std::collections::BTreeMap::new();
    counters.insert("Pages free".to_string(), free_pages);
    counters.insert("Pages speculative".to_string(), 0);
    counters.insert("Anonymous pages".to_string(), used_pages);
    counters.insert("Pages wired down".to_string(), 0);
    counters
      .insert("Pages occupied by compressor".to_string(), compressor_pages);
    counters.insert("Pages purgeable".to_string(), 0);
    counters.insert("Swapouts".to_string(), swapouts);
    HostMemorySample {
      page_size_bytes: 1,
      counters,
    }
  }

  #[test]
  fn memory_record_free_ram_delta_is_positive_means_consumed() {
    let union = footprint_report(1000, vec![]);
    let orchestrator = footprint_report(700, vec![]);
    let session = footprint_report(300, vec![]);
    let before = host_memory_sample(1000, 0, 0, 0);
    let after = host_memory_sample(400, 0, 0, 0);
    let record = build_memory_record(
      &union,
      &orchestrator,
      &session,
      1500,
      Some(200),
      Some(800),
      &before,
      &after,
    );
    // free RAM dropped from 1000 to 400 -- 600 consumed, positive sign.
    assert_eq!(record.free_ram_delta_bytes, 600);
    assert_eq!(record.free_ram_delta_sign, "positive-means-consumed");
    assert_eq!(record.mem_phys_footprint_bytes, 1000);
    assert_eq!(record.orchestrator_attributable_bytes, 700);
    assert_eq!(record.agent_cli_attributable_bytes, 300);
    // 700 + 300 - 1000 == 0: no cross-partition overlap in this fixture.
    assert_eq!(record.cross_partition_shared_bytes, 0);
  }

  #[test]
  fn memory_record_cross_partition_overlap_is_the_excess_over_the_union() {
    let union = footprint_report(1000, vec![]);
    let orchestrator = footprint_report(700, vec![]);
    let session = footprint_report(400, vec![]);
    let before = host_memory_sample(1000, 0, 0, 0);
    let after = host_memory_sample(1000, 0, 0, 0);
    let record = build_memory_record(
      &union,
      &orchestrator,
      &session,
      0,
      None,
      None,
      &before,
      &after,
    );
    // 700 + 400 - 1000 == 100.
    assert_eq!(record.cross_partition_shared_bytes, 100);
  }

  #[test]
  fn memory_record_swapouts_delta_never_goes_negative() {
    let union = footprint_report(0, vec![]);
    let before = host_memory_sample(0, 0, 0, 500);
    let after = host_memory_sample(0, 0, 0, 300);
    let record = build_memory_record(
      &union, &union, &union, 0, None, None, &before, &after,
    );
    assert_eq!(record.swapouts_delta, 0);
  }

  // --- build_cold_start_record / build_idle_cpu_record ---------------------

  #[test]
  fn cold_start_record_names_the_source_of_every_present_mark() {
    let record =
      build_cold_start_record(Some(400), Some(2100), Some(1500), None, 20);
    assert_eq!(
      record.mark_source.get("firstWindowVisibleMs"),
      Some(&"cg-window-list".to_string())
    );
    assert_eq!(
      record.mark_source.get("appWindowReadyMs"),
      Some(&"karijini-log-arrival".to_string())
    );
    assert!(!record.mark_source.contains_key("splashCloseMs"));
  }

  #[test]
  fn idle_cpu_record_carries_the_summary_and_window_state() {
    let summary = stats::Summary {
      median: 4.7,
      q1: 3.0,
      q3: 6.0,
      iqr: 3.0,
      n: 30,
    };
    let record = build_idle_cpu_record(&summary, "foreground-unoccluded");
    assert_eq!(record.idle_cpu_percent_of_one_core_median, 4.7);
    assert_eq!(record.sample_count, 30);
    assert_eq!(record.window_state, "foreground-unoccluded");
  }
}
