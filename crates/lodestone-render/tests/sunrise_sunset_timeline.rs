//! External-anchor gate for the three `Timelines.OVERWORLD_DAY` colour tracks
//! issue #96 needs — `SUNRISE_SUNSET_COLOR`, `SKY_COLOR` and `FOG_COLOR` —
//! checked **byte-exactly at every one of the 24000 ticks** against a JVM dump
//! of 26.2's own `KeyframeTrackSampler` + `AttributeModifier` machinery.
//!
//! # Why this is the gate that matters for these functions
//!
//! `CLAUDE.md`: *an expected value must originate outside the code under test*.
//! A keyframe interpolator is exactly the shape that satisfies a
//! self-consistency test while being wrong — the keyframe *endpoints* are
//! trivially right (the sampler returns them verbatim), so any test that only
//! checks named ticks like noon and peak sunset passes vacuously on a sampler
//! whose segment selection, wraparound, easing or rounding is broken. Four
//! specific ways `crate::sky`'s port could have been wrong and still looked
//! plausible, all of which this file would catch and a keyframe-only test would
//! not:
//!
//! * **Wraparound.** `KeyframeTrackSampler.bakeSegments` prepends a segment
//!   running from the *last* keyframe at `last.ticks - 24000` to the first, so
//!   ticks `0..70` are mid-ramp rather than clamped to the tick-71 keyframe.
//! * **`floor`, not `round`.** `Mth.lerpInt` is `p0 + floor(alpha * (p1 - p0))`.
//!   Rounding instead is off by one byte on a large fraction of ticks — visually
//!   invisible, and caught here immediately.
//! * **Gamma-space lerp.** `LerpFunction.ofColor()` is `ARGB::srgbLerp`, which
//!   interpolates raw bytes. `ARGB.linearLerp` exists right next to it in the
//!   same file and is *not* what these tracks use.
//! * **Segment boundary strictness.** `getSegmentAt` uses a strict `<` on
//!   `toTicks`, so a tick landing exactly on a keyframe resolves through the
//!   segment ending there.
//!
//! # Provenance
//!
//! `tests/support/sunrise_sunset_timeline_jvm.txt` was produced by
//! `../oracle-java/SunriseSunsetTimelineOracle.java` — the sibling of
//! `SkyLightTimelineOracle.java` (issue #49's gate), same pattern: boot the
//! real 26.2 registries via `VanillaRegistries.createLookup()` and sample
//! `Timeline.createTrackSampler` once per tick. Not a hand re-derivation of the
//! interpolation math, and not this crate's own output pasted back.
//!
//! Regenerate after a version bump with a JDK 25 on `PATH`, or via Docker as
//! `crates/protocol/v770/tests/hardness.rs` documents:
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/lodestone-render/oracle-java && pwd)"
//! CP="$CACHE/versions/26.2/server-26.2.jar:$(find "$CACHE/libraries" -name '*.jar' | tr '\n' ':')"
//! javac -cp "$CP" -d /tmp/sunrise-oracle "$HERE/SunriseSunsetTimelineOracle.java"
//! java -cp "/tmp/sunrise-oracle:$CP" SunriseSunsetTimelineOracle \
//!   > crates/lodestone-render/tests/support/sunrise_sunset_timeline_jvm.txt
//! ```

use std::path::PathBuf;

use lodestone_render::sky::{
    fog_color_multiplier_for_time_of_day, sky_color_multiplier_for_time_of_day,
    sunrise_sunset_color_for_time_of_day,
};

/// One dumped tick: the sunrise/sunset ARGB, the sky and fog multipliers
/// (sampled from a white base, so they are the tracks' own per-tick
/// multipliers), and the sky track applied to `plains`' real `#78a7ff`.
struct Row {
    tick: i64,
    sunrise_argb: u32,
    sky_multiplier_argb: u32,
    fog_multiplier_argb: u32,
    sky_over_plains_argb: u32,
}

fn dump_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/sunrise_sunset_timeline_jvm.txt")
}

