#!/usr/bin/env bash
#
# run-wasm.sh — keep the browser (wasm) build rebuilding on change, AND serve
# it (page + the WebSocket->TCP relay it needs to reach a real server) from
# ONE port, in ONE process.
#
# WHY THIS IS TWO PROCESSES, NOT ONE, EVEN THOUGH THERE IS ONE PORT
#   `trunk serve` used to be both halves at once: it rebuilt `dist/` on change
#   AND served it, with a `[[proxies]]` entry in web/Trunk.toml forwarding
#   `/relay` to a separately-run `lodestone-relay` process (a SECOND port,
#   hand-kept in sync with this script's own --listen literal — see git
#   history on this file and on web/Trunk.toml for that arrangement). Serving
#   was a `trunk serve`-only feature, so a deployed, non-`trunk serve` build
#   had no relay path at all — an accepted, documented gap.
#
#   That gap is closed now: `web/server` (crate `lodestone-web-server`) links
#   `lodestone-relay` in as a library and serves both the built page and
#   `/relay` from one listener. It is a plain native binary — a deployable
#   artifact, which `trunk serve`'s proxy could never be — so it is also what
#   a real deployment runs. But it does not rebuild anything, so pairing it
#   with `trunk watch` (rebuilds `dist/` on change, serves nothing) is what
#   reproduces `trunk serve`'s "one command, keeps rebuilding" convenience.
#   `web/Trunk.toml`'s `[[proxies]]` entry is gone; nothing forwards to a
#   second port any more because there is no second port.
#
# WHY THIS IS A SCRIPT AND NOT AN INLINE JUSTFILE RECIPE
#   Running two long-lived processes from one command needs real process
#   management: start one in the background, trap its cleanup so it cannot
#   outlive the run and keep its port bound, verify it actually came up, then
#   hand the terminal to the other. The Justfile's own header forbids a body
#   like that ("No script body moves into this file"), so this follows the
#   established `wasm-size` precedent — the script keeps the body, the recipe
#   is a one-line delegation.
#
# PORT SELECTION
#   LODESTONE_WEB_LISTEN defaults to a fixed, documented 127.0.0.1:8080, so
#   the URL is predictable. Set it to 127.0.0.1:0 to ask the OS for a free
#   port instead (the conflict case the owner raised) — the ACTUALLY bound
#   port is read back from a file lodestone-web-server writes after binding
#   (--port-file), never from this script's own stdout/a pipeline: a shell
#   pipeline is not a reliable way to recover a value like this (this repo has
#   measured `| head` reading as absence and `| grep | tail` reporting exit 0
#   because that is tail's status). That file is also how a fixed, already-
#   bound port is told apart from a real bind failure below.
#
# USAGE
#   scripts/run-wasm.sh                                # page + relay on :8080
#   LODESTONE_WEB_LISTEN=127.0.0.1:0 scripts/run-wasm.sh  # OS-assigned port
#   LODESTONE_RELAY_TARGET=127.0.0.1:25570 scripts/run-wasm.sh  # different server
#   scripts/run-wasm.sh --port 9000                    # extra args go to trunk watch
#
# ENVIRONMENT
#   LODESTONE_WEB_LISTEN  address lodestone-web-server binds for the page AND
#                         /relay; default 127.0.0.1:8080. Use :0 for an
#                         OS-assigned port.
#   LODESTONE_RELAY_TARGET the real Minecraft server /relay bridges to;
#                         default 127.0.0.1:25565, matching web/README.md and
#                         the standalone `lodestone-relay`/`just run-relay`.
#   LODESTONE_JOBS        cargo -j cap for the lodestone-web-server build, same
#                         meaning as in the Justfile. NOT applied as
#                         --target-dir: web/server is a member of web/'s OWN
#                         workspace (web/Cargo.lock, web/target/), so it never
#                         contends for the shared target/ lock the Justfile's
#                         {{tdir}} exists to avoid — same reasoning the
#                         `run-wasm` recipe's own comment already gives for
#                         why trunk gets neither flag.
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

WEB_LISTEN="${LODESTONE_WEB_LISTEN:-127.0.0.1:8080}"
RELAY_TARGET="${LODESTONE_RELAY_TARGET:-127.0.0.1:25565}"

