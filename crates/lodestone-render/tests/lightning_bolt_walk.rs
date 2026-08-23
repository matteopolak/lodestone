//! The lightning bolt's random walk, against `LightningBoltRenderer.submit`'s
//! own structure.
//!
//! # What these gates can and cannot be
//!
//! There is no captured-bytes oracle available for a bolt and there never can
//! be: `LightningBolt.seed` is a plain unsynched field rolled by
//! `random.nextLong()` on each side independently, so **nothing about a bolt's
//! shape is on the wire** and no vanilla server will ever tell us what one
//! looked like. A JVM oracle could reproduce the LCG, but every assertion here
//! is instead a **structural invariant derived from the algorithm** — a
//! property that must hold for any correct transcription and fails for the
//! specific mistakes this port could make.
//!
//! That is a deliberate choice rather than a shortcut, and the strongest of
//! them is `the_trunk_lands_on_the_entity_origin`: the whole purpose of the
//! anchor pre-pass and its `- final_x` subtraction is to put the bottom of the
//! trunk on the strike point, and that only comes out right if the geometry
//! loop re-seeds from the same seed and consumes the same draws in the same
//! order. A bolt built with a differently-seeded or differently-ordered RNG
//! lands somewhere else entirely.
//!
//! These say nothing about whether a bolt reaches pixels — that is
//! `crates/lodestone-shell/tests/lightning_bolt_pixels.rs`.

use lodestone_render::lightning_bolt::{BOLT_COLOR, BOLT_VERTICES, lightning_bolt_vertices};

/// A seed with no special structure. Two of them, because a single seed can
/// coincide with an invariant by luck — the trunk landing on the origin is
/// exactly the kind of claim one lucky walk could satisfy.
const SEEDS: [i64; 2] = [0x0BAD_F00D_DEAD_BEEFu64 as i64, -8_123_456_789];

/// Four shells, fourteen segments, four faces, six vertices — and the segment
/// count is `8 + 3 + 3`, not `8 * 3`.
#[test]
fn a_bolt_emits_exactly_the_documented_vertex_count() {
    for seed in SEEDS {
        let verts = lightning_bolt_vertices(seed);
        assert_eq!(
            verts.len(),
            BOLT_VERTICES,
            "seed {seed}: 4 shells x (8 + 3 + 3) segments x 4 faces x 6 vertices"
        );
        assert_eq!(BOLT_VERTICES, 1344, "the constant itself drifted");
        assert!(
            verts.iter().all(|v| v.color == BOLT_COLOR),
            "every vertex of every quad carries the same flat colour"
        );
    }
}

