# Drowning: the server-side air-supply countdown and its damage

## What it is

Issue #267's server half: `PlayerVitals` (`crates/lodestone-server/src/vitals.rs`)
ticks a connected player's air supply down while their eye is submerged in
water, and deals drowning damage on vanilla's own cadence. Before this, the
client's air-bubble row (`docs/sky-and-air-bubbles.md`) was a display for a
value that only ever arrived at full — `airSupply` decoded correctly end to
end, but nothing on the server ever sent anything other than the join-time
default. This closes the loop from the *server* side, which is the side that
has to be authoritative.

## The thing that was checked first, because it decided the shape

Before writing any drowning logic, the load-bearing question was: **does the
server have a player entity with vitals at all?** It did not.
`crates/lodestone-server/src/server.rs`'s `serve_play` tracked only enough of
`ServerBound::PlayerMoved` to drive chunk streaming (`x`/`z`, truncated to a
chunk column) — no `y`, no health, nothing that survived past that one
computation. So this work is honestly *server-side player vitals*, with
drowning as the first (and currently only) consumer — a materially bigger
task than "add a countdown timer" — and it is documented as such rather than
having a countdown invented a home that did not exist.

## How it works

### The tick

`PlayerVitals::tick(eye_in_water: bool) -> VitalsTick` is a pure step
function with no I/O, mirroring `LivingEntity.baseTick`'s water-breath block
(`.cache/mc/26.2/src/net/minecraft/world/entity/LivingEntity.java:436-458`)
within this module's documented scope:

