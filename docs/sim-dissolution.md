# Dissolving `Sim`

## What it is

`lodestone_shell::sim::Sim` is the shell's god object: the non-graphical game state
plus the driver loop that advances it. [`docs/bevy-migration.md`](./bevy-migration.md)
Stage 5 is its deletion.

**It is not deleted.** This doc is the record of what Stage 5 moved, what it could
not, and *why* — field by field, because the "why" is the audit of Stages 1–4 and is
worth more than the deletion would have been.

Score: **28 fields before, 15 after.** `Sim::step` still exists and is still the
driver loop.

---

## What moved, and where

| was | is now | lives in |
|---|---|---|
| `clock_secs`, `accumulator`, `interp_alpha`, `tick_count`, `frame_count` | `FrameClock` resource | `lodestone-ecs/src/resources.rs` |
| `chat_log` | `SessionChat` component on the local player | `lodestone-ecs/src/session.rs` |
| `ChatLog` (the type) | `lodestone_game::chat::ChatLog` | `lodestone-game/src/chat.rs` |
| `version_data` | `VersionData` resource (§4.3) | `lodestone-ecs/src/resources.rs` |
| `target` | `RayTarget` resource | `lodestone-shell/src/interact.rs` |
| `particles` | `ParticleSim` resource | `lodestone-shell/src/interact.rs` |
| `mining`, `placement`, `attacking` | `MiningPredictor` / `PlacementPredictor` / `Attacking` resources | `lodestone-shell/src/interact.rs` |
| `send_is_sprinting_if_needed()` | `send_sprint_command`, a `TickSet::Send` system | `lodestone-shell/src/interact.rs` |
| `drive_mining()` | `drive_mining`, a `TickSet::Send` system | `lodestone-shell/src/interact.rs` |
| `drive_interaction()` | **deleted** — the `Egress` resource is the gate it used to write by hand | — |
| `last_step` + `step_realtime()` | **deleted** — `step_realtime` had zero callers anywhere in the tree | — |

### The clock and the chat log had to move together

Stage 3 deferred `chat_log` to Stage 5 explicitly, and the reason holds up: every
push stamps a line with a monotonic client clock and every read needs the same clock
again to compute an age for the vanilla fade-out. A component while the clock stayed
a `Sim` field would have put a *second* clock in the process — the exact failure the
authority test exists to catch.

So `FrameClock` came first, and the log followed as `SessionChat`. Two things fell
out of doing it this way:

- `ChatLog` had to leave `lodestone-shell` (→ `lodestone-game`), because
  `lodestone-ecs` cannot depend on the shell. It sits beside the `ChatFeed` it wraps.
- The **age projection moved with it** as `ChatLog::recent_ages(n, now)`. It used to
  be three lines inline in `Sim::recent_chat`, including the `.max(0.0)` guard
  against a backwards clock. That guard now has its own test
  (`a_line_stamped_in_the_future_reads_as_age_zero`) instead of being an
  unremarked-on expression.

### What actually blocked `Mining` / `Placement` — and it was not what the plan said

Stage 2's report listed the blockers as `Sim.target`, `version_data`, the live block
store, the particle emitter and direct demo-world edits: "Stage 3/4 residents, so a
system now would need them mirrored into resources".

**Three of those four were never blockers.** `Sim.target`, `version_data` and the
particle emitter were plain owned values with no cross-`World` dependency; any stage
could have made them resources. The live block store stopped being a blocker at
Stage 4. Re-checking them one at a time — which is what the brief asked for — found
each of them free.

The real blocker was one line down: `drive_mining` reached the client through
`&NetClient`, and

```rust
pub struct NetClient {
    rx: Receiver<NetUpdate>,   // std::sync::mpsc::Receiver: Send, NOT Sync
    ...
}
```

`Receiver` is `!Sync`, so `NetClient` can never be a bevy `Resource`. Nothing about
Stages 3 or 4 would have changed that.

The fix is not to move `NetClient`. **Every read on it except `poll()` is already a
delegation to `SharedHandle = Arc<OnceLock<Arc<ClientHandle>>>`**, which is
`Send + Sync + 'static` — `chunk-world-resource.md` had already noticed this for the
block-outline source. So `interact.rs` adds a `NetHandle(Option<SharedHandle>)`
resource holding *the same* `Arc` the net thread publishes into, and the two systems
read the client through that. It is not a second copy of anything.

