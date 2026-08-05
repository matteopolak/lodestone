#!/usr/bin/env bash
# Start (or restart) the flat creative 26.2 oracle.
#
# This is the most widely depended-on oracle in the repo:
#
#   crates/lodestone-shell/tests/live_world_mesh.rs   (the live-world render gate)
#   crates/protocol/v770/tests/*                      (RCON-driven block/state gates)
#   scripts/live-oracles/terrain.sh                   (copies this world's server.jar)
#
# A **superflat, creative, peaceful** world is deliberate: tests need to *cause* an
# exact block arrangement over RCON without worldgen noise or mobs perturbing it.
# Do not switch this to normal terrain — use terrain.sh (:25580) when a test needs
# hills/caves, and keep both.
#
#   game port : 25570
#   RCON port : 25571  (password below; local-only, never exposed off-host)
#   world     : .cache/mc/creative  (bind-mounted, survives `container rm`)
#
# Runs `--rm` so the container self-cleans on stop. The world directory is
# gitignored and intentionally kept by cleanup.sh, so re-running this is cheap.
#
# Once the server is up, the interactive account named by $LODESTONE_OP_NAME
# (default below) is opped over RCON via rcon-op.py — see that file for why
# RCON `op <name>` and not a hand-written ops.json. This assumes RCON is
# already enabled in this world's server.properties (rcon.port=25571,
# rcon.password=lodestone), which this script does not manage — see terrain.sh
# for the same assumption and why it isn't rewritten here.
#
# # Runtime: Apple `container`
#
# This oracle runs under Apple's `container` CLI
# (https://github.com/apple/container). Docker is gone from this script —
# see docs/oracle-runtimes.md for the migration writeup (memory numbers, the
# traps below, and the couple of things not yet ported: legacy-1.12.sh's and
# worldgen-oracle/run.sh's status, and the orchestrator's out-of-repo
# cleanup.sh, which still targets `docker ps --filter`).
set -euo pipefail

NAME=lodestone-creative
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/creative"
RCON_PORT=25571
RCON_PASSWORD=lodestone

if [ ! -f "$WORLD/server.jar" ]; then
  echo "no server.jar at $WORLD — fetch the 26.2 dedicated server jar there first" >&2
  echo "(this world is the source terrain.sh copies its jar from, so it must exist)" >&2
  exit 1
fi

# `pause-when-empty-seconds` defaults to **60**, and it silently makes every
# RCON timing gate vacuous.
#
# With no player connected, the server pauses the whole world after a minute:
# `gameTime` stops advancing, and because `ServerLevel.tick` calls
# `blockTicks.tick(getGameTime())`, **no scheduled block tick ever fires again**.
# Every oracle here drives the world over RCON with nobody logged in, so this is
# the normal state, not an edge case.
#
# What makes it dangerous rather than merely broken is that the rig still looks
# half-alive: redstone dust propagates synchronously inside `setBlock`, so a dust
# probe answers correctly while anything with a delay — repeaters, comparators,
# observers, torches — never changes. A gate measuring "the signal arrived"
# passes; a gate measuring *when* it arrived reads a frozen world.
#
# This was root-caused (#315/#317) after a falling-sand control failed, and it is
# also the likely source of the older folklore that `tick step N` does not
# advance scheduled block ticks: the jar sets
# `runGameElements = !isFrozen || frozenTicksToRun > 0` and gates block ticks on
# exactly that, so a paused world and a frozen one present identically.
if [ -f "$WORLD/server.properties" ]; then
  if grep -q '^pause-when-empty-seconds=' "$WORLD/server.properties"; then
    sed -i '' 's/^pause-when-empty-seconds=.*/pause-when-empty-seconds=0/' "$WORLD/server.properties"
  else
    echo 'pause-when-empty-seconds=0' >> "$WORLD/server.properties"
  fi
fi

# Idempotent: a no-op if the system services are already up. Unlike Docker
# Desktop (launched by hand before any script runs), `container run` does not
# start its own services — this script has to.
container system start >/dev/null 2>&1 || true

container rm -f "$NAME" >/dev/null 2>&1 || true

# Two traps measured against this exact image, both load-bearing:
#
# * NEVER publish with a host-IP prefix (`-p 127.0.0.1:25571:25571`) — it
#   accepts the TCP connection and then resets on the first byte, every time
#   (found by negative control against vanilla's RCON). The bare
#   `host:container` form below is required, and it listens on all
#   interfaces — same exposure this script always had under Docker's bare
#   form, so this is parity, not a new hazard. Upstream apple/container#2029
#   also reports localhost forwarding broken on the macOS 27 beta: treat this
#   port relay as a fragility hotspot, not a solved problem, and re-verify
#   after any `container` upgrade.
# * `--memory 3g` is required. The per-VM default is 1 GiB and this JVM's own
#   `-Xmx2G` blows straight through that with no `--memory` override.
#
# No explicit `container image pull` here — `container run`'s on-demand pull
# defaults to the host's arch (arm64), which is what we want. An *explicit*
# `pull` of this image without `--platform linux/arm64` fetches the whole
# multi-arch manifest (measured: 5.29 GB / 64 blobs, versus 150.6 MB / 9 blobs
# pinned) — a trap for later, not one this script hits.
container run -d --rm --name "$NAME" \
  --memory 3g \
  -p 25570:25570 -p 25571:25571 \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:25-jdk \
  java -Xmx2G -jar server.jar nogui

# Best-effort: op the interactive account so client-side testing affordances
# (e.g. `/givedebug`) work. Never fails the script — a live gate that depends
# on this oracle starting must not break because RCON hiccupped.
op_interactive_player() {
  local op_name="${LODESTONE_OP_NAME:-LodestonePlayer}"
  local attempt
  for attempt in $(seq 1 10); do
    if python3 "$ROOT/scripts/live-oracles/rcon-op.py" 127.0.0.1 "$RCON_PORT" "$RCON_PASSWORD" "op $op_name" >/dev/null; then
      echo "opped '$op_name' on :$RCON_PORT (override with LODESTONE_OP_NAME)"
      return 0
    fi
    sleep 1
  done
  echo "warning: could not op '$op_name' via RCON on :$RCON_PORT — /givedebug and other op-gated commands will be refused (override the name with LODESTONE_OP_NAME)" >&2
}

echo "waiting for '$NAME' to accept connections..."
for _ in $(seq 1 60); do
  if container logs "$NAME" 2>&1 | grep -q 'Done ('; then
    op_interactive_player
    # ...and keep opping, for every player that joins from now on. The live gates
    # join under `unique_username`, so no gate's name can be opped in advance —
    # see op-on-join.sh for why the log is watched rather than ops.json written.
    # Backgrounded and detached: it outlives this script and dies with the
    # container, since `container logs -f` exits when the container stops.
    nohup "$ROOT/scripts/live-oracles/op-on-join.sh" \
      "$NAME" "$RCON_PORT" "$RCON_PASSWORD" >>"$WORLD/op-on-join.log" 2>&1 &
    disown 2>/dev/null || true
    echo "op-on-join watching '$NAME' (log: $WORLD/op-on-join.log)"
    echo "ready: game on :25570, RCON on :25571"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: container logs $NAME" >&2
exit 1
