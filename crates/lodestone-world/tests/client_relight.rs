//! Gates for the client's own relight ([`lodestone_world::relight`]) — the thing
//! that stops a broken block leaving a pitch-black hole on a real vanilla server.
//!
//! # Where the expected value comes from
//!
//! Not from the relight, and not from a round-trip through it. Every assertion here
//! is judged against [`compute_column_light_with_neighbours`], the from-scratch 3×3
//! flood — a **structurally independent construction of the same physical rule**:
//! one is a bounded box seeded from a fixed shell and recomputed incrementally, the
//! other is a whole-neighbourhood flood from zero over a 48×48 field. They share the
//! injected [`LightProperties`] and nothing else, so agreement is a real claim.
//!
//! That arm is itself judged against real vanilla light by
//! `tests/vanilla_light_oracle.rs`, whose input is the sky and block light a Mojang
//! 26.2 server computed and wrote into `.cache/mc/survival/world`. So the chain of
//! custody runs vanilla → full flood → incremental relight, and no link in it is our
//! own encoder answering our own decoder.
//!
//! # Why this scene and not a simpler one
//!
//! **A block broken in open sky cannot separate the hypotheses.** It comes out at 15
//! whether you relight properly or merely flood sky light straight down, and the
//! wrong implementation looks right. Every scene here therefore breaks a block
//! **under a solid ceiling with light arriving from the side**, where the answer is a
//! partial value that only lateral propagation produces.
//!
//! The terrain is uniform in `x` and tiles with period 16 in `z` on purpose. The
//! relight's box is 31 cells wide, so it always straddles chunk borders; making the
//! terrain seam-invariant means the two arms cannot disagree merely because one of
//! them reached a chunk the other treated as a barrier, which would be a fixture
//! artifact wearing a bug's clothes.
//!
//! # Controls
//!
//! [`the_survey_detects_a_client_that_does_not_relight`] is the negative control and
//! it is not optional: it runs the identical scene with the relight **skipped** —
//! precisely the bug this module fixes — and requires the same comparison to fail,
//! reporting the bounding box of the disagreement. A gate that only ever sees the
//! fixed code cannot tell you it would have caught the broken one.

use std::collections::BTreeSet;

use lodestone_world::relight::{AFFECTED_RADIUS, RELIGHT_CELL_BUDGET};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LightData, LightPatch, LightProperties,
    LoadedChunk, NibbleArray, Neighbourhood, PaletteKind, World,
    compute_column_light_with_neighbours,
};

const MIN_Y: i32 = -64;
const SECTION_COUNT: usize = 24;
const EDGE: i32 = 16;

// A tiny hand-rolled palette. Values are pairwise distinct and none is `1`, so a
// transposed argument or an off-by-one id cannot coincide with a neighbour's.
const AIR: u32 = 0;
const STONE: u32 = 7;
const GLOWSTONE: u32 = 11;

/// Dampening and emission for the three ids above, hand-written rather than looked
/// up so the fixture depends on no generated table.
struct FixtureProps;

