//! Hand-rolled argument parsing (design §4.3: no `clap`, following
//! `tools/update-feed`'s precedent) → a `Command` enum plus `RunSettings`
//! overrides (spec §5.9's command-line surface).

use crate::bundle_paths::BundlePathOverrides;
use crate::tier::Tier;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
  Doctor(DoctorArgs),
  Run(RunArgs),
  Render {
    result_path: String,
    out_path: Option<String>,
  },
  Seed {
    subject: String,
    sessions: u32,
  },
  Restore {
    subject: Option<String>,
  },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DoctorArgs {
  pub bundle_path_overrides: BundlePathOverrides,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunArgs {
  pub subjects: Option<Vec<String>>,
  pub tiers: Option<Vec<Tier>>,
  pub repetitions: Option<u32>,
  pub out_path: Option<String>,
  pub resume_path: Option<String>,
  pub allow_optional_subjects: bool,
  pub allow_version_drift: bool,
  pub bundle_path_overrides: BundlePathOverrides,
  pub repo_path: Option<String>,
  pub agent_cli_path: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct CliError(pub String);

/// Strips a global `--home <path>` override out of the raw argument list
/// before subcommand parsing, wherever it appears (spec item 1) -- it
/// applies to every subcommand that touches the filesystem (`doctor`,
/// `run`, `seed`, `restore`), so it is not scoped to one subcommand's own
/// flag set the way `--bundle-path` is. Returns the override, if any, and
/// the remaining arguments for [`parse_args`] to parse as before.
pub fn extract_home_override(args: &[String]) -> (Option<String>, Vec<String>) {
  let mut home = None;
  let mut rest = Vec::with_capacity(args.len());
  let mut i = 0;
  while i < args.len() {
    if args[i] == "--home" {
      home = args.get(i + 1).cloned();
      i += 2;
    } else {
      rest.push(args[i].clone());
      i += 1;
    }
  }
  (home, rest)
}

pub fn parse_args(args: &[String]) -> Result<Command, CliError> {
  let Some((subcommand, rest)) = args.split_first() else {
    return Err(CliError(
      "usage: resource-benchmark <doctor|run|render|seed|restore>".into(),
    ));
  };

  match subcommand.as_str() {
    "doctor" => Ok(Command::Doctor(DoctorArgs {
      bundle_path_overrides: parse_bundle_path_overrides(rest)?,
    })),
    "run" => Ok(Command::Run(parse_run_args(rest)?)),
    "render" => parse_render_args(rest),
    "seed" => parse_seed_args(rest),
    "restore" => Ok(Command::Restore {
      subject: value_of(rest, "--subject"),
    }),
    other => Err(CliError(format!("unknown subcommand: {other}"))),
  }
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
  args
    .iter()
    .position(|a| a == flag)
    .and_then(|i| args.get(i + 1))
    .cloned()
}

fn flag_present(args: &[String], flag: &str) -> bool {
  args.iter().any(|a| a == flag)
}

/// Repeated `--bundle-path <subject-id>=<path>` flags (spec item 5) --
/// e.g. `--bundle-path termtree=/Users/dev/Apps/TermTree.app`.
fn parse_bundle_path_overrides(
  args: &[String],
) -> Result<BundlePathOverrides, CliError> {
  let mut overrides = BundlePathOverrides::new();
  let mut i = 0;
  while i < args.len() {
    if args[i] == "--bundle-path" {
      let value = args.get(i + 1).ok_or_else(|| {
        CliError("--bundle-path requires <subject-id>=<path>".into())
      })?;
      let (id, path) = value.split_once('=').ok_or_else(|| {
        CliError(format!(
          "invalid --bundle-path (expected <subject-id>=<path>): {value}"
        ))
      })?;
      overrides.insert(id.to_string(), path.to_string());
      i += 2;
    } else {
      i += 1;
    }
  }
  Ok(overrides)
}

fn parse_run_args(args: &[String]) -> Result<RunArgs, CliError> {
  let subjects = value_of(args, "--subjects")
    .map(|v| v.split(',').map(str::to_string).collect());
  let tiers = value_of(args, "--tiers")
    .map(|v| {
      v.split(',')
        .map(|t| {
          Tier::parse(t).ok_or_else(|| CliError(format!("unknown tier: {t}")))
        })
        .collect::<Result<Vec<_>, _>>()
    })
    .transpose()?;
  let repetitions = value_of(args, "--repetitions")
    .map(|v| {
      v.parse::<u32>()
        .map_err(|_| CliError(format!("invalid --repetitions: {v}")))
    })
    .transpose()?;
  Ok(RunArgs {
    subjects,
    tiers,
    repetitions,
    out_path: value_of(args, "--out"),
    resume_path: value_of(args, "--resume"),
    allow_optional_subjects: flag_present(args, "--allow-optional-subjects"),
    allow_version_drift: flag_present(args, "--allow-version-drift"),
    bundle_path_overrides: parse_bundle_path_overrides(args)?,
    repo_path: value_of(args, "--repo-path"),
    agent_cli_path: value_of(args, "--agent-cli-path"),
  })
}

fn parse_render_args(args: &[String]) -> Result<Command, CliError> {
  let result_path = args.first().cloned().ok_or_else(|| {
    CliError(
      "usage: resource-benchmark render <result.json> [--out <file.md>]".into(),
    )
  })?;
  Ok(Command::Render {
    result_path,
    out_path: value_of(args, "--out"),
  })
}

fn parse_seed_args(args: &[String]) -> Result<Command, CliError> {
  let subject = value_of(args, "--subject")
    .ok_or_else(|| CliError("seed requires --subject <id>".into()))?;
  let sessions = value_of(args, "--sessions")
    .ok_or_else(|| CliError("seed requires --sessions <n>".into()))?
    .parse::<u32>()
    .map_err(|_| CliError("invalid --sessions".into()))?;
  Ok(Command::Seed { subject, sessions })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
  }

  #[test]
  fn parses_doctor() {
    assert_eq!(
      parse_args(&args(&["doctor"])),
      Ok(Command::Doctor(DoctorArgs::default()))
    );
  }

  #[test]
  fn parses_doctor_bundle_path_overrides() {
    let command = parse_args(&args(&[
      "doctor",
      "--bundle-path",
      "termtree=/Users/dev/Apps/TermTree.app",
    ]))
    .unwrap();
    let Command::Doctor(doctor_args) = command else {
      panic!()
    };
    assert_eq!(
      doctor_args
        .bundle_path_overrides
        .get("termtree")
        .map(String::as_str),
      Some("/Users/dev/Apps/TermTree.app")
    );
  }

  #[test]
  fn parses_run_with_subjects_and_tiers() {
    let command = parse_args(&args(&[
      "run",
      "--subjects",
      "termtree,collaborator",
      "--tiers",
      "fresh-launch,n-session-5",
      "--repetitions",
      "10",
    ]))
    .unwrap();
    assert_eq!(
      command,
      Command::Run(RunArgs {
        subjects: Some(vec!["termtree".into(), "collaborator".into()]),
        tiers: Some(vec![Tier::FreshLaunch, Tier::NSession(5)]),
        repetitions: Some(10),
        out_path: None,
        resume_path: None,
        allow_optional_subjects: false,
        allow_version_drift: false,
        bundle_path_overrides: BundlePathOverrides::new(),
        repo_path: None,
        agent_cli_path: None,
      })
    );
  }

  #[test]
  fn parses_run_bundle_path_overrides_for_multiple_subjects() {
    let command = parse_args(&args(&[
      "run",
      "--bundle-path",
      "termtree=/Users/dev/Apps/TermTree.app",
      "--bundle-path",
      "diri=/Users/dev/Apps/diri.app",
    ]))
    .unwrap();
    let Command::Run(run_args) = command else {
      panic!()
    };
    assert_eq!(run_args.bundle_path_overrides.len(), 2);
    assert_eq!(
      run_args
        .bundle_path_overrides
        .get("diri")
        .map(String::as_str),
      Some("/Users/dev/Apps/diri.app")
    );
  }

  #[test]
  fn rejects_a_bundle_path_override_missing_the_equals_separator() {
    let error =
      parse_args(&args(&["run", "--bundle-path", "termtree"])).unwrap_err();
    assert!(error.0.contains("--bundle-path"));
  }

  #[test]
  fn parses_run_repo_path_and_agent_cli_path() {
    let command = parse_args(&args(&[
      "run",
      "--repo-path",
      "/tmp/my-repo",
      "--agent-cli-path",
      "/opt/bin/claude",
    ]))
    .unwrap();
    let Command::Run(run_args) = command else {
      panic!()
    };
    assert_eq!(run_args.repo_path.as_deref(), Some("/tmp/my-repo"));
    assert_eq!(run_args.agent_cli_path.as_deref(), Some("/opt/bin/claude"));
  }

  #[test]
  fn extracts_a_global_home_override_from_anywhere_in_argv() {
    let (home, rest) = extract_home_override(&args(&[
      "run",
      "--home",
      "/tmp/scratch-home",
      "--repetitions",
      "2",
    ]));
    assert_eq!(home.as_deref(), Some("/tmp/scratch-home"));
    assert_eq!(rest, args(&["run", "--repetitions", "2"]));
  }

  #[test]
  fn no_home_override_present_leaves_args_unchanged() {
    let (home, rest) = extract_home_override(&args(&["doctor"]));
    assert_eq!(home, None);
    assert_eq!(rest, args(&["doctor"]));
  }

  #[test]
  fn parses_run_with_no_arguments_as_the_full_sweep() {
    let command = parse_args(&args(&["run"])).unwrap();
    assert_eq!(command, Command::Run(RunArgs::default()));
  }

  #[test]
  fn parses_run_flags() {
    let command = parse_args(&args(&[
      "run",
      "--allow-optional-subjects",
      "--allow-version-drift",
    ]))
    .unwrap();
    let Command::Run(run_args) = command else {
      panic!()
    };
    assert!(run_args.allow_optional_subjects);
    assert!(run_args.allow_version_drift);
  }

  #[test]
  fn rejects_an_unknown_tier() {
    let error =
      parse_args(&args(&["run", "--tiers", "not-a-tier"])).unwrap_err();
    assert!(error.0.contains("not-a-tier"));
  }

  #[test]
  fn parses_render() {
    let command =
      parse_args(&args(&["render", "results/run.json", "--out", "run.md"]))
        .unwrap();
    assert_eq!(command, Command::Render {
      result_path: "results/run.json".into(),
      out_path: Some("run.md".into()),
    });
  }

  #[test]
  fn parses_seed() {
    let command =
      parse_args(&args(&["seed", "--subject", "termtree", "--sessions", "5"]))
        .unwrap();
    assert_eq!(command, Command::Seed {
      subject: "termtree".into(),
      sessions: 5
    });
  }

  #[test]
  fn parses_restore_with_and_without_a_subject() {
    assert_eq!(parse_args(&args(&["restore"])).unwrap(), Command::Restore {
      subject: None
    });
    assert_eq!(
      parse_args(&args(&["restore", "--subject", "termtree"])).unwrap(),
      Command::Restore {
        subject: Some("termtree".into())
      }
    );
  }

  #[test]
  fn rejects_an_empty_argument_list() {
    assert!(parse_args(&[]).is_err());
  }

  #[test]
  fn rejects_an_unknown_subcommand() {
    assert!(parse_args(&args(&["frobnicate"])).is_err());
  }
}
