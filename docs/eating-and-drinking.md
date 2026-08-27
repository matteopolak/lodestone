# Eating and drinking

## What it is

Everything visible and audible about a consume, on both sides of the seam: the
first-person dip and jitter of the food toward the mouth, the crumbs that carry the
food's own texture, the third-person raised arm, and the eating/drinking/burp
sounds. The *gameplay* half — nutrition, saturation, the use clock,
cancel-on-release — landed earlier and lives in
[`lodestone_server::item_use`](./hunger.md); this is the half that makes it look and
sound like eating.

Owner report that started it: *"eating works but theres no animation or particles."*

## The single most useful fact here

**Vanilla has no third-person eating animation, and no `ArmPose` for eating.**
`HumanoidModel.ArmPose` has eleven variants and none is `EAT` or `DRINK`, and
`AvatarRenderer.getArmPose` deliberately omits both `ItemUseAnimation` values from
its `if` chain — so a consuming entity falls through to `ArmPose.ITEM`, the ordinary
held-item raise. The whole distinctive motion is *first person*, in
`ItemInHandRenderer.applyEatTransform`.

So "add an `ArmPose::Eating`" is the wrong shape of change. What was actually
missing in third person was `ArmPose::Item` itself, which this repo had listed as
`Empty` along with `BLOCK`, `SPYGLASS` and the rest.

## The client/server split is vanilla's, not a convenience

Vanilla runs one method — `Consumable.emitParticlesAndSounds` — on both sides, and
each side silently drops the half it cannot do:

| half | mechanism | lands on |
|---|---|---|
| particles | `ServerLevel.addParticle` is a no-op | **client**, always predicted |
| sound | `Entity.playSound` → `level.playSound(null, …)`, and `ClientLevel.playSeededSound` skips it because `except == null` is not the local player | **server**, always broadcast |

Both directions of getting this wrong are silent. A client that also plays the
sound double-plays it against a real 26.2 server. A server that also emits particles
sends nothing anyone can see. And note this is the **opposite** of the block-break
case in [sound playback](./sound-playback.md), where the acting player's own sound is
client-predicted and must be excluded from the broadcast: here the eater hears *only*
the broadcast, so `publish_effect` (no exclusion) is right and
`publish_effect_except` would silence the one player who is eating.

## How it works

| stage | where |
|---|---|
| the `minecraft:consumable` component (duration, eat/drink, particle flag, sound) | `lodestone_game::consumable` |
| the effect cadence | `lodestone_game::consumable::should_emit_consume_effects` |
| local use clock, counting **up** | `lodestone_ecs::player::ItemUseTicks`, ticked by `tick_item_use` |
| the press-edge use gate | `lodestone_shell::sim::actions::item_can_start_use` — includes `Player.canEat`'s full-hunger refusal |
| the five-way join that decides "a consume is happening" | `lodestone_shell::consume::ConsumeState::resolve` |
| the crumbs | `lodestone_shell::consume::emit_consume_particles` → `lodestone_particle::emit::spawn_item_particles` → `item_particle` |
| item sprite for a crumb | `SpriteSource::Item(id)` → `Particles::sprite_rect` → `BlockModels::item_particle_uv` |
| first-person pose | `Sim::consume_usage_time` → `RenderState::set_item_use_source` → `first_person_item_mesh_with_use` → `lodestone_render::entity::first_person_eat_matrix` |
| third-person pose | `entities::arm_pose_for` → `ArmPose::Item` → `Skeleton::pose_arms_for_item` |
| server sounds | `lodestone_server::effects::{item_consumed_tick, item_consume_finished, player_burped}`, published on `BlockTickFeed`'s effect lane |

### The counter already existed

The load-bearing question at the start of this work was whether the client tracks its
own use *duration*, since `Sim::using_item()` is a bare `bool` and every vanilla
consume expression is a function of remaining ticks. It does:
`lodestone_ecs::player::ItemUseTicks` is an `Option<u32>` armed at the press edge by
`Sim::use_item_live`, advanced by `tick_item_use` in `TickSet::Physics`, and taken at
the release edge. It counts **up**, because counting up *is*
`getTicksUsingItem()` and needs no per-item `getUseDuration` for a bow (whose
duration is 72000). A consume does have a real duration, so
`consumable::remaining_ticks` is the one place the direction is inverted.

