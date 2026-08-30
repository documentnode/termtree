//! The only module in this crate that spawns a child process.
//!
//! Every external tool this harness shells out to (`lsappinfo`, `footprint`,
//! `vm_stat`, `sysctl`, `pmset`, `notifyutil`, `open`, `osascript`, `/bin/date`)
//! goes through [`run_capture`]. No other module spawns a process, and this
//! module never parses a tool's output -- that split is what makes every
//! parser fixture-testable in isolation (design §11).
//!
//! # The zsh-vs-bash word-splitting hazard
//!
//! [`run_capture`] takes an argument **vector** (`&[&str]`) and calls
//! `std::process::Command`, which spawns no shell -- so this crate is immune
//! to shell word-splitting by construction. Anyone re-running one of this
//! harness's invocations *by hand* is not: under **zsh** (this project's
//! default shell) an unquoted `$PIDS` variable is not word-split the way it
//! is under bash, so `footprint -j out.json $PIDS` silently passes the whole
//! PID list as a single argument and measures only the leading PID, with an
//! empty `errors` array -- no error, just a quietly wrong number. Every
//! copy-pasteable command in this crate's README repeats this warning next
//! to the invocation it applies to.
//!
//! [`run_capture`] never builds or participates in a pipeline: piping a
//! tool's stdout to a consumer that closes early (e.g. `| head`) can SIGPIPE
//! the tool mid-write and leave a truncated output file (reproduced against
//! `footprint -j` at exactly 12,288 bytes during this crate's design). stdout
//! is always captured into an owned buffer instead.

use std::process::Command;

#[derive(Debug)]
pub struct ExecError {
  pub program: String,
  pub args: Vec<String>,
  pub message: String,
}

impl std::fmt::Display for ExecError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{} {}: {}",
      self.program,
      self.args.join(" "),
      self.message
    )
  }
}

impl std::error::Error for ExecError {}

/// Run `program` with `args` and capture stdout as a UTF-8 string.
///
/// `args` is always an argument vector, never an interpolated string, and no
/// shell is ever invoked -- see the module doc for why both matter. A
/// non-zero exit status is not itself an error: several callers (e.g. the
/// quiesce gate's `sysctl -n`) treat a particular exit code as meaningful.
/// Callers that care about exit status read `ExecOutput::status`.
pub fn run_capture(
  program: &str,
  args: &[&str],
) -> Result<ExecOutput, ExecError> {
  let output =
    Command::new(program)
      .args(args)
      .output()
      .map_err(|e| ExecError {
        program: program.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        message: e.to_string(),
      })?;
  let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
  let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
  Ok(ExecOutput {
    status: output.status.code(),
    stdout,
    stderr,
  })
}

pub struct ExecOutput {
  pub status: Option<i32>,
  pub stdout: String,
  pub stderr: String,
}

impl ExecOutput {
  pub fn success(&self) -> bool {
    self.status == Some(0)
  }
}
