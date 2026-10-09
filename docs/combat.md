# Combat

## What it is

Melee and ranged attack resolution end to end: swinging and targeting, the attack packet, knockback, the attack-cooldown ticker and crosshair indicator, hurt and death feedback, equipment-derived combat attributes, and the damage-type registry that tags how a hit is reduced.

## How it works

### Combat sessions

The combat enter/end packets announce the server's combat-tracking lifecycle, not hit feedback. `SessionCombat` on the local player holds the latest packet as active or ended with the exact duration; `Sim::combat_session` reads it and the F3 HUD shows `Combat: active` or `Combat: ended (N ticks)`, and nothing before a packet arrives. Repeated packets replace the record; no client-side clock is inferred, and an absent packet differs from a zero-duration end.

### Swing, targeting and the attack packet

`Sim::begin_attack` (`crates/lodestone-shell/src/sim.rs`) switches on the ray hit and always swings: entity sends the attack packet, block arms hold-to-mine, miss only swings. Entity targeting (`Sim::update_entity_target`, `EntityRayTarget` in `interact.rs`) uses `ENTITY_REACH = 3.0` against block `REACH = 4.5`, clamped to a nearer block hit. The creative reach bonus is not tracked.

`Sim::attack_entity` sends `ClientAction::InteractEntity { interaction: Attack, .. }` immediately, encoded as the 26.x `Attack` packet. The packet carries only the target id; damage is server-authoritative. Server-side, `ServerBound::Attack { entity_id }` records a main-hand swing in `PlayerRegistry` and reaches `MobHandle::with(|sim| sim.attack(..))`, an `Arc<Mutex<_>>` onto the live `MobSim`. Plain `minecraft:interact` decodes to `Ignored`.

`ServerBound::Swing` appends to the `PlayerRegistry` log; each connection captures its cursor atomically with registration, so swings during bootstrap stay visible and earlier ones are excluded.

### Knockback

A set-entity-motion packet naming the local player overwrites `PhysicsState.velocity` directly (the field `player_physics` integrates), not the generic `Velocity` component; remote entities use the component.