Nothing had to be added. What was missing was a consumer.

### `ConsumeState::resolve` is a named composition, on purpose

Four conditions have to hold: the use button is down, a use clock is running, the
selected item is consumable, and the clock has not passed that item's duration. The
particles and the bob both need exactly this answer, tick for tick, and a bug in the
seam between two individually-correct halves has nothing for a test to point at. So
it is one symbol both sides call.

The **duration bound is not a tidiness check**: `use_item_live` arms `ItemUseTicks`
on the press edge for *any* item that can enter vanilla's use state. A food that
the server refuses at a full hunger bar is filtered at that same edge by
`item_can_start_use`, so it never arms `UsingItem` or the movement slowdown;
`ConsumeState::resolve` repeats the hunger check for the animation and particles.
The duration bound then stops an accepted consume at the item's own end tick.
`Sim::restart_completed_consumable_if_held` clears that completed local state and
re-enters `use_item_live` on the same 20 Hz tick if the button is still down. The
restart therefore repeats `item_can_start_use` rather than preserving slowdown by
habit: a full-bar food use remains refused, while a still-edible stack starts its
next bite without another OS press event.

### The cadence, and why the count is the discriminating quantity

`Consumable.shouldEmitParticlesAndSounds` is a **conjunction**:

```java
int itemUsedForTicks = this.consumeTicks() - useItemRemainingTicks;
int waitTicksBeforeUseEffects = (int)(this.consumeTicks() * 0.21875F);
boolean isValidTime = itemUsedForTicks > waitTicksBeforeUseEffects;
return isValidTime && useItemRemainingTicks % 4 == 0;
```

Either clause alone produces something that looks like a working eat animation, which
is why "some particles spawned" is not a test. For the default 32-tick food:

| hypothesis | emissions |
|---|---|
| both clauses (correct) | **6** — `remaining` 24, 20, 16, 12, 8, 4 |
| modulo only | 8 — adds `remaining` 32 and 28 |
| start fraction only | 24 |

`a_food_emits_six_times_over_its_use` computes all three from the constants and
requires the measurement to land on the first. Under a neuter that drops the start
fraction it printed `[32, 28, 24, 20, 16, 12, 8, 4]` — the wrong hypothesis's exact
vector.

Per-emission counts are **5** particles (`ItemStack.onUseTick`) and **16** on the
final bite (`Consumable.onConsume`). Only two items in 26.2 have a non-default
duration — dried kelp at 16 ticks (3 emissions) and honey bottle at 40 (7) — so a
constant schedule would be wrong for both.

### `applyEatTransform`, and the exponent

```java
float currUsageTime = player.getUseItemRemainingTicks() - frameInterp + 1.0F;
float scaledUsageTime = currUsageTime / itemStack.getUseDuration(player);
if (scaledUsageTime < 0.8F) {
   float extraHeightOffset = Mth.abs(Mth.cos(currUsageTime / 4.0F * (float)Math.PI) * 0.1F);
   poseStack.translate(0.0F, extraHeightOffset, 0.0F);
}
float eatJiggle = 1.0F - (float)Math.pow(scaledUsageTime, 27.0);
int invert = arm == HumanoidArm.RIGHT ? 1 : -1;
poseStack.translate(eatJiggle * 0.6F * invert, eatJiggle * -0.5F, eatJiggle * 0.0F);
poseStack.mulPose(Axis.YP.rotationDegrees(invert * eatJiggle * 90.0F));
poseStack.mulPose(Axis.XP.rotationDegrees(eatJiggle * 10.0F));
poseStack.mulPose(Axis.ZP.rotationDegrees(invert * eatJiggle * 30.0F));
```

Four things about it are easy to get wrong, and none is visible in a still frame:

1. **`^27` is the animation's whole character.** A linear `1 - t` agrees with it only
   at the endpoints: at `remaining = 30` of a 32-tick food the real jiggle is
   `0.5755` against `0.03125` — 18× — and by `remaining = 24` it is `0.9985`
   against `0.21875`. Linear reads as the item *drifting* to the mouth over the whole
   use; vanilla snaps it there in about two ticks and then bobs.
