# Distance fog

## What it is

The linear ramp that fades distant geometry into a flat colour, so the loaded
world does not end in a hard wall of terrain against the sky. This doc owns the
**distance math** — where the ramp starts, how wide it is, and which distance
metric measures it. The **colours** it fades into, and how they are chosen per
dimension and per biome, are [`dimension-visuals.md`](./dimension-visuals.md).

Code: `crates/lodestone-render/src/fog.rs` (the math and the GPU uniform),
`crates/lodestone-shell/src/gpu.rs` (`FOG_START_FRACTION`, `RenderState::set_fog`),
`crates/lodestone-shell/src/sim.rs` (`fog_settings`, which picks a preset per
frame), `crates/lodestone-shell/src/app.rs` (`sky_fog`, and the per-frame
reconciliation).

## How it works

`FogSettings { color, sky_color, start, end, environmental_start,
environmental_end }` carries **two** world-space ranges from the eye — the
render-distance term (`start`/`end`) and vanilla's second, independent
environmental term (`environmental_start`/`environmental_end`; see "Vanilla's
two terms" below). `FogUniform` packs both into three `vec4`s with no growth
(the two environmental fields ride `eye.w`/`end_enabled.w`, previously
unused) and every world shader applies

```wgsl
fn fog_amount(rel: vec3<f32>) -> f32 {
    let sph = length(rel);
    let cyl = max(length(rel.xz), abs(rel.y));
    let env = linear_fog(sph, camera.fog_eye.w, camera.fog_end_enabled.w);
    let rd = linear_fog(cyl, camera.fog_color_start.w, camera.fog_end_enabled.x);
    return max(env, rd) * camera.fog_end_enabled.y;
}
let amount = fog_amount(in.world - camera.fog_eye.xyz);
let fogged_srgb = mix(lit_srgb, linear_to_srgb(camera.fog_color_start.rgb), amount);
return vec4<f32>(srgb_to_linear(fogged_srgb), tex.a);
```

`fog::fog_factor` is the CPU twin of `linear_fog`, `fog::total_fog_factor` is
the CPU twin of the whole `fog_amount` combine, and `fog::apply_fog_gamma` is
the CPU twin of the mix, so headless tests describe the shader's behaviour
exactly.

### The fog *colour* also has a day/night track — and it used to only reach the sky disc

`fog_color_for_time_of_day` (`lodestone-render/src/sky.rs`) ports vanilla's
`FOG_COLOR` timeline track and was correct from the day it landed, but its
only consumer in the tree was the sky disc (`sky_pipeline.rs`) — a classic
island (`CLAUDE.md` rule 1, inverted: the mechanism existed and was tested,
one of its two consumers was never wired). `gpu.rs::fog_with_clock`, the sole
producer of the terrain/entity/block-entity fog uniform, stored the flat day
colour unchanged at every hour, so at midnight distant terrain faded to a
full-brightness sky blue against a near-black sky disc, with a hard seam at
the horizon — this is what a player reported as "fog seems a bit too
extreme". Fixed by applying the track inside `fog_with_clock` itself, to a
**local copy** of the settings: `self.fog.color` must stay the untracked day
base, because the sky pass reads that field directly and applies the same
track to paint the disc, so pre-multiplying the stored base would
double-apply it there. Gate:
`gpu::tests::fog_with_clock_carries_the_night_track_gate_a`.

### The mix is in gamma space, and this was wrong for a long time

`fog.glsl`'s `apply_fog` is `mix(inColor.rgb, fogColor.rgb, fogValue)` over
`terrain.fsh`'s `color = texture * vertexColor` — raw, non-colour-managed bytes —
against a `FogColor` that came from `ARGB.vector4fFromARGB32`, i.e. bytes over
255. **Nothing in vanilla's fog chain is linear light**, exactly as `CLAUDE.md`'s
rendering constraints already record for tint and shade.

The three fogged shaders (`model.wgsl`, `fluid.wgsl`, `entity.wgsl`) mixed in
**linear** light, one line after a comment in `model.wgsl` warning about that
precise failure for the tint and shade on the same value. Linear mixing pulls the
result toward the brighter colour — the fog — and the error is **largest where
the factor is smallest**:

| true factor | correct (gamma) | as shipped (linear) | apparent overshoot |
|---|---|---|---|
| 0.25 | 0.25 | 0.373 | **+49%** |
| 0.50 | 0.50 | 0.627 | +25% |
| 1.00 | 1.00 | 1.00 | none |

(sRGB 0.3 fragment against an sRGB 0.75 fog, expressed as the gamma-space factor
that would produce the same pixel.) This is the reported "too foggy too early":
haze appearing well before the ramp says it should, with the onset the worst part.

**It is a *magnitude* defect, which is why every existing gate was blind to it.**
The sign is right, so "distant things are foggier" holds under both. Worse, the
two spaces agree **exactly at both endpoints**, and `fog_gate.rs` measures a
*fully* fogged fragment (factor 1.0) — so the gate that exists to prove fog works
provably cannot see the strength being wrong. `entity_fog_pixels.rs` compares two
mid-ramp depths and only asserts a floor on the delta
(`MIN_FOGGED_DELTA = 25.0`); its mob and fog colour are both bright, so the two
mixes differ by about 1.4 bytes there and it passes either way.

The replacement gate is `fog.rs`'s
`the_world_fog_mix_is_in_gamma_space_at_a_predicted_magnitude`, which computes
**both** hypotheses by hand from the sRGB transfer function and requires the
measurement to land on one and miss the other — plus an executed control that the
linear mix really does produce the second column.

The endpoint agreement has one useful consequence: **the sky disc still mixes in
linear light** (`sky_disc.wgsl`, `fog::apply_fog`) and the horizon seam is still
invisible, because the disc reaches its rim at factor 1.0 where the two spaces
coincide. Bringing the disc over is a separate change with its own gates
(`sky_gradient_pixels.rs` predicts a lot of mid-ramp pixel values).

The shader spends three extra `pow`s per fragment on
`linear_to_srgb(camera.fog_color_start.rgb)`, a value that is uniform across the
draw. Storing the fog colour gamma-encoded in `FogUniform` would remove them, at
the cost of changing what `FogUniform::color_start` *means* for every reader; not
done, deliberately.

### Vanilla's two terms

This is the part that matters and the part this client only half implements.
`fog.glsl:23-30` combines **two independent ramps**, each with its own distance
metric and its own start/end pair:

```glsl
float total_fog_value(float sphericalVertexDistance, float cylindricalVertexDistance,
                      float environmentalStart, float environmantalEnd,
                      float renderDistanceStart, float renderDistanceEnd) {
    return max(linear_fog_value(sphericalVertexDistance,   environmentalStart, environmantalEnd),
               linear_fog_value(cylindricalVertexDistance, renderDistanceStart, renderDistanceEnd));
}
```

with

```glsl
float fog_cylindrical_distance(vec3 pos) {
    return max(length(pos.xz), abs(pos.y));
}
```

| term | metric | source of start/end |
|---|---|---|
| **environmental** | spherical | `visual/fog_start_distance` / `visual/fog_end_distance` attributes, or a `FogEnvironment` (water, lava, blindness, powdered snow) |
| **render distance** | **cylindrical** | derived from the view distance, every frame, unconditionally |

They are combined with `max`, so whichever is denser at a given fragment wins.
The render-distance pair is set *after* the environment hook runs and is never
skipped — a dimension with a declared environmental fog gets both.

### The render-distance ramp (issue #388)

`FogRenderer.setupFog`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/fog/FogRenderer.java:198-200`)
is the whole of it:

```java
float renderDistanceFogSpan = Mth.clamp(renderDistanceInBlocks / 10.0F, 4.0F, 64.0F);
fog.renderDistanceStart = renderDistanceInBlocks - renderDistanceFogSpan;
fog.renderDistanceEnd = renderDistanceInBlocks;
```

The fade band is an **absolute, capped width measured back from the edge** — a
tenth of the view distance, floored at 4 blocks, capped at 64. It is *not* a
proportion of the view distance, which is what this client used until #388.

`fog::render_distance_fade_span` is the authoritative Rust form;
`FogSettings::for_render_distance` builds the settings from it.

| render distance | view distance | vanilla span | vanilla start | old (`0.75`) start |
|---|---|---|---|---|
| 2 | 32 | 4.0 (floored) | 28.0 | 24.0 |
| 8 | 128 | 12.8 | 115.2 | 96.0 |
| 16 | 256 | 25.6 | 230.4 | 192.0 |
| 32 | 512 | 51.2 | 460.8 | 384.0 |
| 48 | 768 | 64.0 (capped) | 704.0 | 576.0 |

The old fraction was wrong in magnitude, not just in onset. At render distance
16, a fragment 240 blocks out read **0.75** fogged against vanilla's **0.375**.
Because the band was proportional it also got *worse* as the player raised the
render distance — 128 blocks of haze at RD 32 against vanilla's 51.2 — so a
larger view distance looked hazier than a small one, which is the reported
symptom.

### Environmental ramps

| preset | range | why |
|---|---|---|
| overworld / the End | none declared | attributes default to `0.0`/`1024.0` (`EnvironmentAttributes.java:18-24`) |
| Nether | `10.0` .. `96.0` | `the_nether.json` declares both — a short haze independent of render distance |
| water | eye .. `min(32, view)` | `WaterFogEnvironment`; ramps from the eye, so `start_fraction` 0 |
| lava | `0` .. `3` | `LavaFogEnvironment`; near-opaque |

`FogSettings::for_view_distance` is the constructor for these — a plain ramp
over a range the caller states outright. **It must never acquire the
render-distance span**: water fog ramps from the eye, and folding a span in
would push its start to within four blocks of its end.

The `0.0 .. 1024.0` default is *not* inert, which an earlier comment in
`fog.rs` claimed. `linear(d, 0, 1024)` reaches `0.125` at 128 blocks and `0.5`
at 512, so vanilla's overworld and End carry a mild spherical mid-field wash
this client does not draw at all.

## What this client does not do yet

Items 1 and 2 below (F2/F3) are now **fixed** — `FogSettings` carries both
terms, `FogUniform` packs both into its existing three `vec4`s (the two
previously-unused lanes, `eye.w`/`end_enabled.w`), and all three fogged
shaders compute the `max` of both metrics. Kept here as history plus the
gates that now prove it, rather than deleted, per this repo's practice of
recording what was measured:

1. ~~**No environmental term.**~~ Fixed: `FogSettings::for_render_distance`
   (overworld/End) sets `environmental_start`/`environmental_end` to the
   registered default `0.0`/`1024.0`; `FogSettings::nether` sets them to the
   Nether's own `10.0`/`96.0` and restores the *real* render-distance span in
   `start`/`end` instead of repurposing that pair to hold its short range (the
   pre-fix approximation this doc used to describe as "exact for the Nether").
   Gate: `fog.rs`'s `ramp_gate::environmental_term_extends_the_ramp_past_the_old_hard_wall`.

   **`FogSettings::for_view_distance` (water, lava, and the one remaining
   fraction below) deliberately keeps its range in the render-distance pair**
   rather than moving it to the environmental one, even though vanilla's
   `WaterFogEnvironment`/`LavaFogEnvironment` set the environmental attributes.
   Several `sim.rs` tests compare water/dry fog settings structurally
   (`a_submerged_eye_selects_short_dense_fog_over_the_sky_fog`), and relocating
   the range would flip which term wins the shader's `max` for every existing
   caller. The measurable cost: that lane now reads **cylindrical** distance
   for these callers instead of spherical, a few percent of a block at the
   short ranges water/lava use — it does not reach the reported symptom.
2. ~~**Spherical distance where vanilla uses cylindrical.**~~ Fixed: every
   fogged shader's `fog_amount` now takes the fragment-relative vector and
   computes `cyl = max(length(rel.xz), abs(rel.y))` for the render-distance
   term, `sph = length(rel)` for the environmental one. The mundane case that
   made this concrete: an eye at `y = 140` looking at a valley floor 110
   blocks out at `y = 64`, RD 8 — spherical `133.7` fully fogged (wrong);
   cylindrical `110` correctly misses the render-distance band and falls back
   to the environmental term, `0.131`. Gate:
   `fog.rs`'s `ramp_gate::cylindrical_distance_uncovers_the_valley_below_an_ordinary_hilltop`.
   The old pin on this gap,
   `spherical_distance_over_fogs_a_pitched_ray_versus_vanillas_cylindrical`,
   is now `cylindrical_distance_and_the_environmental_term_close_the_pitched_ray_gap`
   — retargeted through `total_fog_factor` and inverted from "pin the gap open"
   to "pin it closed", keeping its own negative control (the old single-term
   path) so the before/after is still visible in one test.

3. **The sky disc's colour mix still ignores the render distance** — *not* its
   gradient end, which #399 fixed. Filed from here because it was found while
   measuring this ramp and is the same class of mistake.

   What is fixed: `sky::SKY_FOG_END_DISTANCE` was a constant `512.0`, taken from
   the attribute's *registered default*, where vanilla clamps it —
   `fog.skyEnd = min(renderDistanceInBlocks, attr)`
   (`AtmosphericFogEnvironment.java:73`). At render distance 8 vanilla's horizon
   gradient completes at **128** blocks, not 512, so the ramp was stretched 4x.
   It is now `sky::sky_fog_end_for_render_distance`, carried per frame on
   `SkyFrame::sky_fog_end` and per vertex into `sky_disc.wgsl` — see
   `docs/sky-and-air-bubbles.md`.

   What is not: the same `skyFogEnd` also feeds `getBaseColor`'s
   `skyColorMixFactor` (`AtmosphericFogEnvironment.java:44-47`), a
   `1 - pow(clampedLerp(skyFogEnd/32, 0.25, 1), 0.25)` blend of fog colour
   toward sky colour — so the disc's *colour*, not just its ramp length, is
   render-distance-dependent in vanilla. Note `:44` divides the attribute by 16
   and compares against the render distance in **chunks**, which is a different
   quantity from `:73`'s blocks; reading the two as one expression is how this
   entry was nearly written backwards.

4. **The fog *colour* is the sky colour, where vanilla's is mostly `#C0D8FF`.**
   `sim::fog_for_render_distance` fades terrain toward `gpu::SKY_COLOR`
   (`#89B5EC` once encoded). Vanilla fades it toward
   `AtmosphericFogEnvironment.getBaseColor`, which is
   `ARGB.srgbLerp(skyColorMixFactor, fogColor, skyColor)` with

   ```java
   // AtmosphericFogEnvironment.java:42-47
   float skyFogEnd = Math.min(SKY_FOG_END_DISTANCE / 16.0F, renderDistance);  // in CHUNKS
   float skyColorMixFactor = Mth.clampedLerp(skyFogEnd / 32.0F, 0.25F, 1.0F);
   skyColorMixFactor = 1.0F - (float)Math.pow(skyColorMixFactor, 0.25);
   return ARGB.srgbLerp(skyColorMixFactor, fogColor, skyColor);
   ```

   The overworld's `FOG_COLOR` attribute is `-4138753` = **`#C0D8FF`**
   (`DimensionTypes.java:34`), a pale near-white blue, and `SKY_COLOR` is
   `calculateSkyColor(0.8)` = `#78A7FF`. The mix factor is `0.187` at render
   distance 8 and **`0.0`** at 32 and above, so vanilla's terrain fog is
   81–100% `#C0D8FF`: a *haze*, where ours is terrain dissolving into a
   saturated sky blue. That difference reads as "too foggy" independently of the
   ramp, because a saturated fog colour looks like the sky eating the world
   rather than air between you and it.

   Note `:44` divides the attribute by 16 and compares against the render
   distance in **chunks**, a different quantity from `:73`'s blocks; reading the
   two as one expression is how item 3 above was nearly written backwards.

   **Not changed, and the reason is blast radius, not doubt.** `app.rs` sets the
   frame clear from `FogSettings::color`, and roughly a dozen shell pixel gates
   (`sky_pixels`, `hurt_overlay_pixels`, `view_bob_pixels`, `nametag_pixels`,
   `armour_pixels`, `sheep_wool_pixels`, `chest_block_entity_pixels`, …) hardcode
   `SKY_COLOR.map(|c| (c * 255.0).round())` as "the background". Changing the fog
   colour means re-deriving every one of those backgrounds, and now that the sky
   pass clears to the fog colour too (`docs/sky-and-air-bubbles.md`) it also moves
   the below-horizon void. Worth doing; do it as its own change, with a GPU to run
   those gates on.

`FogUniform`'s two previously-free lanes (`eye.w` and `end_enabled.w` —
`end_enabled.z` is the sky-darken factor) now carry `environmental_start`/
`environmental_end`, so the struct did not grow. Note the 4-bind-group floor
in `CLAUDE.md`: fog rides the group-0 camera uniform precisely so no fifth
group is needed, and this fix does not spend the ones the model shader
already uses for the atlas/palette/anim groups.

