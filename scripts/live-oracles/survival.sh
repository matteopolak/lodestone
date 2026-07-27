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
set -euo pipefail

NAME=lodestone-survival
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORLD="$ROOT/.cache/mc/survival"
JAR_SRC="$ROOT/.cache/mc/26.2/server.jar"

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

echo "waiting for '$NAME' to generate terrain (first run takes a minute)..."
for _ in $(seq 1 90); do
  if docker logs "$NAME" 2>&1 | grep -q 'Done ('; then
    echo "ready: survival world on :25565 (RCON :25566)"
    exit 0
  fi
  sleep 5
done
echo "timed out waiting for server ready; check: docker logs $NAME" >&2
exit 1
