#!/usr/bin/env bash
# Regenerates crates/lodestone-worldgen-data-26-3/assets/block_facts.txt from the real
# server jar of `mc-version` (the per-block property domains and per-state fact words the
# feature engine runs on; layout in BlockFactsOracle263.java).
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$REPO_ROOT/scripts/worldgen-oracle-26-3/run.sh" BlockFactsOracle263 > "$REPO_ROOT/crates/lodestone-worldgen-data-26-3/assets/block_facts.txt.tmp"
mv "$REPO_ROOT/crates/lodestone-worldgen-data-26-3/assets/block_facts.txt.tmp" "$REPO_ROOT/crates/lodestone-worldgen-data-26-3/assets/block_facts.txt"
