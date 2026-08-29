//! The table renderer (spec FR-13, FR-14): `fn render(&ResultFile) ->
//! String`, a **pure function** of the parsed result file with no
//! filesystem or process access, so "table rendering is a pure function of
//! the JSON result file" is enforced by the signature (design §5.10).
//!
//! `render` never computes a percentage delta and never emits comparative
//! adjectives (FR-12) -- it renders medians, IQRs, and counts.

use crate::result::ResultFile;
use crate::subject::{EXCLUSIONS, HOLD_BACKS};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub fn render(result: &ResultFile) -> String {
  let mut out = String::new();
  render_provenance(&mut out, result);
  render_tier_tables(&mut out, result);
  render_discards_table(&mut out, result);
  render_attribution_evidence(&mut out, result);
  render_limitations(&mut out, result);
  render_exclusions(&mut out);
  render_fairness_review(&mut out, result);
  out
}

fn render_provenance(out: &mut String, result: &ResultFile) {
  let _ = writeln!(out, "# Resource Benchmark Result: {}", result.run_id);
  let _ = writeln!(out);
  let _ = writeln!(out, "## Provenance");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "- Machine: {} ({} logical / {} physical cores, {} B RAM)",
    result.machine_spec.cpu_brand,
    result.machine_spec.logical_cores,
    result.machine_spec.physical_cores,
    result.machine_spec.ram_bytes
  );
  let _ = writeln!(
    out,
    "- OS build: {} {} ({})",
    result.os_build.product_name,
    result.os_build.product_version,
    result.os_build.build_version
  );
  let _ = writeln!(
    out,
    "- Agent CLI: {} {}",
    result.agent_cli_version.name, result.agent_cli_version.version
  );
  let _ = writeln!(
    out,
    "- Repo ref: {}@{}",
    result.repo_ref.url, result.repo_ref.commit
  );
  let _ = writeln!(out, "- Run timestamp: {}", result.run_timestamp);
  for subject in &result.subjects {
    let _ = writeln!(
      out,
      "- Subject: {} {}",
      subject.display_name, subject.subject_version
    );
  }
  let _ = writeln!(out);
}

fn render_tier_tables(out: &mut String, result: &ResultFile) {
  let mut by_tier: BTreeMap<String, Vec<&crate::result::Aggregate>> =
    BTreeMap::new();
  for aggregate in &result.aggregates {
    by_tier
      .entry(aggregate.tier.clone())
      .or_default()
      .push(aggregate);
  }
  for (tier, aggregates) in &by_tier {
    let _ = writeln!(out, "## Tier: {tier}");
    let _ = writeln!(out);
    let _ = writeln!(
      out,
      "| Subject | Metric | Median | Q1 | Q3 | IQR | n | Discarded |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
    for aggregate in aggregates {
      let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} | {} | {} | {} |",
        aggregate.subject_id,
        aggregate.metric,
        aggregate.median,
        aggregate.q1,
        aggregate.q3,
        aggregate.iqr,
        aggregate.n,
        aggregate.discarded_count
      );
    }
    let _ = writeln!(out);
  }
  // Every repetition count is disclosed here, per subject/tier, whether or
  // not any aggregate rows exist yet -- an undisclosed shortfall is what
  // FR-12/NFR-2 forbid.
  let _ = writeln!(out, "## Repetition Counts (disclosed per tier)");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "- fresh-launch: n={}",
    result.settings.repetitions.fresh_launch
  );
  let _ = writeln!(
    out,
    "- sustained-use: n={}",
    result.settings.repetitions.sustained_use
  );
  let _ = writeln!(
    out,
    "- n-session-*: n={}",
    result.settings.repetitions.n_session
  );
  let _ = writeln!(out);
}

fn render_discards_table(out: &mut String, result: &ResultFile) {
  let _ = writeln!(out, "## Discards");
  let _ = writeln!(out);
  let _ = writeln!(out, "| Subject | Tier | n | Discarded | Reasons |");
  let _ = writeln!(out, "|---|---|---|---|---|");
  for aggregate in &result.aggregates {
    let reasons: Vec<String> = aggregate
      .discarded_reasons
      .iter()
      .map(|(reason, count)| format!("{reason}: {count}"))
      .collect();
    let _ = writeln!(
      out,
      "| {} | {} | {} | {} | {} |",
      aggregate.subject_id,
      aggregate.tier,
      aggregate.n,
      aggregate.discarded_count,
      reasons.join(", ")
    );
  }
  let _ = writeln!(out);
}

