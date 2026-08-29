#!/usr/bin/env bash
# The fixed sustained-use per-session workload (spec FR-7, design §5.6.5).
#
# This script is byte-identical for every subject at every session: the
# sustained-use tier seeds the same 5-session configuration as the N=5
# tier, and each session's start command runs this loop against the
# seeded repository (SEEDED_REPO_PATH) instead of an idle shell.
#
# Real prompts are deliberately NOT sent to the agent CLI: doing so would
# make this tier non-deterministic (model/network variance) and
# unrepeatable for a third party without API credentials. This tier
# exercises the orchestrator's terminal-output and rendering paths, not
# agent inference -- see the harness README's Limitations section for why
# that distinction matters for what this tier can and cannot show.
#
# Usage: sustained-session.sh <repo-path> <duration-seconds>
set -euo pipefail

repo_path="${1:?usage: sustained-session.sh <repo-path> <duration-seconds>}"
duration_s="${2:?usage: sustained-session.sh <repo-path> <duration-seconds>}"

cd "$repo_path"
end=$(( $(date +%s) + duration_s ))

while [ "$(date +%s)" -lt "$end" ]; do
  git status --short > /dev/null 2>&1 || true
  git log --oneline -20 > /dev/null 2>&1 || true
  # Bounded so a large repo cannot make one iteration run long enough to
  # blow past the fixed duration.
  rg --max-count 50 --max-filesize 1M "TODO" . > /dev/null 2>&1 || true
  find . -maxdepth 2 -type f -name "*.md" -print -quit \
    | xargs -I{} cat {} > /dev/null 2>&1 || true
  sleep 5
done
