#!/usr/bin/env bash
#
# Rebuild the browser bundle with trunk watch and serve it with the native
# page server. Its default build includes /relay; the browser is singleplayer-only.
#
# LODESTONE_WEB_LISTEN defaults to 127.0.0.1:8080. Use 127.0.0.1:0 for an
# OS-assigned port, read back from the server's --port-file after binding.
# LODESTONE_RELAY_TARGET defaults to 127.0.0.1:25565 for the relay destination.
# Additional arguments go to trunk watch, not to the page server.
#
# Examples:
#   scripts/run-wasm.sh                                # page server on :8080
#   LODESTONE_WEB_LISTEN=127.0.0.1:0 scripts/run-wasm.sh  # OS-assigned port
#   LODESTONE_RELAY_TARGET=127.0.0.1:25570 scripts/run-wasm.sh  # different server
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

WEB_LISTEN="${LODESTONE_WEB_LISTEN:-127.0.0.1:8080}"
RELAY_TARGET="${LODESTONE_RELAY_TARGET:-127.0.0.1:25565}"

if [[ ! -f "$ROOT/web/Cargo.toml" ]]; then
  echo "error: web/Cargo.toml not found — nothing to serve." >&2
  exit 2
fi

# Resolve Cargo's configured output directory rather than assuming web/target.
WEB_TARGET_DIR="$(cd "$ROOT/web" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

for tool in trunk cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is not on PATH." >&2
    if [[ "$tool" == trunk ]]; then
      echo "       install it with: cargo install trunk --version 0.21.14" >&2
      echo "       (web/README.md has a faster prebuilt-binary route)" >&2
    fi
    exit 2
  fi
done

if ! rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown; then
  echo "error: the wasm32-unknown-unknown target is not installed." >&2
  echo "       rustup target add wasm32-unknown-unknown" >&2
  exit 2
fi

WEB_PID=""
TRUNK_PID=""
PORT_FILE="$(mktemp "${TMPDIR:-/tmp}/lodestone-web-server-port.XXXXXX")"

# Reap both children and remove the port file on exit. Both children run in the
# background so Bash can interrupt wait to handle signals; a foreground child
# would defer the trap until that command finishes.
cleanup() {
  for pid in "$WEB_PID" "$TRUNK_PID"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null
    fi
  done
  for pid in "$WEB_PID" "$TRUNK_PID"; do
    [[ -n "$pid" ]] && wait "$pid" 2>/dev/null
  done
  rm -f "$PORT_FILE"
}
trap cleanup EXIT INT TERM

# Identify the holder of a fixed port before starting; :0 needs no preflight.
FIXED_PORT="${WEB_LISTEN##*:}"
if [[ "$FIXED_PORT" != "0" ]] && command -v lsof >/dev/null 2>&1; then
  HOLDER="$(lsof -nP -iTCP:"$FIXED_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)"
  if [[ -n "$HOLDER" ]]; then
    HOLDER_CMD="$(ps -o comm= -p "$HOLDER" 2>/dev/null | tr -d ' ')"
    echo "error: port $FIXED_PORT is already bound by pid $HOLDER (${HOLDER_CMD:-unknown})." >&2
    echo "       Stop it, or run with LODESTONE_WEB_LISTEN=127.0.0.1:0 for an" >&2
    echo "       OS-assigned port instead." >&2
    exit 1
  fi
fi

echo "== building lodestone-web-server =="
if ! (cd "$ROOT/web" && cargo build --release -p lodestone-web-server); then
  echo "error: lodestone-web-server failed to build." >&2
  exit 1
fi

WEB_BIN="$WEB_TARGET_DIR/release/lodestone-web-server"
if [[ ! -x "$WEB_BIN" ]]; then
  echo "error: expected binary not found: $WEB_BIN" >&2
  exit 1
fi

echo "== starting lodestone-web-server: --listen $WEB_LISTEN --target $RELAY_TARGET =="
"$WEB_BIN" --listen "$WEB_LISTEN" --dist "$ROOT/web/dist" --target "$RELAY_TARGET" --port-file "$PORT_FILE" &
WEB_PID=$!

# Confirm startup and read the actual bound port from the server's port file.
BOUND_PORT=""
for _ in $(seq 1 50); do
  if ! kill -0 "$WEB_PID" 2>/dev/null; then
    break
  fi
  if [[ -s "$PORT_FILE" ]]; then
    BOUND_PORT="$(cat "$PORT_FILE")"
    break
  fi
  sleep 0.1
done

if ! kill -0 "$WEB_PID" 2>/dev/null; then
  echo "error: lodestone-web-server exited immediately — see its output above." >&2
  WEB_PID=""
  exit 1
fi
if [[ -z "$BOUND_PORT" ]]; then
  echo "error: lodestone-web-server did not report a bound port within 5s." >&2
  exit 1
fi
echo "== lodestone-web-server up (pid $WEB_PID) — http://${WEB_LISTEN%%:*}:${BOUND_PORT}/ =="

# Release matches the server-Worker staging profile. trunk watch only rebuilds
# dist/; lodestone-web-server owns the listener.
echo "== watching web (release, rebuilds dist/ on change) =="
cd "$ROOT/web" || exit 1

# Keep wait interruptible so cleanup can stop both children.
env -u NO_COLOR trunk watch --release "$@" &
TRUNK_PID=$!

# Track the page server's lifetime while keeping signal traps responsive.
wait "$WEB_PID"
STATUS=$?
WEB_PID=""   # already reaped; keep cleanup from waiting on a dead pid

# Propagate ordinary server exit status; a signal-terminated wait (128+signo)
# is treated as a normal stop of the development loop.
if (( STATUS > 128 )); then
  exit 0
fi
exit "$STATUS"
