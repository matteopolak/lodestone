//! Vanilla's lightmap value, in Rust — the single authority every shader in this
//! crate duplicates and every gate re-derives independently.
//!
//! # What it is
//!
//! Vanilla builds a 16×16 `RGBA8_UNORM` texture (its lightmap) indexed by
//! `(block_level, sky_level)` and multiplies each terrain/entity/particle vertex
//! colour by the texel it samples (its terrain vertex shader: `vertexColor = Color *
//! sample_lightmap(Sampler2, UV2)`). [`light_term`] is that texel, as a scalar.
//!
//! # How it works
//!
//! Straight from vanilla's own lightmap fragment shader in the real 26.2
//! `client.jar` — three stages, in this order:
//!
//! ```text
//! float get_brightness(float level) { return level / (4.0 - 3.0 * level); }
//! ...
//! float block_brightness = get_brightness(block_level) * lightmapInfo.BlockFactor;
//! float sky_brightness   = get_brightness(sky_level)   * lightmapInfo.SkyFactor;
//! color  = max(AmbientColor, nightVisionColor);
//! color += SkyLightColor * sky_brightness;
//! color += BlockLightColor * block_brightness;
//! color  = clamp(color, 0.0, 1.0);
//! color  = mix(color, notGamma(color), BrightnessFactor);
//! ```
//!
//! * [`brightness`] is `get_brightness` — and vanilla's own lightmap
//!   brightness function is the
//!   same expression with the dimension's `ambientLight` lerped in, which is
//!   `0.0` in the overworld.
//! * **The curve is applied to the raw level, and `SkyFactor` multiplies the
//!   result.** Applying the curve *after* multiplying by `SkyFactor` instead
//!   gives a different, wrong answer — see the divergence note at the bottom of
//!   this doc. `SkyFactor` is vanilla's own per-dimension sky-light-factor attribute, i.e. exactly
//!   [`crate::entity::sky_darken_for_time_of_day`] (JVM-gated tick by tick in
//!   `tests/sky_light_factor_timeline.rs`).
//! * [`not_gamma`] is `notGamma`, mixed in at [`BRIGHTNESS_FACTOR`]. This is not
//!   optional decoration: it is the largest single term in a night frame.
//!
//! * [`AMBIENT_LIGHT`] is `AmbientColor`, which seeds the accumulator *before*
//!   either light half is added. It is **not** black in the overworld — see that
//!   constant's own docs, and the correction note at the bottom.
//!
//! One vanilla term is deliberately **not** modelled here, and it is noted at
//! its call sites rather than silently dropped:
//!
//! * The additive **combine**. Vanilla adds the sky and block contributions
//!   (with `BlockFactor` ≈ 1.4 and a warm `BLOCK_LIGHT_TINT`); [`light_term`]
//!   takes their `max`. Fixing that faithfully makes the light term a *colour*
//!   rather than a scalar, because `BLOCK_LIGHT_TINT` is not white — which
//!   widens a vertex output in three shaders, and is why this combine stays
//!   unmodelled today.
//!
//! # How to change it
//!
//! The three WGSL copies (`shaders/model.wgsl`, `shaders/entity.wgsl`,
//! `shaders/fluid.wgsl`) duplicate these functions verbatim — WGSL has no
//! `#include` and this crate's convention is to duplicate small helpers (see
//! `srgb_to_linear`, already three-way duplicated). **Change all three plus this
//! file together**, and note that `shaders/block.wgsl` is the demo-only packed
//! path with its own hand-kept copy of the same terms, not one of these three.
//!
//! The gates must *not* call into this module: per `CLAUDE.md`, an expected value
//! has to originate outside the code under test, so each one writes
//! `level / (4 - 3 * level)` out again. A gate that imported [`brightness`] would
//! be the `decode(encode(x))` trap.
//!
//! # Configuration
//!
//! [`BRIGHTNESS_FACTOR`] is vanilla's `Options.gamma` **default** (`0.5`),
//! hardcoded because there is no brightness setting in this
//! client yet. Wiring one means threading it into the three shaders' uniforms;
//! `0.0` reproduces vanilla's "Moody" and `1.0` its "Bright".
//!
//! # The divergence from applying the curve after `sky_darken`
//!
//! Applying `l / (4 - 3l)` to `l = max(sky * sky_darken, block)` — curve
//! *after* the darken multiply — predicts `0.0732` at midnight against the old
//! ramp's `0.3920`, i.e. "5.36× too bright". Both halves of that are off, and in
//! opposite directions:
//!
//! | midnight, sky 15 | light term |
//! |---|---|
//! | old ramp `0.2 + 0.8·l` | 0.3920 |
//! | curve applied **after** `sky_darken`, no `notGamma` | 0.0732 |
//! | vanilla per `lightmap.fsh` (`curve` **before**, with `notGamma`) | **0.4532** |
//!
//! So night was never 5.36× too bright — at full skylight it was ~14% too
//! *dark*. The ramp is still wrong, but the error lives in the **middle** of the
//! range, not at midnight: at sky level 12 in daylight the old ramp gives
//! `0.840` where vanilla gives `0.719`, and at sky level 4 it gives `0.413`
//! where vanilla gives `0.189`. And the `0.2` floor really is a hard mechanism:
//! an unlit surface read `0.200` where vanilla reads [`AMBIENT_LIGHT`]'s
//! `0.0935`.
//!
//! # Two further corrections, from the jar
//!
//! Both of these were stated confidently in this file's first version and both
//! are false. They were found by decoding the raw `ARGB` ints rather than
//! trusting the prose around them.
//!
//! * **`AmbientColor` is not black in the overworld.** It is `0x0A0A0A`, so the
//!   claim that "adding it is a no-op" was wrong and dropping it made every
//!   unlit surface `0.000` instead of `0.0935` — overshooting past vanilla in the
//!   course of fixing an overshoot the other way. Now modelled; see
//!   [`AMBIENT_LIGHT`].
//! * **`SKY_LIGHT_COLOR` is not constant white.** It is a *timeline* attribute
//!   (vanilla's timeline registration) keyframed `-1` (white) at ticks 730 and 11270 and
//!   `NIGHT_SKY_LIGHT_COLOR` at 13140 and 22860 — and that constant is
//!   `colorFromFloat(1.0, 0.48, 0.48, 1.0)`, i.e. **blue**: red and green fall to
//!   48% while blue holds at 100%. So vanilla's night light is not merely dimmer
//!   than day, it is a different *hue*, which is why the grey specialisation in
//!   [`not_gamma`] is a daylight-only convenience. This is the remaining reason
//!   the light term has to become a `vec3`, alongside the warm
//!   `BLOCK_LIGHT_TINT` of `(1.000, 0.847, 0.549)`.