fn rows() -> Vec<Row> {
    let text = std::fs::read_to_string(dump_path())
        .expect("tests/support/sunrise_sunset_timeline_jvm.txt must be committed");
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(
                cols.len(),
                5,
                "expected `<tick> <sunrise> <sky_mul> <fog_mul> <sky_over_plains>`, got {line:?}"
            );
            let hex = |i: usize| {
                u32::from_str_radix(cols[i], 16)
                    .unwrap_or_else(|_| panic!("column {i} of {line:?} is hex"))
            };
            Row {
                tick: cols[0].parse().expect("tick column is a decimal int"),
                sunrise_argb: hex(1),
                sky_multiplier_argb: hex(2),
                fog_multiplier_argb: hex(3),
                sky_over_plains_argb: hex(4),
            }
        })
        .collect()
}

/// Packs `[r, g, b, a]` back into the dump's ARGB layout so the comparison is
/// against the raw 32-bit value the JVM printed, not against a re-split of it.
fn pack_argb(rgba: [u8; 4]) -> u32 {
    (u32::from(rgba[3]) << 24)
        | (u32::from(rgba[0]) << 16)
        | (u32::from(rgba[1]) << 8)
        | u32::from(rgba[2])
}

/// An opaque RGB multiplier packed the same way (alpha `0xff`, which is what
/// `AttributeTypes.RGB_COLOR`'s 6-digit codec produces and what the dump shows).
fn pack_opaque(rgb: [u8; 3]) -> u32 {
    0xFF00_0000
        | (u32::from(rgb[0]) << 16)
        | (u32::from(rgb[1]) << 8)
        | u32::from(rgb[2])
}

