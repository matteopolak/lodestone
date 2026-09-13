#!/usr/bin/env bash
# Op every player that joins a live oracle, for as long as the oracle runs.
#
# Shared by creative.sh, survival.sh and terrain.sh. Each of them used to op a
# single name once at startup (`$LODESTONE_OP_NAME`, default `LodestonePlayer`),
# which is the wrong shape for this repo:
#
#   * The live gates join under `lodestone_testsupport::unique_username`, which
#     is deliberately unpredictable — CLAUDE.md requires it because offline mode
#     derives the account UUID from the *username*, so two runs sharing a name
#     share one persisted player file, and **a dead player is held on the death
#     screen, which sends no chunks**. That is a silent, total chunk blackout
#     while join, keep-alives and entity movement all continue perfectly. So no
#     gate's name can be known in advance, and none of them was ever opped.
#   * An interactive account is whatever the person typed, not the default.
#
# # Why watch the log rather than pre-write ops.json
#
# `ops.json` needs UUIDs, and deriving an offline UUID means reproducing
# Mojang's MD5-based version-3 `UUID` over `"OfflinePlayer:" + name` — the
# hand-rolled reimplementation CLAUDE.md warns burns sessions when subtly wrong,
# with nothing to verify it against offline. `op <name>` makes the server do its
# own deriving. See rcon-op.py's header for the same argument at more length.
#
# There is no vanilla server setting that ops everyone; `op-permission-level`
# sets what an op *can* do, not who is one. Nor can a datapack do it — `/op` is
# not available to functions or command blocks. Watching the log is the
# mechanism, not a workaround for one.
#
# # Which log line — and the one that looks right and never fires
#
# This keys on `PlayerList`'s
# `"{}[{}] logged in with entity id {} at ({}, {}, {})"`
# (`.cache/mc/26.2/src/.../players/PlayerList.java:154`).
#
# The obvious-looking alternative is `ServerLoginPacketListenerImpl`'s
# `"UUID of player {} is {}"` — earlier in login, and its name is not followed
# by `[`, so it needs less parsing. **It is never emitted here.** That statement
# lives inside the `User Authenticator #N` thread that `handleKey` starts, i.e.
# on the *encryption-response* path, which is the online-mode branch
# (`ServerLoginPacketListenerImpl.java:200`, inside the `hasJoinedServer`
# success arm). An offline-mode server never reaches it. This script shipped
# keyed on that line and opped **nobody**: zero occurrences across two days of
# a running oracle's log, no error, no output, and the self-check below passed
# throughout because it was seeded with a line transcribed *from the source*
# rather than captured from a server.
#
# That is CLAUDE.md's *world* species of vacuous test — the flaw is in the input
# data, so no amount of reading the check finds it. Hence: **the accept sample
# below is a real line, pasted out of a running oracle's log** (captured with
# `docker logs` back when this ran under Docker; the line itself is vanilla's
# own log format and is unaffected by which container CLI fetched it). If you
# change the pattern, re-capture it; do not write one from the format string.
#
# # How to change it
#
# Takes the container name, RCON port and password, and runs until the container
# stops (`container logs -f` exits on its own then, so there is no PID to reap).
# Callers background it and never let it fail the parent: an oracle that started
# fine must not be reported as broken because RCON hiccupped.
#
# Idempotent by construction — re-opping an already-opped player is a no-op that
# answers "Nothing changed", so a reconnect or a duplicate log line costs one
# RCON round trip and nothing else.
#
# Parsing is one `grep` plus bash parameter expansion rather than a second
# `grep -o` or a `sed`, for a buffering reason: BSD `sed` needs `-l` to line-
# buffer and GNU `sed` needs `-u`, so a portable unbuffered `sed` in a pipeline
# is a trap that fails in the "silently does nothing" direction. `read` in a
# `while` loop is line-oriented by definition and needs no flag.
#
# **`grep --line-buffered` is load-bearing.** Without it grep blocks until its
# output buffer fills, so ops arrive in bursts minutes late, or never for a
# quiet server. Do not add a `| head` either: it cannot flush at all.
set -uo pipefail

