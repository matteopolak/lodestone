//! External-anchor gate for `sky_darken_for_time_of_day` (issue #49): every
//! one of the 24000 ticks in the day, checked against a JVM dump of 26.2's
//! real `Timelines.OVERWORLD_DAY` / `SKY_LIGHT_FACTOR` timeline track.
//!
//! # Provenance
//!
//! `tests/support/sky_light_timeline_jvm.txt` was produced by
//! `../oracle-java/SkyLightTimelineOracle.java`, which boots the real 26.2
//! registries (`VanillaRegistries.createLookup()`, exactly the pattern
//! `scripts/worldgen-oracle` uses for the density/noise registries — the
//! timeline registry is bootstrapped the same data-driven way, by
//! `Timelines::bootstrap`) and samples `Timeline.createTrackSampler` — the
//! real `KeyframeTrackSampler` + `AttributeModifier` machinery the client
//! uses — directly, once per tick. This is not a hand re-derivation of the
//! interpolation math and not this crate's own output pasted back: per
//! `CLAUDE.md`'s evidence standards, the expected values originate outside
//! the code under test.
//!
//! Regenerate after a version bump with a JDK 21+ on `PATH` (or via the
//! `docker run … eclipse-temurin:25-jdk` pattern in
//! `crates/protocol/v770/tests/hardness.rs`'s doc comment):
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/lodestone-render/oracle-java && pwd)"
//! CP="$CACHE/versions/26.2/server-26.2.jar:$(find "$CACHE/libraries" -name '*.jar' | tr '\n' ':')"
//! javac -cp "$CP" -d /tmp/sky-oracle "$HERE/SkyLightTimelineOracle.java"
//! java -cp "/tmp/sky-oracle:$CP" SkyLightTimelineOracle > tests/support/sky_light_timeline_jvm.txt
//! ```
//!
//! Only the `SKY_LIGHT_FACTOR` column (the second) is asserted by this file.
//! The dump also carries `SKY_LIGHT_COLOR` (the third column, ARGB hex) for
//! whoever picks up the tint half later — see `docs/time-of-day-lighting.md`
//! for the costed plan — but no Rust code consumes it yet, so nothing here
//! checks it.

use std::path::PathBuf;

use lodestone_render::entity::sky_darken_for_time_of_day;

fn dump_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/sky_light_timeline_jvm.txt")
}

/// `(tick, sky_light_factor)` pairs decoded from the committed JVM dump. The
/// factor column is a raw `Float.floatToRawIntBits` hex pattern, so this is a
/// lossless round-trip of the JVM's own `float`, not a re-typed decimal.
fn jvm_factor_samples() -> Vec<(i64, f32)> {
    let text = std::fs::read_to_string(dump_path())
        .expect("tests/support/sky_light_timeline_jvm.txt must be committed");
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.split_whitespace();
            let tick: i64 = parts.next().expect("tick column").parse().expect("tick is an int");
            let bits = u32::from_str_radix(parts.next().expect("factor column"), 16)
                .expect("factor column is hex");
            (tick, f32::from_bits(bits))
        })
        .collect()
}

/// Every tick of the real 26.2 day, not just the plateau endpoints. This is
/// the gate the two anchor-only unit tests in `entity.rs` structurally could
/// not be: `sky_darken_hits_vanillas_noon_and_midnight_anchors` samples noon
/// and midnight, both deep inside flat plateau segments, so a cosine curve
/// that matches both plateaus exactly (as the retired port did) passes it
/// vacuously. The defect issue #49 reports lives entirely in the dusk/dawn
/// *ramp* between those plateaus, which only a full-day scan can see.
#[test]
fn sky_light_factor_matches_the_jvm_across_the_whole_day() {
    let samples = jvm_factor_samples();
    assert_eq!(samples.len(), 24_000, "dump should cover every tick of the day");

    let mut worst: Option<(i64, f32, f32, f32)> = None;
    for (tick, vanilla) in &samples {
        let ours = sky_darken_for_time_of_day(*tick);
        let diff = (ours - vanilla).abs();
        if worst.is_none_or(|(_, _, _, d)| diff > d) {
            worst = Some((*tick, *vanilla, ours, diff));
        }
    }
    let (tick, vanilla, ours, diff) = worst.expect("dump is non-empty");
    assert!(
        diff < 1e-4,
        "worst divergence at tick {tick}: vanilla={vanilla} ours={ours} diff={diff} \
         (dump: tests/support/sky_light_timeline_jvm.txt)"
    );
}

