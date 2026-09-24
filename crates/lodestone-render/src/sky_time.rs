//! Time-of-day math and timeline colour tracks for the sky renderer.
//!
//! This module owns the pure day-clock sampling functions used by both the
//! sky geometry and GPU submission layers. The parent `crate::sky` module
//! re-exports the public functions so callers keep the original API.

use crate::fog::multiply_gamma;
// ---------------------------------------------------------------------------
// Time-of-day math
// ---------------------------------------------------------------------------

/// The fraction of a full day/night rotation completed, in `[0, 1)` — vanilla's
/// angle-of-day formula. `0.0` is noon, `0.5` is midnight (verified against the
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

/// One full rotation of the day/night timeline, in ticks.
pub const DAY_PERIOD_TICKS: i64 = 24_000;

/// `minecraft:visual/sunrise_sunset_color`, verbatim from the vanilla day
/// timeline's own data, as `(tick, ARGB)`.
///
/// The values are **ARGB**, not RGBA: `#feda6333` is alpha `0xfe`, red `0xda`,
/// green `0x63`, blue `0x33` — a warm sunset orange at near-full opacity, not a
/// green at 20% alpha. Reading the channel order off the hex string the wrong
/// way round produces a plausible-looking but completely wrong band; the
/// authority is the attribute's own declared channel-order encoding, not the
/// string's appearance. Blue is a constant `0x33` across every keyframe and
/// alpha is what animates, from `0x00` through the middle of the day to `0xfe`
/// at peak sunset (tick 12732).
///
/// The track declares no modifier, so it defaults to a plain override rather
/// than a combine-with-base rule — the sampled keyframe value *is* the final
/// colour, with no base to combine with.
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

/// `minecraft:visual/sky_color`, from the same day timeline. Its modifier is
/// a plain per-channel multiply, so these are a per-tick **multiplier** over
/// whatever base sky colour applies — a biome's own `minecraft:visual/sky_color`
/// in vanilla, the renderer's existing sky constant here.
///
/// White through the whole day, pure black across the night: vanilla's night
/// sky *disc* is genuinely `0x000000`, and the dark-blue night sky people
/// remember is [`FOG_COLOR_TRACK`] showing through at the horizon via the
/// disc's own fog gradient (see [`SKY_FOG_END_DISTANCE`]). Alpha is `0xff`
/// throughout because the underlying colour codec parses a 6-digit
/// `#RRGGBB` as opaque.
const SKY_COLOR_TRACK: [(i32, u32); 4] = [
    (133, 0xff_ff_ff_ff),
    (11_867, 0xff_ff_ff_ff),
    (13_670, 0xff_00_00_00),
    (22_330, 0xff_00_00_00),
];

/// `minecraft:visual/fog_color`, same file, same `multiply` modifier as
/// [`SKY_COLOR_TRACK`]. Unlike the sky's, this one does **not** reach black:
/// `#0c0c16` at dusk and `0x161616` at deep night, which is why the night
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
/// This track exists to prevent a regression from the sky colour becoming
/// timeline-exact: the cloud tint used to
/// be `sky_color * 0.9`, and now that [`SKY_COLOR_TRACK`] correctly reaches
/// `0x000000` at night, that expression makes night clouds exactly invisible.
/// Vanilla keeps them visible with their own non-black track, so the tint reads
/// this instead of the sky.
const CLOUD_COLOR_TRACK: [(i32, u32); 4] = [
    (133, 0xff_ff_ff_ff),
    (11_867, 0xff_ff_ff_ff),
    (13_670, 0xff_19_19_26),
    (22_330, 0xff_19_19_26),
];

/// Vanilla's integer lerp: `p0 + floor(alpha * (p1 - p0))`. The `floor` is
/// load-bearing — a `round` here is off by one byte on roughly half of all
/// ticks, which the JVM gate catches immediately.
pub(crate) fn lerp_int(alpha: f32, p0: i32, p1: i32) -> i32 {
    p0 + (alpha * (p1 - p0) as f32).floor() as i32
}

