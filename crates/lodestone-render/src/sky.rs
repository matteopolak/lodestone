//! Sky-dome geometry and time-of-day math: the sky disc, sun/moon billboards,
//! star field and cloud plane. GPU-free and headlessly testable by design (see
//! [`crate::sky_pipeline`] for the GPU-owning half).
//!
//! # This subsystem did not exist before this change
//!
//! A tree-wide grep for sky/celestial/star/moon/cloud rendering before writing
//! any of this turned up only *sky light* (`world.rs`'s `sky_light` nibble) and
//! *fog* (`fog.rs`'s distance-fade colour) — real, wired subsystems that happen
//! to share the word "sky", not a dormant celestial renderer. There is no
//! island to reconnect here: the night sky is a genuinely new subsystem, built
//! from nothing.
//!
//! # Reuse, not duplication, of the day clock
//!
//! Every time-varying function here takes the **same** `time_of_day: i64` the
//! rest of the renderer already reads from `WorldTime`
//! (`docs/time-of-day-lighting.md`) — there is deliberately no second clock,
//! no tick counter, and no wall-clock read anywhere in this module.
//!
//! # One formula is duplicated from `entity.rs`, not imported — and it is now one, not two
//!
//! [`celestial_angle_for_time_of_day`] is the *same* vanilla `celestialAngle`
//! math [`crate::entity::sky_darken_for_time_of_day`] computes as a private
//! intermediate — but `entity.rs` was a held file outside this change's scope,
//! so this is a second, independently-written copy rather than an import. If
//! either copy changes, the other must change with it, or the sun's screen
//! position and the lightmap's darken factor will visibly disagree about what
//! time it is.
//!
//! This heading used to say **two** formulas, naming a private `sky_darken_shape`
//! alongside it. That function was deleted (see the section below) and the count
//! was not updated with it, leaving a rustdoc link to a nonexistent item three
//! hundred lines away — invisible to every `cargo check`, since none of them
//! compile documentation.
//!
//! # Which formulas here are timeline-exact and which are still 1.21's (#49)
//!
//! 26.2 replaced the classic cosine `celestialAngle`/`getSkyDarken` with
//! keyframed `EnvironmentAttributes` tracks on `Timelines.OVERWORLD_DAY`
//! (`SUN_ANGLE`, `MOON_ANGLE`, `STAR_ANGLE`, `STAR_BRIGHTNESS`, `SKY_COLOR`,
//! `FOG_COLOR`, `SUNRISE_SUNSET_COLOR`, `MOON_PHASE` — see
//! `.cache/mc/26.2/client-src/net/minecraft/client/renderer/SkyRenderer.java`
//! `extractRenderState`). The split as of #96:
//!
//! * **Timeline-exact, gated against a JVM dump of the real sampler**
//!   ([`crate::sky_pipeline`]'s consumers of them included):
//!   [`sunrise_sunset_color_for_time_of_day`],
//!   [`sky_color_multiplier_for_time_of_day`],
//!   [`fog_color_multiplier_for_time_of_day`], and therefore
//!   [`sky_color_for_time_of_day`] / [`fog_color_for_time_of_day`], which are
//!   thin gamma-space compositions of those multipliers. See
//!   `crates/lodestone-render/tests/sunrise_sunset_timeline.rs`.
//! * **Still the classic 1.21 cosine, i.e. still #49**:
//!   [`celestial_angle_for_time_of_day`] (drives the sun/moon/star *positions*)
//!   and [`star_brightness_for_time_of_day`]. Both match their plateaus and
//!   diverge on the ramp shape.
//!
//! An earlier version of this module doc claimed the classic formulas were
//! "the same ones `entity.rs`'s validated port already uses for `sky_darken`".
//! That was true when written and is **false now**:
//! `entity::sky_darken_for_time_of_day` is a timeline port validated at all
//! 24000 ticks (`tests/sky_light_factor_timeline.rs`), so the private
//! `sky_darken_shape` cosine this module used to blend its sky colour with had
//! silently become a *divergent* second opinion rather than a duplicate of a
//! validated one. It is deleted; [`sky_color_for_time_of_day`] now reads the
//! real `SKY_COLOR` track. That is `CLAUDE.md` rule 2 in miniature — the stale
//! claim looked entirely correct on inspection.

use glam::{Mat4, Vec3};

use crate::fog::multiply_gamma;

// ---------------------------------------------------------------------------
// Time-of-day math
// ---------------------------------------------------------------------------

/// The fraction of a full day/night rotation completed, in `[0, 1)` — vanilla's
/// `celestialAngle`. `0.0` is noon, `0.5` is midnight (verified against the
/// same two anchor points `entity.rs`'s `sky_darken_for_time_of_day` test
/// asserts: this function returns `0.0` at `time_of_day = 6_000` and `0.5` at
/// `time_of_day = 18_000`).
#[must_use]
pub fn celestial_angle_for_time_of_day(time_of_day: i64) -> f32 {
    let day = time_of_day.rem_euclid(24_000) as f64 / 24_000.0;
    let frac = (day - 0.25).rem_euclid(1.0);
    let eased = 0.5 - (frac * std::f64::consts::PI).cos() / 2.0;
    ((frac * 2.0 + eased) / 3.0) as f32
}

// ---------------------------------------------------------------------------
// Timeline colour tracks (26.2 `data/minecraft/timeline/day.json`)
// ---------------------------------------------------------------------------

/// One full rotation of `Timelines.OVERWORLD_DAY`, in ticks (`day.json`'s
/// `period_ticks`).
pub const DAY_PERIOD_TICKS: i64 = 24_000;

/// `minecraft:visual/sunrise_sunset_color`, verbatim from
/// `.cache/mc/26.2/src/data/minecraft/timeline/day.json`, as `(tick, ARGB)`.
///
/// The values are **ARGB**, not RGBA: `#feda6333` is alpha `0xfe`, red `0xda`,
/// green `0x63`, blue `0x33` — a warm sunset orange at near-full opacity, not a
/// green at 20% alpha. Reading the channel order off the hex string the wrong
/// way round produces a plausible-looking but completely wrong band; the
/// authority is `EnvironmentAttributes.SUNRISE_SUNSET_COLOR`'s declared
/// `AttributeTypes.ARGB_COLOR` (`EnvironmentAttributes.java:46`), not the
/// string's appearance. Blue is a constant `0x33` across every keyframe and
/// alpha is what animates, from `0x00` through the middle of the day to `0xfe`
/// at peak sunset (tick 12732).
///
/// The track declares no `modifier`, so it takes
/// `AttributeModifier.override()` (`AttributeTrack.createCodec`'s
/// `optionalFieldOf("modifier", …)` default) — the sampled keyframe value *is*
/// the final colour, with no base to combine with.
const SUNRISE_SUNSET_TRACK: [(i32, u32); 32] = [
    (71, 0x5f_ef_a3_33),
    (310, 0x29_f5_ba_33),
    (565, 0x06_fb_d4_33),
    (730, 0x00_ff_e5_33),
    (11_270, 0x00_ff_e5_33),
    (11_397, 0x04_fc_d8_33),
    (11_522, 0x0f_f9_cb_33),
    (11_690, 0x29_f5_ba_33),
    (11_929, 0x5f_ef_a3_33),
    (12_243, 0xb1_e7_87_33),
    (12_358, 0xcc_e4_7e_33),
    (12_512, 0xe9_e0_72_33),
    (12_613, 0xf6_dd_6b_33),
    (12_732, 0xfe_da_63_33),
    (12_841, 0xfe_d7_5c_33),
    (13_035, 0xec_d2_51_33),
    (13_252, 0xc1_cc_47_33),
    (13_775, 0x36_be_37_33),
    (13_888, 0x1f_bb_35_33),
    (14_039, 0x09_b7_33_33),
    (14_192, 0x00_b3_33_33),
    (21_807, 0x00_b2_33_33),
    (21_961, 0x09_b7_33_33),
    (22_112, 0x1f_bb_35_33),
    (22_225, 0x36_be_37_33),
    (22_748, 0xc1_cc_47_33),
    (22_965, 0xec_d2_51_33),
    (23_159, 0xfe_d7_5c_33),
    (23_272, 0xfe_da_63_33),
    (23_488, 0xe9_e0_72_33),
    (23_642, 0xcc_e4_7e_33),
    (23_757, 0xb1_e7_87_33),
];

