# Combat: swinging, attacking entities, and knockback

## What it is

Issues #72 (left-click only swung when a dig started) and #12 (attacking
entities, taking knockback). Covers three things:

1. **The arm now swings on every left-click**, not only the ones that start a
   dig — including a miss (empty air) and attacking an entity.
2. **Left-clicking a living entity sends the real attack packet** — the
   serverbound `Interact`/`Attack` action was fully built and encoded
   (`crates/protocol/v770/src/adapter.rs`'s `InteractEntity` arm) but had zero
   callers anywhere in `lodestone-shell`; this is the first one.
3. **Server-sent knockback now actually moves the local player.** A
   `ClientboundSetEntityMotionPacket` naming our own entity id was silently
   absorbed into a component nothing reads; it now overwrites the physics
   velocity that `player_physics` integrates.

Also landed: the `HurtTime` countdown component (both `EntityDamaged` and
`EntityHurtAnimation` were decoded-but-unconsumed islands before this), the
ECS-side half of the hurt-flash window a future render pass can key off.

## How it works

### Swing dispatch (`Sim::begin_attack`, `crates/lodestone-shell/src/sim.rs`)

Vanilla's `Minecraft.startAttack` (`Minecraft.java:1603-1672`) switches on
`hitResult.getType()` and swings the arm **unconditionally after the switch**,
on every arm including a miss. Before this fix, only the "a dig actually
started" arm ever reached `Sim::swing_hand` (through `drive_mining`'s queued
`SwingArm`) — punching air, an entity, or empty space produced no animation.

`begin_attack` now splits into `begin_attack_live`/`begin_attack_demo`, each
implementing the same three-way switch:

- **`ENTITY`** — `Self::attack_entity` sends the packet, then swings.
- **`BLOCK`** (not air) — arms the hold-to-mine loop exactly as before; that
  loop's own `SwingArm` still fires when a dig actually starts.
- **`MISS`** (nothing targeted) — swings and does nothing else.