2. **`scaledUsageTime < 0.8F` bounds *remaining* time, so the bob opens late.**
   `currUsageTime` counts down, so the bob is suppressed for the first 20% of a use
   and runs for the last 80%. Reading `< 0.8` as "near the start" inverts it.
3. **`applyItemArmTransform` comes *last*.** `EAT` and `DRINK` have
   `hasCustomArmTransform() == true`, so `submitArmWithItem` skips the pre-switch
   offset and the case applies it *after* the eat transform. Putting it first — the
   order every other pose here uses — rotates the item about the camera instead of
   about the hand and swings it across the screen.
4. **There is no swing.** The `player.isUsingItem()` branch never reaches
   `swingArm`, so `ItemSwingTerms` and `first_person_item_attack_chain` do not apply
   while consuming. Left-clicking mid-bite must not move the item.

`currUsageTime` can exceed the duration on the very first tick, which makes the
jiggle **negative** for that instant. That is vanilla, not a missing clamp — the item
flicks away before coming to the mouth.

### The crumbs

`BreakingItemParticle` and `TerrainParticle` have **byte-identical**
`getU0`/`getU1`/`getV0`/`getV1` overrides (a quarter sub-sprite, `uo`/`vo` each
`nextFloat() * 3.0F`), the same `gravity = 1.0F` and the same `quadSize /= 2.0F`, so
`Behaviour::Terrain` describes both and only the sprite source differs — plus the
absence of `TerrainParticle`'s `0.6` grey, since an item crumb is full-bright.

The one trap is the velocity. `BreakingItemParticle` chains to the zero-velocity
constructor and then does `xd *= 0.1F; … xd += xa;` — a **plain multiply** of all
three components. `Particle::set_power` is the wrong tool: it deliberately preserves
`with_velocity`'s `0.1` upward bias across the scale, and vanilla scales that bias
too (to `0.01`), so `set_power` leaves the crumbs rising about ten times too fast.

`spawnItemParticles` builds both the spawn offset and the velocity in a body-local
frame (`+z` forward, `0.6` ahead of the eye, `0.3..0.9` *below* it) and rotates by
`-xRot` then `-yRot`. A sign error there puts the crumbs behind the head, where first
person cannot see them — so it reads as "no particles" rather than as a wrong
direction.

## How to change it, and the gotchas

- **Adding a consumable** is one row in `lodestone_game::consumable::CONSUMABLES`
  (keep it sorted — binary search). Its `minecraft:food` half is a separate row in
  `lodestone_server::item_use::FOODS`; the two tables have 43 and 40 rows and the
  difference is exactly `milk_bucket`, `potion` and `ominous_bottle`, which are
  drinkable and not food.
- **`Consumable.Builder::soundAfterConsume` is not a `sound` override.** It lowers to
  `onConsume(new PlaySoundConsumeEffect(…))`, so an ominous bottle's
  `item.ominous_bottle.dispose` is a completion effect and its `sound` field is still
  `entity.generic.drink`. Transcribing it into the `sound` column makes the bottle
  play its disposal noise six times while drinking.
- **`minecraft:food` does not imply `minecraft:consumable`.** The four mob buckets
  carry `FOOD` with no `CONSUMABLE` and are not edible — vanilla's `Fox` goal tests
  both components for exactly this reason.
- **A drink has no particles.** `hasConsumeParticles` is false for all four drinks, so
  the particle path must consult the flag and not the animation. A potion that throws
  crumbs passes any presence check.
- **Emit on ticks, not frames.** The cadence is `remaining % 4 == 0` and must be
  evaluated once per 20 Hz tick; driving it from the render loop turns six bursts into
  hundreds, which reads as a wrong particle count rather than as a scheduling bug.
- **The server latches what it already emitted.** `ItemInUse::last_effect_remaining`
  exists because the 50 ms timer arm and `MobSim`'s tick counter are not the same
  clock: without the latch, a timer that fires twice inside one mob tick re-passes the
  predicate and doubles the sound.