/// Negative control, per `CLAUDE.md`'s "a control's premise can be false"
/// trap: this proves the whole-day scan above can actually *see* a wrong
/// ramp shape, rather than passing regardless of what the code does. The
/// retired 1.21-cosine port this test replaced diverges from the real
/// timeline by ~0.016 at tick 12080 (mid-dusk-ramp) — run, not asserted from
/// description, so a green scan above is evidence and not a tautology.
#[test]
fn the_whole_day_scan_would_have_caught_the_retired_cosine_port() {
    /// `sky_darken_for_time_of_day`'s implementation before issue #49: a port
    /// of 1.21's `Level.getSkyDarken` plus `LightTexture`'s `* 0.95 + 0.05`
    /// lift. Kept here only as the control's fixture, not as production code.
    fn retired_cosine_port(time_of_day: i64) -> f32 {
        let day = time_of_day.rem_euclid(24_000) as f64 / 24_000.0;
        let frac = (day - 0.25).rem_euclid(1.0);
        let eased = 0.5 - (frac * std::f64::consts::PI).cos() / 2.0;
        let celestial = ((frac * 2.0 + eased) / 3.0) as f32;
        let mut f = 1.0 - ((celestial * std::f32::consts::TAU).cos() * 2.0 + 0.2);
        f = f.clamp(0.0, 1.0);
        f = 1.0 - f;
        let darken = f * 0.8 + 0.2;
        darken * 0.95 + 0.05
    }

    let samples = jvm_factor_samples();
    let (tick, vanilla) = samples[12_080];
    assert_eq!(tick, 12_080, "dump ordering assumption");

    let old = retired_cosine_port(tick);
    assert!(
        (old - vanilla).abs() > 0.01,
        "control's premise is false: the retired cosine port should have visibly diverged \
         from the JVM here ({old} vs {vanilla}) but the gap is too small — re-check what \
         this scan can actually see before trusting it"
    );

    // And the new port must NOT reproduce that divergence.
    let ours = sky_darken_for_time_of_day(tick);
    assert!(
        (ours - vanilla).abs() < 1e-4,
        "new port diverges from the JVM at the exact tick the old one failed on: \
         ours={ours} vanilla={vanilla}"
    );
}

/// The specific numbers from `docs/time-of-day-lighting.md`'s "known
/// divergence" note, corrected: the note (and issue #49's own text) claimed
/// vanilla was "already at 0.24" at `night` (tick 13000) where the retired
/// port read `0.300`. The real JVM dump says otherwise — tick 13000 is still
/// partway down the dusk ramp (`[11270, 13140)`), not yet at the `13140`
/// plateau. This test pins the corrected number so the doc and the code
/// cannot drift apart again.
#[test]
fn night_tick_is_partway_down_the_ramp_not_at_the_plateau() {
    let samples = jvm_factor_samples();
    let (tick, vanilla) = samples[13_000];
    assert_eq!(tick, 13_000);
    assert!(
        (vanilla - 0.2969).abs() < 1e-3,
        "vanilla at tick 13000 should be ~0.2969 (partway down the ramp), got {vanilla}"
    );
    assert!(
        vanilla > 0.24 + 1e-3,
        "tick 13000 should NOT yet be at the 0.24 plateau (that starts at 13140), got {vanilla}"
    );

    let ours = sky_darken_for_time_of_day(tick);
    assert!(
        (ours - vanilla).abs() < 1e-4,
        "ours={ours} should match vanilla={vanilla} at tick 13000"
    );
}