`case ENTITY` takes priority: `EntityRayTarget` (below) is already the nearer
of an entity-or-block pick, so a `Some` there means mining must not start even
when a block is also targeted (`begin_attack_live_prefers_an_entity_target_over_mining`
in `sim.rs`'s test module pins this).

The demo world (no live connection, no networked entities) only ever sees
`BLOCK` vs `MISS` — there is nothing to populate an `ENTITY` case with.

### Entity targeting (`Sim::update_entity_target`, same file)

A new resource, `EntityRayTarget(pub Option<i32>)` (`crates/lodestone-shell/src/interact.rs`),
holds the server entity id (not a `bevy_ecs::Entity`) the view ray currently
points at. Recomputed every frame alongside the existing block `RayTarget`, by
`Sim::update_target`, against a **shorter** range:

- `ENTITY_REACH = 3.0` — vanilla's `DEFAULT_ENTITY_INTERACTION_RANGE`
  (`Player.java:134`), distinct from and shorter than block `REACH` (`4.5`,
  `Player.java:133`). Creative's `+2.0` modifier (`Player.java:150`) is **not**
  tracked — this shell has no attribute-modifier pipeline for it yet, so every
  session uses the unmodified survival default.
- Further shortened to the block hit's own entry distance when a block sits
  closer than that, so a wall between the eye and an entity is never picked
  through it (vanilla's `blockDistance` clamp in `GameRenderer.pick`). The
  block is treated as a unit cube for this cutoff rather than its real outline
  shape (the shell does not carry outline geometry at all — see
  `Sim::outline_shape_source`'s docs on the same gap); the only effect of that
  approximation is a very slightly conservative cutoff, never a pick through
  solid terrain.

Candidates come from the same `(Position, EntityKind)` query
`Sim::tick_nearby_entities` already uses for the entity-push crowd pass,
resolved to a hitbox through the identical `VersionData::entity_facts` seam —
an unknown entity type is excluded, never approximated. The local player is
never a candidate, with no special-case code needed: `apply_entity_spawn`/
`apply_local_player_login` (`lodestone-ecs/src/ingest.rs`) never give the local
player's own `Entity` a `Position`/`EntityKind` component, so the query
structurally cannot return it.

The actual ray-vs-box test is `crate::raycast::ray_aabb` — a plain slab test
taking raw `min`/`max` triples rather than `lodestone_physics::Aabb`, so
`raycast.rs` keeps the "no `lodestone-world`, no GPU" independence its module
docs already promise for the block DDA raycast next to it.

### Sending the attack (`Sim::attack_entity`)

Lowers straight to `ClientAction::InteractEntity { entity_id, interaction:
EntityInteraction::Attack, sneaking }`, sent directly (like
`Sim::use_item_live`'s sends) rather than queued — an attack is a discrete
click event, not a per-tick one. The v770 adapter already encodes this variant
as the dedicated 26.2 `Attack` packet (`crates/protocol/v770/src/adapter.rs`);
before this change nothing in the shell ever constructed
`ClientAction::InteractEntity` at all, so the encoder was dead, unused code.

The `Attack` packet's wire shape carries only the target entity id — no
damage, no strength scalar. Damage is fully server-authoritative.

### Knockback (`apply_entity_velocity`, `crates/lodestone-ecs/src/ingest.rs`)

Vanilla's `Entity.lerpMotion` (`Entity.java:2649-2651`) is
`this.setDeltaMovement(movement)` — an unconditional **replace** despite the
"lerp" name — and `LocalPlayer` declares no override
(`ClientPacketListener.handleSetEntityMotion`, `:623-629`). So a
`ClientboundSetEntityMotionPacket` naming our own id means "overwrite your own
velocity", not "nudge it".

`apply_entity_velocity` now checks whether the event's `entity_id` resolves
(through `EntityIndex`) to the `LocalPlayer` entity. If so, it writes directly
into `PhysicsState.0.velocity` — the exact field `player_physics` integrates
every `TickSet::Physics` — instead of inserting the generic `Velocity`
component, which nothing reads for the local player (motion comes from
`PhysicsState`, never that component). Before this fix every such packet fell
into that dead branch: **the client took a hit and never moved.**

No staging/pending component is needed. `NetIngest` runs synchronously on the
net thread as each packet decodes, strictly before the driver's next
`GameTick` (see `ingest.rs`'s module docs, "How events get in"), so a plain
overwrite here is picked up by that tick's `player_physics` exactly once —
matching vanilla's one-shot `setDeltaMovement`.

Remote entities are unaffected: they still get the generic `Velocity`
component exactly as before.

### The hurt-flash countdown (`HurtTime`, `crates/lodestone-ecs/src/entity.rs`)

`ClientEvent::EntityDamaged` and `ClientEvent::EntityHurtAnimation` were both
**decoded, unconsumed islands** before this change — real events, correctly
produced by the v770 adapter, with zero `match` arms anywhere in
`lodestone-ecs`/`lodestone-shell`/`lodestone-client`. Worse: neither was even
in `ingest::handles_event`'s routing `matches!`, so `SharedState::apply`
(`crates/lodestone-client/src/state.rs`) never sent them to `NetIngest` at
all — they fell into the legacy `Inner::apply` fallback and vanished
regardless of what a hermetic test showed (a test that pushes straight onto
`IngestQueue` and runs the schedule bypasses that routing gate entirely; see
`handles_event_covers_exactly_the_variants_with_a_system` in `ingest.rs`'s
test module for the control that catches this class of bug).

Both events now reset a `HurtTime(pub u32)` component to `10` ticks — vanilla's
`LivingEntity.handleDamageEvent` (`:2044-2049`) and `LivingEntity.animateHurt`
(`:1873-1876`) write the identical `hurtDuration = 10; hurtTime = hurtDuration;`
pair, so one countdown covers both reports. `tick_hurt_time`
(`TickSet::Animate`) ages it toward zero, one per `GameTick`, matching
vanilla's per-tick decrement. `EntityHurtAnimation`'s `yaw` field is not
carried into the component — vanilla's own override accepts the parameter and
never stores it either.

## What is deliberately not built here

**Vanilla's `attackStrengthTicker`/`getAttackStrengthScale` cooldown, the crit
condition, and the sweep-attack condition.** These are real per-hit vanilla
mechanics (see `Player.java:951-1053` for `attack()`/the crit and sweep
conditions, and `:1816-1837` for the ticker/cooldown itself, in
`.cache/mc/26.2/src`), but every one of them exists only to scale **local** sound/
particle feedback and the crosshair cooldown indicator — the damage number
itself is server-authoritative, and the wire packet carries none of it. None of
their consumers exist in this shell yet: the crosshair indicator is `hud.rs`'s
(held by a different agent at the time of this change), and sweep/crit
sound-and-particle feedback is `entities.rs`/asset work, also out of
`lodestone-shell/src/{sim.rs,interact.rs,net.rs}`'s scope. Building a ticker
with nothing to read it would be exactly the unconsumed-island class this
repo's `CLAUDE.md` warns about, so it stays unbuilt rather than built and
orphaned. Whoever adds the crosshair pip or the sweep sound is the natural
owner — it plugs in as a new read of `Sim::attack_entity`'s call site, or a new
field alongside it.

**`HurtTime` has no render-side consumer yet.** `entities.rs` does not read it
— nobody asked it to. The patch spec for that hookup: read `HurtTime` (and
maybe `EntityFlags`) off each drawn entity in whatever assembles
`EntitySnapshot` (`crates/lodestone-shell/src/entities.rs`), and drive a red
tint / a "just hit" pose the same way the walk cycle already reads other
per-entity components there. Local-player-specific hurt feedback (screen tint,
camera shake on being hit) is issue #58, not this one — it needs per-tick
camera state that does not exist yet.

## Configuration

- `crates/lodestone-shell/src/sim.rs::ENTITY_REACH` — `3.0`, vanilla's
  unmodified `DEFAULT_ENTITY_INTERACTION_RANGE`. No creative/attribute
  modifier tracked.
- `crates/lodestone-ecs/src/ingest.rs::HURT_DURATION_TICKS` — `10`, vanilla's
  `hurtDuration` constant.

## Dependencies

- `lodestone_model::{ClientAction::InteractEntity, EntityInteraction}` — the
  outbound action shape (`crates/lodestone-model/src/action.rs`).
- `crates/protocol/v770/src/adapter.rs` — the existing `InteractEntity` encode
  arm this change is the first caller of.
- `VersionData::entity_facts` (`lodestone-model`/`lodestone-v770`'s entity
  census) — entity hitbox dimensions for the attack ray, the same seam
  `tick_nearby_entities` already depends on for the push crowd pass.
- `lodestone_ecs::entity::{MinecraftEntityId, Position, EntityKind, HurtTime}`
  and `lodestone_ecs::player::PhysicsState` — the ECS components this change
  reads and writes.

## How to change it

- Adding the crosshair attack-strength indicator or crit/sweep feedback:
  start from `Sim::attack_entity` (`sim.rs`) — that is where the outbound send
  already happens, and where a ticker/cooldown component would need to be
  read to decide `criticalAttack`/`fullStrengthAttack`. See "What is
  deliberately not built here" above before adding one with no consumer.
- Adding the render-side hurt tint: `HurtTime` already exists and already
  counts down correctly; the missing half is entirely in `entities.rs`.
- Changing entity reach/attribute modifiers: `ENTITY_REACH` is a `const`
  today, not attribute-driven. Creative's `+2.0` needs an attribute-modifier
  pipeline this shell does not have; do not hardcode `5.0` for creative
  without one, since gamemode isn't read at that call site today either.
- `EntityRayTarget`/`RayTarget` must stay computed from the *same* ray
  (`Sim::update_target` calls `update_entity_target` with the exact `origin`/
  `dir`/`block_hit` it just used) — computing them independently would let the
  two disagree about, e.g., a diagonal look direction.
