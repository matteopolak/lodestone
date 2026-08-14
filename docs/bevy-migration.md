# Migrating to `bevy_ecs`

## What this is

A staged plan for moving Lodestone's world/entity/session state onto `bevy_ecs`, so that
third-party extensions are native Rust plugins with the same power as built-in code.

**This is not a new direction.** [`DESIGN.md` §8](../DESIGN.md) already specifies
`bevy_ecs` standalone, 0.19, "for world/entity state — gives systems, schedules, and natural
plugin points for both the renderer and third-party extensions", and adds that
the renderer should be "a separate crate that observes the same ECS world". `DESIGN.md` §13
names azalea as "the best reference for macro ergonomics and ECS client design".

**At the time this plan was written, there was no `bevy` dependency anywhere in the tree**
(`grep -rn bevy --include=Cargo.toml` returned nothing). The architecture described below —
a `RwLock`-guarded read-model in `lodestone-client`, a channel to a god-object `Sim` in
`lodestone-shell` — was a *departure* from the design doc that had never been reconciled. This
migration is a return to it, and Stages 0–3 (§7) have since landed: `bevy_app`/`bevy_ecs` are
workspace dependencies today, and `crates/lodestone-ecs` is the crate this plan added.

**It is not a performance win.** Live entity counts are ~30. `bevy_ecs` will not make that
faster, and azalea's own docs are candid about the trade
([`azalea/src/_docs/performance.md:3-4`](https://github.com/azalea-rs/azalea/blob/main/azalea/src/_docs/performance.md):
"In some cases, performance is left on the table in exchange for simpler or more flexible
interfaces"). The case is extensibility, plus the collapse of the three-copy entity pipeline
described below. Say that plainly to anyone who asks; do not let a stage get justified on frame
time.

---

## 1. What the plan is for: the requirement, and its sharp consequence

The requirement is **"plugins can do everything native code can."** That has one architectural
consequence that dominates every decision below:

> **The ECS must be the *authoritative* state, not a projection of it.**

If the ECS ends up mirroring `ClientState`, a plugin that mutates a component changes nothing
real — and the failure mode is the worst available: a plugin API that appears to work and
silently does not. So every stage in this plan is graded on one test:

**Authority test — did this stage delete the old owner of the data, or merely add a second
reader?** A stage that adds components while `Inner` still holds the fields has failed, no
matter how green the tree is.

Each stage below states its authority test explicitly, and states how long any
two-sources-of-truth window lasts and what bounds it.

### 1.1 There are already two sources of truth, today, and it is a bug

This is not a hypothetical hazard. Measured in the current tree:

| state | fold #1 | fold #2 |
|---|---|---|
| scoreboard | `lodestone_client::scoreboard::Scoreboard` via `Inner::apply` ([`state.rs`](../crates/lodestone-client/src/state.rs)) | folded into `lodestone_ecs::SessionScoreboard`, read by [`Sim::sidebar`](../crates/lodestone-shell/src/sim/session.rs) |
| player list | `Inner.players: HashMap<Uuid, PlayerListEntry>` ([`state.rs`](../crates/lodestone-client/src/state.rs)) | folded into `lodestone_ecs::SessionTabList`, read by [`Sim::tab_list`](../crates/lodestone-shell/src/sim/session.rs) |
| entity pose | `EntityView` ([`state.rs`](../crates/lodestone-client/src/state.rs)) | `EntitySnapshot` ([`entities.rs`](../crates/lodestone-shell/src/entities.rs)) → `Track` ([`entities.rs`](../crates/lodestone-shell/src/entities.rs)) |

Two *different types* named `Scoreboard` fold the same `ClientEvent` stream in two crates, and the
shell simultaneously reads `handle.players()` ([`net.rs`](../crates/lodestone-shell/src/net.rs),
[`Sim::tab_list`](../crates/lodestone-shell/src/sim/session.rs)) *and* keeps its own `TabList`. Entity pose is
copied three times per frame: `EntityView` → `EntitySnapshot` (`net.rs`) → `Track`.

Collapsing these is the concrete, checkable benefit of the migration. It is also the reason the
authority test is the grading criterion: if the migration adds a fourth copy, it has made the
repo worse.

---

## 2. What azalea actually does

Read from `azalea-rs/azalea`, `main`, July 2026 (MIT). Concrete, because "it uses ECS" is not
actionable.

### 2.1 It uses `bevy_app` + `bevy_ecs` + `bevy_tasks` + `bevy_time`, and nothing graphical

[`Cargo.toml:36-43`](https://github.com/azalea-rs/azalea/blob/main/Cargo.toml) pins
`bevy_app = "0.19.0"`, `bevy_ecs = { version = "0.19.0", default-features = false, features = ["multi_threaded"] }`,
plus `bevy_utils`, `bevy_log`, `bevy_tasks`, `bevy_time`. No `bevy_render`, no `bevy_window`, no
`bevy` umbrella crate. `azalea/src/lib.rs:63-64` re-exports `bevy_app as app` and `bevy_ecs as ecs`
so plugin authors do not need to match versions by hand.

### 2.2 A *client* is an ECS entity, not a struct

`azalea-client/src/client.rs:42-78` defines two bundles inserted onto the client's own `Entity`:
`LocalPlayerBundle` (the `RawConnection` component, the `WorldHolder`, player metadata) at login,
and `JoinedClientBundle` (inventory, tab list, hunger, experience, mining, attack, prediction
handler, `EntityIdIndex`) once in the `game` state. `InGameState` / `InConfigState` are marker
components for the protocol state.

Other entities are ordinary ECS entities carrying `EntityBundle`
([`azalea-entity/src/plugin/components.rs:17-36`](https://github.com/azalea-rs/azalea/blob/main/azalea-entity/src/plugin/components.rs)):
`EntityKindComponent`, `EntityUuid`, `WorldName`, `Position`, `LastSentPosition`, `EntityChunkPos`,
`Physics`, `LookDirection`, `EntityDimensions`, `Attributes`, `Jumping`, `Crouching`,
`FluidOnEyes`, `OnClimbable`, `ActiveEffects`. `LocalEntity` (`:84`) marks "ours, don't let another
client's packets move it".

### 2.3 Chunks are **not** components

[`azalea-world/src/world.rs:78-107`](https://github.com/azalea-rs/azalea/blob/main/azalea-world/src/world.rs)
keeps `World { chunks: ChunkStorage, entities_by_chunk, entity_by_id, registries }` as a plain
struct, held in a `Worlds` resource (`azalea-client/src/client.rs:101`,
`app.init_resource::<Worlds>()`). `PartialWorld` (`:25-30`) is the per-client render-distance slice,
and its doc comment is explicit: *"Some metadata about entities, like what entities are in certain
chunks. This does not contain the entity data itself, that's in the ECS."*

This is the split we should copy verbatim: **entities in the ECS, chunk storage behind a resource.**

### 2.4 Packets enter the ECS through an exclusive system in `PreUpdate`

`ConnectionPlugin` ([`azalea-client/src/plugins/connection.rs:40`](https://github.com/azalea-rs/azalea/blob/main/azalea-client/src/plugins/connection.rs))
registers `(read_packets, poll_all_writer_tasks).chain()` in `PreUpdate`. `read_packets` (`:44`) is
an **exclusive** system (`&mut World`): it iterates entities with a `RawConnection` component,
`try_read()`s the socket in a loop (`:85-124`), deserializes, and calls
`game::process_packet(ecs, entity, packet)` *inline* (`:274`). It **also** queues a
`ReceiveGamePacketEvent` per packet, batch-written after the read loop (`:127`, `:152`), so plugin
authors can observe the raw stream.

`process_packet` ([`packet/game/mod.rs:52-56`](https://github.com/azalea-rs/azalea/blob/main/azalea-client/src/plugins/packet/game/mod.rs))
is a `declare_packet_handlers!` macro over ~150 named variants of `ClientboundGamePacket`. Each
handler runs `as_system::<(Query<…>, Commands, Res<…>)>(self.ecs, |params| …)`
([`packet/mod.rs:62-78`](https://github.com/azalea-rs/azalea/blob/main/azalea-client/src/plugins/packet/mod.rs)),
which borrows system params from inside the exclusive context using a `SystemState` cached in a
resource. That is the trick for writing system-shaped code in an exclusive handler.

Handlers do one of three things:
- **mutate components directly** — `player_position` (`game/mod.rs:419-449`) writes `Position`,
  `LookDirection`, `Physics`, then `commands.trigger(SendGamePacketEvent::new(…))` twice;
- **spawn** — `add_entity` (`:566-660`) checks the global `entity_by_id` index first (a swarm may
  already have the entity), otherwise `commands.spawn((entity_id, LoadedBy(…), bundle))` and
  registers it in the per-client `EntityIdIndex` and global `EntityUuidIndex`;
- **defer to a system via a message** — `level_chunk_with_light` (`:555-564`) writes only a
  `chunks::ReceiveChunkEvent`; the decode happens in a normal system in `chunks.rs`.

Entity updates from the network go through `RelativeEntityUpdate` (`:699`), which uses a per-partial-
world `updates_received` counter (`azalea-world/src/world.rs:52-67`) so a shared swarm world does not
apply the same movement once per bot.

### 2.5 Two schedules, one runner, one lock

`azalea-core/src/tick.rs:10` defines `GameTick` — "runs every Minecraft game tick, i.e. every 50ms
… either zero or one times after every Bevy `Update`". `run_schedule_loop`
(`azalea-client/src/client.rs:163-223`) is a hand-written runner: `Update` at ≤60 Hz, `GameTick` at
20 Hz, with a **ten-tick catch-up cap** (`:199-206`) that is the same rule as our
[`docs/frame-pacing.md`](./frame-pacing.md). The whole `World` is taken out of the `App` and put
behind `Arc<parking_lot::RwLock<World>>` (`:143`); the runner takes the write lock each iteration
(`:188`), and `Client` methods take it from async user code (`azalea/src/bot.rs:85`,
`let mut ecs = self.ecs.write()`).

**One comment in that runner is a warning for us**, `client.rs:171-172`:

> `azalea runs the Update schedule at most 60 times per second to simulate framerate. unlike vanilla`
> `though, we also only handle packets during Updates due to everything running in ecs systems.`

Packet ingest is therefore gated on `Update` rate. Our [`docs/frame-pacing.md`](./frame-pacing.md)
records the opposite rule — *presentation must never gate simulation, because a stalled client is
sent no chunks*. **Do not copy this part of azalea.** §4 says what to do instead.

### 2.6 Everything is a plugin, and ordering anchors are public

`DefaultPlugins` ([`azalea-client/src/plugins/mod.rs:30-73`](https://github.com/azalea-rs/azalea/blob/main/azalea-client/src/plugins/mod.rs))
is a `PluginGroup` of 25 plugins — physics, movement, mining, inventory, chat, chunks, connection,
login, join, cookies, tick-end. A user can drop any of them (perf doc `:106-111`, "somewhat risky
and isn't technically officially supported").

`BotPlugin` ([`azalea/src/bot.rs:30-51`](https://github.com/azalea-rs/azalea/blob/main/azalea/src/bot.rs))
shows how a plugin inserts behaviour *between* existing stages:

```rust
.add_systems(Update, (
    insert_bot,
    look_at_listener.before(clamp_look_direction).after(update_dimensions),
    jump_listener,
))
.add_systems(GameTick, stop_jumping
    .after(PhysicsSystems)
    .after(azalea_client::movement::send_player_input_packet))
```

Anchors are a mix of a public `SystemSet` (`PhysicsSystems`) and public *system functions*
(`clamp_look_direction`, `send_player_input_packet`). We should offer only sets (§6).

### 2.7 There is a second, weaker, ergonomic tier — deliberately

`azalea/src/events.rs` defines a plain `Event` enum (`:58`) for async handler functions, fed by
systems that read the ECS messages. The contributor note at `:29-49` is worth quoting because it
states the two-tier design as policy:

> `HOW TO ADD A NEW (packet based) EVENT: - Add it as an ECS event first … At this point, you've`
> `created a new ECS event. That's annoying for bots to use though, so you might wanna add it to`
> `the Event enum too`

`Event::Packet` (`:113`) carries `Arc<ClientboundGamePacket>` and is behind the default-on
`packet-event` feature (`azalea-client/Cargo.toml:51`) precisely because it costs a clone per packet
(perf doc `:79-86`).

### 2.8 What azalea does *not* do, that we must not do

`process_packet` matches on `ClientboundGamePacket` — **a version-specific type — inside
`azalea-client`, the shared client crate.** That is fine for azalea: `DESIGN.md:2083` already notes
it is "single version at a time". It is fatal for us. §5 is the whole answer.

---

## 3. `bevy_ecs` 0.19 is real, and this is the version to use

The briefing doubted that `bevy_ecs` 0.19 exists. It does: `0.19.0` was published **2026-06-19**
and is `max_version` on crates.io (`GET /api/v1/crates/bevy_ecs`), after `0.19.0-rc.1`…`rc.3` in
May/June and `0.18.1` on 2026-03-04. azalea `main` pins exactly `0.19.0`. **`DESIGN.md:520` is
correct as written** — no discrepancy to record.

Features, per [docs.rs](https://docs.rs/crate/bevy_ecs/0.19.0/features) — 15 flags, 4 default:

| feature | default | take it? |
|---|---|---|
| `std` | yes | yes |
| `async_executor` | yes | only with `multi_threaded` |
| `backtrace` | yes | yes (native), drop for wasm |
| `bevy_reflect` | **yes** | **no** — heavyweight, and we have no scene/serialisation need |
| `multi_threaded` | no | **no** (see below) |
| `debug` | no | dev-only; azalea pulls `bevy_utils/debug` for system names in conflict warnings (`azalea-client/Cargo.toml:40-42`) |

Start with `bevy_ecs = { version = "0.19", default-features = false, features = ["std"] }` and add
back only what fails to compile.

### 3.1 wasm

`lodestone-client` is in `scripts/wasm-check.sh`'s crate list (`:102`), so anything it depends on
must build for `wasm32-unknown-unknown`. `bevy_ecs` gained `no_std` support in
[bevy#16758](https://github.com/bevyengine/bevy/pull/16758) and `wasm32-unknown-unknown` is the
one wasm target Bevy supports with `std`, so this is *expected* to work.

**Do not write that down as a fact.** The repo's evidence standard says an expected value must come
from outside the code under test, and "compiles for wasm" is exactly the kind of claim that has been
wrong here before. Stage 0's deliverable is `scripts/wasm-check.sh` green with `bevy_ecs` in the
graph. If it is not green, the migration stops at Stage 0 for the cost of a day — which is why
Stage 0 is first.

Two things are known-broken and must be avoided:
- **`multi_threaded` + no threads.** wasm has no threads; `multi_threaded` without `std` does not
  even compile ([bevy#21144](https://github.com/bevyengine/bevy/issues/21144)). With ~30 entities
  there is no reason to want it. Leave it off on *all* targets so native and wasm run the same
  executor and the same system order.
- **Cargo features are advisory here, as everywhere in this repo.** Feature unification means
  "`multi_threaded` only on native" is not a boundary any consumer must respect. If it ever matters,
  the boundary is `cfg(target_arch)`, not a feature.

---

## 4. The target architecture

### 4.1 One World, one driver, a lock only for outsiders

**(c) and (d) landed.** See [`world-unification.md`](./world-unification.md) for (c) and
[`chunk-world-resource.md`](./chunk-world-resource.md) for (d). Six places where this section, or the
brief handed to (c), was wrong:

- **"a lock only for outsiders" is not what shipped, and could not be.** The heading and (c)'s own
  text say the driver "owns the `World` outright" and the lock exists for bot code. But the *net
  thread* writes the read-model, so the driver and the net thread both hold the handle and the
  contention is **driver-vs-ingest**, not "bot-code-vs-driver". §2.5's promise that "a slow frame
  delays *application*, never *receipt*" therefore does **not** hold for this lock:
  `SharedState::apply` runs inline in the driver task *before* `events.send(event).await`, so blocking
  on the `World` lock stops the socket being read too. What bounds it is that no guard spans a frame —
  `Sim::step` takes ~15 short guards plus ~8 per catch-up tick, and the longest single hold is one
  `run_schedule`. The worst case a packet waits is one guard hold, not one frame.
- **The catch-up policy had to be *decided*, not merely unified.** There were two clamps, five ticks
  and ten, and the tighter one silently shadowed the other. Ten won — vanilla's
  `MAX_TICKS_PER_UPDATE`, the only one of the two with an external oracle — and §4.2's claim that
  `GameTick`'s cap "comes from `docs/frame-pacing.md`" was true of the document and false of the code
  until now. `app.rs`'s pacing assertion changed from `5` to `10`, and its three tick constants are
  now *aliases* of `lodestone-ecs`'s rather than local re-derivations.
- **The `PlayerSnapshot` vitals were not blocked only on (c).** Stages 2 and 3 both bounded that
  residue by "the §4.1 `World` unification". The second blocker is `SharedState::apply`'s
  **exclusive** routing switch: `Login`, `HealthChanged`, `Respawned` and `Death` each carry vitals
  *and* `dimension`/`game_mode`/`alive`, and claiming one for a `NetIngest` system stops `Inner::apply`
  seeing it — freezing `dimension` (the too-bright-Nether bug, by traversal) and `alive`. They are
  **still duplicated**. Separately, `alive` and `Dead` are two different rules over the same events,
  and one of them has a live-gate test switch on it (`recover_from_death`), so they must not be merged
  either.
- **One `World` forces one *entity*.** `spawn_local_player` and `spawn_session` both spawn a
  `LocalPlayer`; in one `World` that is two players and every `With<LocalPlayer>` system silently sees
  both. This section does not mention it and it is the first thing that breaks.
- **One `World` does *not* mean one resource per type.** `tick_item_physics` and `player_physics`
  both wanted `PlayerCollision`, and merging them would have merged two genuinely different decisions
  (`Pending` holds the player but must not freeze items; the `collide_against_live_world` control must
  not also disable item physics). `ItemCollision` is a deliberate second resource.
- **`EntityInterpolator` still owns a `World`, on purpose.** Two `#[ignore]`d live GPU gates and ~25
  unit tests drive it with no `Sim` at all. It runs the same systems off the same `FrameClock` type,
  so it is a second *instance*, not a second mechanism — and `TickAccum`, the second accumulator
  *type*, is deleted.

Still open from Stage 1: ingest folds the server's report onto one entity per mob and
`fold_snapshots` spawns a second, render-side entity per mob, with `EntitySnapshot` between them. (c)
removed one of that collapse's two stated blockers (the borrowed-slice `'static` problem — the
components are in the same `World` now); the other stands, since ingest runs in `NetIngest` (ordered
*before* `GameTick`) while the interpolator's order is clocks → ticks → fold.


```
  net thread (tokio, owns the socket)          driver thread (winit, or a headless timer)
  ┌───────────────────────────────┐            ┌──────────────────────────────────────────┐
  │ transport → VersionAdapter    │  ClientEvent│  NetIngest    drain channel → components │
  │ ::handle_packet               │───channel──▶│  GameTick ×N  20 Hz, catch-up capped 10  │
  │ chunks → WorldSink ───────────┼──Arc<RwLock │  Update       input, interp α, camera    │
  │ (heavy payload never channels)│  <World>>──▶│  Extract      components → GPU buffers   │
  └───────────────────────────────┘            └──────────────────────────────────────────┘
                                                        ▲ Arc<RwLock<bevy World>>
                                                        │ short read/write locks
                                                   async bot code / ClientHandle
```

Four decisions, each load-bearing:

**(a) The net thread stays.** It owns the tokio runtime and the socket
([`connect_impl`](../crates/lodestone-shell/src/net.rs) spawns `lodestone-net`), and it must keep
draining regardless of frame rate. It produces `ClientEvent`s into the existing channel — which
`NetClient::poll()` (`net.rs`) already is. **This is how we avoid azalea's ingest-gated-on-
`Update` problem (§2.5):** the socket is drained continuously and buffered; a slow frame delays
*application*, never *receipt*.

**(b) All schedules run on one thread, in order, once per frame.** Not azalea's split. The reason is
`lodestone-physics`: it is bit-exact against a JVM oracle with golden traces. If input lands on one
thread and physics runs on another, which input a tick sees becomes a scheduling artefact and the
golden traces stop pinning anything. One thread, fixed order, deterministic.

**(c) The World lives behind `Arc<parking_lot::RwLock<bevy_ecs::World>>` anyway** — not for the
driver, which owns it, but so async bot code and `ClientHandle` can read it, exactly as azalea's
`Client` does (`client.rs:143`, `bot.rs:85`). Contention is bot-code-vs-driver only, which azalea
has shipped for years.

**(d) Chunks stay in `Arc<RwLock<lodestone_world::World>>`, exposed as a `Resource`.** Not
components. This is azalea's `Worlds` (§2.3) and it is also what preserves the reason chunk data
bypasses the event channel today ([`state.rs`](../crates/lodestone-client/src/state.rs)'s "Why chunk
data lives here, not in the event channel" doc comment: a decoded column is orders of magnitude
larger than a scalar event, and routing it through a bounded channel lets a slow consumer buffer
whole columns). The adapter keeps writing through `WorldSink`
(`state.rs`); systems read the resource.

### 4.2 Schedules and public sets

New crate `lodestone-ecs` owns these. They are the plugin ABI (§6).

| schedule | cadence | contains |
|---|---|---|
| `NetIngest` | once per driver iteration | `IngestSet::Drain` → `IngestSet::Apply` → `IngestSet::Index` |
| `GameTick` | 20 Hz, ≤10 catch-up | `TickSet::Input` → `TickSet::Physics` → `TickSet::Predict` → `TickSet::Animate` → `TickSet::Send` |
| `Update` (bevy's) | per frame | `FrameSet::Input` → `FrameSet::Interpolate` → `FrameSet::Camera` |
| `Extract` | per frame, last | `ExtractSet::Terrain` → `ExtractSet::Entities` → `ExtractSet::Hud` |

`GameTick`'s catch-up cap and the ten-tick rule come from
[`docs/frame-pacing.md`](./frame-pacing.md), not from azalea — they happen to agree
(`azalea-client/src/client.rs:199-206`).

### 4.3 `VersionAdapter` becomes a resource, and stays a trait object

`VersionAdapter` is already `Send + Sync + Debug`
([`adapter.rs`](../crates/lodestone-model/src/adapter.rs)), so this is legal today with no
signature change:

```rust
#[derive(Resource, Debug)]
pub struct VersionData(pub Box<dyn lodestone_model::VersionAdapter>);
```

Keep it a trait object. Making it a generic parameter would monomorphise the whole App per protocol
family and force the shell to name a version — the thing `lodestone-shell` has never done
(its only route to version data is `lodestone_registry::adapter_for_protocol`, see
[`Sim::adopt`](../crates/lodestone-shell/src/sim/build.rs)).

Small free win: the shell currently holds a **second** adapter instance because the first was moved
into the driver thread (`Sim::adopt` in `sim/build.rs`). With one App, that collapses to one resource.

### 4.4 The renderer observes, but does not depend on bevy

`DESIGN.md:521` says the renderer is "a separate crate that observes the same ECS world". Take the
spirit, not the letter: **`lodestone-render` keeps zero bevy dependency.** `Extract` systems live in
`lodestone-shell` and produce the plain POD instance buffers `lodestone-render` already consumes
(`EntityInstance`, the mesh/instance uploads).

Why not put `Query`s in `lodestone-render`:
- it is in the wasm crate list (`wasm-check.sh:92`), and `bevy_reflect`/`bevy_app` are real bundle
  cost for the browser client;
- it makes bevy a *public* dependency of the GPU layer, so a bevy minor bump becomes a renderer
  change on top of the wgpu-4-bind-group and reversed-Z constraints that already live there;
- extract-in-the-consumer is what Bevy's own renderer does.

The observable consequence: `EntityInterpolator::draws()`
([`entities.rs`](../crates/lodestone-shell/src/entities.rs)) becomes a system in
`ExtractSet::Entities`, and `EntityDraw` either survives as the extract output type or is replaced
directly by `EntityInstance`.

---

## 5. Packet handling without leaking version types

This is the one place we must diverge from azalea structurally, and the reason it works is that
**we already have the seam azalea lacks.**

azalea: `azalea-client` (shared) matches on `ClientboundGamePacket` (version-specific).

Lodestone: the version crate implements `VersionAdapter::handle_packet(&self, world: &mut dyn WorldSink,
state, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError>`
([`adapter.rs`](../crates/lodestone-model/src/adapter.rs)). Version knowledge is already
lowered to `ClientEvent` (version-free, `lodestone-model/src/event.rs`) plus `Directive`, and
`packet_id` is documented as never escaping that boundary.

So the migration is: **the fold becomes systems; the decode does not.**

- The net thread calls `handle_packet` as it does today. Unchanged.
- `NetIngest` systems replace `Inner::apply` ([`state.rs`](../crates/lodestone-client/src/state.rs)),
  one system per event family (player, entity spawn/despawn, entity movement, metadata, equipment,
  attributes, scoreboard, boss bars, menus, time). Each is a public item in the `IngestSet::Apply`
  set, so a plugin can order against it.
- A version crate **never** becomes a bevy plugin and never appears in `lodestone-ecs`'s
  dependencies. `xtask check-isolation` continues to enforce that.

**The rule, stated so it can be checked:** version types may flow to *leaf* crates (a binary, a
plugin, a test), never to *shared* crates (`lodestone-model`, `lodestone-ecs`, `lodestone-world`,
`lodestone-render`, `lodestone-shell`).

### 5.1 Raw packet access for plugins that want it

"Plugins can do everything native can" includes seeing bytes. Offer azalea's `packet-event`
equivalent, off by default:

```rust
#[derive(Message)]
pub struct RawPacket { pub state: ConnectionState, pub id: i32, pub payload: Arc<[u8]> }
```

Version-**opaque**: an `i32` and bytes. A plugin that wants to decode it depends on
`lodestone-v770` itself — legal, because the plugin is a leaf. The invariant holds and the
capability exists. Off by default for the same reason azalea gates it: a clone per packet
(perf doc `:79-86`).

---

## 6. The plugin API

A plugin is `impl bevy_app::Plugin`, added with `App::add_plugins`. `lodestone-ecs` re-exports
`bevy_app` and `bevy_ecs` (azalea does this at `azalea/src/lib.rs:63-64`) so authors never mismatch
versions.

**Registers:** systems into `NetIngest` / `GameTick` / `Update` / `Extract`, ordered against the
public `SystemSet` labels in §4.2; its own components, resources, messages and observers; and
optionally its own `PluginGroup`.

**May read:** every component and resource in the prelude — `Position`, `LookDirection`,
`Health`, `Hunger`, `Inventory`, `MinecraftEntityId`, `EntityKind`, `WorldTime`, the chunk-world
resource, the folded scoreboard/tab-list/boss-bar components.

**May mutate:** the same. That is the point of the requirement. A plugin can move the local player,
retarget its look, change its inventory intent, add HUD overlays, and add a `GameTick` system
between `TickSet::Physics` and `TickSet::Send`.

**Sends actions** through the one sanctioned egress, `MessageWriter<SendAction>` carrying a
`lodestone_model::ClientAction` — the analogue of azalea's `SendGamePacketEvent`
(`azalea-client/src/plugins/tick_end.rs:34`). Never by touching the socket.

**Deliberately off-limits — by construction, not policy:**
- version types (§5), unless the plugin depends on a version crate itself;
- the GPU device / queue / pipelines. `lodestone-render` is not in the ECS (§4.4). A plugin that
  wants to draw registers an `Extract` system that appends to an instance buffer; it does not get a
  `wgpu::Device`. The 4-bind-group floor and the winding-sign invariant are constraints a plugin
  author cannot be expected to know, so they stay behind the renderer's own API.

**Deliberately off-limits — by policy: nothing.** Which brings us to the security story, because
this is the easiest thing in the whole plan to get wrong.

### 6.1 Two plugin tiers, and they are not substitutes

| | native bevy plugin | WASM host (`DESIGN.md:522`) |
|---|---|---|
| power | everything native code can do | a curated capability ABI: queries + actions |
| trust | **fully trusted. no sandbox.** | untrusted-safe |
| loading | **compiled into the binary** | loaded at runtime |
| filesystem / network | unrestricted (`std::fs`, sockets, `Command`) | denied unless a capability is granted |
| stability | pinned to `bevy_ecs` 0.19's API; breaks on bevy bumps | our ABI, versioned by us |
| primary? | **yes, per the requirement** | secondary, weaker tier |

Two things must be said plainly and repeated in the public docs:

1. **A plugin with full ECS access is trusted code with no meaningful sandbox.** `add_plugins` is
   `dlopen`-equivalent trust. It can delete the user's files. There is no capability check to add
   that would change this, because the requirement *is* native-equivalent power.
2. **bevy plugins are compiled in, not dynamically loaded.** "Install a plugin" means "add a
   dependency and rebuild". If the goal includes third parties shipping *binaries* users drop into a
   folder, bevy does not deliver that — that is `abi_stable`/`dlopen` territory, with Rust's
   unstable ABI in the way. This is the most likely misunderstanding of what this migration buys,
   so it belongs in the README the day Stage 0 lands.

The WASM host is therefore not made redundant by this migration; it becomes the answer to a
*different* question (untrusted automation, hot-loadable scripts). Both may be wanted. Conflating
them is the easiest mistake here.

---

## 7. The staged sequence

Six stages. Each ends with something observable on screen or in a gate, and each states its
authority test. The repo's dominant defect is the island — a subsystem that lands complete and
reaches nothing (§"the two rules that matter most" in `CLAUDE.md`) — and a state migration that
converts state without converting a consumer would be the largest island yet.

`cargo xtask check-connected` is the island detector for this work. **Do not allowlist
`lodestone-ecs`.** If a stage leaves it unreachable from a shipped binary root, that tool is
supposed to go red, and its going red is the plan working.

---

### Stage 0 — the App, the schedules, and one real slice

**Landed** (`415138f`). `crates/lodestone-ecs` exists with the four schedules and
their set labels (§4.2), `WorldTime`, and the `Arc<RwLock<World>>` handle. `Inner`'s
`world_age`/`time_of_day` fields are gone from `lodestone-client/src/state.rs` —
`state.rs` now reads `WorldTime` through the resource and its own comment says so, on
`SharedState::time` (`state.rs`): "`docs/bevy-migration.md` deleted `Inner.world_age`/`Inner.time_of_day`".
`lodestone-ecs` is in `scripts/wasm-check.sh`'s `CRATES` list, and
`cargo xtask check-connected` has no allowlist entry for it, per plan.

**Moves:** `world_age` / `time_of_day` (`Inner`, `state.rs`) →
`#[derive(Resource)] struct WorldTime { age: i64, time_of_day: i64 }`.

**Adds:** crate `lodestone-ecs` — `bevy_app` + `bevy_ecs` (`default-features = false`,
`features = ["std"]`), the four schedule labels and set labels of §4.2, a `CorePlugin`, the
`Arc<RwLock<World>>` handle type, and a `Runner` seam (winit-driven in the shell, timer-driven
headless — azalea's `run_schedule_loop`, `client.rs:163-223`, is the model for the headless arm).
`lodestone-shell`'s `redraw` ([`app.rs::WindowApp::redraw`](../crates/lodestone-shell/src/app.rs)) calls
`app.update()` before `sim.step(dt)`.

**Stays:** everything else, untouched.

**Authority test:** the `Inner` fields are **deleted**, not mirrored. The day/night driver of the
sky and light reads `WorldTime`. If `Inner.time_of_day` still exists at the end of this stage, the
stage failed.

**Two sources of truth:** none, by construction. This deletion-not-mirroring shape is the template
for every later stage.

**Verified by:** (1) `scripts/wasm-check.sh` green with `lodestone-ecs` added to the `CRATES` list
at `:85-111` — this is the *only* acceptable evidence `bevy_ecs` is wasm-clean, and it is the
migration's single biggest go/no-go; (2) `cargo xtask check-connected` green — the new crate has a
consumer immediately; (3) the existing time-of-day light gate
(`crates/lodestone-shell/tests/live_entity_light_time_of_day.rs`) still shows entity lighting
tracking the server clock. Record the wasm bundle delta from `trunk build`, since `bevy_reflect`
being default-on is the thing most likely to surprise.

**Why first:** it front-loads the only risk that can kill the plan outright, for about a day.

---

### Stage 1 — entities become entities (three copies → one)

**Landed** (`8be6544`). See [`entity-components.md`](./entity-components.md) for
the shipped shape and three places it differs from the plan below:

- **`EntitySnapshot` did not die.** Its own doc comment (`entities.rs`) says why:
  it survives because its producer (`net.rs`) and its consumer (`sim.rs`) are on
  opposite sides of a boundary this stage did not close, and it is explicitly
  "slated for deletion" once ingest writes the render-side components directly —
  which needs the collision source and the snapshot slice to become `'static`,
  and neither can yet (see `docs/entity-components.md`'s "two things are not
  systems yet" gotcha; that closes at Stage 4).
- **`EntityView` is the sanctioned intermediate, exactly as planned** —
  components authoritative, the struct derived on demand for
  `ClientHandle::entities()` — not the reverse.
- **The render and ingest sides landed as two `World`s, not one**, deliberately:
  `crates/lodestone-ecs/src/{entity,ingest}.rs` (the net thread's `World`, owned
  by `SharedState`) versus `crates/lodestone-shell/src/entities.rs`
  (`EntityInterpolator`'s own `World`). Unifying them is §4.1, not this stage.

**Moves:**
- `EntityView` (`state.rs`) and `Inner.entities` (`state.rs`) → components:
  `MinecraftEntityId(i32)`, `EntityKind(ResourceKey)`, `Position`, `Rotation`, `HeadYaw`,
  `Velocity`, `OnGround`, `EntityFlags`, `CustomName`, `Pose`, `Health`, `Baby`, `Variant`,
  `Equipment`, `Attributes`, `DisplayItem`.
- `Inner::apply`'s entity arms (`state.rs`) and `apply_metadata` (`state.rs`) →
  `NetIngest` systems.
- `EntityInterpolator` / `Track` / `ItemPhysics` (`entities.rs`) → components
  `InterpFrom`, `InterpTo`, `InterpClock`, `WalkAnim`, `ItemPhysics`, plus a `GameTick` system for
  the 20 Hz item-physics step and walk-animation tick and an `Update` system for the clocks.
- `EntitySnapshot` (`entities.rs`) and `net::entity_snapshot` (`net.rs`) → **deleted.** The
  intermediate copy has no reason to exist once ingest writes components directly.
- `EntityInterpolator::draws()` (`entities.rs`) → an `ExtractSet::Entities` system.

**Stays:** `lodestone_entity::{pose, item_entity}` — `WalkAnimation`, `walk_target_speed`,
`clamp_head_to_body`, `ItemMotion`. Plain functions the systems call. Do **not** turn them into
systems; they are the vanilla-constant carriers and their unit tests are the only thing pinning
them.

**Authority test:** `EntityView`, `EntitySnapshot` and `Track` are all gone from the tree at the end
of the stage. A plugin that writes `Position` on a mob moves it on screen.

**Two sources of truth:** the danger is landing "components exist, the interpolator is still
authoritative". Bound it two ways:
- land the component set and the deletion of `EntityView`/`Track` **in one change**;
- if that is too large, the *only* legal intermediate is the reverse direction — components
  authoritative, with a `#[deprecated(note = "removed in stage 1b")] fn entity_view_compat()`
  deriving an `EntityView` on demand for `ClientHandle::entities()`. One direction only, one stage
  wide, with the removal stage named in the attribute.

**Verified by:** the ~20 interpolation tests in `entities.rs`'s test module (`mod tests`, after line
2589) ported to drive systems — including the two negative controls
(`item_pop_without_velocity_never_rises_above_spawn_apex_control` at
`entities.rs::item_pop_without_velocity_never_rises_above_spawn_apex_control` and
`item_pop_position_only_snapshots_produce_no_apex_either` at
`entities.rs::item_pop_position_only_snapshots_produce_no_apex_either`),
which must still **fail** if the item-physics system is removed. Plus
`crates/lodestone-render/tests/entity_night_pixels.rs` and the live entity gates: pixels inside the
mob's screen rect, which is the only thing that can see an island here.

**Gotcha that will bite:** the nested `Option<Option<T>>` in `EntityView.item` /
`EntityView.custom_name` encodes *"never reported"* vs *"explicitly cleared"*, and
`entities.rs`'s test module has tests for both. In the ECS the natural encoding is **component absent =
never reported, component present with an empty value = explicitly cleared** — which is strictly
clearer. A careless port that spawns every component with a default silently regresses the
dropped-item-goes-invisible defect. Port those two tests first.

---

### Stage 2 — the local player, and physics intent

**Landed.** See [`local-player-components.md`](./local-player-components.md) for what the shipped
shape is and why it differs from the plan below in three places:

- **`Mining` / `Placement` did not move**, and the reason is the same one that blocked item physics
  in Stage 1: their inputs (`Sim.target`, `version_data`, the live block store, the particle
  emitter, direct demo-world edits) are Stage 3/4 residents, so a system now would need them
  mirrored into resources — the exact failure the authority test forbids. `SelectedSlot`,
  `LastPlayerInput` and `LastSprintingSent` *did* move.
- **`PlayerSnapshot` did not move.** It is in `lodestone-client`, on the net thread, folding the
  *server's* view of the player from `ClientEvent`s; the components are the driver thread's
  *prediction*. Collapsing the two is the `World` unification of §4.1, not this stage.
- **The collision borrow was solved, and not the way Stage 1's report expected.** `Arc<dyn
  CollisionView + Send + Sync>` covers the live path but not the offline demo world, whose adapter
  borrows. The shipped seam is `CollisionSource`, which hands a `&dyn CollisionView` to a callback
  instead of returning one. `LiveCollision: Send + Sync` held. **This also unblocks
  `tick_item_physics`**, which no longer has any reason not to be a `GameTick` system.

**Moves:** `PlayerSnapshot` (`state.rs`) and `Sim.player: PlayerState` (`sim.rs`) →
components on the `LocalPlayer` entity, plus `PhysicsState`, `MovementIntent`, `FluidState`
(`sim.rs`), `Mining`, `Placement`, `SelectedSlot`, `LastPlayerInput` (`sim.rs`).

**Stays — emphatically:** `lodestone-physics` remains a plain library. The system reads components,
calls `lodestone_physics::…`, writes components back. **Do not move the integrator into a system.**
It is bit-exact against a JVM oracle with golden traces; a system that reads and writes components
is a *new* integration surface for the same math, and if the math itself moves, the golden traces no
longer pin it — you would be re-deriving the oracle from the code under test, which is the exact
failure the evidence standard names.

**Ordering:** `TickSet::Input` → `TickSet::Physics` → `TickSet::Predict` → `TickSet::Animate` →
`TickSet::Send`, with movement/input packets emitted only in `Send`. azalea's
`game_tick_packet.after(PhysicsSystems).after(MiningSystems).after(send_position)`
(`tick_end.rs:18-26`) is the precedent: the tick-end packet must follow everything that might send.

**Authority test:** `PlayerSnapshot` deleted; `Sim.player` deleted. A plugin adding a system between
`Physics` and `Send` can change what the server is told this tick.

**Two sources of truth:** one stage wide. `Sim` still exists but no longer owns player state.

**Verified by:** the physics golden traces (unchanged — they test the library, not the schedule);
the live jump gate, whose discriminator is `teleport_count` (`sim.rs`) — a burst *during* a
jump is the server rejecting the ascent, so a flat count through a clean arc is the assertion; and
`collide_against_live_world = false` (`sim.rs`) still reproducing the fall-through negative
control.

---

### Stage 3 — session and HUD state; the double fold dies

**Landed.** See [`session-components.md`](./session-components.md) for the shipped shape. Five
places where the plan below was wrong:

- **The double fold was a *triple* implementation, and one leg was already dead.** Besides the two
  §1.1 named, `lodestone_game::bossbar::BossBarSet` was a complete, unit-tested fold of the same
  event family with **no production caller**, and `NetClient::sidebar()` — the only reader of
  `lodestone_client::Scoreboard` on the shell side — had **zero callers**, so the client's scoreboard
  never reached a pixel. The two live folds also *disagreed* (3 display slots vs 19, create-on-demand
  vs drop for a score preceding its objective, `Option<Text>` vs defaulted display name), and the
  player-list fold in `Inner::apply` had **no `PlayerListRemove` arm at all** — a player who left the
  server never left `ClientHandle::players()`. Collapsing was right; the plan understated it.
- **`Inner` is not empty and could not be.** The authority test as written ("`Inner` is empty at the
  end of this stage and the struct is deleted") is unreachable while there are three `World`s.
  `PlayerSnapshot` stays, per Stage 2's own ruling, and its *vitals* remain a genuine duplicate of
  the driver's `Vitals`/`Xp`/`ServerEntityId`/`Dead` — because those are read by **systems** in the
  driver's `World` (`Dead` gates `MovementIntent`, `ServerEntityId` filters mob effects) and a
  component in one `World` is invisible to a system in the other. That residue closes at §4.1, not
  here. What did happen is what the test was for: every one of the four `Inner` fields the stage
  names is deleted, and `Sim` lost eleven fields plus the `SessionPhase` definition.
- **`chat_log` did not move.** Every push needs `Sim.clock_secs` and every read needs it again to
  age the line for the vanilla fade; a component would carry a second copy of `Sim`'s clock. It
  moves with `clock_secs`, in Stage 5.
- **`Egress` did not collapse into the session phase**, as Stage 2's note predicted it would. Its
  `in_world` bit *is* now derived from the `Phase` component, but its `live` bit is
  `vanilla_atlas.is_some() && net.is_some()` — an asset/config fact, not a phase — so the resource
  survives as the derived gate it already was.
- **`lodestone_game::player_state::HudState` is a fourth implementation of the vitals fold, also
  with no production caller, and it is the wrong shape to adopt.** Its `health` is an `f32`
  defaulting to `20.0` with no "reported yet" bit; both live folds carry one, and the HUD needs it
  (the offline world must draw *no* health bar, not a full one). Adopting it is a change to a
  canonical aggregate, not a migration step.

One bug shipped and was caught inside the stage, and it is worth reading rather than summarising:
`SessionPlugin` registered its own `drain_ingest_queue` "idempotently" beside `IngestPlugin`'s, but
`add_systems` does not deduplicate — the second copy cleared the batch the first had filled, so the
real `new_ingest_handle()` configuration folded **nothing** while `SessionPlugin`'s own unit tests
stayed green on a one-plugin `App`. A closed loop, with no pixels involved. See
[`session-components.md`](./session-components.md)'s gotchas.

**Moves:** `chat_log`, `tab_list`, `scoreboard`, `hud_effects`, `title`, `action_bar`, `health`,
`food`, `experience`, `phase`, `dead`, `respawn_count`, `local_entity_id`
(`sim.rs`) and `Inner.{players, scoreboard, boss_bars, menus}` (`state.rs`) →
components on the `LocalPlayer` entity.

Components on the local player, **not** resources — that is what keeps multi-client/swarm possible
later (azalea's entire design rests on it), and it is free now and expensive to retrofit.

**Stays:** `lodestone-game`'s folds (`Scoreboard::apply`, `TabList::apply`, `Menus::apply`,
`TitleState::apply`) as plain functions. One system per fold, calling one implementation.

**Authority test:** `Inner` is **empty** at the end of this stage and the struct is deleted. The
duplicate fold in §1.1 is gone: `lodestone_client::scoreboard::Scoreboard` and the second
`TabList` fold — the one `[`sim.rs::Sim::tab_list`](../crates/lodestone-shell/src/sim.rs) /
[`sim.rs::Sim::sidebar`](../crates/lodestone-shell/src/sim.rs) now read — both disappear, leaving
`lodestone-game`'s one implementation called from one system.

**Two sources of truth:** this stage's *purpose* is to remove the two that already exist. The
overall `Inner`-vs-ECS window therefore spans Stages 0–3 only, per-field one stage, and closes here
— not in Stage 5.

**Verified by:** `crates/lodestone-shell/tests/container_screen.rs`, the vanilla-HUD-text gates, and
a new assertion that is cheap and worth having: **exactly one** system writes each HUD component.
Set `ScheduleBuildSettings { ambiguity_detection: LogLevel::Error }` in tests (azalea uses `Warn` in
its `AmbiguityLoggerPlugin`, `client.rs:246-262`) and prove the detector works by temporarily adding
a second writer and observing the failure.

---

### Stage 4 — the chunk world and meshing

**Landed.** See [`chunk-world-resource.md`](./chunk-world-resource.md) for the
shipped shape. Five places where the plan (or the brief handed to the stage) was
wrong:

- **§4.1 has two independent `World` unifications and they were being read as one.**
  Clause (c) is the *bevy* `World` behind `Arc<parking_lot::RwLock<bevy World>>`;
  clause (d) is the *chunk* store behind `Arc<RwLock<lodestone_world::World>>`.
  Stage 4 is (d). Everything Stages 1–3 deferred "to §4.1" — `PlayerSnapshot`'s
  vitals, the `Dead`/`ServerEntityId` duplicates, "a plugin adding a `GameTick`
  system has to pick which `App`" — is deferred to **(c)**, and (d) does not touch
  it. (c) needs `Sim`'s `EcsHandle` threaded through `NetClient::connect` in
  `lodestone-shell/src/net.rs`; the reverse direction is not an alternative,
  because `Sim.local`'s stability across `end_session` is load-bearing and a
  `World` that changes identity mid-session would invalidate it.
- **`CorePlugin`'s refusal to insert `WorldTime` is therefore *not* obsolete.**
  That guard exists to stop two bevy `World`s becoming two diverging clocks, and
  after Stage 4 there are still three. It stays until (c).
- **The duplication was not two stores holding the same data** — it was two
  stores, *exactly one of them ever populated*, and a three-term branch
  (`vanilla_atlas && net && world_dimensions`) at five read sites to pick. Those
  five had also drifted apart in three ways, one of which (vertical boundary
  light) was a latent Nether bug. Collapsing them deleted the branch; the only
  thing it genuinely encoded survives as one bool, `MeshPolicy::id_spaces_agree`.
- **One negative control had to be pinned *away* from the unified store.**
  `collide_against_live_world = false` reproduced "a live session colliding
  against the offline world it does not have". With one store that becomes "collide
  against the server's real terrain through the demo classifier", where every
  non-air vanilla id reads as solid — the control would have stopped failing while
  still looking correct. It now names an explicitly empty store.
- **The stage's *reported* blockers were mostly not blocked on it.** The block
  selection box needed no chunk-store change at all (`SharedHandle` +
  `ClientHandle::block_at` + a `VersionAdapter` were already `'static` and
  `Send + Sync`), and `CollisionSource` was the wrong seam for it — outline and
  collision are different vanilla shape families and half of all 26.2 states
  disagree. Item physics was already unblocked by Stage 2, as
  [`local-player-components.md`](./local-player-components.md) said. The one thing
  Stage 4 does unblock outright is a `'static` spatial store for anything that
  needs to read blocks off the frame thread.

**Moves:** `Arc<RwLock<lodestone_world::World>>` → a `Resource` (§4.1(d)). `Sim.scheduler`
(`MeshScheduler`), `dirty_columns` (`sim.rs`), `pending_removals` (`sim.rs`),
`vanilla_atlas`, `mesh_drops` (`sim.rs`) → resources plus `Update` systems that enqueue and
drain.

**Stays:** chunks are **not** entities. The worker pool stays a worker pool; systems only enqueue
and drain it. `lodestone-worldgen` stays a version-free interpreter called by the integrated
server — it is verified block-for-block against real server chunks, and making it systems would put
a scheduler between the oracle and the code it validates.

**Authority test:** the world resource is the only chunk store; `Sim` holds no world.

**Verified by:** live chunk-load gates; `mesh_drops` staying `0` in a healthy session
(`sim.rs` — a non-zero value is the one-line diagnosis for this defect class); water not
growing a falling wall at chunk borders, which is the `dirty_columns` coalescing still working
(`sim.rs`).

---

### Stage 5 — `Sim` is deleted

**Partly landed — `Sim` is not deleted.** See
[`sim-dissolution.md`](./sim-dissolution.md) for the field-by-field record. 28 fields
before, **15** after; `Sim::step` is still the driver loop. Seven places the plan (or
the brief handed to the stage) was wrong:

- **The authority test as written is unreachable, for the same reason Stage 3's was.**
  "`struct Sim` no longer exists" requires one owner driving one `App`, and `Sim` owns
  *two* bevy `World`s (its own and `EntityInterpolator`'s). That is §4.1**(c)**.
  Thirteen of the fifteen surviving fields would move without (c); the two that would
  not are `ecs` (a `World` cannot be a resource in itself) and `entity_interp`
  (nesting a `World` inside a `World` compiles and unifies nothing).
- **The blocker Stage 2 recorded for `Mining`/`Placement` was the wrong one.** It
  named `Sim.target`, `version_data`, the live block store and the particle emitter as
  "Stage 3/4 residents". Re-checked one at a time: all four were free — three were
  plain owned values with no cross-`World` dependency, and the store stopped mattering
  at Stage 4. The actual blocker was that `drive_mining` reached the client through
  `&NetClient`, and `NetClient` holds an `mpsc::Receiver`, which is **`!Sync`** and so
  can never be a `Resource`. No earlier stage could have changed that, and no later one
  needs to: every read on `NetClient` bar `poll()` already delegates to
  `SharedHandle`, which is `Send + Sync + 'static`. Both are now `TickSet::Send`
  systems.
- **`vanilla_atlas` is listed under Stage 4's "Moves" and Stage 4 did not move it.**
  It is still a `Sim` field, and it is the `is_live()` discriminant read at ~8 sites.
- **Two of the fields the plan names do not exist.** `input` and `fly` moved in
  Stage 2 (`RawInput`, `Flying`); `audio` and `language` are still there, and
  `ShellAudio` is `Send + Sync` — measured with a scratch `need<T: Send + Sync>()`
  probe, not assumed, because "a rodio engine must be `!Send`" was the obvious guess
  and it is wrong.
- **`step_realtime` had zero callers anywhere in the tree.** Deleted, along with
  `last_step`, the only `Instant` on `Sim`. A `pub fn` in a lib+bin crate that nothing
  calls is an island by the repo's own definition.
- **`snapshot_section` / `snapshot_section_live` are not `Sim` state and block
  nothing.** The brief listed them as surviving "only as thin wrappers". They are thin
  adapters over `snapshot_section_in`, but `snapshot_section` has six callers (two in
  `gpu.rs`, four in `mesher.rs`'s own tests) and `snapshot_section_live` one
  (`tests/live_world_mesh.rs`). Deleting them buys no authority and churns a live gate.
- **There are two 20 Hz accumulators driving two `GameTick` schedules, and the
  divergence is unbounded.** `Sim::step` banks `dt.clamp(0.0, 0.25)` (five ticks);
  `EntityInterpolator` banks the pacer-clamped `dt` unclamped (up to ten). A maximal
  stall therefore advances item physics five ticks further than player physics, the
  excess real time is discarded rather than reconciled, and `end_session` resets one
  accumulator and not the other. The `f32`-vs-`f64` term is real but ~1.5e-8 relative —
  one tick per ~39 days — and is not the mechanism. Unifying needs (c) (one `GameTick`
  is one `World`'s schedule) *and* a decision about which catch-up policy is right,
  since the interpolator's ten ticks is the one that matches
  [`frame-pacing.md`](./frame-pacing.md). Consequence for §6: a plugin adding a
  `GameTick` system today picks not just which `App` but which **clock**.

**Moves:** whatever `Sim` still holds — `input`, `stats`, `target`, `clock_secs`, `particles`,
`audio`, `language`, `asset_banner`, `interp_alpha`, `tick_count`, `frame_count`, `fly`.
`Sim::step` (`sim/step.rs`) becomes the four schedules; `App::redraw` becomes `app.update()` then
`renderer.draw(extracted)`.

**Stays:** `lodestone-render` bevy-free (§4.4). `lodestone-particle`, `lodestone-audio`,
`lodestone-assets` stay plain libraries behind resources.

**Authority test:** `struct Sim` no longer exists. `sim.rs` is 3950 lines today; at the end of this
stage it is gone.

**Verified by:** `cargo xtask check-connected`; the screenshot/pixel gates; `cargo check --workspace
--all-targets` and `cargo test --workspace --no-fail-fast` (**not** `-p`, which fail-fasts and has
misled twice).

---

### Stage 6 — the async bot tier

**Moves:** `SharedState` (`state.rs`) and `ClientHandle`'s query methods →
`ClientHandle { world: Arc<RwLock<bevy World>>, entity: Entity }`, reimplemented over the ECS —
azalea's `Client` (`azalea-client/src/client.rs`, `azalea/src/bot.rs:82-144`). `wait_for(pred)`
becomes "await a tick broadcast, then re-check the World", which is azalea's `wait_ticks` /
`get_tick_broadcaster` (`bot.rs:102-128`).

**Stays:** `connect`, `send_action`, and the typed `ClientEvent` stream — `DESIGN.md:521`'s
"async connect, a typed event stream, and an action API" is unchanged in shape.

**Authority test:** no read-model outside the ECS remains anywhere in the tree.

**Two sources of truth:** none by this point — `Inner` died in Stage 3. This stage is API surface
only, which is exactly why it is last: it has the most test churn
(`crates/lodestone-client/tests/read_model.rs`) and produces no new pixel, so doing it early buys
nothing and risks stalling the migration in the middle.

**Verified by:** `read_model.rs` ported and green; a headless bot example that connects, walks, and
mines with no window — which also proves the headless `Runner` arm from Stage 0 is real and not an
untested branch.

---

## 8. What should NOT move

Be opinionated here; the wrong answer costs an oracle.

| stays a plain library | why |
|---|---|
| **`lodestone-physics`** | bit-exact vs a JVM oracle, pinned by golden traces. A system calls it; the math never becomes a system. If the integrator moves, the traces stop being an external oracle. |
| **`lodestone-worldgen`** | version-free interpreter, verified block-for-block against real server chunks. Same argument. |
| **the codec / `protocol/v*`** | sans-IO by design and version-specific. A version crate must never be a bevy plugin (§5). |
| **`lodestone-render`** | in the wasm set; adding bevy makes it a public dependency of the GPU layer. Extract systems live in the shell (§4.4). |
| **`lodestone-world` chunk storage** | copy-on-write `Arc<ChunkSection>` snapshots exist so a mesher can grab and release the lock. A chunk is not an entity; azalea agrees (§2.3). |
| **`lodestone-game` folds** | `Scoreboard`/`TabList`/`Menus`/crafting are pure functions over `ClientEvent`. One implementation, called from one system. |
| **`lodestone-entity` pose/anim** | vanilla constants and `WalkAnimation`; their unit tests are the only pin. |
| **`lodestone-net` transport** | owns the socket and the tokio runtime on its own thread (§4.1(a)). |

The pattern: **anything with an external oracle stays a library the ECS calls.** The ECS owns
*state and scheduling*, never *verified math*.

---

## 9. Cost, and the strongest argument against doing it

### 9.1 Cost

Sized against the real line counts: `sim.rs` 3950 lines / 40+ fields, `event.rs` 1624 (unchanged —
`ClientEvent` remains the ingest vocabulary, which is a large part of why this is tractable),
`entities.rs` 1204, `state.rs` 880.

| stage | estimate | risk |
|---|---|---|
| 0 App + wasm proof + `WorldTime` | 1–2 d | **go/no-go**: is `bevy_ecs` wasm-clean |
| 1 entities | 4–6 d | test port volume; the `Option<Option<T>>` gotcha |
| 2 player + physics intent | 4–5 d | **highest** — bit-exact gates |
| 3 HUD/session | 3–4 d | mechanical; deletes a real bug |
| 4 world + meshing | 4–5 d | live chunk gates, mesher lock discipline |
| 5 `Sim` deletion | 3–4 d | broad but shallow |
| 6 async tier | 3–4 d | `lodestone-client` test churn |

**~22–30 focused days, call it 4–6 weeks.** Then be honest about the tail: this repo's own record
says the tail is longer than the estimate, and a six-stage migration is six chances to land a stage
whose consumer is not wired. Budget the tail explicitly rather than discovering it.

### 9.2 The strongest argument against

*Every concrete capability a plugin needs already exists, and bevy is not what supplies it.*

"Run code at a defined point in the tick", "read entity state", "send an action" are all available
today from `ClientHandle` + `ClientEvent` + `ClientAction` — a version-free, wasm-clean,
already-tested API. What `bevy_ecs` adds is exactly two things:

1. **Ordering against internal systems by name.** Real, and obtainable for ~200 lines with a
   `HookPoint` enum and a `Vec<Box<dyn Hook>>`.
2. **Mutating internal state in place.** This is the genuine difference — and it is also the
   liability. Every component a plugin can touch becomes public API. `Position` cannot be renamed
   without breaking plugins. And you would be pinning that public plugin ABI to a fast-moving
   upstream: bevy 0.17 renamed buffered `Event` → `Message`/`MessageWriter`/`add_message` (all over
   azalea's current code), and azalea still carries a stale `# TODO: in bevy 0.18, we should be able
   to…` at `azalea-client/Cargo.toml:40-42` while pinned to 0.19. Context7 only has bevy_ecs 0.16
   docs, which is itself a churn signal.

Three more costs worth weighing:

- **The migration is paid where the repo is most fragile.** Stage 2 touches the one subsystem whose
  correctness rests on an external oracle that a refactor cannot re-derive.
- **wasm bundle.** `bevy_reflect` is default-on, and the browser client is a stated goal. Measurable
  at Stage 0; do measure it.
- **`add_plugins` is unsandboxed trust, and it is compile-time.** If what is actually wanted is
  "users install extensions without rebuilding", this migration does not deliver it (§6.1) and the
  WASM host does.

### 9.3 The rejected narrower alternative

**ECS for entities only** — Stages 0 and 1, stop. ~1 week, 1 go/no-go risk instead of 6, no physics
exposure, and it collapses the three-copy entity pipeline, which is the single largest concrete win
available. A plugin could observe and mutate mobs and add extract systems.

**Rejected**, on the user's explicit decision ("whole-codebase migration is fine, think about long
term. we need plugins to be able to do everything that native can"). It is rejected on the
requirement, not the cost: entities-only leaves the local player, physics intent, HUD and the
session in `Sim`, so a plugin could not affect what the server is told, could not add a HUD element,
and could not run between physics and packet send. That fails the requirement outright, and
retrofitting the rest later is *more* total work than doing it in sequence, because Stage 1's
extract boundary would be built against a `Sim` that has to be dismantled anyway.

Recorded here because the cost delta is real and worth knowing if the requirement ever softens.

---

## 10. Configuration

- `bevy_ecs = { version = "0.19", default-features = false, features = ["std"] }` in
  `[workspace.dependencies]`; add `bevy_app` at the same version. Never `multi_threaded` (§3.1).
- `scripts/wasm-check.sh`: add `lodestone-ecs` to `CRATES` (`:85-111`). No new confinement rule is
  needed unless `bevy_ecs` turns out to reach for `std::fs` / `Instant::now` on a path we hit — if
  it does, that is a Stage 0 finding, not a Stage 5 surprise.
- `cargo xtask check-connected`: no allowlist entry for `lodestone-ecs`. Its going red is the island
  detector working.
- Tests: `ScheduleBuildSettings { ambiguity_detection: LogLevel::Error }` (§Stage 3).
- `--features live` remains the only version selector; nothing in this plan changes that, and
  nothing in it lets the shell name a version crate.

## 11. Dependencies

- **New:** `bevy_ecs` 0.19, `bevy_app` 0.19 (both MIT/Apache-2.0), and `parking_lot` for the World
  lock (azalea uses it for the same purpose).
- **New crate:** `lodestone-ecs` — schedules, sets, core components/resources, the `Runner` seam,
  the plugin prelude. Depends on `bevy_app`, `bevy_ecs`, `lodestone-model`, `lodestone-world`.
  Depends on **no** version crate, ever.
- **Unchanged:** `lodestone-model` stays version-free; the codec stays sans-IO;
  `lodestone-physics` / `lodestone-worldgen` / `lodestone-render` gain no bevy dependency.
- **Reference:** [azalea](https://github.com/azalea-rs/azalea) (MIT) — read it, do not transliterate
  it. §2.8 and §5 are where we must differ.
