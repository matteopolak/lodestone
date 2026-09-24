#!/usr/bin/env bash
# Start the official Hermitcraft S10 world as the large client benchmark oracle.
#
# Provision it first with:
#
#   python3 scripts/install-client-benchmark-world.py
#
# game :25590, RCON :25591, local cache `.cache/mc/megaworld`.
set -euo pipefail

NAME=lodestone-megaworld-oracle
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/megaworld"
RCON_PORT=25591
RCON_PASSWORD=lodestone

if [ ! -f "$WORLD/.lodestone-benchmark-world.json" ] || [ ! -f "$WORLD/server.jar" ]; then
  echo "large benchmark world is not installed; run:" >&2
  echo "  python3 scripts/install-client-benchmark-world.py" >&2
  exit 1
fi

container system start >/dev/null 2>&1 || true
container rm -f "$NAME" >/dev/null 2>&1 || true

container run -d --rm --name "$NAME" \
  --memory 6g \
  -p 25590:25590 -p 25591:25591 \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:25-jdk \
  java -Xmx4G -jar server.jar nogui

op_interactive_player() {
  local op_name="${LODESTONE_OP_NAME:-LodestonePlayer}"
  local attempt
  for attempt in $(seq 1 20); do
    if python3 "$ROOT/scripts/live-oracles/rcon-op.py" \
      127.0.0.1 "$RCON_PORT" "$RCON_PASSWORD" "op $op_name" >/dev/null; then
      echo "opped '$op_name' on :$RCON_PORT (override with LODESTONE_OP_NAME)"
      return 0
    fi
    sleep 1
  done
  echo "warning: could not op '$op_name' on :$RCON_PORT" >&2
}

echo "waiting for '$NAME' to load and convert the imported world..."
for _ in $(seq 1 180); do
  if container logs "$NAME" 2>&1 | /usr/bin/grep -q 'Done ('; then
    op_interactive_player
    nohup "$ROOT/scripts/live-oracles/op-on-join.sh" \
      "$NAME" "$RCON_PORT" "$RCON_PASSWORD" >>"$WORLD/op-on-join.log" 2>&1 &
    disown 2>/dev/null || true
    echo "op-on-join watching '$NAME' (log: $WORLD/op-on-join.log)"
    echo "ready: game on :25590, RCON on :25591"
    exit 0
  fi
  sleep 5
done

echo "timed out waiting for server ready; check: container logs $NAME" >&2
exit 1
