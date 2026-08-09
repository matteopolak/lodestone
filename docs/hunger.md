# Hunger

## What it is

Server-authoritative food: exhaustion accumulation from actions, the hidden
saturation buffer, the visible food level, natural health regeneration and
starvation damage. `crates/lodestone-server/src/food.rs` holds the rules as a pure
value type; `crate::vitals::PlayerVitals` owns one and applies its health
consequences; `crate::server` charges exhaustion at the sites that know an action
happened.

Before this landed there was no hunger simulation at all — the server sent a
hardcoded `food: 20, saturation: 5.0` on every `SetHealth`, so a real client's
haunches never moved.

## How it works

### The three-layer buffer

This is the part that gets skipped, and skipping it makes hunger deplete five times
too fast at a fresh spawn:

1. **Exhaustion** accumulates from actions, capped at `40.0`.
2. Each tick, exhaustion **strictly** above `4.0` (`EXHAUSTION_DROP`) is spent: `4.0`
   is subtracted and **one point of saturation** goes with it.
3. Only once saturation has reached `0.0` does the **food level** drop — and not at
   all on Peaceful.

So the visible bar does not move until the invisible one is empty.

### Exhaustion costs, and the one that is zero

Read off `FoodConstants` and `ServerPlayer.checkMovementStatistics`. The
per-distance terms are `constant * round(sqrt(dx²+dz²) * 100) * 0.01`, so the
per-**block** cost is just the leading constant.

| action | per block / per event | charged where |
|---|---|---|
| sprint | **0.1** | `server.rs`'s `PlayerMoved` arm |
| walk, crouch | **zero** | — (nothing to charge) |
| swim / eye underwater | 0.01 | **not charged yet** — see gaps |
| jump | 0.05 | **not charged** — no jump packet exists |
| sprint-jump | 0.2 | **not charged** — same reason |
| break a block | 0.005 | `destroy_block` |
| attack | 0.1 | the `Attack` arm |
| natural regen | 6.0 (slow) / amount spent (fast) | inside `FoodData::tick` |

**Walking costs nothing.** Vanilla writes it as a literal `0.0F` multiply rather
than by omitting the branch, and this is the single most commonly-wrong fact about
hunger. Charging it invents a depletion vanilla does not have.

### The cadence, exactly

The threshold test is `exhaustion > 4.0`, **strictly** greater, so exhaustion has to
reach `4.1` and the *k*-th drop lands on sprint block `40k + 1`:

| block | saturation | food |
|---|---|---|
| 41 | 4.0 | 20 |
| 161 | 1.0 | 20 |
| 201 | 0.0 | 20 |
| **241** | 0.0 | **19** |

A fresh spawn sprints **241** blocks before the visible bar moves — not the round
200 that dropping the strictness gives.

### Regeneration and starvation are one if/else chain

Four arms sharing **one** timer, so a player cannot regenerate and starve in the
same tick, and any arm not taken resets the timer:

| arm | condition | period | effect |
|---|---|---|---|
| saturated regen | `natural_regen && saturation > 0 && hurt && food >= 20` | 10 ticks | heal `min(sat, 6)/6`, exhaust by the amount spent |
| slow regen | `natural_regen && food >= 18 && hurt` | 80 ticks | heal `1.0`, exhaust `6.0` |
| starvation | `food <= 0` | 80 ticks | `1.0` damage, gated by difficulty |
| idle | otherwise | — | reset the timer |

The saturated arm is a **partial** heal — spending `3.0` saturation heals `0.5`, not
a heart.

**Regeneration pays for itself in food and then stops.** A player at 10 health with a
full bar and `5.0` saturation regenerates only **5.5** health: each heal charges
exhaustion, the charge eats saturation then food, and once food falls below `18` the
arms switch off. It settles at food 17. A player who keeps eating does reach the cap.

### The starvation difficulty gate is not "peaceful is safe"

`health > 10 || difficulty == HARD || (health > 1 && difficulty == NORMAL)`. So
**Easy and Peaceful still starve a player down to 10 health**, Normal to 1, Hard to
death. Peaceful's real protection is upstream: the depletion branch's own
`difficulty != PEACEFUL` guard means the food level never reaches zero there. Two
mechanisms; modelling only the obvious one gets Peaceful wrong in both directions.

## How to change it

* **A new exhaustion producer**: call `PlayerVitals::add_exhaustion` from the site
  that knows the action happened, with a constant from `crate::food`. **Guard it on
  `Abilities::for_mode(mode).invulnerable`** — vanilla's guard lives in
  `Player.causeFoodExhaustion`, and forgetting it makes a creative player starve.
* **Eating**: `FoodData::eat(nutrition, modifier)` applies
  `nutrition * modifier * 2.0` saturation. The `* 2.0` is what gets dropped, and
  dropping it halves every food's saturation. Saturation is clamped to the **new**
  food level, not to 20. **Nothing calls `eat` yet** — the per-item nutrition table
  is data living in the item's food component, and no `UseItem` path supplies it.
* **Persistence**: the four fields map to `foodLevel` / `foodTickTimer` /
  `foodSaturationLevel` / `foodExhaustionLevel`, vanilla's own names, via
  `FoodData::restored`. **`PlayerData` does not read or write them yet**, so hunger
  resets on rejoin.

### Gotchas

* `FoodData::tick` **reports** rather than applies: it hands back a `FoodTick` and
  `PlayerVitals::tick_food` moves health. Applying inside would give the food module
  a second copy of health, and the two would drift the first time anything else
  damaged the player.
* `FoodTick::display_changed` is set only by a food/saturation change, not by
  exhaustion or the timer. Setting it for exhaustion would send a `SetHealth` on
  every sprinting tick.
* **`natural_health_regeneration` gates the two regeneration arms and nothing else.**
  With it off, hunger still depletes and a starving player still takes damage.
  Reading it as a master switch is the mistake it invites.
* A test that drives drowning or fall damage over a long window and then reads "the
  next `SetHealth`" will now see a *heal* first, because a hurt well-fed player
  regenerates. `serve_play.rs`'s drowning gate turns the rule off for exactly this
  reason.

## Configuration

| knob | where | effect |
|---|---|---|
| `natural_health_regeneration` | game rule | the two regeneration arms only |
| difficulty | `WorldStateHandle::difficulty` | Peaceful never depletes food; the starvation gate's health floor |
| game mode | `Abilities::for_mode().invulnerable` | creative/spectator accumulate no exhaustion and skip the tick |

## Dependencies

`lodestone-model` for `Difficulty`. `crate::vitals` for the health join,
`crate::game_rules` / `crate::world_state` for the rule and difficulty,
`crate::server` for the producer sites and for `encode_set_health`'s `food` and
`saturation` fields. No protocol, no world access, no clock — timestamps and
periods are all tick counts.
