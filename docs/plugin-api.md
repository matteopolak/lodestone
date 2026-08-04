# The plugin API

## What it is

The surface a third-party bevy plugin uses to do everything native Lodestone code can do — read
world/entity/player state, write intent, order systems against internal ones, and observe events —
specified against `docs/bevy-migration.md`'s six-stage plan. This is a specification, not an
implementation: Stages 0–4 are landed (`crates/lodestone-ecs`, `415138f`, `8be6544`, `beae37c`,
`b2baf02`, and the chunk-world resource of Stage 4 — see the stage-map table below for its commit);
Stages 5–6 are not. (Stages 2, 3 and 4 each landed after this document's first draft — see "Correction:
Stages 2 and 3 landed after the paragraphs above were written" below for what changed and, just as
important, what it did not; Stage 4's own landing is recorded in the stage-map table rather than a
second correction section, since by then it was one row, not three paragraphs of prose.) Read this
before building any of them — several requirements below are things a stage must
deliver, and finding that out at Baritone time (`docs/baritone-port.md`) would mean reopening
finished stages.

**The driving requirement, verbatim from the owner:** "we need plugins to be able to do everything
that native can." `docs/bevy-migration.md` §1 makes the architectural consequence explicit — the ECS
must be *authoritative* state, not a projection of it — and that consequence is why this document
exists ahead of Stages 3–6 rather than after them: every stage decides what becomes reachable, and a
stage that lands without an ordering anchor, a component, or a seam a plugin needs has to be reopened
to add it.

## How it works

### The surface: read, write, schedule, intercept

A plugin is `impl bevy_app::Plugin`, added with `App::add_plugins`. `lodestone-ecs` re-exports
`bevy_app` and `bevy_ecs` as `lodestone_ecs::{app, ecs}` so a plugin author never has to match
versions by hand (`crates/lodestone-ecs/src/lib.rs:47-50`) — the same trick azalea uses
(`azalea/src/lib.rs:63-64`).

**Schedule and set labels a plugin orders against today** (`crates/lodestone-ecs/src/{schedules,sets}.rs`):

