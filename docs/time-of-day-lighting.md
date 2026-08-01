# Time-of-day lighting: the day clock and `sky_darken`

## What it is

The one number that makes terrain and mobs darker at night: the factor the **sky**
half of the lightmap is scaled by. Both the model (terrain/fluid) and entity
shaders compute

```wgsl
light_term = 0.2 + 0.8 * max(sky * sky_darken(), block)
```

so `sky_darken` is the whole of "what time is it?" as far as rendering is
concerned. It comes from the server's day clock, and getting that clock wrong
looks exactly like a lighting bug.

Reported as two bugs, fixed as one: *"the world is fullbright"* and *"the mobs
look like they're in the daytime"*.

## How it works

```text
set_time packet
  → V770Adapter (holds a DayClock anchor)      crates/protocol/v770/src/adapter.rs
  → ClientEvent::TimeChanged { world_age, time_of_day }
  → WorldTime resource                          crates/lodestone-ecs/src/resources.rs
  → ClientHandle::world_time().1
  → sky_darken_for_time_of_day(time_of_day)     crates/lodestone-render/src/entity.rs
  → RenderState::set_sky_darken_source(..)      installed in app.rs, two connect paths
  → FogUniform.end_enabled[2]  (the spare `z` lane of the group-0 uniform)
  → model_pipeline.rs `sky_darken()` and entity_pipeline.rs `sky_darken()`
```

The factor rides the fog uniform's spare lane rather than getting a bind group of
its own because **the model shader is at wgpu's 4-bind-group floor** — camera,
atlas, palette and anim already spend all four. It is deliberately the *same* lane
for both passes so terrain and mobs cannot disagree about the hour; wiring one
without the other is worse than wiring neither (at midnight it makes mobs darker
than the blocks they stand on, which reads as a mob bug).

`0.0` in that lane is the "never wired" sentinel and reads as full daylight, since
vanilla's legitimate range is `[0.24, 1.0]`.

### 26.2's `set_time` is a clock *map*, and it is usually empty

`ClientboundSetTimePacket` is `(long gameTime, Map<Holder<WorldClock>,
ClockNetworkState>)`. Three different senders populate that map differently, and
this is the crux:

| sender | when | clock map |
|---|---|---|
| `ServerClockManager::createFullSyncPacket` | once, at join | **full** (all clocks) |
| `ServerClockManager::modifyClock` | on `/time set`, rate, pause | **one entry** |
| `MinecraftServer::forceGameTimeSynchronization` | ~once a second, forever | **`Map.of()` — empty** |

So roughly 19 packets in 20 carry no clock at all. An absent clock update means
*"nothing changed, keep what you had"* — vanilla's own client holds the clock
locally and lets the server correct it.

The adapter therefore holds a `DayClock` anchor (`total_ticks`, `rate`,
`at_game_time`) and extrapolates:

```rust
time_of_day = total_ticks + (game_time - at_game_time) * rate
```

Both the anchor and the elapsed-tick reference ride on `set_time`'s own
`gameTime`, so **no local tick loop is needed** — `time_of_day` advances in
~20-tick steps, one per sync. That granularity is invisible in the only consumer,
whose curve moves over thousands of ticks.

`rate == 0.0` means paused. `/gamerule advance_time false` (26.2's snake_case
rename of `doDaylightCycle`) reaches the client as a rate of zero, never as a
flag — `ClockInstance::packNetworkState` sends `paused || !advance_time ? 0.0 :
rate`.

### Which clock

26.2 registers two (`WorldClocks::bootstrap`): `minecraft:overworld` = `0`,
`minecraft:the_end` = `1` — now **confirmed off the wire** rather than read out of
`bootstrap`, by the captured `minecraft:world_clock` registry payload
(`crates/protocol/v770/tests/fixtures/registry_data_world_clock.hex`).

`SetTime::day_clock()` selects **the lowest holder id**, not the first on the wire,
because the wire type is a Java `HashMap` and the join-time full sync's two entries
can arrive in either order. **Since issue #288 that is the fallback, not the
answer**: the adapter resolves the current dimension type's `default_clock`
(`minecraft:overworld` in the overworld, **`minecraft:the_end` in the End**, absent
in the Nether) against the ingested `world_clock` registry and passes the resulting
holder id to `SetTime::clock_for`.

