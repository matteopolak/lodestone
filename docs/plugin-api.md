# The plugin API

## What it is

The surface a third-party bevy plugin uses to do everything native Lodestone code can do — read
world/entity/player state, write intent, order systems against internal ones, and observe events —
specified against `docs/bevy-migration.md`'s six-stage plan. This is a specification, not an
implementation: Stages 0–3 are landed (`crates/lodestone-ecs`, `415138f`, `8be6544`, `beae37c`,
`b2baf02`); Stages 4–6 are not. (Stages 2 and 3 landed after this document's first draft — see
"Two items below that landed after this document was written" for what that changed and, just as
important, what it did not.) Read this before building any of them — several requirements below are
things a stage must
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
  or `LookIntent` component exists") is now half true, not fully true — see that
  item for the precise remaining half.
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
- **What is still genuinely missing, unchanged:** `RawPacket`/raw-event
  observation (`grep -rn RawPacket crates` is still empty) and `LookIntent` (see
  the second gap item below). The chunk world is still off-ECS (Stage 4, in
  progress elsewhere as of this writing).

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

**1. `TickSet::Intent` ordering anchor — still missing.**
`crates/lodestone-ecs/src/sets.rs` currently defines `TickSet` as exactly `Input, Physics, Predict,
Animate, Send` — five variants, no `Intent`. Two systems both writing movement intent inside
`TickSet::Input` (human input, plus a hypothetical navigator) is an ordering ambiguity, and
`docs/bevy-migration.md`'s Stage 3 intends `ScheduleBuildSettings { ambiguity_detection:
LogLevel::Error }` in tests — so this is not a style nit, it is something that will fail a build the
day two writers exist. **Unaddressed by `67ff7c3`**, which touched only collision/item/armour data,
not `lodestone-ecs`.

**2. Analog `MovementIntent`, plus a `LookIntent` distinct from the camera — still missing.**
No `MovementIntent` or `LookIntent` component exists anywhere in `crates/lodestone-ecs/src/` today
(`grep -rn "MovementIntent\|LookIntent" crates/lodestone-ecs` is empty) — expected, since these are
Stage 2 deliverables and Stage 2 has not landed. The gap `docs/baritone-port.md` §7.2 describes is
confirmed current: `crates/lodestone-controller/src/input.rs:45-46` defines
`InputState { forward: bool, … }` — digital, not analog — and `Sim::physics_tick`
(`crates/lodestone-shell/src/sim.rs:1385`) is a private method, so nothing outside `lodestone-shell`
can drive it even off-ECS. A plugin steering a bot (§2.2(1) and §2.3 of `docs/baritone-port.md`
explain why overshoot correction needs sub-integer forward/strafe) has no route to analog input at
all today.

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
| exists today | Stages 0–3 (entity, local-player and session/HUD read/write, plus the `ActionQueue` egress); chunk world and beyond not started | not started — no crate, no design doc yet |

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
| `MovementIntent` (analog), `LookIntent` | Stage 2 | **`MovementIntent` done, `LookIntent` still missing** — landed partly with `beae37c`; see the correction note above. `LookIntent` distinct from the camera is unaffected by this and remains a real gap |
| `TickSet::Intent` ordering anchor | Stage 2 (recommended) | **gap — re-verified against `crates/lodestone-ecs/src/sets.rs` directly: `TickSet` is still exactly `Input, Physics, Predict, Animate, Send`, no `Intent` variant** |
| health/hunger/effects/inventory/tab-list/scoreboard as components | Stage 3 | **done** — landed as `b2baf02`, after this row was written; see the correction note above and [`docs/session-components.md`](./session-components.md) |
| exactly-one-writer ambiguity gate (`ambiguity_detection: Error`) | Stage 3 | **done** — `crates/lodestone-ecs/src/session.rs:681` |
| chunk world as a `Resource` with batched snapshot reads | Stage 4 | not started |
| `SendAction` message / `MessageWriter<SendAction>` egress | unassigned | **closed under a different shape** — `player.rs`'s `ActionQueue(Vec<ClientAction>)` resource landed with Stage 2 and is reachable from a plugin system via `ResMut<ActionQueue>`; see the correction note above. Not a bevy `Message`, so the ordering/observability a `MessageWriter` gives for free is still absent — recorded here as a design question, not a completeness gap |
| raw-packet observation (`RawPacket` message) | unassigned | **gap — re-verified: `grep -rn RawPacket crates` is still empty** |
| `Sim` deleted; plugin no longer reaches into shell internals | Stage 5 | not started |
| async bot tier / headless plugin host | Stage 6 | not started |
| world-space debug-geometry `Extract` channel | unassigned | **gap — re-verified against `crates/lodestone-ecs/src/sets.rs` directly: `ExtractSet` is still exactly `Terrain, Entities, Hud`, no debug/overlay set** |
| block physics constants (friction/speed/jump/bounce/stuck/climbable) reachable without depending on `lodestone-shell` | unassigned | **closed in `24af787`** — `lodestone_model::block_physics(&str) -> BlockPhysics`; see §3 above |
| `PathType` per state through the seam | unassigned | **gap — `docs/baritone-port.md` §3.3 named this in the original document; still true today** |

**The gap list, restated as the single most useful output of this document:** at the time this table
was first written, four items had **no stage that claims them** at all in `docs/bevy-migration.md`'s
§7. One of those four — the block-physics constants — closed in `24af787`, as a stage-independent fix
exactly as recommended below, not by acquiring a stage. The other three remain open and re-verified
current, directly against `crates/lodestone-ecs/src/sets.rs` for the two ordering-anchor gaps: the
`TickSet::Intent` anchor, the `SendAction`/`RawPacket` messages (though `SendAction`'s underlying
capability now exists in a different shape — a plain resource, not a message — via `ActionQueue`, per
the correction note above), and the `Extract` debug-geometry channel. Each is small in isolation (an
enum variant, a message type, a `Vec` resource drained per frame). None is *hard* — the risk is
exactly the one `CLAUDE.md` names as the dominant defect class here: a stage lands, its authority test
passes, and one of these is quietly never added because no stage's checklist named it. **Recommendation
(unchanged for the three still open):** fold the `TickSet::Intent` anchor into whatever stage adds
`LookIntent`, and fold the debug-geometry channel into Stage 4 or 5 (it wants the chunk-world resource
to be useful for anything spatial).

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
  directly (legal — a plugin is a leaf crate — at the cost of version-locking itself, per §3).
- `cargo xtask check-connected` is the island detector for this surface, same as for the rest of the
  migration: `lodestone-ecs` is deliberately **not** allowlisted, so a stage that lands a component set
  with no consumer shows up as red rather than silently shipping an island (`CLAUDE.md`'s rule 1;
  `crates/lodestone-ecs/src/lib.rs`'s own doc comment says the same).

## See also

- [`docs/bevy-migration.md`](./bevy-migration.md) — the six-stage plan this document specifies
  against; §6 and §6.1 are the plugin-API and trust-tier sections this doc expands.
- [`docs/entity-components.md`](./entity-components.md) — Stage 1's actual output: the component set
  a plugin can read/write today, and the `'static`-borrow blocker this document's §"two Stage-1
  constraints" section cites.
- [`docs/baritone-port.md`](./baritone-port.md) — the concrete plugin this API is being sized against;
  its §7 is the requirements list this document verifies and closes gaps against, and its §3.2/§3.3
  are the source of the `block_facts` finding in §3 above.
