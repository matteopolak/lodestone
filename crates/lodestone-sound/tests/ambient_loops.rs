//! Gates for ambient loops, the mood accumulator, and client-predicted local sounds.
//!
//! Every gate predicts a **value** derived from decompiled 26.2 rather than asserting
//! a direction or an "it happened". Cadence is asserted as an exact tick count or an
//! exact number of plays, never as elapsed wall time — a count is immune to machine
//! load and a duration ratio is not.

use glam::{DVec3, IVec3, Vec3};
use lodestone_audio::JavaRandom;
use lodestone_sound::ambient::{
    AmbientAdditionsSettings, AmbientMoodSettings, AmbientSounds, AmbientLoops, LightSample,
    LoopAction, LoopFade, LOOP_SOUND_CROSS_FADE_TIME, MoodAccumulator, SKY_MOOD_RECOVERY_RATE,
};
use lodestone_sound::biome_ambient::{
    ambient_sounds_at, biome_ambient, dimension_ambient, table_len,
};
use lodestone_sound::predict::{
    ECHO_POSITION_EPSILON, ECHO_WINDOW_TICKS, MOVE_DIST_SCALE, PredictionLedger, StepAccumulator,
    swim_pitch,
};

/// Light levels for a block in total darkness — the only condition that accumulates
/// moodiness.
const PITCH_DARK: LightSample = LightSample { sky: 0, block: 0 };

// ---------------------------------------------------------------------------
// 1. The two-layer attribute lookup
// ---------------------------------------------------------------------------

/// Cave ambience lives on the **dimension**, the Nether's loops live on its
/// **biomes**, and the Nether dimension itself declares nothing. Getting this split
/// wrong fails silently in whichever direction you lean, so all three claims are
/// asserted rather than assumed.
#[test]
fn cave_ambience_comes_from_the_dimension_and_nether_biomes_override_it() {
    // The dimension layer: overworld and End carry vanilla's own legacy-cave
    // ambient-sound settings
    // (both set in vanilla's own dimension-type bootstrap).
    for dim in ["overworld", "the_end"] {
        let d = dimension_ambient(dim)
            .unwrap_or_else(|| panic!("{dim} must declare ambient sounds"));
        let mood = d
            .mood
            .as_ref()
            .unwrap_or_else(|| panic!("{dim} must carry the cave mood"));
        assert_eq!(mood.sound, "ambient.cave");
        assert_eq!(
            (mood.tick_delay, mood.block_search_extent, mood.sound_position_offset),
            (6_000, 8, 2.0),
            "{dim}: vanilla's own mood-settings record"
        );
        assert!(d.loop_sound.is_none(), "{dim} has no ambient loop");
        assert!(d.additions.is_empty(), "{dim} has no additions");
    }

    // The Nether dimension deliberately declares nothing.
    assert!(
        dimension_ambient("the_nether").is_none(),
        "the Nether dimension type sets no AMBIENT_SOUNDS attribute; its biomes do"
    );

    // The biome layer: exactly the five Nether biomes.
    assert_eq!(table_len(), 5);
    for biome in [
        "nether_wastes",
        "crimson_forest",
        "warped_forest",
        "soul_sand_valley",
        "basalt_deltas",
    ] {
        let b = biome_ambient(biome).unwrap_or_else(|| panic!("{biome} declares ambient sounds"));
        assert_eq!(
            b.loop_sound.as_deref(),
            Some(format!("ambient.{biome}.loop").as_str())
        );
        assert_eq!(b.additions.len(), 1);
    }
    // An overworld biome declares none of its own.
    assert!(biome_ambient("plains").is_none());
    assert!(biome_ambient("jungle").is_none());
}

/// Attributes **override, they do not merge**. This is the assertion that a merging
/// implementation fails: in the Nether the cave mood must be *gone*, replaced by the
/// biome's own.
#[test]
fn the_biome_attribute_replaces_the_dimensions_rather_than_merging() {
    // Overworld: the dimension's cave mood, and no loop.
    let plains = ambient_sounds_at("overworld", "plains");
    assert_eq!(plains.mood.as_ref().map(|m| m.sound.as_ref()), Some("ambient.cave"));
    assert!(plains.loop_sound.is_none());

    // Nether: the biome's loop and the biome's mood — and **not** ambient.cave.
    let wastes = ambient_sounds_at("the_nether", "nether_wastes");
    assert_eq!(
        wastes.loop_sound.as_deref(),
        Some("ambient.nether_wastes.loop")
    );
    assert_eq!(
        wastes.mood.as_ref().map(|m| m.sound.as_ref()),
        Some("ambient.nether_wastes.mood"),
        "a merging implementation would leave ambient.cave here"
    );
    assert_ne!(
        wastes.mood.as_ref().map(|m| m.sound.as_ref()),
        Some("ambient.cave")
    );
    assert_eq!(wastes.additions.len(), 1);

    // A Nether biome inside the *overworld* still overrides — the biome layer wins
    // regardless of which dimension supplied the fallback.
    let odd = ambient_sounds_at("overworld", "crimson_forest");
    assert_eq!(
        odd.mood.as_ref().map(|m| m.sound.as_ref()),
        Some("ambient.crimson_forest.mood")
    );

    // An unknown dimension and an unknown biome give EMPTY, not a panic.
    let nothing = ambient_sounds_at("custom:void", "custom:nowhere");
    assert!(nothing.is_empty());
}

