# Combat

## What it is

Melee/ranged attack resolution end to end: swinging and targeting, sending
and decoding the attack packet, knockback, the attack-cooldown ticker and its
HUD indicator, hurt/death visual feedback, equipment-derived combat
attributes (damage, armor, toughness), and the damage-type registry that
tags how a hit is reduced.

## How it works

### Swing, targeting and sending the attack

`Sim::begin_attack` (`crates/lodestone-shell/src/sim.rs`) mirrors vanilla's own
attack-start entry point: a three-way switch on the ray hit that swings the
arm unconditionally on every branch — `ENTITY` sends the attack packet then
swings, `BLOCK` arms the hold-to-mine loop, `MISS` just swings. Entity
targeting (`Sim::update_entity_target`, `EntityRayTarget` in `interact.rs`)
uses a shorter reach than block interaction — `ENTITY_REACH = 3.0` vs block
`REACH = 4.5` — clamped to a closer block hit so a wall is never picked
through. Creative's `+2.0` entity-reach modifier isn't tracked.

`Sim::attack_entity` lowers to `ClientAction::InteractEntity { interaction:
Attack, .. }`, sent immediately (not queued), encoded as the 26.2 `Attack`
packet. **The wire packet carries only the target entity id — no damage, no
strength scalar. Damage is fully server-authoritative.**

Server-side, `ServerBound::Attack { entity_id }` decodes `minecraft:attack`
and reaches `MobHandle::with(|sim| sim.attack(..))` — a
`BlockEntityHandle`-shaped `Arc<Mutex<_>>` handle onto the live `MobSim`,
letting a connection task mutate the same sim the tick loop ticks.
`minecraft:interact` (plain right-click) deliberately decodes to `Ignored` —
there's no interaction model (taming/feeding/mounting) to consume it yet.

### Knockback

Vanilla's own motion-lerp setter is an unconditional **replace**, not a lerp,
and `LocalPlayer` takes no override — so a set-entity-motion packet
naming the local player overwrites `PhysicsState.velocity` directly (the
field `player_physics` integrates) instead of the generic `Velocity`
component nothing reads for the local player. Remote entities still get the
generic component.

**Direction convention: attacker-relative, `target - attacker`.** Vanilla's
`dealDefaultKnockback` computes `source.getSourcePosition().x() -
this.getX()` — the target flies *away* from its attacker. This is easy to
get backwards: a comment labelled "attacker→target" that actually computes
the opposite sign is a real bug shape that has shipped here before. Derive
the sign from the source, never from a sibling's label.

**The flat knockback impulse (vanilla's `hurtServer`, `0.4`) applies on
every hit, unconditionally — only the *sprint bonus* on top of it is gated
on the attacker sprinting.** A non-sprinting hit still knocks back; it just
doesn't get the bonus. Server-side, `getKnockback()` resolves to the
attacker's `minecraft:attack_knockback` attribute (default `0.0`, no weapon
model to add to it), so a non-sprinting player attack's total knockback
power is genuinely `0.0` today, not a placeholder.
`SPRINT_ATTACK_KNOCKBACK_POWER = 0.5` is added when the attacker's tracked
`sprinting` flag is set. The push direction server-side is currently
attacker-position→target (a stand-in for real facing, since crosshair and
facing nearly always agree in melee); real per-connection yaw is now
tracked and swapping it in is a remaining wire-up, not a decode gap. Mobs
have no persistent-velocity/drag model, so their knockback applies as an
immediate one-tick position displacement rather than a decaying velocity.

### Attack-cooldown ticker and the crosshair indicator

`AttackStrengthTicker` (component on the local player) increments by 1 every
`GameTick` and resets to 0 on every attack — unconditionally, since there's
no client-side "cannot attack" gate. **The delay is not a constant**:
`Sim::attack_strength_delay` computes `(1.0 / attack_speed_attribute) *
20.0`, reading `minecraft:attack_speed` off `Attributes` (registry default
`4.0`, giving the vanilla 5-tick unarmed delay before any server attribute
packet arrives). A weapon's speed modifier arrives via a server
`update_attributes` packet, not a per-item census (there is none).
`Sim::attack_strength_scale` combines ticker and delay, clamped `0.0..=1.0`.