/// Vanilla's `Options.gamma` default, which its lightmap fragment shader consumes as
/// `BrightnessFactor` to mix [`not_gamma`] in. See this module's "Configuration".
pub const BRIGHTNESS_FACTOR: f32 = 0.5;

/// The overworld's ambient-light-colour dimension attribute, as a grey scalar.
///
/// Vanilla's dimension-type defaults set it to `-16119286`, which is `0xFF0A0A0A` — grey
/// `10/255`, **not** black. Its lightmap fragment shader seeds its accumulator with
/// `max(AmbientColor, nightVisionColor)` before adding either light half, so a
/// fully unlit surface in vanilla is not pure black: it reads `0.0935` once
/// [`not_gamma`] is mixed in. Dropping this term is what made caves render
/// absolutely black.
///
/// It is grey in the overworld, which is the only reason the light term can stay
/// a scalar. The Nether's `0x302821` and the End's `0x3F473F` are *not* grey and
/// belong to the same per-dimension colour pass as the block-light tint.
///
/// Used only by [`light_term`]/[`light_term_from_levels`] (the scalar model)
/// and as the fallback for [`light_color_from_levels`]'s `ambient` parameter
/// when no per-dimension colour is known. It is **not** a universal floor:
/// treating it as one is exactly the bug that under-lit the Nether — see
/// [`OVERWORLD_AMBIENT_LIGHT`].
pub const AMBIENT_LIGHT: f32 = 10.0 / 255.0;

/// [`AMBIENT_LIGHT`] broadcast into all three channels — the overworld's own
/// `AMBIENT_LIGHT_COLOR`, and the correct **default** for
/// [`light_color_from_levels`]'s `ambient` parameter when a caller has no
/// per-dimension colour yet (pre-login, an offline demo, a hermetic test).
/// Never the right value for a *known* non-overworld dimension: the Nether's
/// real floor is [`rgb24_to_channels`]`(0x302821)` and the End's is
/// [`rgb24_to_channels`]`(0x3F473F)`, both markedly brighter than this grey —
/// see `DimensionType::ambient_light_color`'s doc for the source and
/// `nether_ambient_floor_reads_meaningfully_brighter_than_the_overworlds`
/// below for the measured gap.
pub const OVERWORLD_AMBIENT_LIGHT: [f32; 3] = [AMBIENT_LIGHT, AMBIENT_LIGHT, AMBIENT_LIGHT];

/// Unpacks a `0xRRGGBB` colour — as decoded off the wire by
/// `DimensionType::ambient_light_color` — into per-channel `0.0..=1.0` floats.
/// Vanilla's packed-24-bit-to-vector unpack: a bare `byte / 255`, no linearisation
/// (this module's whole convention — see `srgb_to_linear`'s doc elsewhere in
/// this crate for why vanilla's lightmap constants are never gamma-corrected
/// on the way in).
#[must_use]
pub fn rgb24_to_channels(packed: u32) -> [f32; 3] {
    [
        f32::from(((packed >> 16) & 0xFF) as u8) / 255.0,
        f32::from(((packed >> 8) & 0xFF) as u8) / 255.0,
        f32::from((packed & 0xFF) as u8) / 255.0,
    ]
}

/// Vanilla's `get_brightness`: one lightmap axis' response to a `0.0..=1.0` light
/// level (the wire nibble over 15).
///
/// Its lightmap fragment shader's `level / (4.0 - 3.0 * level)`, equal to
/// vanilla's own lightmap brightness function with the overworld's `ambientLight` of `0.0`. `0.0`
/// maps to `0.0` and `1.0` to `1.0`; in between it is strongly concave — half
/// light is a fifth of the brightness, which is the whole reason a linear ramp
/// looked so wrong in the middle of the range.
#[must_use]
pub fn brightness(level: f32) -> f32 {
    level / (4.0 - 3.0 * level)
}

/// Vanilla's `notGamma`, specialised to a grey value.
///
/// Its lightmap fragment shader scales an RGB triple by `maxScaled / maxComponent` where
/// `maxScaled = 1 - (1 - maxComponent)^4`. When all three components are equal
/// that whole expression collapses to `1 - (1 - c)^4` with no division — which
/// also removes vanilla's `0.0 / 0.0` at the darkest texel. Grey is the right
/// specialisation here because the overworld's `SKY_LIGHT_COLOR` is white
/// (`-1`) and its `AMBIENT_LIGHT_COLOR` is black; it stops being right the day
/// the block-light tint is modelled (see this module's docs).
#[must_use]
pub fn not_gamma(c: f32) -> f32 {
    let inverted = 1.0 - c;
    1.0 - inverted * inverted * inverted * inverted
}

/// The lightmap value for a *combined* brightness, i.e. vanilla's lightmap
/// fragment shader's final
/// two lines: clamp, then mix [`not_gamma`] in at [`BRIGHTNESS_FACTOR`].
///
/// Exactly `1.0` at `1.0` and exactly `0.0` at `0.0`, so every full-bright path
/// in the tree (GUI items, `GUI_ITEM_LIGHT`, the sky-15 daylight gates) is
/// byte-identical to what it produced under the old linear ramp.
#[must_use]
pub fn apply_brightness_option(combined: f32) -> f32 {
    let c = combined.clamp(0.0, 1.0);
    c + (not_gamma(c) - c) * BRIGHTNESS_FACTOR
}

/// The full lightmap sample for a packed `sky << 4 | block` byte under a
/// `sky_darken` of vanilla's own per-dimension sky-light-factor attribute.
///
/// `sky_darken` scales only the sky half — block light is a torch and does not
/// dim at dusk. Pass `1.0` for full daylight; the shaders additionally treat
/// `0.0` as a "lane never written" sentinel meaning daylight, which is a
/// uniform-plumbing concern and deliberately not repeated here.
#[must_use]
pub fn light_term(packed_light: u8, sky_darken: f32) -> f32 {
    let sky = f32::from((packed_light >> 4) & 0x0F) / 15.0;
    let block = f32::from(packed_light & 0x0F) / 15.0;
    light_term_from_levels(sky, block, sky_darken)
}