// ---------------------------------------------------------------------------
// 2. The mood accumulator — the trigger condition
// ---------------------------------------------------------------------------

/// **The headline cadence gate.** Cave ambience is not "Y below sea level"; it is a
/// moodiness accumulator that needs `tick_delay` consecutive *fully dark* samples.
///
/// The exact value is predicted: at block light 0 the per-tick increment is
/// `-(0 - 1) / 6000 = +1/6000`, so the sound fires after ~6000 ticks — five minutes.
/// A test that only checked "it eventually fires" would pass for an implementation off
/// by a factor of ten.
///
/// # It is 6001 ticks, not 6000, and the reason is not the one it looks like
///
/// In exact arithmetic 6000 increments of `1/6000` reach exactly `1.0` and the sound
/// fires on tick 6000. In binary floating point they do not, because `1/6000` is not
/// representable and rounds **down**, so repeated addition always undershoots and
/// needs one extra step.
///
/// The obvious guess is that this is an `f32` artifact. It is not, and that guess was
/// checked rather than assumed: an experiment converting the accumulator to `f64`
/// storage **also fires at 6001**. Measured both ways —
///
/// | accumulator | value after 6000 ticks | crosses 1.0 at |
/// |---|---|---|
/// | `f32` | `0.9999486` | 6001 |
/// | `f64` | `0.9999999999999232` | 6001 |
///
/// — so the `+1` is a property of accumulating a rounded-down step in *any* binary
/// float, and this gate is correctly insensitive to storage precision. There is
/// therefore **no control that flips it by changing precision**, and it would be
/// dishonest to imply one: what evidences the `+1` is the direct computation below,
/// which this test performs inline for both widths.
///
/// Vanilla lands on 6001 too — `moodiness` is a Java `float` and the expression is the
/// same `(float)(brightness - 1) / tickDelay` — so 6001 is the behaviour to match and
/// the naive 6000 is the idealisation.
#[test]
fn the_mood_sound_needs_exactly_tick_delay_dark_ticks() {
    let mood = AmbientMoodSettings::LEGACY_CAVE;
    assert_eq!(mood.tick_delay, 6_000);

    let mut acc = MoodAccumulator::new();
    let mut rng = JavaRandom::new(42);
    let eye = DVec3::new(0.5, 64.0, 0.5);

    let mut fired_at = None;
    for tick in 1..=8_000 {
        let play = acc.tick(&mood, eye, &mut rng, &mut |_| PITCH_DARK);
        if play.is_some() {
            fired_at = Some(tick);
            break;
        }
    }
    assert_eq!(
        fired_at,
        Some(mood.tick_delay + 1),
        "the mood sound must fire on the {}th fully-dark tick (f32 accumulation of \
         1/{} crosses 1.0 one tick late, exactly as vanilla's Java float does)",
        mood.tick_delay + 1,
        mood.tick_delay
    );
    // Evidence the +1 by direct computation rather than prose, in BOTH widths — this
    // is what shows the extra tick is inherent to the rounded-down step and not a
    // consequence of choosing f32.
    let mut m32 = 0.0f32;
    let step32 = -1.0f32 / 6_000.0f32;
    for _ in 0..6_000 {
        m32 -= step32;
    }
    let mut m64 = 0.0f64;
    let step64 = -1.0f64 / 6_000.0f64;
    for _ in 0..6_000 {
        m64 -= step64;
    }
    assert!(
        m32 < 1.0 && m32 > 0.9999,
        "f32 must fall just short at 6000 ticks, got {m32}"
    );
    assert!(
        m64 < 1.0 && m64 > 0.999_999_999,
        "f64 must ALSO fall just short at 6000 ticks — the +1 is not an f32 artifact — \
         got {m64}"
    );

    // And it resets, so the second one takes the same again rather than firing every
    // tick from then on.
    assert_eq!(acc.moodiness(), 0.0);
}

