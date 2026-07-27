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
set -euo pipefail

NAME=lodestone-terrain-oracle
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/terrain"

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

echo "waiting for '$NAME' to finish generating the world..."
for _ in $(seq 1 60); do
  if docker logs "$NAME" 2>&1 | grep -q 'Done ('; then
    echo "ready: game on :25580"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
