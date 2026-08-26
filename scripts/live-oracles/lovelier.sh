#!/usr/bin/env bash
# Start Stampy's Lovelier World as the open large-build client benchmark oracle.
#
# game :25600, RCON :25601, local cache `.cache/mc/lovelier`.
set -euo pipefail

NAME=lodestone-lovelier-oracle
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/lovelier"
RCON_PORT=25601
RCON_PASSWORD=lodestone

if [ ! -f "$WORLD/.lodestone-benchmark-world.json" ] || \
   [ ! -f "$WORLD/server.jar" ] || [ ! -f "$WORLD/world/level.dat" ]; then
  echo "Lovelier World benchmark cache is incomplete at $WORLD" >&2
  exit 1
fi

container system start >/dev/null 2>&1 || true
container rm -f "$NAME" >/dev/null 2>&1 || true

container run -d --rm --name "$NAME" \
  --memory 6g \
  -p 25600:25600 -p 25601:25601 \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:25-jdk \
  java -Xmx4G -jar server.jar nogui

echo "waiting for '$NAME' to load or convert the imported world..."
for _ in $(seq 1 180); do
  if container logs "$NAME" 2>&1 | /usr/bin/grep -q 'Done ('; then
    nohup "$ROOT/scripts/live-oracles/op-on-join.sh" \
      "$NAME" "$RCON_PORT" "$RCON_PASSWORD" >>"$WORLD/op-on-join.log" 2>&1 &
    disown 2>/dev/null || true
    echo "op-on-join watching '$NAME' (log: $WORLD/op-on-join.log)"
    echo "ready: game on :25600, RCON on :25601"
    exit 0
  fi
  sleep 5
done

echo "timed out waiting for server ready; check: container logs $NAME" >&2
exit 1