- The direction is `target - attacker` (the target flies away). A comment labelled the other way has shipped as a bug; derive the sign from the source.
- The flat `0.4` impulse applies on every hit; only the sprint bonus (`SPRINT_ATTACK_KNOCKBACK_POWER = 0.5`, when the attacker's tracked `sprinting` flag is set) is gated. The weapon term resolves to the attacker's `minecraft:attack_knockback` attribute (default `0.0`), so a non-sprint player hit's power really is `0.0` today.
- Server-side direction is attacker position to target (a stand-in for facing); per-connection yaw is tracked and swapping it in is the remaining wire-up.
- Mobs have no persistent velocity or drag, so knockback is a one-tick displacement.

### Attack cooldown and indicator

`AttackStrengthTicker` on the local player increments per `GameTick` and resets on every attack. The delay is `(1.0 / attack_speed) * 20.0` from `minecraft:attack_speed` in `Attributes` (default `4.0`, so 5 ticks unarmed); weapon modifiers arrive via `update_attributes`. `Sim::attack_strength_scale` clamps ticker over delay to `0.0..=1.0`.

`HudFrame::attack_cooldown` shares the crosshair's visibility gate; `OFF`, `CROSSHAIR` and `HOTBAR` all reach pixels (hotbar is a distinct 18x18 sprite pair). Not built: the full-charge "ready" icon (it needs live target liveness and range in `HudFrame`); full charge draws nothing.

### Hurt and death feedback

`EntityDamaged`/`EntityHurtAnimation` set `HurtTime` to 10, counting down per tick. `EntityStatus` byte 3 sets `DeathTime` counting up from 0 (absence means alive, so the first death tick draws upright). The overlay is a blend toward red, not a multiply (a multiply crushes toward black), at flat alpha `178/255` while `hurtTime > 0 || deathTime > 0`, on every drawn living entity except the local first-person view. Death adds a fall-over rotation `sqrt((deathTime - 1)/20 * 1.6)` clamped to 1, saturating at `deathTime == 13.5`.

Not wired: the local player's third-person body (no ingest entity) and the camera-roll damage tilt, blocked on `Camera` gaining a roll degree of freedom (`ViewBob::hurt`/`BobFrame` already compute it). There is no full-screen damage overlay or camera shake to build.

The protocol 766 split death-combat packet is decoded independent of health: player and killer ids are routing fields and its JSON message becomes `ClientEvent::Death`, entering the shared session, shell and driver route, so the death screen and auto-respawn do not depend on the encoder or damage source.

### Shield, bow and generic use

Release-on-use items (shield, bow) need `ClientAction::ReleaseUseItem` produced by a release edge reaching `Sim::end_use`, and `Sim::use_item_live` must fall through to `Sim::use_item_generic` after a non-consuming result: entity, no-target and block branches do (the block branch only when nothing was placed and the item is not itself placeable). Divergence: with no local interact-success prediction every entity interact falls through, which can send one redundant use packet when boarding a vehicle.

### Crit and sweep particles

Crit is client-side dual simulation (the packet has no crit flag): full-strength attack (scale `> 0.9` at partial tick `0.5`), airborne, not sprinting, not on ground, climbable or in water, living target. One tick of the 16-candidate unit-sphere burst spawns (the reference emitter runs 3 ticks; there is no persistent per-attack emitter). The sweep-attack particle reaches pixels through the `LEVEL_PARTICLES` path; sweep damage (an entities-in-a-box loop with its own knockback) is unbuilt and needs a server attack-strength ticker and a sword tag.

### Equipment-derived stats

`lodestone_entity::equipment` feeds the reference's attribute modifiers into the `AttributeMap`: `(slot, item id)` to `item_modifiers` (armour and tool material tables) to `apply_equipment` inserting `Modifier { id, amount, AddValue }` to the attribute fold to `defenses_from_attributes`/`attack_damage_from_attributes`. Keying by the real modifier id means two helmets cannot stack and a new weapon replaces the old.

`PlayerInventory::combat_equipment` folds feet 36, legs 37, chest 38, head 39, off-hand 40 and the selected hotbar slot as main hand (not native slot 0). Gotchas:

- A modifier applies only in the slot it is published for, hence `(slot, item)` pairs.
- The defense maker's argument order is boots first (boots, legs, chest, helm, body); totals can coincide across a swap, so only a per-piece assertion catches it.
- A weapon's damage is baseline plus material bonus (diamond sword `3.0 + 3.0`); trident `8.0` and mace `5.0` are flat.
- The player's base `attack_damage` is `1.0`, not the registry default `2.0`.
- Not modelled: enchantment protection and effectiveness (neutral `Defenses` fields are accurate), and shield blocking (needs an item-data model for blocking). Mob equipment feeds the same functions (see [mob spawning](mob-spawning.md)).

### Damage types and tags

The `minecraft:damage_type` registry (51 types, 36 tags, from the 26.3 jar; 26.2 is the same) is generated into `crates/lodestone-data/src/generated/damage_types.rs` and read through `DamageFlags::for_damage_type` (`lodestone-entity/src/damage.rs`), which maps five tags onto the five pipeline stages: `bypasses_armor`, `bypasses_effects`, `bypasses_resistance`, `bypasses_enchantments`, `bypasses_cooldown`. Behaviour keys off tags, never type names.

- Tag membership is a transitive closure (7 of 36 tag files reference tags), resolved at generation so `is_in` is one bit test.
- `bypasses_cooldown` is a real tag with zero members (a test asserts it).
- `no_wolf_retaliation` is new in 26.3 with one member (`sulfur_cube_hot`), unread. Bit order is alphabetical and shifted after `no_knockback`, so never persist or send a raw mask; a new tag variant goes in its alphabetical slot because the discriminant is the bit index.
- `minecraft:generic` is `bypasses_armor`-tagged; test armour with `minecraft:mob_attack`.
- `message_id` is not the type name (`mob_attack` is `"mob"`, `ender_pearl` is `"fall"`).
- The generated table is distinct from the hand-written `src/damage_types.rs` accessor, and indices are not network ids (those come from registry-sync order).

### Server-side melee

`ServerBound::Attack` goes to `MobHandle::with` to `SimMob::apply_damage` (`HurtCooldown`, `apply_reductions`) to `lodestone_physics::knockback::knockback_impulse` to `NavigatingMob::apply_knockback`. No reply is sent; `EntityStreamer::sync` carries the result. `PLAYER_BASE_ATTACK_DAMAGE = 1.0` is the empty-hand base and the weapon modifiers feed `PlayerInventory::combat_stats`. There is no server attack-strength ticker, so every hit is full strength with no critical multiplier.

Mob-on-player damage: `MobSim` matches an attack's target position against its player list, queues a `PlayerHit`, and `serve_play`'s periodic vitals tick drains it through `PlayerVitals::apply_damage` with the player's armour. A grudge-target attack can aim at a stale remembered position. `encode_damage_event` (needing a damage-type registry id per source) is absent; `encode_hurt_animation` is sent instead for players and mobs.

## How to change it

- **New stat**: emit another `Modifier` from `item_modifiers`; nothing downstream enumerates attributes. Gates against the flat `1.0` base need updating once weapon damage lands.
- **Cooldown-scaled damage, crit bonus, sweep damage**: need a server-tracked attack-strength ticker and a weapon/item damage model.
- **Facing knockback**: swap per-connection yaw into `attack_direction`.
- **`encode_damage_event`**: a new optional `ServerProtocol` method needing a damage-type id per source; the client consumer exists.
- **Regenerate damage types** after a bump with `just regen-damage-types`. The data lives in the inner server jar under `.cache/mc/<version>/versions/<version>/`; the outer bundler jar has none of it.
- **Shield blocking**: unbuilt; it needs an item-data model, not a `damage.rs` change.

## Configuration

- `ENTITY_REACH = 3.0` (`sim.rs`); `HURT_DURATION_TICKS = 10` (`lodestone-ecs/src/ingest.rs`); `HURT_OVERLAY_ALPHA_BYTE = 178` (`lodestone-render/src/entity_pipeline.rs`).
- Attack delay is computed as `20.0 / attack_speed`; the only literal is the `4.0` default in `lodestone-entity/src/attribute.rs`.
- `PLAYER_BASE_ATTACK_DAMAGE = 1.0`; `SPRINT_ATTACK_KNOCKBACK_POWER = 0.5` in `lodestone-server/src/server.rs`; crit candidates 16.
- Damage types are compiled in; `LODESTONE_REGEN=1` makes the drift test regenerate instead of assert.

## Dependencies

- `lodestone_model` client actions and the v26 adapters' encoders.
- `lodestone_ecs` (`Position`, `HurtTime`, `DeathTime`, `Attributes`, `PhysicsState`, `AttackStrengthTicker`).
- `lodestone_entity::{attribute, equipment, damage}`, `lodestone_data::{damage_types, entity_census}`, `lodestone_physics::knockback`, `lodestone_particle::emit::{crit, sweep_attack}`.
- `lodestone_server::{MobHandle, ServerBound}`.
- See [mob AI](mob-ai.md) and [mob spawning](mob-spawning.md).
