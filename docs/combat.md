# Combat: swinging, attacking entities, knockback, and the attack-cooldown indicator

## What it is

Issues #72 (left-click only swung when a dig started), #12 (attacking
entities, taking knockback), and #121 (the crosshair cooldown reticle).
Covers three things from the original change, plus the ticker/indicator pair
added afterward:

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

### The attack-strength ticker and the crosshair indicator (issue #121)

Built as one unit deliberately: the ticker (state) and the crosshair reticle
(the thing that displays it) were left unbuilt together in the original
combat change specifically so they would not become two separate unconsumed
islands — see the git history around `24943a3` and the previous revision of
this doc's "deliberately not built" section.

**The ticker** is `AttackStrengthTicker(pub u32)`
(`crates/lodestone-ecs/src/player.rs`), a component on the `LocalPlayer`
entity mirroring vanilla's `attackStrengthTicker` field
(`Player.java:210`/`268`). `tick_attack_strength`, registered in
`LocalPlayerPlugin::build` under `TickSet::Animate` (already chained after
`Physics`/`Predict` by `CorePlugin`, so no new ordering edge was needed),
increments it by exactly `1` every `GameTick` — vanilla's unconditional
`this.attackStrengthTicker++` in `Player.tick()`. `spawn_local_player`/
`reset_local_player` start it at `0`, matching Java's bare-`int` default.
`Sim::attack_entity` (`crates/lodestone-shell/src/sim.rs`) resets it to `0`
synchronously on every entity attack — vanilla's
`MultiPlayerGameMode.attack` calling `player.resetAttackStrengthTicker()`
right after the client-side `player.attack(entity)`
(`MultiPlayerGameMode.java:425-430`). Unconditional, because this shell has
no client-side `cannotAttack` gate (damage is fully server-authoritative, see
above): every left-click on an entity restarts the cooldown, matching what
the real client does before any server response is known.

