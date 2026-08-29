# resource-benchmark

A standalone Rust harness that measures resident memory, cold start /
time-to-interactive, idle CPU, and memory at 5/10/20 live agent sessions for
TermTree against comparable agent-orchestrator apps (CodeNomad's Electron
and Tauri builds, Collaborator, and optionally diri) on one fixed macOS
host, correcting for the `launchd`-vs-child-process attribution asymmetry
that makes every off-the-shelf measurement tool favor TermTree by
construction.

This directory is the harness's **canonical home** — it was developed in
TermTree's private repository and moved here so it can be run, read, and
audited by anyone, not mirrored from somewhere else. It is also the one
deliberate exception to this repo's otherwise source-free posture: see
[Licence](#licence) below.

## macOS only, structurally

This harness is **macOS-only**, not just untested elsewhere. Every
measurement it takes goes through a macOS-specific unprivileged tool with
no Linux or Windows equivalent: `lsappinfo` (LaunchServices attribution),
`footprint` (`phys_footprint`, the memory metric this benchmark exists to
get right), `vm_stat` (host memory pressure and compression), `notifyutil`
(thermal pressure), and `pmset` (power source). A Linux or Windows user
running this crate will get a build error or a run refusal, not a
degraded-but-working experience — there is no portable fallback path for
any of these five tools, by design (see each module's doc comment under
`src/`).

## What is, and is not, comparable across machines

The **method** — attribution by deduplicated union, `phys_footprint`
instead of RSS, an external monotonic clock for cold start, the
foreground/unoccluded idle-CPU sample — travels to any Mac. So do the
**within-run ratios** between subjects measured back-to-back on the same
host under the same load.

Absolute numbers do not travel. Megabytes and milliseconds measured on one
CPU, RAM size, macOS build, and background-load state will not reproduce
on different hardware. Every result file embeds its own machine spec, OS
build, and quiesce readings (`provenance`, `quiesce` in the JSON) precisely
so a rerun on different hardware is **self-disclosing** — a reader can see
immediately that the numbers came from a different host — rather than
silently compared against a number it was never measured against.

## Licence

The rest of this public repository publishes no product source. This
directory is the one deliberate exception: a benchmark nobody can run is
an assertion, not evidence. `benchmark/` ships with its own
[`LICENSE`](./LICENSE) (Apache-2.0), scoped to this directory only.
Nothing else in this repo is licensed for reuse.

## The published run's machine

No run has been published from this harness yet. Once one is, this section
states the exact host it was measured on: **Apple M1, 8 cores, 16 GB RAM,
macOS 15.7.4 (24G517)** is the specified measurement host for the first
run. **If your numbers come from different hardware or a different OS
build, say so next to them.**

The `fixtures/` directory's captures (used by this crate's own test suite,
not by a live run) were taken on that same machine and OS build.

## Prerequisites

- macOS only (see above).
- No root/`sudo` required anywhere in the default path. `footprint` and
  `lsappinfo` need none; `powermetrics`/`launchctl procinfo` are never
  used.
