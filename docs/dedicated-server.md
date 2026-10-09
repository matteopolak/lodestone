# The integrated and dedicated server

## What it is

`IntegratedServer` (`crates/lodestone-server/src/integrated.rs`) is the one server implementation. Singleplayer, open-to-LAN and the standalone `lodestone-dedicated-server` binary run the same code (login, tick loop, chunk streaming, commands) over different transports.

## How it works

| deployment | transport | entry point |
|---|---|---|
| singleplayer | in-memory `tokio::io::DuplexStream` | `lodestone-shell`'s `net.rs`, on the net thread |
| open-to-LAN | real TCP, same process | pause-menu button, `IntegratedServer::publish`, adding an accept loop to the running world |
| dedicated | real TCP, headless | `lodestone-dedicated-server`, no client/render/GPU crate in its graph |

`lodestone-registry` is the only crate that may name a version crate; callers use `server_protocol_for_protocol(protocol)`, so a version-free build reports "no servable family" rather than refusing every join. Joinable and hostable are different sets.

### Tick loop

A real 20 Hz clock independent of client traffic, spawned once per world (per connection would multiply world speed). Backlogs under 2 s are absorbed; beyond that the loop drops the backlog, logs a rate-limited (15 s) warning and jumps its clock forward. MSPT uses a 100-sample window; TPS is `1000 / mspt_avg.max(50.0)`, never above 20. Keep-alive, time-sync, vitals and container-sync timers stay per-connection. Rule: anything two connections must agree on, or that must advance with nobody connected, is simulation and belongs to the world tick; anything derived from authoritative state plus one connection's cursor is replication and belongs to the connection task.

### Server-side ECS

`lodestone-server` links `bevy_app`/`bevy_ecs` directly, not `lodestone-ecs` (which would pull the client vocabulary and `lodestone-physics`/`-game`/`-world` into the server and browser bundle). Schedule labels in `crates/lodestone-server/src/ecs/schedules.rs` reuse none of `lodestone-ecs`'s names, and `bevy_ecs` has no `multi_threaded` feature, so `World::run_schedule` is a synchronous call on the tick task.

- The server `World` is separate from the client's (clock policies differ; singleplayer already carries real protocol bytes; a shared world would expose state a remote server never would).
- The tick task owns the `World` outright, so no lock; connection tasks only enqueue proposals and read published snapshots.
- `GameTick` runs after the scheduled-and-physics timing sample and before `TickClock::record_tick`, so it counts in total MSPT; a completed `TickStats::tick_count` is a barrier for the matching `ServerTickWitness`. Independently managed dimension loops tick generically and do not share this `World`.
- Applying packets inside a scheduled system gives plugins an adjudication window for vetoes. `ActionVetoes`, `PermissionStore` and `CommandRegistry` exist only in `lodestone-ecs` ([plugin framework](roadmap/plugin-framework.md)).

### Login

Modern protocols keep the Configuration phase and wait for both acknowledgements; legacy protocols return `false` from `ServerProtocol::has_configuration_phase` and enter Play right after `login_success`. Both share the Play join sequence. Ordering is load-bearing: `V770ServerProtocol::login_success` sends `LOGIN_COMPRESSION` (threshold 256) uncompressed, flips the codec to compressed framing, and only then sends `LOGIN_FINISHED`.

Online mode negotiates encryption first: a fresh RSA keypair and verify token per connection (implementors are stateless), decrypt token and shared secret, enable AES-128-CFB8 before anything else, check the server-id hash via `lodestone_auth::has_joined`, and use the verified profile's `id`/`name`, not the client's claim. Rejection and an unreachable session server are distinct disconnect errors. Singleplayer never reads online-mode config; LAN/dedicated hosting needs a `reqwest::Client` passed to `OnlineModeConfig::new`. Offline mode derives the UUID from the username, so two connections with one username share one player file.

### Game modes, status, disconnect, liveness

- A game-mode change is two packets: a game event (the mode) and an abilities packet (flight lives only there). Per-mode effects are gated at each call site. Every connection joins survival; the join mode is neither persisted nor configurable.
- Status ping returns JSON and echoes the client timestamp; the player count is always `0` (status lands on its own connection and no cross-connection counter exists). A disconnect reason is a JSON string during Login and an NBT chat component during Configuration/Play.
- Resource-pack and plugin-channel traffic drain on the same 50 ms timer; an unregistered `custom_payload` channel is dropped. Broadcast uses a bounded cursor queue, so a slow connection loses overflow instead of stalling others.
- Served sessions add keep-alive (15 s; an unanswered one disconnects), time-of-day broadcast (at join, then each second) and view streaming (`ViewTracker` recenters a square window on column change; a live render-distance change resizes against a separate ceiling `max_radius`: generous for singleplayer, the configured radius for LAN). Timer-driven behaviour does not run on `wasm32`.