CONTAINER="${1:?usage: op-on-join.sh <container> <rcon-port> <rcon-password>}"
RCON_PORT="${2:?missing rcon port}"
RCON_PASSWORD="${3:?missing rcon password}"

HERE="$(cd "$(dirname "$0")" && pwd)"

# Defined once so the self-check tests the *same* expression rather than a
# restatement of it. Vanilla's layout is `[%d{HH:mm:ss}] [%t/%level]: %msg`;
# `[^]]*` cannot cross a `]`, so the two bracket groups match exactly one way and
# a chat line (which renders as `<name> text`, starting with `<`) cannot occupy
# the name position.
LOGIN_RE='^\[[^]]*\] \[[^]]*\]: [A-Za-z0-9_]{1,16}\[/[^]]+\] logged in with entity id [0-9]+'

# `[17:52:03] [Server thread/INFO]: Name[/192.168.65.1:52357] logged in …`
#   → strip through the first `]: `, then take up to the first `[`.
# `\[` so bash reads the bracket literally instead of opening a glob class.
player_name_from() {
  local rest="${1#*]: }"
  printf '%s' "${rest%%\[*}"
}

# Run every start. Both directions, because this pattern has failed both ways:
# once matching nothing at all and opping nobody, once accepting a forgery — and
# *neither* was visible to `bash -n`. A silent no-op is much the worse of the
# two, so the accept case is checked first.
#
# `good` is captured verbatim from `docker logs lodestone-survival`. See the
# header: seeding this from the Java format string is what let a pattern that
# matched zero real lines pass for two days.
self_check() {
  local good='[17:52:03] [Server thread/INFO]: E1_3512rn[/192.168.65.1:52357] logged in with entity id 472 at (-41.5, 70.0, -381.5)'
  local chat='[16:09:03] [Server thread/INFO]: <Sneaky> Sneaky[/1.2.3.4:5] logged in with entity id 9'

  if ! printf '%s\n' "$good" | grep -qE "$LOGIN_RE"; then
    echo "op-on-join: FATAL — the pattern does not match a real login line;" >&2
    echo "  nobody would be opped. The server log format has probably changed." >&2
    return 1
  fi
  if [ "$(player_name_from "$good")" != "E1_3512rn" ]; then
    echo "op-on-join: FATAL — name extraction returned" \
         "'$(player_name_from "$good")', expected 'E1_3512rn'." >&2
    return 1
  fi
  if printf '%s\n' "$chat" | grep -qE "$LOGIN_RE"; then
    echo "op-on-join: FATAL — a chat line matches the login pattern." >&2
    return 1
  fi
}

self_check || exit 1

# **No `--since`**, so the whole log is replayed before following. Two reasons,
# and the first was measured the hard way: `--since 0m` is a *relative duration*,
# so it resolves to `now - 0` and shows nothing historical at all — a watcher
# started with it produced an empty log against a container holding eight real
# joins. It reads as "from the beginning" and means the opposite.
#
#   * The caller only starts us once the server reports `Done (`, and a client
#     can connect before that, so the ready-wait window has to be replayed.
#   * Replaying makes a restart self-healing: re-attaching to a live oracle
#     re-ops everyone who is still on it.
#
# Both are safe because `op` is idempotent — a duplicate answers "Nothing
# changed" for one RCON round trip.
container logs -f "$CONTAINER" 2>&1 \
  | grep --line-buffered -E "$LOGIN_RE" \
  | while IFS= read -r line; do
      name="$(player_name_from "$line")"
      [ -n "$name" ] || continue
      if python3 "$HERE/rcon-op.py" 127.0.0.1 "$RCON_PORT" "$RCON_PASSWORD" \
           "op $name" >/dev/null 2>&1; then
        echo "op-on-join: opped '$name'"
      else
        echo "op-on-join: failed to op '$name' (RCON on :$RCON_PORT)" >&2
      fi
    done
