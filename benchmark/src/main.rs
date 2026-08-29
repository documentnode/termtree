//! Entry point: dispatches `doctor` | `run` | `render` | `seed` | `restore`
//! (spec §5.9).

use resource_benchmark::bundle_paths::{self, BundlePathOverrides};
use resource_benchmark::cli::{self, Command, DoctorArgs, RunArgs};
use resource_benchmark::run::{self, RunOrchestrator};
use resource_benchmark::seeding;
use resource_benchmark::{
  cold_start, footprint, host_memory, launch_services, provenance, quiesce,
  render, result, scratch_home, subject, tier,
};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

/// The disposable per-run scratch home (spec item 1): every seeder, the
/// karijini log path, and every subject's launch `HOME` key off this --
/// never the runner's real `$HOME`. `--home` (extracted by `main` before
/// subcommand parsing) wins, then `RESOURCE_BENCHMARK_HOME`, then a
/// freshly named directory under the OS temp dir. The directory is
/// created here so every caller can assume it already exists.
fn resolve_scratch_home(cli_override: Option<&str>) -> PathBuf {
  let env_override = std::env::var(scratch_home::HOME_OVERRIDE_ENV).ok();
  let disambiguator = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_nanos();
  let home = scratch_home::resolve_scratch_home(
    cli_override,
    env_override,
    &std::env::temp_dir(),
    std::process::id(),
    disambiguator,
  );
  // Spec item 1 states the property unconditionally: the runner's real
  // application state is never read or written. The default home can never
  // be the real one, but an explicit override can -- refuse it rather than
  // silently destroying the profile they actually use.
  if let Err(error) = scratch_home::reject_real_home(
    &home,
    std::env::var_os("HOME").map(PathBuf::from).as_deref(),
  ) {
    eprintln!("{error}");
    std::process::exit(1);
  }
  if let Err(error) = scratch_home::ensure_scratch_home_exists(&home) {
    eprintln!(
      "warning: could not create scratch home {}: {error}",
      home.display()
    );
  }
  home
}

