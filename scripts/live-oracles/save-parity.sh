#!/usr/bin/env bash
# Hand a world directory to a real vanilla 26.2 server, let it load and save it,
# then stop it cleanly and get out of the way.
#
#   usage: save-parity.sh <serverRoot> <levelName> [rconCommand ...]
#
# `<serverRoot>/<levelName>` is the world folder. Both are the caller's to
# create; this script writes `eula.txt`, `server.properties` and `vanilla.log`
# into `<serverRoot>` and touches nothing under `<levelName>` itself — so
# everything that appears in the world folder afterwards was put there by
# Mojang's code, which is the entire point.
#
# Unlike every other script in this directory this one is **not** a long-lived
# oracle. It is a one-shot: boot, wait for `Done (`, run each `rconCommand` in
# order, `save-all flush`, `stop`, wait for the container to exit, return. The
# caller is `crates/lodestone-anvil/tests/vanilla_save_parity.rs`, which needs
# vanilla to have *finished* writing before it reads the directory back — so
# "the container has exited" is the only acceptable completion signal, and this
# script blocks until it sees one.
#
#   game port : 25590   } deliberately not any of the long-lived oracles'
#   RCON port : 25591   } (25565/6, 25568/9, 25570/1, 25580/1)
#
# # Why the shutdown cannot be `container stop`
#
# The save happens on **clean shutdown**, in `MinecraftServer.stopServer` ->
# `saveAllChunks(true, true, true)`. `container stop` sends SIGTERM; vanilla
# does install a shutdown hook, but it races the runtime's kill timeout, and a
# half-written region file reads as a parity defect rather than as a harness
# bug. RCON `stop` is the in-band request. The `save-all flush` before it is
# not redundant: `flush` makes `ChunkMap.save` write every held chunk rather
# than only the ones it currently considers unsaved.
#
# # Why RCON goes through rcon-op.py
#
# Vanilla's RCON server performs exactly one `read()` per request and closes the
# socket unless `pktsize == read - 4`, so the whole frame must go out in one
# write. `rcon-op.py`'s `build_frame` + single `sendall` already satisfies that
# and is the proven path here (see its docstring); a third copy of that
# constraint in bash is exactly how it gets got wrong. Its name says `op`
# because that was its first caller — it sends an arbitrary command.
#
# # Why the jar is not copied in
#
# `server.jar` is a *bundler*: run directly it extracts `libraries/` and
# `versions/` into the working directory on first start, ~60 MB per run, into a
# temp dir that is deleted straight afterwards. `.cache/mc/26.2` already holds
# that extraction (39 library jars + `versions/server-26.2.jar`), so this mounts
# it read-only and invokes `net.minecraft.server.Main` on an explicit classpath
# — the same trick `scripts/anvil-oracle/run.sh` uses, for the same reason.
# Read-only is safe because nothing the server writes goes next to the jar once
# the bundler is out of the picture; it all goes to the working directory.
#
# # Runtime: Apple `container`
#
# Same runtime, image and traps as creative.sh — see docs/oracle-runtimes.md.
# The two that bite here: the bare `host:container` `-p` form is required (a
# `127.0.0.1:` prefix accepts the TCP connection and then resets on the first
# byte), and `--memory` must be set because the per-VM default is 1 GiB. The
# caps below are *lower* than the long-lived oracles' 3g/2G on purpose: this
# server holds a handful of chunks and no players, and it usually runs while
# another oracle container already has 3 GiB on a 16 GB machine.
set -euo pipefail

SERVER_ROOT="${1:?usage: save-parity.sh <serverRoot> <levelName> [rconCommand ...]}"
LEVEL_NAME="${2:?missing levelName}"
shift 2
SERVER_ROOT="$(cd "$SERVER_ROOT" && pwd)"
# Seconds to let the world settle after the caller's RCON commands and before
# the save. A `forceload add` returns immediately but the chunks it requests
# are loaded, lit and promoted to FULL over the following ticks, and it is that
# promotion — `ChunkAccess.setLightCorrect(true)`, which calls `markUnsaved()`
# — that makes vanilla rewrite a chunk whose `isLightOn` we wrote as 0. Save
# too early and the file is not rewritten at all, which is the *vacuous*
# outcome: the caller would then be comparing its own bytes to themselves.
# Override for a slow machine; do not lower it without re-checking the
# caller's `vanilla_rewrote_*` control.
SETTLE_SECONDS="${LODESTONE_SAVE_PARITY_SETTLE:-10}"

NAME=lodestone-save-parity
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CACHE="$ROOT/.cache/mc/26.2"
GAME_PORT=25590
RCON_PORT=25591
RCON_PASSWORD=lodestone
JVM_HEAP=1200M
VM_MEMORY=2g