CARGO_FLAGS=()
if [[ -n "${LODESTONE_JOBS:-}" ]]; then
  CARGO_FLAGS+=(-j "$LODESTONE_JOBS")
fi

if [[ ! -f "$ROOT/web/Cargo.toml" ]]; then
  echo "error: web/Cargo.toml not found — nothing to serve." >&2
  exit 2
fi

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

# Kill both children however we leave — including Ctrl-C, which is the normal
# way to stop a dev loop. Without this, whichever process is backgrounded
# survives, keeps its port (or its watch) alive, and the NEXT run fails or
# doubles up for a reason that looks nothing like the cause.
#
# MEASURED (from this script's earlier two-process shape, same mechanism):
# this trap does NOT fire if the script blocks on a foreground child. bash
# defers a caught signal until the current foreground command finishes, and a
# long-lived server never finishes on its own — so a SIGTERM to this script
# would leave both children running. Ctrl-C in a terminal happens to work,
# because SIGINT goes to the whole foreground process GROUP and reaches the
# children directly, which is exactly why the bug survives casual testing.
# The fix is at the bottom of this file: the foreground child runs in the
# BACKGROUND too and the script blocks in `wait`, which bash interrupts to run
# the handler. Do not "simplify" that back into a bare foreground call.
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

# Pre-flight the port when it is a fixed one (not OS-assigned :0), so a stale
# process from a previous run — or an unrelated `trunk serve` — reports as a
# named holder rather than as lodestone-web-server's own "Address already in
# use", which describes the symptom and not the cause.
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
# "${CARGO_FLAGS[@]}" alone throws "unbound variable" under `set -u` on this
# repo's actual /usr/bin/env bash (3.2 on macOS) when the array is empty — the
# classic bash-3.2 empty-array quirk. The `+` form below expands to nothing
# rather than erroring when CARGO_FLAGS is unset/empty, and to the array
# otherwise. MEASURED: the plain form failed this script's own first live run.
if ! (cd "$ROOT/web" && cargo build --release -p lodestone-web-server "${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}"); then
  echo "error: lodestone-web-server failed to build." >&2
  exit 1
fi

WEB_BIN="$ROOT/web/target/release/lodestone-web-server"
if [[ ! -x "$WEB_BIN" ]]; then
  echo "error: expected binary not found: $WEB_BIN" >&2
  exit 1
fi

echo "== starting lodestone-web-server: --listen $WEB_LISTEN --target $RELAY_TARGET =="
"$WEB_BIN" --listen "$WEB_LISTEN" --dist "$ROOT/web/dist" --target "$RELAY_TARGET" --port-file "$PORT_FILE" &
WEB_PID=$!

# Confirm it survived startup and read back the port it actually bound — read
# with a program (a bounded poll loop over a file), never by parsing this
# script's own backgrounded stdout.
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

# --release is mandatory for the WASM build, and for a reason unlike the
# native build's: a debug wasm build makes single-threaded worldgen ~10x
# slower, which blows the singleplayer probe's own 30 s deadline and so
# presents as a FAILURE rather than as slowness. See web/README.md.
#
# `trunk watch` — unlike `trunk serve` — only rebuilds dist/, never serves;
# lodestone-web-server (already running) is what a browser actually talks to.
echo "== watching web (release, rebuilds dist/ on change) =="
cd "$ROOT/web" || exit 1

# Backgrounded deliberately — see the `cleanup` comment above. A foreground
# `trunk watch` blocks bash from running the EXIT/TERM handler at all, which
# would leave lodestone-web-server alive holding its port.
trunk watch --release "$@" &
TRUNK_PID=$!

# Foreground: wait on lodestone-web-server (the user-facing process — its
# stdout is the request/relay log) rather than on trunk watch's rebuild log,
# so the trap still catches Ctrl-C via `wait` the same way the old shape did.
wait "$WEB_PID"
STATUS=$?
WEB_PID=""   # already reaped; keep cleanup from waiting on a dead pid

# A signal-terminated `wait` reports 128+signo. Report the real status when
# the process exited on its own (a bind or build error is the common case and
# should propagate), and treat a signal as the ordinary way a dev loop is
# stopped rather than a failure.
if (( STATUS > 128 )); then
  exit 0
fi
exit "$STATUS"