/// The real logged-in user's home directory, read only for OS-identity
/// lookups that must resolve an actual Directory Services record (e.g.
/// `dscl -read <home> UserShell` for the login shell path used to
/// classify a process as a session root, design §5.2.3). Never used as a
/// write target, a seeder target, or a subject's launch `HOME` -- that is
/// exclusively [`resolve_scratch_home`]'s job (spec item 1: this harness
/// must never silently touch the runner's real profile).
fn real_home_for_identity_lookup() -> PathBuf {
  std::env::var_os("HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("/var/empty"))
}

fn bundle_path_overrides_from_env() -> BundlePathOverrides {
  subject::SUBJECTS
    .iter()
    .filter_map(|s| {
      std::env::var(bundle_paths::env_var_name(s.id))
        .ok()
        .map(|v| (s.id.to_string(), v))
    })
    .collect()
}

fn merged_bundle_path_overrides(
  cli_overrides: BundlePathOverrides,
) -> BundlePathOverrides {
  bundle_paths::merge_cli_over_env(
    bundle_path_overrides_from_env(),
    cli_overrides,
  )
}

fn main() -> ExitCode {
  let raw_args: Vec<String> = std::env::args().skip(1).collect();
  let (home_override, args) = cli::extract_home_override(&raw_args);
  let command = match cli::parse_args(&args) {
    Ok(command) => command,
    Err(error) => {
      eprintln!("error: {}", error.0);
      return ExitCode::FAILURE;
    }
  };

  match command {
    Command::Doctor(doctor_args) => {
      run_doctor(home_override.as_deref(), doctor_args)
    }
    Command::Run(run_args) => run_sweep(run_args, home_override.as_deref()),
    Command::Render {
      result_path,
      out_path,
    } => run_render(&result_path, out_path.as_deref()),
    Command::Seed { subject, sessions } => {
      run_seed(&subject, sessions, home_override.as_deref())
    }
    Command::Restore { subject } => {
      run_restore(subject.as_deref(), home_override.as_deref())
    }
  }
}

/// Checks every prerequisite and changes nothing (spec §5.9, item 6): the
/// five system tools exist, `open` supports the `--env` flag this harness
/// requires to isolate a subject's `HOME` (spec item 1), every
/// non-optional subject bundle is installed at its pinned version, no
/// subject is currently running (spec item 3), the quiesce gate reads, a
/// fresh scratch home can be created, and no leftover seeder backup
/// exists. Exit 1 lists what is missing; unverified-seed-format subjects
/// (spec item 4) are printed as notes, not failures, since they do not
/// block a run from starting.
fn run_doctor(
  cli_home_override: Option<&str>,
  doctor_args: DoctorArgs,
) -> ExitCode {
  let bundle_path_overrides =
    merged_bundle_path_overrides(doctor_args.bundle_path_overrides);
  let mut problems = Vec::new();
  let mut notes = Vec::new();

  for (name, program) in [
    ("lsappinfo", launch_services::LSAPPINFO_PROGRAM),
    ("footprint", footprint::FOOTPRINT_PROGRAM),
    ("vm_stat", host_memory::VM_STAT_PROGRAM),
    ("sysctl", quiesce::SYSCTL_PROGRAM),
    ("pmset", quiesce::PMSET_PROGRAM),
    ("notifyutil", quiesce::NOTIFYUTIL_PROGRAM),
    ("open", cold_start::OPEN_PROGRAM),
  ] {
    if !std::path::Path::new(program).exists() {
      problems.push(format!("missing required tool: {name} ({program})"));
    }
  }

  // Spec item 1: `open --env` is required to isolate every subject's
  // launch `HOME`. This probe is side-effect-free (it only reads
  // `open --help`), so `doctor` changes nothing.
  if !cold_start::supports_env_flag() {
    problems.push(
      "/usr/bin/open does not document --env -- this harness cannot \
       isolate a subject's HOME on this macOS version"
        .to_string(),
    );
  }

  // Spec item 1: confirm a scratch home can actually be created, without
  // leaving one behind (`doctor` changes nothing).
  let probe_home = std::env::temp_dir().join(format!(
    "resource-benchmark-doctor-probe-{}",
    std::process::id()
  ));
  match std::fs::create_dir_all(&probe_home) {
    Ok(()) => {
      let _ = std::fs::remove_dir_all(&probe_home);
    }
    Err(error) => problems.push(format!(
      "could not create a scratch home under {}: {error}",
      std::env::temp_dir().display()
    )),
  }

  let ls_text = launch_services::invoke_lsappinfo_list().unwrap_or_default();
  let ls_entries = launch_services::parse_lsappinfo_list(&ls_text);

  for spec in subject::SUBJECTS {
    if spec.optional {
      continue;
    }
    let bundle_path = bundle_paths::resolve(spec, &bundle_path_overrides);
    if !std::path::Path::new(bundle_path).exists() {
      problems.push(format!(
        "subject not installed: {} ({bundle_path}) -- install it there, or \
         point the harness at an existing install with \
         `--bundle-path {}=<path to the .app>`",
        spec.display_name, spec.id
      ));
      continue;
    }
    match provenance::probe_subject_version(bundle_path) {
      Some(version) if version == spec.expected_version => {}
      Some(version) => problems.push(format!(
        "{}: version drift, expected {} found {version}",
        spec.display_name, spec.expected_version
      )),
      None => problems.push(format!(
        "{}: could not probe version at {bundle_path}",
        spec.display_name
      )),
    }
    // Spec item 3: refuse before seeding if a subject is already running,
    // keyed on bundle identifier.
    if let Some(entry) = ls_entries.iter().find(|entry| {
      entry.pid.is_some()
        && entry.bundle_identifier.as_deref() == Some(spec.bundle_identifier)
    }) {
      problems.push(format!(
        "{} ({}) is already running (pid {}) -- quit it before running the \
         benchmark",
        spec.display_name,
        spec.bundle_identifier,
        entry.pid.unwrap()
      ));
    }
    // Spec item 4: informational only -- does not block a run from
    // starting, but a stranger reading `doctor`'s output should know
    // before they trust an N-session/sustained-use result for it.
    if !spec.seed_format_verified {
      notes.push(format!(
        "{}'s session seeder has not been verified against a real \
         install; its N-session/sustained-use samples will report \
         invalidReason=seed-format-unverified until verified",
        spec.display_name
      ));
    }
  }

  let reading = quiesce::read_quiesce_gate(None);
  if reading.verdict != quiesce::QuiesceVerdict::Pass {
    problems.push(format!(
      "quiesce gate would fail: {}",
      reading.failing_signals.join(", ")
    ));
    // Spec item 6: name the remedy, not just the failing signal.
    for signal in &reading.failing_signals {
      problems.push(format!(
        "  to clear {signal}: {}",
        quiesce::remediation_for_signal(signal)
      ));
    }
  }

  // A default scratch home is a brand-new, never-before-used directory
  // (spec item 1), so it can never have a leftover backup -- checking it
  // would be both vacuous and, worse, a `doctor`-created side effect this
  // command must not have. This check only makes sense, and only runs,
  // against an explicitly reused home (`--home`/`RESOURCE_BENCHMARK_HOME`).
  let explicit_home_override = cli_home_override
    .map(str::to_string)
    .or_else(|| std::env::var(scratch_home::HOME_OVERRIDE_ENV).ok());
  if let Some(home) = explicit_home_override.map(PathBuf::from) {
    let termtree_seeder = seeding::termtree::TermTreeSeeder::production(&home);
    let backup = termtree_seeder
      .state_directory
      .join("state.json.before-resource-benchmark.json");
    if backup.exists() {
      problems.push(format!(
        "leftover TermTree seed backup at {} -- run `resource-benchmark restore`",
        backup.display()
      ));
    }
  }

  for note in &notes {
    println!("doctor note: {note}");
  }

  if problems.is_empty() {
    println!("doctor: all checks passed.");
    ExitCode::SUCCESS
  } else {
    for problem in &problems {
      println!("doctor: {problem}");
    }
    ExitCode::FAILURE
  }
}

fn run_render(result_path: &str, out_path: Option<&str>) -> ExitCode {
  let path = PathBuf::from(result_path);
  let result = match result::read_result_file(&path) {
    Ok(result) => result,
    Err(error) => {
      eprintln!("error reading {result_path}: {error}");
      return ExitCode::FAILURE;
    }
  };
  let markdown = render::render(&result);
  let destination = out_path
    .map(PathBuf::from)
    .unwrap_or_else(|| path.with_extension("md"));
  if let Err(error) = std::fs::write(&destination, markdown) {
    eprintln!("error writing {}: {error}", destination.display());
    return ExitCode::FAILURE;
  }
  println!("rendered {}", destination.display());
  ExitCode::SUCCESS
}

fn seeded_repo(repo_path_override: Option<&str>) -> seeding::SeededRepo {
  seeding::SeededRepo {
    url: std::env::var("RESOURCE_BENCHMARK_REPO_URL")
      .unwrap_or_else(|_| "https://github.com/example/benchmark-repo".into()),
    commit: std::env::var("RESOURCE_BENCHMARK_REPO_COMMIT")
      .unwrap_or_else(|_| "unknown".into()),
    local_path: repo_path_override
      .map(str::to_string)
      .or_else(|| std::env::var("RESOURCE_BENCHMARK_REPO_PATH").ok())
      .unwrap_or_else(|| "/Users/Shared/benchmark-repo".into()),
  }
}

fn agent_cli_pin(
  agent_cli_path_override: Option<&str>,
) -> seeding::AgentCliPin {
  let executable_path = agent_cli_path_override
    .map(str::to_string)
    .or_else(|| std::env::var("RESOURCE_BENCHMARK_AGENT_CLI_PATH").ok())
    .unwrap_or_else(|| "/usr/local/bin/claude".into());
  let probed = provenance::probe_agent_cli_version(&executable_path);
  seeding::AgentCliPin {
    name: probed.name,
    version: probed.version,
    executable_path: probed.executable_path,
  }
}

fn run_seed(
  subject_id: &str,
  sessions: u32,
  cli_home_override: Option<&str>,
) -> ExitCode {
  let home = resolve_scratch_home(cli_home_override);
  let repo = seeded_repo(None);
  let agent = agent_cli_pin(None);
  let result =
    seeding::seed_subject(&home, subject_id, sessions, &repo, &agent);
  match result {
    Ok(plan) => {
      println!(
        "seeded {subject_id} with {sessions} sessions via {} (scratch home: {})",
        plan.method,
        home.display()
      );
      ExitCode::SUCCESS
    }
    Err(error) => {
      eprintln!("error seeding {subject_id}: {error}");
      ExitCode::FAILURE
    }
  }
}

fn run_restore(
  subject_id: Option<&str>,
  cli_home_override: Option<&str>,
) -> ExitCode {
  let home = resolve_scratch_home(cli_home_override);
  let subjects: Vec<&str> = match subject_id {
    Some(id) => vec![id],
    None => vec!["termtree", "collaborator", "codenomad-electron", "diri"],
  };
  let mut failed = false;
  for id in subjects {
    let result = seeding::restore_subject(&home, id);
    if let Err(error) = result {
      eprintln!("error restoring {id}: {error}");
      failed = true;
    } else {
      println!("restored {id}");
    }
  }
  if failed {
    ExitCode::FAILURE
  } else {
    ExitCode::SUCCESS
  }
}

/// A full subject/tier sweep (spec FR-16, design §5.8/§7). This is the live
/// orchestration path: it is exercised via the smoke sweep described in the
/// README, not unit tests (design §11's explicit scope for what a
/// measurement harness cannot unit-test). A refusal (quiesce failure,
/// missing subject, version drift without `--allow-version-drift`,
/// `open --env` unsupported, a subject already running) prints an
/// actionable message and exits non-zero -- this command never reports
/// success without either performing the sweep or being told not to
/// (`--out`/`--resume` control where results land).
fn run_sweep(run_args: RunArgs, cli_home_override: Option<&str>) -> ExitCode {
  let subjects = match run::select_subjects(
    run_args.subjects.as_deref(),
    run_args.allow_optional_subjects,
  ) {
    Ok(subjects) => subjects,
    Err(error) => {
      eprintln!("error: {error}");
      return ExitCode::FAILURE;
    }
  };
  let tiers = run_args
    .tiers
    .clone()
    .unwrap_or_else(|| tier::DEFAULT_TIERS.to_vec());
  let settings = run::build_settings(run_args.repetitions);
  let home = resolve_scratch_home(cli_home_override);
  let repo = seeded_repo(run_args.repo_path.as_deref());
  let agent = agent_cli_pin(run_args.agent_cli_path.as_deref());
  let bundle_path_overrides =
    merged_bundle_path_overrides(run_args.bundle_path_overrides.clone());
  let login_shell_path = provenance::probe_login_shell_path(
    &real_home_for_identity_lookup().to_string_lossy(),
  );
  let out_path =
    run_args
      .out_path
      .clone()
      .map(PathBuf::from)
      .unwrap_or_else(|| {
        PathBuf::from(format!(
          "results/{}.json",
          provenance::iso_timestamp_now().replace([':', 'Z'], "-")
        ))
      });
  if let Some(parent) = out_path.parent() {
    let _ = std::fs::create_dir_all(parent);
  }

  run::install_interrupt_handler();

  let orchestrator = RunOrchestrator {
    subjects,
    tiers,
    settings,
    home,
    repo,
    agent,
    allow_version_drift: run_args.allow_version_drift,
    out_path: out_path.clone(),
    bundle_path_overrides,
    login_shell_path,
  };

  let resume_path = run_args.resume_path.as_ref().map(PathBuf::from);
  match orchestrator.run(resume_path.as_deref()) {
    Ok(result) => {
      let destination = resume_path.unwrap_or(out_path);
      let markdown = render::render(&result);
      let markdown_path = destination.with_extension("md");
      if let Err(error) = std::fs::write(&markdown_path, markdown) {
        eprintln!("error writing {}: {error}", markdown_path.display());
        return ExitCode::FAILURE;
      }
      println!(
        "resource-benchmark run: wrote {} and {}",
        destination.display(),
        markdown_path.display()
      );
      ExitCode::SUCCESS
    }
    Err(refusal) => {
      eprintln!("resource-benchmark run: refused to start: {refusal}");
      ExitCode::FAILURE
    }
  }
}
