#!/usr/bin/env bash
# Start (or restart) a real vanilla oracle for one of the pre-1.17 protocol
# families this repo still ports by hand, generalising legacy-1.12.sh's
# pattern to the two families whose #[ignore]d live gates target a container
# no script here creates:
#
#   version   family  game port  RCON port  container name
#   1.8.9     v1-8    25566      25576      lodestone-mc189
#   1.9.4     v1-9    25580      25581      lodestone-mc194
#   1.10.2    v1-9    25582      25583      lodestone-mc1102
#   1.11.2    v1-9    25584      25585      lodestone-mc1112
#   1.12.2    v1-9    25568      25569      lodestone-legacy-1-12
#   1.13.2    v1-13   25590      25591      lodestone-mc1132
#   1.14.4    v1-14   25586      25587      lodestone-mc1144
#   1.15.2    v1-14   25588      25589      lodestone-mc1152
#   1.16.5    v1-14   25573      25574      lodestone-mc1165
#
# (1.8.9/1.12.2/1.16.5 container names and ports read directly off
# crates/versions/<fam>/tests/live_*.rs's own doc comments and #[ignore]
# messages -- not invented here. The three 1.9-era rows arrived with the era
# merge that made crates/versions/1.9 speak 110, 210 and 316 as well as 340;
# their ports avoid every port already used here and are named by
# crates/versions/1.9/tests/capture_join.rs. The two 1.14-era rows arrived
# the same way, when crates/versions/1.14 gained 498 and 578; their ports are
# named by crates/versions/1.14/tests/capture_join.rs. The 1.13.2 row arrived
# the same way, when crates/versions/1.13 landed as a single-version era; its
# ports are the next two free above every port already listed here, and are
# named by crates/versions/1.13/tests/capture_join.rs.)
#
# 1.12.2 already had a working script (legacy-1.12.sh); this one covers the
# rest, and also answers for 1.12.2 so one entry point serves them all.
# legacy-1.12.sh is left in place untouched -- v1-9's live test doc comments
# name it directly, and there is no reason to disturb a script that already
# works.
#
# Usage: ./legacy.sh <version>
#        (1.8.9 | 1.9.4 | 1.10.2 | 1.11.2 | 1.12.2 | 1.13.2 | 1.14.4 | 1.15.2 | 1.16.5)
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
  echo "  supported versions: 1.8.9 (v1-8); 1.9.4, 1.10.2, 1.11.2, 1.12.2 (v1-9); 1.13.2 (v1-13); 1.14.4, 1.15.2, 1.16.5 (v1-14)" >&2
  exit 1
}

VERSION="${1:-}"
[ -n "$VERSION" ] || usage

# Extra server.properties this version's live gates need beyond the base set
# every oracle here always sets (server-port, RCON, online-mode) -- read off
# each family's own live_*.rs doc comments, not guessed:
#   - v1-8 (live_entity.rs): "flat world, spawn-monsters=true"
#   - v1-9 at 1.12.2 (legacy-1.12.sh, unchanged): no extra properties --
#     vanilla defaults, matching the script this generalises
#   - v1-9 at 1.9.4/1.10.2/1.11.2 (capture_join.rs): a flat world only, so a
#     join capture is small and its chunk columns are the same shape every
#     run; nothing there depends on mobs
#   - v1-13 at 1.13.2 (capture_join.rs): a flat world only, for the same
#     reason. `FLAT` is still the right spelling here despite 1.13's
#     namespacing sweep -- measured, by booting with each spelling and reading
#     the resulting level.dat's generator name back: `FLAT` gives `flat`,
#     while `minecraft:flat` matches nothing and falls back to `default`
#     without warning. An unrecognised level-type is silent, not fatal, so
#     the check is the world, never the log
#   - v1-14 at 1.14.4/1.15.2 (capture_join.rs): a flat world only, so a join
#     capture is small and its chunk columns are the same shape every run;
#     nothing there depends on mobs
#   - v1-14 at 1.16.5 (live_entity.rs / live_interaction.rs): "flat world, RCON enabled";
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
  1.9.4)
    NAME=lodestone-mc194
    GAME_PORT=25580
    RCON_PORT=25581
    EXTRA_PROPS=(level-type=FLAT)
    ;;
  1.10.2)
    NAME=lodestone-mc1102
    GAME_PORT=25582
    RCON_PORT=25583
    EXTRA_PROPS=(level-type=FLAT)
    ;;
  1.11.2)
    NAME=lodestone-mc1112
    GAME_PORT=25584
    RCON_PORT=25585
    EXTRA_PROPS=(level-type=FLAT)
    ;;
  1.12.2)
    NAME=lodestone-legacy-1-12
    GAME_PORT=25568
    RCON_PORT=25569
    EXTRA_PROPS=()
    ;;
  1.13.2)
    NAME=lodestone-mc1132
    GAME_PORT=25590
    RCON_PORT=25591
    EXTRA_PROPS=(level-type=FLAT)
    ;;
  1.14.4)
    NAME=lodestone-mc1144
    GAME_PORT=25586
    RCON_PORT=25587
    EXTRA_PROPS=(level-type=FLAT)
    ;;
  1.15.2)
    NAME=lodestone-mc1152
    GAME_PORT=25588
    RCON_PORT=25589
    EXTRA_PROPS=(level-type=FLAT)
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

# The three 1.9-era jars, and the two 1.14-era ones, arrive from
# `fetch-version` as a bare server.jar in an otherwise empty directory, unlike
# 1.8.9/1.12.2/1.16.5 which were booted by hand long ago. Accepting the EULA
# is what every other script in this directory does for a fresh directory, and
# the server refuses to start without it.
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
# Offline mode: no Mojang auth needed for a local oracle (same as every other
# script in this directory).
set_prop online-mode false
for entry in "${EXTRA_PROPS[@]+"${EXTRA_PROPS[@]}"}"; do
  set_prop "${entry%%=*}" "${entry#*=}"
done

container system start >/dev/null 2>&1 || true

container rm -f "$NAME" >/dev/null 2>&1 || true

# `eclipse-temurin:8-jdk`, matching legacy-1.12.sh: every one of these
# server.jars targets Java 8, and there is no reason to rely on a newer JVM's
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