/// The sign flip in the block-light term, which is the part most likely to be
/// mis-transcribed. `-(block - 1) / tick_delay` is **positive** at light 0,
/// **exactly zero** at light 1, and **negative** above it.
#[test]
fn block_light_one_is_the_break_even_point_and_brighter_drains() {
    let mood = AmbientMoodSettings::LEGACY_CAVE;
    let eye = DVec3::new(0.5, 64.0, 0.5);

    // Light 0: accumulates 1/6000 per tick.
    let mut acc = MoodAccumulator::new();
    let mut rng = JavaRandom::new(1);
    acc.tick(&mood, eye, &mut rng, &mut |_| PITCH_DARK);
    let expected = 1.0 / 6_000.0;
    assert!(
        (acc.moodiness() - expected).abs() < 1e-9,
        "expected {expected}, got {}",
        acc.moodiness()
    );

    // Light 1: exactly zero change. A torch's own block is break-even.
    let mut acc = MoodAccumulator::new();
    acc.set_moodiness(0.5);
    let mut rng = JavaRandom::new(1);
    acc.tick(&mood, eye, &mut rng, &mut |_| LightSample { sky: 0, block: 1 });
    assert_eq!(acc.moodiness(), 0.5, "block light 1 must be break-even");

    // Light 8: drains by 7/6000 per tick.
    let mut acc = MoodAccumulator::new();
    acc.set_moodiness(0.5);
    let mut rng = JavaRandom::new(1);
    acc.tick(&mood, eye, &mut rng, &mut |_| LightSample { sky: 0, block: 8 });
    let expected = 0.5 - 7.0 / 6_000.0;
    assert!(
        (acc.moodiness() - expected).abs() < 1e-6,
        "bright block light must drain moodiness: expected {expected}, got {}",
        acc.moodiness()
    );

    // A lit room never fires, no matter how long you stand in it. This is the
    // assertion that fails for a depth-based implementation.
    let mut acc = MoodAccumulator::new();
    let mut rng = JavaRandom::new(9);
    let deep = DVec3::new(0.5, -40.0, 0.5);
    for _ in 0..60_000 {
        assert!(
            acc.tick(&mood, deep, &mut rng, &mut |_| LightSample { sky: 0, block: 15 })
                .is_none(),
            "a lit space must never produce cave ambience, even at Y=-40"
        );
    }
    assert_eq!(
        acc.moodiness(),
        0.0,
        "moodiness is floored at zero, not banked negative"
    );
}

/// Sky light drains at a different, much slower rate:
/// `sky / 15 * 0.001` (vanilla's own biome-ambient-sounds tick routine). At full sky light that is
/// exactly `0.001` per tick — 15x faster than the darkness accumulation, so stepping
/// into daylight discards ~5 minutes of accumulated mood in ~20 seconds.
#[test]
fn sky_light_drains_at_the_recovery_rate() {
    let mood = AmbientMoodSettings::LEGACY_CAVE;
    let eye = DVec3::new(0.5, 64.0, 0.5);
    let mut acc = MoodAccumulator::new();
    acc.set_moodiness(0.5);
    let mut rng = JavaRandom::new(3);

    acc.tick(&mood, eye, &mut rng, &mut |_| LightSample { sky: 15, block: 0 });
    let expected = 0.5 - 15.0 / 15.0 * SKY_MOOD_RECOVERY_RATE;
    assert!((acc.moodiness() - expected).abs() < 1e-6);
    assert_eq!(SKY_MOOD_RECOVERY_RATE, 0.001);

    // Note the sky branch wins outright: a fully *dark* block that still sees sky
    // light drains rather than accumulating. That is the `if sky > 0` ordering, and
    // reversing it would make every night-time surface tick accumulate.
    let mut acc = MoodAccumulator::new();
    acc.set_moodiness(0.5);
    let mut rng = JavaRandom::new(3);
    acc.tick(&mood, eye, &mut rng, &mut |_| LightSample { sky: 1, block: 0 });
    assert!(
        acc.moodiness() < 0.5,
        "any sky light must drain, not accumulate"
    );
}

