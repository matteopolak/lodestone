#!/usr/bin/env bash
# Bounded cargo-fuzz run of every target under `fuzz/fuzz_targets/`, starting
# from the committed seed corpus. This is what CI runs (the `fuzz` job in
# .github/workflows/ci.yml) and what `just fuzz-smoke` runs locally.
#
# Why a bounded smoke run and not a long campaign: a fuzz target nobody
# executes is an island with a corpus. Thirty seconds per target will not
# exhaust a decoder's state space, but it *does* prove every target still
# builds, still links its entry point, still loads its seeds, and still
# reaches real code — and it re-runs the whole committed corpus, which is the
# regression half. Long campaigns are a human's `just fuzz-run-seeded` job.
#
# What it gates on: a panic, an out-of-memory, or a hang (a single input
# exceeding the per-input timeout) in any target. Leak detection is off:
# these targets' documented property is "decoding attacker-controlled bytes
# must not panic", and a decoder that returns Err after allocating is a
# correct decoder, not a finding.
#
# Exclusions live in `fuzz/smoke-exclusions.txt`, one per line with a reason.
# Every other target is gated automatically, so adding a fuzz target does not
# require also remembering to add it here.
#
# Usage: fuzz/smoke.sh [seconds-per-target]
#
# Lives under fuzz/ rather than scripts/ because it is part of the cargo-fuzz
# workspace it drives: it reads that directory's target list, seed corpus and
# exclusion file, and has no meaning without them.
set -euo pipefail

SECONDS_PER_TARGET="${1:-30}"
FUZZ="$(cd "$(dirname "$0")" && pwd)"
EXCLUSIONS="$FUZZ/smoke-exclusions.txt"

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "cargo-fuzz is not installed: cargo install cargo-fuzz --locked" >&2
  exit 1
fi

cd "$FUZZ"

all_targets=()
for path in fuzz_targets/*.rs; do
  all_targets+=("$(basename "$path" .rs)")
done
if [ "${#all_targets[@]}" -eq 0 ]; then
  echo "no targets under $FUZZ/fuzz_targets — nothing to smoke, which is itself the bug" >&2
  exit 1
fi

excluded=()
if [ -f "$EXCLUSIONS" ]; then
  while IFS= read -r line; do
    line="${line%%#*}"
    line="$(echo "$line" | tr -d '[:space:]')"
    [ -n "$line" ] || continue
    if [ ! -f "fuzz_targets/$line.rs" ]; then
      echo "stale exclusion: $line has no fuzz_targets/$line.rs — remove the line" >&2
      exit 1
    fi
    excluded+=("$line")
  done <"$EXCLUSIONS"
fi

is_excluded() {
  local candidate="$1"
  for name in "${excluded[@]+"${excluded[@]}"}"; do
    [ "$name" = "$candidate" ] && return 0
  done
  return 1
}

# Built in one invocation rather than per-target: `cargo fuzz run` would
# otherwise re-resolve and re-link the shared crate graph for each of them,
# and a build failure should be reported once, before any fuzzing starts.
echo "::group::cargo fuzz build"
cargo fuzz build
echo "::endgroup::"

failed=()
for target in "${all_targets[@]}"; do
  if is_excluded "$target"; then
    echo "--- $target: excluded (see fuzz/smoke-exclusions.txt)"
    continue
  fi
  mkdir -p "corpus/$target"
  # Two corpus directories: libFuzzer writes coverage-increasing inputs only
  # into the FIRST one, so the committed seeds stay read-only and a smoke run
  # never rewrites the tree. A target with no seeds still runs, from whatever
  # `corpus/` already holds.
  dirs=("corpus/$target")
  if [ -d "seeds/$target" ]; then
    dirs+=("seeds/$target")
  else
    echo "--- $target: no committed seeds (fuzz/seeds/$target absent)"
  fi
  echo "::group::cargo fuzz run $target (${SECONDS_PER_TARGET}s)"
  if cargo fuzz run "$target" "${dirs[@]}" -- \
    "-max_total_time=$SECONDS_PER_TARGET" \
    -rss_limit_mb=2048 \
    -timeout=25 \
    -detect_leaks=0 \
    -print_final_stats=1; then
    echo "--- $target: ok"
  else
    echo "--- $target: FAILED (artifact under fuzz/artifacts/$target)"
    failed+=("$target")
  fi
  echo "::endgroup::"
done

if [ "${#failed[@]}" -gt 0 ]; then
  echo
  echo "fuzz smoke failed: ${failed[*]}"
  echo "reproduce one with: just fuzz-repro <target> fuzz/artifacts/<target>/<file>"
  exit 1
fi

echo
echo "fuzz smoke: all gated targets clean at ${SECONDS_PER_TARGET}s each"
