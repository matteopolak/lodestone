# Status effects

## What it is

The general server-side status-effect registry: duration countdown, amplifier
stacking with vanilla's hidden-effect chain, and the periodic poison / wither /
regeneration / hunger ticks. `crates/lodestone-server/src/mob_effects.rs` holds the
rules; `/effect give` and `/effect clear` write to it; `server.rs`'s vitals timer ticks
it and applies the result through `PlayerVitals`.

**This is the registry `lodestone-physics::effect` is scoped *not* to be.** That
module classifies which effects the movement integrator reads directly and which fold
into the `MOVEMENT_SPEED` attribute; it holds no duration, no stacking and no
periodic tick. This is the shared store it should read from — `classify` takes exactly
an id and an amplifier, which is what `ActiveEffects::active()` hands back.

Before this landed, nothing applied an effect. `crate::brewing` knew the effect
*names* a potion recipe produces and there was nowhere for one to land.

## How it works

### The periodic interval is a shift, and at high amplifiers it fires every tick

```java
int interval = 25 >> amplification;
return interval > 0 ? tickCount % interval == 0 : true;
```

Once the shift reaches zero the effect applies **every tick**, not never. Poison
bottoms out at amplifier 5, wither and regeneration at 6. Dropping the `interval > 0`
guard either divides by zero or — the plausible version — returns `false`, which makes
**Poison VI harmless**.

| effect | base interval | effect | health guard |
|---|---|---|---|
| `poison` | 25 | 1.0 damage | **only if `health > 1.0`** |
| `wither` | 40 | 1.0 damage | **none — wither can kill** |
| `regeneration` | 50 | heal 1.0 | only if hurt |
| `hunger` | every tick | `0.005 * (amplifier + 1)` exhaustion | — |
| `instant_health` | instant | heal `4 << amplifier` | — |
| `instant_damage` | instant | `6 << amplifier` damage | — |

**Poison cannot kill and wither can**, and the asymmetry is one `if` in vanilla.
Instant damage is `6 <<`, instant health `4 <<` — different constants, which factoring
the two into one function loses.

### `tickCount` is the remaining duration, not an age

`tickServer` passes `this.duration` for a finite effect and `target.tickCount` for an
infinite one, so the modulo counts **down**. A 210-tick poison first fires on tick
**11** (remaining 200, the first multiple of 25), not tick 1. Counting up lands on a
different set of ticks whenever the duration is not a multiple of the interval.

An effect is removed on the tick its duration reaches **zero**, not the tick after —
`tickServer` returns `hasRemainingDuration()` *after* ticking down. An off-by-one here
gives every effect in the game one extra tick.

### Stacking has a hidden-effect chain

| new vs current | result |
|---|---|
| higher amplifier | takes over; if *shorter*, the current one is pushed onto a hidden chain and resurfaces |
| equal amplifier, longer | duration replaced |
| equal amplifier, shorter | ignored |
| lower amplifier, longer | becomes a hidden effect behind the current one |
| lower amplifier, shorter | ignored entirely |

So "does a lower amplifier get ignored or replace" is **neither**: it is remembered.
Strength II then Strength I leaves Strength II now and Strength I afterwards — and the
queued instance's **own clock runs while it waits**, so it surfaces at 300 ticks, not
400. A registry keeping only the strongest loses the tail; one keeping only the newest
loses the strength.

An infinite duration (`-1`) is longer than everything, which is why
`isShorterDurationThan` is not a plain comparison.

### A splash/lingering potion's impact burst

`mob_effects::potion_splash_effects` ports `ThrownSplashPotion.onHitAsPotion`: every
effect in the potion's built-in list (`lodestone_data::potion::potion_built_in_effects`)
is scaled by `splash_scale(distance_sq) = 1.0 - sqrt(distance_sq) / 4.0` (`1.0` on a
direct hit, `0.0` at the four-block edge of the blast) and split in two:

