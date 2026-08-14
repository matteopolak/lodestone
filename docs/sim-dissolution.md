# Dissolving `Sim`

> **A separate, purely mechanical split happened alongside this work and is
> not part of it.** `sim.rs` had grown to 10,337 lines, 4,610 of them (45%)
> the inline `mod tests` — one of the most contended files in the repo (69
> commit-touches in 30 days) and a recorded clobber site. The test module
> moved verbatim into `crates/lodestone-shell/src/sim/tests.rs` behind
> `#[cfg(test)] mod tests;` in `sim.rs` — the same pattern `gpu.rs`+`gpu/`,
> `menu.rs`+`menu/` and `hud.rs`+`hud/` already use, and the same one
> `lodestone-model/src/lib.rs`'s own `mod tests;` → `tests.rs` already
> proves at crate scope. No rename to `mod.rs` was needed; Rust resolves
> `mod tests;` inside `src/sim.rs` to `src/sim/tests.rs` on its own.
>
> The move dedents the body by one level (4 spaces), matching
> `lodestone-model/src/tests.rs`'s own convention — verified lossless by
> re-indenting the extracted body and diffing it byte-for-byte against the
> original before writing anything. Test count and content are unchanged;
> only the file boundary moved. This halves the contended file in one
> commit, with a diff shape of one small insertion (`mod tests;`) and one
> large, pure deletion in `sim.rs`, plus a new, previously-uncontended file
> — deliberately the safest available shape, since a large deletion in a
> shared file is the one change pattern that most resembles this repo's
> recorded clobber incidents.
>
> **Seam 2 landed too**, same session: the placement-prediction block
> (`PlacementFacts`, `BlockStates`, `state_for_placement`,
> `predicted_placement_state`, `write_predicted_block`, the orientation/face/
> axis/half property helpers — pure functions and plain data types, zero
> `Sim` state) moved into `sim/placement.rs`, re-exported into `sim`'s own
> namespace so nothing calling them had to change; `predicted_placement_state`/
> `write_predicted_block` specifically as `pub use`, since both are named by
> their original `crate::sim::`/`lodestone::sim::` path from
> `block_entities.rs` and an external integration test. One real complication
> surfaced here that the test-module move never hit: another agent's
> in-flight, uncommitted work (`Sim::difficulty`/`Sim::block_destruction_stage_at`)
> landed directly in the shared `sim.rs` between this
> session's own commits, sitting immediately adjacent to the extraction
> boundary. Committed via the private-index/`commit-tree` route rather than a
> pathspec commit of the whole file, so the split shipped without also
> shipping — or deleting — that other agent's unfinished work; see the
> session's own report for the two stale-index entries that turned up
> alongside it.
>
> **Seam 3 landed too**: the interaction/combat cluster — `break_block`,
> `begin_attack`/`begin_attack_demo`/`begin_attack_live`, `entity_target`,
> `attack_entity`, `maybe_spawn_crit_particles`, `interact_entity`,
> `end_attack`, `use_item`, `end_use`/`end_use_live`, `use_item_live`,
> `use_item_generic`, `placement_facts`, `predict_block`, `place_block`,
> `block_intersects_player` — moved into `sim/actions.rs` as a second
> `impl Sim { .. }` block. Unlike the first two seams this one *is* `Sim`
> state (every item is a `&self`/`&mut self` method), which is exactly why it
> needed no re-export at all: a method call resolves through the `Sim` type
> regardless of which file defines it, so nothing calling `sim.break_block()`
> anywhere else in the tree had to change. The one real wrinkle:
> `sim::actions` is a *child* of `sim` and so already sees `Sim`'s private
> fields (privacy cascades down to descendants, the same rule that let
> `sim::tests` call private methods for free) — but `sim::tests` is a
> *sibling* of `sim::actions`, not its descendant, so three methods
> (`begin_attack_live`, `end_use_live`, `use_item_live`) that were private
> and only ever called from inside the old single `impl Sim` block needed
> bumping to `pub(crate)` once `sim/tests.rs`'s own calls to them crossed a
> module boundary that did not exist before. Caught immediately by
> `cargo check`, not a silent gap.
>
> **Seams 4 through 7 landed too, in a later session, closing the sequence.**
> Each moved as its own commit, each re-derived its line ranges fresh against
> the immediately preceding commit rather than trusting this doc's stale
> numbers (by the time seam 4 started, roughly 6,000 lines had already moved
> out of `sim.rs` today), and each was verified byte-for-byte against the
> exact range removed before being written to the new file.
>
> - **Seam 4** — `poll_net` (the ~66 `NetUpdate::` arms) and `fold_entities`,
>   the two calls `Sim::step` makes right after the tick loop, into
>   `sim/net_apply.rs`. This is the file with the most future contention:
>   adding a `NetUpdate` variant means an arm here. Both methods widened from
>   private to `pub(crate)`: `Sim::step` calls them from `sim.rs` itself,
>   which is `net_apply`'s *parent* — privacy only cascades downward, so a
>   child module's private item is invisible to its parent, the mirror image
>   of seam 3's own wrinkle — and `sim/tests.rs`'s many `sim.poll_net()` calls
>   cross the same sibling boundary seam 3's three methods did.
> - **Seam 5** — the audio cluster (`entity_sound_position`,
>   `set_audio_listener`, `play_block_break_sound`/`play_block_place_sound`/
>   `play_block_surface_sound`, `block_sound_seed`) into `sim/audio.rs`.
>   `entity_sound_position` and the break/place sound pair widened to
>   `pub(crate)` for the same parent/sibling reasons as seam 4;
>   `play_block_surface_sound`/`block_sound_seed` stayed private since their
>   only callers moved here with them.
> - **Seam 6** — the camera cluster (the fog helpers `fog_for_render_distance`/
>   `water_fog`/`lava_fog`, `fog_settings`/`biome_sky_color`,
>   `interpolated_player`, `camera`, `toggle_third_person`, `set_view_bobbing`,
>   `bob_frame`, `render_camera`, `spyglass_scoping`, `third_person_body_state`,
>   plus the `NoCollision` stand-in) into `sim/camera.rs`. The one re-export
>   with a real wrinkle: `fog_for_render_distance` is named by `app.rs` at its
>   full path (`crate::sim::fog_for_render_distance`), and `app.rs` is
>   neither `sim` nor a descendant of it — a plain (private) `use` re-export
>   in `sim.rs`, which is all `sim/tests.rs`'s glob import needs, is *not*
>   enough for a sibling module. That re-export needed `pub(crate)`, matching
>   the item's original visibility.
> - **Seam 7**, closing the sequence — `dirty_sections_for_blocks` and the
>   block-store/re-mesh/reconciliation cluster (`block_at_world`,
>   `set_block_world`, `remesh_around`, `remesh_section`,
>   `remesh_changed_blocks`, `on_column_arrived`, `mark_column_dirty`,
>   `reconcile_predictions`) into `sim/meshing.rs`. `dirty_sections_for_blocks`
>   re-exports `#[cfg(test)]`-gated, unlike every earlier seam's re-export:
>   its only non-test caller (`remesh_changed_blocks`) now lives inside
>   `sim::meshing` itself and needs no re-export, so an unconditional one is
>   dead code — and therefore a warning — in a `--lib`-only build. Five
>   methods widened to `pub(crate)`, same reasoning as every seam above:
>   `block_at_world`/`set_block_world` are called from `sim.rs`'s own
>   `crack_target` (this module's parent now) and from `sim/actions.rs`/
>   `sim/tests.rs`; `remesh_around` from `sim/actions.rs`;
>   `remesh_changed_blocks`/`reconcile_predictions` from `sim/net_apply.rs`'s
>   `poll_net`; `on_column_arrived`/`mark_column_dirty` from both.
>   `remesh_section` stayed private.
>
> Nothing here was entangled enough to stop for. `sim.rs` is now roughly
> 3,200 lines — larger than this doc's own "~1,200" estimate two sections
> down, because the accessor facade (dozens of small `pub fn` reads `app.rs`
> calls once per frame: chat, xp, sidebar, boss bars, the menu snapshot,
> mouse/input handling, `step` itself, `tick_collision`, `tick_nearby_entities`,
> the outline/block-entity/skull/sign/bell render sources, and more) turned
> out bigger than the plan accounted for. What remains is exactly what the
> plan below says should: the struct, lifecycle (`new`/`build`/`connect`/
> `attach_net`/`end_session`), the accessor facade, and `step`.
>
> **The field count in "What is left on `Sim`, and why" below is stale in the
> optimistic direction, and has been since seams 4–7 landed.** It says 15.
> A fresh count against the `Sim` struct definition in `sim.rs`, taken for a later architecture
> review (issue-tracker-independent work, `PlaceIntent`/re-mesh-seam/audio
> triage), found **28** — regrowth had outpaced dissolution, not the other
> way round. None of the 28 are seam-4-through-7 leftovers; they are fields
> added *after* those seams landed, each for a real single-consumer reason
> recorded in its own doc comment: `particle_atlas`, `death_message`, `won`,
> `third_person`, `body_pose`, `eye_height_smoother`, `view_bob`,
> `view_bobbing`, `invert_mouse_x`/`invert_mouse_y`, `toggle_sneak`/
> `toggle_sprint`, `chest_lids`, `pickups` — camera/animation/HUD-adjacent
> state that Stage 5 never looked at, plus two options-menu bools and two
> issue-driven additions (`chest_lids` for chest-lid animation, `pickups` for
> item pickups). **Not
> all rot** — several are the deliberate, documented single-consumer shape
> this doc's own "Not blocked, just not done" table already describes for
> `config`/`stats`/`status` — but the count itself was wrong for as long as
> this doc went unread against the file.
>
> **Seam 8 landed the same session**: `audio: Option<ShellAudio>` moved out
> of the struct entirely into an [`AudioEngine`] resource
> (`crate::sim::AudioEngine`, defined in `sim.rs` just above `impl Sim`),
> bringing the count to **27**. This one *is* a Stage-5-shaped move —
> everything else in "What did **not** become a system, deliberately" and
> the accessor-facade pattern below still describes it — but it landed late
> because nothing needed it to be a resource rather than a private field
> until now: a `GameTick` **system** (a free function over `&mut World`, not
> a `Sim` method) cannot reach a private `Sim` field at all, and
> `f6ab384`'s `PlaceIntent`-blocked note names exactly that gap. `Self::audio`/
> `Self::audio_mut` in `sim.rs` (beside `Self::mining`/`Self::terrain`) read
> the resource under the guard, so every existing call site in
> `sim/audio.rs` and `sim/net_apply.rs` kept its shape — only the two direct
> `&self.audio`/`&mut self.audio` field reads changed. New:
> `Sim::play_local_sound` (`sim/audio.rs`), the public, non-networked play
> path — the direct motivation, since `crate::interact::drive_placement`'s
> plugin-driven placement sound and `app.rs`'s recorded `RainAmbience`
> island (`app.rs`'s `WindowApp::weather` doc: "no producer, because the
> only `ShellAudio` in the process is a private field on `Sim` with no
> public play method") both need to play a sound from somewhere that is not
> a `NetUpdate` arm.
>
> **The `RainAmbience` island itself is still open.** This move removes the
> blocker its own doc named, but does not wire the producer: driving
> `lodestone_render::RainAmbience::tick` needs a heightmap "landing" sample
> and a "roof above the player" check, and — checked directly, not assumed —
> **nothing in the tree reads `lodestone_world::LoadedChunk::heightmaps` at
> all** (`grep -rn heightmaps crates/lodestone-client crates/lodestone-world`
> finds only the storage type and its own tests). That accessor would be new
> work in `net.rs` (a chunk-lock-only read, the same shape as
> `NetHandle::block_at`), and the per-frame tick call belongs in `app.rs`'s
> `WindowApp::redraw`, both files owned elsewhere in this session. Left
> named rather than built around, same as the original gap.
>
> **Seams 9–13 landed together, a later session, and they retire the "closing
> the sequence" claim above.** `sim.rs` was still 3,420 lines — and the two
> paragraphs above are the reason to distrust a finished-sounding seam log:
> seam 7's entry says it "clos[ed] the sequence" and that what remained "is
> exactly what the plan below says should", while `sim/meshing.rs`'s own module
> doc still calls itself "the last of the sim.rs decomposition sequence". Both
> were true when written. Neither is now, and `sim/meshing.rs` is deliberately
> **left as it stands** — this split was a pure move, and editing a
> neighbour's prose is not part of one. The correction lives here, plus a note
> in each of the five new files, so a reader arriving through `meshing.rs` is
> not misled.
>
> The split was mechanical, not architectural: five whole line ranges copied
> verbatim into new files, no renames, no signature changes, no reordering.
> `sim.rs` **3,420 → 1,221 lines**.
>
> - **Seam 9** — construction (`new`, `with_demo_world`, `build`) into
>   `sim/build.rs`. 311 lines, deliberately under the 400-line floor the rest
>   aim for: `build` is the single most contended function in the file, since
>   every new plugin, resource, worker pool or spawn-time component set adds a
>   line there and nowhere else. Nothing widened — `build`'s only callers are
>   its two siblings in the same file.
> - **Seam 10** — the session lifecycle *and* the session scalars, into
>   `sim/session.rs` (875 lines): `connect`/`attach_net`/`end_session`, the
>   phase accessors, the death/respawn/win latches, and every HUD-facing read
>   folded by the net thread (health, food, saturation, air, xp, tab list,
>   scoreboard, boss bars, the three overlays, attack-strength cooldown, the
>   folded menus, hotbar selection, difficulty). The two halves stay together
>   because `end_session`'s doc is a hand-written list of exactly what a
>   teardown resets, and it is only auditable beside the accessors that would
>   otherwise leak the previous server's values forward — that comment already
>   records one such stale claim it had to correct.
>   `set_phase`, `server_entity_id` and `attack_strength_scale_at` widened to
>   `pub(crate)`; `vitals`, `attack_strength_delay` and `send_selected_slot`
>   stayed private (all callers in-file).
> - **Seam 11** — the collision seam into `sim/collide.rs` (320 lines):
>   `tick_collision`, `tick_nearby_entities`, `item_collision`,
>   `live_collision`, `is_live`, `fluid_state`/`set_fluid_state`, and
>   `NEARBY_ENTITY_RADIUS`. Named `collide.rs`, **not** `collision.rs`: a
>   `mod collision;` in `sim.rs` would make the bare path `collision::` mean
>   `sim::collision` for every reader of the root, shadowing
>   `crate::collision` — it compiles and misleads. Seven methods widened.
> - **Seam 12** — the per-frame driver into `sim/step.rs` (546 lines): `step`
>   itself, `apply_mouse`, the mouse/toggle option pushes, the mesh drains,
>   `drain_action_queue`, the swing pair, `update_target`/
>   `update_entity_target`, `tick_count` and `refresh_stats`. Kept as one file
>   because the frame's *ordering* is load-bearing in three places its own
>   comments record (`Update` before the tick loop so `advance_interp_clocks`
>   runs first; the walk-bob inputs captured before the tick's movement; the
>   swing clock ticking before the queue drains, a deliberate one-tick offset
>   from vanilla) and none of that is checkable across three files.
>   `drain_action_queue`/`update_entity_target` widened for `sim/tests.rs`.
> - **Seam 13** — what the renderer pulls out of `Sim` each frame, into
>   `sim/render_sources.rs` (430 lines): the outline/block-entity/skull/sign/
>   bell sampler closures, `chest_lid_count`, `entity_draws`,
>   `crack_target`/`crack_targets`, and the particle
>   `tick_particles`/`extract_particles`/`particle_instances` trio. One shape
>   throughout: hand out either a `'static + Send + Sync` closure that captures
>   an owned `SharedHandle` and re-samples per frame, or owned data — never a
>   `World` guard that escapes into a GPU upload. `tick_particles` widened.
>
> **What the root deliberately kept, and why it is not an omission.** `sim.rs`
> still holds the struct and its field docs, the two `CollisionSource`
> adapters, the free helpers — and **the whole lock-scoped accessor layer**
> (`read`, `write`, `write_local`, the per-resource accessors,
> `refresh_mesh_policy`, `adopt_live_world`, the local-player/physics-intent
> reads, `translator`/`resolve_text`). Privacy cascades **downward**: a
> parent's private item is visible to every descendant, while a child's is
> invisible to its siblings. So leaving that layer in the root widened
> *nothing*, whereas moving it out would have made `read`/`write`
> `pub(crate)` and put this crate's entire lock discipline —
> `EcsHandle`'s three rules, including "never call into `NetClient` with a
> guard live" — onto the crate-internal surface. The three-line
> `chunk_collision` stayed for a sharper version of the same reason: it
> returns `Arc<ChunkWorldCollision>`, so widening it while its return type
> stayed private would have tripped `private_interfaces`.
>
> **How it was verified**, since a 2,200-line move is unreviewable by reading:
> a program (not a shell pipeline — `diff | grep -c` has reported 0 in this
> repo when the true figure was ~15,000) compared the *multiset* of source
> lines in `sim.rs` at `HEAD` against the union of the new root plus the five
> new files. Two passes: code-only (non-blank, non-comment) and all-non-blank
> including every comment. Both reported **zero unexplained removals**; the
> only additions were `use super::*;`/`impl Sim {`/`}`/`mod` wrappers and the
> new header prose. The gate carries its own control — deleting one real line
> makes it name that line — because an emptiness check with no control is
> exactly the vacuous shape `CLAUDE.md`'s evidence rules warn about. Test
> counts: the lib-test binary ran **1,117** tests with 50 ignored and 121
> `sim::` tests both before and after, across 69 binaries, so nothing stopped
> being compiled.

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
| `audio: Option<ShellAudio>` (seam 8, later session) | `AudioEngine` resource | `lodestone-shell/src/sim.rs` (type + `Self::audio`/`Self::audio_mut` accessors), `sim/audio.rs` (call sites + new `Self::play_local_sound`) |

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

**`Send + Sync` was necessary and not sufficient, and the gap froze the client.** A
handle that a system can *hold* is not a handle a system can *call*: most of
`ClientHandle`'s read-model accessors take a read guard on the very `World` the
schedule is running inside, and `drive_mining` called one (`player_menu`) for the
held item — a silent deadlock on the first tick of the first dig. `NetHandle` now
exposes only the **chunk**-backed `block_at` (`get()` is private), and the held item
comes off the `SessionMenus` component instead. See
[`world-unification.md`](./world-unification.md)'s lock-discipline section for the
incident and the reentrancy tripwire that now aborts on it, and
`crates/lodestone-shell/tests/mining_deadlock.rs` for the gate.

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
| `audio: Option<ShellAudio>` | **Moved** — seam 8, see the note at the top of this doc. Was `Send + Sync` (measured). Now `AudioEngine`, a resource. |

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
the feet whenever a non-standing pose was active (`Avatar.POSES`: `0.4`
swimming, `1.27` crouching, `1.62` standing). The eye height is now a parameter and
`Sim::camera` passes `interp.eye_height`; the three `camera_rig` unit tests pass
`PLAYER_EYE_HEIGHT` explicitly. The swim-camera gate's number is unchanged, which is
the point.
