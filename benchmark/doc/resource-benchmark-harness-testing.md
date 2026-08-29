# Resource benchmark harness

## Scope

Covers the harness's safety, refusal, and portability behaviour — the parts a
third party hits before they ever produce a number:

- `doctor` naming every unmet prerequisite **and its remedy**, while changing
  nothing.
- The disposable per-run scratch home: the runner's real application profiles
  are never read or written.
- Refusing to start when a subject is already running, keyed on **bundle
  identifier**.
- `seed` / `restore` round-tripping a profile byte-for-byte.
- `run` refusing on an unquiesced machine and measuring nothing.
- Per-subject bundle-path overrides and version-drift reporting.

**Deliberately excluded.** A full measurement sweep is not verifiable from a
testing guide: it needs a rebooted machine with nothing else running, for
hours. See *Not verifiable here*. This guide proves the harness is safe to
hand a stranger; it does not prove any published number.

**Risk being managed.** This harness seeds application state and launches real
applications. Its predecessor wrote directly into the operator's live
TermTree, Collaborator and CodeNomad profiles. Every scenario below exists to
prove that is no longer possible.

## Verification surface

`backend` — the harness is a command-line binary with no UI. Every assertion
is made at its CLI boundary: exit code, stdout, and the filesystem state it
did or did not change. There is no browser surface. The one native concern
(launching a real `.app` with an isolated `HOME`) is covered under *Not
verifiable here*, because it requires quitting applications the operator is
using.

## Setup

- Repository: this repo (`documentnode/termtree`). The harness is a
  standalone `[workspace]` crate at `benchmark/` with no path dependencies —
  it needs no other repo checked out.
- Dependencies: Rust stable, plus the nightly toolchain for the format check.
  The macOS tools it shells out to — `lsappinfo`, `footprint`, `vm_stat`,
  `sysctl`, `pmset`, `notifyutil`, `open` — ship with a stock macOS and none
  need `sudo`.
- macOS only. `open --env` must be supported; `doctor` probes for it.

```bash
cd benchmark
cargo build --release
B=./target/release/resource-benchmark
```

**You no longer need to export a scratch `HOME`.** Earlier versions of this
harness read your real `$HOME` and required you to remember to override it.
It now creates a fresh disposable home under the OS temp directory for every
invocation, and **refuses** an explicit `--home` that points at your real home
directory. Scenario 3 proves both halves.

## Automated checks

Run before the manual pass. They complement it; they do not replace it.

```bash
cd benchmark
cargo test                                 # expect: all pass, exit 0
cargo clippy --all-targets -- -D warnings  # expect: exit 0
cargo +nightly fmt -- --check              # expect: no output, exit 0
```

Nightly is required for the format check — `rustfmt.toml` sets
`unstable_features = true`.

## Manual testing

### Scenario 1 — `doctor` names every unmet prerequisite and its remedy

1. Run `$B doctor`.
2. Read every line. For each unmet prerequisite, confirm the output says what
   to do about it, not only what is wrong:
   - a not-installed subject names the expected path **and** the
     `--bundle-path <id>=<path>` override, e.g.
     `subject not installed: Collaborator (/Applications/Collaborator.app) --
     install it there, or point the harness at an existing install with
     `--bundle-path collaborator=<path to the .app>``
   - a failing quiesce gate is followed by one indented `to clear <signal>:`
     line per failing signal, each naming a concrete command or action.
3. Confirm the exit code is `1` when any problem was printed:
   `$B doctor; echo "exit=$?"`. With no problems it prints
   `doctor: all checks passed.` and exits `0`.
4. **Confirm it changed nothing.** `doctor` creates and deletes a scratch-home
   probe directory. Afterwards, `ls $TMPDIR | grep resource-benchmark-doctor-probe`
   must return nothing.

### Scenario 2 — `doctor` detects an already-running subject by bundle identifier

1. Launch TermTree normally (or any subject in the registry).
2. Run `$B doctor`.
3. Expect a line naming the app, its **bundle identifier**, and its pid:
   `TermTree (com.termtree.desktop) is already running (pid NNNNN) -- quit it
   before running the benchmark`.
4. Confirm the pid matches: `lsappinfo list | grep -A5 TermTree | grep 'pid ='`.

Bundle identifier rather than app name is the point. Two differently named
bundles can declare the same identifier; the second launch is then handed off
to the running instance by the single-instance plugin and exits within
seconds, having measured nothing. `open -n` does not bypass that.

