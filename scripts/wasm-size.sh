#!/usr/bin/env bash
# Build the browser module and enforce its post-bindgen gzip ceiling.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TARGET="wasm32-unknown-unknown"
CEILING_BYTES="${CEILING_BYTES:-5800000}"
CEILING_MIB="$(awk "BEGIN{printf \"%.2f\", $CEILING_BYTES/1048576}")"

if [[ ! -f "$ROOT/web/Cargo.toml" ]]; then
  echo "error: web/Cargo.toml not found — nothing to size."
  exit 2
fi

CARGO_TARGET="$(cd "$ROOT/web" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

echo "== Lodestone wasm bundle-size guard =="
echo "target:  $TARGET   ceiling: $CEILING_BYTES B gzip ($CEILING_MIB MiB)"
echo

echo "building web (release)…"
if ! ( cd "$ROOT/web" && cargo build --release --target "$TARGET" >/dev/null 2>&1 ); then
  echo "error: release build failed. If a sibling crate is mid-edit, wait and retry."
  exit 1
fi

BINDGEN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-wasm-size.XXXXXX")"
cleanup_bindgen(){ rm -r -- "$BINDGEN_DIR"; }
trap cleanup_bindgen EXIT

WASM="$CARGO_TARGET/$TARGET/release/lodestone-web.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "error: expected artifact not found: $WASM"
  exit 1
fi

BOUND="$BINDGEN_DIR/lodestone-web_bg.wasm"
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "error: wasm-bindgen is required for a browser-valid size measurement" >&2
  exit 1
fi
if ! wasm-bindgen --target web --out-dir "$BINDGEN_DIR" --out-name lodestone-web "$WASM" >/dev/null 2>&1; then
  echo "error: wasm-bindgen failed; refusing to measure the Cargo intermediate" >&2
  exit 1
fi
if [[ ! -f "$BOUND" ]]; then
  echo "error: wasm-bindgen produced no browser module: $BOUND" >&2
  exit 1
fi
echo "wasm-bindgen: measured post-bindgen browser module"

AUTH_PATTERN='Microsoft|OAuth|device.?code|login\.microsoftonline\.com|user\.auth\.xboxlive|minecraftservices\.com/authentication'
AUTH_MATCHES="$(strings "$BOUND" | rg -i "$AUTH_PATTERN" || true)"
if [[ -n "$AUTH_MATCHES" ]]; then
  echo "error: browser module retains Microsoft account authentication code" >&2
  printf '%s\n' "$AUTH_MATCHES" >&2
  exit 1
fi

RAW=$(wc -c < "$BOUND" | tr -d ' ')
GZ=$(gzip -9 -n -c "$BOUND" | wc -c | tr -d ' ')
JS="$BINDGEN_DIR/lodestone-web.js"
JS_RAW=0
JS_GZ=0
if [[ -f "$JS" ]]; then
  JS_RAW=$(wc -c < "$JS" | tr -d ' ')
  JS_GZ=$(gzip -9 -n -c "$JS" | wc -c | tr -d ' ')
fi
mib(){ awk "BEGIN{printf \"%.2f\", $1/1048576}"; }

BR=""
if command -v brotli >/dev/null 2>&1; then
  BR=$(brotli -q 11 -c "$BOUND" | wc -c | tr -d ' ')
fi

echo
printf '  raw    : %10d B  (%s MiB)\n' "$RAW" "$(mib "$RAW")"
printf '  gzip   : %10d B  (%s MiB)   <- enforced\n' "$GZ" "$(mib "$GZ")"
if (( JS_RAW > 0 )); then
  printf '  glue   : %10d B raw / %d B gzip\n' "$JS_RAW" "$JS_GZ"
fi
if [[ -n "$BR" ]]; then
  printf '  brotli : %10d B  (%s MiB)   <- real wire cost\n' "$BR" "$(mib "$BR")"
else
  echo   "  brotli : (brotli not on PATH — install for the real wire number)"
fi
echo

if (( GZ > CEILING_BYTES )); then
  echo "RESULT: FAIL — gzip $GZ B exceeds ceiling $CEILING_BYTES B."
  echo "        A jump here is almost always a dependency/feature change. Inspect with:"
  echo "          twiggy top $WASM   (build once with strip=false for names)"
  exit 1
fi
echo "RESULT: PASS — gzip $GZ B within ceiling $CEILING_BYTES B."