/// Vanilla's `ARGB.multiply` (`ARGB.java:80`) — byte-space, integer-truncating.
/// Reproduced here rather than imported because the crate's own composition
/// path goes through linear RGB floats (`fog::multiply_gamma`), and this column
/// exists to check the *shape* of that arithmetic independently of the float
/// round trip.
fn argb_multiply(lhs: u32, rhs: u32) -> u32 {
    if lhs == 0xFFFF_FFFF {
        return rhs;
    }
    if rhs == 0xFFFF_FFFF {
        return lhs;
    }
    let ch = |shift: u32| {
        let a = (lhs >> shift) & 0xFF;
        let b = (rhs >> shift) & 0xFF;
        ((a * b) / 255) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

#[test]
fn every_tick_of_all_three_colour_tracks_matches_the_jvm() {
    let rows = rows();
    assert_eq!(rows.len(), 24_000, "dump should cover every tick of the day");

    let mut mismatches: Vec<String> = Vec::new();
    for row in &rows {
        let ours = pack_argb(sunrise_sunset_color_for_time_of_day(row.tick));
        if ours != row.sunrise_argb {
            mismatches.push(format!(
                "tick {}: sunrise ours {ours:08x} vs jvm {:08x}",
                row.tick, row.sunrise_argb
            ));
        }
        let ours = pack_opaque(sky_color_multiplier_for_time_of_day(row.tick));
        if ours != row.sky_multiplier_argb {
            mismatches.push(format!(
                "tick {}: sky ours {ours:08x} vs jvm {:08x}",
                row.tick, row.sky_multiplier_argb
            ));
        }
        let ours = pack_opaque(fog_color_multiplier_for_time_of_day(row.tick));
        if ours != row.fog_multiplier_argb {
            mismatches.push(format!(
                "tick {}: fog ours {ours:08x} vs jvm {:08x}",
                row.tick, row.fog_multiplier_argb
            ));
        }
        if mismatches.len() > 12 {
            break;
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of 24000 ticks disagree with the JVM (first few):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The fourth dumped column: the sky track multiplied through `plains`' real
/// `minecraft:visual/sky_color`. This is the composition step per-biome sky
/// tint will use once biome ids are reachable, so it is pinned now, in the
/// gamma byte space vanilla actually does it in — a linear-space multiply would
/// be visibly brighter at every tick where the multiplier is neither `0x00` nor
/// `0xff`.
#[test]
fn multiplying_a_real_biome_sky_colour_through_the_track_matches_the_jvm() {
    let plains = 0xFF78_A7FF_u32;
    let mut checked = 0usize;
    for row in &rows() {
        let ours = argb_multiply(plains, pack_opaque(sky_color_multiplier_for_time_of_day(row.tick)));
        assert_eq!(
            ours, row.sky_over_plains_argb,
            "tick {}: ours {ours:08x} vs jvm {:08x}",
            row.tick, row.sky_over_plains_argb
        );
        checked += 1;
    }
    assert_eq!(checked, 24_000);
}

/// **Control, EXECUTED.** The gate above only means something if it can fail,
/// and the two most plausible ways to get a keyframe sampler wrong are to clamp
/// instead of wrapping and to round instead of flooring. Both are reproduced
/// here as deliberately-wrong samplers over the same track data, and both must
/// disagree with the JVM dump — if either matched, the real gate would be
/// passing for a reason unrelated to the code it claims to check.
#[test]
fn the_wrong_samplers_this_gate_exists_to_catch_do_fail_it() {
    let rows = rows();

    // Wrong #1: clamp to the first keyframe instead of wrapping through the
    // last one. Only ticks before the first keyframe (71) differ, so a gate
    // that sampled only named times would never see this.
    let clamping_disagreements = rows
        .iter()
        .filter(|row| row.tick < 71)
        .filter(|row| {
            // What a clamping sampler would return: the tick-71 keyframe.
            let clamped = 0x5f_ef_a3_33_u32;
            clamped != row.sunrise_argb
        })
        .count();
    assert!(
        clamping_disagreements > 0,
        "control failed to fail: a sampler that clamped to the first keyframe would \
         agree with the JVM on every tick before it, which would mean the wraparound \
         segment this gate exists to check is unobservable"
    );

    // Wrong #2: round instead of floor in the channel lerp. Count the ticks
    // where that changes the answer; it must be a substantial fraction, not a
    // handful of edge cases.
    let rounding_disagreements = rows
        .iter()
        .filter(|row| rounding_sunrise_sampler(row.tick) != row.sunrise_argb)
        .count();
    assert!(
        rounding_disagreements > 1_000,
        "control failed to fail: only {rounding_disagreements} ticks distinguish floor \
         from round, so this gate would not be evidence that `Mth.lerpInt`'s floor was \
         ported correctly"
    );

    eprintln!(
        "=== #96 timeline gate controls ===\n\
         clamp-instead-of-wrap disagrees on {clamping_disagreements} of the 71 pre-first-keyframe ticks\n\
         round-instead-of-floor disagrees on {rounding_disagreements} of 24000 ticks"
    );
}

/// The deliberately-wrong sampler the control above measures: identical to the
/// shipped one except the channel lerp rounds.
fn rounding_sunrise_sampler(time_of_day: i64) -> u32 {
    // Same table, re-declared here on purpose: a control that imported the
    // shipped constant would silently follow it if the table itself were
    // edited, and this control is only about the arithmetic.
    const TRACK: [(i32, u32); 32] = [
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
    const PERIOD: i32 = 24_000;
    let tick = time_of_day.rem_euclid(i64::from(PERIOD)) as i32;
    let (first_ticks, first_value) = TRACK[0];
    let (last_ticks, last_value) = TRACK[TRACK.len() - 1];
    let (from_ticks, from_value, to_ticks, to_value) = if tick < first_ticks {
        (last_ticks - PERIOD, last_value, first_ticks, first_value)
    } else {
        TRACK
            .windows(2)
            .find(|w| tick < w[1].0)
            .map_or((last_ticks, last_value, first_ticks + PERIOD, first_value), |w| {
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
    let ch = |shift: u32| {
        let a = ((from_value >> shift) & 0xFF) as i32;
        let b = ((to_value >> shift) & 0xFF) as i32;
        // The bug: `round` where vanilla floors.
        let v = a + (alpha * (b - a) as f32).round() as i32;
        ((v.clamp(0, 255)) as u32) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}