if [ ! -f "$CACHE/versions/26.2/server-26.2.jar" ]; then
  echo "no extracted 26.2 server at $CACHE/versions/26.2/server-26.2.jar" >&2
  echo "run any of the other live-oracle scripts once to let the bundler extract it," >&2
  echo "or fetch the 26.2 dedicated server jar into $CACHE and start it once by hand." >&2
  echo "this gate has no fallback: it exists to have a real Mojang server adjudicate our bytes." >&2
  exit 1
fi
if [ ! -d "$SERVER_ROOT/$LEVEL_NAME" ]; then
  echo "no world folder at $SERVER_ROOT/$LEVEL_NAME" >&2
  exit 1
fi

# Offline mode, nobody joins, no account is ever involved.
# `pause-when-empty-seconds=0` for the reason creative.sh documents at length:
# with no player connected a default server pauses the world after 60 s. Here
# that would only delay the save, but the setting is free and the surprise is
# not. `sync-chunk-writes=true` is load-bearing for a gate — it makes region
# writes synchronous, so "the process exited" really does imply "the bytes are
# on disk" rather than "an async writer was still draining".
printf 'eula=true\n' > "$SERVER_ROOT/eula.txt"
cat > "$SERVER_ROOT/server.properties" <<PROPERTIES
level-name=${LEVEL_NAME}
online-mode=false
server-port=${GAME_PORT}
enable-rcon=true
rcon.port=${RCON_PORT}
rcon.password=${RCON_PASSWORD}
broadcast-rcon-to-ops=false
pause-when-empty-seconds=0
sync-chunk-writes=true
spawn-protection=0
max-players=1
view-distance=6
simulation-distance=4
PROPERTIES

# `logs/` is deleted rather than reused. log4j rolls an existing `latest.log`
# into a dated `.gz` *at boot*, and the readiness poll below reads `latest.log`
# from the host — so a stale file from a previous run is a `Done (` that has
# already happened, and the poll would return before this server had loaded
# anything. Measured shape, not hypothetical: two runs against one server root
# is exactly what the two directions of the parity gate do.
rm -rf "$SERVER_ROOT/logs"

container system start >/dev/null 2>&1 || true
container rm -f "$NAME" >/dev/null 2>&1 || true

container run -d --rm --name "$NAME" \
  --memory "$VM_MEMORY" \
  -p "${GAME_PORT}:${GAME_PORT}" -p "${RCON_PORT}:${RCON_PORT}" \
  -v "$CACHE":/mc:ro \
  -v "$SERVER_ROOT":/w -w /w \
  eclipse-temurin:25-jdk \
  bash -c '
    set -e
    CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
    exec java -Xmx'"$JVM_HEAP"' -cp "$CP" net.minecraft.server.Main --nogui
  ' >/dev/null

# Nothing below may leave a container behind, including on the `set -e` paths —
# an orphan holds 2 GiB and both ports.
cleanup() {
  container rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# **`container logs` is empty for this image, and that is not a transient.**
# Measured: a 26.2 server that booted fine, printed 27 lines to its own
# `logs/latest.log` and reported `Done (0.165s)!` produced *nothing* on
# `container logs`/`container logs -f`. log4j2's console appender is not
# line-flushed when stdout is not a tty and the runtime never captured it. A
# readiness poll built on `container logs` therefore times out against a
# perfectly healthy server — which is how the first version of this script
# failed.
#
# So the log this script reads is **vanilla's own**, at
# `<serverRoot>/logs/latest.log`, which log4j's file appender flushes and which
# is on the bind mount and so directly readable from the host. It is also
# evidence the caller reads: a rejected `world_gen_settings.dat` logs "Unable to
# read or access the world gen settings file!" and then silently re-rolls the
# seed (issue #468's exact bug), which is invisible in the saved bytes alone.
LOG="$SERVER_ROOT/logs/latest.log"

echo "save-parity: waiting for the 26.2 server to load '$SERVER_ROOT/$LEVEL_NAME'..."
ready=no
for _ in $(seq 1 90); do
  if grep -q 'Done (' "$LOG" 2>/dev/null; then
    ready=yes
    break
  fi
  # A container that has already died will never print `Done (`, and waiting the
  # full 90 iterations for that turns a clear failure into a timeout. Checking
  # liveness is what surfaces a rejected level.dat or an OOM as itself.
  if ! container inspect "$NAME" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done

if [ "$ready" != yes ]; then
  echo "save-parity: the server never reported 'Done (' — tail of $LOG follows" >&2
  tail -60 "$LOG" 2>/dev/null >&2 || echo "(no $LOG at all — the JVM never started)" >&2
  exit 1
fi

rcon() {
  python3 "$ROOT/scripts/live-oracles/rcon-op.py" \
    127.0.0.1 "$RCON_PORT" "$RCON_PASSWORD" "$1"
}

# **A command vanilla rejects is not a failed RCON call, and that difference has
# already cost this gate a run.**
#
# `gamerule randomTickSpeed 0` was silently refused for eleven runs — 26.2
# renamed the rule to `random_tick_speed` (`GameRules.java:74`, with a
# `GameRuleRegistryFix` datafixer for the old spelling). The server answered
# `Incorrect argument for commandgamerule randomTickSpeed 0<--[HERE]`, which is
# a perfectly valid RCON *response*: rcon-op.py printed it and exited 0, the
# script carried on, and random ticks grew kelp through every subsequent run
# while the transcript looked correct.
#
# This is CLAUDE.md's "treat an audit that prints nothing as a failure to run"
# in a different costume — here it printed the failure and nobody read it. So
# every command a caller depends on goes through this wrapper, which reads the
# response and fails on Brigadier's own error markers.
rcon_checked() {
  local response
  response="$(rcon "$1")" || {
    echo "save-parity: RCON transport failed for '$1'" >&2
    return 1
  }
  echo "$response"
  case "$response" in
    *'<--[HERE]'* | *'Unknown or incomplete command'* | *'Incorrect argument for command'* | \
      *'Expected whitespace to end one argument'*)
      echo "save-parity: vanilla REJECTED the command '$1':" >&2
      echo "  $response" >&2
      echo "  A rejected command is not a failed call — the gate would otherwise run with this" >&2
      echo "  setting silently absent. Fix the command; do not remove this check." >&2
      return 1
      ;;
  esac
}

