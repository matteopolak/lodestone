# HUD heart regeneration wave

## What it is

Vanilla's travelling heart "bounce" — a **−2 px** vertical offset that moves along the heart row one container per tick while the player has the Regeneration effect. It is the one heart animation still missing from our HUD, and this doc exists mainly to correct a plausible-sounding but wrong model of *what triggers* the heart and hunger offsets.

Measured 2026-08-04 from a player report ("hearts and hunger are missing the bounce when the user takes damage, regens, loses hunger, etc."). Only the *regen* half of that report is a real gap.

## How it works

### Vanilla has exactly two heart y-offsets, and neither is a health *delta*

Both are in `extractHearts` (`.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`), applied to `yo` before any heart sprite is drawn:

| offset | value | gate | cite |
|---|---|---|---|
| low-health jitter | `+random.nextInt(2)` → 0 or +1 | `currentHealth + absorption <= 4` — a **level** | `Hud.java:863-865` |
| regen wave | `-2` exactly | `containerIndex == heartOffsetIndex` — one container only | `Hud.java:867-869` |

They compose: a player at ≤2 hearts *and* regenerating gets both on the same container.

`heartOffsetIndex` is what makes the second one a wave, and its gate is a **status effect**, not a change:

```java
int heartOffsetIndex = -1;                                     // Hud.java:792
if (player.hasEffect(MobEffects.REGENERATION)) {               // Hud.java:793
   heartOffsetIndex = this.tickCount % Mth.ceil(maxHealth + 5.0F);   // Hud.java:794
}
```

At the default 20 max health the period is `ceil(25.0) = 25`, so the index cycles `0..=24` while only `0..=9` match a real container (`healthContainerCount = ceil(maxHealth / 2) = 10`, `Hud.java:855`). The visible result is a single heart lifting 2 px, travelling left to right across the row over 10 ticks, then a 15-tick pause before the next pass. That is the animation players recognise as "hearts bouncing".

### The health-change window applies no vertical offset at all

This is the correction. `blink` (`Hud.java:766`) is the change response, opened for 20 ticks on damage and 10 on heal (`Hud.java:768-773`), and it moves nothing. It does two things, both purely visual substitutions:

* swaps every container to the `_blinking` sprite variant (`Hud.java:871`, `blinks = blink` for **all** containers regardless of their own fill);
* draws a "ghost" layer of `displayHealth` — health about to be lost — on the blinking variant (`Hud.java:881-885`).

So in vanilla there is **no bounce on damage** and **no bounce on regen ticking a health point**. There is a bounce while the Regeneration *effect* is present, which is why the owner's instinct was right about "regens" and wrong about "takes damage".

### Hunger is a level trigger too

`extractFood` has one offset and it is gated on **saturation**, not on food changing:

```java
if (player.getFoodData().getSaturationLevel() <= 0.0F && this.tickCount % (food * 3 + 1) == 0) {
   yo += this.random.nextInt(3) - 1;                            // Hud.java:977-979
}
```

Range `-1..=+1`, and only on ticks where `tickCount % (food * 3 + 1) == 0`, so the wobble gets faster as food drops. There is no change-triggered food offset anywhere in `Hud.java`. Our `anim::hunger_wobble` already models exactly this, so hunger has no gap.

### Status against the four-item report

| behaviour | vanilla | ours | status |
|---|---|---|---|
| heart flash on change (`blink` + ghost layer) | `Hud.java:766,871,881-885` | `anim::HeartAnim::tick`, `hud.rs:1521-1552` | **present** (`45db062`) |
| low-health jitter | `Hud.java:863-865` | `anim::heart_jitter`, `hud.rs:1532-1536` | **present** (`45db062`) |
| hunger wobble on empty saturation | `Hud.java:977-979` | `anim::hunger_wobble`, `hud.rs:1570` | **present** (`8bfd1d1`) |
| hotbar item pop on pickup | `ItemStack.popTime` | `anim::HotbarPop`, `hud.rs:1343` | **present** (`3c9f2f0`) |
| **regen wave (−2 px, travelling)** | `Hud.java:792-794,867-869` | — | **absent** |
| "bounce on damage" / "bounce on hunger loss" | does not exist in vanilla | — | **not a gap** |

The absence is not a stale claim: `heartOffsetIndex`, `heart_offset`, `regen_wave`, `Regeneration` and `REGENERATION` all return **zero hits** across `crates/lodestone-shell/src/`, searched for the *producer* tree-wide rather than for a consumer in one named file.

## How to change it

The wave needs one bit of state the HUD frame does not yet carry — "is the local player regenerating" — plus a per-tick index. The plumbing already exists at both ends:

* **Source.** `Sim::active_effects()` (`crates/lodestone-shell/src/sim/session.rs`) returns `lodestone_game::effect::ActiveEffects`, which has `get(&lodestone_model::Identifier)`. `app.rs:3241` already calls it in the same function that fills `hud_frame`, for the status-effect overlay, so producing the bool costs one line and no new read.
* **Sink.** `HudAnim` (`hud.rs:1363-1390`) is the established carrier for per-tick animation state, computed in `HudRenderer` at `hud.rs:2466-2480` alongside `heart_blink`. The wave index belongs there, next to `heart_blink`, not recomputed in the draw.

Gotchas:

* **Do not gate the wave on a health delta.** That is the mistake this doc exists to prevent, and it produces a bounce that fires on damage (which vanilla never does) and not during a steady regen (which is the only time vanilla does).
* **The offset is exactly `-2`, and it is not random.** `heart_jitter` is the random one; conflating them gives a wave that shimmers instead of travelling. A test that asserts only "the heart moved" is satisfied by either, which is the *magnitude* species of vacuous test — assert `-2.0`.
* **The period is `ceil(maxHealth + 5.0)`, not the container count.** With our row hardcoded to 10 containers, max health is pinned at 20, so the period is 25 — and the 15 ticks where the index exceeds 9 are a real, visible pause, not dead code. A period of 10 would produce a continuous wave with no gap, which looks wrong.
* **It applies only to health containers, not absorption ones** (`containerIndex < healthContainerCount`, `Hud.java:867`). Absorption is not modelled in `HudFrame` yet, so this is currently vacuous — but write the condition anyway or it becomes a silent divergence when absorption lands.
* **It composes with the jitter, so apply both.** `Hud.java:863-869` adds the jitter first and then subtracts 2; a player at 2 hearts with Regeneration sees the sum.

## Configuration

None. Both offsets are hardcoded in `Hud.java` and no `Options` field gates either.

## Dependencies

* `crates/lodestone-shell/src/hud.rs` — `sprite_vitals` (the heart row draw), `HudAnim`, and `HudRenderer`'s per-frame tick.
* `crates/lodestone-shell/src/hud/anim.rs` — the sibling animations, and where the wave index belongs.
* `crates/lodestone-shell/src/sim/session.rs` — `Sim::active_effects`, the Regeneration signal.
* `crates/lodestone-game/src/effect.rs` — `ActiveEffects::get`.
* `docs/hud-animations.md` — the three animations that already landed under issue #30. Written by a concurrent change and may be uncommitted at the time of reading.