## How to change it

- **Changing the ramp shape** → `fog::render_distance_fade_span` and
  `FogSettings::for_render_distance`. Every expectation in `fog.rs`'s
  `ramp_gate` module is a hand-written literal taken from the Java above, not a
  call back into our own formula, so changing the implementation will correctly
  fail them.
- **The one remaining fraction — no longer just tidiness.** `sim::fog_for_render_distance`
  still calls `for_view_distance(SKY_COLOR, rd * 16.0, gpu::FOG_START_FRACTION)`.
  `FOG_START_FRACTION` is `0.9`, which is `1 - span/rd_blocks` wherever the
  clamp is inactive — an algebraic identity, exact for render distances 3
  through 40. Outside that range it loses the 4-block floor and the 64-block
  cap (RD 2: `28.8` against `28.0`; RD 48: `691.2` against `704.0`).
  `gpu.rs`'s `fog_start_fraction_matches_vanillas_span` pins both the agreement
  and the divergence. Because `for_view_distance` deliberately does **not**
  populate the environmental pair (see item 1 above), this un-migrated call
  site is also why F2/F3 do not yet reach the *live* plain-overworld fog —
  they are fully implemented and gated in `fog.rs`, but the deployed sky/fog
  preset still goes through the one-term path. Migrating this one call to
  `FogSettings::for_render_distance(SKY_COLOR, render_distance)` is a
  one-line change, deletes the constant, and is what actually wires F2/F3
  into gameplay. `sim.rs` is a shared, actively-edited file this session, so
  that migration was handed off rather than made directly — see the fog
  investigation's report for the exact patch.
