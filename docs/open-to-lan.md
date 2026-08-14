# Open to LAN

## What it is

`IntegratedServer::open_to_lan` — the TCP-listening entry point, plus the config surface
for the four subsystems that were implemented, gated, and then unreachable because
`IntegratedServer::bind` took no way to say anything about them: RCON (#331), the
GameSpy4/UT3 query listener (#332), resource-pack pushes (#334), plugin channels (#335) and
commands (#48). Adds vanilla's LAN-discovery broadcast so the world shows up in a client's
multiplayer list without anyone typing an address.

Scopes 1, 2 and 3 of issue #535 are all closed in the sense that `open_to_lan` itself is real,
tested and reachable — the pause menu's **Open to LAN** button (vanilla's
`menu.multiplayerOptions.button`, whose `en_us` value really is "Open to LAN") is a production
caller of `open_to_lan`.

**But that caller does not turn every field on.** `net.rs`'s `open_lan_world` builds its
`LanConfig` as `LanConfig { view_radius, discovery: Some(..), ..LanConfig::default() }` — so
`rcon` (`None`) and, until the fix this paragraph documents, `query` (`false`) stayed at their
defaults. `start_rcon` and `QueryServer::bind` are both real and correctly wired *when a config
asks for them*; the code example below is what a caller with the fields turned on looks like,
not a description of what the shipped pause-menu button passes. Issues #331 and #332 were
reopened for exactly this — "implemented, gated, and then unreachable" — and #331 in particular
is **still** open: RCON additionally needs a password, and there is no UI anywhere in this flow
that collects one (see "Known gaps" below), so closing it needs a real settings control, not a
config-literal flip. `query` needs only the latter — flip `net.rs`'s `LanConfig` literal to
`query: true` — and nothing else in this crate changes; whether that one-line fix has landed is
this doc drifting again if the field table above still says "off" and nobody has updated it.

## How it works

```rust
let server = IntegratedServer::open_to_lan(
    "0.0.0.0:25565",
    protocol,
    source,
    LanConfig {
        view_radius,
        query: true,
        discovery: Some(LanDiscovery { motd: world_name }),
        commands: dispatch,
        ..LanConfig::default()
    },
).await?;
```

`bind` is now a thin wrapper over it with `LanConfig { view_radius, query: true, ..default }`,
which is `bind`'s pre-#535 behaviour verbatim — no existing caller changes.

### What each field reaches

| field | reaches | was |
|---|---|---|
| `rcon` | `start_rcon` right after the handle is built | only ever called by `tests/rcon.rs`; **still true of `net.rs`'s real caller** — needs a password UI, see "Known gaps" |
| `query` | the UDP listener on the game port's UDP space | always on, unconditionally in `bind`; `net.rs`'s `open_to_lan` caller left it off until the fix noted above |
| `discovery` | `spawn_lan_discovery` | did not exist |
| `commands` | every accepted connection's `/`-commands | hardcoded `CommandDispatch::none()` |
| `resource_packs` | `ResourcePackPushFeed` per connection | hardcoded `::default()` |
| `plugin_channels` | `PluginChannelRegistry` per connection | hardcoded `::default()` |
| `online_mode` | picks `serve_connection_with_online_mode` vs the plain wrapper, per accepted socket | did not exist — every join was offline, unconditionally |

The `resource_packs`/`plugin_channels`/`commands` trio arrive because the accept loop calls
`serve_connection_with_mob_events_and_commands_shared` (which grew the resource-pack and
plugin-channel parameters and lost its `#[allow(dead_code)]`) instead of
`serve_connection_with_mob_events_shared`, which hardcodes all three.

### Online mode is a per-connection branch, not a parameter on that wrapper (issue #273)

`serve_connection_with_mob_events_and_commands_shared`'s own doc comment explains why it never
grew an `online_mode` parameter: its signature is depended on from outside this crate's ownership
split (this file's caller, plus `crates/protocol/v770/tests/*`), so a new capability here always
gets a **sibling** entry point instead — `serve_connection_with_online_mode`, the
`_and_commands_shared`-shaped function that additionally takes `&OnlineModeConfig`. The accept
loop's `async move` block picks between the two per accepted socket:

```rust
let _ = match &online_mode {
    Some(online_mode) => serve_connection_with_online_mode(/* .., */ online_mode).await,
    None => serve_connection_with_mob_events_and_commands_shared(/* .. */).await,
};
```

`online_mode` itself is cloned out of `LanConfig` before the accept loop's `async move` (same
`.clone()`-before-the-block trap this file's own "How to change it" section already names) and
cloned again per accepted socket, exactly like `access` just above it — cheap either way:
`None` costs nothing, and `Some` shares one `reqwest::Client` connection pool across every player,
per `OnlineModeConfig::http`'s own doc comment. See
[`docs/server-online-mode.md`](./server-online-mode.md) for the handshake itself, and
`crates/lodestone-server/tests/open_to_lan_online_mode.rs` for a real-TCP-loopback proof of both
branches — `default_lan_config_stays_offline_no_network_call` (the discriminating gate: a
default-built `LanConfig` must never send an `EncryptionRequest`) and
`lan_config_online_mode_demands_encryption_and_substitutes_identity` (a real RSA/AES-128-CFB8
round trip through the real listener, ending with the session server's identity — not the
client's claimed one — on the wire).

Singleplayer never reaches this branch at all: `open_in_memory_with_mobs`/`open_persistent_with_mobs`
call `serve_connection_with_mob_events_and_commands_shared` directly, with no `LanConfig` and no
`online_mode` field anywhere in the call, so there is no way to make a singleplayer world
authenticate short of editing those constructors themselves.

### Failure policy is deliberately not uniform

* **TCP bind** — fatal. There is no server without it.
* **RCON bind** — fatal, propagated with `?`. A host that asked for a remote console and
  silently did not get one has a security-relevant surprise.
* **Query and discovery binds** — logged and skipped. Neither is needed to play, and taking a
  whole world down for a busy UDP port is the wrong trade.

### LAN discovery

Vanilla's `LanServerPinger` is the entire protocol: one UDP datagram reading
`[MOTD]<name>[/MOTD][AD]<port>[/AD]` to `224.0.2.60:4445`, every 1.5 s, with no handshake and
no reply. `LanDiscovery::payload` builds exactly that string and
`lan_discovery_payload_is_vanillas_literal_format` pins it — an off-by-one in the markers is a
world that never appears in the list, with no error anywhere.

Ping failures log at `debug`, not `warn`: a machine with no route to the multicast group would
otherwise warn every 1.5 s for the life of the session.

### The per-connection feed fix that came with it

`BlockTickFeed`'s doc comment named a one-line LAN gap and said its owner was elsewhere. It is
fixed here: the accept loop builds each `LanSubscriber` with `hub_block_ticks.subscriber()`
rather than `BlockTickFeed::default()`. That keeps the **outbound** queue per-connection (the
relay's drain-all depends on it) while **sharing** the inbound one, so a LAN player placing a
repeater, comparator, observer or redstone torch actually reaches the tick loop that hosts its
scheduled recheck. `default()` shared neither and dropped every such placement silently.

The relay also forwards the effect lane now (with its `except` tag intact), which it did not
before — so a LAN player heard no server-caused sounds at all.

## How to change it

* **Add a config field, not a `bind` parameter.** `bind`'s signature is load-bearing for the
  existing tests and for the "same loop as singleplayer" story.
* A new per-connection surface has to reach the accept loop's `async move`, so clone it out
  *before* the block — the file already has a comment block about exactly this trap, and a
  `.clone()` written inside moves the original in.
* A new background task needs a field, an abort in `Drop`, and a join-or-abort decision in
  `shutdown`. Join it only if it races the `shutdown` notify and returns promptly (like the
  query listener); abort an infinite timer loop, or `shutdown` hangs.

## The shell caller

`PauseButton::OpenToLan` → `MenuAction::OpenToLan` → `WindowApp::open_current_world_to_lan`
(`app/session.rs`) → `NetClient::publish_to_lan` → the net thread's own loop, which calls
`IntegratedServer::publish` on the handle it already holds.

### A failed publish must not disconnect the session

`IntegratedServer::publish` returning `Err` — already published, or a bind
failure — used to be reported through `NetUpdate::Error`, and `Sim::poll_net`'s
arm for that variant unconditionally ends the session
(`SessionPhase::Ended(SessionEnd::failed(..))`), the same terminal state a
real server kick reaches. Nothing about the net thread's own loop actually
left — `publish_rx`'s `while let Ok(port) = publish_rx.try_recv()` stays
inside `run_async`'s outer `loop {}`, and the connection this update rides on
is exactly as alive after it as before — so a second press of Open to LAN read
as a client-side session failure and tore down a perfectly healthy connection.
`NetUpdate::LanPublishError` now carries the same message through the ordinary
local-chat path (`Sim::push_local_chat`) instead — the same mechanism
`NetUpdate::LanOpened`'s success case already uses for "Local game hosted on
port N" — and never touches `SessionPhase`.

### It publishes the live handle in place — issue #562

This used to reopen the world: `open_current_world_to_lan` called `Sim::end_session` and then
`NetClient::open_to_lan`, which **constructs** a fresh server — a new `ChunkStore`, a new tick
loop, the local player rejoining over `127.0.0.1` like a stranger. The button that vanilla
treats as invisible-if-nothing-happens produced a real loading screen every time.

`IntegratedServer::publish(&mut self, addr, discovery_motd)` fixes this by adding a *second*
accept loop to a server that is already running, mirroring vanilla's
`Minecraft.getSingleplayerServer().publishServer`. It works because
`open_in_memory_with_mobs`/`open_persistent_with_mobs` (the two constructors with a shared tick
loop and `PlayerRegistry`) now also build a `HostCore`: type-erased handles
(`Arc<Box<dyn ServerProtocol>>`, a double-`Arc`'d `Arc<dyn ChunkSource>`, the block-entity
registry, the live-mob source, the player registry, the tick loop's outbound hub, and the relay's
live subscriber list) stashed on the `IntegratedServer` itself. `publish` binds a
`TcpListener`, reads `host: &HostCore` back off `self`, and spawns an accept loop that clones
those handles into every accepted connection — the same composition `open_to_lan`'s own accept
loop already uses, just reached from `&mut self` instead of from a constructor's local variables.

**The relay unification that made this possible.** Before this, singleplayer's one in-memory
connection read the tick loop's `BlockTickFeed`/`ExplosionFeed` **directly** — safe with exactly
one consumer, and exactly what a second (published) connection could not share without racing it
for the same drain-all queues. `open_in_memory_with_mobs_using` now spawns the same
hub-plus-relay-plus-subscriber shape `open_to_lan` already used for LAN: the tick loop publishes
into a hub, a relay task drains it once per tick period and fans out to a live subscriber list,
and the constructor's own local connection is subscriber #1 — pushed into `HostCore.subscribers`
before the constructor even returns. `publish` pushes every later connection into that **same**
list, so a published joiner receives the identical block-tick/detonation/effect stream the local
player already does. `IntegratedServer` gained two more background-task fields for this
(`relay_task`, `publish_task`) with the usual join-or-abort treatment in `shutdown`/`Drop`.

`Arc<dyn ChunkSource>`/`Arc<Box<dyn ServerProtocol>>` reaching a `serve_connection*` entry point's
generic `S`/`P` bounds needed two small, purely-additive trait impls: `chunk.rs` gained
`impl<S: ChunkSource + ?Sized> ChunkSource for Arc<S>` (hand-forwarded, mirroring
`protocol.rs`'s existing `impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P>`), and
`HostCore::source` is a *double* `Arc` (`Arc<Arc<dyn ChunkSource>>`) because `serve_connection*`'s
`source: &Arc<S>` parameter always wraps its own generic `S` in one more `Arc` — satisfying it
with an erased source needs `S` itself to already be `Arc<dyn ChunkSource>`. One gotcha the new
blanket surfaced: `DimensionalSource` has its own **inherent** `dimension() -> Dimension`
(deliberately shadowing the trait's `Option<Dimension>`, per that method's own doc comment), and
Rust's method resolution stops at the *first* receiver type with any match at all — so calling
`.dimension()` through an `Arc<DimensionalSource<..>>` directly now resolves to the new blanket's
*trait* method instead of continuing to deref to the inherent one. The one call site this bit
(`open_in_memory_with_mobs_using`'s own `TickFollow` construction) now writes `(*source).dimension()`
to force resolution to start one deref past the `Arc`.

### Vanilla parity, and what is deliberately not carried over

Every connection `publish` accepts gets `CommandDispatch::none()`, a default
`ResourcePackPushFeed`/`PluginChannelRegistry`, and a default `AccessHandle` — the same inert
starting point `bind` gives a freshly-built LAN world. A host that wants RCON, real commands or
access control still needs `open_to_lan` at world-open time; `publish` is deliberately the
minimal "add a socket" verb, not a second config surface. `discovery_motd: Option<String>` is
carried through to `spawn_lan_discovery` exactly as `LanConfig::discovery` is, but the shell does
not pass one yet — `open_current_world_to_lan` has no route to the running session's world
directory at the point it calls `publish_to_lan`, so a joining player still needs to be told the
address rather than finding the world in their own multiplayer list.

### One transport, no privileged host

The host's own client dials the socket like everybody else. Nothing on this path is a second,
special kind of connection — which is what stops "works in singleplayer, broken over LAN"
from being possible for anything the host does.

### The autosave is shell-driven; the registries come off the source

`open_to_lan` sets `save: None`, so it starts no autosave task and flushes nothing at
shutdown. `net.rs`'s LAN branch therefore wraps the generator in a `RegionChunkSource` itself
(so a saved world's terrain and edits **load**), holds the `WorldSaveHandle`, writes on the
same `AUTOSAVE_INTERVAL` singleplayer uses, and writes once more before dropping the handle.

**Block entities and scheduled ticks now persist too**, and the mechanism is worth knowing
because it is not a new `LanConfig` field. `open_to_lan` is generic over `S: ChunkSource`, so it
could not name `RegionChunkSource::block_entities` — it built a `BlockEntityHandle::default()`
and a `ScheduledTickHandle::default()`, ticked them faithfully, and lost every chest a guest
filled, because `WorldSaveHandle::extras_for` reads the *source's* registry. The fix is a
defaulted trait accessor, `ChunkSource::world_registries -> Option<WorldRegistries>`:
`RegionChunkSource` is the one implementor that answers `Some`, `ChunkStore` **forwards** it
(the constructor wraps before asking, so a non-forwarding cache would silently restore the
bug), and every in-memory source keeps the honest `None` and its private pair.

No shell change was needed and the config surface did not grow — a LAN host that was already
handed a persistent source now shares its registries by construction. The gotcha for a new
cache or filter layer in front of a source: **forward `world_registries`**, or the world starts
losing containers again with nothing failing.
`integrated.rs`'s `a_persistent_source_hands_its_registries_through_the_chunk_store_wrap`
asserts the join, since playing cannot reveal it.

### Access control is a `LanConfig` field

`LanConfig::access` carries the host's ops/whitelist/ban lists (issue #336,
[`server-access-control.md`](./server-access-control.md)). The accept loop shares one
`AccessHandle` across every connection and hands each one its own `peer_addr().ip()` for the IP ban
list, so an op granted on one connection is an op on the next and a banned address is refused before
it reaches Configuration. The `Default` is empty — admits everybody, ops nobody — which is exactly
what `bind` has always done.

### Teardown uses `drop`, not `shutdown().await`

`shutdown().await` joins the accept loop, which parks in `accept()` where the shutdown notify
cannot reach it; it hung indefinitely for a `view_radius: 0` handle while #535's own gate was
written. `net.rs`'s teardown drops the handle (which aborts both loops) and then runs the
explicit flush described above.

## Known gaps

* **No port field, no game mode, no allow-commands toggle, and (issue #331) no password field
  for RCON.** Vanilla's `MultiplayerOptionsScreen` is a form; this publishes straight away on an
  OS-assigned port (`publish_to_lan(0)`, issue #559 — matching vanilla's own
  `HttpUtil.getAvailablePort()` default), with no dialog of any kind in between the button press
  and the publish. The port actually bound is read back from the listener and reported through
  `NetUpdate::LanOpened`, never the `0` that was requested — `IntegratedServer::publish`'s own
  doc comment and its `publish_reports_the_actual_bound_port_not_the_requested_zero` gate are the
  discriminating check (asserting only "some port came back" would pass for a hardcoded echo of
  the request). `net::LAN_DEFAULT_PORT` (25565, vanilla's well-known port) still exists, kept for
  an explicit-port control to default a text field to — vanilla's `/publish <port>`, not yet
  wired into this crate's command set — rather than for anything the automatic publish path binds
  today. The RCON gap is the same shape: `LanConfig::rcon` is real and correctly wired
  (`start_rcon`, tested against a real vanilla RCON server in
  `crates/lodestone-server/tests/rcon_live_oracle.rs`), but `RconConfig` requires a non-empty
  password by design (mirrors vanilla refusing to enable RCON with an empty `rcon.password`), so
  there is no config-literal fix the way `query`'s was — someone has to add the settings screen
  before this field can ever be `Some`. `IntegratedServer::publish` does not expose an RCON
  option at all yet; a host who wants one still needs `open_to_lan` at world-open time.
* **`online_mode` is the same shape as the RCON gap: real and correctly wired, but nothing
  turns it on yet.** `net.rs`'s `open_lan_world`/`publish_to_lan` callers build their `LanConfig`
  with `..LanConfig::default()`, which leaves `online_mode: None` — there is no settings control
  anywhere in the pause menu or world-creation flow that would ever construct
  `Some(OnlineModeConfig::new(..))`. Closing this needs a real toggle (plus, per
  [`docs/server-online-mode.md`](./server-online-mode.md), a `reqwest::Client` the shell already
  owns one of for account sign-in) threaded into whichever caller builds the `LanConfig` — not a
  change to this crate.
* **Fixed**: the button used to be present unconditionally, including after the world was
  already published — a second press produced `IntegratedServer::publish`'s
  `AlreadyExists` error, which used to reach the player as a full disconnect
  (see "The shell caller" below) and, even once that was fixed, left an
  action with nothing left to do still on screen. `MenuNav::pause_buttons`
  now omits the row once `Sim::is_lan_published()` is true. The "no world of
  ours to publish at all" case (a multiplayer session, or the button pressed
  before a hosted session exists) is still stated in chat rather than by
  hiding the row — that case is about *what kind of session this is*, not
  about publish state, and `PauseButton::enabled` stays a pure function of
  the variant.
* **The LAN world has no mob population.** `open_to_lan` seeds no `MobSim` (its own comment
  says so); the tick loop ticks whatever is there, and nothing puts anything there.
* **The query listener's reported player count comes from the shared `PlayerRegistry`**, so an
  in-memory singleplayer world served alongside is not counted.

## Configuration

`LanConfig` is the whole surface. `LanConfig::default()` is query-on, everything else off
(including `online_mode: None`, i.e. offline), `view_radius: 0`.

## Dependencies

* `crate::rcon` (`RconConfig`, `spawn_listener`), `crate::query` (`QueryConfig`,
  `QueryServer`), `crate::command::CommandDispatch`,
  `crate::plugin_channels::PluginChannelRegistry`, `crate::server::ResourcePackPushFeed`,
  `crate::server::{OnlineModeConfig, serve_connection_with_online_mode}` (issue #273).
* `tokio::net::{TcpListener, UdpSocket}`; native targets only (the whole module is
  `#[cfg(not(target_arch = "wasm32"))]`).
