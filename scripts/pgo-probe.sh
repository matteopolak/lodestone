#!/usr/bin/env bash
#
# pgo-probe.sh — reproduce docs/pgo-experiment.md's PGO-vs-baseline measurement
# on demand.
#
# WHAT THIS IS
#   The exact three-build cycle docs/pgo-experiment.md's own "Method" section
#   already documents by hand (baseline, instrument+train, re-optimize),
#   wrapped into one script so re-deriving the 14.6%-fewer-instructions-retired
#   number is a single command instead of six manually-typed `cargo`/
#   `llvm-profdata` invocations. Issue #556 asked for PGO to be reproducible,
#   not landed as a default -- this is that, and nothing here touches
#   `[profile.release]` or any other build-config file. See that doc for the
#   full method writeup, caveats, and the original measurement this script's
#   own output should land close to (machine load and thermal state will move
#   the absolute instruction count a little; the RATIO is what to compare).
#
# WHY A SEPARATE PRIVATE TARGET DIR
#   PGO's `-Cprofile-generate`/`-Cprofile-use` flags change RUSTFLAGS between
#   the three builds, and RUSTFLAGS is part of sccache's cache key -- reusing
#   the shared target/ would mean every one of these three builds pays a full
#   rebuild wave for whichever OTHER live agent's next ordinary build follows
#   it (the same "flip cost" CLAUDE.md documents for the `-Z threads=8` and
#   Cranelift RUSTFLAGS changes, but tripled since this script itself changes
#   RUSTFLAGS three more times). A private CARGO_TARGET_DIR isolates all of
#   that -- see docs/build-caching.md's own precedent for exactly this pattern.
#
# WHAT IT COSTS
#   Three separate release builds of lodestone-worldgen + lodestone-worldgen-core
#   (thin LTO, codegen-units=1, per this workspace's existing [profile.release])
#   in one private target dir. Measured once: ~250-400 MiB total, a few minutes
#   wall time. Deleted automatically on exit (trap below) unless
#   PGO_PROBE_KEEP_TARGET=1 is set, in which case the path is printed instead
#   so a human can inspect the two `.profraw` files or the merged `.profdata`.
#
# USAGE
#   ./scripts/pgo-probe.sh                # 3 runs per build (default)
#   PGO_PROBE_RUNS=5 ./scripts/pgo-probe.sh
#
# Requires: the pinned nightly (rust-toolchain.toml) and its own llvm-profdata
# (bundled with the `llvm-tools` rustup component -- already installed for
# this pin; `rustup component add llvm-tools --toolchain $(cat
# rust-toolchain.toml | grep channel | cut -d'"' -f2)` if missing elsewhere).
# macOS only, same as pgo_probe.rs itself (proc_pid_rusage).

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "pgo-probe.sh: macOS only (pgo_probe.rs's instructions-retired counter is proc_pid_rusage)" >&2
  exit 1
fi

cd "$(git rev-parse --show-toplevel)"

RUNS="${PGO_PROBE_RUNS:-3}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-pgo-probe.XXXXXX")"
TARGET_DIR="$WORK/target"
PROFILE_DIR="$WORK/profiles"
mkdir -p "$PROFILE_DIR"

