#!/usr/bin/env bash
# Start (or restart) a real vanilla 1.12.2 (protocol 340) oracle, for
# verifying `lodestone-v340`'s chunk decode + flattening-table canonicalisation
# against a genuine pre-Flattening server rather than only against fixtures
# derived from this project's own encoder — see
# docs/protocol-340-canonical-bridge.md for why that distinction matters and
# what this oracle was used to check.
#
#   game port : 25568  (matches tests/live_chunk.rs's LODESTONE_V340_PORT default)
#   RCON port : 25569  (password below; local-only, never exposed off-host)
#   world     : .cache/mc/1.12.2  (bind-mounted, already fetched per the
#               project briefing — this script does not download anything)
#
# Deliberately different ports from every other oracle in this directory
# (26.2's :25565/:25570-1/:25580-1) so this can run alongside them.
#
# 1.12.2's server.jar targets Java 8; `eclipse-temurin:8-jdk` is used rather
# than the newer JDK the 26.2 oracles use, even though a modern JVM would
# probably still load 1.12.2's class files (old bytecode is forward-compatible)
# — no reason to rely on that when the exact intended runtime is one `docker
# run` away.
#
# This script *does* manage server.properties (unlike creative.sh/terrain.sh,
# which assume RCON is pre-configured): the cached instance was fetched with
# RCON disabled and the default port, so both are patched in place here,
# idempotently, before every start.
set -euo pipefail

NAME=lodestone-legacy-1-12
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/1.12.2"
GAME_PORT=25568
RCON_PORT=25569
RCON_PASSWORD=lodestone

if [ ! -f "$WORLD/server.jar" ]; then
  echo "no server.jar at $WORLD — per the project briefing this should already be fetched" >&2
  exit 1
fi

PROPS="$WORLD/server.properties"
set_prop() {
  local key="$1" value="$2"
  if grep -q "^${key}=" "$PROPS" 2>/dev/null; then
    sed -i.bak "s/^${key}=.*/${key}=${value}/" "$PROPS" && rm -f "$PROPS.bak"
  else
    echo "${key}=${value}" >> "$PROPS"
  fi
}
touch "$PROPS"
set_prop server-port "$GAME_PORT"
set_prop enable-rcon true
set_prop rcon.port "$RCON_PORT"
set_prop rcon.password "$RCON_PASSWORD"
# Offline mode: no Mojang auth needed for a local oracle (same as the 26.2
# oracles' assumption, per this repo's live-server hazards).
set_prop online-mode false

docker rm -f "$NAME" >/dev/null 2>&1 || true

docker run -d --rm --name "$NAME" \
  -p "$GAME_PORT:$GAME_PORT" -p "$RCON_PORT:$RCON_PORT" \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:8-jdk \
  java -Xmx2G -jar server.jar nogui

echo "waiting for '$NAME' to accept connections..."
for _ in $(seq 1 60); do
  if docker logs "$NAME" 2>&1 | grep -q 'Done ('; then
    echo "ready: game on :$GAME_PORT, RCON on :$RCON_PORT"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
