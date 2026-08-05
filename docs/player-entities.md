# Player entities

## What it is

The server-side machinery that makes a connected player an **entity other
connections receive** (issue #438): a shared registry of connected players, a
per-connection tab-list diff, and the two `ServerProtocol` encoders that put
both on the wire. Before it, two players on one server — including over LAN —
were completely invisible to one another.

## How it works

### The three absences, and which one turned out not to exist

Issue #438 named three independent blockers. Two were real; the third was not.

| named blocker | outcome |
|---|---|
| no player entity | real — [`PlayerRegistry`](../crates/lodestone-server/src/players.rs) creates one |
| no player registry | real — same file |
| **no broadcast path** | **not needed.** `EntityStreamer`'s per-connection diff is already a *pull*: a player appearing in the registry is picked up by every other connection's next streaming pass. A `broadcast::Sender` would have been a second, redundant mechanism for a diff that already existed. |

### The pieces

* **`PlayerRegistry`** (`crates/lodestone-server/src/players.rs`) — an
  `Arc<Mutex<…>>` handle, the same shape `BlockEntityHandle` and `MobHandle`
  already established. Every connection task clones it.
* **`PlayerTicket`** — RAII ownership of one registration. Dropping it
  deregisters. This is not stylistic: `serve_play` returns through a dozen `?`
  paths (transport error, keep-alive timeout, clean disconnect, invalid packet)
  and a hand-written removal on the success path alone would leak a ghost
  player — visible to everyone, standing still, forever — on every other one.
* **`PlayerRegistry::view(viewer)`** — the roster *and* the entity snapshots
  from **one** lock acquisition. The roster **includes** the viewer (vanilla
  lists you in your own tab list); the entities **exclude** it (vanilla never
  sends a player their own entity).
* **`PlayerListStreamer`** — the roster-shaped twin of `EntityStreamer`,
  diffing uuids so each pass emits one `player_info_update` for the newly
  joined and one `player_info_remove` for the departed.
* **`PlayerAwareSource<E>`** — composes an existing `EntitySource` (in
  production `LiveMobSource`) with a registry.
* **`EntitySource::players()`** — a **defaulted** trait method returning
  `Option<&PlayerRegistry>`. It is the conduit rather than a new parameter
  because `serve_connection` and its five siblings are called directly from
  `crates/protocol/v770/tests/*`; a new argument would have churned every one
  of those call sites for a feature none of them uses. Every pre-existing
  source inherits `None` and keeps its exact previous behaviour.
* **`server::stream_pass`** — one streaming pass: tab-list diff first, then the
  entity diff over mobs *and* other players.

### The ordering constraint that is not optional

**A real client silently discards an `ADD_ENTITY` of type `minecraft:player`
for a uuid it holds no `PlayerInfo` for.** From the jar, not inferred —
`ClientPacketListener.createEntityFromPacket`
(`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/ClientPacketListener.java:591-604`)
logs *"Server attempted to add player prior to sending player info"* and
returns `null`; `handleAddEntity` then logs *"Skipping Entity with id"* and the
entity never enters the level.

So a server streaming a byte-perfect `ADD_ENTITY` and no `player_info_update`
reaches **zero pixels** while every wire in `cargo xtask connectedness` reads
green — this repo's island failure mode exactly. Hence:

1. `stream_pass` emits the player-list directives **before** the entity diff.
2. Both come from one `PlayerRegistry::view` call, because two separate reads
   could interleave a join between them and produce precisely that dropped
   spawn.

### Entity ids come from a second allocator

`MobSim` counts up from `1` (from `1000` in production); `PlayerRegistry`
counts up from `PLAYER_ENTITY_ID_BASE` (`1 << 30`). A collision would make a
mob and a player alias into one entry of
`EntityStreamer::last_sent: HashMap<i32, EntitySnapshot>`, each pass
overwriting the other's position. Vanilla has no such split —
`Entity.ENTITY_COUNTER` is one `AtomicInteger` — so the real fix is one shared
allocator once the server-ECS migration (#433) gives both a common owner.

## How to change it

* **Add a field a player carries on the wire**: extend `TrackedPlayer`, set it
  in `PlayerRegistry`, lower it in `PlayerRegistry::view`. Nothing in
  `crate::server` changes.
* **Player metadata**: run `EntityDataIndexOracle.java` first. Never
  hand-count an index — a player is a `LivingEntity`, index 8 is
  `LivingEntity.DATA_LIVING_ENTITY_FLAGS` *and* `AbstractArrow.ID_FLAGS`, and
  which census column separates the claimants is not guessable from the
  previous collision's guard.
* **The entity-type key is a silent failure mode.**
  `encode_add_entity_body` resolves it with
  `entity_type_id(&entity.entity_type.to_string()).unwrap_or(0)`, so a
  misspelling streams entity type `0` — `minecraft:acacia_boat` — with no error
  anywhere. Any gate must assert the **type id**, not that an entity arrived.
  `minecraft:player` is **156** (Mojang's own `registries.json` for 26.2).

### Gotchas

* **Rotation is not streamed, and cannot be yet.**
  `ServerBound::PlayerMoved` carries `(x, y, z, on_ground)` and no angles:
  `v770`'s decoder discards the rotation from `move_player_pos_rot` and maps
  `move_player_rot`/`move_player_status_only` to `Ignored`. Every other player
  therefore faces yaw 0. Fixing it means growing that variant, which changes
  every one of its match sites — a separate unit.
* **Cadence is packet-driven.** A player's movement reaches another connection
  on that connection's *next inbound packet*, not on a timer. Adequate in
  practice (a real client sends a movement or status packet every tick) and
  identical to how mob positions already propagate.
* **`bind` starves a current-thread tokio runtime.** Its `run_tick_loop` at
  20 TPS prevents a `#[tokio::test]`'s own `tokio::time::timeout` timers from
  firing: login and configuration complete, then every read hangs with the
  timeout never expiring. Use
  `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` for any test
  that drives `bind`.
* **The uuid must be the one `login_success` echoed.** The client resolves a
  player spawn by looking that uuid up in its own `PlayerInfo` map, so a
  second, independently derived uuid produces a spawn every client discards.
  Vanilla in offline mode *derives* the uuid from the username and ignores what
  the client sent; matching that means changing `login_success` too, and the
  two must move together or not at all.
* **Not the rendering half.** Issue #62 (skins for other players) is the client
  side and is a different axis. The tab list *does* already draw
  (`crates/lodestone-game/src/tablist.rs` consumes `player_info_update`), so
  this change is observable on screen today even before #62.

## Configuration

None. No env vars, no feature flags. Player streaming is on for any connection
whose `EntitySource::players()` returns `Some`, which in production means the
`PlayerAwareSource` composition in `IntegratedServer::bind`.

## Dependencies

* `lodestone-server` internals only, plus `lodestone-model` and `uuid`. The
  module is version-free like the rest of the crate: it names no packet id and
  no wire layout.
* The wire half is `ServerProtocol::encode_player_info_add` /
  `encode_player_info_remove`, implemented for protocol 776 in
  `crates/protocol/v770/src/server_protocol.rs`. Both default to emitting
  nothing, so a version crate without tab-list support simply never streams a
  player entity — the honest consequence rather than a half-sent spawn.

## Gates

* `crates/protocol/v770/tests/server_player_entity_stream.rs` — two real
  connections, and every assertion is made on **bytes connection B received**:
  the `ADD_ENTITY` type id against Mojang's own registry dump, the
  player-info-before-spawn ordering as an index comparison, the doppelgänger
  rule by uuid, movement, and departure. Four negative controls are recorded in
  that file's own header with the message each produced.
* `crates/lodestone-server/src/players.rs`'s unit tests cover the registry in
  isolation — deliberately labelled a closed loop in the integration test's
  header, since they would all pass with no player ever reaching a socket.