The crosshair indicator (`HudFrame::attack_cooldown`) shares the crosshair's
own visibility gate. All three vanilla `AttackIndicatorStatus` variants
(`OFF`/`CROSSHAIR`/`HOTBAR`) reach pixels; `HOTBAR` is a distinct 18x18
sprite pair anchored bottom-up next to the hotbar, not the crosshair bar
re-anchored. Not built: the full-charge "ready" icon (needs the crosshair's
live target liveness/range in `HudFrame`) — at full charge this draws
nothing, matching vanilla's non-"ready" default.

### Hurt/death feedback

`EntityDamaged`/`EntityHurtAnimation` reset `HurtTime` to 10 ticks, counted
down one per tick. `EntityStatus` byte 3 (death) sets `DeathTime` counting
*up* from 0 — absence means alive, so the first death tick draws upright,
matching vanilla's one-tick lag between `die()` and the first `tickDeath()`.

The render overlay is vanilla's per-model red blend (`hasRedOverlay =
hurtTime > 0 || deathTime > 0`), **a blend toward red, not a multiply** —
treating it as a multiply crushes the model toward black instead. Alpha is
a flat `178/255`, boolean-gated, no fade. Applies to every drawn living
entity except the local player's own first-person view (matching vanilla —
there's no first-person hurt overlay; that's the HUD heart flash's
territory). Death additionally drives a fall-over rotation, `sqrt((deathTime
- 1)/20 * 1.6)` clamped to 1, saturating at `deathTime == 13.5` rather than
20.

Not wired: the local player's own third-person body (no ingest entity for
it to read `HurtTime` from), and `bobHurt` (the camera-roll damage tilt) —
blocked on `Camera` gaining a real roll degree of freedom, since
`Camera::view_matrix` hardcodes world-up and a view-matrix decomposition
cannot recover a pure roll. `ViewBob::hurt`/`BobFrame` already compute the
correct value; only the camera plumbing is missing. There is no vanilla
full-screen damage overlay or camera shake at all — nothing should be built
for either.

### Shield, bow, and the generic-use fallthrough

Two independent gaps kept the shield and bow (both `useOnRelease() ==
true`) functionally dead: `ClientAction::ReleaseUseItem` had zero producers
(no mouse/key release ever sent it), and `Sim::use_item_live` returned early
whenever the crosshair was over any entity or nothing at all, instead of
falling through to the generic use-item send the way vanilla's switch does
after a non-consuming result. Both are fixed: a release edge reaches
`Sim::end_use`, and the entity/no-target/block branches fall through to
`Sim::use_item_generic` under the same conditions vanilla does — the block
branch only falls through when nothing was placed and the held item isn't
itself placeable (else a refused placement would equip a carved pumpkin).
Deliberate divergence: with no local interact-success prediction, every
entity interact now falls through, which can send one harmless redundant
use packet when boarding a vehicle — smaller than the shield/bow being dead.

### Crit particles and the sweep-attack particle

Crit is real client-side dual simulation matching vanilla's own client copy
of its player-attack routine: the wire packet carries no crit flag, so this prediction
can't disagree with the server about anything that matters. Condition:
full-strength attack (ticker scale `> 0.9` at partial-tick `0.5`, not the
indicator's `0.0`), airborne, not sprinting, not on ground/climbable/in
water, target is a living entity. One tick's worth of the 16-candidate
unit-sphere burst is spawned (vanilla's own tracking-emitter type runs 3 ticks; this
particle system has no persistent per-attack emitter, a disclosed
simplification rather than an approximation of the physics).

The sweep-attack *particle* (a stationary 4-tick billboard) is built and
reaches pixels through the generic `LEVEL_PARTICLES` broadcast path — no
client dispatch code was needed. The sweep-attack *damage* mechanic
(vanilla's entities-in-a-box loop around the original target, its own
knockback) is a structurally separate, still fully unbuilt feature — it
needs a server-side attack-strength ticker and a sword item tag, not merely
"more damage".

### Equipment-derived combat stats

`lodestone_entity::equipment` feeds vanilla's real attribute **modifiers**
into the existing `AttributeMap` rather than a parallel formula: `(slot,
item id)` → `item_modifiers` (rows from armor/tool material tables) →
`apply_equipment` inserts `Modifier { id, amount, AddValue }` → the existing
attribute fold → `defenses_from_attributes`/`attack_damage_from_attributes`.
Using vanilla's real modifier ids means two helmets can't stack and a new
weapon replaces rather than adds to the old one, both correct vanilla
behavior for free from keying by id.

`PlayerInventory::combat_equipment` folds the six combat slots — feet 36,
legs 37, chest 38, head 39, off-hand 40, and main hand is the **selected
hotbar slot**, not native slot 0.

Gotchas:
- **A modifier only applies in the slot vanilla publishes it for** — hence
  `apply_equipment` taking `(slot, item)` pairs, not a bare item list.
- **`makeDefense`'s argument order is boots-first** (`boots, legs, chest,
  helm, body`) — reading it head-first swaps a helmet's value with a
  boot's; totals can coincide across the swap, so only a per-piece
  assertion catches it.
- **A weapon's damage modifier is `attackDamageBaseline +
  material.attackDamageBonus`** — a diamond sword is `3.0 + 3.0`, not `3.0`.
  Trident (`8.0`) and mace (`5.0`) are flat literals, not tier-derived.
- **The player's `attack_damage` base is `1.0`, not the registry default
  `2.0`** — vanilla's own player attribute registration overrides it.
- Not modelled: enchantment protection/effectiveness (`Defenses` fields stay
  at neutral defaults — accurate, not a stub), and shield blocking (needs an
  item-data model, `BlocksAttacks`, this workspace doesn't have). Mob
  equipment now feeds the same functions — see
  [`mob-spawning.md`](./mob-spawning.md).

### Damage types and tags

The `minecraft:damage_type` registry (51 types, 35 tags) is generated from
vanilla's datapack JSON into `crates/lodestone-data/src/generated/
damage_types.rs` and consumed through `DamageFlags::for_damage_type`
(`lodestone-entity/src/damage.rs`), which maps five tags onto the five
damage-pipeline stages one-for-one: `bypasses_armor`, `bypasses_effects`,
`bypasses_resistance`, `bypasses_enchantments`, `bypasses_cooldown`.
Behavior keys off tags, never the type name.

Gotchas:
- **Tag membership is a transitive closure**, not a flat list — 7 of 34 tag
  files reference other tags. Resolved once at generation time so `is_in`
  is a single bit test; a flat reader is wrong for exactly those seven and
  passes most spot checks anyway.
- **`bypasses_cooldown` has no data file anywhere in 26.2** — a real tag
  (gates the i-frame window) with zero members; the emptiness is asserted
  by a dedicated test.
- **`minecraft:generic` is itself `bypasses_armor`-tagged** — the wrong type
  to test armor reduction with. Use `minecraft:mob_attack`, which reduces.
- **`message_id` is not the type name** (`mob_attack` → `"mob"`,
  `ender_pearl` → `"fall"`) — death-message code must read the field.
- **The generated table lives at `src/generated/damage_types.rs`**,
  distinct from the hand-written `src/damage_types.rs` accessor — grepping
  the wrong one reports a false absence.
- **Indices are not network ids** — the registry is purely data-driven, so
  per-connection network ids come from registry-sync order. Never put an
  index on the wire.
- The tag enum's discriminant *is* the closure's bit index — a new variant
  must go in the correct alphabetical slot or it shifts every membership
  bit.

### Server-side melee damage (integrated server)

Punching a mob on the integrated server reaches real damage and knockback:
`ServerBound::Attack` → `MobHandle::with` → `SimMob::apply_damage`
(`HurtCooldown` + `apply_reductions`) →
`lodestone_physics::knockback::knockback_impulse` →
`NavigatingMob::apply_knockback`. No reply packet is sent — the existing
`EntityStreamer::sync` carries the result to every connection tracking the
mob. `PLAYER_BARE_HAND_ATTACK_DAMAGE = 1.0` is the only damage today (no
server-side attack-strength ticker or weapon model, so every hit is flat,
full-strength, no crit).

Mob-on-player damage is a live trigger once pursuit AI connects: `MobSim`
matches an attack's target position against its fed player list, queues a
`PlayerHit`, and `serve_play`'s periodic vitals tick drains it through the
same `PlayerVitals::apply_damage` pipeline mobs already use, with the
player's own armor defenses. One disclosed miss: a grudge-target attack
(neutral mobs) can target a stale remembered position if the player has
moved; ordinary hostile pursuit always matches.

`encode_damage_event` (vanilla's clientbound route for mob damage, needing
a damage-type registry id per source) is still absent; `encode_hurt_
animation` is sent instead for both players and mobs — same pixels, a
different packet than a real vanilla client would see.

## How to change it

- **Adding a combat attribute or item stat**: emit another `Modifier` from
  `item_modifiers` — nothing downstream enumerates attributes, so a new one
  flows through `apply_equipment` unedited. Gates written against the flat
  `1.0` player base damage will need updating once real weapon damage lands.
- **Cooldown-scaled damage/crit-bonus formula server-side**: needs a
  server-tracked attack-strength ticker (client-only today) plus a
  weapon/item damage model — both disclosed gaps, not new scope.
- **Real attacker-facing knockback direction server-side**: per-connection
  yaw is already tracked; swap it into `attack_direction` in place of the
  attacker-position→target stand-in.
- **The sweep damage mechanic**: a distinct entities-in-a-box loop, not a
  multiplier on the one hit already landed. Needs the same ticker/item-tag
  prerequisites as cooldown scaling above.
- **Adding `encode_damage_event`**: a new optional `ServerProtocol` method
  needing a damage-type registry id resolved per source; the client-side
  consumer chain already exists end to end.
- **Regenerating damage types** after a version bump: `just
  regen-damage-types`. The real jar is `.cache/mc/26.2/versions/26.2/
  server-26.2.jar` — the outer bundler jar contains none of these paths and
  searching it looks like the version dropped the data.
- **Shield blocking** is unbuilt entirely — needs an item-data model
  (`BlocksAttacks`) this workspace doesn't have; not a `damage.rs` gap.

## Configuration

- `ENTITY_REACH = 3.0` (`sim.rs`) — no creative/attribute modifier applied.
- `HURT_DURATION_TICKS = 10` (`lodestone-ecs/src/ingest.rs`).
- `HURT_OVERLAY_ALPHA_BYTE = 178` (`lodestone-render/src/entity_pipeline.rs`)
  — not configurable, matches vanilla's overlay texture exactly.
- Attack-strength delay has no standalone constant — computed fresh as
  `20.0 / attack_speed_attribute`; the only literal is the registry default
  `4.0` unarmed speed (`lodestone-entity/src/attribute.rs`).
- `InputAction::Drop` default `KeyQ` (`keybinds.rs`).
- Crit particle candidate count: `16` per tick, not configurable.
- `PLAYER_BARE_HAND_ATTACK_DAMAGE = 1.0`, `SPRINT_ATTACK_KNOCKBACK_POWER =
  0.5` (`lodestone-server/src/server.rs`).
- Damage types: no env vars/features, table is compiled in. `LODESTONE_
  REGEN=1` switches the drift test from assert to regenerate.

## Dependencies

- `lodestone_model::{ClientAction::InteractEntity, EntityInteraction,
  DropSelectedItem, DropSelectedItemStack, UseItem, ReleaseUseItem}` and the
  v26-2 adapter's encoders for all of them.
- `lodestone_ecs::entity::{Position, EntityKind, HurtTime, DeathTime,
  Attributes}`, `lodestone_ecs::player::{PhysicsState, AttackStrengthTicker}`.
- `lodestone_entity::{attribute, equipment, damage::{apply_reductions,
  HurtCooldown, Defenses, DamageFlags}}`.
- `lodestone_data::damage_types` (generated table) and `lodestone_data::
  entity_census` (living-entity checks shared with the crit condition).
- `lodestone_physics::knockback::{knockback_impulse, attack_direction}` and
  `lodestone_entity::ai::navigating_mob::NavigatingMob::apply_knockback`.
- `lodestone_particle::emit::{crit, sweep_attack}`.
- `lodestone_server::{MobHandle, ServerBound::{Attack, PlayerInput}}` — zero
  cycle risk, since `lodestone-physics` depends on nothing.
- See [`mob-ai.md`](./mob-ai.md) for the pursuit/melee goals feeding
  mob-on-player damage, and [`mob-spawning.md`](./mob-spawning.md)
  for mob-side equipment.
