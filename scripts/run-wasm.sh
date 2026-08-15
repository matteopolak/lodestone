#!/usr/bin/env bash
#
# run-wasm.sh — launch the browser (wasm) build together with the WebSocket→TCP
# relay it needs, so `just run-wasm` gives a browser that can actually join a
# server rather than one that can only render.
#
# WHY THIS IS A SCRIPT AND NOT AN INLINE JUSTFILE RECIPE
#   Running two long-lived servers from one command needs real process
#   management: start one in the background, trap its cleanup so it cannot
#   outlive the run and keep its port bound, verify it actually came up, then
#   hand the terminal to the other. The Justfile's own header forbids a body
#   like that ("No script body moves into this file"), so this follows the
#   established `wasm-size` precedent — the script keeps the body, the recipe is
#   a one-line delegation.
#
# WHY THE RELAY IS ON BY DEFAULT
#   A browser cannot open a raw TCP socket. Without the relay, the page renders
#   and in-memory singleplayer works, but joining any real server does not: the
#   net HUD line reads `relay UNREACHABLE`. That is a confusing default for a
#   command whose name is "run" — you get something that looks broken rather
#   than something that is missing an optional extra. Set LODESTONE_NO_RELAY=1
#   to serve the page alone.
#
# THE PORT IS NOT FREELY CHOOSABLE
#   The browser no longer hardcodes a relay port in Rust at all — it asks its
#   own page origin for `/relay`, and web/Trunk.toml's `[[proxies]]` entry
#   forwards that to the relay's real listener. But Trunk.toml's `backend` is
#   still a literal `127.0.0.1:25580`, because trunk reads only its own static
#   TOML and cannot see this file's `--listen`. So changing --listen here
#   without editing web/Trunk.toml's `backend` still produces a page that
#   cannot reach its own relay — the drift check below (search
#   "relay/proxy port drift") catches exactly that mismatch and warns before
#   you spend a 90 s wasm build finding out some other way. Note also that
#   25580 is the port scripts/live-oracles/terrain.sh binds for the light-gate
#   oracle: if that oracle is up, the relay cannot bind, and the pre-flight
#   check below reports that too.
#
# WHY THE RELAY IS BUILT BEFORE TRUNK STARTS
#   Two reasons, both about failing fast. A relay that does not compile should
#   not cost you a full release wasm build to discover, and building it up front
#   keeps its cargo output from interleaving with trunk's.
#
# USAGE
#   scripts/run-wasm.sh                       # relay + page on :8080
#   LODESTONE_NO_RELAY=1 scripts/run-wasm.sh  # page only
#   scripts/run-wasm.sh --port 9000           # extra args go to trunk serve
#
# ENVIRONMENT
#   LODESTONE_NO_RELAY   non-empty: skip the relay entirely
#   LODESTONE_RELAY_ARGS relay flags; the Justfile passes its own `relay_defaults`
#                        so the endpoints have exactly one definition
#   LODESTONE_TARGET_DIR per-agent private target dir, same meaning as in the
#                        Justfile — read here so the relay build honours it too
#   LODESTONE_JOBS       cargo -j cap, same meaning as in the Justfile
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

RELAY_ARGS="${LODESTONE_RELAY_ARGS:---listen 127.0.0.1:25580 --target 127.0.0.1:25565}"

# Mirror the Justfile's target-dir/jobs handling for the one cargo command this
# script runs, so a private target dir is not silently bypassed here while every
# recipe respects it. Passed as FLAGS, never as CARGO_* environment variables:
# sccache hashes CARGO_* vars into its cache keys, and the env-var form of
# --target-dir measured 0% cache hits against 78-94% for the flag form
# (docs/build-caching.md). Note trunk itself takes neither flag, which is why
# only the relay build below can honour them.
TARGET_DIR="${LODESTONE_TARGET_DIR:-target}"
CARGO_FLAGS=(--target-dir "$TARGET_DIR")
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

RELAY_PID=""
TRUNK_PID=""

# Kill both children however we leave — including Ctrl-C, which is the normal way
# to stop a dev server. Without this the relay survives, keeps its port bound, and
# the NEXT run fails to bind for a reason that looks nothing like the cause.
#
# MEASURED: this trap does NOT fire if the script blocks on a foreground child.
# bash defers a caught signal until the current foreground command finishes, and
# `trunk serve` never finishes — so a SIGTERM to this script left both trunk and
# the relay running, still holding the port. Ctrl-C in a terminal happened to
# work, because SIGINT goes to the whole foreground process GROUP and reached the
# children directly, which is exactly why the bug survives casual testing. The
# fix is at the bottom of this file: trunk runs in the BACKGROUND and the script
# blocks in `wait`, which bash interrupts to run the handler. Do not "simplify"
# that back into a foreground call.
cleanup() {
  for pid in "$TRUNK_PID" "$RELAY_PID"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null
    fi
  done
  # Reap only after signalling both, so the relay is not left running for however
  # long trunk takes to shut down.
  for pid in "$TRUNK_PID" "$RELAY_PID"; do
    [[ -n "$pid" ]] && wait "$pid" 2>/dev/null
  done
}
trap cleanup EXIT INT TERM

