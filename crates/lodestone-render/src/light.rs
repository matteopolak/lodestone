//! Vanilla's lightmap value, in Rust — the single authority every shader in this
//! crate duplicates and every gate re-derives independently.
//!
//! # What it is
//!
//! Vanilla builds a 16×16 `RGBA8_UNORM` texture (`Lightmap`) indexed by
//! `(block_level, sky_level)` and multiplies each terrain/entity/particle vertex
//! colour by the texel it samples (`block.vsh`: `vertexColor = Color *
//! sample_lightmap(Sampler2, UV2)`). [`light_term`] is that texel, as a scalar.
//!
//! # How it works
//!
//! Straight from `assets/minecraft/shaders/core/lightmap.fsh` in the real 26.2
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
//! * [`brightness`] is `get_brightness` — and `Lightmap.getBrightness` is the
//!   same expression with the dimension's `ambientLight` lerped in, which is
//!   `0.0` in the overworld.
//! * **The curve is applied to the raw level, and `SkyFactor` multiplies the
//!   result.** That order matters and it is the opposite of what issue #386's
//!   table assumed — see the divergence note at the bottom of this doc.
//!   `SkyFactor` is `EnvironmentAttributes.SKY_LIGHT_FACTOR`, i.e. exactly
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
//!   widens a vertex output in three shaders. Issue #383's third divergence,
//!   still open.
//!
//! # How to change it
//!
//! The three WGSL copies (`shaders/model.wgsl`, `shaders/entity.wgsl`,
//! `shaders/fluid.wgsl`) duplicate these functions verbatim — WGSL has no
//! `#include` and this crate's convention is to duplicate small helpers (see
//! `srgb_to_linear`, already three-way duplicated). **Change all three plus this
//! file together**, and note that `shaders/block.wgsl` is the demo-only packed
//! path tracked separately by #400.
//!
//! The gates must *not* call into this module: per `CLAUDE.md`, an expected value
//! has to originate outside the code under test, so each one writes
//! `level / (4 - 3 * level)` out again. A gate that imported [`brightness`] would
//! be the `decode(encode(x))` trap.
//!
//! # Configuration
//!
//! [`BRIGHTNESS_FACTOR`] is vanilla's `Options.gamma` **default** (`0.5`,
//! `Options.java:900`), hardcoded because there is no brightness setting in this
//! client yet. Wiring one means threading it into the three shaders' uniforms;
//! `0.0` reproduces vanilla's "Moody" and `1.0` its "Bright".
//!
//! # The divergence from issue #386's table
//!
//! #386 specified `l / (4 - 3l)` applied to `l = max(sky * sky_darken, block)`,
//! predicting `0.0732` at midnight against our old ramp's `0.3920` — "5.36× too
//! bright". Both halves of that are off, and in opposite directions:
//!
//! | midnight, sky 15 | light term |
//! |---|---|
//! | old ramp `0.2 + 0.8·l` | 0.3920 |
//! | #386's table (`curve` **after** `sky_darken`, no `notGamma`) | 0.0732 |
//! | vanilla per `lightmap.fsh` (`curve` **before**, with `notGamma`) | **0.4532** |
//!
//! So night was never 5.36× too bright — at full skylight it was ~14% too
//! *dark*. The ramp is still wrong, and wrong in the way #383 measured, but the
//! error lives in the **middle** of the range, not at midnight: at sky level 12
//! in daylight the old ramp gives `0.840` where vanilla gives `0.719`, and at
//! sky level 4 it gives `0.413` where vanilla gives `0.189`. And the `0.2` floor
//! really is a hard mechanism, exactly as #386 says: an unlit surface read
//! `0.200` where vanilla reads [`AMBIENT_LIGHT`]'s `0.0935`.
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
//!   (`Timelines.java:72`) keyframed `-1` (white) at ticks 730 and 11270 and
//!   `NIGHT_SKY_LIGHT_COLOR` at 13140 and 22860 — and that constant is
//!   `colorFromFloat(1.0, 0.48, 0.48, 1.0)`, i.e. **blue**: red and green fall to
//!   48% while blue holds at 100%. So vanilla's night light is not merely dimmer
//!   than day, it is a different *hue*, which is why the grey specialisation in
//!   [`not_gamma`] is a daylight-only convenience. This is the remaining reason
//!   the light term has to become a `vec3`, alongside the warm
//!   `BLOCK_LIGHT_TINT` of `(1.000, 0.847, 0.549)`.

/// Vanilla's `Options.gamma` default, which `lightmap.fsh` consumes as
/// `BrightnessFactor` to mix [`not_gamma`] in. See this module's "Configuration".
pub const BRIGHTNESS_FACTOR: f32 = 0.5;

/// The overworld's `EnvironmentAttributes.AMBIENT_LIGHT_COLOR`, as a grey scalar.
///
/// `DimensionTypes.java:36` sets it to `-16119286`, which is `0xFF0A0A0A` — grey
/// `10/255`, **not** black. `lightmap.fsh` seeds its accumulator with
/// `max(AmbientColor, nightVisionColor)` before adding either light half, so a
/// fully unlit surface in vanilla is not pure black: it reads `0.0935` once
/// [`not_gamma`] is mixed in. Dropping this term is what made caves render
/// absolutely black.
///
/// It is grey in the overworld, which is the only reason the light term can stay
/// a scalar. The Nether's `0x302821` and the End's `0x3F473F` are *not* grey and
/// belong to the same per-dimension colour pass as the block-light tint.
pub const AMBIENT_LIGHT: f32 = 10.0 / 255.0;

/// Vanilla's `get_brightness`: one lightmap axis' response to a `0.0..=1.0` light
/// level (the wire nibble over 15).
///
/// `lightmap.fsh`'s `level / (4.0 - 3.0 * level)`, equal to
/// `Lightmap.getBrightness` with the overworld's `ambientLight` of `0.0`. `0.0`
/// maps to `0.0` and `1.0` to `1.0`; in between it is strongly concave — half
/// light is a fifth of the brightness, which is the whole reason a linear ramp
/// looked so wrong in the middle of the range.
#[must_use]
pub fn brightness(level: f32) -> f32 {
    level / (4.0 - 3.0 * level)
}

/// Vanilla's `notGamma`, specialised to a grey value.
///
/// `lightmap.fsh` scales an RGB triple by `maxScaled / maxComponent` where
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

/// The lightmap value for a *combined* brightness, i.e. `lightmap.fsh`'s final
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
/// `sky_darken` of `EnvironmentAttributes.SKY_LIGHT_FACTOR`.
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

    /// Issue #386's three named proof points, on the bare curve. These are
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

    /// The midnight number this whole change turns on, with issue #386's own
    /// table as the third hypothesis. All three are computed here from constants
    /// that originate in `lightmap.fsh` and `Options.java`, not from the shader.
    #[test]
    fn midnight_lands_on_vanillas_value_and_not_on_either_wrong_one() {
        let midnight = 0.24_f32;
        let ours = light_term(0xF0, midnight);

        // Hypothesis A, the retired ramp: `0.2 + 0.8 * (1.0 * 0.24)`.
        let old = retired_linear_ramp(midnight);
        // Hypothesis B, #386's table: the curve applied *after* `sky_darken`,
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
             {old}, not issue #386's {issue_table}, and not the ambient-free \
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
}
