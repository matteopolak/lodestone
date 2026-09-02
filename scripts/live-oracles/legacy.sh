#!/usr/bin/env bash
# Start (or restart) a real vanilla oracle for one of the pre-1.17 protocol
# families this repo still ports by hand, generalising legacy-1.12.sh's
# pattern to the two families whose #[ignore]d live gates target a container
# no script here creates:
#
#   version   family  game port  RCON port  container name
#   1.8.9     v47     25566      25576      lodestone-mc189
#   1.12.2    v340    25568      25569      lodestone-legacy-1-12
#   1.16.5    v735    25573      25574      lodestone-mc1165
#
# (container name / ports read directly off crates/protocol/<fam>/tests/
# live_*.rs's own doc comments and #[ignore] messages -- not invented here.)
#
# 1.12.2 already had a working script (legacy-1.12.sh); this one covers the
# other two, and also answers for 1.12.2 so one entry point serves all three.
# legacy-1.12.sh is left in place untouched -- v340's live test doc comments
# name it directly, and there is no reason to disturb a script that already
# works.
#
# Usage: ./legacy.sh <version>   (1.8.9 | 1.12.2 | 1.16.5)
#
# Runtime: Apple `container`, not Docker -- see docs/oracles-and-benchmarks.md
# ("Oracle runtimes: Apple container"). That content used to live in
# docs/oracle-runtimes.md, which a concurrent docs-consolidation commit
# (1bb71676) deleted; docs/README.md's own generated index still links the
# dead path as of this writing -- committed drift this script's own doc
# pointer deliberately does not repeat.
#
# Same three traps every ported oracle script here has to account for
# (measured directly against these images, per that doc): never publish a
# port with a host-IP prefix (`-p 127.0.0.1:PORT:PORT` resets on the first
# byte -- bare `-p PORT:PORT` is correct and is parity with every other
# script in this directory, not a new exposure), `--memory 3g` because the
# per-VM default (1 GiB) is smaller than every JVM here's own `-Xmx2G`, and
# `container logs` has no `--since` -- readiness is polled by grepping a
# fixed line count for the server's own "Done (" line, same as every other
# script in this directory.
set -euo pipefail

usage() {
  echo "usage: $0 <version>" >&2
  echo "  supported versions: 1.8.9 (v47), 1.12.2 (v340), 1.16.5 (v735)" >&2
  exit 1
}

VERSION="${1:-}"
[ -n "$VERSION" ] || usage

# Extra server.properties this version's live gates need beyond the base set
# every oracle here always sets (server-port, RCON, online-mode) -- read off
# each family's own live_*.rs doc comments, not guessed:
#   - v47 (live_entity.rs): "flat world, spawn-monsters=true"
#   - v340 (legacy-1.12.sh, unchanged): no extra properties -- vanilla
#     defaults, matching the script this generalises
#   - v735 (live_entity.rs / live_interaction.rs): "flat world, RCON enabled";
#     live_entity.rs additionally: "spawn-monsters=false/spawn-animals=false
#     ... difficulty=peaceful" (it summons what it needs over RCON instead of
#     relying on natural spawns)
EXTRA_PROPS=()

case "$VERSION" in
  1.8.9)
    NAME=lodestone-mc189
    GAME_PORT=25566
    RCON_PORT=25576
    EXTRA_PROPS=(level-type=FLAT spawn-monsters=true spawn-animals=true)
    ;;
  1.12.2)
    NAME=lodestone-legacy-1-12
    GAME_PORT=25568
    RCON_PORT=25569
    EXTRA_PROPS=()
    ;;
  1.16.5)
    NAME=lodestone-mc1165
    GAME_PORT=25573
    RCON_PORT=25574
    EXTRA_PROPS=(level-type=FLAT spawn-monsters=false spawn-animals=false difficulty=peaceful)
    ;;
  *)
    echo "unsupported version: $VERSION" >&2
    usage
    ;;
esac

RCON_PASSWORD=lodestone
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/$VERSION"

if [ ! -f "$WORLD/server.jar" ]; then
  echo "no server.jar at $WORLD -- fetch it first: cargo run -p xtask -- fetch-version --version $VERSION" >&2
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
# Offline mode: no Mojang auth needed for a local oracle (same as every other
# script in this directory).
set_prop online-mode false
for entry in "${EXTRA_PROPS[@]+"${EXTRA_PROPS[@]}"}"; do
  set_prop "${entry%%=*}" "${entry#*=}"
done

container system start >/dev/null 2>&1 || true

container rm -f "$NAME" >/dev/null 2>&1 || true

# `eclipse-temurin:8-jdk`, matching legacy-1.12.sh: all three of these
# server.jars target Java 8, and there is no reason to rely on a newer JVM's
# forward-compatibility with old bytecode when the exact intended runtime is
# one `container run` away.
container run -d --rm --name "$NAME" \
  --memory 3g \
  -p "$GAME_PORT:$GAME_PORT" -p "$RCON_PORT:$RCON_PORT" \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:8-jdk \
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