fn render_attribution_evidence(out: &mut String, result: &ResultFile) {
  let _ = writeln!(out, "## Attribution Evidence");
  let _ = writeln!(out);
  let _ = writeln!(out, "| Subject | launch-services | process-tree | both |");
  let _ = writeln!(out, "|---|---|---|---|");
  let mut by_subject: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new();
  for sample in &result.samples {
    let Some(attribution) = &sample.attribution else {
      continue;
    };
    let entry = by_subject.entry(sample.subject_id.clone()).or_default();
    for process in &attribution.processes {
      match process.discovered_by.as_str() {
        "launch-services" => entry.0 += 1,
        "process-tree" => entry.1 += 1,
        "both" => entry.2 += 1,
        _ => {}
      }
    }
  }
  for (subject, (ls, tree, both)) in &by_subject {
    let _ = writeln!(out, "| {subject} | {ls} | {tree} | {both} |");
  }
  let _ = writeln!(out);
}

fn has_tier(result: &ResultFile, tier_prefix: &str) -> bool {
  result.samples.iter().any(|s| s.tier == tier_prefix)
    || result.aggregates.iter().any(|a| a.tier == tier_prefix)
}

fn has_any_n_session_tier(result: &ResultFile) -> bool {
  result
    .samples
    .iter()
    .any(|s| s.tier.starts_with("n-session-"))
    || result
      .aggregates
      .iter()
      .any(|a| a.tier.starts_with("n-session-"))
}

fn has_subject_runtime_family(result: &ResultFile, family: &str) -> bool {
  result.subjects.iter().any(|s| s.runtime_family == family)
}

fn has_termtree_cold_start_row(result: &ResultFile) -> bool {
  result.subjects.iter().any(|s| s.subject_id == "termtree")
    && result
      .samples
      .iter()
      .any(|s| s.subject_id == "termtree" && s.cold_start.is_some())
}

fn has_idle_cpu_row(result: &ResultFile) -> bool {
  result.samples.iter().any(|s| s.idle_cpu.is_some())
}

fn has_rss_field(result: &ResultFile) -> bool {
  result.samples.iter().any(|s| s.memory.is_some())
}

fn unverified_seed_format_display_names(result: &ResultFile) -> Vec<&str> {
  result
    .subjects
    .iter()
    .filter(|s| !s.seed_format_verified)
    .map(|s| s.display_name.as_str())
    .collect()
}