### Wire order is unchanged, and that took care

Before Stage 5 the per-tick order was:

```
run_schedule(GameTick)   → send_move_action, send_player_input queue into ActionQueue
drain_action_queue()     → those reach the socket
tick_particles()
drive_interaction()      → sprint edge + mining, sent DIRECTLY via net.send_action
```

After:

```
run_schedule(GameTick)   → send_move_action, send_player_input,
                           send_sprint_command, drive_mining  — all into ActionQueue
drain_action_queue()     → all of them reach the socket, in that order
tick_particles()
```

Same bytes in the same order. Two traps avoided:

1. **The systems queue into `ActionQueue`; they do not call
   `ClientHandle::send_action`.** `ClientHandle::send_action` exists and would have
   been one line shorter, but it bypasses the net thread's action channel — so a
   mining packet could overtake the movement packet queued microseconds earlier.
   One ordered egress or none.
2. **`.after(send_player_input)` is explicit.** `add_systems` gives no ordering from
   registration order. The server derives its sneak state from the player-input
   packet, so a `use_item_on` or mining `START` that overtook it would be judged
   against the previous tick's crouch.

The gate moved from the call site (`if phase == Connected && is_live()`) into each
system (`if !(egress.in_world && egress.live) { return; }`). Equivalent in
production, and *stricter* in the right direction: gating inside is what stops
`LastSprintingSent` latching a value as "already sent" while disconnected, which is
the identical hazard `send_player_input` documents.

### What did **not** become a system, deliberately

`tick_particles` stayed a `Sim` method reading the `ParticleSim` resource. The state
moved (so the authority test passes) but the tick did not, because the particle
collision decision is **not** the player's:

| case | `tick_collision` (player) | `tick_particles` |
|---|---|---|
| live, column not streamed | `PlayerCollision::Pending` — hold the player | falls back to the chunk store |
| `collide_against_live_world = false` | an explicitly **empty** store | the real chunk store |

Reusing the per-tick `PlayerCollision` resource would silently change both. Making
it a system needs a second per-tick collision resource with its own documented
decision, which is a behaviour question, not this stage's.

---

## What is left on `Sim`, and why

15 fields. Exactly **two** are blocked by something real; the other thirteen are
unfinished mechanical work with nothing in their way.

### Genuinely blocked

| field | blocker | what would free it |
|---|---|---|
| `net: Option<NetClient>` | `NetClient` holds `std::sync::mpsc::Receiver`, which is `!Sync`, so it cannot be a `Resource`. **Not an earlier stage's fault** — it is a type property. | Either split `poll()`'s receiver out behind a `Mutex`/crossbeam channel, or make ingest a `NetIngest` system reading a `Sync` channel, or `insert_non_send_resource` (legal for a single-threaded driver, but an escape hatch that would need justifying). Touches `net.rs`. |
| `entity_interp: EntityInterpolator` | It holds **its own bevy `World`**. It *is* `Send + Sync + 'static`, so it could be a resource today — but a `World` nested inside a `World` unifies nothing and would freeze the two-clock defect below in place. | §4.1**(c)**. |
| `ecs: EcsWorld` | This *is* the `World`. A `World` cannot be a resource in itself. | Nothing frees it: deleting `Sim` means `WindowApp` holds the `App` and every `Sim` method becomes a system or a free function over `&mut World`. That is the shape of the rest of Stage 5. |

Measured, not assumed: a scratch `fn need<T: Send + Sync + 'static>()` probe over
`ShellAudio`, `EntityInterpolator`, `Config`, `DebugStats`, `BlockAtlas` and
`Language` compiled clean. `ShellAudio` in particular is fine — the guess that a
rodio-backed engine would be `!Send` is wrong here. **`NetClient` is the only
`!Sync` thing on `Sim`.**

### Not blocked, just not done

