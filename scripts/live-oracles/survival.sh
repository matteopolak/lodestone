#!/usr/bin/env bash
# Start (or restart) a **survival, normal-terrain** 26.2 server — the one to
# actually play the client against.
#
# This is the only oracle that is not a test fixture. The others are all
# deliberately flat/peaceful/creative so gates can *cause* an exact block
# arrangement without worldgen noise; this one is the opposite on purpose:
# real terrain, real mobs, real survival, so the renderer, lighting, entities
# and physics are all exercised the way a player would.
#
#   game port : 25565
#   RCON port : 25566
#   world     : .cache/mc/survival  (bind-mounted, survives `docker rm`)
#   seed      : fixed, so the spawn area is reproducible when comparing screenshots
#
# online-mode=false so any username can join without a Mojang account.
# Runs `--rm` so the container self-cleans on stop.
#
# Once up, the interactive account named by $LODESTONE_OP_NAME (default
# below) is opped over RCON via rcon-op.py, so client-side testing
# affordances (e.g. `/givedebug`) work without the player having joined
# before — see that file for why RCON `op <name>` and not a hand-written
# ops.json. This is separate from `unique_username()`'s per-test names
# (`crates/lodestone-testsupport`): a fixed interactive name is opped once
# here and never collides with those, and no live gate depends on being op.
set -euo pipefail

NAME=lodestone-survival
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/survival"
JAR_SRC="$ROOT/.cache/mc/26.2/server.jar"
RCON_PORT=25566
RCON_PASSWORD=lodestone

if [ ! -f "$WORLD/server.jar" ]; then
  if [ ! -f "$JAR_SRC" ]; then
    echo "no 26.2 server.jar at $JAR_SRC to copy from" >&2
    exit 1
  fi
  mkdir -p "$WORLD"
  cp "$JAR_SRC" "$WORLD/server.jar"
  printf 'eula=true\n' > "$WORLD/eula.txt"
fi

# Written every run so the mode can't silently drift if someone edits it in-game.
# level-type/level-seed only take effect at generation time; delete the `world`
# directory to regenerate with different terrain.
cat > "$WORLD/server.properties" <<'PROPS'
server-port=25565
enable-rcon=true
rcon.port=25566
rcon.password=lodestone
online-mode=false
gamemode=survival
force-gamemode=false
difficulty=normal
hardcore=false
level-name=world
level-type=minecraft\:normal
level-seed=lodestone
generate-structures=true
spawn-monsters=true
spawn-animals=true
spawn-npcs=true
allow-nether=true
view-distance=10
simulation-distance=10
max-players=10
enforce-secure-profile=false
motd=Lodestone survival test world
PROPS

docker rm -f "$NAME" >/dev/null 2>&1 || true

docker run -d --rm --name "$NAME" \
  -p 25565:25565 -p 25566:25566 \
  -v "$WORLD":/w -w /w \
  eclipse-temurin:25-jdk \
  java -Xmx2G -jar server.jar nogui

# Best-effort: op the interactive account so client-side testing affordances
# (e.g. `/givedebug`) work. Never fails the script — this is the oracle a
# human plays against, and a live gate script must not break from it.
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

echo "waiting for '$NAME' to generate terrain (first run takes a minute)..."
for _ in $(seq 1 90); do
  if docker logs "$NAME" 2>&1 | grep -q 'Done ('; then
    op_interactive_player
    echo "ready: survival world on :25565 (RCON :25566)"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