/// `minecraft:visual/sky_color`, from the same `day.json`. `"modifier":
/// "multiply"` (`ColorModifier.MULTIPLY_RGB = ARGB::multiply`), so these are a
/// per-tick **multiplier** over whatever base sky colour applies — a biome's
/// own `minecraft:visual/sky_color` in vanilla, the renderer's existing sky
/// constant here.
///
/// White through the whole day, pure black across the night: vanilla's night
/// sky *disc* is genuinely `#000000`, and the dark-blue night sky people
/// remember is [`FOG_COLOR_TRACK`] showing through at the horizon via the
/// disc's own fog gradient (see [`SKY_FOG_END_DISTANCE`]). Alpha is `0xff`
/// throughout because `AttributeTypes.RGB_COLOR`'s codec parses a 6-digit
/// `#RRGGBB` as opaque.
const SKY_COLOR_TRACK: [(i32, u32); 4] = [
    (133, 0xff_ff_ff_ff),
    (11_867, 0xff_ff_ff_ff),
    (13_670, 0xff_00_00_00),
    (22_330, 0xff_00_00_00),
];

/// `minecraft:visual/fog_color`, same file, same `multiply` modifier as
/// [`SKY_COLOR_TRACK`]. Unlike the sky's, this one does **not** reach black:
/// `#0c0c16` at dusk and `#161616` at deep night, which is why the night
/// horizon reads faintly blue-grey rather than as a hard edge against a black
/// zenith.
const FOG_COLOR_TRACK: [(i32, u32); 4] = [
    (133, 0xff_ff_ff_ff),
    (11_867, 0xff_ff_ff_ff),
    (13_670, 0xff_0c_0c_16),
    (22_330, 0xff_16_16_16),
];

/// `minecraft:visual/cloud_color`, same file, same `multiply` modifier. Stored
/// in `day.json` as raw signed ARGB ints rather than hex strings (`-1` day,
/// `-15132378` night); `-15132378 as u32` is `0xff191926`, a dark blue-grey.
///
/// This track is only *incidentally* part of #96, and it is here because the
/// sky change would otherwise have caused a regression: the cloud tint used to
/// be `sky_color * 0.9`, and now that [`SKY_COLOR_TRACK`] correctly reaches
/// `#000000` at night, that expression makes night clouds exactly invisible.
/// Vanilla keeps them visible with their own non-black track, so the tint reads
/// this instead of the sky.
const CLOUD_COLOR_TRACK: [(i32, u32); 4] = [
    (133, 0xff_ff_ff_ff),
    (11_867, 0xff_ff_ff_ff),
    (13_670, 0xff_19_19_26),
    (22_330, 0xff_19_19_26),
];

/// `Mth.lerpInt` (`.cache/mc/26.2/src/net/minecraft/util/Mth.java:541`):
/// `p0 + floor(alpha * (p1 - p0))`. The `floor` is load-bearing — a `round`
/// here is off by one byte on roughly half of all ticks, which the JVM gate
/// catches immediately.
pub(crate) fn lerp_int(alpha: f32, p0: i32, p1: i32) -> i32 {
    p0 + (alpha * (p1 - p0) as f32).floor() as i32
}