That mattered on plain vanilla, not only under a data pack: **in the End the
lowest-id pick returned holder `0`, the overworld's clock**, so the End's sky
followed overworld time. The old note here anticipated only "a data pack reorders
the registry" and missed the default case. See
[`registry-data-ingest.md`](./registry-data-ingest.md).

The key codec is `holderRegistry`, i.e. `registry(key, Registry::asHolderIdMap)`:
a **bare** VarInt registry id. There is no `+1` offset and no inline-direct path —
that convention belongs to the *other* codec, `ByteBufCodecs.holder(key,
directCodec)`, which `set_time` does not use.

## What was actually wrong (measured)

`SetTime::day_time()` fell back to `game_time` when `clocks` was empty. Since the
once-a-second sync *is* the empty case, that fallback ran every second and
overwrote the day time with the **monotonic world age**. Consequence:
`sky_darken_for_time_of_day` returned a **session constant** — whatever hour
`world_age % 24000` happened to name, for the entire session, unresponsive to the
server's clock.

Measured on the survival oracle, driving the server's clock over RCON and reading
the client's value back (`crates/lodestone-shell/tests/live_time_of_day.rs`):

```text
BEFORE — one reading, then the gate stops
/time set noon      -> client age=643037 time_of_day=643037 (reduced 19037) sky_darken=0.240

AFTER
/time set noon      -> client age=641917 time_of_day=894000 (reduced  6000) sky_darken=1.000
/time set midnight  -> client age=641977 time_of_day=906000 (reduced 18000) sky_darken=0.240
/time set day       -> client age=642037 time_of_day=913000 (reduced  1000) sky_darken=1.000
/time set night     -> client age=642097 time_of_day=925000 (reduced 13000) sky_darken=0.300
```

Note `time_of_day` **exceeds** `world_age` after the fix (894000 > 641917): the
day clock is its own counter and the two are unrelated on any world that has ever
had `/time set` run on it.

On that world the constant was `0.240` — permanent midnight. On a world whose
`age % 24000` lands in daylight the constant is `1.0`: **permanent noon**, which is
the reported "fullbright terrain" and "daytime mobs", both from this one value.

### Why "the light seems right when I break a block" needed no second bug

The reporter's observation was real, and their explanation of it was not. With
`sky_darken` pinned to `1.0`:

* Every **sky-lit** surface renders at `light_term = 1.0` — uniformly, wrongly
  bright. That is most of what you see outdoors.
* Every **sky-dark** surface is *untouched* by the defect, because
  `max(sky * sky_darken, block)` with `sky == 0` is `block`, algebraically
  independent of `sky_darken`.

Breaking a block exposes faces that open into a freshly-dug cell — low or zero sky
light. Those land in the second population, so they render correctly dark against
a world rendered wrongly bright. Nothing was fixed by the break; the break merely
revealed the one population the bug cannot reach.

### Beliefs that were confidently held and turned out false

Three, all of which cost time and one of which was the whole briefing:

1. **"The wire light never reaches the mesher for freshly-loaded chunks, so
   sections take the full-bright bridge; `light_update` arrives before its chunk,
   the `world.rs` seam no-ops, and the light is dropped permanently."** False, and
   the *opposite* of measured. Fingerprinting every light section of every streamed
   column at the instant its arrival event drained, and again after an 8-second
   settle: **61 of 61 columns byte-identical**. Light arrives complete inside
   `level_chunk_with_light` and is applied before `ChunkLoaded` is emitted. No
   light is dropped, and the standalone `light_update` no-op seam never fires for
   a column we hold. The CPU light path was correct end to end the whole time —
   a live terrain column meshes to sky nibbles spread across the full `0..15`
   range (14284 vertices at `0`, 10126 at `15`), not a flat field.
2. **"`ClockUpdate::holder_id` is `id + 1`, with `0` meaning an inline direct
   value."** False; the record said so and the decode never did. `holderRegistry`
   writes a bare id. The code was right and the comment was wrong — the failure
   mode CLAUDE.md warns about, found by reading the *codec definition* rather
   than the call site.
