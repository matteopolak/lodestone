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
   because the overworld's `SKY_LIGHT_COLOR` is white (`-1`) and its
   `AMBIENT_LIGHT_COLOR` is black (`-16777216`).

The endpoints are **exact**: `get_brightness(1) = 1` and `notGamma(1) = 1`, so full
light is `1.0`; `get_brightness(0) = 0` and `notGamma(0) = 0`, so no light is `0.0`.
That exactness at `1.0` is why most of the tree was unmoved by this change.

## What was wrong, and by how much

Two numbers were in the record and both were wrong. Worth keeping because both were
arithmetically checkable at the time and neither looked wrong on inspection.

| midnight, sky 15, block 0 | light term |
| --- | --- |
| retired ramp `0.2 + 0.8 * l` | 0.3920 |
| #386's spec: curve applied **after** `sky_darken`, no `notGamma` | 0.0732 |
| vanilla per `lightmap.fsh`: curve **before**, `notGamma` at gamma 0.5 | **0.4532** |

So **night was never "5.36x too bright"**. At full skylight the retired ramp was
about 14% too *dark*. #386's figure came from composing the curve with `sky_darken`
in the wrong order and from omitting `notGamma`; the two errors point in the same
direction and compound.

The ramp was still wrong, and wrong in the direction #383 measured — the error just
lives in the **middle** of the range rather than at midnight, because the two curves
meet exactly at both endpoints:

| level | retired ramp | vanilla (curve + `notGamma`) | ratio |
| --- | --- | --- | --- |
| 15/15 | 1.000 | 1.000 | 1.00 |
| 12/15 | 0.840 | 0.719 | 1.17 |
| 8/15 | 0.627 | 0.428 | 1.46 |
| 7/15 | 0.573 | 0.363 | 1.58 |
| 4/15 | 0.413 | 0.189 | 2.19 |
| 0 | **0.200** | **0.000** | infinite |

The last row is the mechanism #386 named and the one part of its diagnosis that was
exactly right: a hard 20% floor that no darkening could go below. Vanilla has no
floor. Unlit surfaces are now black.

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
  surfaces at the `0.2` floor; now it renders them pure **black**.
  `entity_night_pixels::the_unset_lane_renders_identically_to_explicit_noon` is the
  gate.
* **Ratio gates against light 0 are now degenerate.** A sky-0 frame is black, so
  `dark / bright` is `0.000` under any build that darkens at all — including one that
  draws nothing. Two gates had to move their second measurement point into the
  interior of the curve for this reason (`entity_light_pixels`,
  `grass_light_response_gate`), and each grew a separate assertion that light 0 really
  does reach black.

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
| `lodestone-render` `light.rs` unit tests | the curve's shape at 0, 4/15, 7/15, 8/15, 12/15, 0.8, 1.0 and midnight, each against the retired ramp *and* #386's table; monotonicity and range over all 256 packed bytes at both plateaus | pixels — this is the closed loop, and on its own it proves only that the mirror agrees with itself |
| `entity_night_pixels::a_sky_lit_mob_is_darker_at_midnight_than_at_noon` | **the midnight magnitude at pixels**: 0.4532, with a band that rejects 0.392 and 1.000, asserted rather than described | which curve terrain uses |
| `entity_light_pixels::a_mob_in_shadow_is_darker_than_the_same_mob_in_sunlight` | the sky-7 magnitude, 0.36312, band rejecting 0.57333 and 1.0 | the clock |
| `entity_light_pixels::the_light_floor_is_gone` | light 0 renders pure black on a mob, with the sunlit frame as the control | terrain |
| `grass_light_response_gate::tinted_surfaces_respond_to_sky_light_exactly_as_stone_does` | the same 0.36312 on **real baked geometry** through `mesh_models`, for four populations including the tinted and cutout classes | entities |
| `grass_light_response_gate::unlit_faces_reach_black_with_no_floor` | light 0 is black even where a tint multiplies — a tint applied outside the light multiply would survive nowhere else | anything about the interior of the curve |
| `screen_effects` unit tests | the underwater overlay tints on the same curve, at 8/15 and at both endpoints | that the overlay reaches the screen (that is `screen_overlay_pixels.rs`) |
| `particles::light_term_matches_the_terrain_shader` | break particles shade on the same curve at block 8, and reach 0 unlit | that particles dim at night — they do not; `Particles` has no clock |
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
