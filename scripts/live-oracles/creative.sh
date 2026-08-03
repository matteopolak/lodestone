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
#   world     : .cache/mc/creative  (bind-mounted, survives `docker rm`)
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

docker rm -f "$NAME" >/dev/null 2>&1 || true

docker run -d --rm --name "$NAME" \
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
  if docker logs "$NAME" 2>&1 | grep -q 'Done ('; then
    op_interactive_player
    # ...and keep opping, for every player that joins from now on. The live gates
    # join under `unique_username`, so no gate's name can be opped in advance —
    # see op-on-join.sh for why the log is watched rather than ops.json written.
    # Backgrounded and detached: it outlives this script and dies with the
    # container, since `docker logs -f` exits when the container stops.
    nohup "$ROOT/scripts/live-oracles/op-on-join.sh" \
      "$NAME" "$RCON_PORT" "$RCON_PASSWORD" >>"$WORLD/op-on-join.log" 2>&1 &
    disown 2>/dev/null || true
    echo "op-on-join watching '$NAME' (log: $WORLD/op-on-join.log)"
    echo "ready: game on :25570, RCON on :25571"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
