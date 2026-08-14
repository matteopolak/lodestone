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

## The doctrine: five clauses, already in force

The driving requirement above is not a wish list — a completed architecture review found the
client is already substantially built to it, and that the rules below were being derived and
re-derived by agents rather than read, because nobody had written them down in one place. This
section is that: five clauses, each checked directly against the source rather than assumed, plus
the two consequences that make this a *doctrine* — a constraint on the whole codebase — rather
than a description of one corner of it.

**1. Wishes are expressed in observation vocabulary, never wire vocabulary.**
`BreakIntent { pos: BlockPos, face: BlockFace }` (`crates/lodestone-ecs/src/player.rs`) is
the two facts a mouse ray hit carries — nothing else. No `sequence`, no dig-state id, no `Hand`, no
raw `ClientAction`. `PlaceIntent { pos, face }` (`crates/lodestone-ecs/src/player.rs`)
mirrors it exactly for placement, down to the same two fields; its own docs state the rule
explicitly ("exactly the two facts a mouse ray hit carries"). A plugin never speaks the packet's
language — it speaks the mouse's.

**2. Exactly one system owns each machine.**
The dig state machine, the prediction `sequence` counter and the post-break cooldown are internal
state of one consumer system, not an API anyone calls. `MiningPredictor(pub Mining)`
and `PlacementPredictor(pub Placement)`
(both `crates/lodestone-shell/src/interact.rs`) are private machines that only `drive_mining`
and `drive_placement`
(same file) touch. A plugin cannot reach either resource — it
depends on `lodestone-ecs`, never on `lodestone-shell` — so there is structurally one writer, not a
convention that could be violated by a second one.

**3. Refusal is always observable.**
`BreakOutcome(pub BreakStatus)` and `PlaceOutcome { status, generation }`
(`crates/lodestone-ecs/src/player.rs`) are *always-present* components —
`spawn_local_player`/`reset_local_player` insert the `Default` on every entity, so a plugin can
poll on the very first tick without first checking whether the shell has ever run with an intent
installed at all. Rejections are typed (`BreakRejection`/`PlaceRejection`,
same file), not a silent no-op. Placement is a
one-shot verb, so `PlaceOutcome::generation` (`player.rs`) is bumped by exactly one every
time `drive_placement` resolves an attempt (`crates/lodestone-shell/src/interact.rs`,
`outcome.generation += 1;`) — the counter a late poller needs to tell "the result of the attempt I
just made" from "an attempt from several ticks ago I never read."

**4. Human input outranks installed intent, per verb, with no handshake.**
`drive_mining` computes `human_attacking = attacking.0 && dead.is_none()`
(`crates/lodestone-shell/src/interact.rs`) and only falls through to a plugin's `BreakIntent`
when that is false. `drive_placement` returns immediately `if using_item.0`
(same file), before even reading `PlaceIntent`. Neither checks
for a plugin and asks it to back off — a real player's own attack/use button always wins, and a
plugin's intent left behind after it stops running simply loses every tick the human is active.
There is no handshake because there is nothing to negotiate: priority is a per-tick predicate, not
a lock.

**5. Lifecycle encodes verb shape.**
A dig is continuous — `BreakIntent` stays installed for the whole multi-tick duration, and *the
plugin* removes it when it wants to stop (`crates/lodestone-ecs/src/player.rs`, "the plugin
removes it itself when it wants to stop"). A placement is one-shot — *the shell* removes
`PlaceIntent` the instant `drive_placement` resolves an attempt
(`crates/lodestone-shell/src/interact.rs`, `commands.entity(entity).remove::<PlaceIntent>();`),
whatever the result, and that removal is itself the acknowledgement: one insertion is one attempt,
so a plugin never has to guess whether a leftover component is still pending or long since
processed.

**The precedent these five clauses generalise from is movement, and it is fully converted, not
partially.** `crates/lodestone-controller/src/ecs.rs`'s
`exactly_one_system_writes_movement_intent` is a contract test, not a unit test — it builds the
real shipped `GameTick` schedule under `ScheduleBuildSettings { ambiguity_detection:
LogLevel::Error }` and asserts the schedule *itself* has no unordered conflicting writer of
`MovementIntent`. Its negative control, `a_second_unordered_intent_writer_fails_the_ambiguity_check`
(same file), adds a rogue second writer with no explicit order and asserts the same build then
*fails* — proof the detector would have caught the thing it exists to catch, not just that the
happy path is quiet. This is the proof that movement is already fully converted to clause 2, and it
is the shape `BreakIntent`/`PlaceIntent` (clauses 1, 3, 4, 5 above) were built to match.

### Two consequences that make this doctrine, not description

**Refusing a capability to plugins refuses it to our own engine too.** Under a bolted-on plugin API
— one written after the fact, on top of an engine that already works some other way — a refusal is
a preference: the internals can always route around their own API when it is inconvenient. Here
that route does not exist, because the plugin surface *is* the internal surface. So when
`docs/plugin-api.md`'s own text above says a plugin "must never push a `ClientAction::BlockAction`
directly," that is not a capability denial aimed at plugins — it is the **single-writer discipline**
clause 2 states, applied uniformly, and it binds native code exactly as hard as it binds a plugin,
because native code reaches the counter through the identical `drive_mining` system. Bukkit has the
same shape for the same reason: a plugin cannot write the server's entity-id counter directly
either — it calls `World.spawnEntity(...)` and the server's own single writer assigns the id. The
sequence-counter refusal here is that same call, not an exception to "plugins can do everything
native can."

**The complete list of genuinely privileged internals is two items.** §"What stays privileged, and
why" below names them: the socket and the driver task that owns it (the wire itself — Bukkit hides
netty from plugins too, for the same reason), and the GPU device/queue/pipelines (a
hardware-constraint firewall — the 4-bind-group floor documented in `CLAUDE.md` is not something a
plugin author can be asked to respect correctly, any more than a Bukkit plugin is asked to respect
OpenGL's own limits). **Everything else in this codebase is single-writer state sitting behind an
intent, reachable by anyone** who inserts the right component or pushes the right resource entry —
which is the point of clauses 1–5 above. This list is asserted complete as of this writing; if a
future pass finds a third thing an internal system can do that no plugin route reaches, the right
frame for that finding is **a defect in the surface**, to be closed the way `BreakIntent`/
`PlaceIntent` closed the mining/placement gap — not a third privileged item to add to this list, and
not policy to defend.

### The half-adopted state: human break/place do not go through the intent seam yet

The five clauses above describe the *plugin* path for breaking and placing blocks. The *human*
path does not use it. `drive_mining`'s `human_attacking` branch reads `Attacking`
and `RayTarget`
(both `crates/lodestone-shell/src/interact.rs`) — both shell-only resources set by mouse input, not
`BreakIntent`. `drive_placement`'s human path is `Sim::use_item_live`
(`crates/lodestone-shell/src/sim/actions.rs`), which runs `Placement::use_on` directly, also
never touching `PlaceIntent`. Both converge one level lower, at the same `MiningPredictor`/
`Placement::use_on` machines a plugin's intent resolves into (clause 2) — so a human dig and a
plugin dig run the identical predictor, but only the plugin one arrives through the observable,
refusable seam.

This is recorded as **the flagship conversion still to do, not as a defect to be quietly worked
around.** It is also a real, named prerequisite rather than a cosmetic gap: today a protection
plugin can veto another *plugin's* dig (by never installing `BreakIntent`, or by racing to remove
it — the mechanisms clauses 3–5 give it), but it has no way to veto a *human* player's dig, because
the human path never asks the intent seam anything. Routing human break/place through `BreakIntent`/
`PlaceIntent` — with human input becoming the *default* producer of the same components a plugin
writes, rather than a separate code path that outranks them — is what would let a plugin cancel a
human verb at all. Until then, clause 4's "human wins" is really "human bypasses," which happens to
look like winning because nothing can contest a path it never joins.

