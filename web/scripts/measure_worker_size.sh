#!/usr/bin/env bash
# Measure both browser server-worker variants and their wasm-bindgen glue after
# staging. This is separate from the page-module size gate.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$(mktemp -d "${TMPDIR:-/tmp}/lodestone-worker-size.XXXXXX")"
trap 'rm -rf "$out"' EXIT

"$root/web/scripts/stage_worker.sh" "$out"

size_bytes() { wc -c < "$1" | tr -d ' '; }
compressed_bytes() {
  local file="$1" codec="$2"
  case "$codec" in
    gzip) gzip -9 -n -c "$file" | wc -c | tr -d ' ' ;;
    brotli) brotli -q 11 -c "$file" | wc -c | tr -d ' ' ;;
  esac
}

echo "== Lodestone server-worker post-bindgen sizes =="
for mode in serial threaded; do
  wasm="$out/lodestone-server-worker-wasm-${mode}_bg.wasm"
  js="$out/lodestone-server-worker-wasm-${mode}.js"
  raw="$(size_bytes "$wasm")"
  gzip_size="$(compressed_bytes "$wasm" gzip)"
  js_raw="$(size_bytes "$js")"
  js_gzip="$(compressed_bytes "$js" gzip)"
  printf '  %-8s wasm raw=%s B gzip=%s B' "$mode" "$raw" "$gzip_size"
  if command -v brotli >/dev/null 2>&1; then
    printf ' brotli=%s B' "$(compressed_bytes "$wasm" brotli)"
  else
    printf ' brotli=unavailable'
  fi
  printf ' | glue raw=%s B gzip=%s B\n' "$js_raw" "$js_gzip"
done

snippet_raw=0
snippet_gzip=0
while IFS= read -r -d '' snippet; do
  snippet_raw=$((snippet_raw + $(size_bytes "$snippet")))
  snippet_gzip=$((snippet_gzip + $(compressed_bytes "$snippet" gzip)))
done < <(find "$out/snippets" -type f -name '*.js' -print0 2>/dev/null)
printf '  bindgen snippets raw=%s B gzip=%s B\n' "$snippet_raw" "$snippet_gzip"