/// `ARGB.srgbLerp` (`ARGB.java:155`): a per-channel [`lerp_int`] over the raw
/// **bytes**, i.e. interpolation in gamma space, not linear light. Alpha is
/// interpolated exactly like the colour channels.
fn srgb_lerp(alpha: f32, from: u32, to: u32) -> u32 {
    let ch = |shift: u32| {
        let a = ((from >> shift) & 0xFF) as i32;
        let b = ((to >> shift) & 0xFF) as i32;
        (lerp_int(alpha, a, b).clamp(0, 255) as u32) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// Samples a periodic ARGB keyframe track at `time_of_day`, reproducing
/// `KeyframeTrackSampler` (`.cache/mc/26.2/src/net/minecraft/util/KeyframeTrackSampler.java`)
/// for a **linear**-eased track.
///
/// Three details of that class are easy to lose and all three are checked by
/// the JVM gate:
///
/// * `bakeSegments` prepends a wraparound segment
///   `(last, last.ticks - period) -> (first, first.ticks)` and appends
///   `(last, last.ticks) -> (first, first.ticks + period)`. So a tick before the
///   *first* keyframe is not clamped to it — it is on the ramp coming round
///   from the last keyframe through the tick-0 seam.
/// * `getSegmentAt` picks the first segment with `t < segment.toTicks`, a
///   strict `<`, so a tick landing exactly on a keyframe belongs to the segment
///   *ending* there and `sample`'s `t >= toTicks` branch returns that
///   keyframe's value exactly.
/// * the easing is **linear**. `KeyframeTrack.Builder` defaults to
///   `EasingType.LINEAR` and none of the three tracks in this module declares
///   an `ease` in `day.json` — only the neighbouring `sun_angle`/`moon_angle`/
///   `star_angle` tracks opt into a cubic bezier. Issue #49's own text once
///   said these were bezier-eased; that was a transcription error.
fn sample_argb_track(track: &[(i32, u32)], time_of_day: i64) -> u32 {
    debug_assert!(track.len() >= 2, "a periodic track needs at least two keyframes");
    let period = DAY_PERIOD_TICKS as i32;
    let tick = time_of_day.rem_euclid(DAY_PERIOD_TICKS) as i32;
    let &(first_ticks, first_value) = track.first().expect("non-empty track");
    let &(last_ticks, last_value) = track.last().expect("non-empty track");

    // Segment selection, in `getSegmentAt`'s own order: the leading wrap
    // segment first, then each consecutive pair, then the trailing wrap
    // segment as the fallback.
    let (from_ticks, from_value, to_ticks, to_value) = if tick < first_ticks {
        (last_ticks - period, last_value, first_ticks, first_value)
    } else {
        track
            .windows(2)
            .find(|w| tick < w[1].0)
            .map_or((last_ticks, last_value, first_ticks + period, first_value), |w| {
                (w[0].0, w[0].1, w[1].0, w[1].1)
            })
    };

    if tick <= from_ticks {
        return from_value;
    }
    if tick >= to_ticks {
        return to_value;
    }
    let alpha = (tick - from_ticks) as f32 / (to_ticks - from_ticks) as f32;
    srgb_lerp(alpha, from_value, to_value)
}

/// Vanilla's `EnvironmentAttributes.SUNRISE_SUNSET_COLOR` at `time_of_day`, as
/// `[r, g, b, a]` sRGB bytes (reordered from the track's packed ARGB for a
/// Rust-natural call site).
///
/// `a == 0` for the whole middle of the day and the deep middle of the night —
/// vanilla skips the draw entirely when `alpha <= 0.001`
/// (`SkyRenderer.renderSunriseAndSunset`), and so does
/// [`crate::sky_pipeline::SkyRenderer::render`]. Measured from the JVM dump,
/// alpha is non-zero only on ticks `0..=702`, `11302..=14175` and
/// `21825..=23999`: one dusk band and one dawn band, the dawn one wrapping the
/// tick-0 seam.
#[must_use]
pub fn sunrise_sunset_color_for_time_of_day(time_of_day: i64) -> [u8; 4] {
    let argb = sample_argb_track(&SUNRISE_SUNSET_TRACK, time_of_day);
    [
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
        ((argb >> 24) & 0xFF) as u8,
    ]
}

/// The `SKY_COLOR` track's per-tick multiplier, as sRGB bytes — white at noon,
/// black at night. Multiply a base sky colour by this in **gamma** space
/// ([`crate::fog::multiply_gamma`]), which is what `ARGB.multiply` does.
#[must_use]
pub fn sky_color_multiplier_for_time_of_day(time_of_day: i64) -> [u8; 3] {
    let argb = sample_argb_track(&SKY_COLOR_TRACK, time_of_day);
    [
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
    ]
}

/// The `FOG_COLOR` track's per-tick multiplier, as sRGB bytes. See
/// [`sky_color_multiplier_for_time_of_day`]; this one bottoms out at
/// `#0c0c16`/`#161616` rather than black.
#[must_use]
pub fn fog_color_multiplier_for_time_of_day(time_of_day: i64) -> [u8; 3] {
    let argb = sample_argb_track(&FOG_COLOR_TRACK, time_of_day);
    [
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
    ]
}

/// The `CLOUD_COLOR` track's per-tick multiplier, as sRGB bytes. See
/// [`CLOUD_COLOR_TRACK`] on why the cloud tint has its own track rather than
/// reusing the sky's.
#[must_use]
pub fn cloud_color_multiplier_for_time_of_day(time_of_day: i64) -> [u8; 3] {
    let argb = sample_argb_track(&CLOUD_COLOR_TRACK, time_of_day);
    [
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
    ]
}

/// The cloud tint at `time_of_day`: a **linear** `day_cloud` base multiplied by
/// the real `CLOUD_COLOR` track in gamma space.
#[must_use]
pub fn cloud_color_for_time_of_day(time_of_day: i64, day_cloud: [f32; 3]) -> [f32; 3] {
    let m = cloud_color_multiplier_for_time_of_day(time_of_day);
    multiply_gamma(day_cloud, m.map(|c| f32::from(c) / 255.0))
}

/// The sky-dome colour at `time_of_day`: `day_color` (a **linear** RGB base —
/// pass the renderer's clear/sky colour, or a biome's `visual/sky_color` once
/// one is reachable) multiplied by the real `SKY_COLOR` track in gamma space.
///
/// This replaced a hand-rolled blend toward a fixed dark-navy `NIGHT` constant.
/// Two things changed measurably: night is now exactly black rather than
/// `[0.006, 0.008, 0.02]` (which is what vanilla's `#000000` keyframe says),
/// and the dusk/dawn ramp follows the track's `11867 -> 13670` /
/// `22330 -> 133` linear segments rather than a cosine. The visible night sky
/// is *not* black as a result: the horizon end of the disc's gradient is
/// [`fog_color_for_time_of_day`], not this.
#[must_use]
pub fn sky_color_for_time_of_day(time_of_day: i64, day_color: [f32; 3]) -> [f32; 3] {
    let m = sky_color_multiplier_for_time_of_day(time_of_day);
    multiply_gamma(day_color, m.map(|c| f32::from(c) / 255.0))
}

/// The fog colour at `time_of_day`: a **linear** `day_fog` base multiplied by
/// the real `FOG_COLOR` track in gamma space, exactly as
/// [`sky_color_for_time_of_day`] does for the sky.
#[must_use]
pub fn fog_color_for_time_of_day(time_of_day: i64, day_fog: [f32; 3]) -> [f32; 3] {
    let m = fog_color_multiplier_for_time_of_day(time_of_day);
    multiply_gamma(day_fog, m.map(|c| f32::from(c) / 255.0))
}

/// Vanilla's legacy `getStarBrightness`: `0.0` for most of the day, ramping up
/// around dusk to a `0.5` plateau at night. Ported literally rather than
/// re-derived from the sky-darken curve's different constants, since it is
/// vanilla's own distinct formula — and note that curve is now the timeline port
/// in [`crate::entity::sky_darken_for_time_of_day`], not the cosine this
/// sentence used to link to.
#[must_use]
pub fn star_brightness_for_time_of_day(time_of_day: i64) -> f32 {
    let angle = celestial_angle_for_time_of_day(time_of_day);
    let mut f = 1.0 - ((angle * std::f32::consts::TAU).cos() * 2.0 + 0.25);
    f = f.clamp(0.0, 1.0);
    f * f * 0.5
}

/// The active moon phase, `0..8`, in [`lodestone_assets::MOON_PHASE_NAMES`]
/// order. Vanilla's `MoonPhase.startTick() == index() * 24000`
/// (`.cache/mc/26.2/client-src/net/minecraft/world/level/MoonPhase.java`) fixes
/// the mapping: the phase active on world-day `d` (`d = time_of_day / 24000`)
/// is enum index `d % 8`. This is a per-day integer cycle, not a continuous
/// keyframe track, so unlike the ramp-shaped formulas above it is not subject
/// to the #49 divergence.
#[must_use]
pub fn moon_phase_index_for_time_of_day(time_of_day: i64) -> u8 {
    time_of_day.div_euclid(24_000).rem_euclid(8) as u8
}

// ---------------------------------------------------------------------------
// Sky disc
// ---------------------------------------------------------------------------

/// Sky-disc radius in blocks (vanilla `SkyRenderer.SKY_DISC_RADIUS`).
pub const SKY_DISC_RADIUS: f32 = 512.0;

/// The **upper bound** on the distance at which the sky disc has faded entirely
/// into the fog colour, in blocks — `EnvironmentAttributes.SKY_FOG_END_DISTANCE`'s
/// registered default (`512.0`, `EnvironmentAttributes.java:25-28`).
///
/// # This is a ceiling, not the value. Use [`sky_fog_end_for_render_distance`]
///
/// It was a plain constant here until issue #399, and that was wrong for every
/// render distance below 32 chunks: vanilla clamps the attribute to the render
/// distance before the shader ever sees it —
///
/// ```java
/// fog.skyEnd = Math.min(renderDistance, camera.attributeProbe().getValue(EnvironmentAttributes.SKY_FOG_END_DISTANCE, partialTicks));
/// ```
///
/// (`AtmosphericFogEnvironment.java:73`, where `renderDistance` is the
/// `renderDistanceInBlocks = renderDistanceInChunks * 16` that
/// `FogRenderer.setupFog` passes in at `FogRenderer.java:185`/`:193` — *blocks*,
/// not chunks; the `/ 16.0F` at `AtmosphericFogEnvironment.java:44` is a
/// different, chunk-space use of the same attribute for the fog/sky colour mix
/// and is not this.)
///
/// So this constant is the value only at render distance 32 and above. At the
/// common default of 8 chunks vanilla's gradient completes at **128** blocks,
/// and shipping `512` there stretched the whole horizon ramp roughly 4x too far.
/// The clamp is [`sky_fog_end_for_render_distance`]; the value travels to the
/// shader on [`crate::SkyFrame::sky_fog_end`].
///
/// # This is where vanilla's horizon-to-zenith gradient comes from
///
/// The gradient is not baked into the disc's vertex colours; the disc is drawn a
/// single flat colour and then *fogged*. `assets/minecraft/shaders/core/sky.fsh`
/// is one line:
///
/// ```glsl
/// fragColor = apply_fog(ColorModulator, sphericalVertexDistance, cylindricalVertexDistance,
///                       0.0, FogSkyEnd, FogSkyEnd, FogSkyEnd, FogColor);
/// ```
///
/// so with `include/fog.glsl`'s definitions the disc's colour is
/// `mix(sky_color, fog_color, clamp(dist / sky_end, 0, 1))` where `dist` is the
/// camera-relative distance of the point being shaded and `sky_end` is
/// [`sky_fog_end_for_render_distance`]. The disc sits at `y = 16` with radius
/// `512`, so at render distance 32 (where `sky_end` is this constant) its centre
/// is at distance `16` (fog factor `0.031`, essentially pure sky colour) and its
/// rim at `512.25` (factor `1.0`, pure fog colour). That radial ramp *is* the
/// gradient — and shortening `sky_end` does not change either endpoint's
/// *colour*, only how few degrees of elevation the ramp is spread over: at
/// render distance 8 the disc is already fully fog-coloured everywhere outside
/// 128 blocks, i.e. below `asin(16/128) = 7.2` degrees of elevation rather than
/// vanilla-at-RD-32's `1.79`.
///
/// The second `apply_fog` term is provably dead for this geometry and is not
/// implemented: `total_fog_value` takes the `max` of the spherical ramp and
/// `linear_fog_value(cylindrical, SkyEnd, SkyEnd)` — a step at `SkyEnd` on
/// `max(|xz|, |y|)`. Since `sqrt(x²+y²+z²) >= max(sqrt(x²+z²), |y|)` for every
/// point, any fragment where the step fires already has spherical distance
/// `>= SkyEnd`, where the first term is already `1.0`. The `max` can therefore
/// never raise the result.
///
/// # Why ours is smoother than vanilla's, deliberately
///
/// `RenderPipelines.SKY` computes `sphericalVertexDistance` in the **vertex**
/// shader (`sky.vsh`) over a 10-vertex, 8-triangle fan whose triangles are
/// hundreds of blocks across, so vanilla interpolates the fog *factor*
/// barycentrically and the ramp shows as flat-shaded wedges — the banding in
/// issue #96's title. `SKY_DISC_WGSL` interpolates the camera-relative
/// *position* instead and takes `length()` per fragment, which costs one
/// `sqrt` per pixel and removes the banding entirely. It is a closer
/// approximation of the radial gradient vanilla is describing, not a departure
/// from it.
pub const SKY_FOG_END_DISTANCE: f32 = 512.0;

/// Where the sky disc's gradient actually ends, in blocks, for a render distance
/// of `render_distance_chunks`: vanilla's
/// `Math.min(renderDistanceInBlocks, SKY_FOG_END_DISTANCE)`
/// (`AtmosphericFogEnvironment.java:73`).
///
/// Worked values, so the curve is legible without running it: RD 2 → 32; RD 4 →
/// 64; RD 8 → **128**; RD 16 → 256; RD 32 → 512; RD 48 → 512 (clamped). The
/// clamp only binds at and above 32 chunks, which is why the pre-#399 constant
/// `512` looked right to whoever last checked it on a long view distance and was
/// 4x too long at the client default.
///
/// A render distance of 0 chunks yields `0.0`, and a zero fog end is a division
/// by zero in the ramp; `sky_disc.wgsl` floors the divisor rather than relying on
/// no caller ever passing it, because the failure mode is a `NaN` whose `clamp`
/// result is not specified by WGSL — the disc would go an arbitrary colour rather
/// than the fully-fogged one that is the correct limit.
#[must_use]
pub fn sky_fog_end_for_render_distance(render_distance_chunks: u32) -> f32 {
    sky_fog_end_for_render_distance_blocks(render_distance_chunks as f32 * 16.0)
}

/// [`sky_fog_end_for_render_distance`] for a view distance already expressed in
/// **blocks** — vanilla's own parameter, since `FogRenderer.setupFog` converts to
/// blocks (`renderDistanceInChunks * 16`, `FogRenderer.java:185`) before handing
/// the value to the fog environment.
///
/// Negative inputs are floored at zero; nothing upstream produces one, but the
/// alternative is propagating a negative divisor into the shader.
#[must_use]
pub fn sky_fog_end_for_render_distance_blocks(render_distance_blocks: f32) -> f32 {
    render_distance_blocks.clamp(0.0, SKY_FOG_END_DISTANCE)
}

/// Camera-relative sky-disc triangle-fan positions: the centre plus 9
/// perimeter points across `-180..=180` degrees in 45-degree steps (vanilla
/// `SkyRenderer.buildSkyDisc`, `SKY_VERTICES = 10`). Pass a positive `y` for
/// the overhead sky disc, negative for the below-horizon "dark disc".
#[must_use]
pub fn sky_disc_positions(y: f32) -> [[f32; 3]; 10] {
    let x_radius = y.signum() * SKY_DISC_RADIUS;
    let mut out = [[0.0f32; 3]; 10];
    out[0] = [0.0, y, 0.0];
    for (i, deg) in (-180..=180i32).step_by(45).enumerate() {
        let rad = (deg as f32).to_radians();
        out[i + 1] = [x_radius * rad.cos(), y, SKY_DISC_RADIUS * rad.sin()];
    }
    out
}

/// Triangle-fan indices for [`sky_disc_positions`]'s 10 vertices (8 triangles,
/// all sharing vertex `0`).
#[must_use]
pub fn sky_disc_indices() -> Vec<u32> {
    let mut idx = Vec::with_capacity(8 * 3);
    for i in 1..9u32 {
        idx.push(0);
        idx.push(i);
        idx.push(i + 1);
    }
    idx
}

// ---------------------------------------------------------------------------
// Sun / moon
// ---------------------------------------------------------------------------

/// Sun billboard half-size in blocks (vanilla `SkyRenderer.SUN_SIZE`).
pub const SUN_SIZE: f32 = 30.0;
/// Sun billboard distance in blocks (vanilla `SkyRenderer.SUN_HEIGHT`).
pub const SUN_HEIGHT: f32 = 100.0;
/// Moon billboard half-size in blocks (vanilla `SkyRenderer.MOON_SIZE`).
pub const MOON_SIZE: f32 = 20.0;
/// Moon billboard distance in blocks (vanilla `SkyRenderer.MOON_HEIGHT`).
pub const MOON_HEIGHT: f32 = 100.0;

/// The rotation vanilla's pose stack accumulates before placing a celestial
/// billboard or the star field: a fixed `-90`-degree turn about `+Y` (so the
/// unrotated quad's "up" axis becomes the sky's zenith), then the time-varying
/// rotation about `+X` by `angle_rad`. Composing sun/moon/star geometry through
/// this single helper is what keeps all three agreeing on which way is "up" in
/// the sky.
#[must_use]
pub fn celestial_rotation_matrix(angle_rad: f32) -> Mat4 {
    Mat4::from_rotation_y(-90f32.to_radians()) * Mat4::from_rotation_x(angle_rad)
}

/// Camera-relative positions of a celestial billboard quad (sun or moon),
/// vertex order matching vanilla's `buildCelestialQuad`/`buildMoonPhases`:
/// `(-1,-1), (1,-1), (1,1), (-1,1)` in the quad's own `(u, v)` before the
/// transform. Composes [`celestial_rotation_matrix`] with the translate-then-
/// scale vanilla applies per billboard (`translate(0, height, 0)` then
/// `scale(size, 1, size)`).
#[must_use]
pub fn celestial_quad_positions(angle_rad: f32, height: f32, size: f32) -> [[f32; 3]; 4] {
    let m = celestial_rotation_matrix(angle_rad)
        * Mat4::from_translation(Vec3::new(0.0, height, 0.0))
        * Mat4::from_scale(Vec3::new(size, 1.0, size));
    let local = [
        Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, 1.0),
        Vec3::new(-1.0, 0.0, 1.0),
    ];
    local.map(|p| m.transform_point3(p).to_array())
}

/// UVs for a celestial quad's 4 corners (same winding as
/// [`celestial_quad_positions`]) into atlas rect `[u0, v0, u1, v1]`.
///
/// `mirrored` swaps the rect corners the way vanilla's `buildMoonPhases` does
/// relative to `buildSunQuad` (the moon texture is authored mirrored relative
/// to the sun): sun corner 0 samples `(u0, v0)`, moon corner 0 samples
/// `(u1, v1)`.
#[must_use]
pub fn celestial_quad_uvs(rect: [f32; 4], mirrored: bool) -> [[f32; 2]; 4] {
    let [u0, v0, u1, v1] = rect;
    if mirrored {
        [[u1, v1], [u0, v1], [u0, v0], [u1, v0]]
    } else {
        [[u0, v0], [u1, v0], [u1, v1], [u0, v1]]
    }
}

/// Two triangles over a quad's 4 vertices (`0,1,2,2,3,0`), for any of the quad
/// builders in this module.
#[must_use]
pub const fn quad_indices() -> [u32; 6] {
    [0, 1, 2, 2, 3, 0]
}

// ---------------------------------------------------------------------------
// Sunrise / sunset horizon band
// ---------------------------------------------------------------------------

/// Perimeter steps in the sunrise/sunset fan (vanilla `SkyRenderer.SUNRISE_STEPS`).
pub const SUNRISE_STEPS: usize = 16;
/// Vertices in the sunrise/sunset fan: one centre plus `SUNRISE_STEPS + 1`
/// perimeter points (the last repeats the first to close the fan) — vanilla's
/// own `int vertices = 18`.
pub const SUNRISE_FAN_VERTICES: usize = SUNRISE_STEPS + 2;
/// Distance from the eye to the fan's bright centre, in blocks
/// (`buildSunriseFan`'s `addVertex(0, 100, 0)`).
pub const SUNRISE_FAN_HEIGHT: f32 = 100.0;
/// The fan's perimeter radius, in blocks (`sinAngle * 120`).
pub const SUNRISE_FAN_RADIUS: f32 = 120.0;
/// The fan's out-of-plane bow, in blocks (`-cosAngle * 40`); scaled by the
/// band's alpha at draw time, which is what makes the band flatten as it fades.
pub const SUNRISE_FAN_BOW: f32 = 40.0;

/// Camera-relative, **untransformed** positions of the sunrise/sunset fan
/// (vanilla `SkyRenderer.buildSunriseFan`): a centre vertex at
/// `(0, SUNRISE_FAN_HEIGHT, 0)` followed by 17 perimeter vertices at
/// `(sin(a) * 120, cos(a) * 120, -cos(a) * 40)` for `a = i * 2π/16`, `i` in
/// `0..=16`.
///
/// Apply [`sunrise_fan_transform`] to place them; on their own they are a disc
/// standing in the XY plane 100 blocks *above* the eye, which is not where the
/// band is drawn.
#[must_use]
pub fn sunrise_fan_positions() -> [[f32; 3]; SUNRISE_FAN_VERTICES] {
    let mut out = [[0.0f32; 3]; SUNRISE_FAN_VERTICES];
    out[0] = [0.0, SUNRISE_FAN_HEIGHT, 0.0];
    for i in 0..=SUNRISE_STEPS {
        let angle = i as f32 * std::f32::consts::TAU / SUNRISE_STEPS as f32;
        let (sin, cos) = angle.sin_cos();
        out[i + 1] = [
            sin * SUNRISE_FAN_RADIUS,
            cos * SUNRISE_FAN_RADIUS,
            -cos * SUNRISE_FAN_BOW,
        ];
    }
    out
}

/// Per-vertex alpha for [`sunrise_fan_positions`]: `1.0` at the centre, `0.0`
/// at every perimeter vertex (`ARGB.white(1.0F)` / `ARGB.white(0.0F)` in
/// `buildSunriseFan`).
///
/// The *colour* is white in the buffer; the band's actual hue arrives as
/// `ColorModulator` — `core/position_color.fsh` computes
/// `vertexColor * ColorModulator`, so the effective fragment is
/// `sunrise_rgb` with alpha `vertex_alpha * sunrise_alpha`. Both factors are
/// folded into the vertex colour on the CPU here, exactly as the sky disc's
/// per-frame colour already is.
#[must_use]
pub fn sunrise_fan_vertex_alphas() -> [f32; SUNRISE_FAN_VERTICES] {
    let mut out = [0.0f32; SUNRISE_FAN_VERTICES];
    out[0] = 1.0;
    out
}

/// Triangle-fan indices for [`sunrise_fan_positions`]: [`SUNRISE_STEPS`]
/// triangles, all sharing the centre vertex.
#[must_use]
pub fn sunrise_fan_indices() -> Vec<u32> {
    let mut idx = Vec::with_capacity(SUNRISE_STEPS * 3);
    for i in 1..=SUNRISE_STEPS as u32 {
        idx.push(0);
        idx.push(i);
        idx.push(i + 1);
    }
    idx
}

/// The transform that places the sunrise/sunset fan on the horizon, on the
/// correct side of the sky, squashed by the band's own `alpha`.
///
/// A literal reading of `SkyRenderer.renderSunriseAndSunset`, which builds
/// `poseStack` from **identity** (`LevelRenderer.addSkyPass` hands it a fresh
/// `new PoseStack()`, unlike `renderSunMoonAndStars` which adds its own
/// `Axis.YP.rotationDegrees(-90)`):
///
/// ```java
/// poseStack.mulPose(Axis.XP.rotationDegrees(90.0F));
/// float angle = Mth.sin(sunAngle) < 0.0F ? 180.0F : 0.0F;
/// poseStack.mulPose(Axis.ZP.rotationDegrees(angle + 90.0F));
/// modelViewStack.mul(poseStack.last().pose());
/// modelViewStack.scale(1.0F, 1.0F, alpha);
/// ```
///
/// `mulPose` *post*-multiplies, so the composed matrix is
/// `Rx(90°) · Rz(90° + flip) · S(1, 1, alpha)` and a vertex is scaled first,
/// then Z-rotated, then X-rotated — getting that order backwards puts the band
/// 90° away from the sun, in the middle of nowhere, still looking like a
/// plausible horizon glow in a screenshot.
///
/// Working the centre vertex through it: `(0, 100, 0)` → `Rz(90°)` →
/// `(-100, 0, 0)` → `Rx(90°)` → `(-100, 0, 0)`. So the band centres on the
/// horizon 100 blocks toward `-X`, which is exactly where
/// [`celestial_quad_positions`] puts the setting sun. The perimeter becomes
/// `(-cos(a) · 40 · alpha, sin(a) · 120, cos(a) · 120)`: ±120 blocks wide along
/// the horizon, and only ±40·alpha tall — a band, not a disc, flattening as it
/// fades.
///
/// # Only the *sign* of `sin(sun_angle_rad)` is consumed
///
/// The `flip` picks dawn (`+X`) or dusk (`-X`). That makes this function immune
/// to the #49 ramp-shape divergence in [`celestial_angle_for_time_of_day`]: the
/// sign is stable across the whole of each band's non-zero-alpha window
/// (measured on the dump: dusk `11302..=14175` has `sin > 0` throughout, dawn
/// `21825..=702` has `sin < 0` throughout), so a slightly-wrong ramp cannot make
/// the band flicker sides.
#[must_use]
pub fn sunrise_fan_transform(sun_angle_rad: f32, alpha: f32) -> Mat4 {
    let flip = if sun_angle_rad.sin() < 0.0 { 180.0 } else { 0.0 };
    Mat4::from_rotation_x(90f32.to_radians())
        * Mat4::from_rotation_z((flip + 90.0f32).to_radians())
        * Mat4::from_scale(Vec3::new(1.0, 1.0, alpha))
}

/// The alpha below which vanilla skips the sunrise/sunset draw entirely
/// (`renderSunriseAndSunset`'s `if (!(alpha <= 0.001F))`).
pub const SUNRISE_MIN_ALPHA: f32 = 0.001;

// ---------------------------------------------------------------------------
// Star field
// ---------------------------------------------------------------------------

/// Star count (vanilla `SkyRenderer.STAR_COUNT`). The *iteration* count, not
/// the guaranteed output count — see [`build_star_field`].
pub const STAR_COUNT: usize = 1500;
/// Star field distance in blocks (vanilla `SkyRenderer`'s star `starDistance`).
pub const STAR_DISTANCE: f32 = 100.0;
/// The seed this module uses for its own deterministic star field. **Not**
/// vanilla's seed in any meaningful sense — see [`build_star_field`]'s docs.
pub const STAR_FIELD_SEED: u64 = 10842;

/// A small, dependency-free splitmix64 PRNG, seeded once.
///
/// Deliberately not vanilla's Java `RandomSource`: reproducing that generator
/// bit-for-bit is not attempted here (the star field is a visual feature, not
/// a decode-parity gate — nothing anywhere compares it against captured server
/// bytes). This generator is chosen only for being small, dependency-free, and
/// *platform-and-run deterministic*, which is what makes [`build_star_field`]
/// reproducible and testable at all; the resulting star positions will not
/// match a real vanilla client's.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform `f32` in `[0, 1)`, from the top 24 bits (full `f32` mantissa
    /// precision, no rounding bias toward either end).
    fn next_f32(&mut self) -> f32 {
        const TWO_POW_24: f32 = 16_777_216.0;
        ((self.next_u64() >> 40) as f32) / TWO_POW_24
    }
}