# **A server with no players loads almost nothing, and that makes the whole
# gate vacuous.** Measured on a real 26.2 world handed to this script: `Loading
# 0 persistent chunks... / Preparing spawn area: 100% / Time elapsed: 16 ms`,
# and not one region chunk was touched or rewritten. A caller that then diffed
# the directory would be comparing its own bytes to themselves and passing.
#
# So the caller's commands are expected to include a `forceload add` covering
# every chunk it intends to compare, and this script does not second-guess which
# ones — the coordinates are the caller's fixture, and hardcoding a box here
# would silently shrink somebody's gate later. `forceload query` is echoed after
# them so a run's transcript records how many chunks vanilla agreed to hold.
# **Freeze the tick loop before anything is loaded.** The world is being loaded
# and saved, not played, and every tick of simulation between the two is a
# difference the caller would have to allowlist. Measured over a fragment of a
# real world with the clock running: random ticks grew kelp and cave vines
# (`water[level=0]` -> `kelp[age=24]`, `kelp[age=23]` -> `kelp_plant`), gravel
# fell (a matched `water[level=0]` -> `gravel` / `gravel` -> `water[level=0]`
# pair), and pending fluid ticks were rescheduled — 60-odd cells and 11 ticks of
# pure noise, all of it correct vanilla behaviour and none of it about the save
# format.
#
# Freezing is safe here and the reason is specific: `runGameElements =
# !isFrozen || frozenTicksToRun > 0` gates *block* ticks, random ticks and
# entity ticks, but **not** the chunk source. Chunk loading, promotion to FULL
# and the relight that comes with it still run — and this gate depends on that,
# because the relight is what marks our `isLightOn = 0` chunks unsaved. If a
# future version changes that, the caller's `vanilla_rewrote` control fails
# rather than the gate passing vacuously; that control exists for exactly this.
#
# `random_tick_speed` is belt-and-braces on top of the freeze, and its *name* is
# 26.2-specific: the rule was `randomTickSpeed` before this version
# (`GameRules.java:74`, `registerInteger("random_tick_speed", ...)`, with a
# `GameRuleRegistryFix` for the old spelling). Both go through `rcon_checked`,
# so a rename in a later version fails the run instead of being ignored.
rcon_checked 'tick freeze'
rcon_checked 'gamerule random_tick_speed 0'

for command in "$@"; do
  echo "save-parity: rcon> $command"
  rcon_checked "$command"
done
if [ "$#" -gt 0 ]; then
  echo "save-parity: rcon> forceload query"
  rcon_checked 'forceload query'
fi

echo "save-parity: settling for ${SETTLE_SECONDS}s so requested chunks load and light..."
sleep "$SETTLE_SECONDS"

rcon_checked 'save-all flush'
# `stop` closes the RCON socket as the server shuts down, so a non-zero exit
# here is expected rather than a failure. The real completion signal is the
# container going away, checked next.
rcon 'stop' || true

echo "save-parity: waiting for a clean exit (the save happens in stopServer)..."
for _ in $(seq 1 60); do
  if ! container inspect "$NAME" >/dev/null 2>&1; then
    if grep -q 'All chunks are saved' "$LOG" 2>/dev/null; then
      echo "save-parity: done; $SERVER_ROOT/$LEVEL_NAME is vanilla's own save"
      exit 0
    fi
    echo "save-parity: the server exited without completing a save; see $LOG" >&2
    tail -40 "$LOG" 2>/dev/null >&2 || true
    exit 1
  fi
  sleep 2
done

echo "save-parity: the server did not exit within 120 s of 'stop'" >&2
tail -40 "$LOG" 2>/dev/null >&2 || true
exit 1
