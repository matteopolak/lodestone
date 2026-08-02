# The light ramp: vanilla's lightmap curve

## What it is

The scalar every terrain, fluid, entity and particle fragment multiplies its texel
by, as a function of the server's packed sky/block light byte and the time of day.
Vanilla calls it a *lightmap*: a 16x16 texture indexed by `(block_level, sky_level)`
that `block.vsh` folds into the vertex colour with
`vertexColor = Color * sample_lightmap(Sampler2, UV2)`.

We have no lightmap texture — we compute the same value per vertex. One Rust
authority, [`lodestone_render::light`](../crates/lodestone-render/src/light.rs),
and three verbatim WGSL copies (`model.wgsl`, `entity.wgsl`, `fluid.wgsl`).

Issues [#383] and [#386]; superseded a linear ramp, `0.2 + 0.8 * level`, that had
shipped since the light byte was first wired.

## How it works

Straight from `assets/minecraft/shaders/core/lightmap.fsh` in the real 26.2
`client.jar`, in this order:

```glsl
float get_brightness(float level) { return level / (4.0 - 3.0 * level); }

float block_brightness = get_brightness(block_level) * lightmapInfo.BlockFactor;
float sky_brightness   = get_brightness(sky_level)   * lightmapInfo.SkyFactor;
vec3  color  = max(AmbientColor, nightVisionColor);
      color += SkyLightColor * sky_brightness;
      color += BlockLightColor * block_brightness;
      color  = clamp(color, 0.0, 1.0);
      color  = mix(color, notGamma(color), BrightnessFactor);
```

Ours, in each of the three shaders:

```wgsl
fn lightmap_term(sky_level: f32, block_level: f32) -> f32 {
    let sky = light_brightness(sky_level) * sky_darken();
    let block = light_brightness(block_level);
    let c = clamp(max(sky, block), 0.0, 1.0);
    return mix(c, not_gamma_grey(c), BRIGHTNESS_FACTOR);
}
```

Three things to notice, each of which the written record got wrong at some point:

1. **The curve is applied to the raw level, and `SkyFactor` multiplies the
   result** — `get_brightness(sky_level) * SkyFactor`, not
   `get_brightness(sky_level * SkyFactor)`. `SkyFactor` is
   `EnvironmentAttributes.SKY_LIGHT_FACTOR`, which is exactly
   `sky_darken_for_time_of_day` (JVM-gated tick by tick in
   `sky_light_factor_timeline.rs`).
2. **`notGamma` is not optional decoration.** `BrightnessFactor` is
   `Options.gamma`, whose default is `0.5` (`Options.java:900`), so a default-settings
   vanilla client always has it half-applied. It is the largest single term in a
   night frame.
3. **`notGamma` collapses for a grey value.** `lightmap.fsh` scales an RGB triple by
   `maxScaled / maxComponent` where `maxScaled = 1 - (1 - maxComponent)^4`; with all
   three components equal that is exactly `1 - (1 - c)^4`, and the division
   disappears (so does vanilla's `0.0 / 0.0` at the darkest texel). Ours is grey
   because the overworld's `AMBIENT_LIGHT_COLOR` is grey and its `SKY_LIGHT_COLOR`
   is white **in daylight** — but see [Two later corrections](#two-later-corrections),
   because neither of those is the whole story and the second one is why this has to
   become a colour.
4. **`AmbientColor` seeds the accumulator** before either light half is added, and
   the overworld's is `0x0A0A0A`, not black. So an unlit surface is `0.0935`, not
   `0.0`.

The **top** endpoint is exact: `get_brightness(1) = 1`, `notGamma(1) = 1`, and the
ambient term clamps away, so full light is `1.0`. That exactness at `1.0` is why most
of the tree was unmoved by this change. The bottom endpoint is *not* zero — it is
the ambient floor.

## What was wrong, and by how much

Two numbers were in the record and both were wrong. Worth keeping because both were
arithmetically checkable at the time and neither looked wrong on inspection.

| midnight, sky 15, block 0 | light term |
| --- | --- |
| retired ramp `0.2 + 0.8 * l` | 0.3920 |
| #386's spec: curve applied **after** `sky_darken`, no `notGamma` | 0.0732 |
| curve **before**, `notGamma` at gamma 0.5, `AmbientColor` dropped | 0.4532 |
| vanilla per `lightmap.fsh`, with `AmbientColor` | **0.5047** |

So **night was never "5.36x too bright"**. At full skylight the retired ramp was
about 14% too *dark*. #386's figure came from composing the curve with `sky_darken`
in the wrong order and from omitting `notGamma`; the two errors point in the same
direction and compound.

The ramp was still wrong, and wrong in the direction #383 measured — the error just
lives in the **middle** of the range rather than at midnight, because the two curves
meet exactly at both endpoints:

| level | retired ramp | vanilla | ambient dropped | ratio (ramp/vanilla) |
| --- | --- | --- | --- | --- |
| 15/15 | 1.000 | 1.000 | 1.000 | 1.00 |
| 12/15 | 0.840 | 0.747 | 0.719 | 1.12 |
| 8/15 | 0.627 | 0.482 | 0.428 | 1.30 |
| 7/15 | 0.573 | 0.423 | 0.363 | 1.35 |
| 4/15 | 0.413 | 0.265 | 0.189 | 1.56 |
| 0 | **0.200** | **0.0935** | 0.000 | 2.14 |

The last row is the mechanism #386 named and the one part of its diagnosis that was
exactly right: a hard 20% floor that no darkening could go below. Vanilla's floor is
`0.0935` — real, but less than half as high.

Note how the third column converges on the second as light rises: at sky 12 the
ambient term is worth only `0.028`. That is precisely why dropping it survived every
daylight gate in the tree, and why the acceptance bands in
`entity_light_pixels` and `grass_light_response_gate` had to be narrowed to `±0.017`
when it was restored — the ambient-free hypothesis is the closest wrong answer either
gate has ever had to reject.

## How to change it

## Two later corrections

Both were stated confidently in the first version of `light.rs` and this doc, and both
are false. Neither looked wrong on inspection; both were found by decoding the raw
`ARGB` ints instead of trusting the prose beside them.

**1. `AmbientColor` is not black in the overworld.** `DimensionTypes.java:36` sets
`AMBIENT_LIGHT_COLOR` to `-16119286`, which is `0xFF0A0A0A` — grey `10/255`. The claim
that "adding it is a no-op" was wrong, and dropping it made every unlit surface `0.000`
instead of `0.0935`: an overshoot past vanilla committed in the course of fixing an
overshoot the other way. Caves and unlit faces rendered absolutely black. Now modelled
as `light::AMBIENT_LIGHT`, added into the combine before the clamp, and the tables above
are the corrected ones.

It is grey in the overworld, which is the only reason the light term is still a scalar.
The Nether's `0x302821` and the End's `0x3F473F` are not.

**2. `SKY_LIGHT_COLOR` is not constant white — it is keyframed, and blue at night.**
`Timelines.java:72` animates it: `-1` (white) at ticks 730 and 11270, then
`NIGHT_SKY_LIGHT_COLOR` at 13140 and 22860. And that constant
(`Timelines.java:30`) is `ARGB.colorFromFloat(1.0F, 0.48F, 0.48F, 1.0F)` — red and green
fall to **48%** while blue holds at **100%**.

So vanilla's night light is not merely dimmer than day, it is a different *hue*. This is
the direct cause of the player report "at night the shadow is supposed to be a different
colour", and it is the remaining reason the light term has to become a `vec3`, alongside
the warm `BLOCK_LIGHT_TINT` of `-10100` = `(1.000, 0.847, 0.549)`. The grey
specialisation of `notGamma` is a daylight-only convenience and stops being right the day
either colour lands. Tracked as [#383]'s third divergence.

## How to change it

* **Change all four copies together.** WGSL has no `#include`, and this crate's
  convention is to duplicate small helpers (`srgb_to_linear` is already three-way
  duplicated). The four are `light.rs`, `model.wgsl`, `entity.wgsl`, `fluid.wgsl`.
* **`block.wgsl` is deliberately not one of them.** It is the packed full-cube path,
  reached only by the demo world (no `client.jar`) and the headless gates —
  `mesher.rs` emits `SectionGeometry::Packed` only when `classifier.models()` is
  `None`. It has neither this curve nor `sky_darken` nor fog, and it multiplies shade
  in *linear* space. That is [#400], not this.
* **Gates must not call `lodestone_render::light`.** Per `CLAUDE.md`, an expected
  value has to originate outside the code under test. Every gate writes
  `level / (4 - 3 * level)` and `1 - (1 - c)^4` out again by hand. A gate that
  imported `brightness` would be the `decode(encode(x))` trap.
* **Assert magnitude at more than one level.** The two candidate curves are equal at
  both endpoints, so a full-bright gate passes on either one and a midnight-only gate
  passes on a curve that merely happens to cross there. The gates below pin sky 7,
  sky 8, sky 12 and midnight, and each computes the *wrong* hypothesis alongside the
  right one and asserts its acceptance band excludes it.

Two consequences worth knowing before touching anything downstream:

* **The `0.0` sky-darken sentinel matters more than it did.** All three shaders read
  a `fog_end_enabled.z` of `<= 0.0` as full daylight, because every caller builds the
  uniform from a `FogUniform` that zeroes the lane. Taking it literally used to pin
  surfaces at the `0.2` floor.
  `entity_night_pixels::the_unset_lane_renders_identically_to_explicit_noon` is the
  gate.
* **Bands must be tight enough to reject the ambient-free hypothesis.** It sits only
  `0.05`–`0.06` from the correct value at every interior level — far closer than the
  retired ramp ever was — and it converges further as light rises (`0.028` at sky 12).
  Every band in the gates below was narrowed to roughly `±0.017` when `AmbientColor` was
  restored. A band inherited from the ramp-versus-curve era is wide enough to accept a
  build that drops ambient entirely, which is exactly how that regression shipped
  green.
* **Ratio gates against light 0 are usable again.** While ambient was dropped, a sky-0
  frame was black, so `dark / bright` was `0.000` under any build that darkens at all —
  including one that draws nothing, i.e. vacuous in the *world* sense. Vanilla's real
  `0.0935` floor restores light 0 as a discriminating measurement point, and both
  `entity_light_pixels` and `grass_light_response_gate` now assert a *band* on that
  ratio rather than "is it black".

## Configuration

`BRIGHTNESS_FACTOR = 0.5`, hardcoded in `light.rs` and in each of the three shaders.
It is vanilla's `Options.gamma` **default**, not a constant of the game: `0.0`
reproduces vanilla's "Moody" (the bare curve, no `notGamma`) and `1.0` its "Bright".
Wiring a brightness setting means threading it through the three shaders' group-0
uniforms; `fog_end_enabled.w` and `fog_eye.w` are the two free lanes.

Nothing else. No flags, no env vars.

## Gates

| gate | what it pins | what it structurally cannot see |
| --- | --- | --- |
| `lodestone-render` `light.rs` unit tests | the curve's shape at 0, 4/15, 7/15, 8/15, 12/15, 0.8, 1.0 and midnight, each against the retired ramp, #386's table *and* the ambient-free chain; that ambient is **added** rather than another `max` candidate; monotonicity and range over all 256 packed bytes at both plateaus | pixels — this is the closed loop, and on its own it proves only that the mirror agrees with itself |
| `entity_night_pixels::a_sky_lit_mob_is_darker_at_midnight_than_at_noon` | **the midnight magnitude at pixels**: 0.50465, with a band that rejects 0.45319 (ambient dropped), 0.392 (retired ramp) and 1.000, asserted rather than described | which curve terrain uses |
| `entity_light_pixels::a_mob_in_shadow_is_darker_than_the_same_mob_in_sunlight` | the sky-7 magnitude, 0.42307, band rejecting 0.36312, 0.57333 and 1.0 | the clock |
| `entity_light_pixels::the_light_floor_is_vanillas_ambient_and_not_the_retired_ramps` | light 0 renders at 0.0935 of daylight on a mob — a band rejecting both 0.2 and 0.0 — with the sunlit frame and the silhouette count as controls | terrain |
| `grass_light_response_gate::tinted_surfaces_respond_to_sky_light_exactly_as_stone_does` | the same 0.42307 on **real baked geometry** through `mesh_models`, for four populations including the tinted and cutout classes | entities |
| `grass_light_response_gate::unlit_faces_reach_vanillas_ambient_floor_and_not_the_retired_ramps` | light 0 is the **same fraction** (0.0935) of daylight in every channel even where a tint multiplies — a tint applied outside the light multiply would read near 1.0 and survive nowhere else | anything about the interior of the curve |
| `screen_effects` unit tests | the underwater overlay tints on the same curve, at 8/15 and at both endpoints, each against the ambient-free value as well as the ramp | that the overlay reaches the screen (that is `screen_overlay_pixels.rs`) |
| `particles::light_term_matches_the_terrain_shader` | break particles shade on the same curve at block 8, and reach the 0.0935 ambient floor unlit | that particles dim at night — they do not; `Particles` has no clock |
| `sky_light_factor_timeline.rs` | `sky_darken_for_time_of_day` against a JVM dump at all 24000 ticks | the ramp — it never touches it, and did not change |

Unmoved by this change, and worth stating because it is the reason the blast radius
was survivable: every gate that renders at **full** light is byte-identical, because
both curves are exactly `1.0` there. That covers `model_ao_corner_gate`,
`entity_diffuse_two_lights_pixels`, `sprite_drop_pixels`,
`thrown_and_held_item_pixels`, `GUI_ITEM_LIGHT` and every GUI/HUD/container item gate.

`first_person_hand_light_pixels` is the one gate deliberately left loose. It draws to
a plain `Rgba8Unorm` target, where the shader's gamma-space shade multiply is *not*
proportional to the readback byte, so the ratio is not predictable from the light term
alone. It asserts direction and a margin only; the magnitude is `entity_night_pixels`'
job, on an sRGB target where the transfer functions cancel.

## Dependencies

* `crates/lodestone-render/src/light.rs` — the Rust authority.
* `crates/lodestone-render/src/shaders/{model,entity,fluid}.wgsl` — the three copies.
* `crates/lodestone-render/src/entity::sky_darken_for_time_of_day` —
  `SKY_LIGHT_FACTOR`, supplied through `fog_end_enabled.z`. See
  [time-of-day-lighting.md](./time-of-day-lighting.md).
* `crates/lodestone-render/src/screen_effects.rs` and
  `crates/lodestone-shell/src/particles.rs` — the two Rust consumers.
* `.cache/mc/26.2/client-src/assets/minecraft/shaders/core/lightmap.fsh`,
  `net/minecraft/client/renderer/Lightmap.java`,
  `net/minecraft/world/attribute/EnvironmentAttributes.java`,
  `net/minecraft/client/Options.java` — the sources every number here is derived from.

[#383]: https://github.com/matteopolak/lodestone/issues/383
[#386]: https://github.com/matteopolak/lodestone/issues/386
[#400]: https://github.com/matteopolak/lodestone/issues/400