/// Builds the star field's quads (unrotated — apply [`celestial_rotation_matrix`]
/// per frame to place them, matching how vanilla rotates the static star
/// buffer by `starAngle` rather than rebuilding it).
///
/// Ports vanilla's `SkyRenderer.buildStars` algorithm: reject-sample a point in
/// the shell `0.1 <= |p| < 1.0` inside the unit cube, place a billboard quad of
/// random size at that point normalized to [`STAR_DISTANCE`], oriented to face
/// outward (tangent to the sphere) with a random in-plane rotation. [`STAR_COUNT`]
/// is the *iteration* count exactly as in vanilla — rejected samples are
/// skipped, not retried, so the returned `Vec` is shorter than `STAR_COUNT`
/// (vanilla's own `starIndexCount` is likewise runtime-derived, not a
/// constant, for the same reason).
///
/// See the struct docs on [`SplitMix64`] for why this does not reproduce
/// vanilla's exact star positions, only its distribution shape.
#[must_use]
pub fn build_star_field(seed: u64) -> Vec<[[f32; 3]; 4]> {
    let mut rng = SplitMix64::new(seed);
    let mut quads = Vec::with_capacity(STAR_COUNT);
    for _ in 0..STAR_COUNT {
        let x = rng.next_f32() * 2.0 - 1.0;
        let y = rng.next_f32() * 2.0 - 1.0;
        let z = rng.next_f32() * 2.0 - 1.0;
        let star_size = 0.15 + rng.next_f32() * 0.1;
        let len_sq = x * x + y * y + z * z;
        if len_sq <= 0.010_000_001 || len_sq >= 1.0 {
            continue;
        }
        let dir = Vec3::new(x, y, z).normalize();
        let center = dir * STAR_DISTANCE;
        let z_rot = rng.next_f32() * std::f32::consts::TAU;

        // An orthonormal basis in the plane perpendicular to `dir`, so the
        // quad is tangent to the star sphere at `center`.
        let up_hint = if dir.y.abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let tangent = dir.cross(up_hint).normalize();
        let bitangent = dir.cross(tangent);
        let (s, c) = z_rot.sin_cos();
        let u = tangent * c + bitangent * s;
        let v = tangent * -s + bitangent * c;

        let corner = |su: f32, sv: f32| (center + u * (su * star_size) + v * (sv * star_size)).to_array();
        quads.push([
            corner(1.0, -1.0),
            corner(1.0, 1.0),
            corner(-1.0, 1.0),
            corner(-1.0, -1.0),
        ]);
    }
    quads
}