/// [`light_term`] from already-unpacked `0.0..=1.0` levels, for callers whose
/// wire encoding is not a single byte (vanilla's particle path packs block light
/// at bit 4 and sky light at bit 20).
#[must_use]
pub fn light_term_from_levels(sky_level: f32, block_level: f32, sky_darken: f32) -> f32 {
    let sky = brightness(sky_level) * sky_darken;
    let block = brightness(block_level);
    // `AMBIENT_LIGHT` is *added*, as vanilla adds it — it is a floor under the
    // whole range, not another candidate for the `max`. It clamps away at full
    // light, so every full-bright path in the tree stays exactly 1.0.
    apply_brightness_option(AMBIENT_LIGHT + sky.max(block))
}

// ---------------------------------------------------------------------------
// The colour lightmap (N1/N2/N3): `light_term`/`light_term_from_levels` above
// are a scalar model of a texel that is genuinely three-channel in vanilla.
// The scalar is *exactly* vanilla's blue channel (`not_gamma_grey`'s grey
// specialisation collapses to vanilla's real `notGamma` when blue is the max
// component, which it is at night), which is why every gate above passes on a
// wrong hue and a right blue byte — a textbook "magnitude" vacuous test per
// `CLAUDE.md`: the assert is right, its *subject* (one channel of three) is
// not. `light_color_from_levels` below is the real, faithful vec3 port.
// ---------------------------------------------------------------------------

/// Vanilla's warm torch tint, its own per-dimension block-light-tint attribute
/// (`0xFFFFD88C`), as linear-space-agnostic sRGB
/// bytes over 255 — the same "no linearisation" convention every other
/// lightmap constant in this module uses (vanilla's packed-24-bit-to-vector
/// unpack is a bare
/// `byte / 255`).
pub const BLOCK_LIGHT_TINT: [f32; 3] = [1.0, 216.0 / 255.0, 140.0 / 255.0];

/// Vanilla's lightmap render-state extraction function: `blockFactor =
/// blockLightFlicker + 1.4F`. The flicker term is not modelled here (see this
/// module's "How to change it" — a visible torch shimmer, tracked as a
/// follow-up); `1.4` alone is what keeps every hermetic gate deterministic.
pub const BLOCK_FACTOR: f32 = 1.4;

/// Vanilla's `notGamma` (`lightmap.fsh:24-29`), the real three-channel form:
/// scale the whole triple by `maxScaled / maxComponent` where `maxScaled = 1 -
/// (1 - maxComponent)^4`, rather than [`not_gamma`]'s grey specialisation
/// (which is only exact when all three channels already agree).
///
/// Guards vanilla's `0.0 / 0.0` at the darkest texel (`max == 0.0`) by
/// returning black rather than propagating a `NaN` — `lightmap.fsh` runs on a
/// GPU where that division is hardware-defined, not a Rust panic, and this
/// makes the two behave the same way for the one input where the formula
/// itself is undefined.
#[must_use]
pub fn not_gamma_vec3(c: [f32; 3]) -> [f32; 3] {
    let max_component = c[0].max(c[1]).max(c[2]);
    if max_component <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let inv = 1.0 - max_component;
    let max_scaled = 1.0 - inv * inv * inv * inv;
    let ratio = max_scaled / max_component;
    [c[0] * ratio, c[1] * ratio, c[2] * ratio]
}

/// Vanilla's own per-dimension `SKY_LIGHT_COLOR` timeline track, recovered
/// from `sky_darken` (`SKY_LIGHT_FACTOR`) instead of the raw tick.
///
/// `SKY_LIGHT_COLOR` and `SKY_LIGHT_FACTOR` share identical keyframe ticks —
/// `730 / 11270 / 13140 / 22860` (vanilla's timeline registration) — and neither track
/// calls `.setEasing(...)`, so both interpolate linearly on the same
/// parameter. `SKY_LIGHT_FACTOR` runs `1.0` (day, ticks `≤ 730` and the
/// `11270..13140` plateau) down to `0.24` (night, `≥ 13140`), so
///
/// ```text
/// t = clamp((1.0 - sky_darken) / (1.0 - 0.24), 0.0, 1.0)
/// ```
///
/// is the same interpolation parameter `SKY_LIGHT_COLOR` uses, and
/// `srgbLerp(t, white, NIGHT_SKY_LIGHT_COLOR)` (`NIGHT_SKY_LIGHT_COLOR =
/// colorFromFloat(1.0, 0.48, 0.48, 1.0)` = `0xFF7A7AFF`, vanilla's timeline
/// registration)
/// recovers the colour — **verified byte-exact** against the JVM oracle
/// `tests/support/sky_light_timeline_jvm.txt` at ticks 0, 12000, 13000, 13140
/// (see this function's tests), including `Mth.lerpInt`'s `floor` (a `round`
/// here is off by one byte on roughly half of all ticks).
///
/// **Two known exceptions, both momentary, and both safe under the `clamp`:**
/// vanilla's sky-flash overrides (its per-level tick,
/// and its lightmap render-state extraction function) push `SKY_LIGHT_FACTOR` to or
/// above `1.0` during a lightning flash without touching `SKY_LIGHT_COLOR`,
/// so for a few ticks this derivation reads pure white (`t` clamped to `0.0`)
/// where vanilla would still show a faint blue tint it never actually applies
/// during the flash either (both tracks are read at the same instant, and the
/// colour track's own `-1`/white keyframe is what a `sky_darken` of `1.0`
/// already maps to) — the clamp does not paper over a real divergence here,
/// it reproduces the one moment the two tracks are farthest from their normal
/// relationship, and lands on the value vanilla is *already* extracting for
/// its colour track at that instant.
#[must_use]
pub fn sky_light_color_from_darken(sky_darken: f32) -> [f32; 3] {
    const NIGHT: [i32; 3] = [0x7A, 0x7A, 0xFF];
    let t = ((1.0 - sky_darken) / (1.0 - 0.24)).clamp(0.0, 1.0);
    [
        f32::from(crate::sky::lerp_int(t, 0xFF, NIGHT[0]).clamp(0, 255) as u8) / 255.0,
        f32::from(crate::sky::lerp_int(t, 0xFF, NIGHT[1]).clamp(0, 255) as u8) / 255.0,
        f32::from(crate::sky::lerp_int(t, 0xFF, NIGHT[2]).clamp(0, 255) as u8) / 255.0,
    ]
}