/// Each limitation is emitted by a rule keyed on what is present in the
/// data, so it cannot be omitted by editing prose (spec FR-14, design
/// §5.10).
fn render_limitations(out: &mut String, result: &ResultFile) {
  let _ = writeln!(out, "## Limitations");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "- **Attribution asymmetry**: `launchd`-parented (WebKit) helper \
     processes and app-parented (Chromium/Electron) helper processes are \
     resolved by two different unprivileged mechanisms, unioned and \
     deduplicated (design §5.2). This union/partition approach is macOS-specific."
  );
  if has_rss_field(result) {
    let _ = writeln!(
      out,
      "- **phys_footprint vs RSS**: `memRssBytes` (naive per-process sum) \
       diverges materially from `memPhysFootprintBytes` (footprint's \
       deduplicated set total) and the choice can change which subject \
       looks better; RSS is never the primary published figure."
    );
  }
  if has_termtree_cold_start_row(result) {
    let _ = writeln!(
      out,
      "- **TermTree's cold-start marks are self-reported and \
       tail-trimmed**: `appWindowReadyMs` and `splashCloseMs` are read from \
       TermTree's own log lines, so no other subject has an equivalent and \
       only the externally probed `mainWindowVisibleMs` is comparable \
       across subjects. Neither mark proves a painted first frame: the \
       window is still hidden when `app_window_ready` runs \
       (`src-tauri/src/command/window_cmd.rs:465-494`), so \
       `requestAnimationFrame` has not yet fired. `splashCloseMs` is \
       readiness-driven, not floored -- taskhub#672 removed the 2,000 ms \
       minimum splash and the fixed 500 ms pre-show sleep that used to pad \
       it -- but a launch whose frontend never signals readiness hits the \
       15 s forced transition in `start_splash_monitor` \
       (`src-tauri/src/lib.rs:104-143`) and is excluded from the aggregate \
       as a `splash-timeout` sample (FR-12): a discard reason no other \
       subject can trigger, so TermTree's slowest launches leave the \
       aggregate (counted in the Discards table) while other subjects' \
       slowest launches stay in."
    );
  }
  if has_tier(result, "fresh-launch") {
    let _ = writeln!(
      out,
      "- **Fresh-launch-only would flatter TermTree**: TermTree's own \
       disclosed measurement after 8 days of uptime showed 3.6-3.9 GB of \
       phys_footprint across six attributed processes, of which the Rust \
       process itself was ~74 MB, and the figure was still climbing \
       within a single session."
    );
  }
  if has_idle_cpu_row(result) {
    let _ = writeln!(
      out,
      "- **TermTree's unavoidable idle work**: the 5 s terminal idle sweep, \
       the 5 s wake-gap timer, the 30 s autosave, the hourly updater check, \
       and the 60 s macOS webview health probe are all plausible sources \
       of idle-CPU loss. The mind map renders on demand — frames are \
       produced only while something is animating, being interacted with, \
       or has changed and not yet reached the canvas — so a settled map \
       costs nothing; a map with a session at status running or waiting \
       does render at animation rate, because its status dot is pulsing."
    );
  }
  if has_any_n_session_tier(result) {
    let _ = writeln!(
      out,
      "- **N-session scaling is expected to narrow, not favor, TermTree**: \
       WKWebView content processes are not shared across sessions (Tauri \
       #5031), so scaling is not expected to be sublinear; this is not \
       framed as a surprise result."
    );
  }
  if has_subject_runtime_family(result, "chromium-electron") {
    let _ = writeln!(
      out,
      "- **The orchestrator/agent-CLI partition costs Collaborator**: its \
       vendored `tmux` and `node-pty` sidecar are `Orchestrator` under the \
       one partition rule applied to every subject, because neither is a \
       session root -- they are Collaborator's own implementation choice \
       for a job TermTree does in-process with `portable-pty`."
    );
  }
  if has_tier(result, "sustained-use") {
    let _ = writeln!(
      out,
      "- **The sustained-use tier exercises orchestrator, terminal-output, \
       and rendering paths, not agent inference**: real prompts are \
       deliberately not sent, so this tier is a floor on the long-uptime \
       effect it exists to surface, not a measurement of it."
    );
  }
  let unverified = unverified_seed_format_display_names(result);
  if !unverified.is_empty() {
    let _ = writeln!(
      out,
      "- **Unverified seed formats**: {}'s session seeder has not been \
       checked against a real install (see the seeder's module doc under \
       `src/seeding/`). Every N-session/sustained-use sample for it \
       reports `invalidReason: \"seed-format-unverified\"` until this is \
       verified and the registry is updated.",
      unverified.join(", ")
    );
  }
  let _ = writeln!(
    out,
    "- **Open question**: whether App Nap or windowing-engine occlusion \
     throttling makes the fixed foreground/unoccluded idle-CPU state \
     itself non-representative of real-world usage is not resolved by \
     this harness."
  );
  let _ = writeln!(out);
}

fn render_exclusions(out: &mut String) {
  let _ = writeln!(out, "## Excluded Subjects");
  let _ = writeln!(out);
  for exclusion in EXCLUSIONS {
    let _ = writeln!(out, "- **{}**: {}", exclusion.name, exclusion.reason);
  }
  let _ = writeln!(out);
  let _ = writeln!(out, "## Hold-back Subjects");
  let _ = writeln!(out);
  for hold_back in HOLD_BACKS {
    let _ = writeln!(out, "- **{}**: {}", hold_back.name, hold_back.reason);
  }
  let _ = writeln!(out);
}

