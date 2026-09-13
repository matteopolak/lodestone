#!/usr/bin/env bash
# Start (or restart) a real vanilla Minecraft 1.21.11 server as the live oracle
# for the `crates/versions/1.21.11` era (protocol 774).
#
#   version   family     game port  RCON port  container name
#   1.21.11   v1-21-11   25604      25605      lodestone-mc12111
#
# The ports are the next two free above every port any other script in this
# directory publishes, and are named by
# crates/versions/1.21.11/tests/capture_join.rs.
#
# `eclipse-temurin:21-jdk` because this jar's own version.json declares
# `"java_version": 21`; it refuses to start under 17.
#
# Server properties beyond the base set (port, RCON, offline mode):
#   - a flat world, so a join capture is small and every column it records is
#     the same shape on every run;
#   - enforce-secure-profile=false, because from 1.19 a server may require every
#     chat message to carry a real signature and this client has no session key.
#     With enforcement on the server rejects the join outright, not the message;
#   - the no-natural-spawn properties, so the recorded window around one RCON
#     `/summon` contains that spawn and no unrelated ones.
#
# Same three traps as every other oracle script here (see
# docs/oracles-and-benchmarks.md): publish a port with no host-IP prefix,
# `--memory 3g` because the per-VM default is smaller than the JVM's own
# -Xmx2G, and readiness is polled by grepping `container logs` for the
# server's own "Done (" line because that subcommand has no --since.
#
# Usage: ./mc-1-21-11.sh
#
# Runtime: Apple `container`, not Docker.
set -euo pipefail

VERSION=1.21.11
NAME=lodestone-mc12111
GAME_PORT=25604
RCON_PORT=25605
RCON_PASSWORD=lodestone
JDK_IMAGE=eclipse-temurin:21-jdk

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/$VERSION"

if [ ! -f "$WORLD/server.jar" ]; then
  echo "no server.jar at $WORLD -- fetch it first: cargo run -p xtask -- fetch-version --version $VERSION" >&2
  exit 1
fi

printf 'eula=true\n' > "$WORLD/eula.txt"

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
set_prop online-mode false
set_prop level-type flat
set_prop enforce-secure-profile false
set_prop spawn-monsters false
set_prop spawn-animals false
set_prop difficulty peaceful

container system start >/dev/null 2>&1 || true
container rm -f "$NAME" >/dev/null 2>&1 || true

container run -d --rm --name "$NAME" \
  --memory 3g \
  -p "$GAME_PORT:$GAME_PORT" -p "$RCON_PORT:$RCON_PORT" \
  -v "$WORLD":/w -w /w \
  "$JDK_IMAGE" \
  java -Xmx2G -jar server.jar nogui

echo "waiting for '$NAME' to accept connections..."
for _ in $(seq 1 60); do
  if container logs "$NAME" 2>&1 | grep -q 'Done ('; then
    echo "ready: $VERSION game on :$GAME_PORT, RCON on :$RCON_PORT (container $NAME)"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: container logs $NAME" >&2
exit 1
