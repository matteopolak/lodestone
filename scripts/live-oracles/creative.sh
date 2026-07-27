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
set -euo pipefail

NAME=lodestone-creative
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/creative"

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

echo "waiting for '$NAME' to accept connections..."
for _ in $(seq 1 60); do
  if docker logs "$NAME" 2>&1 | grep -q 'Done ('; then
    echo "ready: game on :25570, RCON on :25571"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