### Open-to-LAN and the dedicated binary

`LanConfig` is the whole publish surface: view radius, RCON, query listener, LAN discovery (one-way UDP broadcast), online mode, commands, resource packs and plugin channels, each defaulting off. Each LAN connection gets its own block-tick/explosion/effect feed pair fed by a relay draining the tick loop's hub (an append-and-drain feed supports one consumer).

The binary is its own crate so a TCP-binding `[[bin]]` never needs `cfg`-gating out of `lodestone-server`, which also builds for `wasm32`. It reads vanilla `server.properties` keys (71 round-trip whether or not hosting consumes them; no `pvp` key; `online-mode` defaults `true`). Gotchas: the status MOTD is hardcoded; `level-type=minecraft:flat` falls back to a normal overworld with a warning; the query listener is not started on this path; `simulation-distance` (default 10, clamped 1..=32) is the radius each player's follow area ticks at via `WorldStateHandle::set_simulation_distance`, and only resident columns tick, so a smaller client view shrinks it (the join packet still echoes view distance in its simulation-distance field); a never-visited generated column is not flushed on shutdown, so rolled structure loot is lost.

### Maintenance commands

- `lodestone-server native-compact --native-path <store>` compacts a native `world.ls` segment (every server on that store stopped; it cannot lock other processes; refuses a missing segment).
- `lodestone-server anvil-convert export-players|export-entities|export-metadata` exports a native store into an Anvil world ([chunk export](anvil-native-chunk-export.md), [entity import](anvil-native-entity-import.md)); each validates a full native snapshot, stages on the same filesystem, publishes with one rename and refuses to merge; absent fields get new-player or empty defaults. `export-entities` takes `--dimension <id>`; `export-metadata` takes `--template <anvil-world> --world-name --timestamp` (template must match the data version, supplies the dimensions tree, `level.dat` written last; a nonzero native day clock is refused because 26.2 keeps it in a per-dimension clock file).

## How to change it

- New `server.properties` key: add a `ServerProperties` field, read it in `ServerProperties::from_raw`, default from reference data, not memory.
- New per-connection timer or cache that another connection must agree with: make it simulation state in the tick loop.
- Native server plugin in an embedder: add via `ServerApp::bootstrap_with` (after `ServerCorePlugin`, before finalization and `ServerBoot`) and pass the application to `IntegratedServer::open_in_memory_with_mobs_and_server_app` or `open_persistent_with_mobs_and_commands_and_server_app`. Never build a second `App` (only the extracted primary `World` ticks) and never install the client's `CorePlugin`. In the standalone binary add it in `dedicated_server_app`, keeping `open_persistent_server` the single path into persistent construction; there is no runtime plugin loader.
- New `ServerProtocol` seam method: default to `ServerDirective::None` and forward through the `Box<dyn ServerProtocol>` blanket impl (a missing forward silently answers the default).
- Ticking columns beyond a player's streamed view needs a loader that makes them resident without sending them; today the ticked set is the follow square intersected with what the views hold.
- Windows shutdown uses the OS native notification, not SIGTERM.

## Configuration

- Server root (dedicated only): `server.properties`, `eula.txt`, `ops.json`, `whitelist.json`, `banned-players.json`, `banned-ips.json`.
- `LanConfig` for open-to-LAN (offline and minimal by default); `lodestone-server` constants `TICK_PERIOD`, the 100-sample MSPT window, `KEEP_ALIVE_INTERVAL` (15 s), `TIME_SYNC_INTERVAL` (1 s); `RUST_LOG` (default `info`).
- Experimental Java adapter (`cargo run -p lodestone-dedicated-server --features jvm`; default builds reject Java configuration): `LODESTONE_JAVA_ADAPTER_CLASS` and `LODESTONE_JAVA_CLASSPATH` (both required), `LODESTONE_JAVA_DEADLINE_MS` (default `5000`), `LODESTONE_PAPER_JAR` plus `LODESTONE_PAPER_PLUGIN_DIRECTORY` (both set to validate and load, not initialise, a Paper bootstrap and plugin entry classes), optional `LODESTONE_PAPER_SHIM_PATH` (must contain `lodestone.bridge.IsolatedPaperShim`). No adapter means no JVM or timer. Each plugin entry gets a fresh shim-first loader; a bootstrap Load failure saves and stops the server, a plugin Load failure disables that entry; entries stay blocked from construction until a server-owned Bukkit facade exists. Callbacks see the newest completed tick, and `IntegratedServer::resident_block_state_id` never generates terrain ([Java plugin bridge](java-plugin-bridge.md)).

## Dependencies

`lodestone-server`, `lodestone-registry` (feature `v26-3`), `bevy_app`/`bevy_ecs` (not `lodestone-ecs`), `lodestone-auth` + `reqwest` (online mode), `lodestone-net`, and `tokio` (`time`, `net`, `signal`; native only).