/// The sampled position is uniform over a `(2 * extent + 1)³` cube around the
/// **eye**, and the sound is pushed `offset` blocks *past* the sampled block.
#[test]
fn the_sample_cube_and_the_position_offset_match_the_jar() {
    let mood = AmbientMoodSettings::LEGACY_CAVE;
    assert_eq!(mood.search_span(), 17, "extent 8 -> 17³ cube");

    let eye = DVec3::new(100.5, 64.0, -50.5);
    let mut rng = JavaRandom::new(0xBEEF);
    let mut acc = MoodAccumulator::new();
    let mut sampled = Vec::new();

    // Collect the samples over many ticks and check the cube's extent exactly.
    for _ in 0..20_000 {
        acc.tick(&mood, eye, &mut rng, &mut |p| {
            sampled.push(p);
            LightSample { sky: 0, block: 15 } // never fires, so the run is uniform
        });
    }
    let lo = IVec3::new(
        sampled.iter().map(|p| p.x).min().unwrap(),
        sampled.iter().map(|p| p.y).min().unwrap(),
        sampled.iter().map(|p| p.z).min().unwrap(),
    );
    let hi = IVec3::new(
        sampled.iter().map(|p| p.x).max().unwrap(),
        sampled.iter().map(|p| p.y).max().unwrap(),
        sampled.iter().map(|p| p.z).max().unwrap(),
    );
    // floor(100.5 + (0..=16) - 8) = 92..=108, and 17 distinct values.
    assert_eq!((lo.x, hi.x), (92, 108));
    assert_eq!((lo.y, hi.y), (56, 72));
    // floor(-50.5 + (0..=16) - 8) = -59..=-43.
    assert_eq!((lo.z, hi.z), (-59, -43));

    // Now the position: force an immediate fire and check the offset is applied
    // *beyond* the block, not to it.
    let mut acc = MoodAccumulator::new();
    acc.set_moodiness(1.0);
    let mut rng = JavaRandom::new(7);
    let mut where_sampled = None;
    let play = acc
        .tick(&mood, eye, &mut rng, &mut |p| {
            where_sampled = Some(p);
            PITCH_DARK
        })
        .expect("moodiness was already at 1.0");
    let block = where_sampled.unwrap();
    let centre = DVec3::new(
        f64::from(block.x) + 0.5,
        f64::from(block.y) + 0.5,
        f64::from(block.z) + 0.5,
    );
    let block_distance = (centre - eye).length();
    let sound_distance = (play.position - eye).length();
    assert!(
        (sound_distance - (block_distance + mood.sound_position_offset)).abs() < 1e-6,
        "the sound must sit offset ({}) blocks beyond the sampled block: block at {block_distance}, \
         sound at {sound_distance}",
        mood.sound_position_offset
    );
    // ...and along the same direction.
    let to_block = (centre - eye).normalize();
    let to_sound = (play.position - eye).normalize();
    assert!(to_block.distance(to_sound) < 1e-9, "direction must be preserved");
    assert_eq!(play.sound, "ambient.cave");
}

// ---------------------------------------------------------------------------
// 3. Additions cadence
// ---------------------------------------------------------------------------

/// Additions fire on `nextDouble() < tick_chance` at exactly `0.0111`, i.e. about once
/// every 90 ticks. Asserted as a **count over a fixed number of draws** against the
/// predicted expectation — a "fires sometimes" test passes for `0.111` too, which is a
/// 10x cadence error and unmistakable in play.
#[test]
fn additions_fire_at_the_declared_tick_chance() {
    let additions = &biome_ambient("nether_wastes").unwrap().additions;
    assert_eq!(additions.len(), 1);
    let a = &additions[0];
    assert_eq!(a.tick_chance, 0.0111);
    assert_eq!(a.sound, "ambient.nether_wastes.additions");

    const TRIALS: u32 = 1_000_000;
    let mut rng = JavaRandom::new(0x5EED);
    let fires = (0..TRIALS).filter(|_| a.fires(&mut rng)).count();

    // Expectation is TRIALS * 0.0111 = 11100. A seeded stream makes this
    // deterministic rather than probabilistic, so a tight band is safe; 3% excludes
    // both a 10x error and an off-by-one in the comparison.
    let expected = (f64::from(TRIALS) * a.tick_chance) as usize;
    let tolerance = expected / 33;
    assert!(
        fires.abs_diff(expected) <= tolerance,
        "fired {fires} times in {TRIALS} draws; expected ~{expected} (+/-{tolerance}) \
         for tick_chance {}",
        a.tick_chance
    );
    // And the two wrong hypotheses are far outside that band.
    assert!(fires < expected * 5, "a 10x-too-high chance would fire ~111000 times");
    assert!(fires > expected / 5, "a 10x-too-low chance would fire ~1110 times");

    // Strictly `<`, so a zero chance never fires even though next_f64 can return 0.0.
    let never = AmbientAdditionsSettings::of("x", 0.0);
    let mut rng = JavaRandom::new(1);
    assert_eq!((0..100_000).filter(|_| never.fires(&mut rng)).count(), 0);
}