3. **"A green gate over the shader proves the feature works."** Two gates covered
   the two ends of this chain and both were green while the bug was on screen:
   `entity_night_pixels.rs` supplies `sky_darken` by hand, and
   `grass_light_response_gate.rs` supplies the light byte by hand. Both prove the
   *shader* responds; neither can see where the number comes from. This is the
   **world** species of vacuous test — the source is exemplary and the flaw is in
   what it was pointed at. See "Gates" below.

## The full-bright bridge: real, but transient

`snapshot_section_in` fills an absent neighbour's light slot with `None`, which
`mesh_snapshot`/`mesh_snapshot_models` resolve to
`UniformLight::pre_light_bridge()` — full sky `15`, no block light. This is
correct for the *edge of the loaded world* (air there should not render black),
but it means a column meshed before its horizontal neighbours arrive renders its
boundary faces full-bright.

That is measurable and it is not small. Column `(-4, -7)` on the terrain oracle,
meshed at the instant of arrival versus after its neighbours landed:

```text
sky nibble        0   ...    8   ...   15
at arrival:     6151      1378       13180
settled:        6744      1021           0
```

Over half that column's geometry was full-bright at arrival, and **zero** of it
should be sky-lit at all.

It heals, and the healing is not best-effort: `on_column_arrived` marks the eight
loaded neighbours dirty and `heal_dirty_columns` drains them on a budget.
Measured over a 60-second spiral load of 329 columns on the survival oracle:

```text
interior columns (all 8 neighbours arrived) = 257
  re-meshed after their newest neighbour    = 249
  meshed BEFORE their newest neighbour      =   0   <-- the stale-seam count
  never meshed (still in the worker queue)  =   8
mesh drops counter                          =   0
```

Zero stale seams. So the bridge produces a **sub-second full-bright frontier
during streaming**, never a persistent one, and it cannot be the reported defect.
Do not "fix" it by defaulting absent neighbour light to `0` — that reintroduces
the black-faces-at-the-frontier bug the bridge exists to prevent.

## How to change it

* **The clock anchor** lives in `DayClock` in `crates/protocol/v770/src/adapter.rs`.
  It still holds exactly **one** clock — the one the current dimension follows,
  selected by `V770Adapter::clock_holder` and re-selected on every
  `login`/`respawn`. Surfacing two clocks *at once* would need it widened to a map
  keyed by `holder_id`; the constraint that makes that pointless today is
  `ClientEvent::TimeChanged`'s single `time_of_day` field, not the struct. (The
  older note here said selecting per dimension needed that widening. It did not —
  re-anchoring one slot on a dimension change was enough.)
* **Resolving the clock by name** is done, as of issue #288: the crate ingests the
  `minecraft:world_clock` registry (`packets::registry::ClientRegistries`), the
  adapter resolves the current dimension type's `default_clock` to a holder id in
  `enter_dimension`, and `SetTime::clock_for` selects that clock. The old note
  here said this "needs the registry, which this crate does not ingest" and framed
  it as a data-pack-only concern; both halves are now out of date — see the *Which
  clock* section above for the vanilla End case it was already getting wrong.
* **A dimension with no clock of its own** (the Nether: `has_fixed_time: true`, no
  `default_clock`) still falls back to the lowest-id pick. That is deliberate and
  documented rather than fixed: `time_of_day`'s only consumer is the sky curve
  below, which does not yet gate on `has_fixed_time`, so reporting the overworld's
  clock there is exactly the pre-#288 behaviour and no worse. Gating the curve on
  `has_fixed_time` — the field *is* decoded and carried on
  `DimensionTypeInfo` — is the real fix and is unclaimed.
