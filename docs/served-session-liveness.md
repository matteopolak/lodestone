# Served session liveness: keep-alive, time-of-day, and view streaming

## What it is

The three things that make a served session (singleplayer over
`memory_pair`, or open-to-LAN over TCP) survive, keep time, and follow the
player, instead of being a static, timeless island that a real client would
eventually give up on:

1. **Keep-alive**, in both directions: the server sends a periodic challenge
   and disconnects a client that never echoes it back.
2. **Time-of-day**: the server anchors and periodically re-broadcasts the
   world's clock, so a connected client's sky and daylight actually move.
3. **View streaming**: the server tracks which chunk column the player is
   standing in and sends/forgets columns as that changes, instead of sending
   one fixed square at join and never again.

All three are **version-free** scheduling in `lodestone-server`, driven
through the same [`ServerProtocol`] seam every other packet goes through —
this crate never names a wire id, and a protocol crate that does not
implement the new encoders simply emits nothing (see `Configuration` below).

## How it works

### Where it lives

* `crates/lodestone-server/src/protocol.rs` — two new [`ServerBound`]
  variants (`KeepAlive { id }`, `PlayerMoved { x, y, z, rotation, on_ground }`
  — the latter two fields added after this pass) and four new
  [`ServerProtocol`] methods (`encode_keep_alive`, `encode_set_time`,
  `encode_chunk_cache_center`, `encode_forget_chunk`), each defaulted to emit
  nothing.
* `crates/lodestone-server/src/server.rs` — the scheduling itself:
  `ViewTracker` (chunk-streaming bookkeeping) and `serve_play` (the
  post-join loop that owns the timers). Nothing here names a protocol
  number.
* `crates/protocol/v770/src/server_protocol.rs` — the protocol-776 encoders,
  plus the two new `decode` arms (`KEEP_ALIVE`, `MOVE_PLAYER_POS[_ROT]`) that
  produce the new `ServerBound` variants.

### Keep-alive

`serve_connection` hands off to `serve_play` the moment a connection reaches
`State::Play`. On native targets, `serve_play` runs a `tokio::select!` loop
racing the socket read against two `tokio::time::interval`s. Every 15
seconds (`KEEP_ALIVE_INTERVAL`) it sends a fresh challenge
(`ServerProtocol::encode_keep_alive`) — unless the *previous* one is still
unanswered, in which case it returns `ServerError::KeepAliveTimeout` instead
and the connection ends. A `ServerBound::KeepAlive { id }` that matches the
pending challenge clears it.

Both numbers — the interval and the timeout window — come from the real
26.2 server, not from vanilla-adjacent guessing:
`ServerCommonPacketListenerImpl.java:35-36` defines
`LATENCY_CHECK_INTERVAL` and `CLOSED_LISTENER_TIMEOUT` as the **same**
literal `15000` (milliseconds), and `keepConnectionAlive`
(`:118-133`) sends a new challenge once `now - keepAliveTime >= 15000`,
disconnecting immediately if the old one is still pending at that point.
So an unresponsive client is caught within one more interval of the
challenge going out — up to ~15s later, not the ~30s a naive reading of
"interval + timeout" would suggest.