/// `lightmap.fsh`'s parabolic block-tint mix factor, `(2l - 1)^2`
/// (`:31-33`) — `0.0` at `l = 0.5`, rising to `1.0` at both ends, so the tint
/// is strongest at the darkest and brightest block levels and vanishes in the
/// middle.
fn parabolic_mix_factor(level: f32) -> f32 {
    let x = 2.0 * level - 1.0;
    x * x
}

/// The full three-channel lightmap sample — the faithful port
/// `light_term_from_levels` is not. Straight from `lightmap.fsh:35-65`, in
/// order:
///
/// ```text
/// block_brightness = get_brightness(block_level) * BlockFactor
/// sky_brightness   = get_brightness(sky_level)   * SkyFactor
/// color  = AmbientColor                                    // per-dimension, see `ambient`
/// color += SkyLightColor * sky_brightness                  // see sky_light_color_from_darken
/// BlockLightColor = mix(BlockLightTint, white, 0.9 * parabolic(block_level))
/// color += BlockLightColor * block_brightness
/// color  = clamp(color, 0, 1)
/// color  = mix(color, notGamma(color), BrightnessFactor)
/// ```
///
/// `ambient` is vanilla's own per-dimension ambient-light-colour attribute for the *current*
/// dimension — [`rgb24_to_channels`] of `DimensionType::ambient_light_color`,
/// or [`OVERWORLD_AMBIENT_LIGHT`] as the safe default when no per-dimension
/// colour is known yet. It is added once, not per-channel `max`ed with
/// anything — same combine rule as the scalar model. The **sky/block combine
/// changed from `max` to additive**, which [`light_term_from_levels`]'s doc
/// already flags as the one deliberately unmodelled term; this function
/// models it.
///
/// Passing [`OVERWORLD_AMBIENT_LIGHT`] for `ambient` reproduces this
/// function's behaviour before per-dimension colour existed, byte for byte —
/// see `daylight_vec3_reduces_to_the_existing_scalar_when_block_light_is_absent`
/// and friends below, none of which changed when this parameter was added.
#[must_use]
pub fn light_color_from_levels(
    sky_level: f32,
    block_level: f32,
    sky_darken: f32,
    ambient: [f32; 3],
) -> [f32; 3] {
    let sky_brightness = brightness(sky_level) * sky_darken;
    let block_brightness = brightness(block_level) * BLOCK_FACTOR;
    let sky_light_color = sky_light_color_from_darken(sky_darken);

    let block_mix = 0.9 * parabolic_mix_factor(block_level);
    let block_light_color = [
        BLOCK_LIGHT_TINT[0] + (1.0 - BLOCK_LIGHT_TINT[0]) * block_mix,
        BLOCK_LIGHT_TINT[1] + (1.0 - BLOCK_LIGHT_TINT[1]) * block_mix,
        BLOCK_LIGHT_TINT[2] + (1.0 - BLOCK_LIGHT_TINT[2]) * block_mix,
    ];

    let mut color = [
        ambient[0] + sky_light_color[0] * sky_brightness + block_light_color[0] * block_brightness,
        ambient[1] + sky_light_color[1] * sky_brightness + block_light_color[1] * block_brightness,
        ambient[2] + sky_light_color[2] * sky_brightness + block_light_color[2] * block_brightness,
    ];
    color = [color[0].clamp(0.0, 1.0), color[1].clamp(0.0, 1.0), color[2].clamp(0.0, 1.0)];

    let ng = not_gamma_vec3(color);
    [
        color[0] + (ng[0] - color[0]) * BRIGHTNESS_FACTOR,
        color[1] + (ng[1] - color[1]) * BRIGHTNESS_FACTOR,
        color[2] + (ng[2] - color[2]) * BRIGHTNESS_FACTOR,
    ]
}

