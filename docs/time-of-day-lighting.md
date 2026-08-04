# Time-of-day lighting: the day clock and `sky_darken`

## What it is

The one number that makes terrain and mobs darker at night: the factor the **sky**
half of the lightmap is scaled by. **This doc's original scope was the clock
feed alone** — the paragraphs below describing `lightmap_term` and "Decision:
not built" are the historical record of that scope, kept per this repo's
practice of writing down what was measured rather than deleting it once
overtaken. **The colour half described as declined is now built** — see
["`SKY_LIGHT_COLOR`: now built" below](#sky_light_color-now-built-n1n2n3) for
what changed and why the objections below stopped applying.

The clock feed still lands in the same lane. Before the colour work, the model
(terrain/fluid) and entity shaders computed a single scalar:

```wgsl
// `lightmap_term`, retired — see the colour section below. Identical in
// model.wgsl / entity.wgsl / fluid.wgsl, mirrored in Rust by
// `lodestone_render::light::light_term`/`light_term_from_levels` — see
// light-ramp.md. Still used by GUI/particle callers, unaudited.
let c = clamp(max(light_brightness(sky) * sky_darken(), light_brightness(block)), 0.0, 1.0);
light_term = mix(c, not_gamma_grey(c), BRIGHTNESS_FACTOR);
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
  uniform.** There is no lightmap texture at all — `model.wgsl` and
  `entity.wgsl` compute one grayscale scalar per vertex (`lightmap_term`,
  vanilla's curve applied to each half and `max`ed — see
  [light-ramp.md](./light-ramp.md)), and multiply it
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

**Decision at the time: not built.** The factor was the well-bounded,
high-value half and was done. The colour half was assessed as a real
shading-model change spanning two shaders and several existing gates, not a
value to plumb through an existing lane. That assessment is preserved above
because most of it held — the shading-model change genuinely happened. What
turned out to be wrong was the **uniform-budget** half of the objection: see
the section below.

## `SKY_LIGHT_COLOR`: now built (N1/N2/N3)

**This reverses the "not built" decision above.** A later investigation
(prompted by a player report that "at night the shadow is supposed to be a
different colour") re-examined the blocker and found it did not hold:

* **N3 — no new uniform lane needed, contrary to this doc's own costing.**
  This doc's "uniform-lane budget" bullet above correctly noted two free
  `f32` lanes (`eye.w`/`end_enabled.w`) but concluded a *packed* 24-bit RGB
  lane would still be needed and dismissed that as not the real blocker
  anyway. What was missed: `SKY_LIGHT_COLOR` and `SKY_LIGHT_FACTOR`
  (`sky_darken`) share **identical keyframe ticks** — `730 / 11270 / 13140 /
  22860` (`Timelines.java:71-80`) — and neither track calls
  `.setEasing(...)`, so both interpolate linearly on the same parameter.
  The colour is therefore **algebraically recoverable from `sky_darken`
  alone**: `t = clamp((1 - sky_darken) / (1 - 0.24), 0, 1)`, then
  `srgbLerp(t, white, 0x7A7AFF)` with `Mth.lerpInt`'s floor. No uniform
  change, no bind-group change, no packed lane — `crate::light::
  sky_light_color_from_darken`, verified byte-exact against
  `sky_light_timeline_jvm.txt`'s third column (the "already captured" data
  this doc mentioned) at ticks 0, 12000, 13000, 13140.
* **N1 — the shading-model rewrite this doc scoped correctly.** `model.wgsl`/
  `fluid.wgsl`'s `VsOut.shade` and `entity.wgsl`'s `VsOut.light_term` are now
  `vec3<f32>`, computed by `lightmap_color`/`crate::light::
  light_color_from_levels` — the real `notGamma` (`maxScaled/maxComponent`,
  not the grey specialisation), sky and block computed and tinted
  separately, then added, exactly as this doc's "structurally different"
  bullet above said would be required.
* **N2 — the additive combine, `BlockFactor`, and the warm tint**, all part
  of the same change: `light_color_from_levels` adds the sky and block
  contributions (rather than `max`) and scales block by `BLOCK_FACTOR = 1.4`
  with `BLOCK_LIGHT_TINT = (1.0, 216/255, 140/255)` mixed toward white by
  the parabolic factor `(2·level - 1)²`.
* **The blast-radius bullet was largely right about which files would need
  re-baselining, and largely wrong about the risk being prohibitive.** The
  retained scalar `light_term`/`light_term_from_levels` (GUI/particle
  callers, unaudited) keep the pixel gates this doc worried about
  byte-identical, since they were not touched. The genuinely daylight-only
  claim held exactly as predicted: `SKY_LIGHT_COLOR` is white across the
  whole `[730, 11270)` plateau, so `light.rs`'s
  `daylight_vec3_reduces_to_the_existing_scalar_when_block_light_is_absent`
  pins that every pure-sky-lit packed byte is unchanged, written *before*
  the shader edit as the cheapest possible guard on this exact risk.

Why the original scalar gate (`midnight_lands_on_vanillas_value_and_not_on_
either_wrong_one`) never caught the hue gap: `not_gamma_grey`'s grey
specialisation is algebraically identical to the real `notGamma` whenever
the input's max component is the one being measured, and blue is the max
component at night — so the scalar gate's `0.504652` is exactly vanilla's
**blue** channel, and a scalar model cannot be wrong about the one channel
it happens to reproduce. `light.rs`'s
`midnight_blue_matches_the_old_scalar_gate_but_red_does_not` is the direct
correction, and the ratio-of-ratios gate (`ratio_of_ratios_lands_on_vanillas_
hue_not_grey`) is the one CLAUDE.md specifies for this exact failure mode —
see `docs/fog.md`'s twin fix (the fog colour's own day/night track) for the
sibling investigation this one shipped alongside.

## The ramp was linear where vanilla's is a curve (issues #383, #386 — fixed)

The retired `light_term` was `0.2 + 0.8 * level`, a **straight line** in the light
level. Vanilla's is not. The full derivation, the two errors the record carried
about it, and the re-derived gate expectations now live in
[light-ramp.md](./light-ramp.md); the table below is kept because it is what the
diagnosis of #383's third report was built on, and it is still the correct
comparison against the *bare* curve.

| sky level | retired ramp (`0.2 + 0.8 * l`) | bare curve (`l / (4 - 3l)`) |
| --- | --- | --- |
| 15 | 1.000 | 1.000 |
| 14 | 0.947 | 0.778 |
| 13 | 0.893 | 0.619 |
| 12 | 0.840 | 0.500 |
| 11 | 0.787 | 0.407 |
| 10 | 0.733 | 0.333 |
| 8 | 0.627 | 0.222 |
| 4 | 0.413 | 0.083 |
| 0 | 0.200 | 0.000 |

So **in partial light we were consistently too bright, never too dark** — at sky
12, the sort of level a tree canopy leaves under it, by 0.84 against 0.50. Note
that `lightmap.fsh` then mixes `notGamma` in at the default gamma, which lifts the
right-hand column (0.500 becomes 0.719 at sky 12); see light-ramp.md. That does not
change the sign of the divergence, only its size.

This matters for how #383's third report — *"standing under a tree in daylight
darkens the arm more than vanilla does"* — was diagnosed. The two hypotheses in
the issue were that the arm's skylight was used without the day-of-time scaling,
or sampled from the wrong cell. **Both are false:**

* The scaling is applied. `entity_pipeline.rs`'s vertex shader computes
  `sky = ((light >> 4) & 15) / 15 * sky_darken()`, and
  `gpu/first_person.rs::write_hand_camera` fills the `sky_darken` lane for
  *both* hand branches (arm and held item) from the same source terrain uses.
  At noon `sky_darken_for_time_of_day` returns exactly `1.0`, so under a tree at
  midday the factor removes nothing.
* The cell is right. `hand_light` samples `entity_light.sample(camera.position)`
  — the **eye** — which is what vanilla's
  `EntityRenderer.getPackedLightCoords` does via
  `entity.getLightProbePosition(partialTick)`.

And the skylight value itself is the server's own array, not something we
recompute. With the same input and a ramp that is uniformly *brighter* than
vanilla's, the light term cannot be what over-darkens the arm.

What was over-darkening it is the **diffuse** term, which was fixed: the arm's
dominant face rendered at `0.497` where vanilla puts it at `0.877`. Under a tree
that lands at `0.497 x 0.84 = 0.42` against vanilla's `0.877 x 0.50 = 0.44` — so
in the open the arm was 1.8x too dark and under a canopy the two happened to
nearly coincide, which is exactly why the symptom read as *"the tree makes it
worse"* rather than *"the arm is always dark"*. See
[entity-rendering.md](./entity-rendering.md).

**How the ramp was eventually fixed.** In all three shaders and the Rust mirrors at
once, because `model.wgsl`, `fluid.wgsl` and `entity.wgsl` compute the identical
expression on purpose and `entity_light_pixels` exists specifically to catch a mob
that stops agreeing with the terrain it stands on. What it cost, which gates moved,
and which were unmoved because both curves are exactly `1.0` at full light is in
[light-ramp.md](./light-ramp.md).

**Now ported** (see "`SKY_LIGHT_COLOR`: now built" above), except one piece:
the **additive combine** and `BlockFactor = 1.4` (a flat constant, not
`blockLightFlicker + 1.4` — the flicker term is still unmodelled, a visible
torch shimmer tracked as its own follow-up) are in
`crate::light::light_color_from_levels`. `AmbientColor` was already modelled
before this change (see [light-ramp.md](./light-ramp.md)); it is grey in the
overworld and stays a scalar constant, while the Nether's `0x302821` and the
End's `0x3F473F` are not and remain unported, part of the same per-dimension
colour pass as the Nether/End's own fog colours (`docs/fog.md`).

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
| `crates/lodestone-render/src/light.rs` (`sky_light_color_matches_the_jvm_oracle_byte_exact`) | **N3's colour derivation**: `sky_light_color_from_darken` against the same JVM oracle's third column, at ticks 0/12000/13000/13140 | the runtime feed, or that the shaders actually consume it (that is the naga gate plus the shader edit's own review) |
| `crates/lodestone-render/src/light.rs` (`ratio_of_ratios_lands_on_vanillas_hue_not_grey`, `three_populations_in_one_frame_disagree_the_way_vanilla_does`, `midnight_blue_matches_the_old_scalar_gate_but_red_does_not`) | **N1/N2's hue**: the vec3 lightmap against hand-derived vanilla values, per CLAUDE.md's ratio-of-ratios pattern, with a cave population as the control against a global night tint and a torch population as the control against a hue-only fix that missed `BlockFactor`/the additive combine | pixels — these are hermetic, not a screen-rect gate |

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
* `crates/lodestone-render/src/light.rs` — `light_color_from_levels`,
  `sky_light_color_from_darken`, `not_gamma_vec3` (N1/N2/N3, the colour
  lightmap). `crates/lodestone-render/src/shaders/{model,entity,fluid}.wgsl`
  — `lightmap_color`, the three-way-duplicated shader copy.
* Behavioural reference: `.cache/mc/26.2/src/net/minecraft/world/clock/*`,
  `world/timeline/Timelines.java`, `world/attribute/EnvironmentAttributes.java`,
  `util/{Keyframe,KeyframeTrack,KeyframeTrackSampler,EasingType}.java`,
  `util/ARGB.java`, `client-src/.../LightmapRenderStateExtractor.java`,
  `client-src/.../renderer/Lightmap.java`,
  `client-src/assets/minecraft/shaders/core/lightmap.fsh`.