**The delay is not a constant.** `Sim::attack_strength_delay` implements
vanilla's `getCurrentItemAttackStrengthDelay`, `(1.0 /
getAttributeValue(Attributes.ATTACK_SPEED)) * 20.0` (`Player.java:1816-1818`).
It reads `minecraft:attack_speed` off the local player's own `Attributes`
component (`crates/lodestone-ecs/src/entity.rs`) through
`lodestone_entity::attribute::attribute_value` — the same server-fed,
three-stage-`calculateValue` fold `player_physics` already uses for
`WATER_MOVEMENT_EFFICIENCY` (Depth Strider). This was checked rather than
assumed, per two things worth re-verifying named in this task's brief:

- `lodestone-entity`'s attribute census (`crates/lodestone-entity/src/
  attribute.rs::default_def`) already carries `"attack_speed" => d(4.0, 0.0,
  1024.0)` — the correct vanilla default and clamp range — so no new default
  table was needed.
- `lodestone-data`'s `item_prototypes` census (`crates/lodestone-data/src/
  item_prototypes.rs`) does **not** carry attack speed at all — it covers
  only `max_stack_size`/`max_damage`/`equip_slot`. That is not a gap here,
  though: a weapon's `-2.4` (sword)/`-3.0` (axe) attack-speed modifier
  arrives the same way any other equipment-driven attribute change does — a
  server `update_attributes` packet the instant the held item changes
  (`AttributeMap`'s dirty-tracking on `LivingEntity.setItemSlot`), already
  folded into `Attributes` by `apply_entity_attributes`
  (`crates/lodestone-ecs/src/ingest.rs`). Nothing per-item needed adding.

Before the first `update_attributes` for the local player (a fresh demo-world
player, or a live session before login's fold lands), `attribute_value` falls
back to the registry default `4.0` (unarmed), giving the correct 5-tick delay
rather than a guess. `Sim::attack_strength_scale` combines ticker and delay
into vanilla's `getAttackStrengthScale(0.0F)` — the exact call
`Hud.extractCrosshair` makes for the crosshair-style indicator
(`Hud.java:448`) — clamped to `0.0..=1.0`.

**The indicator** is `HudFrame::attack_cooldown: Option<f32>`
(`crates/lodestone-shell/src/hud.rs`), populated unconditionally in `app.rs`
(`Some(self.sim.attack_strength_scale())` — unlike `health`/`food`/`xp`, both
the ticker and the attribute default exist before any server connection, so
this is never `None` on a real run) and drawn inside `HudGeometry::
build_inner`'s existing crosshair block, nested under `if frame.crosshair`
right alongside the white-plus reticle. That nesting is deliberate: it reuses
the same visibility gate the crosshair itself already has (issue #51's
container-screen suppression), rather than inventing a second one.

The two sprites (`hud/crosshair_attack_indicator_background`,
`hud/crosshair_attack_indicator_progress`, both 16x4 native) were already
present in the GUI atlas — `GuiAtlas` globs `gui/sprites/**`, and the
air-bubble/hotbar work had already established that pattern holds — so no
asset plumbing was needed, only the draw call. `Builder::sprite`/
`gui_geometry` are no-op-safe with no atlas attached (see `sprite_vitals`'s
own doc), so a jar-less/headless run draws nothing here instead of needing a
second procedural implementation — the same choice already made for the
underwater bubble row. The progress bar is cropped by shrinking both the
destination width and the sampled UV span to the cooldown fraction, the exact
idiom `sprite_vitals` already uses for the XP-bar progress fill.

**Scope cuts, both deliberate:**

- **Only `AttackIndicatorStatus::CROSSHAIR`.** Vanilla's real enum
  (`AttackIndicatorStatus.java`) has three variants — `OFF`, `CROSSHAIR`,
  `HOTBAR` — and issue #121 explicitly scoped this shell to ship the default
  (crosshair) only, noting the options-menu toggle as future work (#32/#55
  own that menu). There is no `Options::attack_indicator` read anywhere; the
  indicator always draws whenever the crosshair does.
- **No full-charge "ready" icon.** Vanilla replaces the fill bar with a
  distinct `CROSSHAIR_ATTACK_INDICATOR_FULL_SPRITE` circle when the scale
  reaches `1.0` *and* the crosshair is over a living, in-range target *and*
  the held weapon's delay exceeds 5 ticks (`Hud.java:450-465`) — a slow-weapon
  "you're ready" nicety. That needs the crosshair's entity target plus its
  liveness/range, none of which `HudFrame` carries today. At full charge this
  shell simply draws nothing (matching vanilla's non-"ready" default case),
  which is also the correct control for the pixel gate below: the indicator
  must produce **zero** pixels at `attack_cooldown = Some(1.0)`, not the full
  circle.

## What is deliberately not built here

**Vanilla's crit condition and the sweep-attack condition.** These are real
per-hit vanilla mechanics (`Player.java:951-1053`'s `attack()`), but both
exist only to trigger **local** sound/particle feedback — the damage number
itself is server-authoritative, and the wire `Attack` packet carries none of
it. Their consumer is `entities.rs`/asset work (particles, sounds), out of
`lodestone-shell/src/{sim.rs,interact.rs,hud.rs}`'s scope for this pass, and
they need real particle-emitter and sound-cue plumbing this shell does not
have wired to combat yet. Building either now — with the ticker/indicator
already landed as the natural place a crit read would plug into
(`Sim::attack_entity`'s call site) — would still orphan the sound/particle
half, so they stay unbuilt rather than half-started. Whoever adds the sweep
sound or the crit particle burst is the natural owner.

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
- The attack-strength delay has no standalone constant: it is
  `20.0 / attack_speed_attribute`, computed fresh in
  `Sim::attack_strength_delay` every call. The only literal is the registry
  default `4.0` (unarmed attack speed), which lives in
  `lodestone-entity/src/attribute.rs::default_def` — not duplicated in
  `lodestone-shell`.
- `crates/lodestone-shell/src/hud.rs`'s crosshair block hardcodes the
  indicator's native size (`16x4`) and offset (`cx - 8, cy + 9`) — vanilla's
  own constants (`Hud.java:457-458`), not configurable.

## Dependencies

- `lodestone_model::{ClientAction::InteractEntity, EntityInteraction}` — the
  outbound action shape (`crates/lodestone-model/src/action.rs`).
- `crates/protocol/v770/src/adapter.rs` — the existing `InteractEntity` encode
  arm this change is the first caller of.
- `VersionData::entity_facts` (`lodestone-model`/`lodestone-v770`'s entity
  census) — entity hitbox dimensions for the attack ray, the same seam
  `tick_nearby_entities` already depends on for the push crowd pass.
- `lodestone_ecs::entity::{MinecraftEntityId, Position, EntityKind, HurtTime,
  Attributes}`, `lodestone_ecs::player::{PhysicsState, AttackStrengthTicker}`
  — the ECS components this change reads and writes.
- `lodestone_entity::attribute::attribute_value` and
  `crates/lodestone-entity/src/attribute.rs::default_def`'s `"attack_speed"`
  entry — the attribute fold and its registry default, both pre-existing and
  reused rather than duplicated.
- The GUI atlas's `hud/crosshair_attack_indicator_background`/
  `hud/crosshair_attack_indicator_progress` sprites (already stitched from
  `client.jar` — no new asset plumbing).

## How to change it

- Adding crit/sweep sound-and-particle feedback: start from
  `Sim::attack_entity` (`sim.rs`) — that is where the outbound send already
  happens and where `AttackStrengthTicker`/`attack_strength_scale` are
  already read, so a `criticalAttack`/`fullStrengthAttack`/`sweepAttack`
  decision has everything it needs except the particle-emitter/sound-cue
  plumbing. See "What is deliberately not built here" above.
- Adding the hotbar-style attack indicator or the `AttackIndicatorStatus`
  options toggle: `HudFrame::attack_cooldown` already carries the fraction;
  the missing half is an `Options`-driven read gating which of
  `hud.rs`'s crosshair-block draw (already built) vs. a new hotbar-adjacent
  draw (not built) runs, mirroring vanilla's `extractItemHotbar`
  (`Hud.java:606-621`).
- Adding the full-charge "ready" icon: needs the crosshair's live entity
  target's liveness/range plus the held weapon's delay compared against `5`
  ticks (`Hud.java:450-455`) threaded into `HudFrame`, then a third sprite
  branch (`hud/crosshair_attack_indicator_full`, already in the atlas)
  alongside the existing background/progress branch in `hud.rs`.
- Changing the delay's item-attribute-modifier source: it is **not** a
  per-item census read (`lodestone-data`'s `item_prototypes` deliberately has
  none) — it is whatever the server's `update_attributes` packets put in the
  local player's `Attributes` component. A demo-world (offline) weapon swap
  will *not* change the delay, because nothing generates that packet without
  a server; that is a real, current gap for offline testing, not a bug in the
  live path.
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
