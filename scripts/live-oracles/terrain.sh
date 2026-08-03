#!/usr/bin/env bash
# Start (or restart) the normal-terrain 26.2 light oracle used by
# `crates/protocol/v770/tests/live_terrain_light.rs`.
#
# Unlike the flat creative oracle on :25570, this world is `minecraft:normal`
# (hills, caves, overhangs, trees) so the light diff exercises horizontal decay
# and section seams — the paths a superflat world cannot express.
#
#   game RCON? no (not needed: terrain light is baked into every chunk)
#   game port : 25580
#   world     : .cache/mc/terrain (level-type=minecraft:normal, fixed seed)
#
# Runs `--rm` so the container self-cleans on stop. Reuses the bundled 26.2
# server.jar already fetched for the creative oracle.
#
# Once up, the interactive account named by $LODESTONE_OP_NAME (default
# below) is opped over RCON via rcon-op.py — see that file for why RCON
# `op <name>` and not a hand-written ops.json. This assumes RCON is already
# enabled in this world's server.properties (rcon.port=25581,
# rcon.password=lodestone), which — like creative.sh — this script does not
# manage: server.properties here isn't generated fresh each run, so a world
# regenerated from scratch (`rm -rf .cache/mc/terrain`) needs RCON turned on
# by hand before this op step can do anything; the op step degrades to a
# harmless warning if it can't connect.
set -euo pipefail

NAME=lodestone-terrain-oracle
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/terrain"
RCON_PORT=25581
RCON_PASSWORD=lodestone

docker rm -f "$NAME" >/dev/null 2>&1 || true

if [ ! -f "$WORLD/server.jar" ]; then
  mkdir -p "$WORLD"
  cp "$ROOT/.cache/mc/creative/server.jar" "$WORLD/server.jar"
  printf 'eula=true\n' > "$WORLD/eula.txt"
fi

docker run -d --rm --name "$NAME" \
  -p 25580:25580 -p 25581:25581 \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:25-jdk \
  java -Xmx2G -jar server.jar nogui

# Best-effort: op the interactive account so client-side testing affordances
# (e.g. `/givedebug`) work. Never fails the script — a live gate that starts
# this oracle must not break because RCON wasn't enabled or hiccupped.
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
  echo "warning: could not op '$op_name' via RCON on :$RCON_PORT — /givedebug and other op-gated commands will be refused (override the name with LODESTONE_OP_NAME, or enable RCON in $WORLD/server.properties)" >&2
}

echo "waiting for '$NAME' to finish generating the world..."
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
    echo "ready: game on :25580"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