| schedule | cadence | public sets, in order |
|---|---|---|
| `NetIngest` | once per driver iteration | `IngestSet::{Drain, Apply, Index}` |
| `GameTick` | 20 Hz, ≤10 catch-up | `TickSet::{Input, Physics, Predict, Animate, Send}` |
| `Update` (bevy's own) | per frame | `FrameSet::{Input, Interpolate, Camera}` |
| `Extract` | per frame, last | `ExtractSet::{Terrain, Entities, Hud}` |

These are re-exported from `lodestone_ecs` and are the plugin ABI's ordering anchors. Per
`docs/bevy-migration.md` §2.6, anchors are **sets, not system functions** — azalea offers both and a
system function it later renames breaks every plugin that named it; a set survives internal
refactors. `sets.rs`'s own doc comment states this as policy.

**Components a plugin can read and write today** — Stage 1's output, `crates/lodestone-ecs/src/entity.rs`:
`MinecraftEntityId`, `EntityUuid`, `EntityKind`, `Position`, `Rotation`, `HeadYaw`, `Velocity`,
`OnGround`, `EntityFlags`, `CustomName`, `CustomNameVisible`, `Pose`, `Health`, `Baby`, `Variant`,
`Attributes`, `Equipment`, `DisplayItem`, plus the `EntityIndex` resource (server entity id →
`bevy_ecs::Entity`). These are non-player entities only — mobs, dropped items, projectiles. Writing
`Position` on a tracked mob moves it on screen next `Extract`; see
[`docs/entity-components.md`](./entity-components.md) for the three-state `Reported<T>` encoding
that governs when a component should be absent versus present-with-`None`.

**Resources:** `WorldTime { age, time_of_day }` (`crates/lodestone-ecs/src/resources.rs`) is the only
one Stage 0 delivered. Everything else a plugin will eventually want — the local player, the chunk
world, HUD/session state — is still off-ECS, in `lodestone-shell::Sim` and
`lodestone_client::state::Inner`, per §5 below.

**How a plugin observes events:** not yet built. `NetIngest`'s `IngestSet::Apply` systems fold
`ClientEvent`s into components today, but there is no bevy `Message`/`MessageWriter` a plugin can
read to observe the raw event stream — `docs/bevy-migration.md` §5.1 proposes a `RawPacket` message
type (`state: ConnectionState, id: i32, payload: Arc<[u8]>`, version-opaque) for exactly this, and
it is unbuilt. Until it lands, a plugin can only observe *effects* (component changes) via its own
`Query`, never the *event* that caused them.

**How a plugin injects intent:** also not yet built, and this is the motivating case named in this
document's brief — a plugin driving the player. `docs/bevy-migration.md` §6 specifies
`MessageWriter<SendAction>` carrying a `lodestone_model::ClientAction` as the one sanctioned egress,
analogous to azalea's `SendGamePacketEvent`. No `SendAction` message type exists in
`crates/lodestone-ecs/src/` today (`grep -rn SendAction crates/lodestone-ecs` is empty). The only
egress that exists right now is off-ECS: `lodestone_client::ClientHandle::send_action`
(`crates/lodestone-client/src/handle.rs:69`) and `lodestone_shell::net::NetClient::send_action`
(`crates/lodestone-shell/src/net.rs:413`), both of which take a `ClientAction` directly and both of
which predate the ECS entirely. A plugin cannot reach either from inside a system today because
neither is a bevy resource — this is one of the concrete Stage 2/6 deliverables (§4 below).

### Correction: Stages 2 and 3 landed after the paragraphs above were written

The three paragraphs above (components, resources, event/intent) describe the
tree as it stood after Stage 1 (`8be6544`), which was current when this document
was first drafted (`0b0facf`). `beae37c` (Stage 2) and `b2baf02` (Stage 3) landed
immediately after, in the same twelve-commit run, and moved several of the
"not yet built" claims above. Re-verified directly against the tree rather than
assumed from the stage numbering:

- **The local player is components now.** `crates/lodestone-ecs/src/player.rs`
  has `PhysicsState`, `MovementIntent(MovementInput)`, `Submersion`,
  `PrevPosition`, `Flying`, `SelectedSlot`, `LastPlayerInput`, `Dead` on the
  `LocalPlayer` entity, plus a `TickSet::Physics` system that advances them by
  calling `lodestone_physics` (the integrator itself stayed a plain library, per
  §8). See [`docs/local-player-components.md`](./local-player-components.md).
  `MovementIntent` existing means the second gap item below ("no `MovementIntent`
  or `LookIntent` component exists") was half true when this correction was
  first written — **it is now fully closed**, `LookIntent` included; see that
  item, updated below, rather than this bullet.
- **The sanctioned egress exists, as a resource rather than a message.**
  `player.rs`'s `ActionQueue(pub Vec<ClientAction>)` is `app.init_resource`'d
  (`player.rs:530`) and drained every tick by the driver
  (`lodestone-shell/src/sim.rs:1697`, `resource_mut::<ActionQueue>()`). A plugin
  system can push a `ClientAction` onto it via `ResMut<ActionQueue>` today — the
  capability `docs/bevy-migration.md` §6 asked for (`MessageWriter<SendAction>`)
  exists under a different shape (a plain `Vec` resource, not a bevy `Message`),
  which is close enough that "no egress reachable from inside a system" is no
  longer accurate. Whether that shape is the one to keep, or whether it should
  still become a `Message` for the ordering/observability a `MessageWriter` gives
  for free, is open.
- **Session/HUD state is components too.** `crates/lodestone-ecs/src/session.rs`
  holds the scoreboard/tab-list/boss-bar/menu/health/food/experience/phase fold,
  with the `ambiguity_detection: LogLevel::Error` gate `docs/bevy-migration.md`
  §Stage 3 asked for (`session.rs:681`). See
  [`docs/session-components.md`](./session-components.md).
- **What is still genuinely missing:** `RawPacket`/raw-event observation
  (`grep -rn RawPacket crates` is still empty — re-verified for this pass, not
  assumed). `LookIntent` and the chunk world (Stage 4) were the other two items
  this bullet used to list as open; both landed since — `LookIntent` in `0d82ab4`
  (see the second gap item below, corrected) and the chunk world as
  [`lodestone_ecs::ChunkWorld`](../crates/lodestone-ecs/src/chunks.rs) (see the
  stage-map table further down, also corrected).

This correction is deliberately narrow — full paragraph-by-paragraph rewrites of
the material above, and of the stage-map table further down, are a larger pass
than this note; treat the bullets here as the authoritative current state where
they overlap with the prose above, and the prose above as Stage-1-era history.

### What stays privileged, and why

Two things are off-limits **by construction**, not by a permission check a plugin could route
around:

- **Version types.** `packet_id`, wire codecs, and every `protocol/v*` type never cross into
  `lodestone-ecs`, `lodestone-model`, `lodestone-world`, `lodestone-render`, or `lodestone-shell`
  (`docs/bevy-migration.md` §5). A plugin *can* still reach version data — by depending on
  `lodestone-v770` itself, since a plugin is a leaf crate — but doing so version-locks it. §5.3 below
  is the seam that is supposed to make that unnecessary for the common case, and §5.3 also records
  exactly how much of that seam exists today.
- **The GPU device, queue, and pipelines.** `lodestone-render` carries no bevy dependency
  (`docs/bevy-migration.md` §4.4, confirmed unchanged in the current tree — `lodestone-render`'s
  `Cargo.toml` has no `bevy_ecs`/`bevy_app` line) and is never in the ECS. A plugin that wants to draw
  gets an `Extract`-time channel to append to (§4.6 below, itself a gap today), never a
  `wgpu::Device`. The 4-bind-group floor and the winding-sign invariant
  (`CLAUDE.md`'s rendering constraints) are constraints a plugin author cannot be expected to satisfy
  correctly, so they stay behind the renderer's own API on purpose.

**By policy, nothing is off-limits**, and that is the tension worth naming plainly rather than
softening: **a compiled-in bevy plugin is fully trusted code with no sandbox.** `add_plugins` is
`dlopen`-equivalent trust — a plugin can call `std::fs::remove_dir_all("/")`, open a socket to
anywhere, or `std::process::Command::new("rm")`. There is no capability check that could be added
here without contradicting the requirement itself, because the requirement *is* native-equivalent
power, and native code has always had all of that. `docs/bevy-migration.md` §6.1 says this must be
"repeated in the public docs" and it is repeated here for the same reason.

**The second, quieter tension:** "everything native can do" reads to most people as "install
something without touching the build." A bevy plugin is `impl Plugin` compiled into the binary.
"Install a plugin" means "add a `Cargo.toml` dependency and rebuild." If what is actually wanted is
users dropping a `.so`/`.wasm` file into a folder, this tier does not deliver that at all — see §6.

### The four concrete gaps, verified against the current tree

`docs/baritone-port.md` §7 named four gaps as prerequisites for a Baritone-class plugin. All four were
re-checked directly against the tree for this document, not assumed from the plan.

**1. `TickSet::Intent` ordering anchor — closed in `0d82ab4`.**
This paragraph originally reported `TickSet` as exactly `Input, Physics, Predict, Animate, Send` —
five variants, no `Intent` — which would have made two systems writing movement intent inside
`TickSet::Input` (human input, plus a hypothetical navigator) an unresolvable ordering ambiguity under
`docs/bevy-migration.md` Stage 3's `ScheduleBuildSettings { ambiguity_detection: LogLevel::Error }`.
**Re-verified directly against `crates/lodestone-ecs/src/sets.rs` for this pass: `TickSet` is now six
variants, `Input, Intent, Physics, Predict, Animate, Send`.** `Intent` carries a doc comment naming
exactly this ambiguity-detection rationale, and its own contract test
(`lodestone_controller::ecs::exactly_one_system_writes_movement_intent`) pins "exactly one system
writes `MovementIntent` inside the set; a plugin composes with `.in_set(TickSet::Intent).after(...)`,
or overrides wholesale with `.after(TickSet::Intent)`" — the second form is what
[`crates/plugins/lodestone-autopilot`](../crates/plugins/lodestone-autopilot) uses (see its crate docs)
and is now the first real plugin exercising this anchor.

**2. Analog `MovementIntent`, plus a `LookIntent` distinct from the camera — closed in `0d82ab4`.**
This paragraph originally reported no `MovementIntent` or `LookIntent` component anywhere in
`crates/lodestone-ecs/src/`, and `crates/lodestone-controller/src/input.rs`'s digital-only
`InputState` as the only route in even off-ECS. **Re-verified directly against the tree for this
pass, both closed:**

- `crates/lodestone-ecs/src/player.rs` defines `MovementIntent(pub MovementInput)`, where
  `MovementInput { forward: f32, strafe: f32, jump: bool, sneak: bool, sprint: bool }`
  (`lodestone_physics::player::MovementInput`) — genuinely analog, not clamped to `±1.0` anywhere
  between the component and the integrator: `player_physics` (`TickSet::Physics`) reads
  `let intent = intent.0;` and passes it straight into `tick_among_entities`, the same call human
  input's `MovementIntent` write reaches. A plugin system writing `f32` values here gets the identical
  precision a human's analog stick would (`docs/keybindings.md`'s #219 gamepad-deferral note names the
  same seam for that reason).
- `crates/lodestone-ecs/src/player.rs` also defines `LookIntent { yaw: f32, pitch: f32 }`, distinct
  from the camera by design (its own doc comment walks through why), applied by `apply_look_intent`
  in `TickSet::Intent` before `TickSet::Physics` reads yaw to resolve `MovementInput`'s axes — and
  absent by default, so inserting/removing it is the whole "claim rotation / hand it back" protocol,
  with no handshake needed.

`lodestone_nav::WalkDrive::tick(&PlayerState) -> DriveTick { input: MovementInput, yaw: f32 }` is
built for exactly this pair, and `crates/plugins/lodestone-autopilot`'s `drive_plan` system writes both
components from it every tick — this is no longer a specification, it is exercised by a hermetic test
(`crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs`) that ticks a real `GameTick` schedule
and asserts the local player's `PhysicsState` arrives at a commanded block.

**3. A world-space debug-geometry channel in `Extract` — closed.** This gap is stale as of a later
pass; re-verified directly against the tree rather than assumed from this paragraph's own age.
`ExtractSet` (`crates/lodestone-ecs/src/sets.rs`) now has a fourth variant, `Debug`, and
`crates/lodestone-ecs/src/player.rs` has the resource it guards: `DebugLines(pub Vec<DebugLine>)`,
`init_resource`'d by `LocalPlayerPlugin` and cleared each frame by `clear_debug_lines`
(`.before(ExtractSet::Debug)`) — so a plugin system ordered `.in_set(ExtractSet::Debug)` can push
world-space segments via `ResMut<DebugLines>` exactly the way `ActionQueue` is written to. The render
half exists too: `crates/lodestone-shell/src/gpu.rs`'s `DebugLineRenderer`/`DebugLinesSource`/
`RenderState::set_debug_lines_source`, a real line-list pipeline distinct from the single-box outline
pipeline `docs/baritone-port.md` §7.6 described. The one piece that was missing longest — the actual
install call wiring the ECS resource to the renderer's polled source — is also done now:
`WindowApp::install_debug_lines_source` (`crates/lodestone-shell/src/app.rs`) clones `Sim::ecs()`'s
`EcsHandle` and installs `move || lodestone_ecs::hold_read(&ecs, |world| debug_line_vertices(&world
.resource::<DebugLines>().0))`, called at all three places `install_outline_source` already was
(`begin_singleplayer`, `connect_to`, `resumed`) — though unlike that one, this needs no live
connection at all, since `LocalPlayerPlugin` is on every `Sim`'s one `World` regardless of session
kind. A navigator plugin (`docs/baritone-port.md`) can draw its own planned route today.

**4. `VersionAdapter::block_facts` — the physics-constants half closed in `24af787`; `PathType` is the
remaining gap.** `67ff7c3` added exactly five methods to `crates/lodestone-model/src/adapter.rs`:
`block_collision(state_id) -> Option<&'static [BlockAabb]>`, `block_name(state_id) -> Option<&'static
str>`, `block_outline`, `block_interaction`, and `item_prototype(item: &str) -> Option<ItemPrototype>`.
Verified directly against the trait definition — this closed the *shape* and *name-lookup* half of the
gap: a plugin (or `lodestone-nav`) can get real per-state collision geometry and the block's canonical
name through `VersionAdapter` without depending on a version crate.

At the time this document was first written, the *physics-constants* half was still closed off: the
six name-keyed constants `docs/baritone-port.md` §7.5 asked for lived as six private functions inside
`crates/lodestone-shell/src/collision.rs`, reachable by nothing outside the driver crate. **That has
since changed.** `24af787` moved them to a public function,
`lodestone_model::block_physics(block_name: &str) -> BlockPhysics`
(`crates/lodestone-model/src/adapter.rs`), returning a `BlockPhysics { friction, speed_factor,
jump_factor, bounce_restitution, stuck_multiplier, climbable }` struct — the same six fields, now one
call instead of six private match statements, and callable by anything depending on `lodestone-model`,
which every plugin already does. `collision.rs`'s `physics_at` (`collision.rs:293`) is now a thin
caller of it, not the owner: `v.name_of(...).map_or(DEFAULT_BLOCK_PHYSICS, block_physics)`. This is
still deliberately **not** a `VersionAdapter` method — the data is name-keyed and stable across
versions, not state-keyed, so putting it behind the version seam would be the over-engineering §"how
to change it" below warns against — it is a plain function in the version-free crate a plugin already
depends on, which is exactly where `docs/baritone-port.md` §7.5 wanted it to end up.

**What is still open:** `PathType` per state. `crates/lodestone-data/src/path_types.rs`'s census still
has no `VersionAdapter` method (`lodestone_model::PathTypeRegistry` exists; nothing constructs one
from v770 data) — confirmed still true, unaffected by either `67ff7c3` or `24af787`. That is the one
piece of `docs/baritone-port.md` §7.5's "the important one" gap still without a route for a plugin.

**5. `BreakIntent` — a plugin's wish to mine a block, without a raw `ClientAction` — closed.**
Walking has `MovementIntent`/`LookIntent` (gap 2); mining had nothing equivalent. `ActionQueue`
(the correction note above) lets a plugin push any `ClientAction`, but it must never push
`ClientAction::BlockAction` directly: the block-prediction `sequence` counter, the dig state
machine and the post-break cooldown are owned by `lodestone_shell::interact::MiningPredictor`,
driven by shell-only resources (`Attacking`, the mouse-driven ray target) a plugin cannot reach —
a plugin depends on `lodestone-ecs`, never on `lodestone-shell`. A plugin-synthesised sequence
number would fork the counter, which `docs/baritone-port.md` §3.6 forbids outright ("threaded,
never synthesised").

`crates/lodestone-ecs/src/player.rs` now has `BreakIntent { pos: BlockPos, face: BlockFace }`,
optional and additive exactly like `LookIntent` — absent changes nothing about human play, and a
plugin claims a dig by inserting it on `LocalPlayer` with no other handshake. **While the human
attack button is held, the human path takes priority**, mirroring how a plugin never fights mouse
input for rotation either. `lodestone_shell::interact::drive_mining` consumes it: it resolves the
intent into the identical `RayHit` shape a mouse click produces (casting its own ray through
`VersionData::block_outline`, since a plugin has no crosshair to have already done that), then
runs the *same* `MiningPredictor` pipeline a human dig uses — same counter, same state machine,
same cooldown, never a second implementation that could drift from the first.

**The refusal side is the part worth naming, because it is the one a movement intent does not
need as sharply.** `MovementIntent` degrades gracefully — an intent physics cannot satisfy just
produces less motion. A break intent for an unreachable, obstructed, or unresolvable block has a
binary answer, and silently doing nothing is indistinguishable, to a plugin with no crosshair and
no chat, from "still working on it." So `BreakOutcome(BreakStatus)` is a second, **always-present**
component (`Idle` / `Progressing` / `Rejected(BreakRejection)`) that `drive_mining` writes on every
tick a plugin's intent is (or would be) consulted — `docs/baritone-port.md`'s own "a plan can be
legal, executable, and still stall forever" is exactly the failure mode an unreported rejection
would reproduce at the level of a single edge.

**`PlaceIntent` does not exist yet, and this is a real, checked stop, not an oversight.**
`BreakIntent` was reachable as "an additional input" to an already-`Resource`-shaped system
because `MiningPredictor`, `NetHandle` and `VersionData` are all bevy resources a `GameTick` system
can already read. Placement's equivalent local write —
`lodestone_shell::sim::placement::write_predicted_block` plus the re-mesh that makes it visible
(`Sim::remesh_around`) — is reachable from `ChunkWorld` for the *write* half (a plugin-driven
placement could set the block state), but `remesh_around` reaches into `Sim`'s own mesh-worker
pool and GPU terrain state, which are plain struct fields, not resources, and are not part of
`crates/lodestone-shell/src/interact.rs`'s ownership. Building `PlaceIntent`'s consumer honestly
needs either a new resource exposing "remesh this position" from `sim.rs`/`sim/meshing.rs`, or
accepting a placement that writes the block but never repaints it — neither of which is "add a
system alongside `drive_mining`." That is real restructuring, in files this document's own
authoring pass did not have standing to change; see the issue this gap is tracked against for the
brokered patch.

### Native versus WASM

`docs/bevy-migration.md` §6.1 sets up two tiers and is explicit that neither substitutes for the
other:

| | native bevy plugin | WASM host |
|---|---|---|
| power | everything native code can do | a curated capability ABI: queries + actions |
| trust | fully trusted, no sandbox | untrusted-safe |
| loading | compiled into the binary, `add_plugins` | loaded at runtime |
| filesystem / network | unrestricted | denied unless a capability is granted |
| stability | pinned to `bevy_ecs` 0.19's API; breaks on bevy bumps | Lodestone's own ABI, versioned by Lodestone |
| exists today | Stages 0–4 (entity, local-player and session/HUD read/write, the `ActionQueue` egress, and the chunk world as `lodestone_ecs::ChunkWorld`); Stage 5 (`Sim` deletion) and Stage 6 not started | not started — no crate, no design doc yet |

**What the WASM boundary costs, concretely, using the pathfinder as the stress case.**
`docs/baritone-port.md` §4 is unambiguous that `lodestone-nav`'s search runs on a dedicated OS thread
against an **owned** `Arc<ChunkSection>` snapshot (§4.2, §5.2) — thousands of per-tick collider
queries during a 20,000-node search, ~15,000 `Arc` clones per snapshot, mutable local search state
(an arena, a binary heap) reused across steps. None of that crosses a WASM/host boundary cheaply:

- **Per-tick world queries are the failure case named directly in this brief.** A capability ABI that
  is "queries + actions" implies each `collision_boxes`/`friction`/`state_at` call is a host call —
  a serialization boundary, at minimum a function-table indirection, at worst a copy. §4.4's cost
  derivation alone calls physics-equivalent queries thousands of times per search; doing that through
  a WASM import for every call is the kind of overhead a native plugin's direct `&SnapshotView` never
  pays.
- **A resumable search cannot be "an action."** `Search::step(budget)` (§4.6) needs to persist an
  arena and a heap across many host round-trips, which a stateless capability call does not model
  well — it either means the WASM guest owns that state (fine, if the guest can allocate real memory
  and run real code, which is what WASM is for) or the host owns it and exposes a `step()` capability,
  which reduces to "the host runs the search anyway" and the sandbox buys nothing for this
  particular subsystem.
- **The owned snapshot itself is the thing that makes the search safe to run off-thread at all**
  (`docs/baritone-port.md` §5.1(3), the mesher's rule). Handing an owned `Arc<ChunkSection>` map
  across a WASM boundary means either copying the whole snapshot into guest linear memory (defeats
  the point of `Arc` sharing) or keeping it host-side and paying the per-query cost above.

**Verdict, matching `docs/baritone-port.md` §8's own framing:** `lodestone-nav` is designed as a
plain library precisely so it can be gated headlessly and consumed by a native plugin
(`lodestone-autopilot`) with direct `&SnapshotView` access. **Baritone targets the native tier.** The
WASM host answers a different question — untrusted, hot-loadable automation with a narrower surface
— and is not a fallback for a pathfinder. Both tiers may eventually exist; treating either as a
cheaper substitute for the other is the mistake `docs/bevy-migration.md` warns against.

### Stage map: what each stage delivers, and the gap list

| surface element | delivered by | status |
|---|---|---|
| entity components (mobs, items): read + write | Stage 1 | **done** |
| `NetIngest`/`GameTick`/`Update`/`Extract` schedules and their current sets | Stage 0 | **done** |
| `WorldTime` resource | Stage 0 | **done** |
| local player position/velocity/on-ground/collision as components | Stage 2 | **done** — landed as `beae37c`, after this row was written; see the correction note above and [`docs/local-player-components.md`](./local-player-components.md) |
| `MovementIntent` (analog), `LookIntent` | Stage 2 | **done, both** — closed in `0d82ab4`; see the "four concrete gaps" section above, updated |
| `TickSet::Intent` ordering anchor | Stage 2 (recommended) | **done** — closed in `0d82ab4`; `crates/lodestone-ecs/src/sets.rs`'s `TickSet` is now `Input, Intent, Physics, Predict, Animate, Send`, and `crates/plugins/lodestone-autopilot` is a real plugin ordered against it |
| health/hunger/effects/inventory/tab-list/scoreboard as components | Stage 3 | **done** — landed as `b2baf02`, after this row was written; see the correction note above and [`docs/session-components.md`](./session-components.md) |
| exactly-one-writer ambiguity gate (`ambiguity_detection: Error`) | Stage 3 | **done** — `crates/lodestone-ecs/src/session.rs:681` |
| chunk world as a `Resource` with batched snapshot reads | Stage 4 | **done** — `lodestone_ecs::ChunkWorld` (`crates/lodestone-ecs/src/chunks.rs`), a `Clone`-able handle over one shared `lodestone_world::World`; `crates/plugins/lodestone-autopilot` reads it via `Res<ChunkWorld>` to snapshot a `lodestone_nav::SnapshotView` for search |
| `SendAction` message / `MessageWriter<SendAction>` egress | unassigned | **closed under a different shape** — `player.rs`'s `ActionQueue(Vec<ClientAction>)` resource landed with Stage 2 and is reachable from a plugin system via `ResMut<ActionQueue>`; see the correction note above. Not a bevy `Message`, so the ordering/observability a `MessageWriter` gives for free is still absent — recorded here as a design question, not a completeness gap |
| raw-packet observation (`RawPacket` message) | unassigned | **gap — re-verified: `grep -rn RawPacket crates` is still empty** |
| `Sim` deleted; plugin no longer reaches into shell internals | Stage 5 | not started — `lodestone-shell/src/sim.rs` still exists and still owns plugin registration (`Sim::new`'s `app.add_plugins((...))`), which is why a third-party plugin cannot self-register into the shipped client today: it has to be added to that call by whoever owns `sim.rs` |
| async bot tier / headless plugin host | Stage 6 | not started |
| world-space debug-geometry `Extract` channel | unassigned | **done** — see gap 3 above; this row was left stale (still saying "gap") for a time after gap 3 itself was marked closed, which is its own small instance of `CLAUDE.md`'s "staleness is the most common defect" rule: fixing one paragraph does not fix every other paragraph that restates the same fact |
| block physics constants (friction/speed/jump/bounce/stuck/climbable) reachable without depending on `lodestone-shell` | unassigned | **closed in `24af787`** — `lodestone_model::block_physics(&str) -> BlockPhysics`; see §3 above |
| `PathType` per state through the seam | unassigned | **gap — `docs/baritone-port.md` §3.3 named this in the original document; still true today** |
| `BreakIntent`/`BreakOutcome` (mine-a-block seam, mirroring `MovementIntent`) | unassigned | **done** — `crates/lodestone-ecs/src/player.rs`, consumed by `lodestone_shell::interact::drive_mining`; see gap 5 above |
| `PlaceIntent` (place-a-block seam) | unassigned | **gap, checked rather than assumed** — needs `sim.rs`/`sim/meshing.rs` to expose a remesh-capable resource before a `drive_placement` system can exist without touching `Sim`-only state; see gap 5 above for exactly what is missing |

**The gap list, restated as the single most useful output of this document:** at the time this table
was first written, four items had **no stage that claims them** at all in `docs/bevy-migration.md`'s
§7 — the block-physics constants, the `TickSet::Intent` anchor, the `SendAction`/`RawPacket` messages,
and the `Extract` debug-geometry channel. **Three of the four are closed as of this pass**: the
block-physics constants in `24af787`, and both the `TickSet::Intent` anchor and the debug-geometry
channel in `0d82ab4` (the latter's row above sat stale for a time even after the "four concrete gaps"
section itself was updated — a small, self-contained instance of the exact staleness pattern
`CLAUDE.md` warns is this repo's most common written-record defect). **Only the fourth remains open**,
and even it is narrower than it was: `SendAction`'s underlying capability exists today, in a different
shape — a plain resource, not a message — via `ActionQueue` (see the correction note above), so what
is actually still missing is `RawPacket` (raw-event observation) and the design question of whether
`ActionQueue` should still become a bevy `Message` for the ordering/observability that gives for free.
Neither was ever *hard* — the risk was always the one `CLAUDE.md` names as the dominant defect class
here: a stage lands, its authority test passes, and a small unassigned piece is quietly never added
because no stage's checklist named it. That risk has now materialised on the documentation side
instead, twice: two of these three closures shipped in one commit (`0d82ab4`, 2026-07-29) whose own
message named this document by name as what it was closing, and this document was not updated for six
days until issue #180 forced a pass (this one) — the fix landing is not the same event as the record
catching up to it, and closing a gap in the tree does not automatically close it in a doc that was
written when it was open.

### Two Stage-1 constraints that shape the API, both verified rather than assumed

**A `bevy_ecs::Resource` must be `'static`.** This is bevy's own trait bound
(`Resource: Send + Sync + 'static`, unchanged since well before 0.19), and it is why item physics is
not yet a `TickSet::Physics` system: `docs/entity-components.md`'s "how to change it" section records
that `tick_item_physics` needs a `&dyn CollisionView` and `fold_snapshots` needs a `&[EntitySnapshot]`
— both borrows, neither `'static` — so neither can be smuggled into a system as a resource, and the
workspace's `unsafe_code = "deny"` (below) forecloses the usual escape hatch of transmuting the
lifetime away. The collision source becomes owned (and thus `'static`-able as a resource) only at
`docs/bevy-migration.md` §4.1(d) — the chunk world becoming a `Resource`, Stage 4. **Consequence for
a plugin:** a plugin cannot order a system against item physics, or against anything else built the
same way, until Stage 4 lands, because until then it is not a system at all — it is a plain function
called from inside another system, with no `SystemSet` label to anchor on.

**`unsafe_code = "deny"` is a workspace-wide lint**, set once in the root `Cargo.toml`
(`[workspace.lints.rust] unsafe_code = "deny"`, line 90) and inherited by every crate whose
`Cargo.toml` sets `[lints] workspace = true` — confirmed for `lodestone-ecs`
(`crates/lodestone-ecs/Cargo.toml:8-9`) and for every crate checked in this investigation. This closes
the two usual routes around the `'static` constraint above (`unsafe impl` a shorter-lived type as
`'static`, or hand-roll a raw-pointer resource) for the shipped binary. **It is not a plugin sandbox**
— an external plugin crate sets its own lints and can simply not opt into workspace lints, so this
constrains code that lives in this repository, not third-party plugin crates. Worth stating plainly
alongside §"what stays privileged" above: the deny lint is why the Stage-4 dependency is real
*internally*, not evidence that a plugin is somehow prevented from doing unsafe things generally — it
is not.

## How to change it, and the gotchas

- **Adding a new ordering anchor is additive and safe; renaming or removing one is a plugin-breaking
  change.** Per `docs/bevy-migration.md` §2.6, anchors are sets rather than system functions
  specifically so internal systems can be renamed/split freely — but the *set* itself, once public,
  is the ABI. Treat `TickSet`, `IngestSet`, `FrameSet`, `ExtractSet` variants the way a public API
  treats enum variants: additions are fine, renames need a deprecation window if any plugin exists
  yet to break.
- **Do not add a system-function anchor "just this once."** azalea offers both set-based and
  function-based anchors (`docs/bevy-migration.md` §2.6) and the function-based ones are the ones
  that break when internals move. This repo's stated policy is sets only.
- **A `Resource` you want a plugin to order against must be truly `'static`-owned before it can
  exist.** Check `docs/entity-components.md`'s "two things are not systems yet" note before assuming
  a borrow-shaped subsystem can become a `Resource` — the borrow has to be resolved (own the data) or
  the type has to change, not just add a derive.
- **When closing a version-seam gap, check whether the data is state-keyed or name-keyed before
  choosing where it lives.** The `block_facts` half-fix in §3 above was the cautionary example while it
  was still open: state ids are version-owned by construction (they're renumbered every version, per
  `adapter.rs`'s own doc comments on `block_collision`/`block_hardness`), so state-keyed data belongs
  behind `VersionAdapter`. Name-keyed constants are *not* version-owned, which is exactly why `24af787`
  landed them as `lodestone_model::block_physics` rather than a new `VersionAdapter` method — the
  general rule this bullet states is now also the worked example of someone following it.
- **Verify docs against the actual trait/struct definitions, not the commit message.** `67ff7c3`'s
  message describes "10 [`CollisionView`] methods real" — true, and about `lodestone-shell`'s
  *consumption* of the new adapter methods for the player's own movement. It does not mean the
  adapter methods themselves cover the same six properties; §3 above exists because those are two
  different claims that are easy to conflate.
- **A plugin crate that derives `Resource`/`Component` needs `bevy_ecs` as a direct dependency, not
  only `lodestone-ecs`.** This is not obvious from "re-exported so plugin authors never have to match
  `bevy_app`'s version by hand" (this document's own §"The surface" above) — that line is true for
  every *type* a plugin names, but bevy's derive macros expand to absolute `bevy_ecs::…` paths, which
  only resolve if the crate being compiled has `bevy_ecs` in its own dependency graph under that exact
  name. `lodestone-controller/Cargo.toml` already documents this for an engine crate; measured again
  while building `crates/plugins/lodestone-autopilot` for this pass — `cargo check` failed with
  `cannot find module or crate 'bevy_ecs'` pointing *at* a `#[derive(Resource)]` line until `bevy_ecs`
  and `bevy_app` were added as direct dependencies, pinned to the same `[workspace.dependencies]`
  entry `lodestone-ecs` itself builds against so there is still only one `bevy_ecs` in the graph. Any
  plugin that only reads/writes existing components and resources through `Query`/`Res`/`ResMut` never
  hits this; it is specifically deriving a *new* `Resource` or `Component` type that does.

## Configuration

None yet — there is no plugin-loading mechanism, feature flag, or manifest format to configure,
because a plugin today is just another `Cargo.toml` dependency added with `App::add_plugins`. When
Stage 6 or a WASM host lands, this section should record: how a native plugin crate is declared
(likely a `[workspace.dependencies]` / feature-gated `add_plugins` call in `lodestone-shell`'s driver
setup), and, if a WASM host is built, its manifest/capability-declaration format.

## Dependencies

- `lodestone-ecs` → `bevy_app`, `bevy_ecs` (both `default-features = false, features = ["std"]`, never
  `multi_threaded` — `crates/lodestone-ecs/Cargo.toml`), `parking_lot`, `lodestone-model`, `uuid`.
  Never a version crate (`docs/bevy-migration.md` §5); `cargo xtask check-isolation` (`xtask/src/lib.rs`)
  already exists and enforces protocol-version-crate dependency isolation workspace-wide today, so a
  plugin crate that accidentally pulled a version crate into a shared crate would already be caught —
  the open question is only whether `lodestone-ecs` itself is in its scope, not whether the tool exists.
- A native plugin crate depends on `lodestone-ecs` (for the schedules/sets/components/resources) and,
  if it needs version data unavailable through the seam, may additionally depend on a version crate
  directly (legal — a plugin is a leaf crate — at the cost of version-locking itself, per §3). If it
  derives its own `Resource`/`Component` types, it also needs `bevy_ecs` (and `bevy_app`, if it derives
  `Plugin`-adjacent types or names `App`/`Plugin` itself) as a **direct** dependency — see the "how to
  change it" bullet above; `crates/plugins/lodestone-autopilot/Cargo.toml` is a real example of the
  resulting manifest shape.
- `cargo xtask check-connected` is the island detector for this surface, same as for the rest of the
  migration: `lodestone-ecs` is deliberately **not** allowlisted, so a stage that lands a component set
  with no consumer shows up as red rather than silently shipping an island (`CLAUDE.md`'s rule 1;
  `crates/lodestone-ecs/src/lib.rs`'s own doc comment says the same). As of this pass there is a real
  consumer for the intent seam specifically: `crates/plugins/lodestone-autopilot`, which is not itself
  reachable from the shipped binary yet (`lodestone-shell::sim::Sim::new` does not register it — see
  `docs/autonomous-navigation.md`), so `check-connected` going red for *this* crate until that
  registration lands is the detector working as designed, not a regression to chase.

## Ordering-anchor changelog

Issue #170 asks for "a short `CHANGELOG`-style section... that every PR touching one of \[the
ordering-anchor\] enums is expected to update" — the enforcement mechanism this document's own "how to
change it" section describes only in prose ("additions are fine, renames need a deprecation window").
This section is that changelog. **Every PR that adds, renames or removes a `TickSet`/`IngestSet`/
`FrameSet`/`ExtractSet` variant should add an entry here**, oldest first:

| commit | change | why |
|---|---|---|
| `415138f` (Stage 0) | `IngestSet`, `TickSet`, `FrameSet`, `ExtractSet` all land with their original variant sets — `TickSet` as `Input, Physics, Predict, Animate, Send`, `ExtractSet` as `Terrain, Entities, Hud` | baseline |
| `0d82ab4` | `TickSet` gains `Intent`, between `Input` and `Physics` | give automation-supplied movement intent (a plugin, or a future navigator) a named ordering anchor distinct from raw human input, so two writers of `MovementIntent` become an explicit, checkable order instead of an `ambiguity_detection: Error` build failure — see `sets.rs`'s own doc comment on `TickSet::Intent` |
| `0d82ab4` | `ExtractSet` gains `Debug`, between `Entities` and `Hud` | a world-space debug-geometry channel (`DebugLines`) a plugin can push planned routes/probes into, ordered with the other world-space extracts before the screen-space `Hud` one |

**On `#[non_exhaustive]` (issue #170's other proposed mechanism):** re-checked for this pass —
`grep -rn "match.*\(TickSet\|IngestSet\|FrameSet\|ExtractSet\)" crates/` finds **zero** matches
anywhere in the tree. These enums are consumed exclusively as `bevy_ecs::schedule::SystemSet` labels
(`.in_set(...)`, `.after(...)`, `.before(...)`), never pattern-matched, which is the intended usage
this document's "anchors are sets, not system functions" policy is built around. `#[non_exhaustive]`'s
actual protection — forcing a wildcard arm in an exhaustive external `match` — therefore guards
against a usage pattern that does not occur in this codebase and that a plugin author following the
documented idiom would have no reason to reach for. It also does **not** protect against the
breaking change that actually matters here (renaming or removing a variant, which no attribute
prevents — only review discipline and this changelog do). Given that, `#[non_exhaustive]` is cheap
and not harmful, but its value is narrower than issue #170's framing suggests; see the issue for the
disposition and the patch (outside this document's own file ownership — `sets.rs` is
`crates/lodestone-ecs/`, a different agent's cluster as of this writing).

## See also

- [`docs/bevy-migration.md`](./bevy-migration.md) — the six-stage plan this document specifies
  against; §6 and §6.1 are the plugin-API and trust-tier sections this doc expands.
- [`docs/entity-components.md`](./entity-components.md) — Stage 1's actual output: the component set
  a plugin can read/write today, and the `'static`-borrow blocker this document's §"two Stage-1
  constraints" section cites.
- [`docs/baritone-port.md`](./baritone-port.md) — the concrete plugin this API is being sized against;
  its §7 is the requirements list this document verifies and closes gaps against, and its §3.2/§3.3
  are the source of the `block_facts` finding in §3 above.
- [`docs/autonomous-navigation.md`](./autonomous-navigation.md) — `crates/plugins/lodestone-autopilot`
  as it exists today: what it does (M1, walk-only), how it uses the seams this document specifies
  (`ChunkWorld`, `VersionData`, `TickSet::Intent`, `MovementIntent`/`LookIntent`), and the one thing
  outside its own ownership that stops it reaching the shipped client — registration in
  `Sim::new`.
