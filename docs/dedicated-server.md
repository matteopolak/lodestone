# Standalone dedicated server

## What it is

`lodestone-dedicated-server` is a new, thin binary crate — its produced
binary is named `lodestone-server` — that runs `lodestone-server` headless: no
client, no renderer, no windowing crate in the graph. Drop it into a
directory and run it; it reads `server.properties` and `eula.txt` from that
directory (writing vanilla-shaped defaults on a first run, exactly like
vanilla's own `server.jar`), enforces ops/whitelist/bans, opens a persistent,
autosaving world, hosts it over real TCP, takes commands on stdin, and saves
and closes the world cleanly on `stop` or a shutdown signal.

`lodestone-server` itself stays a library — it already has no `[[bin]]` and
still does not need one. See "How to change it" for why the binary lives in
its own crate instead.

## How it works

### The binary's dependency graph, measured

```
$ cargo tree -p lodestone-dedicated-server
```

The graph pulls in `lodestone-server`, `lodestone-registry` with its `v770`
feature (26.2 is the only family that implements `ServerProtocol` — see the
repo's own `CLAUDE.md` on why `Family`/`ServerFamily` are different tables),
`lodestone-auth` (the
crypto-provider install online-mode's TLS call needs), `reqwest` (the same
HTTP client `OnlineModeConfig` already required), and `tokio`/`tracing`/
`tracing-subscriber`. **No `lodestone-shell`, no `lodestone-render`, no GPU or
windowing crate anywhere in the tree** — `cargo tree` was run and read, not
assumed, before this doc was written.

`lodestone-server` itself already compiles for `wasm32-unknown-unknown` as
part of the browser bundle (see its own `Cargo.toml`), which is the actual
reason the binary is a separate crate rather than a `[[bin]]` target added
there: a binary target needing `tokio`'s `rt-multi-thread`/`signal` and a
real `TcpListener` would need `cfg`-gating out of a *library* crate's build
graph for one target, which is exactly the class of thing this repo's own
wasm-hazard record says goes stale silently. A separate crate needs no gate
at all — nothing in the browser bundle or `lodestone-shell` depends on it, so
it is never even considered for a wasm32 build.

#### Why the registry rather than the version crate

This crate originally depended on `lodestone-v770` and constructed
`V770ServerProtocol` by name, which is the shortest path to the only family
that can be hosted. `cargo xtask check-isolation` fails on it, and correctly:
a *required* edge from a crate outside `crates/protocol/` to a version crate
is precisely what makes that family undeletable, and the invariant is that a
family stays deletable as its folder plus a manifest line. `lodestone-registry`
is the one crate exempted from that rule, through optional feature-gated edges.

So `main` asks `lodestone_registry::server_protocol_for_protocol` for the
highest protocol in `supported_protocols()` that actually answers — a family
whose *client* adapter is compiled in need not implement `ServerProtocol` —
and never spells a family name. `None` is a real product state, reported and
exited on, not an error to route around: a build with no servable family
cannot host anything and must say so rather than starting and refusing every
join. `check-deletable v770` reports the crate cleanly deletable again.

#### `tokio::signal::unix` is a Windows compile error, not a degradation

The shutdown loop below races `Ctrl-C` against SIGTERM, and
`tokio::signal::unix` is `#![cfg(unix)]` *inside tokio*. Naming it
unconditionally is therefore `E0433: cannot find 'unix' in 'signal'` on
Windows — a hard build failure for the whole crate, which the CI matrix's
Windows leg caught and no `cargo check` on a Mac or Linux box can. The Windows
arm is `tokio::signal::windows::ctrl_shutdown`, the genuine analogue: the OS
raises it as the machine shuts down, which is the same "flush now" event a
supervisor's SIGTERM is. Windows has no way for one process to *send* it to
another, so `taskkill /F` still cannot be made graceful; that is an OS
property, not something the loop can fix.

### Startup sequence

`main.rs` in `crates/lodestone-dedicated-server/src/main.rs` is orchestration
only. In order:

1. Resolve the server directory (`argv[1]`, default `.`), creating it if
   missing.
2. `lodestone_server::eula::check` — refuse and exit `1` (after writing the
   template if the file is missing) unless `eula=true`.
3. `lodestone_server::ServerProperties::load_or_create` — parse
   `server.properties`, or write vanilla's own defaults there first.
4. `AccessHandle::load` the four ops/whitelist/ban JSON files from the server
   root (not the world directory — same layout vanilla uses), then apply
   `white-list`/`max-players` onto it.
5. Resolve the seed (`level-seed`, or a fresh random one) and the world type
   (`level-type`), and open the world with
   `IntegratedServer::open_persistent_with_mobs` — the same constructor
   `lodestone-shell`'s own "Open to LAN for a persistent singleplayer world"
   path already uses, reused rather than a new one built for this crate.
6. Drop the constructor's in-memory "local connection" duplex end unread —
   this binary is headless, so nothing ever calls `LoginStart` on it, and its
   connection task observes an immediate EOF and returns, same as any client
   that connects and disconnects before logging in.
7. Set the world's default game mode and difficulty from `server.properties`.
8. `IntegratedServer::publish_with_config` — bind `server-ip:server-port` and
   thread real `access`/`online_mode` configuration through (see
   "`PublishConfig`" below).
9. `IntegratedServer::start_rcon` if `enable-rcon=true` and a non-empty
   `rcon.password` is set.
10. Read stdin lines as console commands (`lodestone_server::console::run`),
    racing `Ctrl-C`/SIGTERM (`CTRL_SHUTDOWN` on Windows — see above); `stop`,
    `Ctrl-C` or a termination signal all break the
    loop into `IntegratedServer::shutdown().await`, which flushes the world
    (region files, `level.dat`, entities, POI) before the process exits.

### `PublishConfig` — the access-control/online-mode gap this closed

`IntegratedServer::publish` (the call that adds a TCP listener to an
already-running persistent world — the exact shape a dedicated server needs)
used to hardcode every accepted connection to `CommandDispatch::none()` and
`AccessHandle::default()`, with **no `online_mode` parameter at all**. That
was a real island, not a deliberate simplification: `crate::access` and
`OnlineModeConfig` existed and were already wired into
`IntegratedServer::open_to_lan`'s own accept loop, but `publish`'s accept
loop — the one path that adds a socket to a *persistent* world — never
consulted either. A world hosted this way could not be whitelisted, could not
enforce a ban, and could not require online-mode authentication no matter
what a caller configured, because nothing carried that configuration to the
listener.

`IntegratedServer::publish_with_config` (`crates/lodestone-server/src/integrated.rs`)
is the fix: `publish` is now a thin wrapper calling it with
`PublishConfig::default()`, which reproduces `publish`'s previous behaviour
exactly — so `lodestone-shell`'s own two-argument call site is unaffected.
The dedicated server binary is the first caller that passes a real
`PublishConfig`.

### The stdin console

`lodestone_server::console::run` (`crates/lodestone-server/src/console.rs`) is
a two-line adapter over `crate::rcon` — it builds a throwaway `RconConfig`
(its `addr`/`password` fields are never read; nothing here binds a socket)
and calls `crate::rcon::run_console_command`, which is
`crate::rcon::run_command_as` (the RCON handler's own body, generalised over
the caller's identity so it is not duplicated) under identity `"Server"` at
permission level 4 — vanilla's own dedicated-server console identity,
distinct from RCON's `"Rcon"`. The built-in command tree
(`crate::commands::ServerCommands`) runs first; a root it does not own falls
through to the host `CommandDispatch` sink, same ordering RCON and in-game
chat already use.

`IntegratedServer::players()` is a new accessor (same shape as the existing
`mobs()`/`world_state()`) that hands back the world's real, shared
`PlayerRegistry` — before it existed, neither RCON nor this console had any
way to reach a dedicated server's real connections, so `/list`/`/say`/a
targeted `/gamemode <player>` would have seen an empty registry regardless of
who was actually connected.

## `server.properties`

Real vanilla key names and real vanilla default *values*, transcribed from
this repo's own pinned 26.2 decompile
(`.cache/mc/26.2/src/net/minecraft/server/dedicated/DedicatedServerProperties.java`),
not from memory or an older Minecraft version — see
`crates/lodestone-server/src/properties.rs`'s own doc comment for the two
things that transcription caught that an assumption would not have:
**there is no `pvp` key in 26.2** (confirmed against both the decompile and
the real, vanilla-written `.cache/mc/26.2/server.properties` this repo's own
oracles already run against — neither has `pvp`/`allow-nether`/
`spawn-monsters`/`spawn-npcs`/`spawn-animals`), and **`online-mode`'s real
default is `true`**.

All 71 real keys round-trip (unknown/unmodelled keys included, in their
original position) through `RawProperties`; only the subset below is typed
and read by the dedicated-hosting path.

### Implemented — reaches a real consumer

| key | consumer |
|---|---|
| `level-seed` | `WorldOptions.parseSeed` (numeric, else Java `String::hashCode` via `lodestone_worldgen::hash::java_string_hash`, else random) |
| `level-name` | the world's subdirectory under the server root |
| `level-type` | `minecraft:normal`/`large_biomes`/`amplified` map to a real `WorldType`; anything else (**including `minecraft:flat`**) falls back to normal with a logged warning — see "Accepted, partially implemented" below |
| `gamemode` | `WorldStateHandle::set_default_game_mode` |
| `difficulty` | `WorldStateHandle::set_difficulty` |
| `max-players` | `AccessLists::set_max_players` — enforced at join (`AccessLists::may_join`) |
| `online-mode` | `PublishConfig::online_mode` → real RSA/AES-128-CFB8 handshake + session-server verification (see "Online-mode" below) |
| `view-distance` | `open_persistent_with_mobs`'s `view_radius` |
| `simulation-distance` | the tick/mob-simulation area radius — **clamped**, see "A real, measured limit" below |
| `server-port` / `server-ip` | the TCP address `publish_with_config` binds |
| `white-list` | `AccessHandle::set_whitelist_enabled` |
| `enable-rcon` / `rcon.port` / `rcon.password` | gates `IntegratedServer::start_rcon` |

### Accepted, partially implemented

- **`motd`** — parsed and preserved, but **the live status-ping reply is
  hardcoded** to `crate::server::STATUS_MOTD` ("A Lodestone Server") deep
  inside `serve_connection_inner`, one constant shared by every
  `serve_connection_with_*` wrapper (~50 call sites across `src/` and
  `tests/`). Threading a real value through was out of scope for this pass —
  wiring it needs a parameter added to that whole family of functions, which
  is a real, contained follow-up, not a design change.
- **`level-type=minecraft:flat`** — needs `generator-settings`' JSON, which
  this properties reader does not parse; falls back to a normal overworld
  with a logged warning rather than silently building the wrong terrain.
- **`enable-query` / `query.port`** — parsed and preserved; `publish_with_config`
  does not start a GameSpy4/UT3 query listener (unlike `open_to_lan`, which
  does). A real follow-up, not a design decision: the machinery
  (`crate::query::QueryServer`) already exists and is already proven in
  `open_to_lan`'s own accept loop.

### Accepted and ignored — parsed, preserved on save, no consumer

`accepts-transfers`, `allow-flight`, `broadcast-console-to-ops`,
`broadcast-rcon-to-ops`, `bug-report-link`, `chat-spam-threshold-seconds`,
`command-spam-threshold-seconds`, `enable-code-of-conduct`,
`enable-jmx-monitoring`, `enable-status`, `enforce-secure-profile`,
`enforce-whitelist`, `entity-broadcast-range-percentage`, `force-gamemode`,
`function-permission-level`, `generate-structures`, `generator-settings`,
`hardcore`, `hide-online-players`, `initial-disabled-packs`,
`initial-enabled-packs`, `log-ips`, `management-server-*` (six keys — no
management server exists in this codebase), `max-chained-neighbor-updates`,
`max-tick-time`, `max-world-size`, `network-compression-threshold`,
`op-permission-level`, `pause-when-empty-seconds`, `player-idle-timeout`,
`prevent-proxy-connections`, `rate-limit`, `region-file-compression`,
`require-resource-pack`, `resource-pack*` (four keys), `spawn-protection`
(parsed; **no spawn-protection enforcement exists in this crate at all** —
a fresh world's spawn is not protected from any player), `status-heartbeat-interval`,
`sync-chunk-writes`, `text-filtering-*` (two keys), `use-native-transport`.

### A real, measured limit: `simulation-distance`

Vanilla's own default (`simulation-distance=10`, a 21×21 = 441-column tick
area) was tried first, unclamped, against this crate's debug build, and the
tick loop fell behind without recovering — `"Can't keep up!"` at 143 ticks
behind within 10 seconds of boot, climbing to 1591 ticks (79.5 s) before the
process was killed. `crate::integrated`'s own `LAN_TICK_RADIUS` constant
already documents exactly this cost ("widening it costs a full generator run
per chunk per tick") and keeps LAN hosting at radius 2 (25 columns) for that
reason — pending issue #289's loaded-chunk ticket-driven set, which this
crate's tick-loop architecture does not have yet. `MAX_SIM_RADIUS` in
`crates/lodestone-dedicated-server/src/main.rs` mirrors that established,
load-tested number rather than trusting vanilla's default against an
architecture not yet built for it: `simulation-distance` is honoured only
**downward** from that cap.

## Online-mode

**The server side already existed and already worked before this change** —
`OnlineModeConfig`/`serve_connection_with_online_mode` implement the real
RSA/AES-128-CFB8 handshake and the `hasJoinedServer`-equivalent session-server
check (`lodestone_auth::has_joined`), landed for issue #273. What it lacked
was a reachable caller for a dedicated server: `IntegratedServer::open_in_memory*`
never read it at all (singleplayer cannot authenticate by construction, and
still cannot), and `publish` — the call a persistent, hosted world needs —
took no `online_mode` parameter whatsoever. `PublishConfig::online_mode` is
that reachable caller. `online-mode=true` in `server.properties` now installs
a real `OnlineModeConfig` (after `lodestone_auth::install_crypto_provider`,
the same one-time step `lodestone-auth`'s own login path requires before its
first `reqwest` TLS call) into `publish_with_config`.

## `eula.txt`

Mirrors vanilla's own `Eula.java` mechanically: a single `eula=<bool>` key,
`true`/`false` case-insensitively, default `false` (including when the file
or the key is missing). Absent or `false` refuses to start (writing the
template first if the file did not exist) and exits with status `1`; `true`
proceeds.

**The wording was an open question; it is decided now.**
`crate::eula::NOTICE` points the operator at Mojang's own EULA
(`https://aka.ms/MinecraftEULA`) as the terms governing *operating* a
Minecraft-compatible server — reasoning: Mojang's EULA governs Mojang's
software and Lodestone is a from-scratch reimplementation, not obviously
bound by an agreement written for their binary (this repository's own
`docs/legal-notices.md` and non-affiliation disclaimer exist for exactly
that reason), but an operator running this dedicated server is still
joining Mojang's ecosystem, over which Mojang does assert their EULA and
commercial-usage guidelines regardless of whose server binary is involved.
The notice text says so plainly — pointing at the EULA without claiming
Lodestone is Mojang's software or is affiliated with Mojang — and separately
notes that Lodestone itself is provided under its own open-source license.
See `crate::eula`'s own module doc for the full reasoning.

## A real, measured persistence gap

A freshly generated chunk column — one nobody has ever placed or broken a
block in — is **not** flushed to disk by `IntegratedServer::shutdown()`.
Measured: a fresh world, 25 columns generated for the mob-seed area, `stop`
issued immediately after boot — `world saved on shutdown: 0 chunk columns`
(the crate's own `tracing::debug!` line), and `world/dimensions/minecraft/overworld/region/`
exists but is empty. Entities and POI *do* save correctly on the same
shutdown (`entities saved on shutdown: 12 of 12` in the same run). This is
existing `crate::region_source`/`ChunkStore` dirty-tracking behaviour, not
something this binary introduced — terrain generation is deterministic from
the seed, so a restart regenerates identical shape, but it means **any
generation-time side effect that is not itself re-derived deterministically
(rolled structure loot, for one) is lost on a restart with no player having
ever visited**. Worth a follow-up in `crate::region_source`/`crate::chunk_store`,
not something this doc's own binary can fix from the outside.

## How to change it

- **A new `server.properties` key**: add a field to `ServerProperties`
  (`crates/lodestone-server/src/properties.rs`), read it in
  `ServerProperties::from_raw`, and add its default (copied from the
  decompile, not memory) to `DEFAULTS`. Say in this doc which bucket
  (implemented / accepted-and-partial / accepted-and-ignored) it lands in —
  the module itself only parses, it does not grade its own keys.
- **Wiring a currently-ignored key**: find its bucket above and follow the
  pointer; most of them (`enable-query`, `spawn-protection`) name the exact
  gap and the exact existing machinery (or lack of it) to close it with.
- **The EULA wording**: a `crate::eula::NOTICE`-only edit if it ever needs to
  change again — see "eula.txt" above for the reasoning behind the current
  text.
- **Loosening `MAX_SIM_RADIUS`**: only after `crate::tick`'s loaded-chunk
  ticket-driven set (issue #289) replaces the fixed-radius tick area this
  crate still has everywhere (`LAN_TICK_RADIUS` included) — raising the
  constant without that landing reproduces the measured overload above.

## Configuration

- `server.properties` and `eula.txt` in the server's root directory (the
  binary's `argv[1]`, default `.`).
- `ops.json`/`whitelist.json`/`banned-players.json`/`banned-ips.json`, also
  at the server root — vanilla's own layout.
- `RUST_LOG` (via `tracing_subscriber::EnvFilter`), default `info`.

## Dependencies

`lodestone-server`, `lodestone-registry` (feature `v770`), `lodestone-auth`, `reqwest`, `tokio`
(`rt-multi-thread`, `net`, `signal`, `io-std`, `time`, `sync`, `macros`),
`tracing`, `tracing-subscriber`. No client, renderer, GPU, or windowing
crate — see "How it works" for the measured `cargo tree`.