- **Do not reach for a clock anywhere on this path.** Every duration is in ticks;
  `SystemTime::now`/`Instant::now` trap on wasm32, and a particle engine here has
  already killed the browser tab that way once.

## Not done

- ~~A merely-*held* item still gets `ArmPose::Empty`.~~ **Landed, and the premise
  written here was wrong.** This entry claimed vanilla's fallthrough gives every armed
  humanoid a raised arm, and that widening it needed a re-baseline of
  `bow_draw_pose_pixels` and `aggressive_bow_pose_pixels`. Both claims came from reading
  `AvatarRenderer.getArmPose` and generalising it to mobs. **`HumanoidMobRenderer.getArmPose`
  ends `? SPEAR : EMPTY`**, so a mob's arms hang in vanilla too and the fallthrough is
  avatar-only. It changes no mob silhouette; both gates use a skeleton subject and a
  zombie control, so **neither needed re-baselining**. See
  [arm poses](./item-use-arm-poses.md) for the type set, the controls that were run, and
  why the armour-stand row is the one that makes the wrong scope visible.
- **`BLOCK`, `SPYGLASS`, `TOOT_HORN`, `BRUSH`, `THROW_TRIDENT` and `SPEAR` now resolve
  to `ArmPose::Item`**, which is a *closer* wrong answer than `Empty` (the arm goes up,
  which is the half those poses share) but is not right: vanilla reaches each of them
  before the fallthrough. `THROW_TRIDENT` additionally needs the one-handed dispatch
  described in [arm poses](./item-use-arm-poses.md) — though `ArmPose::Item` is itself
  the first one-handed pose here and poses only the holding arm, so that groundwork is
  now laid.
- **The off hand is not modelled.** `ItemUseTicks` and `UsingItem` are hand-free
  scalars and the shell's first-person path draws the main hand only, so drinking from
  the off hand animates the main-hand item. Fixing it means putting a hand on the use
  state, not more render work.
- **The finish burst of 16 crumbs is not emitted.** The client learns a consume
  finished by the server's `set_health`/`container_set_slot` reply, not from its own
  clock, so `Consumable.onConsume`'s larger burst has no trigger on this side yet.
  `FINISH_PARTICLE_COUNT` is carried so the number does not have to be rediscovered.
- **`onConsumeEffects` are still unimplemented** — a golden apple's regeneration,
  chorus fruit's teleport, milk clearing effects, `usingConvertsTo` (a stew leaving a
  bowl). Those are gameplay and belong with `lodestone_server`'s effect model.
- **A wasm32-served connection never finishes a consume at all**, so it never reaches
  the finish sounds. That is the pre-existing documented gap at the second
  `serve_connection` loop (no `tokio::time`, hence no per-tick timer), not something
  this change introduced.

## Configuration

None. No env vars, no feature flags. The sounds need the sample corpus
[sound playback](./sound-playback.md) describes (`xtask fetch-sounds`); without it the
registry resolves and nothing plays, which that doc's startup census reports.

## Dependencies

- `lodestone-game`'s `consumable` module — the shared component table and cadence.
- `lodestone-ecs`'s `ItemUseTicks` / `tick_item_use`, `SelectedSlot`, `SessionMenus`.
- `lodestone-particle`'s `SpriteSource::Item`, `emit::item_particle` and
  `emit::spawn_item_particles`.
- `lodestone-render`'s `first_person_eat_transform`/`_chain`/`_matrix`,
  `first_person_item_mesh_with_use`, `eat_usage_time`, `ArmPose::Item`,
  `BlockModels::item_particle_uv`.
- `lodestone-shell`'s `consume` module, `Particles::sprite_rect`,
  `gpu::ItemUseSource`, `Sim::consume_usage_time`, `entities::arm_pose_for`.
- `lodestone-server`'s `effects::{item_consumed_tick, item_consume_finished,
  player_burped}` and `item_use::FOODS`.
- Reference only, never transliterated: `.cache/mc/26.2/{src,client-src}`'s
  `Consumable`, `Consumables`, `FoodProperties`, `ItemStack`, `LivingEntity`,
  `ItemInHandRenderer`, `BreakingItemParticle`, `TerrainParticle`, `HumanoidModel`,
  `AvatarRenderer`.