// ---------------------------------------------------------------------------
// Clouds
// ---------------------------------------------------------------------------

/// Cloud plane height in blocks: the overworld's
/// `EnvironmentAttributes.CLOUD_HEIGHT`, which `DimensionTypes.java:38` sets to
/// `192.33F` — the same value as the attribute's registered default
/// (`EnvironmentAttributes.java:52-54`). This was a rounded `192.0`.
pub const CLOUD_HEIGHT: f32 = 192.33;
/// Blocks per cloud-texture cell (vanilla `CloudRenderer.CELL_SIZE_IN_BLOCKS`).
pub const CLOUD_CELL_BLOCKS: f32 = 12.0;

/// The overworld's `EnvironmentAttributes.CLOUD_COLOR`, as **linear** RGB.
///
/// `DimensionTypes.java:37` sets it to `ARGB.white(0.8F)`, and `ARGB.white`
/// (`ARGB.java:188`) is `as8BitChannel(alpha) << 24 | 16777215` — so the RGB is
/// `0xFFFFFF`, **pure white**, and only the alpha is `0.8`. Vanilla's clouds are
/// white geometry at 80% opacity, tinted per-tick by [`CLOUD_COLOR_TRACK`]'s
/// `multiply` modifier (`#FFFFFF` by day, `#191926` at night).
///
/// This is the base [`cloud_color_for_time_of_day`] must be given. It used to be
/// handed `SkyFrame::day_sky_color` and then scaled by an invented `0.9`, which
/// made day clouds `#78A7FF × 0.9` — the reported *blue-grey, flat* clouds. The
/// sky colour is not a cloud colour and never was; they are two separate
/// attributes with two separate timeline tracks.
pub const CLOUD_COLOR_RGB: [f32; 3] = [1.0, 1.0, 1.0];