/// The bottom of the trunk sits on the entity's own origin.
///
/// This is the anchor pre-pass's entire reason to exist: it walks eight levels
/// downward, keeps the total as `final_x`/`final_z`, and the geometry loop then
/// subtracts it. Because the loop **re-seeds from the same seed**, the trunk
/// retraces those same eight steps, so the eighth one cancels the subtraction
/// exactly and the lowest vertices land at `x = z = 0` plus their own half
/// width.
///
/// So the lowest ring's x extent must be symmetric about zero. It is symmetric
/// for *any* correct transcription and asymmetric for a walk whose RNG was
/// re-seeded differently, consumed draws in another order, or dropped the
/// subtraction — which are the three ways this port could plausibly be wrong.
///
/// Only the trunk reaches `y = 0`: the two branches stop at `h = 4` and `h = 3`
/// (`y = 64` and `y = 48`), so the lowest ring needs no filtering.
#[test]
fn the_trunk_lands_on_the_entity_origin() {
    for seed in SEEDS {
        let verts = lightning_bolt_vertices(seed);
        let lowest: Vec<_> = verts.iter().filter(|v| v.position[1] == 0.0).collect();
        assert!(
            !lowest.is_empty(),
            "seed {seed}: no vertex at y = 0 — the trunk never reached the strike point"
        );
        for axis in [0usize, 2] {
            let min = lowest
                .iter()
                .map(|v| v.position[axis])
                .fold(f32::INFINITY, f32::min);
            let max = lowest
                .iter()
                .map(|v| v.position[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                (min + max).abs() < 1.0e-4,
                "seed {seed}, axis {axis}: the lowest ring spans {min}..{max}, which is not \
                 symmetric about the origin. The anchor subtraction and the geometry loop's \
                 re-seeding are what put the strike point at the entity's own position; a \
                 mismatch here means one of them is wrong."
            );
        }
    }
}

/// The bolt is 128 blocks tall, and the whole of it is above the strike point.
///
/// A sanity check on the scale, which is the fact about this renderer most
/// likely to be "corrected" by someone assuming model-space texels: `h * 16` is
/// in **blocks**, not sixteenths of one.
#[test]
fn a_bolt_spans_a_hundred_and_twenty_eight_blocks_upward() {
    for seed in SEEDS {
        let verts = lightning_bolt_vertices(seed);
        let min_y = verts
            .iter()
            .map(|v| v.position[1])
            .fold(f32::INFINITY, f32::min);
        let max_y = verts
            .iter()
            .map(|v| v.position[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(min_y, 0.0, "seed {seed}: the walk must reach the strike point");
        assert_eq!(
            max_y, 128.0,
            "seed {seed}: eight levels of 16 blocks each. A bolt in texels would top out \
             at 8.0 here."
        );
    }
}

/// The trunk's half-width at the top and bottom rings equals the value the
/// vanilla constants predict, and **not** the value the swapped-pair
/// hypothesis predicts.
///
/// # Why this is not a direction check
///
/// The obvious gate is "the top is wider than the bottom", and it was written
/// that way first. It was then measured passing under a deliberate swap of
/// `rr1`/`rr2` in the producer, because the bolt still tapers downward under
/// the wrong pairing — just by different amounts. Direction is a property both
/// hypotheses have; only the magnitudes separate them.
///
/// # The predicted values
///
/// The outermost shell is `r = 3`, so its base half-width is
/// `0.1 + 3 * 0.2 = 0.7`. It alone sets the extent of a ring, since the inner
/// three are narrower and concentric with it.
///
/// * The **top** ring, `y = 128`, is fed only by segment `h = 7`'s upper
///   vertices, which carry `rr1 = base * (h * 0.1 + 1) = 0.7 * 1.7 = 1.19`.
/// * The **bottom** ring, `y = 0`, is fed only by segment `h = 0`'s lower
///   vertices, which carry `rr2 = base * ((h - 1) * 0.1 + 1) = 0.7 * 0.9 = 0.63`.
///
/// Under the swapped pairing those become `0.7 * 1.6 = 1.12` and
/// `0.7 * 1.0 = 0.70` — both still "top wider than bottom", both rejected here.
///
/// The measurement is offset-independent by construction: a ring's four faces
/// put corners at `±rr` about the walk's own centre at that height, so
/// `(max - min) / 2` is the half-width whatever the lateral drift.
#[test]
fn the_trunk_taper_matches_the_predicted_half_widths() {
    /// `0.1 + r * 0.2` at the outermost shell.
    const OUTER_BASE: f32 = 0.7;
    /// `h * 0.1 + 1.0` at `h = 7`.
    const TOP_TAPER: f32 = 1.7;
    /// `(h - 1.0) * 0.1 + 1.0` at `h = 0`.
    const BOTTOM_TAPER: f32 = 0.9;
    /// What the swapped pairing would give instead, at the same two rings.
    const SWAPPED_TOP: f32 = OUTER_BASE * 1.6;
    const SWAPPED_BOTTOM: f32 = OUTER_BASE * 1.0;

    for seed in SEEDS {
        let verts = lightning_bolt_vertices(seed);
        let ring_half_width = |y: f32| -> f32 {
            let xs: Vec<f32> = verts
                .iter()
                .filter(|v| v.position[1] == y)
                .map(|v| v.position[0])
                .collect();
            assert!(!xs.is_empty(), "seed {seed}: no ring at y = {y}");
            let min = xs.iter().copied().fold(f32::INFINITY, f32::min);
            let max = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            (max - min) / 2.0
        };

        for (label, y, expected, wrong) in [
            ("top", 128.0f32, OUTER_BASE * TOP_TAPER, SWAPPED_TOP),
            ("bottom", 0.0f32, OUTER_BASE * BOTTOM_TAPER, SWAPPED_BOTTOM),
        ] {
            let measured = ring_half_width(y);
            assert!(
                (measured - expected).abs() < 1.0e-4,
                "seed {seed}: the {label} ring measures {measured} half-width, expected \
                 {expected} from the vanilla constants. The swapped-rr1/rr2 hypothesis \
                 predicts {wrong} here."
            );
            assert!(
                (measured - wrong).abs() > 1.0e-3,
                "seed {seed}: the {label} ring's predicted correct and swapped values \
                 ({expected} and {wrong}) are too close to tell apart, so this arm cannot \
                 discriminate — a defect in this test, not in the walk."
            );
        }
    }
}

/// Two seeds give two different bolts.
///
/// The check that the seed reaches the walk at all rather than being accepted
/// and ignored — the shape of defect this repo files as "a correct function fed
/// a constant by its producer".
#[test]
fn different_seeds_walk_differently() {
    let a = lightning_bolt_vertices(SEEDS[0]);
    let b = lightning_bolt_vertices(SEEDS[1]);
    assert_eq!(a.len(), b.len());
    let differing = a
        .iter()
        .zip(&b)
        .filter(|(x, y)| x.position != y.position)
        .count();
    assert!(
        differing > a.len() / 2,
        "only {differing} of {} vertices differ between two seeds; a walk that ignored its \
         seed would look exactly like this",
        a.len()
    );
}