// ---------------------------------------------------------------------------
// 4. Loop crossfade
// ---------------------------------------------------------------------------

/// Vanilla's own loop-sound-instance type: a 40-tick linear
/// fade, and the stop check happens **before** the counter moves.
#[test]
fn a_loop_fades_in_over_exactly_the_crossfade_time() {
    assert_eq!(LOOP_SOUND_CROSS_FADE_TIME, 40);

    let mut fade = LoopFade::new();
    fade.fade_in();
    let mut reached_full = None;
    for tick in 1..=100 {
        let v = fade.tick().expect("a fading-in loop must not stop");
        if v >= 1.0 && reached_full.is_none() {
            reached_full = Some(tick);
        }
    }
    assert_eq!(
        reached_full,
        Some(LOOP_SOUND_CROSS_FADE_TIME),
        "full volume must be reached on tick 40, not 39 or 41"
    );

    // Fading out from full takes 40 ticks to reach zero, then one extra tick at
    // negative fade before stopping (vanilla's own loop-sound-instance tick routine checks before
    // incrementing). That extra tick is why the check order matters: reversing it
    // clips the loop.
    let mut fade = LoopFade::new();
    fade.fade_in();
    for _ in 0..LOOP_SOUND_CROSS_FADE_TIME {
        fade.tick();
    }
    assert_eq!(fade.volume(), 1.0);
    fade.fade_out();
    let mut stopped_at = None;
    for tick in 1..=100 {
        if fade.tick().is_none() {
            stopped_at = Some(tick);
            break;
        }
    }
    assert_eq!(
        stopped_at,
        Some(LOOP_SOUND_CROSS_FADE_TIME + 2),
        "a full loop stops 42 ticks after fade_out: 40 down to 0, one to -1, one to notice"
    );
}

/// Walking between two loop biomes must **crossfade**, keeping both voices live, not
/// cut the first off. This is the assertion a single-slot implementation fails.
#[test]
fn stepping_between_two_loop_biomes_crossfades_rather_than_cutting() {
    let mut loops = AmbientLoops::new();

    // Long enough in the crimson forest for its loop to reach *full* volume — the
    // fade counter must actually be at 40, or the fade-out below is shorter and the
    // tick count means something else. (An earlier version of this gate ran only 10
    // ticks and then asserted the 42-tick figure; it measured 12, which is correct for
    // a fade of 10. The lesson: the crossfade duration depends on the fade level
    // reached, not on the constant.)
    let mut started = Vec::new();
    for _ in 0..LOOP_SOUND_CROSS_FADE_TIME {
        for action in loops.tick(Some("ambient.crimson_forest.loop")) {
            if let LoopAction::Start(s) = action {
                started.push(s.to_string());
            }
        }
    }
    assert_eq!(started, vec!["ambient.crimson_forest.loop".to_string()]);
    assert_eq!(loops.len(), 1);
    assert!(
        (loops.live().next().unwrap().1 - 1.0).abs() < 1e-6,
        "the first loop must be at full volume before the crossover"
    );

    // Step into the warped forest. The new loop starts; the old one must still be
    // live and fading, not stopped.
    let actions = loops.tick(Some("ambient.warped_forest.loop"));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, LoopAction::Start(s) if s == "ambient.warped_forest.loop")),
        "the new loop must start"
    );
    assert!(
        !actions.iter().any(|a| matches!(a, LoopAction::Stop(_))),
        "the old loop must not be stopped on the crossover tick: {actions:?}"
    );
    assert_eq!(loops.len(), 2, "both loops must be live during the crossfade");

    // The old one fades down while the new one fades up.
    let mut ticks = 1;
    while loops.len() == 2 && ticks < 200 {
        loops.tick(Some("ambient.warped_forest.loop"));
        ticks += 1;
    }
    assert_eq!(loops.len(), 1, "the old loop must eventually stop");
    // It had reached full volume (fade 40), so fade_out takes 42 ticks.
    assert_eq!(
        ticks,
        LOOP_SOUND_CROSS_FADE_TIME + 2,
        "the old loop should stop {} ticks after the change",
        LOOP_SOUND_CROSS_FADE_TIME + 2
    );
    let live: Vec<String> = loops.live().map(|(n, _)| n.to_string()).collect();
    assert_eq!(live, vec!["ambient.warped_forest.loop".to_string()]);

    // A steady biome must not restart or re-fade its loop — that is the check
    // vanilla's own current/previous equality check in its own biome-ambient-sounds
    // tick routine exists for.
    for _ in 0..500 {
        let actions = loops.tick(Some("ambient.warped_forest.loop"));
        assert!(
            !actions.iter().any(|a| matches!(a, LoopAction::Start(_) | LoopAction::Stop(_))),
            "a steady biome must produce only volume updates: {actions:?}"
        );
    }
    assert_eq!(loops.len(), 1);
    assert!((loops.live().next().unwrap().1 - 1.0).abs() < 1e-6);
}