| field | note |
|---|---|
| `config: Config` | ~20 read sites. Purely mechanical. |
| `stats: DebugStats` | A per-frame *output* buffer that `app.rs` also writes into. |
| `local: Entity` | Wants to be a `LocalPlayerEntity` resource in the same `World`. |
| `adopted_live_world: bool` | One bit; belongs beside `ChunkWorld` in `lodestone-ecs`. |
| `status: String` | **Already duplicated**: `refresh_stats` copies it into `stats.status` every frame. Two copies of one string, which is the §1.1 smell in miniature. Collapse to one. |
| `vanilla_atlas: Option<Arc<BlockAtlas>>` | The plan lists this under **Stage 4's** "Moves". Stage 4 did not move it. It is the live/demo discriminant behind `is_live()`, read at ~8 sites. |
| `language: Option<Arc<Language>>` | Asset, `Arc`, free. |
| `teleport_count`, `collide_against_live_world`, `recover_from_death` | Three diagnostics/test switches. `collide_against_live_world` in particular must be a resource before `tick_collision` can be a system. |
| `asset_banner: Option<String>` | Asset, free. |
| `audio: Option<ShellAudio>` | `Send + Sync` (measured). Free. |

### Was §4.1(c) required?

> **(c) has since landed** — [`world-unification.md`](./world-unification.md).
> `entity_interp` is deleted, `ecs` is the shared `EcsHandle`, and `Sim` is at 14
> fields. `net` is still the one genuinely blocked survivor.


**For deleting `Sim` outright: yes.** Not because of the state — thirteen of the
fifteen remaining fields would move without it — but because `Sim` owns the driver's
`World` *and* the interpolator's, and "delete `Sim`" means one owner drives one
`App`. Nesting the second `World` in a resource of the first would satisfy the
compiler and nothing else.

**For everything Stage 5 actually did: no.** The chat log, the clock, the version
adapter, the pick target, the particle emitter and both interaction predictors moved
with the three `World`s exactly where Stage 4 left them.

---

## Two `GameTick` schedules on two clocks

> **Resolved by §4.1(c).** There is one `World`, one `GameTick`, one accumulator
> (`FrameClock`) and one catch-up policy (ten ticks — vanilla's, not the `0.25 s`
> this section measured). `TickAccum` is deleted and `end_session` resets the one
> accumulator. Everything below is the *measurement* that forced the decision, kept
> because the mechanism (a clamp mismatch, not float width) is the interesting part;
> see [`world-unification.md`](./world-unification.md) for what shipped, including
> which clamp won and why.

Investigated as part of this stage. **There are two independent 20 Hz accumulators
driving two separate `GameTick` schedules**, and they are not in lockstep.

| | the player's clock | the interpolator's clock |
|---|---|---|
| where | `FrameClock::accumulator` (`f64`), `sim.rs` `Sim::step` | `TickAccum` (`f32`), `entities.rs` `EntityInterpolator::update_with_view` |
| period | `TICK_DT: f64 = 1.0 / 20.0` | `TICK_SECONDS: f32 = 0.05` |
| carries | player physics, movement intent, egress, HUD overlay ageing | `tick_item_physics`, `tick_walk_animation` |
| fed | `dt.clamp(0.0, 0.25)` | `dt` **unclamped** (`as f32`) |

### The dominant term is the clamp, not the float width

