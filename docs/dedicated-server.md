# The integrated and dedicated server

## What it is

`IntegratedServer` (`crates/lodestone-server/src/integrated.rs`) is the one server implementation
Lodestone has, and singleplayer, open-to-LAN, and the standalone `lodestone-dedicated-server`
binary are all the same code path over a different transport: an in-memory duplex for
singleplayer, a real `TcpListener` for LAN and dedicated hosting. Everything downstream of the
socket — login, the tick loop, chunk streaming, commands — is byte-identical code regardless of
which one is running.

## How it works

### One server, three transports

| deployment | transport | who calls it |
|---|---|---|
| singleplayer | `tokio::io::DuplexStream`, same process | `lodestone-shell`'s `net.rs`, on the net thread, with no extra synchronization |
| open-to-LAN | real TCP, same process as the playing host | the pause-menu "Open to LAN" button → `IntegratedServer::publish`, added to an already-running world in place (no world reopen, no rejoin) |
| dedicated server | real TCP, headless | `lodestone-dedicated-server`, a thin binary crate with no client/render/GPU crate anywhere in its dependency graph |

`lodestone-registry` is the only crate allowed to name a version crate; the shell/binary ask it
for `server_protocol_for_protocol(protocol)` rather than naming `V770ServerProtocol` directly, so a
version-free build (`--no-default-features`) still compiles and correctly reports "no servable
family" instead of silently refusing every join. Only `v26-2` (protocol 776, MC 26.2) implements
`ServerProtocol` today — joinable and hostable are different sets.

### The tick loop