/// Leaving a loop biome for one with no loop fades out and stops, and starts nothing.
#[test]
fn leaving_a_loop_biome_for_a_loopless_one_fades_out_to_nothing() {
    let mut loops = AmbientLoops::new();
    for _ in 0..LOOP_SOUND_CROSS_FADE_TIME {
        loops.tick(Some("ambient.nether_wastes.loop"));
    }
    assert_eq!(loops.len(), 1);

    // The overworld has no ambient loop at all (only a mood).
    let overworld = AmbientSounds::legacy_cave();
    assert!(overworld.loop_sound.is_none());

    let mut ticks = 0;
    while !loops.is_empty() && ticks < 200 {
        let actions = loops.tick(None);
        assert!(
            !actions.iter().any(|a| matches!(a, LoopAction::Start(_))),
            "nothing should start"
        );
        ticks += 1;
    }
    assert_eq!(ticks, LOOP_SOUND_CROSS_FADE_TIME + 2);
    assert!(loops.is_empty());
}

// ---------------------------------------------------------------------------
// 5. Client prediction: the step cadence
// ---------------------------------------------------------------------------

/// Footsteps are spaced by **distance**, not time. The scale is `0.6`
/// (vanilla's own movement-emission-and-play-sound routine) against a threshold starting at `1.0`
/// (vanilla's own next-step field initializer), so the first
/// step lands after `1 / 0.6 = 1.667` blocks and subsequent ones on each further
/// integer of accumulated scaled distance.
#[test]
fn footsteps_are_spaced_by_distance_at_the_jars_scale() {
    assert_eq!(MOVE_DIST_SCALE, 0.6);

    // Walk in a straight line 0.1 blocks per tick and count the steps over 100
    // blocks. Predicted: scaled distance is 100 * 0.6 = 60, thresholds are crossed at
    // each integer from 1 to 60, so exactly 60 steps.
    let mut acc = StepAccumulator::new();
    let mut steps = 0;
    for _ in 0..1_000 {
        if acc.advance(DVec3::new(0.1, 0.0, 0.0), false, false) {
            steps += 1;
            acc.consume();
        }
    }
    assert_eq!(
        steps, 60,
        "100 blocks at scale 0.6 must produce exactly 60 steps, i.e. one every 1.667 blocks"
    );

    // The first step specifically: 1.0 / 0.6 = 1.6667 blocks, so at 0.1/tick it is
    // tick 17 (1.7 blocks scaled to 1.02 > 1.0), not tick 10 or 16.
    let mut acc = StepAccumulator::new();
    let mut first = None;
    for tick in 1..=100 {
        if acc.advance(DVec3::new(0.1, 0.0, 0.0), false, false) {
            first = Some(tick);
            break;
        }
    }
    assert_eq!(first, Some(17), "1/0.6 = 1.667 blocks -> tick 17 at 0.1/tick");

    // Vertical movement alone produces no steps, because the horizontal component is
    // what accumulates (vanilla's own movement-emission-and-play-sound routine).
    let mut acc = StepAccumulator::new();
    for _ in 0..1_000 {
        assert!(
            !acc.advance(DVec3::new(0.0, -0.5, 0.0), false, false),
            "falling must not produce footsteps"
        );
    }
    assert_eq!(acc.move_dist(), 0.0);

    // ...unless climbing, which uses the full 3D length (same method).
    let mut acc = StepAccumulator::new();
    let mut climbed = 0;
    for _ in 0..1_000 {
        if acc.advance(DVec3::new(0.0, 0.1, 0.0), true, false) {
            climbed += 1;
            acc.consume();
        }
    }
    assert_eq!(climbed, 60, "climbing a ladder does step, on the 3D length");

    // Air underfoot suppresses the step entirely (the same method's
    // `!supportingState.isAir()` guard) even though distance still
    // accumulates — so the step fires as soon as you land, rather than being lost.
    let mut acc = StepAccumulator::new();
    for _ in 0..100 {
        assert!(!acc.advance(DVec3::new(0.1, 0.0, 0.0), false, true));
    }
    assert!(acc.move_dist() > 1.0, "distance still accumulates in the air");
    assert!(
        acc.advance(DVec3::ZERO, false, false),
        "the pending step fires on landing"
    );
}

