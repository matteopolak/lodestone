#!/usr/bin/env bash
# Shared by the oracle launchers: the release they serve is the repository's
# `mc-version`, never a literal, so a version bump moves every oracle at once.
# Source this after `ROOT` is set.

MC_VERSION="$(tr -d '[:space:]' < "$ROOT/mc-version")"

# sync_server_jar <world-dir>: make <world-dir>/server.jar the cached server
# jar of $MC_VERSION. A world last run on an older release is opened by the
# newer server, which upgrades it in place.
sync_server_jar() {
  local world="$1"
  local src="$ROOT/.cache/mc/$MC_VERSION/server.jar"
  if [ ! -f "$src" ]; then
    echo "no $MC_VERSION server.jar at $src — fetch it first (docs/mc-version-bump.md)" >&2
    return 1
  fi
  mkdir -p "$world"
  if ! cmp -s "$src" "$world/server.jar"; then
    cp "$src" "$world/server.jar"
    rm -rf "$world/versions" "$world/libraries"
  fi
  [ -f "$world/eula.txt" ] || printf 'eula=true\n' > "$world/eula.txt"
}

# set_property <properties-file> <key> <value>: replace or append one line.
set_property() {
  local file="$1" key="$2" value="$3"
  [ -f "$file" ] || return 0
  if grep -q "^${key}=" "$file"; then
    sed -i '' "s/^${key}=.*/${key}=${value}/" "$file"
  else
    echo "${key}=${value}" >> "$file"
  fi
}

# The oracles are offline test rigs: nothing may refuse a joining client.
open_to_all() {
  set_property "$1/server.properties" white-list false
  set_property "$1/server.properties" enforce-whitelist false
}