fn render_fairness_review(out: &mut String, result: &ResultFile) {
  let _ = writeln!(out, "## Fairness Review");
  let _ = writeln!(out);
  match (
    &result.fairness_review.reviewer,
    &result.fairness_review.reviewed_at,
  ) {
    (Some(reviewer), Some(date)) => {
      let _ = writeln!(out, "Reviewed by {reviewer} on {date}.");
      if let Some(notes) = &result.fairness_review.notes {
        let _ = writeln!(out);
        let _ = writeln!(out, "{notes}");
      }
    }
    _ => {
      let _ = writeln!(out, "**NOT REVIEWED — DO NOT PUBLISH**");
    }
  }
  let _ = writeln!(out);
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::quiesce::QuiesceReading;
  use crate::result::*;
  use crate::settings::RunSettings;

  fn base_result() -> ResultFile {
    ResultFile {
      schema_version: SCHEMA_VERSION,
      run_id: "test-run".into(),
      run_timestamp: "2026-08-25T00:00:00Z".into(),
      harness_ref: HarnessRef {
        repo: "termtree".into(),
        commit: "abc".into(),
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
        url: "https://example.com/repo.git".into(),
        commit: "abc123".into(),
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

  fn termtree_subject() -> SubjectProvenance {
    SubjectProvenance {
      subject_id: "termtree".into(),
      display_name: "TermTree".into(),
      subject_version: "1.0.0".into(),
      runtime_family: "webkit-tauri".into(),
      bundle_identifier: "com.termtree.desktop".into(),
      bundle_path: "/Applications/TermTree.app".into(),
      optional: false,
      seeder: "termtree-state-json".into(),
      seed_method: "production state.json pre-write".into(),
      calibrated_main_window_area_pt: Some(1_310_720.0),
      version_drift_accepted: false,
      seed_format_verified: true,
    }
  }

  fn cold_start_sample() -> Sample {
    Sample {
      sample_id: "termtree/fresh-launch/001".into(),
      subject_id: "termtree".into(),
      tier: "fresh-launch".into(),
      session_count: 0,
      repetition: 1,
      is_calibration: false,
      sampled_at: "2026-08-25T00:00:00Z".into(),
      is_valid: true,
      invalid_reason: None,
      attribution: None,
      memory: None,
      cold_start: Some(ColdStartRecord {
        first_window_visible_ms: Some(400),
        main_window_visible_ms: Some(2100),
        app_window_ready_ms: Some(1580),
        splash_close_ms: Some(2100),
        mark_source: Default::default(),
        mark_resolution_ms: 20,
      }),
      idle_cpu: None,
      quiesce: None,
      warm_helper_count: None,
      helper_kill_count: None,
    }
  }

  #[test]
  fn always_contains_a_limitations_section() {
    let result = base_result();
    let rendered = render(&result);
    assert!(rendered.contains("## Limitations"));
  }

  /// taskhub#671 (FR-11): the idle-CPU limitation is a published claim about
  /// how TermTree behaves, hardcoded here rather than in prose, so it goes
  /// stale silently. It must describe render-on-demand, and must not resurrect
  /// the pre-#671 "unconditional ticker" claim.
  #[test]
  fn idle_cpu_limitation_describes_render_on_demand() {
    let mut result = base_result();
    let without = render(&result);
    assert!(!without.contains("idle work"));

    result.subjects.push(termtree_subject());
    let mut sample = cold_start_sample();
    sample.cold_start = None;
    sample.idle_cpu = Some(IdleCpuRecord {
      idle_cpu_percent_of_one_core_median: 0.4,
      idle_cpu_percent_of_one_core_iqr: 0.1,
      sample_count: 30,
      window_state: "foreground-unoccluded".into(),
    });
    result.samples.push(sample);

    let with = render(&result);
    assert!(with.contains("renders on demand"));
    assert!(!with.contains("unconditionally at display rate"));
    // The honest caveat (FR-6) and the other periodic work stay disclosed.
    assert!(with.contains("running or waiting"));
    assert!(with.contains("5 s terminal idle sweep"));
    assert!(with.contains("60 s macOS webview health probe"));
  }

  /// taskhub#672 deleted the 2,000 ms minimum splash and the 500 ms
  /// pre-show sleep this rule used to disclose. The rule survives because a
  /// cold-start bias remains -- self-reported marks and a TermTree-only
  /// `splash-timeout` discard -- so this pins the corrected claim, its code
  /// citations, and the absence of the superseded one.
  #[test]
  fn cold_start_mark_line_only_appears_with_a_termtree_cold_start_row() {
    let mut result = base_result();
    let without = render(&result);
    assert!(!without.contains("self-reported and tail-trimmed"));
    assert!(!without.contains("window_cmd.rs:465-494"));

    result.subjects.push(termtree_subject());
    result.samples.push(cold_start_sample());
    let with = render(&result);
    assert!(with.contains("self-reported and tail-trimmed"));
    assert!(with.contains("src-tauri/src/lib.rs:104-143"));
    assert!(with.contains("src-tauri/src/command/window_cmd.rs:465-494"));
    assert!(with.contains("15 s forced transition"));
    assert!(with.contains("`splash-timeout` sample"));
    // The two superseded claims, verbatim as they were once published.
    assert!(!with.contains("enforces a 2,000 ms minimum splash"));
    assert!(!with.contains("sleeps a fixed 500 ms"));
  }

  #[test]
  fn tmux_partition_disclosure_only_with_a_chromium_subject() {
    let mut result = base_result();
    let without = render(&result);
    assert!(!without.contains("node-pty"));

    result.subjects.push(SubjectProvenance {
      subject_id: "collaborator".into(),
      display_name: "Collaborator".into(),
      subject_version: "0.8.4".into(),
      runtime_family: "chromium-electron".into(),
      bundle_identifier: "com.collaborator.desktop".into(),
      bundle_path: "/Applications/Collaborator.app".into(),
      optional: false,
      seeder: "collaborator".into(),
      seed_method: "canvas.json pre-write".into(),
      calibrated_main_window_area_pt: None,
      version_drift_accepted: false,
      seed_format_verified: false,
    });
    let with = render(&result);
    assert!(with.contains("node-pty"));
  }

  #[test]
  fn fairness_banner_appears_when_reviewer_is_absent() {
    let result = base_result();
    let rendered = render(&result);
    assert!(rendered.contains("NOT REVIEWED"));
  }

  #[test]
  fn fairness_review_renders_when_present() {
    let mut result = base_result();
    result.fairness_review = FairnessReview {
      reviewer: Some("Jane Reviewer".into()),
      reviewed_at: Some("2026-08-25".into()),
      verdict: Some("pass".into()),
      notes: Some("Checked the Limitations section against raw data.".into()),
    };
    let rendered = render(&result);
    assert!(!rendered.contains("NOT REVIEWED"));
    assert!(rendered.contains("Jane Reviewer"));
  }

  #[test]
  fn never_contains_percent_delta_phrasing_or_an_unlabeled_memory_column() {
    let mut result = base_result();
    result.subjects.push(termtree_subject());
    result.samples.push(cold_start_sample());
    result.aggregates.push(Aggregate {
      subject_id: "termtree".into(),
      tier: "fresh-launch".into(),
      metric: "memPhysFootprintBytes".into(),
      median: 100.0,
      q1: 90.0,
      q3: 110.0,
      iqr: 20.0,
      n: 19,
      discarded_count: 1,
      discarded_reasons: Default::default(),
      derivation: "measured".into(),
    });
    let rendered = render(&result);
    assert!(!rendered.contains('%'));
    assert!(!rendered.lines().any(|l| l.trim() == "| Memory |"));
    assert!(!rendered.contains("| Memory | "));
    assert!(!rendered.contains("CPU |\n|---"));
  }

  #[test]
  fn unverified_seed_format_limitation_names_the_subject() {
    let mut result = base_result();
    let without = render(&result);
    assert!(!without.contains("Unverified seed formats"));

    result.subjects.push(SubjectProvenance {
      subject_id: "collaborator".into(),
      display_name: "Collaborator".into(),
      subject_version: "0.8.4".into(),
      runtime_family: "chromium-electron".into(),
      bundle_identifier: "com.collaborator.desktop".into(),
      bundle_path: "/Applications/Collaborator.app".into(),
      optional: false,
      seeder: "collaborator".into(),
      seed_method: "canvas.json pre-write".into(),
      calibrated_main_window_area_pt: None,
      version_drift_accepted: false,
      seed_format_verified: false,
    });
    result.subjects.push(termtree_subject());
    let with = render(&result);
    assert!(with.contains("Unverified seed formats"));
    assert!(with.contains("Collaborator"));
    // TermTree's seeder IS verified -- it must not be named here.
    assert!(!with.contains("Unverified seed formats: TermTree"));
  }

  #[test]
  fn exclusions_and_hold_backs_are_rendered() {
    let result = base_result();
    let rendered = render(&result);
    assert!(rendered.contains("Conductor"));
    assert!(rendered.contains("Nimbalyst"));
  }
}
