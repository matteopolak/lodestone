#!/usr/bin/env bash
# Prove that the generated live-fluid differential search detects the exact
# historical delay-one, seven-cell fluid seed. This intentionally operates on
# a detached worktree at HEAD so the shared checkout is never mutated.
set -euo pipefail

FIXED_TEST_NAME=fixed_generated_fluid_stream_has_no_live_divergence
HISTORICAL_TEST_NAME=historical_reversion_is_found_shrunk_and_replayed_against_live_vanilla
ROOT="$(git rev-parse --show-toplevel)"
WORKTREE="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-fluid-reversion.XXXXXX")"

cleanup() {
    local status=$?
    trap - EXIT
    cd "$ROOT"
    if [ -e "$WORKTREE" ]; then
        git -C "$ROOT" worktree remove --force "$WORKTREE" 2>/dev/null \
            || rmdir "$WORKTREE" 2>/dev/null \
            || status=1
    fi
    exit "$status"
}
trap cleanup EXIT

rmdir "$WORKTREE"

git -C "$ROOT" worktree add --detach "$WORKTREE" HEAD
cd "$WORKTREE"
TARGET_DIR="$WORKTREE/target"

# First prove that the committed fixed tree has no divergence over the exact
# bounded stream that will be searched again after mutation. Running this
# before applying anything makes a later historical finding discriminating.
if ! grep -Fq "fn $FIXED_TEST_NAME()" crates/lodestone-fuzz/tests/differential_live_generated_fluid.rs; then
    echo "fixed-tree reversion control is not present at committed HEAD" >&2
    exit 1
fi
CARGO_TARGET_DIR="$TARGET_DIR" RUSTC_WRAPPER='' cargo test -p lodestone-fuzz --features rcon-oracle \
    --test differential_live_generated_fluid "$FIXED_TEST_NAME" -- --ignored --nocapture

# The patch is the reviewed, single-function form of the seed that commit
# 40d39c13 removed: schedule the edited cell and all six neighbours at delay
# one, without inspecting their fluid states. Keeping the current function
# signature means later unrelated callers and tests remain buildable. A context
# drift fails before a live test can label a different mutation as historical.
MUTATION="$WORKTREE/scripts/historical-fluid-delay-one-seed.patch"
git apply --check "$MUTATION"
git apply "$MUTATION"

changed_paths="$(git diff --name-only)"
if [ "$changed_paths" != "crates/lodestone-server/src/fluid.rs" ]; then
    echo "historical mutation changed unexpected paths: ${changed_paths:-none}" >&2
    exit 1
fi
# Reverse and reapply the committed patch around a clean-diff assertion. This
# proves every delay-one, seven-cell, no-filter hunk is present and that no
# other fluid edit can be mistaken for the historical mutation.
git apply --reverse "$MUTATION"
if ! git diff --quiet -- crates/lodestone-server/src/fluid.rs; then
    echo "historical mutation differs from its committed seven-cell delay-one patch" >&2
    exit 1
fi
git apply "$MUTATION"

if ! grep -Fq "fn $HISTORICAL_TEST_NAME()" crates/lodestone-fuzz/tests/differential_live_generated_fluid.rs; then
    echo "historical reversion control is not present at committed HEAD" >&2
    exit 1
fi

# The ignored test asserts all three detector stages: it finds the externally
# observed first-tick mismatch, semantically shrinks while preserving that
# exact divergence, then replays the serialized result from a fresh live lane.
CARGO_TARGET_DIR="$TARGET_DIR" RUSTC_WRAPPER='' cargo test -p lodestone-fuzz --features rcon-oracle \
    --test differential_live_generated_fluid "$HISTORICAL_TEST_NAME" -- --ignored --nocapture

echo "historical fluid reversion detected, shrunk, and replayed"