cleanup() {
  if [[ "${PGO_PROBE_KEEP_TARGET:-0}" == "1" ]]; then
    echo "PGO_PROBE_KEEP_TARGET=1: leaving $WORK in place (delete it yourself when done)" >&2
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

TOOLCHAIN_SYSROOT="$(rustc --print sysroot)"
LLVM_PROFDATA="$TOOLCHAIN_SYSROOT/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-profdata"
if [[ ! -x "$LLVM_PROFDATA" ]]; then
  echo "pgo-probe.sh: llvm-profdata not found at $LLVM_PROFDATA" >&2
  echo "  install with: rustup component add llvm-tools" >&2
  exit 1
fi

BIN="$TARGET_DIR/release/examples/pgo_probe"

build() {
  local label="$1" flags="$2"
  echo "== building ($label) rustflags=[$flags] ==" >&2
  CARGO_TARGET_DIR="$TARGET_DIR" RUSTFLAGS="$flags" \
    cargo build --release -p lodestone-worldgen --example pgo_probe >&2
}

# `pgo_probe` prints two lines, each prefixed `PGO_PROBE`:
#   PGO_PROBE patch=6x6 seed=42 non_air_checksum=<u64>
#   PGO_PROBE instructions_retired=<u64>
# -- see crates/lodestone-worldgen/examples/pgo_probe.rs's own `main`. This
# helper runs it RUNS times and echoes the MEDIAN instructions_retired plus
# every checksum seen, so a checksum mismatch (PGO changing what is
# generated, not just how it is compiled) is loud rather than silently
# averaged away.
run_median() {
  local -a counts=()
  local -a checksums=()
  for ((i = 0; i < RUNS; i++)); do
    local output ir chk
    output="$("$BIN")"
    ir="$(echo "$output" | sed -n 's/^PGO_PROBE instructions_retired=\([0-9]*\)$/\1/p')"
    chk="$(echo "$output" | sed -n 's/.*non_air_checksum=\([0-9]*\).*/\1/p')"
    if [[ -z "$ir" || -z "$chk" ]]; then
      echo "pgo-probe.sh: could not parse pgo_probe output: $output" >&2
      exit 1
    fi
    counts+=("$ir")
    checksums+=("$chk")
  done
  local sorted
  sorted="$(printf '%s\n' "${counts[@]}" | sort -n)"
  local median
  median="$(echo "$sorted" | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')"
  local uniq_checksums
  uniq_checksums="$(printf '%s\n' "${checksums[@]}" | sort -u | wc -l | tr -d ' ')"
  if [[ "$uniq_checksums" != "1" ]]; then
    echo "pgo-probe.sh: non_air_checksum varied across runs (${checksums[*]}) -- generation output changed, not just how it was compiled. Aborting rather than reporting a misleading number." >&2
    exit 1
  fi
  echo "$median|${checksums[0]}"
}

echo "Runs per build: $RUNS"
echo "Private target dir: $TARGET_DIR"
echo

# 1. Baseline
build "baseline" ""
baseline_result="$(run_median)"
baseline_ir="${baseline_result%%|*}"
baseline_chk="${baseline_result##*|}"
echo "baseline: instructions_retired(median of $RUNS)=$baseline_ir non_air_checksum=$baseline_chk"

# 2. Instrument + train
build "instrument" "-Cprofile-generate=$PROFILE_DIR"
# Training runs: same binary, just to populate .profraw files. Their own
# instructions_retired output is discarded -- an instrumented binary's
# counts are not comparable to baseline/optimized (the instrumentation
# itself adds overhead).
for ((i = 0; i < RUNS; i++)); do
  "$BIN" >/dev/null
done
echo "instrument: wrote $(ls "$PROFILE_DIR"/*.profraw 2>/dev/null | wc -l | tr -d ' ') .profraw files"

# 3. Merge
"$LLVM_PROFDATA" merge -o "$PROFILE_DIR/merged.profdata" "$PROFILE_DIR"/*.profraw
echo "merged profile: $PROFILE_DIR/merged.profdata"

# 4. Re-optimize using the profile
build "pgo-optimized" "-Cprofile-use=$PROFILE_DIR/merged.profdata -Cllvm-args=-pgo-warn-missing-function"
pgo_result="$(run_median)"
pgo_ir="${pgo_result%%|*}"
pgo_chk="${pgo_result##*|}"
echo "pgo-optimized: instructions_retired(median of $RUNS)=$pgo_ir non_air_checksum=$pgo_chk"
echo

if [[ "$baseline_chk" != "$pgo_chk" ]]; then
  echo "pgo-probe.sh: non_air_checksum differs between baseline ($baseline_chk) and PGO-optimized ($pgo_chk) builds -- PGO changed WHAT is generated, not just how it compiles. This should never happen; treat the ratio below as invalid and investigate before trusting it." >&2
fi

ratio="$(awk -v a="$pgo_ir" -v b="$baseline_ir" 'BEGIN { printf "%.4f", a / b }')"
pct="$(awk -v r="$ratio" 'BEGIN { printf "%.1f", (1 - r) * 100 }')"

echo "=== Result ==="
printf '%-16s %s\n' "baseline:" "$baseline_ir"
printf '%-16s %s\n' "pgo-optimized:" "$pgo_ir"
printf '%-16s %s (%.1f%% fewer instructions retired)\n' "ratio:" "$ratio" "$pct"