if [[ -z "${LODESTONE_NO_RELAY:-}" ]]; then
  # Word-split the flags into an array rather than relying on an unquoted
  # expansion: explicit, and it survives being copied into a shell that does not
  # word-split unquoted variables at all (zsh does not).
  read -ra RELAY_ARGV <<< "$RELAY_ARGS"

  # Pre-flight the port. Binding twice fails, and the failure text ("failed to
  # bind") describes the symptom rather than the cause — which is usually either
  # a relay left over from a previous run or the terrain oracle holding 25580.
  RELAY_PORT=""
  for ((i = 0; i < ${#RELAY_ARGV[@]}; i++)); do
    if [[ "${RELAY_ARGV[i]}" == "--listen" ]]; then
      RELAY_PORT="${RELAY_ARGV[i + 1]##*:}"
      break
    fi
  done

  # relay/proxy port drift check. web/Trunk.toml's `[[proxies]]` entry has to
  # carry the relay's --listen port as a second, hand-kept literal (trunk reads
  # only its own static TOML — see that file's own comment for why this can't
  # be one shared definition). If someone changes RELAY_PORT here without
  # updating Trunk.toml's `backend`, the browser's `/relay` proxy silently
  # points at a port nothing is listening on and every ping/join just reads
  # "relay unreachable" with no clue why. A warning, not a hard failure: the
  # relay itself still starts correctly on the requested port either way, and
  # someone intentionally running a bespoke Trunk.toml should not be blocked.
  if [[ -n "$RELAY_PORT" && -f "$ROOT/web/Trunk.toml" ]]; then
    TRUNK_BACKEND_PORT="$(grep -A3 '^\[\[proxies\]\]' "$ROOT/web/Trunk.toml" \
      | grep '^backend' | head -1 | grep -oE ':[0-9]+' | head -1 | tr -d ':')"
    if [[ -n "$TRUNK_BACKEND_PORT" && "$TRUNK_BACKEND_PORT" != "$RELAY_PORT" ]]; then
      echo "warning: relay --listen port is $RELAY_PORT but web/Trunk.toml's" >&2
      echo "         [[proxies]] backend still names port $TRUNK_BACKEND_PORT." >&2
      echo "         The browser's /relay proxy will reach nothing. Update" >&2
      echo "         web/Trunk.toml's backend to match, or pass a matching" >&2
      echo "         --listen." >&2
    fi
  fi

  if [[ -n "$RELAY_PORT" ]] && command -v lsof >/dev/null 2>&1; then
    HOLDER="$(lsof -nP -iTCP:"$RELAY_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)"
    if [[ -n "$HOLDER" ]]; then
      HOLDER_CMD="$(ps -o comm= -p "$HOLDER" 2>/dev/null | tr -d ' ')"
      echo "note: port $RELAY_PORT is already bound by pid $HOLDER (${HOLDER_CMD:-unknown})."
      if [[ "$HOLDER_CMD" == *lodestone-relay* ]]; then
        echo "      That is already a relay — reusing it instead of starting a second."
      else
        echo "      NOT a relay. If this is the terrain oracle (it also binds 25580),"
        echo "      stop it first or the browser build will have no working relay."
      fi
      LODESTONE_NO_RELAY=1
    fi
  fi
fi

if [[ -z "${LODESTONE_NO_RELAY:-}" ]]; then
  echo "== building lodestone-relay =="
  if ! cargo build --release -p lodestone-relay "${CARGO_FLAGS[@]}"; then
    echo "error: the relay failed to build. Fix that first, or re-run with" >&2
    echo "       LODESTONE_NO_RELAY=1 to serve the page without it." >&2
    exit 1
  fi

  RELAY_BIN="$ROOT/$TARGET_DIR/release/lodestone-relay"
  if [[ ! -x "$RELAY_BIN" ]]; then
    echo "error: expected relay binary not found: $RELAY_BIN" >&2
    exit 1
  fi

  echo "== starting relay: ${RELAY_ARGV[*]} =="
  "$RELAY_BIN" "${RELAY_ARGV[@]}" &
  RELAY_PID=$!

  # Confirm it survived startup. The failure this catches is a bind error, which
  # happens in the first few milliseconds — and the whole point of checking now
  # is that the alternative is discovering it after a ~90 s release wasm build.
  # A fixed sleep is the honest tool here: there is no readiness signal to poll,
  # and the check is "did it die", not "is it ready".
  sleep 1
  if ! kill -0 "$RELAY_PID" 2>/dev/null; then
    echo "error: the relay exited immediately — see its output above." >&2
    echo "       Usually the port is already bound. Re-run with" >&2
    echo "       LODESTONE_NO_RELAY=1 to serve the page without it." >&2
    RELAY_PID=""
    exit 1
  fi
  echo "== relay up (pid $RELAY_PID) =="
fi

# --release is mandatory, and for a reason unlike the native build's: a debug
# wasm build makes single-threaded worldgen ~10x slower, which blows the
# singleplayer probe's own 30 s deadline and so presents as a FAILURE rather
# than as slowness. See web/README.md.
echo "== serving web (release) — http://127.0.0.1:8080/ =="
cd "$ROOT/web" || exit 1

# Backgrounded deliberately — see the `cleanup` comment above. A foreground
# `trunk serve` blocks bash from running the EXIT/TERM handler at all, which
# leaves the relay alive holding its port. Blocking in `wait` instead is what
# makes the trap reachable.
trunk serve --release "$@" &
TRUNK_PID=$!

wait "$TRUNK_PID"
STATUS=$?
TRUNK_PID=""   # already reaped; keep cleanup from waiting on a dead pid

# A signal-terminated `wait` reports 128+signo. Report trunk's own status when it
# exited on its own (a build error is the common case and should propagate), and
# treat a signal as the ordinary way a dev server is stopped rather than a failure.
if (( STATUS > 128 )); then
  exit 0
fi
exit "$STATUS"