* **The curve** is `sky_darken_for_time_of_day` in
  `crates/lodestone-render/src/entity.rs`. It is a direct port of 26.2's
  `EnvironmentAttributes.SKY_LIGHT_FACTOR` timeline track
  (`Timelines.OVERWORLD_DAY`, `.cache/mc/26.2/src/net/minecraft/world/timeline/Timelines.java:77-80`),
  which **replaced** 1.21's `Level.getSkyDarken` entirely (issue #49, fixed).

  **What the real track is, read from the jar rather than from the issue's
  own transcription** (which got one thing wrong — see below):

  * Keyframes: `730 → 1.0`, `11270 → 1.0`, `13140 → 0.24`, `22860 → 0.24`,
    applied via `FloatModifier.MULTIPLY` over the attribute's default of
    `1.0` — the multiply is a no-op, so the sampled keyframe value *is* the
    final factor. `0.24` is literally `Timelines.NIGHT_SKY_LIGHT_FACTOR`.
  * **The easing is linear, not cubic-bezier.** `KeyframeTrack.Builder`
    defaults to `EasingType.LINEAR`
    (`.cache/mc/26.2/src/net/minecraft/util/KeyframeTrack.java:78`), and the
    `SKY_LIGHT_FACTOR` track never calls `.setEasing(...)` — only the
    neighbouring `SUN_ANGLE`/`MOON_ANGLE`/`STAR_ANGLE` tracks in the same file
    opt into `EasingType.symmetricCubicBezier(0.362, 0.241)`. The original
    issue text said "cubic-bezier eased between them"; that was a
    transcription error, corrected by reading `Timelines.java` itself rather
    than trusting the earlier summary — exactly the trap `CLAUDE.md` names
    ("read the record definition, not a summary of the call site").
  * `KeyframeTrackSampler.bakeSegments` wraps the segment between the *last*
    and *first* keyframe through the timeline's 24000-tick period, so the
    dawn ramp is one continuous **1870-tick** segment running from 22860
    through the tick-0 seam to 730, not a ramp that resets at midnight.
  * No `LightTexture`-style `* 0.95 + 0.05` lift: that was specifically the
    second step of 1.21's two-step pipeline. 26.2's keyframes are already
    expressed directly in `[0.24, 1.0]`, and the consumer
    (`LightmapRenderStateExtractor` into `assets/minecraft/shaders/core/lightmap.fsh`'s
    `sky_brightness = get_brightness(sky_level) * lightmapInfo.SkyFactor`)
    applies no further transform.

  **The quantified divergence this section used to cite was itself wrong.**
  It said "our `night` (13 000) reads `0.300` where the timeline is already at
  `0.24`" — but a JVM oracle sampling the real `Timeline`/`AttributeTrackSampler`
  at every tick (`crates/lodestone-render/oracle-java/SkyLightTimelineOracle.java`,
  dumped to `crates/lodestone-render/tests/support/sky_light_timeline_jvm.txt`)
  shows vanilla is `0.2969` at tick 13000, not `0.24` — tick 13000 sits
  partway down the `[11270, 13140)` dusk ramp, 140 ticks before the plateau
  starts. The retired cosine port's `0.300` there was actually *close*. The
  real, measured divergence peaks mid-ramp (**~0.016** at ticks 12080 and
  23917, e.g. vanilla `0.6692` vs the old port's `0.6851`) — real, worth
  fixing, but a much smaller and differently-shaped defect than the "night
  already at the floor" framing implied. The new port matches the JVM to
  within `1e-4` at all 24000 ticks
  (`crates/lodestone-render/tests/sky_light_factor_timeline.rs`).

  **`SKY_LIGHT_COLOR` — assessed, not implemented; costed below.**
* **Adding a consumer**: install it off `RenderState::sky_darken()` rather than
  re-deriving from `world_time()`, so there stays exactly one clock on screen.

## `SKY_LIGHT_COLOR`: what it is and why it was not ported (issue #49)

Vanilla does not just scale the sky half of the lightmap at night — it **tints**
it. Read from the jar rather than assumed:

* Same track, `EnvironmentAttributes.SKY_LIGHT_COLOR`
  (`Timelines.java:71-75`), keyframes `730 → -1 (white)`, `11270 → -1`,
  `13140 → NIGHT_SKY_LIGHT_COLOR`, `22860 → NIGHT_SKY_LIGHT_COLOR`, same
  segment/wraparound/linear-easing machinery as the factor track, applied via
  `ColorModifier.MULTIPLY_RGB` (`ARGB.multiply`, a no-op against the `-1`
  white base) and interpolated between keyframes with `ARGB.srgbLerp` — a
  plain per-channel `0..255` lerp in gamma space, consistent with `CLAUDE.md`'s
  "vanilla is not colour-managed" rule, not a linear-light blend.
  `NIGHT_SKY_LIGHT_COLOR = ARGB.colorFromFloat(1.0, 0.48, 0.48, 1.0)` =
  `0xFF7A7AFF` (`Mth.floor(0.48 * 255) = 122` per channel, full blue) — a
  light, unsaturated blue, not a deep or dark one. Sampled ground truth for
  this whole track is the third
  column of `crates/lodestone-render/tests/support/sky_light_timeline_jvm.txt`
  (produced alongside the factor dump, unused by any Rust code today).
* **Where the tint is applied matters more than the values, and this is the
  real finding.** `LightmapRenderStateExtractor.extract` feeds `skyFactor`
  and `skyLightColor` into a per-frame 16×16 RGB lightmap **texture**,
  generated by a real fragment shader
  (`assets/minecraft/shaders/core/lightmap.fsh`):
  ```wgsl
  sky_brightness = get_brightness(sky_level) * SkyFactor   // curved, not linear
  color = max(AmbientColor, NightVisionColor * NightVisionFactor)
  color += SkyLightColor * sky_brightness                   // tint only the sky term
  color += mix(BlockLightTint, vec3(1.0), 0.9 * parabolic(block_level)) * block_brightness
  color = clamp(color, 0, 1)                                 // additive combine, then clamp
  ```
  Terrain/entity shaders then *sample* that texture by `(block_level,
  sky_level)` UV. Vanilla's combine rule is **additive per-channel**, and the
  sky and block contributions are computed and tinted independently before
  summing.
* **Our lighting model is structurally different, not just missing a
  uniform.** There is no lightmap texture at all — `model_pipeline.rs` and
  `entity_pipeline.rs` compute one grayscale scalar per vertex,
  `light_term = 0.2 + 0.8 * max(sky * sky_darken(), block)`, and multiply it
  uniformly into the sampled texel (`out.shade = ao * light_term`). Block and
  sky already lose their separate identities via `max()` before any tint
  could apply. Porting the tint faithfully — sky tinted, block light not,
  summed rather than maxed — means splitting that scalar into (at minimum)
  a sky-contribution and a block-contribution carried separately through both
  the vertex and fragment stages of **both** shaders, then recombining them
  additively instead of via `max`. That is a change to the shading model
  itself, not an additive feature.
* **Uniform-lane budget is tighter than it looks but not the binding
  constraint.** `FogUniform` (shared by both shaders, folded into the
  4-bind-group-floor'd group-0 camera uniform — see the rendering-constraints
  section of `CLAUDE.md`) has exactly two unused `f32` lanes left:
  `eye.w` and `end_enabled.w` (`end_enabled.z` already carries `sky_darken`).
  One packed lane (bit-pack the tint as 24-bit RGB, `bitcast<u32>` to unpack
  in WGSL) would technically fit without growing the struct or adding a bind
  group. So the "no room" framing this section used to gesture at is not
  actually the blocker — the shading-model rewrite is.
* **Blast radius if built anyway.** Both `model_pipeline.rs` and
  `entity_pipeline.rs` (vertex *and* fragment WGSL, plus their Rust-side
  uniform builders); every existing night-time pixel gate that hardcodes an
  expected grayscale value would need re-baselining against a fresh oracle
  once sky light stops being pure grayscale after dusk —
  `entity_night_pixels.rs`, `grass_light_response_gate.rs`,
  `model_shade_gamma_gate.rs`, `entity_light_pixels.rs` at minimum, by
  inspection of what each currently asserts. Daytime scenes are unaffected
  (`SkyLightColor` is white `-1` through the `[730, 11270)` plateau, so the
  tint is a no-op there), which bounds the risk somewhat, but the change
  touches the one formula every terrain and mob pixel on screen currently
  runs through.

**Decision: not built.** The factor is the well-bounded, high-value half and
is done. The colour half is a real shading-model change spanning two shaders
and several existing gates, not a value to plumb through an existing lane —
exactly the "changing several consumers" case where a half-wired version
would be worse than none. Left as a scoped follow-up with the ground-truth
data (`sky_light_timeline_jvm.txt`'s third column) already captured.

## Gates

Kept deliberately separate so a pass on one cannot mask the other.

| gate | what it can see | what it structurally cannot |
|---|---|---|
| `crates/protocol/v770/tests/world_state.rs` | the hold-and-extrapolate rule: empty maps do not clobber, `rate 0` freezes, lowest-holder-id fallback selection | anything above the adapter |
| `crates/protocol/v770/tests/registry_data.rs` | **which clock is the day clock**: the `minecraft:world_clock` holder ids, off captured server bytes, and each dimension type's `default_clock` | that the adapter actually *uses* the resolved holder (`world_state.rs`'s `clock_for` cases) |
| `crates/lodestone-shell/tests/live_time_of_day.rs` | **the runtime feed**: the client's day clock follows the real server's `/time set`, and the derived `sky_darken` spans the curve | pixels |
| `crates/lodestone-render/tests/entity_night_pixels.rs` | the entity shader responds to a `sky_darken` it is *handed* | where that value comes from |
| `crates/lodestone-render/tests/grass_light_response_gate.rs` | the model shader responds to a light byte it is *handed* | where that value comes from |
| `crates/lodestone-render/tests/sky_light_factor_timeline.rs` | **the curve shape**: `sky_darken_for_time_of_day` against a JVM dump of the real timeline at all 24000 ticks, including a negative control proving the scan would have failed the retired cosine port | the runtime feed (that's `live_time_of_day.rs`'s job) or pixels |

The live gate carries the control this defect specifically needs. On a *fresh*
world the world age and the day clock coincide, so a broken feed and a working one
report the same number and the gate would pass vacuously. It therefore asserts
that `world_age % 24000` is **not** the value being reported — a world too young
for the two to have separated fails with a fix hint rather than passing.

Both controls were run, not described. Reverting the adapter to the old fallback
turns three hermetic tests red (`an_empty_clock_map_does_not_overwrite_the_held_day_time`,
`a_paused_clock_does_not_advance_with_the_world_age`,
`an_unsynced_clock_falls_back_to_the_world_age`) and fails the live gate on its
first reading with `time_of_day=643037 (reduced 19037) sky_darken=0.240`.

## Configuration

None. No flags, no env vars. Two things are worth knowing:

* The live gate needs the **survival** oracle (`./scripts/live-oracles/survival.sh`,
  game `:25565`, RCON `:25566`) rather than the flat creative one — not for its
  terrain, but because that world has been running long enough for the world age
  and the day clock to have diverged, which is the condition the defect needs to
  be visible.
* `/gamerule advance_time false` is 26.2's spelling. `doDaylightCycle` is rejected.

## Dependencies

* `crates/protocol/v770/src/packets/time.rs` — `SetTime` / `ClockUpdate` decode.
* `crates/protocol/v770/src/adapter.rs` — `DayClock`, the `SET_TIME` arm.
* `crates/lodestone-ecs/src/resources.rs` — the `WorldTime` resource.
* `crates/lodestone-render/src/entity.rs` — `sky_darken_for_time_of_day`.
* `crates/lodestone-shell/src/gpu.rs` — `SkyDarkenSource`, `fog_with_clock`.
* `crates/lodestone-shell/src/app.rs` — installs the source on both connect paths.
* `crates/lodestone-render/src/{model_pipeline.rs,entity_pipeline.rs}` — the two
  shaders that read the lane.
* `crates/lodestone-render/oracle-java/SkyLightTimelineOracle.java` — the JVM
  oracle for the `SKY_LIGHT_FACTOR`/`SKY_LIGHT_COLOR` timeline, and
  `crates/lodestone-render/tests/support/sky_light_timeline_jvm.txt`, its
  committed dump.
* Behavioural reference: `.cache/mc/26.2/src/net/minecraft/world/clock/*`,
  `world/timeline/Timelines.java`, `world/attribute/EnvironmentAttributes.java`,
  `util/{Keyframe,KeyframeTrack,KeyframeTrackSampler,EasingType}.java`,
  `util/ARGB.java`, `client-src/.../LightmapRenderStateExtractor.java`,
  `client-src/.../renderer/Lightmap.java`,
  `client-src/assets/minecraft/shaders/core/lightmap.fsh`.