* An **instantaneous** effect (`instant_health`/`instant_damage` — the only two a
  potion's own list can carry) scales the *amount*:
  `splash_instant_amount = floor(scale * base_amount + 0.5)`.
* A **timed** effect scales the *duration*:
  `splash_timed_duration = floor(scale * base_duration * duration_scale + 0.5)`, then
  dropped outright (not applied at a token duration) if that leaves it
  `endsWithin(20)` ticks (`splash_would_be_dropped`).

`crate::mobs::projectiles::resolve_potion_splash` is the consumer: for every
splash/lingering impact (block **or** entity — vanilla's own search is the whole blast
AABB, not just whatever the collision sweep named as the target) it finds every living
mob within `SPLASH_RANGE_SQ` (16.0, i.e. 4 blocks) of the impact point and applies the
result — health for an instant effect, `SimMob::apply_effect` (this same module's
stacking rule) for a timed one.

`duration_scale` is always `1.0`: this build's `ItemComponents` does not model
`minecraft:potion_duration_scale`, so every splash uses the potion's unscaled base
duration. And `customEffects` (a `/give`-authored custom effect on a splash potion) is
silently absent for the same reason — `ItemComponents` carries only the potion's own
built-in `minecraft:potion_contents` `potion` id, not its patch's custom-effects list.

## How to change it

* **Another periodic effect**: add it to `periodic_effect`. The interval and the amount
  both come from its own `MobEffect` subclass — do not derive one from another.
* **A consumer**: read `ActiveEffects::amplifier_of` or `::active()` rather than keeping
  your own copy. That is the whole reason this is one store.
* **Another producer**: `Effect::ApplyEffect` / `Effect::ClearEffects` in
  `crate::commands::effect`, applied by `server.rs`'s `apply_own_effect`.
* **The splash formula**: `mob_effects::potion_splash_effects` is the one entry point
  a caller needs; its pieces (`splash_scale`, `splash_instant_amount`,
  `splash_timed_duration`, `splash_would_be_dropped`) are separated because each is
  independently testable against `AbstractThrownPotion`'s own named constant or
  `MobEffectInstance` method. The entity search that calls it is
  `crate::mobs::projectiles::resolve_potion_splash`.

### Gotchas

* Poison's `health > 1.0` guard is applied **inside** the registry, so an amount that
  reaches `PlayerVitals::apply_effect_damage` has already been allowed. Applying the
  guard twice is harmless; applying it nowhere makes poison lethal.
* `apply_effect_damage` deliberately has **no i-frame gate**. Vanilla routes these
  through `hurtServer`, but the interval is the effect's own (25 or 40 ticks, both
  longer than the 20-tick window at amplifier 0), so a gate would be inert at low
  amplifiers and would swallow hits at high ones where the shift makes poison fire
  every tick.
* `DeathCause::Wither` covers **both** poison and wither. Vanilla uses `magic` for
  poison and `wither` for wither; both produce the same number here, so the divergence
  is only in the death message.
* `/effect give <targets> <effect>` with no `<seconds>` is **infinite**, which is
  vanilla's default — not 30 seconds.

## What is not here

* **Attribute-modifier effects** (`speed`, `slowness`, `health_boost`, `absorption`).
  These need an attribute system. `lodestone_physics::effect` already classifies the
  movement ones; the store is here for it to read.
* **A lingering potion's own `AreaEffectCloud` entity.** Real vanilla behaviour is a
  cloud with a radius, a radius-per-tick shrink, a duration and a reapplication delay,
  so the burst lands repeatedly over up to 30 seconds. That entity does not exist here,
  so `resolve_potion_splash` applies a lingering potion's burst exactly **once**, at
  impact — the same as a splash potion — rather than lingering. Tracked as a follow-up.
* **The `update_mob_effect` / `remove_mob_effect` packets.** Nothing encodes them, so a
  client's own effect HUD does not light up — the *consequences* (damage, healing,
  exhaustion) reach the player, the icon does not. A mob's own splash-applied effects
  ([`SimMob::effects`]) are even less visible: nothing renders a mob's status icons at
  all, and nothing yet ticks a mob's periodic poison/wither/regeneration — a splash
  lands the *instance*, but only a player's own vitals timer currently advances one.
* **`ambient` / `visible` / `showIcon`** and the blend state — purely presentational,
  and `/effect`'s `hideParticles` node is omitted rather than parsed-and-discarded.
* **Drinking a potion.** `crate::brewing` produces potion *items*; no item-use path
  turns one into an effect. (A **thrown** splash/lingering potion's impact-time burst
  is covered above — that path is separate from drinking.)
* **`/effect` targeting a mob.** `MobSim` now holds per-mob effect state
  ([`SimMob::effects`], populated by a splash/lingering impact), but the `/effect`
  *command* itself still only resolves player targets — a mob can carry an effect a
  splash gave it, but nothing can `/effect give`/`/effect clear` one directly yet.

## Dependencies

`mob_effects.rs` depends on nothing beyond `std` — no world, no RNG, no clock. The
producer path depends on `crate::commands`; the consumer path on `crate::vitals` and
`crate::food`.