A real 20Hz clock, independent of client traffic, driving the mob simulation and every registered
block entity. It is spawned exactly **once per world** — never per-connection, since spawning it
per accepted connection multiplies world speed by connection count (a physics bug that reads as
"mobs too fast," not as a loop-count bug). Overrun handling is ported from vanilla: a 2-second
overload threshold and a 15-second warning re-fire interval, both derived from the 50ms tick
period. Small backlogs are absorbed for free (the wake-up resolves immediately once already late);
only once behind by more than 2 seconds does the loop give up on the remaining backlog, log a
rate-limited warning, and jump its clock forward without replaying it. MSPT/TPS accounting keeps a
100-sample rolling window (matching vanilla's own `tickTimesNanos`) and derives TPS as
`1000 / mspt_avg.max(50.0)`, so it never reports above 20 even when ticks are comfortably fast.

Four other timers (keep-alive, time-sync, vitals, container-sync) stay per-connection rather than
folding into this loop — they read or write one player's own tracked state, not world state, so
unifying them would misname a per-connection concern as a world one. The organizing rule: anything
two connections must agree on, or that must keep advancing with nobody connected, is *simulation*
and belongs to the world tick; anything reconstructible from (authoritative state × one
connection's own cursor) is *replication* and belongs to the connection task.

### Server-side ECS

`lodestone-server` links `bevy_app`/`bevy_ecs` directly, **not** via `lodestone-ecs`: two
`World`s means a shared `ScheduleLabel` type buys nothing at runtime, while linking
`lodestone-ecs` would drag the entire client vocabulary (`LocalPlayer`, `FrameClock`,
`SessionMenus`, …) plus `lodestone-physics`/`-game`/`-world` into this crate's graph — and into
the browser bundle, which links `lodestone-server` and links neither `bevy_ecs` nor
`lodestone-ecs` today. The server's own schedule labels and `SystemSet` anchors
(`crates/lodestone-server/src/ecs/schedules.rs`) intentionally reuse none of `lodestone-ecs`'s
names even where they coincide (`TickSet`, `GameTick`, `NetIngest`), and it installs no
`Extract`/frame-anything: there is no render loop to extract to, since open-to-LAN never draws
a frame. A server-side plugin gets its own adjudication window on the server's own `World`, not
the client `lodestone-ecs` plugin surface reused wholesale — `ActionVetoes`, `PermissionStore`,
`TaskScheduler`, `CommandRegistry` and the rest of that vocabulary live only in `lodestone-ecs`
and are a documented gap on this binary, not something this link brings along for free (see
[`roadmap/plugin-framework.md`](./roadmap/plugin-framework.md)'s Commands/Permissions rows).
`bevy_ecs` is pinned without the `multi_threaded` feature workspace-wide, so `World::run_schedule`
is a plain synchronous call on the tick task — there is no second runtime or thread pool for
tokio to reconcile with, and the tick loop's structure (spawn once, `sleep_until`-driven,
unchanged call site) does not change.

Each primary `IntegratedServer` constructor moves one server `World` into its one world-tick task.
That task runs `GameTick` once after the scheduled-and-physics timing sample and immediately before
`TickClock::record_tick`. The schedule's work is therefore included in total MSPT without being
misattributed to the scheduled-and-physics phase, and a completed `TickStats::tick_count` is a
completion barrier for the matching `ServerTickWitness` observation. Independently managed
dimension loops retain their own generic ticking path; they do not construct or share this primary
server `World`.

Native embedders can build that primary application with `ServerApp::bootstrap_with`, adding their
plugins after `ServerCorePlugin` installs the public schedules and ordering sets but before plugin
finalization and `ServerBoot`. Passing the result to
`IntegratedServer::open_in_memory_with_mobs_and_server_app` moves its `World` into the same primary
tick task, so plugin systems scheduled in `GameTick` are production work rather than a test-only
application. Persistent embedders use
`IntegratedServer::open_persistent_with_mobs_and_commands_and_server_app` for the same handoff; the
shorter constructors preserve their existing behavior by supplying `ServerApp::bootstrap()`.

The standalone binary builds that application in `dedicated_server_app` and passes it through
`open_persistent_server`, the same persistent leaf tested by an embedding host. Compiled-in native
plugins belong in that builder. The default binary installs none, and there is deliberately no
runtime plugin directory, dynamic Rust loader, or `server.properties` selection policy yet.

The server owns its own `World`, entirely separate from the client's: they have contradictory
clock policies (the client forgives lost time past a cap; the server must keep advancing and only
forgives past vanilla's 2-second threshold), singleplayer is already structurally multiplayer (a
real duplex carries real protocol bytes, so a shared `World` would let a client-side plugin read
state that would not exist against a real remote server), and the two `World`s hold differently
keyed, differently versioned representations of the world. The server's `World` needs no lock at
all — it is owned outright by the tick task, and every connection task only enqueues proposals and
reads published snapshots rather than reaching into it directly, which is a strictly simpler
arrangement than the client's shared-`World` lock discipline. This also opens an adjudication
window a plugin can veto from (a protection plugin blocking a break, an economy plugin taxing a
transaction) that an inline connection-task mutation never could — packet application inside a
scheduled system, rather than in-place, is the whole point.

### Login: compression, then encryption, then online-mode verification

The login state machine is capability-gated at the protocol seam. Modern
protocols keep the Configuration phase and wait for both client acknowledgements;
legacy protocols return `false` from `ServerProtocol::has_configuration_phase`
and the shared connection loop enters Play immediately after `login_success`,
without trying to decode packets their wire cannot carry. Both paths reuse the
same Play join sequence, so chunk streaming, saved-player restoration, and
initial time synchronization do not diverge by protocol family.

Ordering here is load-bearing. On a real join, `V770ServerProtocol::login_success` sends
`LOGIN_COMPRESSION` (threshold 256, matching vanilla's default) uncompressed, flips the connection's
codec to compressed framing, and only then sends `LOGIN_FINISHED` compressed — reversing that order
produces a frame the other side cannot parse. When online mode is configured, encryption negotiates
first: the server generates a fresh RSA keypair and a verify token per connection (vanilla caches
one keypair for the process; this crate's protocol implementors are stateless, so there is no shared
keypair to cache), sends an `EncryptionRequest`, and on the matching response RSA-decrypts the
verify-token echo and the shared secret, enables AES-128-CFB8 encryption on the connection
**before** anything else is sent, computes the server-id hash, and checks it against Mojang's
session server (`lodestone_auth::has_joined`). A verified profile's `id`/`name` — not the client's
self-reported ones — is what `login_success` uses; a rejected or unreachable session server
disconnects with vanilla's own `unverified_username`/`authservers_down` reasons, kept as distinct
errors since an outage and a genuine identity mismatch are different problems to see in a log.
Singleplayer never reads online-mode configuration at all and can never authenticate; only
open-to-LAN and dedicated hosting can turn it on, and turning it on needs a real `reqwest::Client`
plumbed to `OnlineModeConfig::new` — nothing constructs one automatically.

**A load-bearing identity gotcha applies regardless of online mode**: offline mode derives a
player's account UUID from their username, not from any UUID the client claims. Two connections
(or two test runs) sharing a username therefore share exactly one persisted player file on disk.

### Game modes, status, and disconnect

A game-mode change is always **two** clientbound packets sent together — a game-event telling the
client what mode it's in, and an abilities packet telling it whether it may fly/instabuild; flight
permission lives only in the second one, so sending one without the other leaves a client that
thinks it's in creative but cannot fly. Per-mode consequences (instant break, no drops, no fall/
border/drowning damage) are each gated at their own call site rather than taught to a shared
"is this mode special" helper. The join mode is not currently persisted or configurable — every
connection joins survival and switches via `/gamemode` or the F3 menu.

A status-ping (server-list) request gets a JSON status document (MOTD, player cap, version,
protocol, optional favicon) plus a pong reply carrying the client's own echoed timestamp; the
server closes after a second status request or after answering a ping, matching vanilla. Player
count is honestly reported as `0` — a status request lands on its own connection with no visibility
into what other connections are serving, and closing that gap needs a shared, cross-connection
counter this crate does not have yet.

A disconnect reason is encoded differently by phase: a **JSON string** during Login (the phase
predates NBT chat components on the wire) and an **NBT** chat component during Configuration/Play.
Two producers exist end-to-end today: a keep-alive timeout, and a refused username (matching
vanilla's name-validity check, but explained rather than a silent socket close) — an offline-mode
identity gotcha above is exactly why an invalid name matters, since a name with control characters
would otherwise reach storage.

### Resource packs, plugin channels, session liveness

A resource pack push (URL, SHA-1, required flag, optional prompt) is published into a feed and
drained on the same 50ms timer that also drains block/explosion/plugin-channel/chat traffic; a
required pack's accept/decline behavior is entirely client-side. Plugin messaging (`custom_payload`)
is a registry of channels the server is interested in plus a per-connection record of what a client
announced support for; an unregistered channel is a silent drop on both sides, matching vanilla's
own `DiscardedPayload` fallback, and a server-to-client broadcast is a bounded, cursor-based queue so
a slow connection loses overflow rather than stalling everyone else.

A served session (singleplayer or LAN) additionally needs three things a static, timeless
connection would not: **keep-alive** (a 15-second challenge; an unanswered previous challenge
disconnects rather than piling up — matching vanilla's actual ~15s worst case, not "interval +
timeout"), a **periodic time-of-day broadcast** (once at join to anchor the clock, then every second
game-time-only, mirroring vanilla's once-a-second resync), and **view streaming** (`ViewTracker`
recenters the streamed chunk window whenever the player's column changes, sending forgets for what
left and batches for what entered — a square window rather than vanilla's exact circular one, kept
deliberately consistent between the join-time and steady-state code paths). A live render-distance
change resizes the window around its existing center against a separate ceiling (`max_radius`) from
where it started (`radius`); the ceiling differs by deployment — a generous cap for singleplayer's
own slider, the host's configured radius for LAN. None of this runs on `wasm32`, where there is no
timer driver to initiate a challenge or a periodic broadcast (inbound-driven behavior, like echoing
a keep-alive or streaming on movement, still works).

### Open-to-LAN and the dedicated server specifically

`IntegratedServer::publish` adds a second accept loop to an **already-running** world rather than
reopening it — the earlier behavior reopened the world from scratch on every LAN publish, which
looked like a fresh loading screen for a button vanilla treats as invisible when nothing changes.
`LanConfig` is the whole surface: view radius, RCON, the query listener, LAN discovery
(a one-way UDP broadcast so the world appears in a nearby client's list unprompted), online mode,
commands, resource packs, and plugin channels — each defaults off, and a caller opts in per field
rather than `bind` growing parameters. Per-connection tick-driven feeds (block ticks, explosions,
effects) need their own fan-out for LAN specifically, since an append-and-drain-all feed can only
have one consumer safely; each LAN connection gets its own feed pair fed by a relay that drains the
tick loop's hub and republishes.

The dedicated-server binary (`lodestone-dedicated-server`) is a separate crate specifically so a
`tokio`-dependent, TCP-binding `[[bin]]` never has to be `cfg`-gated out of `lodestone-server`
itself, which also compiles for `wasm32` as part of the browser bundle. It reads real vanilla
`server.properties` key names and defaults (transcribed from the 26.2 decompile, not memory or an
older version — notably, 26.2 has no `pvp` key, and `online-mode` really does default to `true`);
71 real keys round-trip whether or not this crate's hosting path consumes them. Live gotchas: the
status-ping MOTD is hardcoded regardless of what `server.properties` says; `level-type=minecraft:flat`
falls back to a normal overworld with a warning, since the flat generator's JSON settings aren't
parsed; and the query listener isn't started on this path even though the machinery exists and is
proven on the LAN path. `simulation-distance`/tick-area radius is clamped to a measured-safe
constant well below vanilla's own default, because this crate's tick loop still regenerates its
whole tick area from source every tick (there is no loaded-chunk ticket-driven set yet) — raising
the cap without that landing first reproduces real tick-loop overload.

A freshly generated, never-visited chunk column is **not** flushed to disk on shutdown. Terrain
regenerates identically from the seed on restart, so this is invisible for ordinary play, but any
generation-time side effect that is not itself deterministic (rolled structure loot, for one) is
lost if the world is stopped before a player ever visits.

## How to change it

- **Adding a `server.properties` key**: add a field to `ServerProperties`, read it in
  `ServerProperties::from_raw`, and give it a default copied from the decompile rather than memory.
- **A new per-connection timer or cache**: ask "does any other connection need to agree with this?"
  first. If yes, it belongs in the server `World`/tick loop as simulation state; if no, the
  connection task is where it belongs.
- **Adding an ECS `Resource` a plugin will order against**: it must be genuinely `'static`-owned
  (`Send + Sync + 'static`), the same constraint the client-side plugin doctrine already enforces.
- **Registering a native server plugin in an embedding application**: add it through
  `ServerApp::bootstrap_with`, then pass that exact application to either
  `IntegratedServer::open_in_memory_with_mobs_and_server_app` or
  `IntegratedServer::open_persistent_with_mobs_and_commands_and_server_app`. Do not build a second
  `App` beside the server: only the extracted primary `World` is ticked.
- **Registering a compiled-in plugin in the standalone binary**: add it in
  `dedicated_server_app`. Keep `open_persistent_server` as the single path into persistent world
  construction so tests and production cannot select different applications.
- **Do not install the client's `CorePlugin` on the server's `App`** — it inserts a frame clock and
  a render-driven schedule chain that are both meaningless server-side; the server needs its own
  core plugin.
- **Adding a `ServerProtocol` seam method**: give it a safe default (`ServerDirective::None`) so a
  protocol family without support for the new behavior degrades to "sends nothing" rather than
  failing to compile, and remember to forward it through the `Box<dyn ServerProtocol>` blanket impl
  — a missing forward is not a compile error, it silently answers with the trait default.
- **Loosening the simulation-distance cap**: only after a loaded-chunk ticket-driven tick area
  replaces the fixed-radius one this crate still has everywhere.
- **Windows has no process-to-process SIGTERM equivalent** — the shutdown loop reacts to the OS's
  own native shutdown notification there instead of racing a Unix signal.

## Configuration

Experimental Java adapters require `cargo run -p lodestone-dedicated-server --features jvm` and
both `LODESTONE_JAVA_ADAPTER_CLASS` and `LODESTONE_JAVA_CLASSPATH`. The latter uses the platform's
path-list separator. `LODESTONE_JAVA_DEADLINE_MS` is a positive integer, default `5000`. No configured
adapter means no JVM startup or polling timer. A default build rejects Java configuration explicitly.

An adapter run may also validate and load, without initialization, an operator-supplied Paper
bootstrap and each discovered plugin entry class: set both
`LODESTONE_PAPER_JAR` and `LODESTONE_PAPER_PLUGIN_DIRECTORY`, and optionally
`LODESTONE_PAPER_SHIM_PATH` for one shim directory or jar that precedes the Paper jar in isolated
resolution. Invalid paths, plugin descriptors, or declared main-class archive entries stop cleanly
before JVM startup. Each plugin entry gets a fresh shim-first isolated loader that sees only the shim,
server, and that plugin jar. A lifecycle-load failure also saves and stops the server, so a requested
Paper intake cannot silently degrade into an ordinary adapter run. These loads do not initialize
Paper, retain a plugin loader, instantiate or enable a plugin, or provide Paper-plugin compatibility.
See [Java plugin bridge](java-plugin-bridge.md) for its explicit boundary and live
fixture.

Adapter callbacks observe the newest completed server tick while idle, coalescing intervening ticks;
resident block reads never generate missing terrain. Callback errors without Paper bootstrap input are
logged and disable the adapter. Closed stdin leaves adapter polling and signal handling active.

- `server.properties` and `eula.txt` at the server root (dedicated server only); `ops.json`/
  `whitelist.json`/`banned-players.json`/`banned-ips.json` alongside them, vanilla's own layout.
- `LanConfig` — the open-to-LAN/publish surface (view radius, RCON, query, discovery, online mode,
  commands, resource packs, plugin channels); defaults are the offline, minimal-surface behavior.
- Native server plugin registration is an in-process, compile-time builder choice, not a
  `server.properties` key. Runtime discovery and dynamic native loading are not supported.
- Tick-loop constants (`TICK_PERIOD`, the 100-sample MSPT window, the derived overload/warning
  thresholds) and session-liveness constants (`KEEP_ALIVE_INTERVAL` = 15s, `TIME_SYNC_INTERVAL` = 1s)
  are compile-time constants in `lodestone-server`, not runtime settings.
- `RUST_LOG` via `tracing_subscriber::EnvFilter` (dedicated server), default `info`.

## Dependencies

`lodestone-server` (the shared implementation), `lodestone-registry` (feature-gated `v26-2`, the
only crate allowed to name a version family), `bevy_app`/`bevy_ecs` directly — **not**
`lodestone-ecs`, deliberately (server-side plugin scheduling on the server's own `World`, no
`multi_threaded`) — `lodestone-auth` + `reqwest` (online-mode session-server verification),
`lodestone-net` (the shared connection/codec seam), `tokio` (`time`, `net`, `signal`, native
only). The dedicated-server binary adds nothing render-, GPU-, or window-shaped to that graph.