`FramePacer::begin_frame` already clamps `dt` to `MAX_CATCHUP_SECS = 0.5 s`
(vanilla's ten ticks). `Sim::step` then clamps *again* to `0.25 s` — five ticks —
before banking it. `update_entities(dt as f32)` is handed the pacer-clamped value,
so:

> **On a maximal stall the player clock gains 0.25 s and the entity clock gains
> 0.5 s: a five-tick divergence, per stall, cumulative and unbounded.** The excess
> real time is *discarded* by the player clock, so nothing ever brings them back
> into agreement.

The `f32`-vs-`f64` term is real but four orders of magnitude smaller.
`0.05f32 = 0.050000000745…` against `1.0/20.0 = 0.050000000000…` is a relative
error of ~1.5e-8, i.e. **one tick of drift per ~39 days of continuous play**. It is
not the mechanism.

There is a third, separate source: `Sim::end_session` replaces `entity_interp`
wholesale (resetting `TickAccum` to zero) and does **not** reset the player clock's
accumulator, so a quit-to-title re-phases the two arbitrarily.

### It cannot be unified inside Stage 5

`world.run_schedule(GameTick)` runs the systems in *that* `World`'s `Schedules`
resource. Two `World`s therefore have two independent `GameTick` schedules and two
accumulators, necessarily. **One `GameTick` needs one `World`: this is §4.1(c) and
nothing less.**

Deliberately **not** done: passing the interpolator the same `dt.clamp(0.0, 0.25)`.
It is one line and would remove the unbounded term, leaving only sub-tick phase —
but two schedules that *mostly* agree are harder to reason about than two that
obviously do not, and it would bury the finding rather than fix it.

### Which clamp is even correct is an open question

Note that the *interpolator* is the one that matches
[`docs/frame-pacing.md`](./frame-pacing.md) and vanilla's `MAX_TICKS_PER_UPDATE`
(ten ticks). `Sim::step`'s extra `0.25 s` is tighter than both, and `app.rs`'s own
pacing test says so out loud:

> `"sim.rs's inner 0.25 s clamp is expected to bind before app.rs's"` — measured
> **5** catch-up ticks, not 10.

So unifying the two clocks forces a decision about catch-up policy. That is a
behaviour change with a live-gate consequence, not a refactor, and it belongs with
(c) rather than smuggled in beside it.

### Consequence for the plugin API, today

A plugin adding a `GameTick` system must pick not just *which `App`* but **which
clock** — and the two disagree about catch-up policy and are re-phased independently
by a quit-to-title. That is not a documentable quirk; it is a reason the plugin ABI
is not finished until (c) lands.

---

## How to change it

- **Adding a per-tick live interaction:** a system in `TickSet::Send` in
  `interact.rs`, queueing into `ActionQueue`. Never `ClientHandle::send_action` from
  a system (ordering, above).
- **Adding a per-*frame* one** (a click handler): `ActionQueue` is drained only
  inside the tick loop, so a frame that runs no tick does not drain it — a queued
  click can wait up to 50 ms. `Sim::{end_attack, use_item_live, send_chat,
  close_open_menu, send_selected_slot}` therefore still send through `NetClient`
  directly. Changing that is a latency change, not a refactor; vanilla handles input
  in the tick, so it may well be the right change, but it needs measuring.
- **Moving one of the thirteen unblocked fields:** insert the resource in
  `Sim::build`, add a private accessor next to the other Stage-5 accessors in
  `sim.rs` (`clock`, `target`, `particles`, `mining`), delete the field, follow the
  compiler. `end_session` is the place that gets missed — prefer putting session
  state in `insert_hud_components`' bundle, which is reset as a set, over adding
  another line to `end_session`'s hand-written list.
- **`ParticleSim` cannot come from a plugin.** Like the mesh worker pool, the
  emitter must be built with the sprite table for whichever block-id space the
  session's world holds; `InteractPlugin` deliberately does not `init_resource` it.
- **`InteractPlugin` asserts `ControllerPlugin` is present rather than adding it.**
  `add_systems` does not deduplicate — Stage 3 shipped a total ingest blackout that
  way — so a plugin must never add another plugin's systems on its behalf.

## Configuration

Nothing new. `--features live` is still the only version selector; `VersionData`'s
`None` arm *is* the no-family-compiled-in build, and its consumers refuse to act
rather than substituting a default.

## Dependencies

- `lodestone-game` gains `ChatLog` (no new external dependency — it already owned
  `ChatFeed`).
- `lodestone-ecs` gains `FrameClock` and `VersionData` in `resources.rs`, and
  `SessionChat` in `session.rs`. No new crate dependency: `VersionAdapter` comes
  from `lodestone-model`, which was already there.
- `lodestone-shell` gains `interact.rs`. No new crate dependency.
- `camera_rig::build_camera` gained an explicit `eye_height: f32` parameter — see
  [below](#a-note-on-camera_rigbuild_camera).

## A note on `camera_rig::build_camera`

Folded into this change because `Sim::camera` survives Stage 5 and was the only
caller. `build_camera` hardcoded the standing
`PLAYER_EYE_HEIGHT`, so the swimming work had pre-biased the *feet* `Y` by
`eye_height - PLAYER_EYE_HEIGHT` at the call site. Arithmetically identical — the
camera consumes `position` solely as the eye — but the argument named `feet` was not
the feet whenever a non-standing pose was active (`Avatar.java:22-36`: `0.4`
swimming, `1.27` crouching, `1.62` standing). The eye height is now a parameter and
`Sim::camera` passes `interp.eye_height`; the three `camera_rig` unit tests pass
`PLAYER_EYE_HEIGHT` explicitly. The swim-camera gate's number is unchanged, which is
the point.