- `/usr/bin/open` must document `--env` (used to isolate each subject's
  `HOME`, see [Disposable scratch home](#disposable-per-run-scratch-home)
  below). `man open`'s own page on the development host is dated April
  2017 and has documented `--env` for as long as this project has checked,
  which suggests it has been available since roughly macOS 10.12/10.13 —
  but that could not be confirmed as an exact minimum version, so
  `resource-benchmark doctor` **probes for the flag at runtime** instead of
  asserting an unverified version constant, and the harness refuses to
  start rather than silently falling back to your real `$HOME` if the
  probe fails.
- Every non-optional subject (TermTree, CodeNomad Electron, CodeNomad
  Tauri, Collaborator) installed at its pinned version — `resource-benchmark
  doctor` checks this and prints exactly what is missing or drifted. If
  your install lives outside `/Applications`, pass
  `--bundle-path <subject-id>=<path>` (repeatable) or set
  `RESOURCE_BENCHMARK_BUNDLE_PATH_<SUBJECT_ID>` (e.g.
  `RESOURCE_BENCHMARK_BUNDLE_PATH_CODENOMAD_ELECTRON`); a CLI flag wins
  over the matching environment variable. diri is optional; pass
  `--allow-optional-subjects` to include it if installed.
- **No subject already running.** The harness refuses to start (or to seed
  a given subject) if any selected subject's **bundle identifier** already
  has a live LaunchServices entry — checked by identifier, not display
  name, because two differently named bundles can declare the same
  identifier, and a single-instance plugin then hands a new launch off to
  the already-running instance, which exits within seconds. Quit the named
  app first.
- An agent CLI (e.g. `claude`) installed and resolvable; its path is
  `--agent-cli-path <path>` or `RESOURCE_BENCHMARK_AGENT_CLI_PATH`
  (defaults to `/usr/local/bin/claude`).
- A seeded repository checked out at a fixed commit; its local path is
  `--repo-path <path>` or `RESOURCE_BENCHMARK_REPO_PATH` (its URL/commit
  are `RESOURCE_BENCHMARK_REPO_URL` / `RESOURCE_BENCHMARK_REPO_COMMIT`).
- A quiesced machine: on AC power, no swap pressure, nominal thermal
  pressure. `resource-benchmark doctor` reads all five quiesce signals and
  reports whether a run would be allowed to start right now.

## Disposable per-run scratch home

Every subject is seeded, launched, and measured with `HOME` pointed at a
**fresh, disposable directory this harness owns** — never your real
`$HOME`. By default that is a brand-new directory under the OS temp
directory, created fresh for the run and never reused. You can point it
somewhere specific with `--home <path>` (highest precedence) or
`RESOURCE_BENCHMARK_HOME`; if you do, that is your choice to make, not this
harness deciding it for you.

This is what makes it safe to run this harness on a machine with a real
TermTree/Collaborator/CodeNomad/diri install and real user data on it: the
launched subject's entire on-disk state — including TermTree's
`state.json` — lives under the scratch home, not under your real profile.
`seeding/termtree.rs`'s `expected_scratch_state_path` refuses to write
anywhere outside the current scratch home, which includes refusing your
real production profile.

## Commands

Run everything from this directory:

```sh
# Preflight everything; changes nothing.
cargo run --release -- doctor

# A single documented command for a full subject/tier sweep on an
# already-quiesced machine. Many hours; must own the machine.
cargo run --release -- run

# A smaller, faster sweep for trying the harness out.
cargo run --release -- run --subjects termtree,collaborator \
  --tiers fresh-launch,n-session-5 --repetitions 5

# Render the Markdown table from a result file -- a pure function of the
# JSON, never hand-edited.
cargo run --release -- render results/<runId>.json --out results/<runId>.md

# Seed a subject's on-disk state without running a full sweep (for manual
# inspection), and undo any seeder state afterwards.
cargo run --release -- seed --subject termtree --sessions 5
cargo run --release -- restore --subject termtree
```

### The CodeNomad Electron-vs-Tauri pair, standalone

CodeNomad ships the same MIT-licensed codebase as two builds — one on
Electron, one on Tauri — with TermTree **not a participant**. That pair is
a first-class standalone comparison: same source, two runtimes, so its
fairness is checkable without trusting this project's TermTree numbers at
all. The exact command:

```sh
cargo run --release -- run \
  --subjects codenomad-electron,codenomad-tauri \
  --tiers fresh-launch,n-session-5,n-session-10,n-session-20
```

## Seed-format verification status

Each subject's session seeder writes whatever on-disk/CLI state makes that
subject start with `n` live sessions (`src/seeding/`). **Only TermTree's
seeder has been checked against a real install** — its `state.json` shape
is pinned against the app's own persistence code
(`seeding/termtree.rs`'s tests). **Collaborator's, CodeNomad's, and
diri's seed formats have never been run against a real install** — assume
they are wrong until proven otherwise; see each module's doc comment
(`seeding/collaborator.rs`, `seeding/codenomad.rs`, `seeding/diri.rs`) for
exactly what is unverified.

The harness does not silently trust an unverified format: every
N-session/sustained-use sample for a subject whose seed format is
unverified reports `invalidReason: "seed-format-unverified"` in the result
file and is excluded from the published aggregate, and `doctor` prints a
note naming which subjects this applies to. If you can verify one of these
three against a real install, flip `seed_format_verified` to `true` on
that subject's entry in `src/subject.rs` and say how you verified it.

## Runtime self-validation

A public build of TermTree could be any version, so this harness detects
drift in its own hardcoded assumptions rather than silently mismeasuring:

- **Cold-start log marks** (`log_marks.rs`): if `karijini.log` advances
  during a TermTree launch but none of the three hardcoded messages match
  a single line, the sample reports
  `invalidReason: "termtree-log-marks-unrecognized"` instead of silently
  leaving the cold-start fields `null`.
- **App data directory**: if TermTree launches but never creates
  `DocumentNode/TermTree` under the scratch home, the sample reports
  `invalidReason: "app-data-dir-not-created"` — the app's data-directory
  convention has likely changed.
- **Seed consumption**: see [Seed-format verification status](#seed-format-verification-status)
  above.

## Two hazards to know before you run anything by hand

**1. `footprint`'s `-p` flag is ambiguous — never use it.** `footprint -h`
documents `-p, --proc <name>` and `-p, --pid <pid>` sharing one short flag;
name resolution wins and it is a *partial* match. Verified on this
project's own measurement host: `footprint -j out.json -p 1` measured four
unrelated `1Password` processes, not PID 1. This harness always builds
`--pid <pid>` (the unambiguous long form), once per requested PID, and
never the short form. If you ever invoke `footprint` by hand while
reproducing a result, do the same.

**2. Under zsh, an unquoted PID-list variable is not word-split.** If you
build a command like `footprint -j out.json $PIDS` by hand, **zsh** (this
project's default shell) passes the whole list as one argument, silently
measuring only the leading PID with an empty `errors` array — no error,
just a quietly wrong number. (bash *does* word-split, so the same command
works differently there.) This harness never does this: every invocation
goes through `exec.rs`'s `run_capture(program, args: &[&str])`, an argument
vector via `std::process::Command`, which spawns no shell and is immune by
construction. This warning is only for anyone re-running one of this
harness's `footprint`/`lsappinfo` invocations by hand.

## Testing

```sh
cd benchmark
cargo test
cargo clippy --all-targets -- -D warnings
cargo +nightly fmt -- --check
```

This crate is its own `[workspace]`, independent of anything else in this
repo, so it has no build-system integration to keep in sync here.

Live measurement is not unit-testable; parsing is. `fixtures/` holds
captured real tool output (`lsappinfo list`, `footprint -j`, `vm_stat`,
`karijini.log` lines, and the five quiesce signals) that every parser's
tests run against. Launching subjects, live `footprint`/`vm_stat`
invocation, `CGWindowList` polling, and seeding third-party apps are not
unit-tested — those are covered by `doctor` and a one-subject smoke sweep
instead.

For the manual pass — what `doctor` must tell you, how to confirm the scratch
home never touches your real profile, and the seed/restore round-trip — follow
[`doc/resource-benchmark-harness-testing.md`](doc/resource-benchmark-harness-testing.md).
It also records what is deliberately *not* verifiable without a quiesced
machine.