### Scenario 3 — the scratch home is disposable, and the real profile is never touched

This is the guide's most important scenario.

1. Record your real profile's state before anything:
   ```bash
   REAL="$HOME/Library/Application Support/DocumentNode/TermTree"
   ls "$REAL" | grep -c before-resource-benchmark   # expect: 0
   ```
2. Seed with **no** `--home`:
   ```bash
   $B seed --subject termtree --sessions 3
   ```
3. Expect exit `0` and a message naming the scratch home it chose, e.g.
   `seeded termtree with 3 sessions via production state.json pre-write
   (scratch home: /var/folders/.../resource-benchmark-home-<pid>-<nanos>)`.
4. Confirm the seeded fixture landed **there**, not in your profile:
   ```bash
   S=<the scratch home from step 3>
   python3 -c "import json;print(json.load(open('$S/Library/Application Support/DocumentNode/TermTree/state.json'))['tree']['label'])"
   ```
   Expect `resource-benchmark-root`.
5. Confirm your real profile was not written:
   ```bash
   ls "$REAL" | grep -c before-resource-benchmark   # expect: still 0
   ```
   The seeder **always** writes a `state.json.before-resource-benchmark.json`
   backup before touching a state file, so the absence of that file is proof
   it never wrote there.

   Do **not** use the real `state.json`'s checksum for this check. If TermTree
   is running it rewrites its own state continuously, so the hash changes for
   reasons that have nothing to do with the harness. Check for the backup
   file, and check the tree's root label is still yours.

6. Now point `--home` at your real home. It must **refuse**:
   ```bash
   $B seed --subject termtree --sessions 3 --home "$HOME"; echo "exit=$?"
   ```
   Expect exit `1` and
   `Refusing to use /Users/<you> as the scratch home: it is the real home
   directory (...) or contains it, so seeding would overwrite the runner's own
   application profiles.`
7. Same via the environment variable:
   ```bash
   RESOURCE_BENCHMARK_HOME="$HOME" $B seed --subject termtree --sessions 3; echo "exit=$?"
   ```
   Expect exit `1`.
8. Confirm it is not over-blocking — an explicit scratch directory still works:
   ```bash
   S=$(mktemp -d); $B seed --subject termtree --sessions 2 --home "$S"; echo "exit=$?"
   find "$S" -name state.json
   ```
   Expect exit `0` and one `state.json` under `$S`.

### Scenario 4 — `seed` backs up an existing profile and `restore` returns it exactly

1. Build a scratch home with a state file standing in for a real one:
   ```bash
   S=$(mktemp -d); D="$S/Library/Application Support/DocumentNode/TermTree"
   mkdir -p "$D"
   printf '{"tree":{"id":"original-root","label":"my real work","children":[]},"themeKey":"dark"}' > "$D/state.json"
   ORIG=$(shasum -a 256 "$D/state.json" | cut -d' ' -f1)
   ```
2. `$B seed --subject termtree --sessions 4 --home "$S"`
3. Confirm both files now exist: `ls "$D"` shows `state.json` and
   `state.json.before-resource-benchmark.json`.
4. Confirm the live file is the fixture: its `tree.label` is
   `resource-benchmark-root`.
5. `$B restore --subject termtree --home "$S"` — expect `restored termtree`.
6. Confirm byte-identical restoration:
   ```bash
   [ "$ORIG" = "$(shasum -a 256 "$D/state.json" | cut -d' ' -f1)" ] && echo PASS || echo FAIL
   ```
7. Confirm the backup was consumed: `ls "$D" | grep -c before-resource-benchmark`
   returns `0`.
8. `rm -rf "$S"`.

### Scenario 5 — the seeder refuses a target outside its scratch root

Covered by `cargo test`'s `refuses_a_directory_outside_the_scratch_root` and
`refuses_a_termtreedev_directory_even_under_the_scratch_root`. There is no
safe manual equivalent: the manual version would require pointing the seeder
at a real profile, which Scenario 3 step 6 now refuses outright.

### Scenario 6 — `run` refuses on an unquiesced machine and measures nothing

1. On an ordinary working machine (browser open, apps running), run `$B run`.
2. Expect exit `1` and
   `refused to start: quiesce gate failed, refusing to start: <signals>`.
3. Confirm nothing was produced: `ls results/` is unchanged.
4. Confirm no subject was launched **or quit** — any app that was running
   before is still running:
   `lsappinfo list | grep -c com.termtree.desktop` is unchanged.

