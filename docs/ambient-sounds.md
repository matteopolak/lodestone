# Ambient sound loops and client-predicted local sounds

## What it is

Two related pieces of the audio layer, both in `crates/lodestone-sound`:

- **Ambient sounds** (`src/ambient.rs`, `src/biome_ambient.rs`) — the looping biome
  ambience with its crossfade, the darkness-driven "mood" one-shot that people know as
  cave ambience, and the flat-probability "additions".
- **Client prediction** (`src/predict.rs`) — playing your *own* footsteps immediately
  instead of waiting for the server to echo them, plus the step-distance cadence that
  triggers them and a ledger that guards against double-play.

Both are device-free and clock-free state machines; the caller plays what they return.

## How it works

### Ambient sounds come from two layers, and the split is the whole story

| layer | carries | cite |
|---|---|---|
| dimension | the cave mood (`ambient.cave`, 6000, 8, 2.0) | `DimensionTypes.java:43` (overworld), `:125` (the End) |
| biome | a full override: loop + mood + additions | `NetherBiomes.java:67`, `:111`, `:153`, `:191`, `:230` |

Read that table carefully, because both single-layer implementations fail silently in
opposite directions:

- A **biome-only** lookup finds cave ambience in **zero** biomes — no biome in vanilla
  sets it — and concludes the feature does not exist.
- A **dimension-only** lookup gives every Nether biome cave ambience and none of its
  own loop.

And the Nether dimension type deliberately declares **nothing**; its five biomes supply
everything. Use `biome_ambient::ambient_sounds_at(dimension, biome)`, which composes
both.

Environment attributes **override, they do not merge**. A Nether biome's mood *replaces*
`ambient.cave` rather than adding to it, so a merging implementation gives you cave
ambience layered under the nether ambience everywhere in the Nether. That is
`AmbientSounds::resolve`, and it has a gate whose control is a merging implementation.

### The mood trigger is darkness, not depth