* **Decrement**: `-1` air/tick while submerged (`decreaseAirSupply`,
  `LivingEntity.java:588-598` — no `OXYGEN_BONUS` attribute exists in this
  crate, so the probabilistic Respiration skip never applies; see "What is
  deferred" below).
* **Refill**: `+4` air/tick, capped at `MAX_AIR_SUPPLY = 300`
  (`increaseAirSupply`, `LivingEntity.java:600-602`), whenever the eye is
  *not* submerged. This is the exact same integer step the client's
  `getCurrentAirSupplyBubble` ceiling-based bubble mapping already assumes
  (`docs/sky-and-air-bubbles.md`) — the server has to tick the identical
  shape or the display and the data disagree.
* **Damage**: once air reaches `<= -20` (`shouldTakeDrowningDamage`,
  `LivingEntity.java:506-508`), air resets to `0` and `2.0` damage
  (`DROWN_DAMAGE`) lands, applied straight to health with no armour/
  absorption model (this crate has none — the same "no inventory model"
  scope `crate::server`'s `UseItemOn` handling already documents). Vitals
  stop ticking once health reaches `0.0`, mirroring the `isAlive()` guard one
  level up from the block this ports.

The **cadence**, not just the mechanism, is the point: a fully-submerged
player takes exactly 300 ticks (15s) to empty from full, then 20 more ticks
(1s) to cross the damage threshold — **320 ticks (16s) to the first hit** —
and every hit after that is another flat 20 ticks apart, because the reset
re-arms an identical countdown. `crates/lodestone-server/src/vitals.rs`'s
module doc comment carries the full jar excerpt and file:line citations;
its unit tests pin the tick-320 and tick-340 hits by exact count, not "a
number went down".

### Submersion, and where position comes from

`serve_play` (native) now tracks the player's `(x, y, z)` from every inbound
`ServerBound::PlayerMoved` (previously only `x`/`z`, truncated to a chunk
column, was kept). A new `VITALS_TICK_INTERVAL` (50ms, matching vanilla's
20 TPS — `crates/lodestone-server/src/server.rs`) fires inside the same
`tokio::select!` loop as keep-alive and time-sync. On each tick, if a
position is known, the eye position (`feet + EYE_HEIGHT`, `1.62` —
`Avatar.java:16`) is floored to a block coordinate and queried through
`ChunkSource::block_state`; `crate::chunk::is_water` (narrower than the
existing `is_air_or_fluid`, since lava does not drown a player) decides
submersion.

If no position has arrived yet (a client that has never sent a single
move), the tick is skipped rather than guessing a spawn position — this
crate is version-free and does not itself know where a protocol's
`begin_play` puts the player.

### Reaching the wire

Two new `ServerProtocol` methods, both defaulting to `ServerDirective::None`
(same pattern as every other optional encoder in this trait):

* `encode_air_supply_update(air: i32)` — a client-bound update to the local
  player's own air supply.
* `encode_set_health(health: f32)` — a client-bound health update.

`V770ServerProtocol` implements both in
`crates/protocol/v770/src/server_protocol.rs`:

* **Air** is a **hand-rolled `SET_ENTITY_DATA` metadata list** carrying only
  vanilla's `DATA_AIR_SUPPLY_ID` field (index `1`, `INT` serializer) for
  `LOCAL_PLAYER_ENTITY_ID` (`1`, the same id `begin_play`'s `GameLogin`
  already sends) — byte-accurate against the existing *decode* side
  (`crates/protocol/v770/src/packets/metadata.rs`'s `read_entity_metadata`,
  `IDX_AIR_SUPPLY`, `SER_INT`), which is what a real client's
  `apply_local_player_air_supply` chain already consumes
  (`docs/sky-and-air-bubbles.md`'s six-hop decode chain). No new client-side
  code was needed — the chain existed, was just never fed non-default
  bytes.
* **Health** re-sends the existing `SetHealth` struct (already used once at
  join) with the new health and the same fixed `food: 20, saturation: 5.0`
  `begin_play` uses — this crate has no hunger model, so there is no real
  value to track for either field.

## What is modelled, and what is deliberately deferred

Say plainly, because the issue's own wording ("Respiration's air-depletion-
rate reduction") implies more than lands here:

* **Respiration (enchantment) and water-breathing / conduit-power (potion
  effects): not modelled.** Nothing in `lodestone-server` or
  `lodestone-entity` implements enchantments or status effects at all yet
  (checked before writing this — no matching type anywhere in either
  crate), so there is no attribute or effect state for
  `decreaseAirSupply`'s `OXYGEN_BONUS` term or
  `MobEffectUtil.hasWaterBreathing`/`shouldEffectsRefillAirsupply` to read.
  `PlayerVitals::tick`'s `eye_in_water` boolean is the seam a future
  enchantment/effect system would extend — decrementing by something other
  than a flat `1`, or refilling while still submerged — not a reason to
  rewrite this module.
* **Bubble columns: not modelled.** The overworld generator this crate
  serves does not place them, so vanilla's `!is(Blocks.BUBBLE_COLUMN)` guard
  can never actually fire against real terrain here.
* **Invulnerability i-frames: not modelled.** Nothing else currently damages
  a player in this crate (no melee, no fall damage, no explosions reaching a
  player), so there is nothing for i-frames to interact with yet. A second
  damage source landing in the same tick window as drowning would currently
  double-apply — a known gap for whoever adds the next player damage source.
* **Non-player entities: player-only.** `Entity` carries air supply too
  (mobs can drown), but `MobSim` streams no per-tick health/metadata to a
  client at all right now (its own module doc comment: "does not stream the
  resulting positions to a connected client"), so there is no wire path for
  mob vitals to reach anyone yet. A mob-drowning ticker would reuse
  `PlayerVitals`'s tick shape inside `MobSim::tick`, not require touching
  this module.
* **Death/respawn: out of scope.** `PlayerVitals::tick` stops ticking once
  health hits `0.0` (mirroring `isAlive()`), but there is no death screen,
  no respawn packet, no corpse — matching `SetHealth`'s own doc comment
  about the client holding a `0.0`-health player on the death screen.
* **What resets air**: only the gradual `+4`/tick refill vanilla itself
  uses whenever the eye is not submerged — there is no separate "instant
  reset" anywhere in the jar for leaving water, death, or respawn to port,
  and this crate has no dimension-change/respawn flow for the latter two to
  matter against yet.

## How to change it, and the gotchas

* **The tick interval is a stand-in, and it has to be the real rate.**
  `VITALS_TICK_INTERVAL` (`crates/lodestone-server/src/server.rs`) is 50ms
  because this crate has no fixed 20 TPS server loop — but unlike
  `TIME_SYNC_INTERVAL` (which only needs to be "often enough"), the exact
  tick counts in `crate::vitals`'s module doc comment are the entire point.
  Changing this constant changes the real-world drowning cadence, not just
  how often a broadcast repeats.
* **`PlayerVitals` is a value type on purpose.** It takes no `ChunkSource`,
  no connection. The submersion test (which does need terrain and position)
  lives in `crate::server`; keeping the split is what makes the tick-cadence
  unit tests in `vitals.rs` runnable with no async runtime, no chunk source,
  no protocol at all.
* **`is_water` is deliberately narrower than `is_air_or_fluid`**
  (`crates/lodestone-server/src/chunk.rs`). Lava does not drown a player (it
  burns, a mechanic this crate does not model) — do not reach for the wider
  predicate here even though it is closer at hand.
* **The metadata hand-encoder restates private constants.** `IDX_AIR_SUPPLY`
  and `SER_INT` live in `packets/metadata.rs` but are not `pub`, so
  `server_protocol.rs`'s `METADATA_IDX_AIR_SUPPLY`/`METADATA_SER_INT` are a
  second copy of the same numbers, not an import. If either changes on the
  decode side (a new 26.2 metadata index reshuffle, say), the encode side
  will not fail to compile — it will silently desync. There is no structural
  guard against this beyond the doc comments pointing at each other; the
  wire round-trip is what `crates/protocol/v770/tests/drowning.rs` actually
  exercises against a real client, which is the practical check.
* **An undrained `EventStream` will silently stall a whole test.** This bit
  the real-client tests once: `ClientBuilder`'s event channel is bounded
  (256 — `crates/lodestone-client/src/builder.rs`), and
  `lodestone-client`'s driver `await`s `events.send(event)` per packet. A
  test that discards the receiver (`let (handle, _events) = ...`) survives
  low-traffic scenarios (a handful of time-of-day broadcasts) but stalls
  outright once traffic crosses the buffer — which one event per 50ms
  vitals tick does well before the first drowning hit. It fails with **no
  error anywhere**: `IntegratedServer::open_in_memory`'s `select!` around
  `serve_connection` discards that future's result the same way, so a stall
  looks identical to nothing having gone wrong except a value that stopped
  changing. `crates/protocol/v770/tests/drowning.rs`'s `drain_events` helper
  is the fix — spawn a task that drains the stream for the test's duration.

## Configuration

None. `MAX_AIR_SUPPLY`, `EYE_HEIGHT`, `DROWN_DAMAGE`, `MAX_HEALTH`
(`crates/lodestone-server/src/vitals.rs`) and `VITALS_TICK_INTERVAL`
(`crates/lodestone-server/src/server.rs`) are compile-time constants, not
runtime knobs — consistent with `KEEP_ALIVE_INTERVAL`/`TIME_SYNC_INTERVAL`
next to them.

## Verification

* `crates/lodestone-server/src/vitals.rs`'s unit tests pin the tick-exact
  cadence in isolation (fresh state, dry no-op, first hit at exactly tick
  320, no damage at tick 319, second hit at exactly tick 340, gradual
  75-tick refill capped at max, a dead player's vitals not re-ticking).
* `crates/lodestone-server/tests/serve_play.rs` adds the version-free
  scheduling proof over the in-memory transport: a subject test spanning
  340 vitals ticks (17s of virtual time, resolved via
  `#[tokio::test(start_paused = true)]` auto-advance) asserting the *entire*
  received air-value sequence against a computed expectation, plus health
  after both hits; and a **control** — a player never submerged, over a
  20s window — asserting **zero** air or health packets, proving the
  submersion test actually gates the tick rather than the subject test
  merely having shown numbers move.
* `crates/protocol/v770/tests/drowning.rs` is the real-wire-format
  companion: a real `lodestone-client` against a real `V770ServerProtocol`,
  proving the hand-rolled `SET_ENTITY_DATA` bytes and the `SET_HEALTH`
  re-send actually decode through the real adapter into
  `PlayerSnapshot::air`/`ClientHandle::health()` — plus the same dry
  control at the real-wire level.

Measured, on this machine: both `serve_play.rs` drowning tests and both
`drowning.rs` real-client tests pass in a fraction of a second of wall time
despite spanning up to 17s of simulated ticking, and `cargo test -p
lodestone-server --no-fail-fast` / `cargo test -p lodestone-v770` are both
green with no `#[ignore]`d or feature-gated tests silently skipped.

## Dependencies

* `crates/lodestone-server` — all of `vitals.rs`; `server.rs`'s
  `dispatch_play_packet`/`serve_play` (position tracking, the new tick);
  `chunk.rs`'s `is_water`; `protocol.rs`'s two new `ServerProtocol` methods.
* `crates/protocol/v770/src/server_protocol.rs` — `encode_air_supply_update`,
  `encode_set_health`, and the `LOCAL_PLAYER_ENTITY_ID` constant `begin_play`
  now shares with them.
* `.cache/mc/26.2/src/net/minecraft/world/entity/{Entity,LivingEntity}.java`,
  `Avatar.java` — the jar sources this behaviour is measured against.
* `docs/sky-and-air-bubbles.md` — the client-side half (issue #60 in the
  issue's own words): the six-hop `airSupply` decode chain and the bubble
  row's ceiling-based refill display, which this work now actually feeds
  live data into instead of a permanent full-air default.
* `docs/served-session-liveness.md` — the `serve_play` scheduling loop
  (keep-alive, time-of-day, view streaming) this work adds a fourth timer
  alongside.