## How it works

### The surface: read, write, schedule, intercept

A plugin is `impl bevy_app::Plugin`, added with `App::add_plugins`. `lodestone-ecs` re-exports
`bevy_app` and `bevy_ecs` as `lodestone_ecs::{app, ecs}` so a plugin author never has to match
versions by hand (`crates/lodestone-ecs/src/lib.rs`'s `pub use bevy_app as app`/`pub use bevy_ecs as ecs`) — the same trick azalea uses
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

**`Attributes` is the one component in this list whose write does not behave like `Position`'s, and
issue #141 asked for exactly this to be verified rather than assumed.** Re-checked directly: the only
write site anywhere in the tree is `ingest.rs`'s `IngestSet::Apply` fold of
`ClientEvent::EntityAttributesUpdated` (`Query<&mut Attributes>`); nothing else writes it, and nothing
reads it back into anything that reaches `ActionQueue` or the wire. This is not a gap to close —
vanilla itself has no client→server attribute-set packet at all (`Attributable.setBaseValue` is a
*server*-side Bukkit API mutating authoritative simulation state; a Java plugin does not write
attributes from the client either). A client plugin writing `Attributes` today would move the local
render/prediction value for one tick until the next server update overwrites it, with no effect on
what the server or any other player sees — the capability the issue asks about belongs on the
*server* side of the client/server split `docs/server-ecs.md` (issue #433) now documents, not here.
It is not designed yet even there: attribute modification would need a server-`World` component a
plugin system can write, and the server-ECS migration's phases (`docs/server-ecs.md`) have not
reached mob/attribute state.

**AI-goal manipulation (issue #141's other half) is blocked the same way, for a different reason.**
`crates/lodestone-entity/src/ai/{goal,goals}.rs` plus `roster/*` is a real, jar-verified goal-selector
system (`docs/mob-goal-roster.md`), so "no AI exists at all" is no longer the blocker — but it is
internal, non-ECS state consumed by `MobSim` on the server, with no component or resource any plugin
(client or server) can reach. Blocked on the same X as the attribute half: a server-side plugin
surface exposing `MobSim`'s goal roster as writable ECS state, which `docs/server-ecs.md`'s migration
has not phased in yet. A disguise/anti-cheat plugin that wants a fake mob with scripted movement is
portable *today*, but only by driving position/rotation directly every tick — not through a goal
selector, because none is plugin-reachable.

**Resources:** `WorldTime { age, time_of_day }` (`crates/lodestone-ecs/src/resources.rs`) is the only
one Stage 0 delivered. Everything else a plugin will eventually want — the local player, the chunk
world, HUD/session state — is still off-ECS, in `lodestone-shell::Sim` and
`lodestone_client::state::Inner`, per §5 below.

**How a plugin observes events:** half built, as of issue #104's `GameEvent` bus. `NetIngest`'s
`IngestSet::Apply` systems still fold `ClientEvent`s into components, and now there is also a bevy
`Message` a plugin can read to observe the stream directly: `lodestone_ecs::GameEvent(pub ClientEvent)`,
written from `lodestone_client::state::SharedState::apply` — one call site, with **no `match` on the
event at all**, so a new `ClientEvent` variant cannot compile with an arm that quietly skips the bus the
way the three routers named in the doctrine above can silently drop one. It is gated off by default
behind the `lodestone_ecs::GameEventBus` marker resource (`GameEventBusPlugin` installs it): a plugin
that never asked for the bus costs nothing extra, not even an additional `EcsHandle` lock, because
`SharedState` checks for the marker once, at construction, rather than on every event. See
[`§5.4 below`](#the-plugin-event-bus-and-cross-plugin-priority-ordering) for the read side —
`EventPriority`'s tiers, `Monitor`'s structural read-only enforcement, and the toy
`crates/plugins/lodestone-event-logger` that exercises the whole pipeline.

`docs/bevy-migration.md` §5.1's other proposal — `RawPacket { state: ConnectionState, id: i32,
payload: Arc<[u8]> }`, the version-*opaque* half that lets a plugin decode a packet type this crate
does not model — remains **unbuilt** (`grep -rn RawPacket crates` is still empty). `GameEvent` is
deliberately not a substitute for it: it carries the same already-decoded, version-*free* vocabulary
`IngestSet::Apply` folds into components, not raw bytes. A plugin that needs the wire form still has
no route but depending on a version crate directly (§3, at the cost of version-locking).

**Decided (issue #156): the packet-interception shape is observation-only, permanently, for both
directions — a `PacketFilter`-style mutate/cancel/inject-at-the-wire trait is rejected outright, not
deferred.** The issue asked to pick between a `RawPacket`-observation-only tier and a heavier
`PacketFilter` trait invoked from inside `handle_packet`. The heavier option cannot be built without
breaking one of two invariants this epic has already spent real cost making load-bearing elsewhere:

- **Inbound.** The net thread applies each event **inline**, under the `World`'s write guard
  (`docs/world-unification.md`'s "Can ingest stall the frame" section). An interceptor that needs
  `&mut World` to decide whether to cancel a packet would be running *inside* that same guard — the
  exact reentrancy shape issue #177 exists to make unrepresentable, not a case to carve an exception
  for. `RawPacket`, once built, must stay observation-only (no verdict, no mutation) for the same
  reason `GameEvent` already is: an observer needs no second guard.
- **Outbound.** Mutating the *encoded* bytes only exists inside a version crate's `VersionAdapter`.
  Offering that as a hook to every plugin would mean the *host* handing a version-typed capability to
  a version-free plugin surface — exactly what `docs/bevy-migration.md` §5 forecloses for shared
  crates (a plugin *depending on* a version crate directly is a different, already-legal thing; the
  host *offering* one to everybody is not). `EgressFilters` (issue #157,
  [`docs/outbound-action-hook.md`](./outbound-action-hook.md)) is therefore the ceiling on the
  outbound side too: version-free `ClientAction` inspect/replace/suppress, never encoded bytes.

**What this actually costs, named per archetype rather than left abstract:** a protection plugin,
economy plugin, minigame manager, world editor and client-side HUD mod need none of this — they are
served by `ActionVetoes` (issue #109, [`docs/cancelable-actions.md`](./cancelable-actions.md), a
pre-check gate at the *verb* level, before the effect commits) and `EgressFilters`, neither of which
needs wire access. **Anti-cheat and server-visible disguise plugins are the honest exception**, and
stay only *partially* portable even once every other issue in this epic lands: real anti-cheat depends
on outbound packet mutation (faking a rubber-band) and low-latency bidirectional raw-byte visibility,
which this decision puts permanently out of reach. A **client-side-only** hologram/disguise (visible
to the local player only, via a local fake entity plus `Extract`-time drawing — issues #138/#140/#161)
stays achievable; a disguise visible to *other real players* needs server-side outbound injection,
which is the same rejected shape. `docs/roadmap/plugin-framework.md`'s port-feasibility table already
carries this finding; this paragraph is what makes it the settled answer rather than an open audit
note.

**Remaining implementation work, now unambiguous in shape:** build `RawPacket` as
observation-only, inbound, off by default (mirroring `GameEventBus`'s opt-in cost model) — the one
piece of #104's original scope `GameEvent` did not close. No further design decision blocks it.

**How a plugin injects intent:** also not yet built, and this is the motivating case named in this
document's brief — a plugin driving the player. `docs/bevy-migration.md` §6 specifies
`MessageWriter<SendAction>` carrying a `lodestone_model::ClientAction` as the one sanctioned egress,
analogous to azalea's `SendGamePacketEvent`. No `SendAction` message type exists in
`crates/lodestone-ecs/src/` today (`grep -rn SendAction crates/lodestone-ecs` is empty). The only
egress that exists right now is off-ECS: `lodestone_client::ClientHandle::send_action`
(`crates/lodestone-client/src/handle.rs`) and `lodestone_shell::net::NetClient::send_action`
(`crates/lodestone-shell/src/net.rs`), both of which take a `ClientAction` directly and both of
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
- **The sanctioned egress exists, as a resource rather than a message — and this
  is now the settled shape, not an open question (issue #181, closed).**
  `player.rs`'s `ActionQueue(pub Vec<ClientAction>)` is `app.init_resource`'d
  and drained every tick by the driver
  (`sim.rs::Sim::drain_action_queue`, `resource_mut::<ActionQueue>()`). A plugin
  system can push a `ClientAction` onto it via `ResMut<ActionQueue>` today — the
  capability `docs/bevy-migration.md` §6 asked for (`MessageWriter<SendAction>`)
  exists under a different shape (a plain `Vec` resource, not a bevy `Message`).
  **`ActionQueue` wins; `SendAction` will not be built.** The migration was
  weighed against the one concrete case that motivated it — the outbound
  interception hook (issue #157) — and lost on that case's own terms, not on
  cost alone: `EgressFilters` (`crates/lodestone-ecs/src/egress.rs`) needed
  synchronous suppression at drain time, and a `MessageWriter<SendAction>`
  structurally cannot deliver that, because a system reading a buffered
  `Message` runs on the next tick's schedule pass — by which point
  `drain_action_queue` has already sent the action (`docs/outbound-action-hook.md`:
  "A `Message` cannot work here... Suppression must be synchronous"). So the
  ordering/observability a `Message` was chosen for turned out to be
  unavailable from a `Message` for this egress shape regardless, which removes
  the one reason migrating was ever worth its cost (every existing consumer and
  test, plus riding a fast-moving bevy API — `docs/bevy-migration.md` §9.2's own
  churn warning). `EgressFilters` gets the same ordering (`priority`, first
  non-`Allow` wins) and the same observability (`names`/`stats`) directly on the
  `Vec`, with no migration. See [`docs/outbound-action-hook.md`](./outbound-action-hook.md)
  and [`docs/cancelable-actions.md`](./cancelable-actions.md) (issue #109) for
  the sibling pre-check veto layer, which answers the same "how does a plugin
  stop an action" question one level earlier, at the verb rather than the
  queue.

  **Still open, and deliberately not resolved by this decision:** `ActionQueue`
  still accepts a raw `ClientAction::UseItemOn`/`::BlockAction` with a
  fabricated `sequence` — the "second door" a plugin could still fork the
  block-prediction counter through. That is a distinct decision (a narrower
  checked enum of plugin-sendable actions, or a separate egress) tracked on its
  own rather than folded into the shape question this section answers.
- **Session/HUD state is components too.** `crates/lodestone-ecs/src/session.rs`
  holds the scoreboard/tab-list/boss-bar/menu/health/food/experience/phase fold,
  with the `ambiguity_detection: LogLevel::Error` gate `docs/bevy-migration.md`
  §Stage 3 asked for, asserted by the `session.rs::net_ingest_is_ambiguous` test. See
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

### Intent components: what shape a producer takes, and where the wall is

Re-verified for issue #436's "no plugin producer exists yet in `crates/plugins/`" ledger entry. The
claim is **true** — the only things that construct a `BreakIntent` or `PlaceIntent` anywhere in the
tree are `lodestone-shell`'s two test harnesses and `lodestone-ecs`' own unit tests — but the
interesting part is *why*, because it is not that the doctrine is the wrong shape.

**The install/remove shape is already proven, and needs no second mechanism.** `lodestone-autopilot`
is a working native producer of the *other* two intents: `drive_plan` mutates `MovementIntent`
in place through its query (the component is always present, inserted at spawn) and claims/releases
`LookIntent` through `Commands` — `insert` while it is steering, `remove::<LookIntent>()` exactly once
on arrival. `BreakIntent`/`PlaceIntent` are the same kind of thing with the same lifecycle, and every
type involved lives in `lodestone-ecs`, which every plugin already depends on. **Nothing structurally
prevents a plugin from producing them today.** So the answer to "does making intents producible require
changing the install/remove doctrine?" is *no, not for native plugins* — the doctrine is a component
lifecycle, and a component lifecycle is exactly what a bevy plugin is good at.

Ordering is the one rule a producer must get right, and `lodestone-autopilot`'s is the idiom to copy:
`.after(TickSet::Intent).before(TickSet::Physics)` for movement, and for break/place anything that runs
before `TickSet::Send`, where both consumers live. Naming the *set* rather than the consuming system is
deliberate — a producer that named `drive_mining` would version-lock itself to the shell.

**And this rule was stated correctly here while a shipped test fixture violated it, which is the
"a rule written in prose is not a rule" shape.** `lodestone-shell/tests/rendered_client_takes_a_plugin.rs`'s
`JumpPlugin` used a bare `.in_set(TickSet::Intent)` with no `.after(…)` — neither sanctioned form — and
its own doc comment claimed the autopilot "does exactly this", which was false. That made it a second
*unordered* writer of `MovementIntent` racing `compute_movement_intent`, and the race resolved
favourably for a while and then stopped. Measured when it did: the plugin's system ran on all 60 ticks
and wrote `jump = true` on all 60, and `MovementIntent.jump` read `false` at `TickSet::Predict` on every
one — so the player never moved and the symptom was *"apex 0.0000, horizontal 0.0000"*, which is equally
consistent with a frozen world, an absent collision view and an unregistered plugin. Two agents
misattributed it to an unrelated commit before the mechanism was measured.

Three things worth carrying from it. **`.in_set(TickSet::Intent)` without an `.after` is the trap**, and
it is easy to write because it reads like "I am an intent writer". **A gate whose failure message cannot
name the mechanism costs a session** — that fixture now asserts the surviving intent *before* the apex,
so a regression says "overwritten on 60 of 60 ticks" instead of a bare zero. And **the shell's composed
`GameTick` is already ambiguous without any plugin** — three conflicting pairs, unrelated to intent — so
a plugin-ordering gate there has to assert a *delta* (this plugin adds zero pairs; a bare-`.in_set` rogue
adds exactly one) rather than demanding a clean schedule, which is what `the_plugins_intent_is_not_a_second_unordered_writer`
does. Those three pairs are why `docs/bevy-migration.md`'s `ambiguity_detection: LogLevel::Error` cannot
simply be switched on for the shell today.

**Where it is genuinely blocked is the ABI, and the dependency wall — not the shape.**

- *WASM guests.* `docs/wasm-plugin-host.md` records that the intent half is absent from the v0.1 ABI,
  and this is the one place install/remove really is the wrong shape: a component handle is not a
  value, so crossing it needs paired ABI calls the host mirrors into real inserts/removes plus an
  outcome poll, rather than the one-crossing-per-tick world that exists. "A guest can chat and swing;
  it cannot yet mine, place or steer." That is an ABI design problem, not an intent design problem.
- *The dependency wall.* The consumers — `drive_mining` and `drive_placement` — live in
  `lodestone-shell`, and **no crate under `crates/plugins/` depends on `lodestone-shell`** (verified:
  zero matches across every plugin manifest). `sim/build.rs` explains why they are stranded there and
  it is not render coupling: `interact.rs` imports fourteen items from `crate::sim`, a cycle with `Sim`
  itself. A plugin can therefore *produce* an intent but cannot integration-test the effect from its
  own crate; an end-to-end gate has to live in `crates/lodestone-shell/tests/`.
- *The declared consumer does not exist yet.* The ledger names "future autopilot mining/bridging
  (M4/M5)". `lodestone-nav` is M1: `MoveKind` is `{Walk, StepUp, Descend, Drop, WalkDiagonal, Climb}`
  with **no `Break` or `Place` variant**, so the search core cannot currently express a plan edge that
  would map to either intent. This island is correctly declared and blocked upstream of itself.

**A separate, undeclared island found while verifying the above: `drive_placement` is registered in no
schedule at all.** `InteractPlugin::build` adds `(send_abilities, send_sprint_command, drive_mining)`
and nothing else; `drive_placement` appears exactly once in `crates/lodestone-shell/src/` — its own
definition — and the only `add_systems` naming it anywhere is `tests/place_intent.rs`'s **hand-built**
`Schedule`. That is the shape issue #467 had: a correct, well-gated subsystem whose gate builds its own
schedule, so a green test suite says nothing about whether production ever runs it. `interact.rs`'s own
module doc says "registers two systems" and lists the two, so the code and the prose agree — nothing
looks wrong on inspection, which is why it survived.

Player-facing impact is nil, and this is worth stating precisely rather than reassuringly: human
right-click placement never went through this system. It goes through `Sim::use_item_live` →
`placement_facts`, a `Sim` method path that `sim/placement.rs`'s module doc contrasts with
`drive_placement` explicitly. What is dead is **only** the plugin `PlaceIntent` path — so a plugin that
inserted a `PlaceIntent` today would watch it sit on the entity forever, unconsumed and un-acknowledged,
while the identical `BreakIntent` worked. Registering it is behaviour-neutral for players: the system
early-returns when no `PlaceIntent` component is present, which is every tick unless a plugin is loaded.

### The plugin event bus and cross-plugin priority ordering

Issues #104/#105/#110, landed together because #105 and #110 both depend on #104's bus existing at
all before either has anything to order.

**Scope: `lodestone-ecs` and `lodestone-client` only, today.** Everything in this section is the
*client's* event surface — `SharedState::apply` is a `lodestone-client` type, and `GameEvent`/
`GameEventBus`/`EventPriority` all live in `lodestone-ecs`, which `lodestone-server` does not depend
on for this. A census of `lodestone-server` taken alongside this pass found **no event bus, no
cancellation, and no hook registration anywhere in it**: `dispatch_play_packet` calls its `apply_*`
helpers inline with no interception point, and every applier is veto-free. So none of the structural
guarantees below (the no-`match` write site, `EventPriority`'s cross-plugin chain, `Monitor`'s
read-only enforcement) currently apply server-side, and nothing here should be read as implying they
do. Per `docs/server-ecs.md` (server-side `bevy_ecs`, decided against issue #433) the two sides are
converging on the same substrate — plugins are meant to be ordinary bevy plugins on *either* side, and
core game systems (physics is the worked example: a client-side plugin a headless bot can omit)
should themselves become plugins where that makes sense, which is what would let a Java/Paper
compatibility layer be just another external plugin on this same public API rather than a second
shape. That convergence is not built yet; a server-side event bus/priority/cancellation design is
`docs/plans/server-ecs-migration.md`'s to make, and it should reference this shape rather than invent
a parallel one — but it is a *reference*, not an assumption that this section's mechanisms already
run on the server.

**The bus.** `lodestone_ecs::GameEvent(pub ClientEvent)` — a bevy `Message`, not a second vocabulary.
`ClientEvent` is already version-free, `Clone`, and `#[non_exhaustive]`, so wrapping it costs nothing
to keep in sync; a parallel ~107-variant enum would have been exactly the staleness factory `CLAUDE.md`
calls this repo's most-documented defect. `lodestone_client::state::SharedState::apply` is the one
write site, and it pushes **every** event with no `match` on it at all — the island-factory property
named throughout this document (`ingest::handles_event`/`session::handles_event`/`net::forward`'s
terminal `_ =>` arms) comes specifically from *selective* matching, so a firehose with no `match`
structurally cannot have that shape. `crates/lodestone-client/src/state.rs`'s
`tests::game_event_bus_write_site_has_no_match_on_the_event` reads that function's own source to keep
it that way, the same idiom `lodestone_model::event::route_tests::route_has_no_catch_all_arm` uses for
`route()`.

Off by default, behind `lodestone_ecs::GameEventBus` (a marker resource; `GameEventBusPlugin` installs
it together with `Messages<GameEvent>` and the system that ages the double-buffer once per `GameTick`,
since this codebase never calls `App::update()` and so never gets bevy's own message-aging system for
free). `SharedState` checks for the marker exactly once, at construction, and caches the answer as a
plain `bool` — a client that never opted in pays nothing beyond one boolean check per event, not an
extra `EcsHandle` lock. Today's only opt-in path is whoever builds the `World` before a `SharedState`
wraps it (`SharedState::adopting`'s caller, i.e. `lodestone_shell::sim::Sim::new` for the live client —
brokered, not part of this pass); `SharedState::default`'s bot/test path has no opt-in at all yet
(`new_ingest_handle` is hardcoded), named as a follow-up rather than solved here.

**Cross-plugin priority.** `lodestone_ecs::sets::EventPriority::{Lowest, Low, Normal, High, Highest,
Monitor}` — `SystemSet`s mirroring `org.bukkit.event.EventPriority` almost exactly, `.chain()`ed and
configured into **all four** public schedules (`NetIngest`, `GameTick`, `Update`, `Extract`) by
`CorePlugin`, since there is no single canonical "the event schedule" here the way Bukkit has one
thread. This is the piece `TickSet`/`IngestSet`/`FrameSet`/`ExtractSet` cannot provide: those anchor a
plugin against *our* systems, which says nothing about two *third-party* plugins that have never heard
of each other and need to agree on relative order without importing one another's crates.

**`Monitor` is enforced structurally, not by convention.** `lodestone_ecs::sets::assert_monitor_system_is_read_only`
panics if a candidate system has any mutable `World` access, checked through
`bevy_ecs::system::System::initialize`'s public `FilteredAccessSet` — the identical per-system access
metadata bevy's own `ambiguity_detection: LogLevel::Error` consults internally. **Walking an
already-*built* schedule to ask it the same question does not work with bevy 0.19's public API**,
checked directly rather than assumed: the type pairing a boxed system with its computed access
(`SystemWithAccess`) keeps that access field `pub(crate)`, and although `ScheduleGraph::systems`/
`Systems::get_mut` are public, the graph's own node storage is emptied once `Schedule::initialize` has
moved the systems into the optimized `executable` representation — confirmed empirically, not just by
reading field visibility, in an earlier draft of `crates/lodestone-ecs/src/sets.rs`'s own test. So the
check runs on a system *before* scheduling rather than reading one back out of a schedule after the
fact; `sets.rs`'s own doc comment on `assert_monitor_system_is_read_only` walks through why, including
the one known gap (`Commands`' deferred mutation is invisible to `System::initialize`'s access set, so
a `Monitor` system that queues a command through it would pass this check and still break the
guarantee — tracked on issue #110, not solved here).

**The toy consumer.** `crates/plugins/lodestone-event-logger`: an `EventPriority::Monitor` reader that
appends every observed `ClientEvent` to a plain `Arc<Mutex<Vec<_>>>` captured by its system's closure —
outside the ECS entirely, which is what lets a genuinely read-only `Monitor` system still report
findings anywhere it likes, exactly as a Bukkit `MONITOR` logger does. Nothing in the shipped client
registers it (`lodestone_shell::sim::Sim::new` does not know it exists), and **that is the settled
design rather than an outstanding follow-up** — see below.

**Its consumer, and the decision not to register it by default** (issue #436's ledger entry, resolved).
The crate landed with `tests/observes_the_game_event_bus.rs`, which registers the plugin through
`add_plugins` and then writes its own events with `World::write_message`. That proves the *reader*
works and cannot prove the plugin is reachable from a real session: the producer in it is the test, so
it is the **world** species of vacuous test — the flaw is in the input data, not anywhere readable in
the assertion. `crates/plugins/lodestone-event-logger/tests/observes_a_real_session.rs` is the real
consumer: a live `IntegratedServer` over `memory_pair` speaking the **real** 26.2 wire format, the real
client driver, and the plugin registered through `lodestone_app::client_app()` + `add_plugins` +
`ClientBuilder::ecs` — the same public composition path `Sim::client_app()` + `Sim::from_app` gives an
embedder, with nothing privileged. Because `lodestone_client::state::SharedState::apply` is
`pub(crate)`, there is no shortcut to it; an actual connection is the only way in, which is why that
test carries the dev-dependency set it does. Its oracle is the `EventStream`: `Driver::dispatch` calls
`read_model.apply(&event)` *before* `events.send(event)`, so the stream is a second, independent
delivery path for the same events and the log is asserted against it value for value, in order.

**It is deliberately not in the shipped client's plugin set**, for the same reason
`lodestone-autopilot` is not (`lodestone_shell::sim::build` documents that removal at length): the
client does not navigate itself and it does not log every decoded packet into an unbounded `Vec`
either. `GameEventBus` is opt-in precisely so it costs nothing when unused, and a default registration
would turn that cost on for every player to serve none. The negative control in that test file pins the
decision down — if anyone adds `EventLoggerPlugin` to `Sim::client_app`'s tuple, the control fails.

Still genuinely open from the same landing, and **not** closed by the above: `SharedState::default()`'s
bot/test path has no `GameEventBus` opt-in at all (it calls `new_ingest_handle()`, which never adds
`GameEventBusPlugin`, so `game_event_bus_enabled` is `false` for every `SharedState::default` — the
constructor's own comment at `crates/lodestone-client/src/state.rs` says so). A bot with no driver
therefore cannot use the bus by any route; only the `adopting` path, which the test above exercises,
can. Tracked on issue #436.

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
  correctly, so they stay behind the renderer's own API on purpose. **This is a permanent ceiling, not
  a gap — decided on issue #165.** A Fabric-class mod that replaces whole rendering stages (Sodium/
  Iris-class) is not portable to the native-plugin tier as designed, and will not become portable
  without abandoning the safety net entirely; that is stated here as the settled answer, not an open
  question.

  **Texture/model *substitution* for existing draws is a separate question from GPU access, and it is
  already fully solved — by the resource-pack mechanism, not a new seam (issue #165's second
  sub-question, also settled).** `crates/lodestone-assets::ResourceManager` is a real, ordered
  override stack (vanilla's own semantics) over `client.jar`, and every `load_*` call in
  `crates/lodestone-shell/src/resources.rs` goes through it — see
  [`docs/resource-packs-screen.md`](./resource-packs-screen.md). A locally-installed pack in
  `resourcepacks/` already swaps block, item, GUI and sky textures, no plugin API required. More to
  the point for a *server*-side plugin (economy shop cosmetics, a custom-look minigame, a "reskin"
  plugin): [`docs/server-resource-pack.md`](./server-resource-pack.md) (issue #334) is a real,
  already-shipped server → client push (`ResourcePackPushFeed::publish`, encoded as vanilla's own
  `ClientboundResourcePackPushPacket`) that directs a connected client to download and apply a pack by
  URL and SHA-1 hash — exactly vanilla's own mechanism, and exactly how a real Bukkit/Paper "custom
  texture" plugin does this too (Java plugins do not swap textures through a rendering API either;
  they push a resource pack). So this needs no rendering-architecture work at all: once a server-side
  plugin surface exists (`docs/server-ecs.md`, issue #433), the remaining piece is calling
  `ResourcePackPushFeed::publish` from a plugin system — mechanical wiring, not a design question.

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

### Custom world generation: not a verification-model conflict, but blocked on two other gaps

`lodestone-worldgen` is deliberately **not** a system — `docs/bevy-migration.md` §8 keeps it a plain
library precisely because it is a version-free interpreter verified block-for-block against real
server chunks, and putting a scheduler between the oracle and the code it validates would compromise
that verification. Issue #132 asked whether a plugin-supplied generator is even compatible with that
rule at all.

**It is, in principle, and the tension the issue raised does not actually exist.** `OverworldGenerator`
and `NetherGenerator` (`crates/lodestone-worldgen/src/{overworld,nether}/mod.rs`) are already called as
plain functions from `crates/lodestone-server/src/worldgen_data.rs`, never installed as a bevy
`System`. A `dyn ChunkGenerator` seam that both the vanilla interpreter (verified) and a plugin
(unverified — exactly Bukkit's own contract for a custom `ChunkGenerator`) implement would preserve
that shape as long as whatever calls it does so imperatively, the same way `worldgen_data.rs` calls
the vanilla generators today. §8's rule is about the ECS never owning verified math as *scheduled*
state; a plain trait object called from a plain function violates nothing.

**It is nonetheless not buildable today, for reasons that have nothing to do with that tension —
named so this is not rediscovered as a confused plugin-author bug report:**

1. **No `ChunkGenerator` trait exists at all.** Confirmed by grep — `OverworldGenerator`/
   `NetherGenerator` are concrete structs with no shared trait, so there is no dispatch point a plugin
   could register into yet.
2. **No per-world generator *selection* mechanism exists.** Which generator runs is hardcoded per
   dimension kind (overworld/nether/end); custom dimension registration — the thing a skyblock/void
   world would need to exist as a selectable target at all — is issue #134, itself an open gap.
3. **No plugin-reachable block *write* API exists** (issue #129, open) for a hand-rolled generator to
   place blocks through, even once it could be selected and invoked.

**Decision: out of scope for v1, blocked on #134 and #129 specifically, not on the ECS-verification
question, which is resolved.** When those two land, the shape to build is a `dyn ChunkGenerator` trait
implemented by both the vanilla interpreter and a plugin, dispatched imperatively (never as a
`System`) from whichever component ends up selecting a generator per world/dimension — preserving
`docs/bevy-migration.md` §8's rule by construction rather than by convention. Leaving issue #132 open
against those two blockers rather than closing it, since the decision names dependencies that do not
exist yet rather than settling something implementable now.

**Correction: the block-write half of that blocker closed after this paragraph was written** — see
"Block read/write, persistent data, plugin config, and bulk edit" below. Custom world generation stays
open on generator *selection* alone now, since a `dyn ChunkGenerator` still has no dispatch point to be
selected from.

### Block read/write, persistent data, plugin config, and bulk edit

Four sibling pieces of work, landed together because the later ones build on the earlier ones: a plugin
config convention (small and self-contained), the block read/write API, a persistent data container, and
a bulk-edit region API layered on top of the block write API.

**The packaged block read/write API is closed.** `lodestone_world::World` gained
`block_state_at(x, y, z) -> Option<u32>` (the read half `set_block` never had), and
`set_block_with_physics(x, y, z, state, physics: bool) -> Option<u32>` — exactly the
`set_block(pos, state, physics)` shape asked for, returning the previous state (what an undo history
captures). `physics: false` is exactly the existing `set_block`; `physics: true` additionally queues the
six orthogonally-adjacent positions onto a new `pending_physics_updates` queue (mirroring the existing
`pending_relight` queue's "record rather than act on" shape) for a future block-tick/neighbour-update
system that **does not exist yet** — this ships the `false` path now with the queue as the TODO anchor
for the `true` path, per the original scope note, rather than pretending physics is simulated. A plugin
reaches this through the same `ChunkWorldWrite`/`ChunkWorld` split every other block writer uses; no new
resource or dependency was needed.

**The plugin config/data-directory convention and the persistent data container's non-persistent half are
closed**, in a new crate,
[`crates/plugins/lodestone-plugin-support`](../crates/plugins/lodestone-plugin-support). `paths`/`config`
mirror `JavaPlugin.getDataFolder()`/`getConfig()`, built on `lodestone_auth::paths::data_dir` (the
platform-data-directory implementation this codebase already settled on — this does not reimplement
per-OS directory discovery a third time). `persistent_data` is a `HashMap`-backed
`EntityDataStore`/`ChunkDataStore` pair, namespaced `"<plugin>:<key>"` → `serde_json::Value`, deliberately
never decoding into named fields — see that module's own doc for why (the exact "static name list
excludes an undecoded field" hazard `CLAUDE.md` names). The persistent (survives-a-restart) half stays
out of scope, as originally scoped, pending world persistence existing at all. Real consumer:
`tests/drives_through_a_real_schedule.rs` runs a real `bevy_ecs::App` with `lodestone_ecs::CorePlugin` +
`PersistentDataPlugin` + a toy system on `GameTick`, with a negative control (an entity spawned partway
through only accumulates its own ticks) proving the system genuinely runs every tick rather than once.
See [`docs/plugin-data-and-config.md`](./plugin-data-and-config.md).

**The bulk-edit (WorldEdit-class) region API is closed**, in a second new crate,
[`crates/plugins/lodestone-worldedit`](../crates/plugins/lodestone-worldedit) — "a *second* plugin,
conceptually... not a server feature," per the original scope note, so nothing here lives in
`lodestone-world`/`lodestone-ecs` beyond the batched write primitive
(`World::fill_region`/`fill_region_capturing`, grouping writes by chunk instead of paying a `HashMap`
lookup per block). Re-derived rather than assumed: the open scoping question was whether a batched entry
point was needed "to avoid re-acquiring the chunk lock per block" — there is exactly **one** lock for the
whole store, taken once by whoever calls `ChunkWorldWrite::write()`, so the lock was never the per-block
cost; the repeated hashmap lookup was, and `fill_region`/`fill_region_capturing` remove it. Measured
(release build, 128×128×128 = 2,097,152 blocks): **43.8 ms** (~20.9 ns/block), `#[ignore]`d per
`CLAUDE.md`'s duration-measurement guidance rather than asserted as a CI regression gate — see
[`docs/worldedit-plugin.md`](./worldedit-plugin.md) for the full measurement and the `EditSession`
undo/redo design built on top.

**What was re-verified as still blocked, not attempted this pass:** entity spawn/despawn and custom
entity type registration both target `crates/lodestone-server/**`
(`MobSim::remove_mob`/`spawn_plugin_mob` do not exist; `IntegratedServer` hands out no `MobHandle`), a
file cluster this pass does not own and which had unrelated concurrent edits in flight while this was
checked. The brokered patches recorded on their tracking issues (and, for custom entity types, verbatim
in [`docs/custom-entity-types.md`](./custom-entity-types.md)) are current as of this re-verification —
both issues' own comments record this explicitly so the claim does not go stale silently.

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
which every plugin already does. `collision.rs`'s `physics_at` is now a thin
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

**6. `PlaceIntent`/`PlaceOutcome` — closed.** The previous version of this section recorded
`PlaceIntent` as a real, checked stop rather than an oversight: `remesh_around` reached into `Sim`'s
own mesh-worker pool and GPU terrain state, which were plain struct fields, not resources, so a
plugin-driven placement could write the block state through `ChunkWorld` but never repaint it. Two
things closed that, in order:

- **The re-mesh seam moved first.** `TerrainMesh::remesh_around(&mut self, store: &ChunkWorld, block:
  [i32; 3])` (`crates/lodestone-shell/src/mesher.rs`) now holds the 3×3×3 boundary filter and extent
  math that used to live in `Sim::remesh_around` (`sim/meshing.rs`) — that method had already reduced
  to pure `ChunkWorld`/`TerrainMesh` math with no other `Sim` state in it, so the move needed nothing
  new, only relocation. `Sim::remesh_around` is now a one-line delegation through
  `Sim::terrain_and_world`. `TerrainMesh` was already `#[derive(Resource)]`, so this is what made
  re-meshing reachable from a `GameTick` system that only holds `Res<ChunkWorld>` +
  `ResMut<TerrainMesh>` — no `Sim` required.
- **The audio engine moved too**, for a related but separate reason: `Sim::audio` was a private field,
  invisible to a system for the same structural reason `remesh_around` used to be, and a
  plugin-driven placement's sound needed it. `AudioEngine` (`crates/lodestone-shell/src/sim.rs`) is
  now a resource, read directly by `drive_placement` rather than through a `Sim` method.

`crates/lodestone-ecs/src/player.rs` now has `PlaceIntent { pos: BlockPos, face: BlockFace }` and
`PlaceOutcome { status: PlaceStatus, generation: u64 }`, mirroring `BreakIntent`/`BreakOutcome`'s
"express a wish, the shell owns the machine" contract with two deliberate divergences, both
documented on the types themselves rather than only here:

- **No sequence, no state id, no hand, no cursor** — narrower than `BreakIntent` even was, because a
  placement needs nothing else: the sequence is threaded internally by
  `lodestone_game::placement::Placement::use_on`, exactly as `MiningPredictor`'s counter is for
  breaking.
- **`generation: u64` on the outcome, which `BreakOutcome` has no need for.** Placement is one-shot
  and its `PlaceIntent` is removed by the shell the instant an attempt resolves — unlike a dig, which
  stays installed for its whole multi-tick duration and is removed by the plugin, not the shell. A
  plugin polling on some later tick needs to tell "the result of the attempt I just made" from "an
  attempt from several ticks ago I never read"; `generation` is what makes that possible without a
  race against the exact tick the attempt landed on.

`lodestone_shell::interact::drive_placement` consumes it, in `TickSet::Send` chained after
`drive_mining`: it resolves the intent into the identical `RayHit` shape a mouse click produces
(`resolve_intent_ray`, the cast `resolve_break_intent` used to do alone, generalised to serve both),
then runs the *same* `Placement::use_on` a human placement uses. Unlike a human right-click — which
vanilla always sends regardless of outcome — every `PlaceRejection` is checked **before** anything
reaches `use_on` or the wire, because a `PlaceIntent` specifically asks to place rather than merely
"interact," so "nothing placeable held" and "would intersect the player" are refused outright rather
than folded into a generic sent-but-nothing-happened result. `PlaceStatus::SentUnpredicted` is
reserved for the cases vanilla itself would still send a packet for: an interactable clicked block, or
a placeable item the census cannot resolve a state for.

**What is still open, named rather than built around:**

- **Neither intent has a plugin producer yet.** `grep -rn "BreakIntent\|PlaceIntent" crates/plugins/`
  is empty — `crates/plugins/lodestone-autopilot` (M1/M2, walk/step/descend/drop) writes
  `MovementIntent`/`LookIntent` only. Both intents are consumer-ready and hermetically gated
  (`crates/lodestone-shell/tests/{break_intent,place_intent}.rs`), but nothing in this tree inserts
  either one outside a test — the mine/place half of a Baritone-class plugin is future work, not
  reachable from the shipped client today.
- **A plugin can only place what is already selected.** There is no `SelectSlotIntent`: writing
  `SelectedSlot` directly would move the shell's own notion of "held item" without echoing the change
  to the server (`ClientAction::SetCarriedItem` has no producer from this seam), so a plugin
  autopilot cannot switch to a placeable item before issuing a `PlaceIntent` without also being wrong
  about what the server thinks is selected. Tracked as its own issue (small — mirrors
  `SelectedSlot`'s existing echo path in `lodestone-shell`) rather than folded into this one, since it
  is additive and does not block `PlaceIntent` itself for a plugin that only ever wants to place
  whatever is already in the active hotbar slot.
- **`ChunkWorld` no longer has a `pub fn write()` — the split landed (issue #423).** The store is a
  read/write pair: `lodestone_ecs::ChunkWorld` (read, no `write()`, no `Arc`) and
  `lodestone_ecs::ChunkWorldWrite` (the only write route, held by `drive_placement`,
  `Sim::predict_block`, the demo world's `set_block_world` and the net-ingest path). A plugin that
  takes `Res<ChunkWorld>` compiles nowhere that mutates the store, so `write_predicted_block`'s
  state+block-entity pairing and the re-mesh cannot be bypassed by accident.
- **`ActionQueue` still accepts a raw `ClientAction::UseItemOn` (or `::BlockAction`) with a
  fabricated `sequence` — the second door remains.** `ActionQueue(pub Vec<ClientAction>)` is a plain
  `Vec` resource with public field access, so a plugin system can still push a hand-built action
  carrying a sequence `PlacementPredictor`/`MiningPredictor` never drew, forking the block-prediction
  counter. The fix is either a smaller, checked enum of plugin-sendable actions (excluding anything
  carrying a predictor-owned `sequence`) or a separate, narrower egress; it is tracked separately
  because it is a decision about the whole action model, not a single type.

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
| exactly-one-writer ambiguity gate (`ambiguity_detection: Error`) | Stage 3 | **done** — `crates/lodestone-ecs/src/session.rs`'s `exactly_one_system_writes_each_session_component` |
| chunk world as a `Resource` with batched snapshot reads | Stage 4 | **done** — `lodestone_ecs::ChunkWorld` (`crates/lodestone-ecs/src/chunks.rs`), a `Clone`-able handle over one shared `lodestone_world::World`; `crates/plugins/lodestone-autopilot` reads it via `Res<ChunkWorld>` to snapshot a `lodestone_nav::SnapshotView` for search |
| `SendAction` message / `MessageWriter<SendAction>` egress | unassigned | **decided (issue #181, closed): `ActionQueue` wins, `SendAction` will not be built** — `player.rs`'s `ActionQueue(Vec<ClientAction>)` resource landed with Stage 2 and is reachable from a plugin system via `ResMut<ActionQueue>`; see the correction note above. The ordering/observability a `MessageWriter` would have given for free turned out to be unavailable from a buffered `Message` for this specific egress anyway (synchronous suppression needs a drain-time hook, not a next-tick reader), so `EgressFilters` (#157) delivers the same properties directly on the `Vec` instead |
| raw-packet observation (`RawPacket` message) | unassigned | **gap — re-verified: `grep -rn RawPacket crates` is still empty** |
| version-free event observation (`GameEvent` message, mirroring `ClientEvent`) | issue #104 | **done, gated off by default** — `lodestone_ecs::GameEvent`/`GameEventBus`/`GameEventBusPlugin`; see "The plugin event bus and cross-plugin priority ordering" above. Not a substitute for the `RawPacket` row above it — version-free and already-decoded, not version-opaque wire bytes |
| cross-plugin event-priority ordering (`EventPriority::{Lowest..Monitor}`) | issue #105 | **done** — `lodestone_ecs::sets::EventPriority`, chained and configured into all four public schedules by `CorePlugin` |
| `Monitor`-tier structural read-only enforcement | issue #110 | **done, via a pre-scheduling check** — `lodestone_ecs::sets::assert_monitor_system_is_read_only`; see the section above for why walking an already-built `Schedule` does not work with bevy 0.19's public API (checked directly) and what this checks instead |
| `Sim` deleted; plugin no longer reaches into shell internals | Stage 5 | not started — `lodestone-shell/src/sim.rs` still exists and still owns plugin registration (`Sim::new`'s `app.add_plugins((...))`), which is why a third-party plugin cannot self-register into the shipped client today: it has to be added to that call by whoever owns `sim.rs` |
| async bot tier / headless plugin host | Stage 6 | not started |
| world-space debug-geometry `Extract` channel | unassigned | **done** — see gap 3 above; this row was left stale (still saying "gap") for a time after gap 3 itself was marked closed, which is its own small instance of `CLAUDE.md`'s "staleness is the most common defect" rule: fixing one paragraph does not fix every other paragraph that restates the same fact |
| block physics constants (friction/speed/jump/bounce/stuck/climbable) reachable without depending on `lodestone-shell` | unassigned | **closed in `24af787`** — `lodestone_model::block_physics(&str) -> BlockPhysics`; see §3 above |
| `PathType` per state through the seam | unassigned | **gap — `docs/baritone-port.md` §3.3 named this in the original document; still true today** |
| `BreakIntent`/`BreakOutcome` (mine-a-block seam, mirroring `MovementIntent`) | unassigned | **done** — `crates/lodestone-ecs/src/player.rs`, consumed by `lodestone_shell::interact::drive_mining`; see gap 5 above |
| `PlaceIntent`/`PlaceOutcome` (place-a-block seam, mirroring `BreakIntent`) | unassigned | **done** — `crates/lodestone-ecs/src/player.rs`, consumed by `lodestone_shell::interact::drive_placement`; see gap 6 above. The blocker gap 6 used to name (re-mesh needing a `Sim`-only mesh-worker pool) is what closed: `TerrainMesh::remesh_around` is now the resource-only entry point, and `Sim::remesh_around` a one-line delegation to it |

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
written when it was open. (Update: the `SendAction`/`ActionQueue` design question itself is now also
closed — issue #181, see the correction note and stage-map row above — so nothing in this paragraph's
"still missing" list remains a design question, only `RawPacket` as an implementation gap.)

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

### Settled: `EcsHandle` reentrancy is unrepresentable for the sanctioned plugin surface

`lodestone_ecs::EcsHandle` (`Arc<parking_lot::RwLock<World>>`) is not reentrant — `write()` then
`read()` on the same thread deadlocks with no panic and no log line
(`docs/world-unification.md`'s "Rule 1 broke the client" record). Issue #177 asked whether a plugin
`System`'s shape could be made *structurally incapable* of expressing that deadlock, rather than
merely warned against it. **Re-checked directly against the tree for this pass: the answer is
already largely yes, by omission rather than by a new wrapper type**, and the remaining gap is
exactly the one already tracked on issue #20.

**A plugin depending only on `lodestone-ecs` — the doctrine-sanctioned dependency — has no route to
an `EcsHandle` at all.** Confirmed by grep: nothing in this crate does `insert_resource::<EcsHandle>`,
and there is no `Res<EcsHandle>`/`ResMut<EcsHandle>` anywhere. `EcsHandle` is re-exported from
`lodestone_ecs` (so host code — the driver, `lodestone-shell::Sim` — can name the type), but a
plugin's ordinary entry point, `impl Plugin::build(&self, app: &mut App)`, is never handed one, and
an ordinary `System` receives `&World`/`&mut World` as a parameter from the schedule runner itself —
not through a second `parking_lot` guard a plugin could misuse. This is candidate (1) from the issue
body, and it did not need building: it is the consequence of `EcsHandle` simply never being placed on
the ECS surface a plugin depends on.

**The one place a plugin legitimately needs to reach across a tick boundary — off-tick work — closes
the same way, at the type level rather than by omission.** `AsyncTaskPool::spawn`/`spawn_with_handback`
(`crates/lodestone-ecs/src/async_task.rs`, issue #114,
[`docs/plugin-async-tasks.md`](./plugin-async-tasks.md)) take an off-tick closure of type
`FnOnce() -> T + Send` — no `&World`, no `&mut World`, no `EcsHandle` parameter — so the closure has
no argument through which to reach the `World` at all. The hand-back closure gets `&mut World`, but
only inside `drain_completed_tasks`, on the tick thread, at a schedule point. This is the same
reasoning `docs/cancelable-actions.md` uses for `VetoFn` (issue #109/#101): give the plugin-facing
closure no parameter capable of reaching the lock, and reentrancy stops being a discipline problem.

**The residual gap is real, is exactly issue #20's scope, and is not closed by this decision.** A
plugin can still opt out of the sanctioned dependency graph — depend on `lodestone-shell` directly
(legal, the same version-locking-style escape hatch as depending on a version crate per §5) — and
obtain a real `EcsHandle` (e.g. `Sim::ecs()`). For that path, and for every internal call site, the
backstop is candidate (3): `hold_read`/`hold_write` keep a thread-local ledger and **panic on
reentrancy instead of hanging**. That backstop is *not* total — `EcsHandle` is a type alias for
`Arc<RwLock<World>>`, so `handle.read()`/`handle.write()` are `parking_lot`'s own inherent methods and
**cannot be intercepted**; a caller that reaches for those directly instead of `hold_read`/`hold_write`
still hangs. Closing that (routing every remaining direct call site through `hold_read`/`hold_write`)
is issue #20's job, tracked there rather than duplicated here. `AsyncTaskPool` additionally marks
every worker thread so a captured `EcsHandle` called from *inside pool work* panics rather than hangs
even off the tick thread — the one case general to "a plugin captured a handle somewhere it
shouldn't have," covered because the pool is the one sanctioned place off-tick work can originate.

**Decision:** combine (1) and (3), as already built — never place `EcsHandle` on the ECS-only plugin
surface (making the deadlock unrepresentable for the sanctioned graph, `lodestone-ecs`-only
dependents), and keep the panic-based ledger as the backstop for the explicit, version-locking escape
hatch (`lodestone-shell` direct dependency), with issue #20 as the named remaining work to make that
backstop cover every call site rather than only `hold_read`/`hold_write`'s. Candidate (2) — a
plugin-prelude wrapper type exposing only `hold_read`/`hold_write` — is not needed on top of this: it
would only matter for a plugin that already has an `EcsHandle` in hand, which per the above should not
happen for a plugin following the doctrine at all. Issue #179's reusable reentrancy test harness
should exercise this shape directly: build a plugin `App` with only `lodestone-ecs` as a dependency
and assert (by construction, e.g. a compile-fail/trybuild-style check or a grep of the plugin's own
manifest) that no `EcsHandle`-typed value is reachable from a system, rather than only re-running
`mining_deadlock.rs`'s one historical shape.

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
`FrameSet`/`ExtractSet` variant should add an entry here** (and, per the row below, the same now goes
for `EventPriority` — a fifth ordering-anchor *type*, not a variant of one of the original four),
oldest first:

| commit | change | why |
|---|---|---|
| `415138f` (Stage 0) | `IngestSet`, `TickSet`, `FrameSet`, `ExtractSet` all land with their original variant sets — `TickSet` as `Input, Physics, Predict, Animate, Send`, `ExtractSet` as `Terrain, Entities, Hud` | baseline |
| `0d82ab4` | `TickSet` gains `Intent`, between `Input` and `Physics` | give automation-supplied movement intent (a plugin, or a future navigator) a named ordering anchor distinct from raw human input, so two writers of `MovementIntent` become an explicit, checkable order instead of an `ambiguity_detection: Error` build failure — see `sets.rs`'s own doc comment on `TickSet::Intent` |
| `0d82ab4` | `ExtractSet` gains `Debug`, between `Entities` and `Hud` | a world-space debug-geometry channel (`DebugLines`) a plugin can push planned routes/probes into, ordered with the other world-space extracts before the screen-space `Hud` one |
| (this pass, issues #104/#105/#110) | New `EventPriority::{Lowest, Low, Normal, High, Highest, Monitor}`, configured into all four public schedules | give two *third-party* plugins that have never heard of each other a shared order to agree on — `TickSet`/`IngestSet`/`FrameSet`/`ExtractSet` anchor a plugin against *our* systems, not against another plugin's, which is a different problem `EventPriority` exists to solve; see "The plugin event bus and cross-plugin priority ordering" above |
| (issue #462) | **No variant change — the two `configure_sets` chains now actually contain `TickSet::Intent` and `ExtractSet::Debug`** | `0d82ab4` (two rows above) added both variants and both changelog rows, but never updated `CorePlugin::build`'s `.chain()` calls. A set outside a `chain()` has no edges, so both were **published ordering anchors carrying no ordering guarantee**: `.in_set(TickSet::Intent)` — which `crates/plugins/lodestone-autopilot` uses, and which `TickSet::Intent`'s own doc comment instructs — had no relation to `TickSet::Physics` and could run either side of it nondeterministically. Since physics reads `yaw` to resolve `MovementInput`'s axes, intent-after-physics silently applied *last* tick's facing. **The chain order is itself part of the ABI**, which is why this gets a row despite adding no variant |

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

### The changelog is now enforced, and its first run found two real defects

The paragraphs above state the policy correctly and enforced nothing. `cargo test -p lodestone-ecs
--test ordering_anchor_abi` (`crates/lodestone-ecs/tests/ordering_anchor_abi.rs`) is the gate that
closes that, in the same shape as `xtask`'s `docs_index_matches_committed`: a committed snapshot
(`crates/lodestone-ecs/tests/support/ordering_anchor_abi.txt`) of the whole anchor surface,
regenerated with `LODESTONE_REGEN=1`, failing loudly on **any** change with a message naming this
section. It fails on *additions* too, deliberately — this changelog asks for an entry from every PR,
not only renames, so a gate that caught only renames would leave its own rule unenforced.

It snapshots two things, and the second one is why:

1. the five enums' **variant lists**, from `sets.rs`;
2. the **sequence of anchor mentions in `plugin.rs`**, which is where `CorePlugin` actually
   `chain()`s them — so a reordering with no rename is visible.

**On its first run the chain half found that `TickSet::Intent` and `ExtractSet::Debug` are declared
but never chained.** The `0d82ab4` rows above describe both as landing "between `Input` and `Physics`"
and "between `Entities` and `Hud`" respectively. The variants landed and these changelog rows landed;
`CorePlugin::build`'s two `configure_sets` calls for `GameTick` and `Extract` (`plugin.rs`) were never updated. So
both are **published ordering anchors carrying no ordering guarantee** — a plugin writing
`.in_set(TickSet::Intent)`, which `crates/plugins/lodestone-autopilot` does and which
`TickSet::Intent`'s own doc comment instructs, gets no relation to `TickSet::Physics` at all and may
run either side of it. They are named in that test's `KNOWN_UNCHAINED` constant rather than silently
snapshotted, so **fixing them fails the gate** with an instruction to shrink the list.

What the gate cannot see is written into its own module doc: a sixth anchor enum declared in a new
file (`ANCHOR_ENUMS` is a hardcoded list — the docs-index gate's `docs/plans/` failure mode exactly),
semantic changes that keep a name, and whether this changelog was actually updated. It makes a
reviewer look; it cannot read prose.

> **Found stale during citation cleanup, not corrected here:** `grep -rn KNOWN_UNCHAINED
> crates/lodestone-ecs/src` now returns nothing, and `CorePlugin::build`'s current
> `configure_sets(GameTick, ...)` and `configure_sets(Extract, ...)` calls in `plugin.rs`
> both already include `TickSet::Intent` and `ExtractSet::Debug` in their `.chain()` tuples.
> This paragraph's claim that the two sets are "declared but never chained" looks resolved
> against the current tree; left for a content re-audit rather than silently edited here.

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