One thing this port does **not** replicate: vanilla skips keep-alive
entirely for the singleplayer owner (`!this.isSingleplayerOwner()` guards
both the send and the mismatch-disconnect in
`ServerCommonPacketListenerImpl.java:80-133`). This crate does not special-case
that connection, so a served singleplayer session gets real keep-alives too —
deliberate, since the same loop also serves open-to-LAN and a future
multiplayer session, and it costs nothing (a real client always answers
automatically; see `lodestone-client`'s `driver.rs`, `KeepAlivePolicy`).

### Time-of-day

`SET_TIME` in protocol 776 carries a monotonic `game_time` plus a **map** of
per-world-clock updates, not a plain pair of longs (see
`crates/protocol/v770/src/packets/time.rs`'s own doc comment, and
`docs/time-of-day-lighting.md` for the client-side half of this). An empty
map means "nothing changed, keep the day/night anchor you already have" —
that is what vanilla's own once-a-second broadcast sends
(`MinecraftServer::forceGameTimeSynchronization`,
`MinecraftServer.java:1095-1099`: `if (this.tickCount % 20 == 0)`), and it is
why `ServerProtocol::encode_set_time`'s second parameter is
`Option<i64>` rather than a bare tick count.

The server sends two different shapes of `SET_TIME`:

* **Once, at join** (`serve_connection`, right after `begin_play`, before
  chunk streaming starts — mirroring vanilla's `PlayerList.sendLevelInfo`,
  `PlayerList.java:648-651`, which sends `ServerClockManager
  ::createFullSyncPacket()` at exactly that point): `encode_set_time(0,
  Some(0))`, anchoring the day/night clock at tick 0. This crate has no
  persisted world age, so "tick 0" is this session's own join moment.
* **Periodically thereafter** (`serve_play`, every `TIME_SYNC_INTERVAL` = 1
  second, standing in for vanilla's every-20-ticks-at-20-TPS cadence since
  this crate has no fixed server tick loop):
  `encode_set_time(ticks_since(play_start), None)` — the game-time-only
  broadcast that does **not** touch the client's day/night anchor.

`V770ServerProtocol::encode_set_time` hand-encodes the packet directly
(`i64` game_time, then a VarInt-counted list of `{holder_id, total_ticks,
partial_tick, rate}` updates) rather than deriving `Encode` on
`packets::time::SetTime`, because that struct has no `Encode` impl to reuse
— see that file's own doc comment for why it was hand-decoded in the first
place. The one clock ever anchored is the overworld clock, holder id `0`
(`WorldClocks::bootstrap` registers `minecraft:overworld` first,
`minecraft:the_end` second — see `ClockUpdate::holder_id`'s doc comment);
the integrated server always joins into the overworld, so the end clock is
never sent.

### View streaming

`ViewTracker` (in `server.rs`) tracks which `(cx, cz)` columns a connection
has been sent and around which center. Every inbound
`ServerBound::PlayerMoved` (produced only by `move_player_pos` and
`move_player_pos_rot` — the only two serverbound movement packets that carry a
position; the look-only and status-only siblings now lift into
`PlayerRotated`/`PlayerStatusOnly` instead, which deliberately do **not**
recenter the view, since a packet with no position cannot have changed the
chunk column) is converted to a chunk column via
`floor(x / 16)` / `floor(z / 16)` and handed to `ViewTracker::recenter`.

If the column did not change, nothing is sent — mirroring vanilla's own
guard (`ChunkMap::updateChunkTracking`, `ChunkMap.java:1110-1120`, only
recomputes the tracked view when the player's 2D chunk position actually
differs, even though `move()` itself may run on any 3D section change).
Otherwise: a `SET_CHUNK_CACHE_CENTER` update, then a `FORGET_LEVEL_CHUNK`
for every column that left the window, then one chunk batch
(`begin_chunk_batch`/`encode_chunk`/`end_chunk_batch`) for every column
that entered it — the same order vanilla's `applyChunkTrackingView`
(`ChunkMap.java:1122-1132`) uses.

**Deliberate simplification**: the tracked window is the same square
`[-view_radius, view_radius]²` `serve_connection`'s initial join view
already used, not vanilla's rounded `ChunkTrackingView.Positioned`
(a buffered Euclidean-distance test — see `ChunkTrackingView.java`). Keeping
the join-time and move-time shapes identical is what stops a live
connection from immediately forgetting chunks it only just finished sending
at join; matching vanilla's exact circular shape was not otherwise
load-bearing for "the world keeps up as the player walks", and would have
made the initial-view and steady-state code paths disagree.

#### A live render-distance change, and the two radii (#545)

`ServerBound::ClientInformationChanged` carries the client's requested render
distance; `ViewTracker::set_view_radius` resizes the window around the *existing*
centre (never moving it) and produces forgets for what left plus one batch for
what entered.

**`ViewTracker` holds two radii, and they answer two different questions.**

| field | question | set from |
|---|---|---|
| `radius` | where does this connection *start*, and where is it *now*? | `serve_connection`'s `view_radius` |
| `max_radius` | how far may it *go*? | `serve_connection_inner`'s `max_view_radius` |

They used to be one value, and that was the bug the owner reported as *"render
distance doesn't seem to apply to the server until I relog"*. The clamp lived at
the call site and read `clamp(0, view_radius.max(0))` — `view_radius` being *this
connection's own join argument*. So **lowering** took effect immediately and
**raising past the launch value was silently clamped back**. Vanilla clamps
against `serverViewDistance`, a server setting
(`ChunkMap.java:826` — `Mth.clamp(player.requestedViewDistance(), 2, this.serverViewDistance)`),
never against the player's current view. The clamp now lives inside
`set_view_radius` against `max_radius`.

The other half of the same report was client-side (#506: the shell never sent the
packet at all) and is fixed separately in `Session::set_render_distance`. Either
half alone leaves the slider looking broken.

**Who supplies `max_view_radius` is a per-path memory-policy decision** — the
same fork `ChunkStore::for_view_radius` vs `for_integrated_view_radius` already
encodes (see [`chunk-store.md`](./chunk-store.md)'s "two policies" section):

| path | ceiling | why |
|---|---|---|
| singleplayer (`open_in_memory*`) | `MAX_CLIENT_VIEW_RADIUS` (33) | the memory of the person who moved the slider |
| open-to-LAN (`IntegratedServer::bind`) | its configured `view_radius` | a host spends memory and bandwidth for players who did not choose the setting |
| every other `serve_connection*` wrapper | `view_radius` | exactly the pre-#545 behaviour; no wrapper serves a path with its own policy |

`MAX_CLIENT_VIEW_RADIUS = 33` is derived, not chosen: the shell's slider tops out
at `config::MAX_RENDER_DISTANCE = 32` and `set_render_distance` sends
`render_distance + 1` (the outermost streamed ring can never be meshed). It is a
*sanity* bound, not a policy one — the wire field is an `i8`, so without it a
malformed `127` would try to stream 65,025 columns.

**Gotcha: the store's capacity does not follow a live raise.** `ChunkStore`'s
capacity is fixed at construction from the *join* radius, and it is a plain
`usize` behind an `Arc`, so nothing can grow it mid-session. Raising render
distance well past the join value therefore over-subscribes the cache and costs
re-generation — of the *innermost* rings, since `join_view_rings` streams outward
and the LRU victim is the least recently touched (see
`chunk_store::integrated_capacity_for_view_radius`). That is the same tradeoff
already accepted for a short capacity, and it degrades the ground underfoot
rather than the horizon. Making capacity follow the radius is a wider change than
separating these two roles — it needs the capacity behind the cache mutex or an
`AtomicUsize`, *and* a way for `dispatch_play_packet` to reach a `ChunkStore`
through the generic `ChunkSource` it actually holds — and is deliberately not
attempted here.

`FORGET_LEVEL_CHUNK`'s wire layout (a single packed `i64`, `x` in the low
32 bits and `z` in the high 32, mirroring vanilla's `ChunkPos.pack`) is
hand-encoded to match `V770Adapter::handle_play`'s existing
`FORGET_LEVEL_CHUNK` decode arm exactly — that decode is the best available
specification for the layout, since (like `set_chunk_cache_center` and the
`player_position` teleport before it) there was no existing struct to reuse
`Encode` from.

## How to change it, and the gotchas

* **A protocol crate that does not implement the new `ServerProtocol`
  methods still compiles and still serves.** All four default to
  `ServerDirective::None`; `apply()` in `server.rs` treats `None` as a
  no-op. A protocol without keep-alive/time/view support just never sends
  those packets — the loop and its timers still run.
* **`serve_play` forks on `#[cfg(target_arch = "wasm32")]`.** `tokio::time`
  has no working timer driver on `wasm32` (the same reason
  `Connection::read_packet_timeout` in `lodestone-net` is native-only). The
  `wasm32` build of `serve_play` degrades to the pre-existing
  packet-driven-only loop: it still answers keep-alive echoes and streams
  the view reactively (both are driven by *inbound* packets, which need no
  timer), it just never **initiates** a keep-alive challenge or a periodic
  time broadcast, since nothing can wake it when the client goes quiet. This
  is a real, documented gap on that target, not a silent one — see
  `ServerError::KeepAliveTimeout`'s own doc comment.
* **The two forked `serve_play` bodies are separate function definitions**,
  not one function with an internal `#[cfg]`-gated block — `#[cfg]` on a
  bare block-as-statement is unusual enough to be worth avoiding; the split
  mirrors the existing `crates/lodestone-server/src/spawn.rs` precedent
  (two whole-item `#[cfg]`-gated definitions of the same function name).
* **Changing the keep-alive/time intervals**: they are `server.rs` module
  constants (`KEEP_ALIVE_INTERVAL`, `TIME_SYNC_INTERVAL`, `MILLIS_PER_TICK`),
  not parameters — `serve_connection`'s and `IntegratedServer`'s public
  signatures are unchanged by this work on purpose, since
  `lodestone-shell` and `web/src/singleplayer.rs` call them and are held by
  other work. Tests that need to observe the keep-alive timeout without a
  real 15-second wait use `#[tokio::test(start_paused = true)]` plus
  `tokio::time::advance`, the existing pattern in
  `crates/lodestone-net/src/connection.rs`'s own tests.
* **The view window is a square, not vanilla's circle.** If a future change
  ever wants the exact vanilla shape, it has to change both
  `serve_connection`'s initial-view loop and `ViewTracker` together, or the
  two will disagree the moment the player first moves.
* **`ServerBound` is `PartialEq` but not `Eq`.** Adding `PlayerMoved`'s
  `f64` fields removed the derived `Eq` — `f64` has no total order. Nothing
  in this crate needed `Eq` (no `HashSet<ServerBound>`), so this cost
  nothing, but a future consumer reaching for `Eq` needs to know why it is
  not there.

## Configuration

No env vars or flags. The knobs are compile-time constants in
`crates/lodestone-server/src/server.rs`:

| constant | value | source |
|---|---|---|
| `KEEP_ALIVE_INTERVAL` | 15,000 ms | `ServerCommonPacketListenerImpl.java:35-36` |
| `TIME_SYNC_INTERVAL` | 1,000 ms | stands in for vanilla's every-20-ticks-at-20-TPS (`MinecraftServer.java:1095-1099`) |
| `MILLIS_PER_TICK` | 50 | vanilla's normal 20 TPS |
| `MAX_CLIENT_VIEW_RADIUS` | 33 | `config::MAX_RENDER_DISTANCE` (32) + the mesher's buffer ring — the largest live raise singleplayer permits (#545) |

`view_radius` (already an existing `serve_connection`/`IntegratedServer`
parameter) sizes both the initial view and every subsequent `ViewTracker`
recenter — it was not made independently configurable for the two.

`max_view_radius` (`serve_connection_inner`, and the two `pub(crate)` `*_shared`
entry points) is the **ceiling** for a live change, which is a different question
— see "A live render-distance change" above. It is not exposed on the seven public
`serve_connection*` wrappers: each passes `view_radius`, so their behaviour is
unchanged, and only a caller with its own capacity policy needs the second value.

## Dependencies

* `lodestone_core::State`, `lodestone_net::{Connection, Transport}` — the
  same connection/codec seam every other packet in this crate uses.
* `tokio`'s `time` feature (native only; see the `wasm32` fork above).
* `crates/protocol/v770/src/packets/common.rs`'s `KeepAlive` and
  `crates/protocol/v770/src/packets/game.rs`'s `MovePlayerPos`/
  `MovePlayerPosRot` — already-derived `Encode`/`Decode` structs this work
  reuses rather than re-encoding by hand (the same "derive over hand-roll"
  discipline `server_protocol.rs`'s own module doc explains).
* `.cache/mc/26.2/src/net/minecraft/server/network/ServerCommonPacketListenerImpl.java`,
  `.../server/MinecraftServer.java`, `.../server/level/{ChunkMap,ChunkTrackingView}.java`,
  `.../server/players/PlayerList.java`, `.../world/clock/ServerClockManager.java` —
  the jar sources this behaviour is measured against.
* `docs/time-of-day-lighting.md` — the client-side half of the time-of-day
  story (`SetTime::day_clock`, the `DayClock` extrapolation), which this
  server-side work now actually feeds on a live connection instead of only
  a hermetic decode test.

[`ServerBound`]: ../crates/lodestone-server/src/protocol.rs
[`ServerProtocol`]: ../crates/lodestone-server/src/protocol.rs