The preflight order is quiesce gate → `open --env` support → already-running
check, and every one of them returns before any seeding, launching, or
teardown. Teardown issues a graceful quit, so it must never be reachable for a
process the harness did not itself launch.

### Scenario 7 — bundle-path override, version drift, and the unverified-seeder note

1. Point a subject at an app that exists but is the wrong one, to exercise all
   three behaviours at once:
   ```bash
   $B doctor --bundle-path 'codenomad-electron=/Applications/<some installed>.app'
   ```
2. Expect the "subject not installed" line for that subject to **disappear** —
   the override was honoured.
3. Expect a version-drift line naming both versions, e.g.
   `CodeNomad (Electron): version drift, expected 0.18.0 found 2.2.0`.
4. Expect a `doctor note:` line stating that subject's seeder has not been
   verified against a real install and that its N-session/sustained-use
   samples will report `invalidReason=seed-format-unverified`.

Note the notes are only reachable for an **installed** subject; a
not-installed subject short-circuits before them, which is why this scenario
uses an override to make one reachable.

### Scenario 8 — unverified seed formats are reported, never silently counted

Collaborator, CodeNomad and diri have seeders whose formats have never been
checked against a real install. Confirm the harness says so rather than
producing a number that looks valid:

1. `grep -rn 'seed_format_verified' src/subject.rs` — only `termtree` is
   `true`.
2. Their N-session and sustained-use samples carry
   `invalidReason: "seed-format-unverified"`, asserted by `cargo test`.
3. `$B doctor` emits the note from Scenario 7 for each installed one.

## Not verifiable here

- **A real measurement sweep.** Requires a rebooted machine at nominal memory
  and thermal pressure, no swap in use, on AC power, with exclusive use for
  hours. `$B doctor` must pass first. On a 16 GB host, never run two subjects
  concurrently.
- **Launch isolation against a real application** — that
  `open -n -F --env HOME=<scratch> -a <bundle>` genuinely redirects an app's
  data directory. Verifying it means launching a subject, which requires that
  subject to be quit first. On a working machine the operator's own TermTree
  and MarkNode are typically running, and launching either triggers the
  single-instance handoff described in Scenario 2. **Precondition to verify:**
  a quiesced machine with the subject quit — i.e. the same sitting as the
  sweep. The mechanism was confirmed manually against MarkNode on 2026-08-25:
  `lsappinfo` attribution still resolved under a scratch `HOME`, the app used
  the scratch data directory, and the real profile's mtime was unchanged.
- **Cold-start log-mark self-validation** (`termtree-log-marks-unrecognized`)
  and **`app-data-dir-not-created`**. Both fire only after a real TermTree
  launch, so they share the precondition above. Their pure logic is unit
  tested.
- **The three unverified seeders against real installs.** Collaborator,
  CodeNomad (Electron and Tauri) and diri must be installed at the pinned
  versions first.
- **The spawn-and-wait orchestration path**
  (`build_envelope` / `measure_one` / `measure_cold_start` / `teardown`) has
  never executed end to end. Treat the first sweep as its verification and
  budget it as debugging.

## Cleanup

- Scratch homes are **not** removed automatically. They accumulate under the
  OS temp directory as `resource-benchmark-home-<pid>-<nanos>` until the OS
  reclaims them. To clear them now:
  `rm -rf "$TMPDIR"/resource-benchmark-home-*`
- Remove any scratch directory you created explicitly with `--home`.
- If a seeding scenario was interrupted between `seed` and `restore`, run
  `$B restore --subject <id> --home <that home>`. `$B doctor --home <that home>`
  reports a leftover backup; it only performs that check for an explicitly
  supplied home, since a fresh default home can never have one.
- Never run `restore` against your real home — `--home "$HOME"` is refused.

## Agent execution notes

- Capture exit codes directly (`cmd; echo "exit=$?"`), never through a pipe —
  `cmd | tail` reports the exit code of `tail`, so a failure reads as `0`.
- Do not assert on the real `state.json`'s checksum while TermTree is running;
  it rewrites its own state. Assert on the absence of
  `state.json.before-resource-benchmark.json` and on the tree's root label.
- Do not launch a subject to test isolation while the operator is working. Check
  `lsappinfo list | grep bundleID=` first; if the subject or anything sharing
  its bundle identifier is running, record the scenario as blocked rather than
  quitting the operator's application.
- `$B run` is safe to invoke on an unquiesced machine — it refuses in preflight
  before touching anything — but do not add `--allow-*` flags to force past a
  refusal during verification.
