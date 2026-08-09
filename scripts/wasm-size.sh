#!/usr/bin/env bash
#
# wasm-size.sh — release bundle-size guard for the browser build.
#
# WHY THIS IS SEPARATE FROM wasm-check.sh
#   wasm-check.sh is a FAST compile tripwire (debug builds, run on every
#   dependency change). This guard does the opposite kind of work: a full
#   `--release` build with `lto = "fat"` and a single codegen unit, which takes
#   ~30-90s, plus an optional `wasm-opt` pass. Folding that into the compile
#   tripwire would slow the command everyone runs and couple it to a binaryen
#   binary that isn't on PATH. So this is an on-demand / CI guard instead.
#
# WHAT IT MEASURES
#   The compressed size of the release `.wasm` — what actually crosses the wire,
#   since any real host serves it compressed. It reports gzip (the ENFORCED
#   ceiling: universally available, deterministic) and, when `brotli` is present,
#   brotli-11 — which is what servers actually ship for wasm and is the true wire
#   cost. Raw size is reported too (it drives browser parse/compile time).
#
#   wasm-opt is applied when a binary is found (PATH, or the wasm-pack cache).
#   MEASURED FINDING (W4): its win is almost entirely raw size (~10%) and barely
#   survives gzip (~2%); on *brotli* it is mildly COUNTERPRODUCTIVE (+4 KiB),
#   because the compressibility it adds is already captured by brotli's larger
#   window. So brotli is measured on the rustc output (pre-wasm-opt), which is the
#   smaller of the two. The gzip ceiling holds with or without wasm-opt.
#
# BASELINE (measured, opt-level="z" + lto=fat + strip):
#   CURRENT, and the ceiling is FAILING: 5,013,669 B gzip against a 1,600,000 B
#   ceiling — 3.13x over. That is not drift; `web/` stopped being a render spike and
#   became a launcher for the real `lodestone-shell`, so the whole client now ships.
#   The cause is measured and is NOT code: `.rodata` is ~76% of the shipped binary,
#   and it is generated static tables — `lodestone-data/src/generated/` ~4.9 MB
#   (`block_states.rs` 1.43 MB, `path_types.rs` 713 KB), `sin_table.rs` 820 KB,
#   `flattening.rs` 369 KB. Two consequences: `opt-level`/`lto` act on the quarter
#   that is not the problem, and **the same tables are in the native binary too**, so
#   this is a whole-project question. The fix is moving jar-derived tables behind the
#   runtime fetch seam `client.jar` already uses. Do NOT raise the ceiling to make
#   this pass; the failure is the signal.
#
#   The paragraph below is the PRE-SHELL baseline, kept because its attribution is
#   still the right method — and as an instance of exactly the trap it warns about:
#   it read "882220 B gzip, 55% of the ceiling" while the truth was 5.7x that, so
#   anyone quoting it would have called a 3.13x overage healthy. A recorded
#   measurement is only true at its sha.
#
#   Re-measured 2026-08-08 (882220 B gzip, 55% of the ceiling). The previous W4
#   figures recorded here -- raw 4.12 / gzip 1.24 / brotli 0.89 MiB -- were stale by
#   about a third: they predate dropping the WebGL2 fallback, whose glow backend the
#   attribution below still counts. Quoting the old number would make a ~30%
#   regression look like the baseline, which is the direction that hides a problem.
#   The dominant cost is the wgpu graphics stack (wgpu-core/-hal + naga shader
#   compiler + glow, ~1.19 MiB attributed); our own code is ~120 KiB and
#   lodestone-assets contributes ~18 KiB (no full-corpus bloat — DCE handles it).
#   So a regression here almost always means a dependency/feature change, not our
#   code — same trigger as wasm-check.sh. An accidental `opt-level = 3` (1.62 MiB
#   gzip) trips the ceiling on purpose; normal dep drift has ~25% headroom below.
#
# USAGE
#   scripts/wasm-size.sh            # measure + enforce ceiling
#   CEILING_BYTES=1400000 scripts/wasm-size.sh   # override ceiling
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TARGET="wasm32-unknown-unknown"
CEILING_BYTES="${CEILING_BYTES:-1600000}"   # 1.53 MiB gzip; baseline is 1.21 MiB
CEILING_MIB="$(awk "BEGIN{printf \"%.2f\", $CEILING_BYTES/1048576}")"

if [[ ! -f "$ROOT/web/Cargo.toml" ]]; then
  echo "error: web/Cargo.toml not found — nothing to size."
  exit 2
fi

echo "== Lodestone wasm bundle-size guard =="
echo "target:  $TARGET   ceiling: $CEILING_BYTES B gzip ($CEILING_MIB MiB)"
echo

echo "building web (release)…"
if ! ( cd "$ROOT/web" && cargo build --release --target "$TARGET" >/dev/null 2>&1 ); then
  echo "error: release build failed. If a sibling crate is mid-edit, wait and retry."
  exit 1
fi

WASM="$ROOT/web/target/$TARGET/release/lodestone-web.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "error: expected artifact not found: $WASM"
  exit 1
fi

# Locate an optional wasm-opt (PATH first, then the wasm-pack cache).
WO=""
if command -v wasm-opt >/dev/null 2>&1; then
  WO="wasm-opt"
else
  cand="$(find "$HOME/Library/Caches/.wasm-pack" "$HOME/.cache" -type f -name wasm-opt 2>/dev/null | head -1 || true)"
  [[ -n "$cand" ]] && WO="$cand"
fi

MEASURED="$WASM"
if [[ -n "$WO" ]]; then
  # `-all` because rustc emits post-MVP wasm features (bulk-memory, sign-ext…).
  if "$WO" -Oz -all "$WASM" -o "$ROOT/web/target/$TARGET/release/lodestone-web.opt.wasm" 2>/dev/null; then
    MEASURED="$ROOT/web/target/$TARGET/release/lodestone-web.opt.wasm"
    echo "wasm-opt: applied (-Oz) via $WO"
  fi
else
  echo "wasm-opt: not found — measuring rustc output (gzip differs by ~2%)."
fi

RAW=$(wc -c < "$MEASURED" | tr -d ' ')
GZ=$(gzip -9 -c "$MEASURED" | wc -c | tr -d ' ')
mib(){ awk "BEGIN{printf \"%.2f\", $1/1048576}"; }

# brotli-11 is the real wire cost for wasm. Measured on the rustc output ($WASM),
# not the wasm-opt one, because wasm-opt mildly *hurts* brotli (see header).
BR=""
if command -v brotli >/dev/null 2>&1; then
  BR=$(brotli -q 11 -c "$WASM" | wc -c | tr -d ' ')
fi

echo
printf '  raw    : %10d B  (%s MiB)\n' "$RAW" "$(mib "$RAW")"
printf '  gzip   : %10d B  (%s MiB)   <- enforced\n' "$GZ" "$(mib "$GZ")"
if [[ -n "$BR" ]]; then
  printf '  brotli : %10d B  (%s MiB)   <- real wire cost\n' "$BR" "$(mib "$BR")"
else
  echo   "  brotli : (brotli not on PATH — install for the real wire number)"
fi
echo

if (( GZ > CEILING_BYTES )); then
  echo "RESULT: FAIL — gzip $GZ B exceeds ceiling $CEILING_BYTES B."
  echo "        A jump here is almost always a dependency/feature change. Inspect with:"
  echo "          twiggy top web/target/$TARGET/release/lodestone-web.wasm   (build once with strip=false for names)"
  exit 1
fi
echo "RESULT: PASS — gzip $GZ B within ceiling $CEILING_BYTES B."