impl LightProperties for FixtureProps {
    fn opacity(&self, state: u32) -> u8 {
        match state {
            AIR => 0,
            _ => 15,
        }
    }
    fn emission(&self, state: u32) -> u8 {
        match state {
            GLOWSTONE => 14,
            _ => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// The scene
// ---------------------------------------------------------------------------

/// `y` of the solid ceiling. Clear of both the world floor and the column top, so no
/// clamp of the relight's box is exercised incidentally.
const CEILING_Y: i32 = -40;
/// The ceiling's one hole, as an inclusive local `x`/`z` range. A 3×3 skylight rather
/// than an open side: the room below is roofed everywhere else, so every lit cell in
/// it got its light by spreading **sideways** out of the shaft under this hole. That
/// is the gradient a purely vertical propagator cannot reproduce.
const SKYLIGHT: std::ops::RangeInclusive<usize> = 7..=9;
/// The subject: a lone stone block under the roof, four cells out from the shaft, so
/// the cell it occupies is surrounded by genuinely partial sky light.
const BREAK: [i32; 3] = [4, CEILING_Y - 2, 4];
/// A second subject in a different shape: a ceiling block on the rim of the skylight.
/// Breaking it widens the shaft, so light changes across the whole room instead of in
/// one cell — the case that exercises the flood and the fixed shell rather than a
/// single write-back.
const RIM_BREAK: [i32; 3] = [6, CEILING_Y, 8];

/// One column of the scene: a stone floor, a stone ceiling with one 3×3 skylight, air
/// between, a glowstone block off to one side so block light is exercised too, and a
/// lone stone block at [`BREAK`].
fn scene_column() -> ChunkColumn {
    let mut c = ChunkColumn::new(
        MIN_Y,
        SECTION_COUNT,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        AIR,
        0,
    );
    for z in 0..16usize {
        for x in 0..16usize {
            c.set_block(x, MIN_Y, z, STONE);
            if !(SKYLIGHT.contains(&x) && SKYLIGHT.contains(&z)) {
                c.set_block(x, CEILING_Y, z, STONE);
            }
        }
    }
    // A block-light source inside the room, deliberately not at either subject so the
    // two layers are not measuring the same cells.
    c.set_block(2, CEILING_Y - 6, 2, GLOWSTONE);
    // The subject.
    c.set_block(BREAK[0] as usize, BREAK[1], BREAK[2] as usize, STONE);
    c
}

/// The 3×3 of chunk positions the relight's box can reach from a break at local
/// `(4, ·, 4)`.
const NEIGHBOURHOOD: [(i32, i32); 9] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (0, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// A world whose nine columns all carry [`scene_column`]'s terrain and, for each,
/// the light the from-scratch 3×3 flood computes for it.
///
/// Seeding the *correct* pre-change light matters: the relight's fixed shell trusts
/// what is stored there, so a fixture that started from zeros would be measuring the
/// relight against a state no server ever sends.
fn scene_world() -> World {
    let mut world = World::new();
    let column = scene_column();
    for (cx, cz) in NEIGHBOURHOOD {
        let mut hood = Neighbourhood::new(&column);
        for (dx, dz) in NEIGHBOURHOOD {
            if (dx, dz) != (0, 0) {
                hood = hood.with(dx, dz, &column);
            }
        }
        let light = compute_column_light_with_neighbours(&hood, &FixtureProps);
        world.load(
            ChunkPos::new(cx, cz),
            LoadedChunk::new(
                column.clone(),
                light,
                Heightmaps::new(),
                Vec::new(),
            ),
        );
    }
    world
}

/// The from-scratch answer for the centre column of a world whose terrain is
/// `column` everywhere — the expectation the relight is judged against.
fn expected_light(column: &ChunkColumn) -> ColumnLight {
    let mut hood = Neighbourhood::new(column);
    for (dx, dz) in NEIGHBOURHOOD {
        if (dx, dz) != (0, 0) {
            hood = hood.with(dx, dz, column);
        }
    }
    compute_column_light_with_neighbours(&hood, &FixtureProps)
}

// ---------------------------------------------------------------------------
// Comparison, reported by location
// ---------------------------------------------------------------------------

/// A cell-by-cell comparison of the centre column, carrying **where** it disagreed.
///
/// A count alone cannot tell a thin residual across a room from one solid black
/// block, and the second is the entire reported symptom, so the bounding box and a
/// few worst cells are printed on failure.
#[derive(Default)]
struct Survey {
    compared: usize,
    sky_bad: usize,
    block_bad: usize,
    bbox: Option<([i32; 3], [i32; 3])>,
    worst: Vec<(String, i32, i32, i32, u8, u8)>,
}

impl Survey {
    fn note(&mut self, layer: &str, x: i32, y: i32, z: i32, ours: u8, theirs: u8) {
        match &mut self.bbox {
            None => self.bbox = Some(([x, y, z], [x, y, z])),
            Some((lo, hi)) => {
                for (a, v) in [x, y, z].into_iter().enumerate() {
                    lo[a] = lo[a].min(v);
                    hi[a] = hi[a].max(v);
                }
            }
        }
        if self.worst.len() < 8 {
            self.worst
                .push((layer.to_string(), x, y, z, ours, theirs));
        }
    }

    fn disagreements(&self) -> usize {
        self.sky_bad + self.block_bad
    }

    fn report(&self, label: &str) -> String {
        let mut s = format!(
            "{label}: {} cells compared, {} sky and {} block disagreements",
            self.compared, self.sky_bad, self.block_bad
        );
        if let Some((lo, hi)) = self.bbox {
            s.push_str(&format!(
                "\n  disagreement bounding box: x {}..={}, y {}..={}, z {}..={}",
                lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]
            ));
        }
        for (layer, x, y, z, ours, theirs) in &self.worst {
            s.push_str(&format!(
                "\n  {layer} at ({x}, {y}, {z}): stored {ours}, expected {theirs}"
            ));
        }
        s
    }
}

/// The band of world `y` the relight's box for a break at `at` could have touched, so
/// the comparison is scoped to what the subject was asked to fix rather than to the
/// whole column. Derived from [`AFFECTED_RADIUS`] and the break, not from a guessed
/// constant.
///
/// The box's own outer shell is excluded: those cells are fixed by construction, so
/// including them would compare cells the relight never wrote and read agreement there
/// as evidence about the relight.
fn compared_y_range(at: [i32; 3]) -> std::ops::RangeInclusive<i32> {
    // Every subject here sits above the floor at `MIN_Y`, so its run of transparent
    // cells below stops at `MIN_Y + 1`; the box spans one radius past that.
    let lo = (MIN_Y + 2 - AFFECTED_RADIUS).max(MIN_Y);
    let hi = (at[1] + AFFECTED_RADIUS - 1).min(MIN_Y + SECTION_COUNT as i32 * EDGE - 1);
    lo..=hi
}

/// Compare the centre chunk's stored light against `expected`, over the centre
/// column's cells in [`compared_y_range`].
fn survey_around(world: &World, expected: &ColumnLight, at: [i32; 3]) -> Survey {
    let mut out = Survey::default();
    let chunk = world.get(ChunkPos::new(0, 0)).expect("centre loaded");
    for y in compared_y_range(at) {
        let ls = usize::try_from((y - MIN_Y).div_euclid(EDGE) + 1).expect("in range");
        let y_in = (y - MIN_Y).rem_euclid(EDGE) as usize;
        for z in 0..16usize {
            for x in 0..16usize {
                let nibble = NibbleArray::index(x, y_in, z);
                let want_sky = expected.sky(ls).get(nibble).unwrap_or(0);
                let want_block = expected.block(ls).get(nibble).unwrap_or(0);
                // A `Missing` sky section means "above the top populated section",
                // which vanilla answers as 15 — the same rule the relight and the
                // mesher's `SkyDefault` follow. Reading it as 0 here would invent
                // disagreements the renderer never sees.
                let got_sky = match chunk.light.sky(ls) {
                    LightData::Missing => 15,
                    other => other.get(nibble).unwrap_or(0),
                };
                let got_block = chunk.light.block(ls).get(nibble).unwrap_or(0);
                out.compared += 2;
                if got_sky != want_sky {
                    out.sky_bad += 1;
                    out.note("sky", x as i32, y, z as i32, got_sky, want_sky);
                }
                if got_block != want_block {
                    out.block_bad += 1;
                    out.note("block", x as i32, y, z as i32, got_block, want_block);
                }
            }
        }
    }
    out
}

/// [`survey_around`] scoped to the primary subject.
fn survey(world: &World, expected: &ColumnLight) -> Survey {
    survey_around(world, expected, BREAK)
}

/// Stored sky light at a centre-column cell, with the same `Missing` convention.
fn stored_sky(world: &World, x: usize, y: i32, z: usize) -> u8 {
    let chunk = world.get(ChunkPos::new(0, 0)).expect("centre loaded");
    let ls = usize::try_from((y - MIN_Y).div_euclid(EDGE) + 1).expect("in range");
    let nibble = NibbleArray::index(x, (y - MIN_Y).rem_euclid(EDGE) as usize, z);
    match chunk.light.sky(ls) {
        LightData::Missing => 15,
        other => other.get(nibble).unwrap_or(0),
    }
}

/// Break `at` (local coordinates, applied in every one of the nine columns so the
/// terrain stays seam-invariant and the from-scratch arm's neighbourhood matches the
/// world the relight sees), returning the post-break column for the expectation.
///
/// The write goes straight into the columns rather than through
/// [`World::set_block`], and the relight is queued explicitly, so a test can also
/// exercise the *unqueued* case — which is the bug.
fn break_at(world: &mut World, at: [i32; 3]) -> ChunkColumn {
    for (cx, cz) in NEIGHBOURHOOD {
        let chunk = world.get_mut(ChunkPos::new(cx, cz)).expect("loaded");
        chunk
            .column
            .set_block(at[0] as usize, at[1], at[2] as usize, AIR);
    }
    world.queue_relight(at[0], at[1], at[2]);
    let mut after = scene_column();
    after.set_block(at[0] as usize, at[1], at[2] as usize, AIR);
    after
}

/// [`break_at`] the primary subject — a lone block under the roof. Only the centre
/// chunk's change is queued, which is what a real `block_update` for one position
/// does.
fn break_the_block(world: &mut World) -> ChunkColumn {
    break_at(world, BREAK)
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// The fixture has to contain the thing under test: a **partial** sky value at the
/// broken cell's neighbours, produced by light arriving from the side under a solid
/// ceiling.
///
/// Without this, every other gate in the file could pass over a scene where the
/// answer is 0 or 15 under any implementation — the vacuous-world species, which
/// reading the test cannot find.
#[test]
fn the_scene_puts_the_break_under_a_ceiling_with_only_lateral_light() {
    let world = scene_world();
    let above = stored_sky(
        &world,
        BREAK[0] as usize,
        BREAK[1] + 1,
        BREAK[2] as usize,
    );
    assert!(
        (1..=14).contains(&above),
        "the cell above the break must hold a partial sky value — got {above}, which \
         means the scene is either open to the sky (15) or fully sealed (0) and \
         cannot separate a real relight from a vertical flood"
    );
    // The ceiling really is solid above the break: a cell open to the sky would be
    // 15 at every height, so a partial value proves the light came sideways.
    let under_ceiling = stored_sky(
        &world,
        BREAK[0] as usize,
        CEILING_Y - 1,
        BREAK[2] as usize,
    );
    assert!(
        under_ceiling < 15,
        "the ceiling is not blocking sky light: cell just below it reads \
         {under_ceiling}"
    );
    // And the break's own cell is dark, because it is solid stone.
    assert_eq!(
        stored_sky(&world, BREAK[0] as usize, BREAK[1], BREAK[2] as usize),
        0,
        "an opaque cell stores sky light 0 — this is the value the mesher reads into \
         the hole the moment the block becomes air"
    );
}

/// **The defect, demonstrated.** Writing air over the block changes no light at all
/// until something relights: the cell keeps the `0` it held as solid stone, which is
/// exactly what the mesher samples for every face now exposed to it.
///
/// This is the *first* of the two candidate causes stated in the brief, isolated: no
/// mask is involved, no packet is involved, and the client has simply not recomputed
/// anything.
#[test]
fn a_block_write_alone_leaves_the_broken_cell_pitch_black() {
    let mut world = scene_world();
    break_the_block(&mut world);
    assert_eq!(
        stored_sky(&world, BREAK[0] as usize, BREAK[1], BREAK[2] as usize),
        0,
        "set_block must not touch light by itself — if this is non-zero the write \
         path has grown a hidden relight and the queue-then-drain design is being \
         bypassed"
    );
}

/// The measurement: after the relight, the centre column's light agrees cell for cell
/// with the from-scratch 3×3 flood over the whole band the relight could reach.
#[test]
fn the_relight_agrees_with_a_from_scratch_flood() {
    let mut world = scene_world();
    let after = break_the_block(&mut world);
    let relit = world.run_pending_relight(&FixtureProps, true);

    let survey = survey(&world, &expected_light(&after));
    eprintln!("{}", survey.report("incremental relight"));
    eprintln!(
        "relit: {} jobs, {} cells visited, {} cells changed, {} dirty sections",
        relit.jobs,
        relit.cells_visited,
        relit.cells_changed,
        relit.dirty_sections.len()
    );

    assert!(
        survey.compared > 10_000,
        "the survey compared only {} cells — a vacuous pass, not agreement",
        survey.compared
    );
    assert_eq!(
        survey.disagreements(),
        0,
        "the incremental relight disagrees with the from-scratch flood\n{}",
        survey.report("incremental relight")
    );
    // And it did real work: agreement with zero cells changed would mean the light
    // was already right and the gate proved nothing about the relight.
    assert!(
        relit.cells_changed > 0,
        "the relight changed no light at all, so agreement here says nothing"
    );
}

/// The same comparison over a change that moves light **in volume** rather than in one
/// cell: breaking the rim of the skylight widens the shaft, so the gradient shifts
/// across the room and hundreds of cells must land on the flood's values.
///
/// This is the arm that exercises the flood and the fixed shell. The single-block gate
/// above is the reported symptom and writes back exactly one cell, which a broken
/// propagator could survive by accident.
#[test]
fn a_change_that_moves_light_in_volume_still_agrees_with_the_flood() {
    let mut world = scene_world();
    let after = break_at(&mut world, RIM_BREAK);
    let relit = world.run_pending_relight(&FixtureProps, true);

    let survey = survey_around(&world, &expected_light(&after), RIM_BREAK);
    eprintln!("{}", survey.report("rim break"));
    eprintln!(
        "relit: {} jobs, {} cells visited, {} cells changed, {} dirty sections",
        relit.jobs,
        relit.cells_visited,
        relit.cells_changed,
        relit.dirty_sections.len()
    );
    assert_eq!(
        survey.disagreements(),
        0,
        "widening the skylight left the room disagreeing with the flood\n{}",
        survey.report("rim break")
    );
    // The magnitude is the point here: a change of one cell would mean the fixture is
    // measuring the same thing the single-block gate already measured.
    assert!(
        relit.cells_changed > 100,
        "widening a skylight changed only {} cells, so this arm is not exercising \
         propagation in volume",
        relit.cells_changed
    );
}

/// **The negative control.** The identical scene with the relight skipped must fail
/// the identical comparison. Without this the gate above cannot distinguish "the
/// relight is correct" from "the comparison cannot fail".
#[test]
fn the_survey_detects_a_client_that_does_not_relight() {
    let mut world = scene_world();
    let after = break_the_block(&mut world);
    // Deliberately do not drain the queue — this is the bug.
    let survey = survey(&world, &expected_light(&after));
    eprintln!("{}", survey.report("no relight (control)"));
    assert!(
        survey.sky_bad > 0,
        "a client that never relights must disagree with the flood; it did not, so \
         this scene cannot see the defect\n{}",
        survey.report("no relight (control)")
    );
    // And the disagreement is *localised* around the break, not spread over the
    // column — a uniform residual would mean the fixture's baseline light was wrong
    // rather than that the break went unlit.
    let (lo, hi) = survey.bbox.expect("a disagreement was counted");
    assert!(
        (lo[1]..=hi[1]).contains(&BREAK[1]),
        "the disagreement does not include the broken block's own y ({}) — box y \
         {}..={}, so this control is firing on something other than the break\n{}",
        BREAK[1],
        lo[1],
        hi[1],
        survey.report("no relight (control)")
    );
}

/// The volume arm's own control, and the reason there are two: the single-block
/// control disagrees in exactly **one** cell of 18,944, which is a real signal but a
/// thin one. Skipping the relight after the rim break must disagree in a hundred or
/// more, so a comparison that had quietly stopped looking at most of the room is
/// caught here rather than shrugged off as a rounding difference.
#[test]
fn the_survey_detects_a_missing_relight_in_volume_too() {
    let mut world = scene_world();
    let after = break_at(&mut world, RIM_BREAK);
    // Deliberately do not drain the queue — this is the bug, at scale.
    let survey = survey_around(&world, &expected_light(&after), RIM_BREAK);
    eprintln!("{}", survey.report("no relight, rim break (control)"));
    assert!(
        survey.sky_bad > 100,
        "widening the skylight without relighting disagreed in only {} sky cells; the \
         comparison is not covering the room\n{}",
        survey.sky_bad,
        survey.report("no relight, rim break (control)")
    );
}

/// The broken cell itself ends up holding the flood's value, and that value is
/// **partial** — the discriminating outcome. A vertical-flood implementation would
/// put 15 here; a client that does nothing leaves 0. Both are excluded by asserting
/// the number the independent arm computed.
#[test]
fn the_broken_cell_ends_up_at_the_value_the_flood_computes() {
    let mut world = scene_world();
    let after = break_the_block(&mut world);
    world.run_pending_relight(&FixtureProps, true);

    let expected = expected_light(&after);
    let ls = usize::try_from((BREAK[1] - MIN_Y).div_euclid(EDGE) + 1).expect("in range");
    let nibble = NibbleArray::index(
        BREAK[0] as usize,
        (BREAK[1] - MIN_Y).rem_euclid(EDGE) as usize,
        BREAK[2] as usize,
    );
    let want = expected.sky(ls).get(nibble).unwrap_or(0);
    assert!(
        (1..=14).contains(&want),
        "the fixture's own answer for the broken cell is {want}, not a partial value \
         — this input cannot separate a real relight from a vertical flood"
    );
    assert_eq!(
        stored_sky(&world, BREAK[0] as usize, BREAK[1], BREAK[2] as usize),
        want,
        "the broken cell did not reach the flood's value"
    );
}

/// A server `light_update` that lands **before** we drain must cancel our relight for
/// that column, or a real correction gets overwritten by our own recomputation — the
/// divergence bug in a subtler form than the darkness.
#[test]
fn a_server_light_patch_cancels_a_pending_relight_for_its_chunk() {
    let mut world = scene_world();
    break_the_block(&mut world);
    // Also queue a change in a *different* column, to prove the cancellation is
    // scoped to the patched chunk rather than clearing the whole queue.
    world.queue_relight(EDGE + 3, BREAK[1], 5);

    let mut patch = LightPatch::new();
    let sky_ls = usize::try_from((BREAK[1] - MIN_Y).div_euclid(EDGE) + 1).expect("in range");
    // A value no relight of this scene would produce, so "the server won" is
    // observable rather than inferred.
    patch.set_sky(sky_ls, LightData::Uniform(9));
    world.merge_light(ChunkPos::new(0, 0), patch);

    let relit = world.run_pending_relight(&FixtureProps, true);
    assert_eq!(
        relit.jobs, 1,
        "exactly the other column's job should have run; {} ran",
        relit.jobs
    );
    assert_eq!(
        stored_sky(&world, BREAK[0] as usize, BREAK[1], BREAK[2] as usize),
        9,
        "the server's patch was overwritten by our own relight"
    );
}

/// The cost, as a **counter** rather than a duration: one break recomputes exactly
/// the box its own radius defines, and nothing wider.
///
/// The number is derived, not observed-and-pasted: `31` is
/// `2 * AFFECTED_RADIUS + 1`, and the `y` extent is shorter than `31` here because
/// the break's sky run reaches the floor and the box clamps to the world. Predicting
/// it from the constants is what makes a silently-widened box fail this.
#[test]
fn one_break_costs_one_bounded_box() {
    let mut world = scene_world();
    break_the_block(&mut world);
    let relit = world.run_pending_relight(&FixtureProps, true);

    let width = (2 * AFFECTED_RADIUS + 1) as usize;
    // The break sits above a floor at MIN_Y; the run of transparent cells below it
    // stops at MIN_Y + 1, and the box spans one radius past that, clamped to the
    // column's light range (which starts one apron section below MIN_Y).
    let y_lo = (MIN_Y + 1 - AFFECTED_RADIUS).max(MIN_Y - EDGE);
    let y_hi = BREAK[1] + AFFECTED_RADIUS;
    let height = (y_hi - y_lo + 1) as usize;
    assert_eq!(
        relit.cells_visited,
        width * height * width,
        "one break should recompute exactly {width}x{height}x{width} cells; the box \
         has changed shape"
    );
    assert_eq!(relit.jobs, 1, "one changed section is one job");
    assert!(
        relit.cells_visited < RELIGHT_CELL_BUDGET,
        "a single break must not exhaust the per-drain budget"
    );
}

/// The relight has to say which section meshes it invalidated, or the light it fixed
/// reaches no pixels. The broken block's own section must be in the set.
#[test]
fn the_relight_reports_the_sections_whose_mesh_went_stale() {
    let mut world = scene_world();
    break_the_block(&mut world);
    let relit = world.run_pending_relight(&FixtureProps, true);

    let own = (
        BREAK[0].div_euclid(EDGE),
        BREAK[1].div_euclid(EDGE),
        BREAK[2].div_euclid(EDGE),
    );
    assert!(
        relit.dirty_sections.contains(&own),
        "the broken block's own section {own:?} is not in the dirty set {:?}",
        relit.dirty_sections
    );
    // And the set is not the whole world: a relight that dirties everything is a
    // full re-mesh wearing an incremental fix's clothes.
    assert!(
        relit.dirty_sections.len() < 64,
        "one break dirtied {} sections",
        relit.dirty_sections.len()
    );
}

/// Many changes inside one section coalesce into a single job, so a bulk edit does not
/// cost one box per cell.
#[test]
fn changes_in_one_section_coalesce_into_one_job() {
    let mut world = scene_world();
    let mut cells: Vec<(u8, u8, u8, u32)> = Vec::new();
    for z in 4..8u8 {
        for x in 4..8u8 {
            cells.push((x, (BREAK[1] - MIN_Y).rem_euclid(EDGE) as u8, z, AIR));
        }
    }
    world.set_blocks(0, BREAK[1].div_euclid(EDGE), 0, &cells);
    let relit = world.run_pending_relight(&FixtureProps, true);
    assert_eq!(
        relit.jobs, 1,
        "{} cells in one section produced {} jobs, not one",
        cells.len(),
        relit.jobs
    );
    // The box is the bounding box of the changes, so it is wider than a single
    // break's — the coalescing must not silently relight only one of them.
    let single = (2 * AFFECTED_RADIUS + 1) as usize;
    assert!(
        relit.cells_visited > single * single,
        "the coalesced box ({} cells) is no bigger than one break's cross-section, \
         so it cannot cover all {} changes",
        relit.cells_visited,
        cells.len()
    );
}

/// The queue is bounded: a host that writes blocks and never drains must not grow a
/// list for the whole session.
#[test]
fn the_pending_queue_is_bounded() {
    let mut world = World::new();
    // No chunk loaded, so nothing relights; the queue is the only thing that moves.
    for i in 0..(lodestone_world::PENDING_RELIGHT_CAP as i32 + 500) {
        world.queue_relight(i, 0, 0);
    }
    let relit = world.run_pending_relight(&FixtureProps, true);
    assert_eq!(
        relit.jobs, 0,
        "no chunk is loaded, so no job can run; {} did",
        relit.jobs
    );
    // Nothing ran, so nothing was requeued either: the drain took the whole capped
    // queue and dropped every job for want of a chunk.
    let mut empty = BTreeSet::new();
    empty.extend(relit.dirty_sections.iter().copied());
    assert!(empty.is_empty(), "unloaded chunks dirtied {empty:?}");
}