This is the one the issue explicitly warned against guessing, and the common guess ("Y
below sea level") is wrong. `BiomeAmbientSoundsHandler.java:70-107`: each tick one block
is sampled uniformly from a `(2 * extent + 1)³` cube — `17³` for the cave settings —
centred on the player's **eye**, and then

- if that block sees **any** sky light, moodiness *drops* by `sky / 15 * 0.001`;
- otherwise moodiness changes by `-(block_light - 1) / tick_delay`.

The second term's sign flips: at block light `0` it is `+1/6000`, at light `1` it is
**exactly zero**, and above `1` it is **negative**. So moodiness accumulates only in
pitch darkness, needs 6000 consecutive fully-dark samples (five minutes), and a single
torch nearby actively drains it. A player in a lit room at Y=-40 accumulates nothing;
one in an unlit box at Y=200 accumulates at full rate.

At `moodiness >= 1.0` the sound plays, positioned `offset` blocks *beyond* the sampled
block along the same direction, and the accumulator resets to `0.0`. Otherwise it is
floored at `0.0`, so drainage cannot bank negative credit.

#### It fires on tick 6001, not 6000 — and not because of `f32`

In exact arithmetic 6000 increments of `1/6000` reach exactly `1.0`. In binary floating
point they do not: `1/6000` is not representable and rounds **down**, so repeated
addition undershoots and needs one extra step.

The obvious explanation is an `f32` artifact. That was checked rather than assumed, and
it is wrong:

| accumulator | after 6000 ticks | crosses 1.0 at |
|---|---|---|
| `f32` | `0.9999486` | 6001 |
| `f64` | `0.9999999999999232` | 6001 |

So the `+1` is a property of accumulating a rounded-down step in *any* binary float, and
the gate is correctly insensitive to storage precision — meaning **no control flips it by
changing precision**. What evidences it is direct computation, which the gate performs
inline for both widths. Vanilla lands on 6001 too, for the same reason.

### The loop is a real looping voice with a 40-tick crossfade

`LOOP_SOUND_CROSS_FADE_TIME` is 40 (`BiomeAmbientSoundsHandler.java:22`), the fade is
linear on an integer counter, and **more than one loop is live at once**: walking from
the crimson forest into the warped forest keeps both voices, fading one down while the
other comes up. Collapsing `AmbientLoops` to a single slot produces an audible seam at
every biome border.

Two orderings matter. The stop check happens **before** the counter moves
(`:125-127`), so a faded-out loop survives one extra tick at negative fade — reversing
it clips the loop. And the fade directions are touched **only on a change**
(`Objects.equals(current, previous)` at `:48`), which is what lets a loop reach full
volume and stay there instead of re-fading every tick.

Note the crossfade duration depends on the fade level *reached*, not on the constant: a
loop only 10 ticks in stops 12 ticks after the change, not 42. An early version of the
gate asserted 42 after a 10-tick fade-in and measured 12.

### Which sounds vanilla predicts client-side

Not a list — a **three-level method override**. `Level.playSound` takes an `except`
player, and the two sides read that one argument in mirror-image ways:

| | behaviour | cite |
|---|---|---|
| `ClientLevel.playSeededSound` | plays locally **iff `except == minecraft.player`** | `ClientLevel.java:679-693` |
| `ServerLevel.playSeededSound` | broadcasts to everyone **except** that player | `ServerLevel.java:1036-1058` |

So `except` is simultaneously "who hears it locally" and "who is left out of the
broadcast". Which player gets passed is decided by three overrides:

| method | passes | for the local player | cite |
|---|---|---|---|
| `Entity.playSound` | `null` | **silent**; arrives only as a packet | `Entity.java:1500-1504` |
| `Player.playSound` | `this` | predicted; server leaves that player out | `Player.java:398-400` |
| `LocalPlayer.playSound` | calls `playLocalSound` | **unconditionally local** | `LocalPlayer.java:540-542` |

The rule that falls out: **every `playSound(event, volume, pitch)` call reached with the
local player as `this` is client-predicted**, because `LocalPlayer` overrides it to a
straight local play. That covers footsteps (`Entity.playStepSound:1471-1473` → the
override), muffled steps, and swim sounds.

**It excludes attacks.** `Player` routes those through a method vanilla literally names
`playServerSideSound`, which passes `null` (`Player.java:1009-1011`), as do the level-up
and deflect sounds (`:1571`, `:1025`). So swing and attack sounds are **not** predicted —
the guess that they are would have produced doubled hit sounds on every swing.

### The double-play answer is structural

**Vanilla has no de-duplication logic at all**, and needs none: the same `except`
argument that makes the client play locally makes the server omit that client from the
broadcast. The two halves are one mechanism.

Our situation makes the risk lower still. `crates/lodestone-server` sends **no sound
packets whatsoever**, so against the integrated server a prediction is the only source,
and against a real vanilla server the exclusion applies. There is no configuration
reachable today that double-plays.

`PredictionLedger` is therefore **defence in depth, not a fix** — a small ring buffer so
that a future server-side `playSound` which forgets the exclusion degrades to "correct"
rather than "everything doubled". A match *consumes* the entry, so one prediction
suppresses exactly one echo and a burst of footsteps is never collapsed into one.

### Footsteps are spaced by distance, not time

`Entity.java:875-895` plus `nextStep()` at `:1270-1271`: travelled distance is scaled by
`0.6` and accumulated, and a step fires when it exceeds a threshold starting at `1.0`.
So the first step lands after `1 / 0.6 ≈ 1.667` blocks, and steps speed up when you
sprint and stop when you walk into a wall. A tick-interval model gets both wrong,
obviously in play and invisibly to a test that only checks "a footstep happened".

Three details worth keeping:

- The threshold re-arms to **`(int)move_dist + 1`** — the next integer boundary, not
  `move_dist + 1`. With `+ 1` each step's overshoot accumulates and the spacing slowly
  drifts.
- It accumulates the **horizontal** component, except when climbing, which uses the full
  3D length. So falling is silent and climbing a ladder steps.
- Air underfoot suppresses the step but **not** the accumulation, so the pending step
  fires on landing rather than being lost.
- The distance is vanilla's `clippedMovement` — movement actually achieved after
  collision, not requested.

## How to change it, and the gotchas

- **`AmbientLoops` must stay a map.** See the crossfade note above.
- **`advance` and `consume` on `StepAccumulator` are deliberately separate**, because
  vanilla re-arms `nextStep` only when a sound was actually produced
  (`Entity.java:892-895`). A crossing over a silent block leaves the threshold armed.
- **`additions` is a compact list**, so the JSON is either a single object or an array
  (`AmbientSounds.java:20`). Real 26.2 data uses the single-object form; assuming an
  array panics. The generator normalises both.
- **`AmbientSounds.additions` is a `Cow<'static, [_]>`, not a `Vec`**, purely so the
  generated table can be a `static` — a `Vec` with elements is not const-constructible.
- **The biome table is generated; the dimension table is not.** The dimension half is
  transcribed from `DimensionTypes.java` because there is no `dimension_type/*.json` in
  this repo. That is a weaker evidence standard and is flagged in the module docs; if a
  dimension-type dump ever lands, generate it.

```bash
LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_ambient_table
```

- **Nothing here can make a sound.** `ambient.rs`, `biome_ambient.rs` and `predict.rs`
  hold no sink, device, clock or filesystem, enforced by a scanner gate rather than
  merely stated — the same guard the music layer carries, for the same reason (a test
  that opens an output device is audible on the owner's machine on every `cargo test`,
  and no health check here can see it).

## The shell wiring (the caller this used to lack)

`crates/lodestone-shell/src/audio/ambient.rs` is the call site. `ShellAmbience` owns the
`MoodAccumulator`, the `AmbientLoops`, a `RainAmbience`, the `StepAccumulator` and the
`PredictionLedger`, and lives in the `AmbienceState` ECS resource beside `MusicState` /
`AudioEngine` — config-scoped, so a reconnect must never reset it.

Two halves, deliberately:

- **`ShellAmbience::tick` is pure.** It returns `AmbienceEvent`s (`OneShot`, `LoopStart`,
  `LoopVolume`, `LoopStop`) and cannot reach a device, so a gate drives an exact number of
  ticks with a synthetic light probe and asserts what *would* play. That is also why this
  module needs no `#[cfg(test)]` playback interception the way `audio/music.rs` does.
- **`ShellAmbience::submit` is the device half**, and owns the loop **voice table** — a
  loop handle has to outlive the tick that started it.

`Sim::tick_ambience` (`sim/audio.rs`) gathers the inputs and is called once per frame from
`app/redraw.rs`, on the same `Instant` as `tick_music`; `ShellAmbience::advance` derives
whole 20 Hz ticks from it, capped at ten, rather than running once per frame.

Three things about the inputs are worth knowing before changing them:

- **The mood light probe is a real per-tick world read at a randomly sampled block**, via
  `crate::net::entity_light_at` — the one reader in the shell that applies the dimension's
  absent-sky policy. Reading `sky_at` directly resolves missing sky to 0, which would bank
  moodiness in open daylight. An absent sample reports **full sky**, so a streaming world
  cannot accumulate mood.
- **The biome is not on the network.** `Sim::standing_biome_name` resolves it out of the
  chunk section's palette against the `BiomeNameCell` registry snapshot every tick, the
  same hop `Sim::biome_sky_color` makes and for the same reason.
- **Rain is narrowed.** `landing` is the listener's own column, gated on sky light, so the
  muffled `weather.rain.above` variant is never selected. Reaching it needs a real
  `MOTION_BLOCKING` heightmap read, which nothing in the shell does yet (`app/weather.rs`
  records the same gap for `canSeeSky`).

Looping playback needed three new primitives, added rather than faked:
`Mixer::set_voice_volume` / `Voice::set_instance_volume` (the crossfade needs a live
voice's gain to move), and `AudioEngine::play_loop`, which forces `looping` **and**
`relative` because vanilla's loop instances are head-relative with no attenuation.

### Predicted footsteps

`Sim::tick_footstep` is called from `Sim::step`'s tick loop, immediately after the walk
bob, with the position *before* and *after* the tick's movement. That position is the
input for the same reason the bob's phase is: `moveDist` accumulates the movement
**actually achieved** after collision, so walking into a wall makes no sound and a
per-frame velocity read would produce steps anyway.

Steps only, deliberately — swing and attack sounds go through vanilla's
`playServerSideSound` and are **not** predicted. Every predicted step is recorded in the
`PredictionLedger`, and the `NetUpdate::Sound` arm in `sim/net_apply.rs` consults
`Sim::suppresses_echo` before playing. Nothing reachable today double-plays, so that is
defence in depth rather than a fix.

## What is deliberately not here

- **`weather.rain.above`** — see the narrowing above; it needs a heightmap read.
- **Underwater ambience.** `LocalPlayer.java:1186`/`:1191` play
  `ambient.underwater.enter`/`.exit` via `playLocalSound` on the water-state transition,
  and `UnderwaterAmbientSoundHandler` owns the loop. A distinct handler, left for whoever
  wires the water-state edge.
- **Swim sounds.** `predict::swim_pitch` and the two swim volume modifiers exist and have
  no caller; they need the water-entry edge the underwater handler above also wants.

## Configuration

No settings of its own. The relevant volume buses are `SoundCategory::Ambient` and
`::Weather`, both of which sit at their `1.0` defaults because
`Mixer::volumes_mut` has no production caller yet.

Ambient loop samples **are** in the default `cargo xtask fetch-sounds` corpus (all six
biome ambience loops, ~2.9 MB), unlike music — so this half needs no extra fetch. That is
deliberate on the xtask's part: it derives exclusion from whether every referencing event
is a music event, rather than from vanilla's `"stream": true` flag, precisely so the
ambience loops are not swept out with the music.

## Dependencies

- `lodestone-audio` — `JavaRandom`, extended here with `next_f64` (`Random.nextDouble`,
  two LCG steps) and `next_f32` (`Random.nextFloat`, one), which the additions
  probability and the swim-pitch jitter need.
- `glam` — `DVec3`/`IVec3`/`Vec3` for sample positions.
- `crates/lodestone-server/assets/worldgen/biome/*.json` — the generated table's oracle,
  at test time only.