- **Gotcha: a frame average cannot see a ramp defect.** Both models are 0 near
  and 1 at the edge and their frame means sit within a few points of each other;
  only sampling *by location* separates them. `ramp_gate` asserts per-sample and
  prints a bounding box over the ray on failure —
  `a_frame_average_could_not_have_caught_this` pins why.
- **Gotcha: `FogSettings` carries the sky-disc colour too.** `set_fog` and
  `set_clear_color` must be called as a pair; see `dimension-visuals.md`.

## Configuration

- `Config::render_distance` (chunks, default 8) — the only input to the
  overworld ramp. Unbounded, so the span's floor and cap are reachable.
- `gpu::FOG_START_FRACTION` — `0.9`; see above, and prefer not to add callers.
- `gpu::SKY_COLOR` — what overworld fog fades into, shared with the frame clear.

## Dependencies

- `lodestone-render`'s `FogUniform`, consumed by the block, model, entity and
  fluid pipelines and by the sky disc.
- `lodestone-shell`'s `Sim::fog_settings` for per-frame preset selection, and
  `app.rs`'s change-detected `set_fog`/`set_clear_color` pair.
- Reference only: `.cache/mc/26.2/client-src/.../fog/FogRenderer.java`,
  `.../fog/environment/*.java`, `assets/minecraft/shaders/include/fog.glsl`.