/// Vanilla's byte-wise sRGB lerp: a per-channel [`lerp_int`] over the raw
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
/// vanilla's own keyframe-track sampler for a **linear**-eased track.
///
/// Three details of that sampler are easy to lose and all three are checked by
/// the JVM gate:
///
/// * Baking the segment list prepends a wraparound segment
///   `(last, last.ticks - period) -> (first, first.ticks)` and appends
///   `(last, last.ticks) -> (first, first.ticks + period)`. So a tick before the
///   *first* keyframe is not clamped to it — it is on the ramp coming round
///   from the last keyframe through the tick-0 seam.
/// * Segment lookup picks the first segment with `t < segment.toTicks`, a
///   strict `<`, so a tick landing exactly on a keyframe belongs to the segment
///   *ending* there and the `t >= toTicks` branch returns that keyframe's
///   value exactly.
/// * the easing is **linear**. The track builder defaults to linear easing and
///   none of the three tracks in this module declares an `ease` in the day
///   timeline's own data — only the neighbouring sun-angle/moon-angle/star-angle
///   tracks opt into a cubic bezier.
fn sample_argb_track(track: &[(i32, u32)], time_of_day: i64) -> u32 {
    debug_assert!(track.len() >= 2, "a periodic track needs at least two keyframes");
    let period = DAY_PERIOD_TICKS as i32;
    let tick = time_of_day.rem_euclid(DAY_PERIOD_TICKS) as i32;
    let &(first_ticks, first_value) = track.first().expect("non-empty track");
    let &(last_ticks, last_value) = track.last().expect("non-empty track");

    // Segment selection, in vanilla's own order: the leading wrap
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

/// The sunrise/sunset colour attribute at `time_of_day`, as
/// `[r, g, b, a]` sRGB bytes (reordered from the track's packed ARGB for a
/// Rust-natural call site).
///
/// `a == 0` for the whole middle of the day and the deep middle of the night —
/// vanilla skips the draw entirely when `alpha <= 0.001`, and so does
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

/// The sky-colour track's per-tick multiplier, as sRGB bytes — white at noon,
/// black at night. Multiply a base sky colour by this in **gamma** space
/// ([`crate::fog::multiply_gamma`]), which is what vanilla's own multiply does.
#[must_use]
pub fn sky_color_multiplier_for_time_of_day(time_of_day: i64) -> [u8; 3] {
    let argb = sample_argb_track(&SKY_COLOR_TRACK, time_of_day);
    [
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
    ]
}

/// The fog-colour track's per-tick multiplier, as sRGB bytes. See
/// [`sky_color_multiplier_for_time_of_day`]; this one bottoms out at
/// `#0c0c16`/`0x161616` rather than black.
#[must_use]
pub fn fog_color_multiplier_for_time_of_day(time_of_day: i64) -> [u8; 3] {
    let argb = sample_argb_track(&FOG_COLOR_TRACK, time_of_day);
    [
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
    ]
}

/// The cloud-colour track's per-tick multiplier, as sRGB bytes. See
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
/// the real cloud-colour track in gamma space.
#[must_use]
pub fn cloud_color_for_time_of_day(time_of_day: i64, day_cloud: [f32; 3]) -> [f32; 3] {
    let m = cloud_color_multiplier_for_time_of_day(time_of_day);
    multiply_gamma(day_cloud, m.map(|c| f32::from(c) / 255.0))
}

/// The sky-dome colour at `time_of_day`: `day_color` (a **linear** RGB base —
/// pass the renderer's clear/sky colour, or a biome's `visual/sky_color` once
/// one is reachable) multiplied by the real sky-colour track in gamma space.
///
/// This replaced a hand-rolled blend toward a fixed dark-navy `NIGHT` constant.
/// Two things changed measurably: night is now exactly black rather than
/// `[0.006, 0.008, 0.02]` (which is what vanilla's `0x000000` keyframe says),
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
/// the real fog-colour track in gamma space, exactly as
/// [`sky_color_for_time_of_day`] does for the sky.
#[must_use]
pub fn fog_color_for_time_of_day(time_of_day: i64, day_fog: [f32; 3]) -> [f32; 3] {
    let m = fog_color_multiplier_for_time_of_day(time_of_day);
    multiply_gamma(day_fog, m.map(|c| f32::from(c) / 255.0))
}

/// Vanilla's legacy star-brightness formula: `0.0` for most of the day, ramping up
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

/// The active [`lodestone_assets::MoonPhase`] for `time_of_day`.
///
/// Each phase begins on a whole world-day boundary, so this is a discrete
/// eight-day cycle rather than a continuous keyframe track.
#[must_use]
pub fn moon_phase_for_time_of_day(time_of_day: i64) -> lodestone_assets::MoonPhase {
    lodestone_assets::MoonPhase::for_day(time_of_day.div_euclid(24_000))
}