/// The alpha of the same attribute: `as8BitChannel(0.8F) = 204`, i.e. `204/255`
/// exactly `0.8`. Every [`CLOUD_COLOR_TRACK`] keyframe has alpha `0xff`, so the
/// per-tick `multiply` leaves it untouched and this is the alpha at every hour.
///
/// It needs the cloud pipeline to blend (vanilla's `CLOUDS_SNIPPET` uses
/// `BlendFunction.TRANSLUCENT`, `RenderPipelines.java:109`). An opaque cloud
/// pipeline silently discards it.
pub const CLOUD_COLOR_ALPHA: f32 = 0.8;
/// Scroll speed in blocks per tick (vanilla `CloudRenderer.BLOCKS_PER_SECOND`
/// applied per-tick as `0.03` — the renderer's own comment there literally
/// reads `0.030000001F`, i.e. this constant).
pub const CLOUD_SCROLL_BLOCKS_PER_TICK: f32 = 0.030_000_001;

/// One large, alpha-tested, camera-centred quad covering `half_extent` blocks
/// in every direction, sampled with a wrapping (`Repeat`-address-mode) sampler
/// across the *whole* `clouds.png`.
///
/// # A deliberate simplification vs. vanilla's fancy clouds
///
/// Vanilla voxelizes `clouds.png` into a 3D cell grid and extrudes visible
/// faces (`CloudRenderer.buildMesh`/`buildExtrudedCell`) for its "fancy" cloud
/// mode. This instead reproduces only the flatter "fast" mode: a single quad
/// whose fragment shader alpha-tests against the same texture (a transparent
/// texel is an empty cell — `CloudRenderer.isCellEmpty`'s `alpha < 10` check —
/// so per-pixel alpha-testing this texture on a flat quad reproduces "which
/// cells are filled" with no CPU-side cell meshing at all). No cell extrusion,
/// no top/side faces, no "inside the cloud layer" cross-section. Chosen for
/// scope: implementing the full voxel mesher is a second renderer's worth of
/// work for a decoration layer, and "flat clouds" is itself a real, selectable
/// vanilla mode, not an invented approximation.
///
/// Returns `(camera-relative positions, uvs)`, both wound `0,1,2,2,3,0`-ready
/// (see [`quad_indices`]).
#[must_use]
pub fn cloud_plane_geometry(
    camera_pos: [f32; 3],
    time_of_day: i64,
    texture_width_texels: u32,
    texture_height_texels: u32,
    half_extent: f32,
) -> ([[f32; 3]; 4], [[f32; 2]; 4]) {
    let local_y = CLOUD_HEIGHT - camera_pos[1];
    let positions = [
        [-half_extent, local_y, -half_extent],
        [half_extent, local_y, -half_extent],
        [half_extent, local_y, half_extent],
        [-half_extent, local_y, half_extent],
    ];

    let scroll_x = time_of_day as f32 * CLOUD_SCROLL_BLOCKS_PER_TICK;
    let tex_w_blocks = CLOUD_CELL_BLOCKS * texture_width_texels.max(1) as f32;
    let tex_h_blocks = CLOUD_CELL_BLOCKS * texture_height_texels.max(1) as f32;
    let uvs = positions.map(|p| {
        let world_x = camera_pos[0] + p[0] + scroll_x;
        let world_z = camera_pos[2] + p[2];
        [world_x / tex_w_blocks, world_z / tex_h_blocks]
    });

    (positions, uvs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celestial_angle_matches_the_validated_noon_and_midnight_anchors() {
        // Same anchors `entity.rs::sky_darken_for_time_of_day`'s own tests use,
        // so a drift between the two duplicated formulas would be caught here.
        assert!((celestial_angle_for_time_of_day(6_000) - 0.0).abs() < 1e-5);
        assert!((celestial_angle_for_time_of_day(18_000) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn celestial_angle_reduces_a_large_world_age_into_the_day() {
        assert_eq!(
            celestial_angle_for_time_of_day(18_000),
            celestial_angle_for_time_of_day(18_000 + 24_000 * 500)
        );
        assert_eq!(
            celestial_angle_for_time_of_day(6_000),
            celestial_angle_for_time_of_day(6_000 - 24_000 * 500)
        );
    }

    #[test]
    fn sky_color_is_bright_at_noon_and_dark_at_midnight() {
        let day = [0.25, 0.46, 0.83];
        let noon = sky_color_for_time_of_day(6_000, day);
        let midnight = sky_color_for_time_of_day(18_000, day);
        assert!((noon[2] - day[2]).abs() < 1e-3, "noon should read as day_color: {noon:?}");
        assert!(
            midnight[2] < day[2] * 0.1,
            "midnight should be much darker than day: {midnight:?}"
        );
    }

    /// The night disc is now *exactly* black, which is what `SKY_COLOR`'s
    /// `#000000` keyframe says — the previous hand-rolled `NIGHT` constant
    /// `[0.006, 0.008, 0.02]` was a guess. The fog track is what keeps the
    /// night horizon from being black too.
    #[test]
    fn night_sky_is_black_but_night_fog_is_not() {
        let day = [0.25, 0.46, 0.83];
        assert_eq!(sky_color_for_time_of_day(18_000, day), [0.0, 0.0, 0.0]);
        let fog = fog_color_for_time_of_day(18_000, day);
        assert!(
            fog.iter().any(|c| *c > 0.0),
            "the night fog multiplier is #161616, never black: {fog:?}"
        );
        // ...and it is much darker than day, so this is not just "unchanged".
        assert!(fog[2] < day[2] * 0.05, "{fog:?}");
    }

    /// Channel order is the one thing about an ARGB hex table that a plausible
    /// screenshot cannot disprove, so it is pinned against a hand-decoded
    /// keyframe: `day.json`'s tick-12732 entry is `#feda6333`.
    #[test]
    fn sunrise_color_is_argb_not_rgba() {
        // Landing exactly on the keyframe returns it verbatim (the `t >= toTicks`
        // branch of `sample`), so no interpolation can mask a channel swap.
        assert_eq!(
            sunrise_sunset_color_for_time_of_day(12_732),
            [0xda, 0x63, 0x33, 0xfe],
            "expected the warm orange (218, 99, 51) at alpha 254"
        );
        // A green-at-20%-alpha reading of the same hex would put 0x33 in alpha.
        let [.., a] = sunrise_sunset_color_for_time_of_day(12_732);
        assert_ne!(a, 0x33);
    }

    /// Noon and deep midnight both draw no band at all; dusk and dawn both do.
    /// The dawn window wraps the tick-0 seam, which the periodic sampler's
    /// leading wrap segment is the only thing that gets right.
    #[test]
    fn sunrise_band_is_invisible_by_day_and_visible_at_both_twilights() {
        let alpha = |t| f32::from(sunrise_sunset_color_for_time_of_day(t)[3]) / 255.0;
        assert_eq!(alpha(6_000), 0.0, "noon");
        assert_eq!(alpha(18_000), 0.0, "midnight");
        assert!(alpha(12_732) > 0.99, "peak sunset: {}", alpha(12_732));
        assert!(alpha(23_272) > 0.99, "peak sunrise: {}", alpha(23_272));
        // Tick 0 sits *before* the first keyframe (71), i.e. on the wraparound
        // segment from the last keyframe (23757) — clamping to the first
        // keyframe instead would report 0x5f here, and reaching for the last
        // would report 0xb1.
        assert_eq!(sunrise_sunset_color_for_time_of_day(0)[3], 0x71);
    }

    #[test]
    fn sky_and_fog_multipliers_are_white_all_day_and_dark_all_night() {
        for t in [1_000, 6_000, 11_000] {
            assert_eq!(sky_color_multiplier_for_time_of_day(t), [0xff; 3], "tick {t}");
            assert_eq!(fog_color_multiplier_for_time_of_day(t), [0xff; 3], "tick {t}");
        }
        assert_eq!(sky_color_multiplier_for_time_of_day(18_000), [0x00; 3]);
        assert_eq!(fog_color_multiplier_for_time_of_day(18_000), [0x11, 0x11, 0x16]);
    }

    /// The fan is 18 vertices with a bright centre and a transparent rim, and
    /// its first and last perimeter vertices coincide (`i = 0` and `i = 16` are
    /// the same angle) so the fan closes.
    #[test]
    fn sunrise_fan_is_a_closed_eighteen_vertex_fan_bright_only_at_the_centre() {
        let pos = sunrise_fan_positions();
        let alphas = sunrise_fan_vertex_alphas();
        assert_eq!(pos.len(), 18);
        assert_eq!(pos[0], [0.0, SUNRISE_FAN_HEIGHT, 0.0]);
        assert_eq!(alphas[0], 1.0);
        assert!(alphas[1..].iter().all(|a| *a == 0.0));
        for axis in 0..3 {
            assert!(
                (pos[1][axis] - pos[17][axis]).abs() < 1e-3,
                "fan does not close on axis {axis}: {:?} vs {:?}",
                pos[1],
                pos[17]
            );
        }
        let idx = sunrise_fan_indices();
        assert_eq!(idx.len(), SUNRISE_STEPS * 3);
        assert!(idx.chunks(3).all(|tri| tri[0] == 0));
        assert!(idx.iter().all(|i| (*i as usize) < SUNRISE_FAN_VERTICES));
    }

    /// The band must land on the horizon at the sun's own bearing, and on
    /// *opposite* sides at dawn and dusk. Derived from the same
    /// `celestial_quad_positions` the sun draw uses rather than from a restated
    /// constant, so a change to one has to move the other.
    #[test]
    fn sunrise_fan_centres_on_the_horizon_where_the_sun_is() {
        let horizon_bearing = |time_of_day: i64| {
            let angle = celestial_angle_for_time_of_day(time_of_day) * std::f32::consts::TAU;
            let sun = celestial_quad_positions(angle, SUN_HEIGHT, SUN_SIZE);
            let sun_x = sun.iter().map(|p| p[0]).sum::<f32>() / 4.0;
            let fan = sunrise_fan_transform(angle, 1.0)
                .transform_point3(Vec3::from(sunrise_fan_positions()[0]));
            (sun_x, fan)
        };

        // Dusk (peak sunset): the sun is toward -X and so is the band.
        let (sun_x, fan) = horizon_bearing(12_732);
        assert!(sun_x < -50.0, "expected the sun toward -X at sunset, got {sun_x}");
        assert!(fan.x < -50.0, "band should follow it: {fan:?}");
        assert!(fan.y.abs() < 1e-3, "band centre must sit on the horizon: {fan:?}");

        // Dawn (peak sunrise): both flip to +X.
        let (sun_x, fan) = horizon_bearing(23_272);
        assert!(sun_x > 50.0, "expected the sun toward +X at sunrise, got {sun_x}");
        assert!(fan.x > 50.0, "band should follow it: {fan:?}");
        assert!(fan.y.abs() < 1e-3, "band centre must sit on the horizon: {fan:?}");
    }

    /// `alpha` squashes the band's vertical extent, not its width — that is the
    /// whole effect of vanilla's `scale(1, 1, alpha)` once the two rotations
    /// have moved the bow axis onto Y.
    #[test]
    fn fan_alpha_flattens_the_band_vertically_without_narrowing_it() {
        let pos = sunrise_fan_positions();
        let extents = |alpha: f32| {
            let m = sunrise_fan_transform(1.354, alpha); // a dusk sun angle
            let (mut h, mut v) = (0.0f32, 0.0f32);
            for p in pos {
                let t = m.transform_point3(Vec3::from(p));
                h = h.max(t.z.abs());
                v = v.max(t.y.abs());
            }
            (h, v)
        };
        let (h_full, v_full) = extents(1.0);
        let (h_faint, v_faint) = extents(0.25);
        assert!((h_full - h_faint).abs() < 1e-3, "width must not change: {h_full} vs {h_faint}");
        assert!(v_faint < v_full * 0.5, "height must shrink: {v_full} -> {v_faint}");
        // And the band is wide-and-thin even at full alpha, not a disc.
        assert!(h_full > v_full * 2.0, "expected a band, got {h_full} x {v_full}");
    }

    #[test]
    fn star_brightness_is_zero_at_noon_and_positive_at_midnight() {
        assert_eq!(star_brightness_for_time_of_day(6_000), 0.0);
        assert!(star_brightness_for_time_of_day(18_000) > 0.0);
    }

    /// `MoonPhase.startTick() == index * 24000` fixes phase `n` to world-day
    /// `n`; day 8 must wrap back to phase 0, not overflow or misalign.
    #[test]
    fn moon_phase_cycles_every_eight_days() {
        assert_eq!(moon_phase_index_for_time_of_day(0), 0);
        assert_eq!(moon_phase_index_for_time_of_day(24_000), 1);
        assert_eq!(moon_phase_index_for_time_of_day(24_000 * 7), 7);
        assert_eq!(moon_phase_index_for_time_of_day(24_000 * 8), 0);
        assert_eq!(moon_phase_index_for_time_of_day(24_000 * 8 + 12_000), 0);
    }

    /// Issue #399. Every expected value here is `Math.min(chunks * 16, 512)`
    /// worked by hand from `AtmosphericFogEnvironment.java:73` +
    /// `FogRenderer.java:185`, never by calling the function under test with a
    /// rearranged argument.
    #[test]
    fn sky_fog_end_is_the_render_distance_clamped_to_the_attribute_default() {
        assert_eq!(sky_fog_end_for_render_distance(2), 32.0);
        assert_eq!(sky_fog_end_for_render_distance(4), 64.0);
        assert_eq!(sky_fog_end_for_render_distance(8), 128.0);
        assert_eq!(sky_fog_end_for_render_distance(16), 256.0);
        // 32 chunks is where the clamp starts binding: the only render distance
        // at which the pre-#399 constant was correct.
        assert_eq!(sky_fog_end_for_render_distance(32), SKY_FOG_END_DISTANCE);
        assert_eq!(sky_fog_end_for_render_distance(48), SKY_FOG_END_DISTANCE);
        assert_eq!(sky_fog_end_for_render_distance(1_000), SKY_FOG_END_DISTANCE);
    }

    /// The bug this replaced was not "a wrong number" but "a number that did not
    /// vary", so the property worth pinning is the *variation*, not any single
    /// value — the `magnitude` species in `CLAUDE.md`'s table. A gate that only
    /// checked RD 32 would have passed against the old constant.
    #[test]
    fn sky_fog_end_actually_varies_with_render_distance_below_the_clamp() {
        let rd8 = sky_fog_end_for_render_distance(8);
        let rd32 = sky_fog_end_for_render_distance(32);
        assert!(
            (rd32 / rd8 - 4.0).abs() < 1e-6,
            "RD 32's gradient must end exactly 4x further out than RD 8's \
             (512 vs 128), got {rd32} vs {rd8}"
        );
        assert_ne!(
            rd8, SKY_FOG_END_DISTANCE,
            "the clamp is inert: the attribute default is being used at RD 8, \
             which is #399 unfixed"
        );
    }

    /// Zero blocks must not become a `NaN` divisor path's excuse: the Rust side
    /// reports the honest `0.0` and the shader floors it (see
    /// `sky_fog_end_for_render_distance`'s doc).
    #[test]
    fn sky_fog_end_is_non_negative_and_never_exceeds_the_attribute_default() {
        assert_eq!(sky_fog_end_for_render_distance(0), 0.0);
        assert_eq!(sky_fog_end_for_render_distance_blocks(-1.0), 0.0);
        assert_eq!(sky_fog_end_for_render_distance_blocks(128.0), 128.0);
        assert_eq!(
            sky_fog_end_for_render_distance_blocks(4_096.0),
            SKY_FOG_END_DISTANCE
        );
    }

    #[test]
    fn sky_disc_has_ten_vertices_closing_the_fan() {
        let disc = sky_disc_positions(16.0);
        assert_eq!(disc[0], [0.0, 16.0, 0.0]);
        // -180 and +180 degrees are the same point on the circle, so the
        // first and last perimeter vertices must coincide (closing the fan).
        let first_perimeter = disc[1];
        let last_perimeter = disc[9];
        assert!(
            (first_perimeter[0] - last_perimeter[0]).abs() < 1e-3
                && (first_perimeter[2] - last_perimeter[2]).abs() < 1e-3,
            "fan does not close: {first_perimeter:?} vs {last_perimeter:?}"
        );
    }

    #[test]
    fn dark_disc_uses_the_opposite_radius_sign() {
        // y < 0 flips the disc's x radius (vanilla: `Math.signum(yy) * 512`).
        let top = sky_disc_positions(16.0);
        let bottom = sky_disc_positions(-16.0);
        assert_eq!(top[1][0], -bottom[1][0]);
    }

    #[test]
    fn sky_disc_indices_form_eight_triangles_sharing_the_centre() {
        let idx = sky_disc_indices();
        assert_eq!(idx.len(), 24);
        assert!(idx.chunks(3).all(|tri| tri[0] == 0));
    }

    #[test]
    fn celestial_quad_is_centred_at_height_scaled_by_size() {
        let quad = celestial_quad_positions(0.0, SUN_HEIGHT, SUN_SIZE);
        let center = quad.iter().fold([0.0f32; 3], |acc, p| {
            [acc[0] + p[0] / 4.0, acc[1] + p[1] / 4.0, acc[2] + p[2] / 4.0]
        });
        // At angle 0 the -90-degree Y rotation puts the billboard's "up"
        // (local +Z translate) along world +X; regardless of axis, its
        // distance from the origin must equal SUN_HEIGHT and every corner
        // must be `SUN_SIZE` from that centre in the billboard plane.
        let dist = (center[0] * center[0] + center[1] * center[1] + center[2] * center[2]).sqrt();
        assert!((dist - SUN_HEIGHT).abs() < 1e-2, "centre at wrong distance: {center:?}");
        for p in quad {
            let r = ((p[0] - center[0]).powi(2)
                + (p[1] - center[1]).powi(2)
                + (p[2] - center[2]).powi(2))
            .sqrt();
            assert!((r - SUN_SIZE * std::f32::consts::SQRT_2).abs() < 1e-2, "{r}");
        }
    }

    #[test]
    fn celestial_quad_moves_as_angle_advances() {
        let a = celestial_quad_positions(0.0, MOON_HEIGHT, MOON_SIZE);
        let b = celestial_quad_positions(std::f32::consts::FRAC_PI_2, MOON_HEIGHT, MOON_SIZE);
        assert_ne!(a, b);
    }

    #[test]
    fn sun_and_moon_uvs_are_mirrored() {
        let rect = [0.0, 0.0, 0.5, 0.5];
        let sun = celestial_quad_uvs(rect, false);
        let moon = celestial_quad_uvs(rect, true);
        assert_eq!(sun[0], [0.0, 0.0]);
        assert_eq!(moon[0], [0.5, 0.5]);
    }

    #[test]
    fn star_field_is_shorter_than_the_iteration_count_but_substantial() {
        let stars = build_star_field(STAR_FIELD_SEED);
        assert!(!stars.is_empty());
        assert!(
            stars.len() < STAR_COUNT,
            "expected some rejection-sampled stars to be skipped, got {} of {}",
            stars.len(),
            STAR_COUNT
        );
        // The rejection shell keeps roughly (volume of unit ball) / (volume of
        // the enclosing cube) of samples, order ~50%; a sanity band rather
        // than an exact figure since this generator is not vanilla's.
        assert!(stars.len() > STAR_COUNT / 4, "too few stars: {}", stars.len());
    }

    /// Every star quad's centre must be very close to [`STAR_DISTANCE`] from
    /// the origin (a bug in the tangent-basis construction could shrink or
    /// blow up the placement instead of just rotating it).
    #[test]
    fn every_star_sits_at_the_configured_distance() {
        for quad in build_star_field(STAR_FIELD_SEED) {
            let center = [
                quad.iter().map(|p| p[0]).sum::<f32>() / 4.0,
                quad.iter().map(|p| p[1]).sum::<f32>() / 4.0,
                quad.iter().map(|p| p[2]).sum::<f32>() / 4.0,
            ];
            let dist = (center[0] * center[0] + center[1] * center[1] + center[2] * center[2]).sqrt();
            assert!(
                (dist - STAR_DISTANCE).abs() < 1.0,
                "star centre {center:?} at distance {dist}, expected ~{STAR_DISTANCE}"
            );
        }
    }

    #[test]
    fn same_seed_is_fully_deterministic() {
        assert_eq!(build_star_field(STAR_FIELD_SEED), build_star_field(STAR_FIELD_SEED));
    }

    #[test]
    fn different_seeds_produce_different_fields() {
        assert_ne!(build_star_field(1), build_star_field(2));
    }

    #[test]
    fn cloud_plane_is_centred_on_the_camera_horizontally() {
        let (positions, _) = cloud_plane_geometry([100.0, 70.0, -40.0], 0, 256, 256, 512.0);
        for p in positions {
            assert!((p[0].abs() - 512.0).abs() < 1e-3);
            assert!((p[2].abs() - 512.0).abs() < 1e-3);
        }
    }

    #[test]
    fn cloud_plane_height_follows_the_camera() {
        let (low, _) = cloud_plane_geometry([0.0, 60.0, 0.0], 0, 256, 256, 512.0);
        let (high, _) = cloud_plane_geometry([0.0, 190.0, 0.0], 0, 256, 256, 512.0);
        assert!(low[0][1] > high[0][1], "cloud plane must drop as the camera rises toward it");
    }

    /// The scroll offset shifts the UV horizontally without touching the Z
    /// axis (vanilla only scrolls clouds along X).
    #[test]
    fn cloud_uvs_scroll_along_x_over_time() {
        let (_, uv0) = cloud_plane_geometry([0.0, 70.0, 0.0], 0, 256, 256, 512.0);
        let (_, uv1) = cloud_plane_geometry([0.0, 70.0, 0.0], 10_000, 256, 256, 512.0);
        assert_ne!(uv0[0][0], uv1[0][0]);
        assert!((uv0[0][1] - uv1[0][1]).abs() < 1e-6);
    }

    #[test]
    fn quad_indices_are_two_ccw_triangles() {
        assert_eq!(quad_indices(), [0, 1, 2, 2, 3, 0]);
    }
}