/// Vanilla's own next-step routine re-arms to `(int)move_dist + 1` — the next
/// integer boundary, **not** `move_dist + 1`. The difference is drift: with `+ 1`
/// each step's overshoot accumulates and the spacing slowly grows.
#[test]
fn the_step_threshold_rearms_to_the_next_integer_not_to_plus_one() {
    let mut acc = StepAccumulator::new();
    // One big move: 5 blocks -> scaled 3.0, crossing the 1.0 threshold well past it.
    assert!(acc.advance(DVec3::new(5.0, 0.0, 0.0), false, false));
    assert!((acc.move_dist() - 3.0).abs() < 1e-6);
    acc.consume();
    // (int)3.0 + 1 == 4.0. A `move_dist + 1` implementation would give 4.0 here too,
    // so use a non-integer distance to separate them.
    assert_eq!(acc.next_step(), 4.0);

    let mut acc = StepAccumulator::new();
    // 2.9 blocks -> scaled 1.74.
    assert!(acc.advance(DVec3::new(2.9, 0.0, 0.0), false, false));
    acc.consume();
    // (int)1.74 + 1 = 2.0. The wrong hypothesis gives 1.74 + 1 = 2.74.
    assert_eq!(
        acc.next_step(), 2.0,
        "must snap to the next integer, not add one to the overshoot"
    );
}

/// A crossing that produced no sound leaves the threshold armed
/// (vanilla's own movement-emission-and-play-sound routine only re-arms on `producedSideEffects`), which is why
/// `advance` and `consume` are separate.
#[test]
fn a_silent_crossing_leaves_the_threshold_armed() {
    let mut acc = StepAccumulator::new();
    let before = acc.next_step();
    assert!(acc.advance(DVec3::new(2.0, 0.0, 0.0), false, false));
    // Caller decides not to play (e.g. the block made no sound) and does not consume.
    assert_eq!(acc.next_step(), before, "threshold must remain armed");
    // So the very next movement crosses again.
    assert!(acc.advance(DVec3::new(0.01, 0.0, 0.0), false, false));
}

/// The swim pitch draws `nextFloat()` **twice**, in vanilla's own play-swim-sound routine, giving a
/// triangular distribution about 1.0 bounded by `0.6..=1.4` — not the uniform
/// distribution a single draw would give.
#[test]
fn the_swim_pitch_is_triangular_about_one() {
    let mut rng = JavaRandom::new(0xC0FFEE);
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    let mut near_centre = 0;
    const N: u32 = 200_000;
    for _ in 0..N {
        let p = swim_pitch(&mut rng);
        lo = lo.min(p);
        hi = hi.max(p);
        // Half the range, centred: |p - 1| < 0.2.
        if (p - 1.0).abs() < 0.2 {
            near_centre += 1;
        }
    }
    assert!(lo > 0.6 && lo < 0.65, "lower bound should approach 0.6, got {lo}");
    assert!(hi < 1.4 && hi > 1.35, "upper bound should approach 1.4, got {hi}");
    // For a triangular distribution the central half of the range holds 75% of the
    // mass; a uniform one would hold 50%. That is the predictive discriminator.
    let fraction = f64::from(near_centre) / f64::from(N);
    assert!(
        (fraction - 0.75).abs() < 0.01,
        "central half held {fraction:.4} of draws; triangular predicts 0.75, \
         uniform (one draw) would predict 0.50"
    );
}

// ---------------------------------------------------------------------------
// 6. Client prediction: the double-play control
// ---------------------------------------------------------------------------