/// [`light_color_from_levels`] from a packed `sky << 4 | block` byte, the
/// vec3 twin of [`light_term`]. `ambient` is the current dimension's
/// `AMBIENT_LIGHT_COLOR` — see that parameter's doc on
/// [`light_color_from_levels`].
#[must_use]
pub fn light_color(packed_light: u8, sky_darken: f32, ambient: [f32; 3]) -> [f32; 3] {
    let sky = f32::from((packed_light >> 4) & 0x0F) / 15.0;
    let block = f32::from(packed_light & 0x0F) / 15.0;
    light_color_from_levels(sky, block, sky_darken, ambient)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old ramp this module replaced, kept only as the wrong hypothesis every
    /// magnitude assertion below is measured against. Per `CLAUDE.md`: predict
    /// the value under *both* hypotheses and require the measurement to land on
    /// the right one, because the two curves agree at both endpoints and a
    /// single-point gate passes on either.
    fn retired_linear_ramp(level: f32) -> f32 {
        0.2 + 0.8 * level
    }

    /// Three named proof points on the bare curve. These are
    /// arithmetic on `l / (4 - 3l)` — written out here rather than taken from
    /// [`brightness`]' own output — and they pin the *shape*: the curves cross at
    /// both endpoints, so only an interior point can tell them apart.
    #[test]
    fn the_bare_curve_matches_vanillas_get_brightness_at_three_levels() {
        for (level, vanilla, old) in [
            (0.24_f32, 0.073_170_73_f32, 0.392_f32),
            (0.8, 0.5, 0.84),
            (1.0, 1.0, 1.0),
        ] {
            assert!(
                (brightness(level) - vanilla).abs() < 1e-6,
                "get_brightness({level}) must be {vanilla}, got {}",
                brightness(level)
            );
            assert!(
                (retired_linear_ramp(level) - old).abs() < 1e-6,
                "the wrong hypothesis at {level} must be {old} (if this drifts, the \
                 discrimination below is not measuring what it claims)"
            );
        }
        // ...and the two only agree at the endpoints, which is why every daylight
        // gate in the tree missed this.
        assert!((brightness(1.0) - retired_linear_ramp(1.0)).abs() < 1e-6);
        assert!(
            (brightness(0.8) - retired_linear_ramp(0.8)).abs() > 0.3,
            "the curves must be far apart in the interior or no gate can see the fix"
        );
    }

    /// The endpoints survive the whole chain exactly, which is what keeps every
    /// full-bright gate in the tree byte-identical.
    #[test]
    fn the_endpoints_are_exact() {
        assert!((apply_brightness_option(1.0) - 1.0).abs() < f32::EPSILON);
        assert!(apply_brightness_option(0.0).abs() < f32::EPSILON);
        // Full light stays *exactly* 1.0 with the ambient term added, because it
        // clamps away. This is what keeps every full-bright gate byte-identical.
        assert!((light_term(0xFF, 1.0) - 1.0).abs() < f32::EPSILON);
    }

    /// An unlit surface is vanilla's ambient floor, and the three candidate
    /// values are far enough apart that nothing can pass this by accident: the
    /// retired ramp's `0.2`, pure black, and vanilla's `0x0A0A0A`.
    ///
    /// The expected value is arithmetic on `10/255` and `Options.gamma`, written
    /// out here rather than taken from [`AMBIENT_LIGHT`] or [`not_gamma`].
    #[test]
    fn an_unlit_surface_lands_on_vanillas_ambient_floor() {
        let ambient = 10.0_f32 / 255.0;
        let expected = {
            let inv = 1.0 - ambient;
            let ng = 1.0 - inv * inv * inv * inv;
            ambient + (ng - ambient) * 0.5
        };
        assert!(
            (expected - 0.093_545_4).abs() < 1e-6,
            "the hand-derived floor drifted: {expected}"
        );

        let ours = light_term(0x00, 1.0);
        assert!(
            (ours - expected).abs() < 1e-6,
            "an unlit surface must read vanilla's ambient floor {expected}, not the \
             retired ramp's {} and not pure black; got {ours}",
            apply_brightness_option(retired_linear_ramp(0.0))
        );
        // Discrimination against both wrong hypotheses, not just one.
        assert!(
            ours > 0.05,
            "dropping AmbientColor renders caves absolutely black; got {ours}"
        );
        assert!(
            ours < 0.2,
            "the retired 0.2 floor must be gone -- that is what made night readable"
        );
    }

    /// Midnight under open sky, now that the floor is back. The ambient term is
    /// additive, so it moves this off `0.453189` by exactly its own contribution
    /// — and a build that dropped it again would land back on the old number.
    #[test]
    fn ambient_light_is_additive_not_another_max_candidate() {
        let midnight = 0.24_f32;
        let ours = light_term(0xF0, midnight);
        // Were ambient folded in with a `max`, it would lose to sky light here
        // and this would still read the ambient-free 0.453189.
        assert!(
            (ours - 0.453_189_1).abs() > 0.01,
            "ambient must *add* to the sky term, not compete with it via max; got \
             {ours}"
        );
        assert!(
            (ours - 0.504_652).abs() < 1e-4,
            "midnight under open sky must be 0.504652; got {ours}"
        );
    }

    /// Full sky light and full block light both reach `1.0`, so a torch-lit room
    /// and open noon sky agree — and neither is clamped.
    #[test]
    fn full_light_reaches_one_from_either_half() {
        assert!((light_term(0xF0, 1.0) - 1.0).abs() < 1e-6);
        assert!((light_term(0x0F, 1.0) - 1.0).abs() < 1e-6);
        assert!((light_term(0xFF, 1.0) - 1.0).abs() < 1e-6);
    }

    /// Sky darkening scales **only** the sky half. Getting this wrong blacks out
    /// every lit interior at sunset, which is what `entity_night_pixels`'
    /// torch-lit invariance gate exists to catch at pixels.
    #[test]
    fn sky_darken_cannot_touch_block_light() {
        let midnight = 0.24;
        assert!(
            (light_term(0x0F, midnight) - light_term(0x0F, 1.0)).abs() < 1e-6,
            "a torch-lit surface must be as bright at midnight as at noon"
        );
        assert!(
            light_term(0xF0, midnight) < light_term(0xF0, 1.0) - 0.4,
            "a sky-lit surface must be substantially darker at midnight"
        );
    }

    /// The midnight number this module's curve-and-combine order turns on, with
    /// the curve-after-darken table as the third hypothesis. All three are computed here from constants
    /// that originate in vanilla's lightmap fragment shader and gamma option, not from the shader.
    #[test]
    fn midnight_lands_on_vanillas_value_and_not_on_either_wrong_one() {
        let midnight = 0.24_f32;
        let ours = light_term(0xF0, midnight);

        // Hypothesis A, the retired ramp: `0.2 + 0.8 * (1.0 * 0.24)`.
        let old = retired_linear_ramp(midnight);
        // Hypothesis B, the curve applied *after* `sky_darken`,
        // with no `notGamma` — `0.24 / (4 - 3 * 0.24)`.
        let issue_table = midnight / (4.0 - 3.0 * midnight);
        // Hypothesis C, this file's own first version: vanilla's chain but with
        // `AmbientColor` dropped as a believed no-op.
        let ambient_free = {
            let c = 1.0_f32 * midnight;
            let ng = 1.0 - (1.0 - c) * (1.0 - c) * (1.0 - c) * (1.0 - c);
            c + (ng - c) * 0.5
        };
        // Vanilla: `max(AmbientColor, …)` seeds the accumulator, then curve first
        // (`1.0`), then `* SkyFactor` added in, then `notGamma` mixed at the
        // default gamma of 0.5.
        let vanilla = {
            let c = 10.0_f32 / 255.0 + 1.0 * midnight;
            let ng = 1.0 - (1.0 - c) * (1.0 - c) * (1.0 - c) * (1.0 - c);
            c + (ng - c) * 0.5
        };

        assert!((old - 0.392).abs() < 1e-6, "hypothesis A drifted: {old}");
        assert!(
            (issue_table - 0.073_170_73).abs() < 1e-6,
            "hypothesis B drifted: {issue_table}"
        );
        assert!(
            (ambient_free - 0.453_189_1).abs() < 1e-6,
            "hypothesis C drifted: {ambient_free}"
        );
        assert!(
            (vanilla - 0.504_652).abs() < 1e-4,
            "vanilla's midnight value drifted: {vanilla}"
        );
        assert!(
            (ours - vanilla).abs() < 1e-6,
            "midnight must land on vanilla's {vanilla} -- not the retired ramp's \
             {old}, not the curve-after-darken table's {issue_table}, and not the ambient-free \
             {ambient_free}; got {ours}"
        );
    }

    /// Four interior levels, because midnight alone cannot distinguish a correct
    /// curve from one that happens to pass through a single point.
    ///
    /// Each row carries all three hypotheses: vanilla, the retired linear ramp,
    /// and the ambient-free chain this file shipped first. The two margins both
    /// **shrink as light rises** — the ambient term clamps toward saturation — so
    /// they are asserted per-row rather than against one global threshold. At sky
    /// 12 the ambient term is worth only `0.028`, which is precisely why dropping
    /// it survived every daylight gate in the tree.
    #[test]
    fn a_daylight_mid_level_lands_on_vanillas_value() {
        for (nibble, vanilla, ambient_free) in [
            (4_u8, 0.264_885_9_f32, 0.188_632_9_f32),
            (7, 0.423_042_0, 0.363_117_6),
            (8, 0.481_948_0, 0.428_136_1),
            (12, 0.747_067_5, 0.718_75),
        ] {
            let ours = light_term(nibble << 4, 1.0);
            let old = retired_linear_ramp(f32::from(nibble) / 15.0);
            assert!(
                (ours - vanilla).abs() < 1e-5,
                "sky {nibble} in daylight must be {vanilla} (vanilla), not {old} (the \
                 retired ramp) and not {ambient_free} (ambient dropped); got {ours}"
            );
            assert!(
                (ours - old).abs() > 0.09,
                "sky {nibble} must discriminate against the retired ramp, but {ours} \
                 and {old} are less than 0.09 apart"
            );
            assert!(
                (ours - ambient_free).abs() > 0.02,
                "sky {nibble} must discriminate against the ambient-free chain, but \
                 {ours} and {ambient_free} are less than 0.02 apart"
            );
        }
    }

    /// Monotonic and bounded across every one of the 256 packed bytes at both
    /// plateaus of the day, so no input produces a value outside `0.0..=1.0`
    /// (the shaders feed this straight into a colour multiply).
    #[test]
    fn every_packed_byte_stays_in_range() {
        for darken in [0.24_f32, 1.0] {
            for packed in 0..=u8::MAX {
                let v = light_term(packed, darken);
                assert!(
                    (0.0..=1.0).contains(&v),
                    "packed 0x{packed:02X} at darken {darken} produced {v}"
                );
            }
            for sky in 0..15_u8 {
                assert!(
                    light_term(sky << 4, darken) <= light_term((sky + 1) << 4, darken),
                    "sky light must be monotonic at darken {darken}"
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // Gate E (N1/N2/N3): the colour lightmap. `q = (R/B at night) / (R/B at
    // noon)` on one pixel cancels the texture's own colour, so it needs no
    // knowledge of the subject — CLAUDE.md's ratio-of-ratios gate.
    // -----------------------------------------------------------------

    /// [`sky_light_color_from_darken`] against the existing JVM oracle
    /// (`tests/support/sky_light_timeline_jvm.txt`), at the same four ticks
    /// the diagnosis verified by hand: `0`, `12000` (both the flat white
    /// plateau), `13000` (mid-ramp) and `13140` (the night plateau's first
    /// tick). `sky_darken_for_time_of_day` is a separately JVM-gated primitive
    /// (`tests/sky_light_factor_timeline.rs`), used here as a trusted input —
    /// what this test checks is the colour recovered *from* that factor, not
    /// the factor itself.
    #[test]
    fn sky_light_color_matches_the_jvm_oracle_byte_exact() {
        for (tick, expected) in [
            (0_i64, [0xcb_i32, 0xcb, 0xff]),
            (12000, [0xcb, 0xcb, 0xff]),
            (13000, [0x83, 0x83, 0xff]),
            (13140, [0x7a, 0x7a, 0xff]),
        ] {
            let darken = crate::entity::sky_darken_for_time_of_day(tick);
            let got = sky_light_color_from_darken(darken);
            let got_bytes = got.map(|c| (c * 255.0).round() as i32);
            assert_eq!(got_bytes, expected, "tick {tick}: sky_darken={darken}");
        }
    }

    /// Vanilla's sky-flash override pushes `SKY_LIGHT_FACTOR` to or above
    /// `1.0` momentarily; the `clamp` in [`sky_light_color_from_darken`] must
    /// turn that into white rather than a negative `t` or a panic.
    #[test]
    fn a_flash_pushed_factor_above_one_clamps_to_white_not_garbage() {
        assert_eq!(sky_light_color_from_darken(1.5), [1.0, 1.0, 1.0]);
        assert_eq!(sky_light_color_from_darken(1.0), [1.0, 1.0, 1.0]);
    }

    /// The daylight regression pin, written *before* any shader touches this.
    ///
    /// **Not** a claim that every packed byte's vec3 result matches the old
    /// scalar — whenever block light is present the two are *supposed* to
    /// diverge now, because [`light_color_from_levels`] also fixes N2 (the
    /// additive sky/block combine and `BlockFactor`/tint the scalar model
    /// never implemented, `light_term_from_levels`'s own doc flags this as
    /// deliberately unmodelled). The genuine invariant is narrower: at full
    /// daylight `SKY_LIGHT_COLOR` is white across the whole flat plateau
    /// (`730..11270`), so a **pure sky-lit texel — block level exactly `0`,
    /// any sky level** — must reduce to grey and match the old scalar exactly,
    /// because both the hue fix (N1) and the combine fix (N2) are inert when
    /// block light is absent. This is the cheapest possible guard against the
    /// re-baselining risk `docs/time-of-day-lighting.md` flagged for that
    /// slice of the input space.
    #[test]
    fn daylight_vec3_reduces_to_the_existing_scalar_when_block_light_is_absent() {
        for sky in 0..=15_u8 {
            let packed = sky << 4;
            let scalar = light_term(packed, 1.0);
            let colour = light_color(packed, 1.0, OVERWORLD_AMBIENT_LIGHT);
            for (i, c) in colour.iter().enumerate() {
                assert!(
                    (c - scalar).abs() < 1e-5,
                    "packed 0x{packed:02X} channel {i}: colour {c} != scalar {scalar}"
                );
            }
        }
    }

    /// The mirror of the pin above, stated as a positive fact rather than an
    /// absence of failure: **any** packed byte with block light present must
    /// now clearly diverge from the old scalar (N2's additive combine and
    /// `BlockFactor` are not a rounding-sized correction), so a fix that
    /// silently kept the old `max`-based combine could not pass this file's
    /// own block-light gates by accident.
    #[test]
    fn block_light_present_diverges_from_the_old_scalar_combine() {
        // Excludes block level 15: both models clamp to exactly 1.0 at full
        // brightness, so that one endpoint is a coincidence, not evidence.
        for block in 1..15_u8 {
            let packed = block; // sky = 0
            let scalar = light_term(packed, 1.0);
            let colour = light_color(packed, 1.0, OVERWORLD_AMBIENT_LIGHT);
            assert!(
                (colour[0] - scalar).abs() > 0.005,
                "block {block}: colour {colour:?} must diverge from scalar {scalar} \
                 (N2's additive combine + BlockFactor + tint)"
            );
        }
    }

    /// **The measurement that settles hue vs. brightness**, per-channel, at
    /// vanilla's exact midnight value. `light_term`'s existing
    /// `midnight_lands_on_vanillas_value_and_not_on_either_wrong_one` passes
    /// at `0.504652` — this pins that that number is the **blue** channel and
    /// nothing else, by requiring red to *miss* it by more than `0.2`.
    #[test]
    fn midnight_blue_matches_the_old_scalar_gate_but_red_does_not() {
        let midnight = 0.24_f32;
        // sky level 15 (1.0), block 0 — open sky, unlit.
        let colour = light_color_from_levels(1.0, 0.0, midnight, OVERWORLD_AMBIENT_LIGHT);
        assert!((colour[2] - 0.504_652).abs() < 1e-4, "blue: {colour:?}");
        assert!((colour[0] - 0.278_367).abs() < 1e-4, "red: {colour:?}");
        assert_eq!(colour[0], colour[1], "red and green must agree: {colour:?}");
        assert!(
            (colour[0] - 0.504_652).abs() > 0.2,
            "red must clearly miss the blue-channel value the old scalar gate \
             matched, or this is not discriminating hue from brightness: {colour:?}"
        );
    }

    /// Gate E's ratio-of-ratios: `q = (R/B at tick T) / (R/B at noon)` on the
    /// same open-sky, unlit pixel. Vanilla `0.551596` at midnight and
    /// `0.570359` at tick 13000 (two ticks, so a single-point coincidence
    /// cannot pass); this client before N1 read `1.000000` at both, because
    /// the scalar model has no hue to lose.
    #[test]
    fn ratio_of_ratios_lands_on_vanillas_hue_not_grey() {
        let noon = light_color_from_levels(1.0, 0.0, 1.0, OVERWORLD_AMBIENT_LIGHT);
        assert_eq!(noon[0], noon[2], "noon must be grey (SKY_LIGHT_COLOR is white)");
        let q_noon = noon[0] / noon[2];
        assert!((q_noon - 1.0).abs() < 1e-6);

        for (tick, sky_darken, expected_q) in [
            (18000_i64, 0.24_f32, 0.551_596_f32),
            (13000, crate::entity::sky_darken_for_time_of_day(13000), 0.570_359),
        ] {
            let c = light_color_from_levels(1.0, 0.0, sky_darken, OVERWORLD_AMBIENT_LIGHT);
            let q = (c[0] / c[2]) / q_noon;
            assert!(
                (q - expected_q).abs() < 1e-3,
                "tick {tick}: q={q}, want {expected_q}"
            );

            // Negative control, executed and observed to fail: the retained
            // scalar `light_term` has no hue, so its ratio-of-ratios is
            // exactly 1.0 regardless of tick — the pre-N1 measurement.
            let scalar_q = light_term(0xF0, sky_darken) / light_term(0xF0, sky_darken);
            assert!(
                (scalar_q - 1.0).abs() < 1e-6,
                "control: scalar q must be exactly 1.0 (it has no red/blue \
                 distinction at all), got {scalar_q}"
            );
        }
    }

    /// **Measure by location, not by frame average.** Three spatially distinct
    /// populations, all computed at the same instant (midnight): open-sky
    /// (blue, `q ≈ 0.55`), a cave (`sky = 0`, must stay exactly grey — the
    /// control against the laziest wrong fix, a global night tint, which
    /// would pass the open-sky row and fail here), and torch-lit (warm,
    /// time-invariant).
    #[test]
    fn three_populations_in_one_frame_disagree_the_way_vanilla_does() {
        let midnight = 0.24_f32;

        // Open sky, unlit: blue.
        let open_sky = light_color_from_levels(1.0, 0.0, midnight, OVERWORLD_AMBIENT_LIGHT);
        assert!(open_sky[2] > open_sky[0] * 1.5, "open sky must read blue: {open_sky:?}");

        // Cave: sky level 0, block level 0. `AmbientColor` is a neutral
        // `#0a0a0a`, and at sky level 0 `SkyLightColor` contributes nothing
        // regardless of its own hue, so this must be exactly grey no matter
        // what tick it is measured at.
        let cave_midnight = light_color_from_levels(0.0, 0.0, midnight, OVERWORLD_AMBIENT_LIGHT);
        let cave_noon = light_color_from_levels(0.0, 0.0, 1.0, OVERWORLD_AMBIENT_LIGHT);
        for c in [cave_midnight, cave_noon] {
            assert!(
                (c[0] - 0.093_545_4).abs() < 1e-5
                    && c[0] == c[1]
                    && c[1] == c[2],
                "a cave must stay exactly grey at vanilla's ambient floor: {c:?}"
            );
        }

        // Torch-lit: sky 0, block 8/15. Time-invariant (block light does not
        // dim at dusk), and warm — R/B = 1.664 in vanilla, where the retained
        // scalar model reads a neutral grey (R/B = 1.0).
        let block_level = 8.0 / 15.0;
        let torch_midnight = light_color_from_levels(0.0, block_level, midnight, OVERWORLD_AMBIENT_LIGHT);
        let torch_noon = light_color_from_levels(0.0, block_level, 1.0, OVERWORLD_AMBIENT_LIGHT);
        for (label, c) in [("midnight", torch_midnight), ("noon", torch_noon)] {
            assert!(
                (c[0] - 0.586_090).abs() < 1e-3,
                "{label} torch red: {c:?}"
            );
            assert!(
                (c[1] - 0.506_752).abs() < 1e-3,
                "{label} torch green: {c:?}"
            );
            assert!(
                (c[2] - 0.352_304).abs() < 1e-3,
                "{label} torch blue: {c:?}"
            );
        }
        assert_eq!(torch_midnight, torch_noon, "block light must not respond to the clock at all");

        // Negative control, executed and observed to fail: the retained
        // scalar model reads the cave correctly (it is genuinely grey) but
        // reads the torch as neutral grey too, missing vanilla's warm tint.
        let scalar_torch = light_term_from_levels(0.0, block_level, midnight);
        assert!(
            (scalar_torch - 0.481_948_0).abs() < 1e-4,
            "control: scalar torch value drifted: {scalar_torch}"
        );
        let scalar_q = 1.0; // a scalar has no red/blue channels to ratio.
        let vanilla_q = torch_midnight[0] / torch_midnight[2];
        assert!(
            (vanilla_q - scalar_q).abs() > 0.5,
            "the scalar model's implicit q=1.0 must clearly miss vanilla's warm \
             torch ratio {vanilla_q}"
        );
    }

    /// [`rgb24_to_channels`] is a bare `byte / 255` per channel, no
    /// linearisation — vanilla's packed-24-bit-to-vector unpack. Hand-derived, not
    /// taken from the function.
    #[test]
    fn rgb24_to_channels_is_a_bare_byte_over_255_per_channel() {
        assert_eq!(
            rgb24_to_channels(0x302821),
            [48.0 / 255.0, 40.0 / 255.0, 33.0 / 255.0]
        );
    }

    /// **The bug this module's per-dimension `ambient` parameter exists to
    /// fix.** A cave in the Nether must read vanilla's real ambient floor —
    /// derived here from `0x302821` by hand, the same `notGamma` mix
    /// [`light_color_from_levels`] applies to `AMBIENT_LIGHT_COLOR` — and that
    /// floor must be **measurably brighter** than the overworld's `#0a0a0a`
    /// floor, not the same number. Passing [`OVERWORLD_AMBIENT_LIGHT`] for a
    /// Nether cell (the bug: treating [`AMBIENT_LIGHT`] as a universal
    /// constant) is the wrong hypothesis this test discriminates against.
    #[test]
    fn nether_ambient_floor_reads_meaningfully_brighter_than_the_overworlds() {
        // `0x302821` per channel, `notGamma`-mixed at `BRIGHTNESS_FACTOR` —
        // written out by hand rather than calling `light_color_from_levels`.
        // `notGamma` is **not** a per-channel operation on a non-grey triple:
        // it scales every channel by the *maximum* channel's response
        // (`not_gamma_vec3`'s doc), which is why this cannot reuse
        // `an_unlit_surface_lands_on_vanillas_ambient_floor`'s per-channel
        // arithmetic — that derivation is only exact when the three channels
        // already agree, which the Nether's `0x302821` does not.
        let raw = [48.0_f32 / 255.0, 40.0 / 255.0, 33.0 / 255.0];
        let max_component = raw[0].max(raw[1]).max(raw[2]);
        let inv = 1.0 - max_component;
        let max_scaled = 1.0 - inv * inv * inv * inv;
        let ratio = max_scaled / max_component;
        let expected_nether = [
            raw[0] + (raw[0] * ratio - raw[0]) * 0.5,
            raw[1] + (raw[1] * ratio - raw[1]) * 0.5,
            raw[2] + (raw[2] * ratio - raw[2]) * 0.5,
        ];
        assert!(
            (expected_nether[0] - 0.377_002).abs() < 1e-5
                && (expected_nether[1] - 0.314_169).abs() < 1e-5
                && (expected_nether[2] - 0.259_189).abs() < 1e-5,
            "hand-derived Nether floor drifted: {expected_nether:?}"
        );

        let nether_ambient = rgb24_to_channels(0x302821);
        // Sky level 0, block level 0: nothing but the ambient floor
        // contributes, at any tick.
        let ours = light_color_from_levels(0.0, 0.0, 0.24, nether_ambient);
        for (i, (got, want)) in ours.iter().zip(expected_nether.iter()).enumerate() {
            assert!(
                (got - want).abs() < 1e-5,
                "channel {i}: got {got}, vanilla's Nether ambient floor is {want}"
            );
        }

        // The wrong hypothesis: reusing the overworld's grey constant for a
        // Nether cell, which is the exact bug report ("the entire Nether
        // seems very dark"). Every channel must clearly miss it.
        let wrong = light_color_from_levels(0.0, 0.0, 0.24, OVERWORLD_AMBIENT_LIGHT);
        for (i, (right, wrong)) in ours.iter().zip(wrong.iter()).enumerate() {
            assert!(
                (right - wrong).abs() > 0.15,
                "channel {i}: the Nether's real floor {right} must clearly miss the \
                 overworld-constant hypothesis {wrong}, or this cannot discriminate \
                 the bug from the fix"
            );
            assert!(
                *right > *wrong,
                "channel {i}: the Nether's real floor must be *brighter* than the \
                 overworld's, not darker — got right={right} wrong={wrong}"
            );
        }
    }

    /// [`not_gamma_vec3`] must collapse to [`not_gamma`] when all three
    /// channels already agree (the daylight/cave case), and must not divide
    /// by zero at pure black.
    #[test]
    fn not_gamma_vec3_matches_the_grey_specialisation_and_guards_black() {
        for c in [0.0_f32, 0.093_545_4, 0.279_216, 1.0] {
            let grey = not_gamma([c, c, c][0]);
            let vec3 = not_gamma_vec3([c, c, c]);
            for v in vec3 {
                assert!((v - grey).abs() < 1e-6, "c={c}: {vec3:?} vs grey {grey}");
            }
        }
        assert_eq!(not_gamma_vec3([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
    }
}