/// **The double-play gate.** A predicted sound plus the server's subsequent echo must
/// produce exactly **one** sound.
///
/// And the control that actually matters, which a "no double-play" assertion alone
/// cannot see: a de-duplicator that suppresses *everything* passes that assertion
/// perfectly. So this also asserts an unrelated server sound is **not** suppressed,
/// that a *second* real sound of the same name is not swallowed by one stale
/// prediction, and that a far-away same-named sound survives.
#[test]
fn a_prediction_plus_its_server_echo_produces_exactly_one_sound() {
    let mut ledger = PredictionLedger::new();
    let pos = Vec3::new(10.0, 64.0, -3.0);

    // We predict our own footstep at tick 100 and play it locally: that is sound #1.
    ledger.record("block.stone.step", pos, 100);
    assert_eq!(ledger.pending(), 1);

    // The server echoes it two ticks later, at a slightly rounded position.
    let echoed = pos + Vec3::new(0.01, 0.0, -0.02);
    assert!(
        ledger.should_suppress("block.stone.step", echoed, 102),
        "the echo of our own predicted sound must be suppressed"
    );
    // Total sounds played: 1. And the entry is consumed.
    assert_eq!(ledger.pending(), 0);

    // --- controls ---

    // (a) A *different* event at the same place is not suppressed.
    ledger.record("block.stone.step", pos, 200);
    assert!(
        !ledger.should_suppress("block.grass.step", pos, 201),
        "a different sound must not be swallowed"
    );
    assert!(
        !ledger.should_suppress("entity.creeper.primed", pos, 201),
        "an unrelated sound must not be swallowed"
    );
    // The prediction is still pending, so nothing was consumed by the misses.
    assert_eq!(ledger.pending(), 1);

    // (b) One prediction suppresses exactly one echo. A second genuine sound of the
    // same name gets through — this is what stops a burst of footsteps collapsing.
    assert!(ledger.should_suppress("block.stone.step", pos, 201));
    assert!(
        !ledger.should_suppress("block.stone.step", pos, 202),
        "one prediction must suppress only one echo"
    );

    // (c) Distance discriminates: a same-named sound from another player across the
    // room must be heard.
    ledger.record("block.stone.step", pos, 300);
    let far = pos + Vec3::new(ECHO_POSITION_EPSILON + 1.0, 0.0, 0.0);
    assert!(
        !ledger.should_suppress("block.stone.step", far, 300),
        "a distant same-named sound is someone else's and must play"
    );

    // (d) The window expires, so a stale prediction cannot swallow a sound minutes
    // later.
    ledger.record("block.stone.step", pos, 400);
    assert!(
        !ledger.should_suppress("block.stone.step", pos, 400 + ECHO_WINDOW_TICKS + 1),
        "a prediction older than the window must not suppress"
    );
    assert_eq!(
        ledger.pending(),
        0,
        "expired predictions must be pruned, not retained"
    );
}

/// The ledger must not grow without bound while the player walks, which it would if
/// pruning happened only on match.
#[test]
fn the_ledger_stays_bounded_over_a_long_walk() {
    let mut ledger = PredictionLedger::new();
    for tick in 0..100_000u64 {
        ledger.record("block.stone.step", Vec3::new(tick as f32, 64.0, 0.0), tick);
        assert!(
            ledger.pending() as u64 <= ECHO_WINDOW_TICKS + 1,
            "ledger grew to {} entries at tick {tick}",
            ledger.pending()
        );
    }
    // And an explicit prune with no records still clears it.
    ledger.prune(200_000);
    assert_eq!(ledger.pending(), 0);
}

// ---------------------------------------------------------------------------
// 7. Structural: no device, no clock
// ---------------------------------------------------------------------------

/// The same structural guard the music modules carry: nothing in the ambient or
/// prediction layer may reach an output device or the wall clock, so no test here can
/// make a sound on the owner's machine. Negative control: add `AudioEngine` to any of
/// these files and this fails.
#[test]
fn the_ambient_module_cannot_reach_a_device_or_a_clock() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden = [
        ("AudioEngine", "would let a tick open the real output device"),
        ("CpalSink", "the device sink belongs to the caller"),
        ("cpal", "no audio backend in the selection layer"),
        ("Instant", "cadence is in ticks, never wall time"),
        ("SystemTime", "cadence is in ticks, never wall time"),
        ("std::fs", "no filesystem: assets are the resolver's job"),
        ("Command::new", "no process spawning, ever"),
    ];

    let files = [
        "src/ambient.rs",
        "src/biome_ambient.rs",
        "src/predict.rs",
    ];
    let mut scanned = 0usize;
    for rel in files {
        let src =
            std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        scanned += 1;
        for line in src.lines() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for (token, why) in forbidden {
                assert!(
                    !code.contains(token),
                    "{rel} references `{token}` in code: {why}\n  {line}"
                );
            }
        }
    }
    assert_eq!(scanned, files.len());

    // The positive half: the scanner sees real code, so a clean pass is not the
    // result of reading nothing.
    let src = std::fs::read_to_string(root.join("src/ambient.rs")).unwrap();
    assert!(
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains("fn tick")),
        "the scanner found no code in ambient.rs — it is reading the wrong thing"
    );
}
