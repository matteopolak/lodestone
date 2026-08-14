//! The integrated server's chunk light, end to end on the wire.
//!
//! # What this gate is for
//!
//! `crates/lodestone-world/src/lighting.rs` is a 1,105-line port of vanilla's
//! `LightEngine`/`BlockLightEngine`/`SkyLightEngine`, unit-tested, benched, and
//! judged against a real vanilla 26.2 server (`live_terrain_light.rs`). Until
//! this crate's own wiring landed, its only production caller was the *client's* singleplayer worldgen.
//! Every column `V770ServerProtocol::encode_chunk` sent carried
//! `ColumnLight::new(section_count)` — all-`Missing`, both layers, every section.
//! The engine was a textbook island: green tests, zero pixels on the server path.
//!
//! **`Missing` is not darkness.** A client resolves an absent overworld sky
//! section to full daylight (`lodestone_render::SkyDefault::Full`; vanilla's own
//! client does the same through `SkyLightSectionStorage`), so the symptom was a
//! uniformly *bright* world — lit caves, lit sealed rooms, no night. Establishing
//! that direction first mattered: anyone hunting this by looking for blackness
//! was looking for the wrong colour.
//!
//! # Fixture choice is the load-bearing decision here
//!
//! The obvious fixture — seed 1234, chunk (0, 0), the one the neighbouring
//! `encode_chunk_*` tests use — is a **vacuous world** for light, and reading a
//! test could never tell you: it is ocean, so its light is sky-15 above the water
//! and a purely *vertical* 15→14→13… decay through it, with **zero** horizontal
//! sky gradient and **zero** block-lit cells. Measured over five chunks at each of
//! two seeds: seed 1234 produced `horiz = 0` at all five, so a gate on it would
//! exercise neither horizontal decay nor emission while looking rigorous.
//!
//! This file therefore uses **seed 42, chunk (−9, 4)** (surveyed `horiz = 1113`)
//! for the terrain claims, and **places its own emitter** for the block-light
//! claim rather than hoping generated lava lands in frame.
//!
//! # What it asserts, and what makes each claim non-vacuous
//!
//! | claim | why it cannot pass vacuously |
//! |---|---|
//! | light reaches the decoded wire bytes with **located, predicted** levels | expected levels are derived from an *independent* generator instance's block states, never from the light engine; a control against the old all-`Missing` payload is run and must fail |
//! | the wire light **is** the engine's output, section for section | rules out a plausible substitute (a uniform 15-sky/0-block fill) passing the located checks |
//! | a placed glowstone's halo reaches the wire at exactly 15/14/13 | the control is the same cells before the edit, which must read 0 |
//! | the isolated compute is **never brighter** than the exact 3×3 compute | adding sources can only raise light, so this is a hard direction claim; over-propagation, a wrong nibble order or a section off-by-one breaks it |
//! | the seam residual sits under a **pinned ceiling**, bounding box printed | a fraction cannot distinguish a correct field from a stripe; the box says *where* |
//! | the seam **detector fires** when a neighbour holds the only source | an absence claim about seams is worth only as much as the evidence the check would have caught one |
//!
//! # The seam, stated plainly
//!
//! `encode_chunk` is handed one column and cannot reach the
//! `lodestone_server::ChunkSource` its neighbours live in, so the shipped compute
//! is `compute_column_light` (isolated), not
//! `compute_column_light_with_neighbours` (exact for the centre). This file
//! measures the difference instead of describing it: surveyed over ten served
//! chunks it was **0 cells on seven of them and at most 121 of 212,992 (0.057%)**,
//! always in the never-brighter direction, and located at the surface where a
//! hillside's sky gradient runs across a column border. See
//! `server_protocol::compute_served_light`'s doc comment and `DESIGN.md` §12.117
//! for the brokered `lodestone-server` patch that takes it to zero.
#![allow(clippy::needless_range_loop)]

use std::time::Instant;

use lodestone_core::Reader;
use lodestone_data::block_states;
use lodestone_server::{ChunkSource, ServerDirective, ServerProtocol, overworld_chunk_source};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_world::{
    ChunkColumn as WorldColumn, ColumnLight, LightData, LightProperties, Neighbourhood,
    NibbleArray, compute_column_light, compute_column_light_with_neighbours,
};

/// The fixture seed and chunk for every terrain claim below. Chosen from the
/// survey in this file's module docs, not from convenience: seed 1234's chunks
/// are ocean and produce no horizontal sky gradient at all.
const SEED: i64 = 42;
const CX: i32 = -9;
const CZ: i32 = 4;

const EDGE: usize = 16;

/// Ceiling on the isolated-vs-exact seam residual, as a fraction of cells
/// compared.
///
/// A **pinned bound derived from a measurement**, not a sign check. The surveyed
/// worst case across ten served chunks was 121/212,992 = 0.057%; the competing
/// hypothesis — a compute that dropped cross-chunk propagation *and* mis-seeded
/// the interior (say, seeding sky only at the very top layer instead of at every
/// unoccluded cell) — measures in whole percent, because it would darken every
/// cell under water and under every leaf. 0.2% separates the two with ~3.5×
/// headroom over the real value. **When the brokered `lodestone-server` patch
/// lands this becomes 0.0**, and the failure message below says so.
const SEAM_RESIDUAL_CEILING: f64 = 0.002;

/// Cost ceiling: the light flood must stay a small fraction of the column
/// *generation* it rides behind, measured in the same process (see
/// [`light_cost_per_column_stays_a_small_fraction_of_generation`] for why a ratio
/// and not a duration).
const LIGHT_OVER_GENERATION_CEILING: f64 = 0.25;

// ---------------------------------------------------------------------------
// Props + column conversion
// ---------------------------------------------------------------------------

/// The same 26.2 census `V770ServerProtocol` runs the engine against.
struct Props;

impl LightProperties for Props {
    fn opacity(&self, state: u32) -> u8 {
        lodestone_data::light_props::dampening(state)
    }
    fn emission(&self, state: u32) -> u8 {
        lodestone_data::light_props::emission(state)
    }
}

/// Resolves a served column into the version-free column the engine walks.
///
/// Deliberately a *test-local* re-implementation of the encoder's private
/// `build_world_column`: reusing the encoder's own conversion would let a
/// conversion bug cancel out on both sides of every comparison below. Ids come
/// from `block_states`, the same authoritative census the encoder resolves
/// through.
fn to_world_column(shape: &ChunkShape, src: &lodestone_server::ChunkColumn) -> WorldColumn {
    let mut column = WorldColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );
    let mut seen: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for si in 0..shape.section_count {
        let base = shape.min_y + (si * EDGE) as i32;
        for ly in 0..EDGE {
            let wy = base + ly as i32;
            for lz in 0..EDGE {
                for lx in 0..EDGE {
                    let state = src.block_state(lx as i32, wy, lz as i32);
                    let id = *seen.entry(state).or_insert_with(|| resolve_state_id(state));
                    if id != shape.air_id {
                        column.set_block(lx, wy, lz, id);
                    }
                }
            }
        }
    }
    column
}

/// Block-state resolution over the committed census — **the production function**,
/// not a copy of it.
///
/// This was a hand-rolled duplicate whose fallback was "the lowest id sharing the
/// block name". That was the *pre-`43a6e030`* rule, and it is wrong for 661 of the
/// 797 multi-state blocks: it made bare `minecraft:grass_block` resolve snowy,
/// bare directionals face whatever the lowest id faced, and
/// redstone dust render climbing rather than flat. So this helper had already
/// become a silent caller of a rule the encoder no longer follows — the exact
/// failure `CLAUDE.md` records, where a *sibling* copy in
/// `block_entities_live.rs` failed as a 30-second live timeout rather than a
/// mismatch.
///
/// Since the resolver moved into `lodestone-data` there is a public function to
/// call, and a light oracle that resolved a state differently from the encoder it
/// is judging would be comparing two different worlds. Do not re-inline this.
fn resolve_state_id(state: &str) -> u32 {
    block_states::state_id(state).unwrap_or_else(block_states::air_state_id)
}

/// The light payload the real encoder put on the wire for `(cx, cz)`.
fn served_light(cx: i32, cz: i32, source: &impl ChunkSource, shape: &ChunkShape) -> ColumnLight {
    let column = source.column(cx, cz);
    let payload = match V770ServerProtocol.encode_chunk(cx, cz, &column) {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("expected Send, got {other:?}"),
    };
    let mut r = Reader::new(&payload);
    let decoded = LevelChunkWithLight::decode(&mut r, shape).expect("decode served column");
    r.ensure_empty()
        .expect("no trailing bytes after the light payload");
    decoded.light
}

/// Reads sky light at chunk-local `(x, z)` and **world** `y`, resolving the
/// light-section off-by-one (light section `i` covers block section `i − 1`).
fn sky_at(light: &ColumnLight, min_y: i32, x: usize, y: i32, z: usize) -> Option<u8> {
    let (s, yl) = light_cell(min_y, y)?;
    light.sky(s).get(NibbleArray::index(x, yl, z))
}

fn block_at(light: &ColumnLight, min_y: i32, x: usize, y: i32, z: usize) -> Option<u8> {
    let (s, yl) = light_cell(min_y, y)?;
    light.block(s).get(NibbleArray::index(x, yl, z))
}

fn light_cell(min_y: i32, y: i32) -> Option<(usize, usize)> {
    let rel = y - (min_y - EDGE as i32);
    Some((
        usize::try_from(rel / EDGE as i32).ok()?,
        usize::try_from(rel.rem_euclid(EDGE as i32)).ok()?,
    ))
}

/// The 3×3 neighbourhood's world columns for `(cx, cz)`.
fn neighbourhood_columns(
    source: &impl ChunkSource,
    shape: &ChunkShape,
    cx: i32,
    cz: i32,
) -> Vec<(i32, i32, WorldColumn)> {
    let mut out = Vec::with_capacity(9);
    for dz in -1..=1 {
        for dx in -1..=1 {
            out.push((
                dx,
                dz,
                to_world_column(shape, &source.column(cx + dx, cz + dz)),
            ));
        }
    }
    out
}

fn centre_of(cols: &[(i32, i32, WorldColumn)]) -> &WorldColumn {
    &cols
        .iter()
        .find(|&&(dx, dz, _)| (dx, dz) == (0, 0))
        .expect("centre present")
        .2
}

fn exact_light(cols: &[(i32, i32, WorldColumn)]) -> ColumnLight {
    let mut nbh = Neighbourhood::new(centre_of(cols));
    for (dx, dz, col) in cols {
        if (*dx, *dz) != (0, 0) {
            nbh = nbh.with(*dx, *dz, col);
        }
    }
    compute_column_light_with_neighbours(&nbh, &Props)
}

// ---------------------------------------------------------------------------
// Located sky expectations, expressed so a control can run them
// ---------------------------------------------------------------------------

/// One located expectation: the level a specific world cell must carry, and the
/// terrain fact that predicts it.
struct Expectation {
    x: usize,
    y: i32,
    z: usize,
    sky: u8,
    why: &'static str,
}

/// Derives the located expectations from a block-state lookup — nothing here
/// consults the light engine.
///
/// Two shapes, both predicted from vanilla's own rule:
///
/// * the first air cell above the highest dampening block in a column is open to
///   the sky (`ChunkSkyLightSources.isEdgeOccluded`'s scalar case is
///   `dampening != 0`), so its sky light is exactly `15`;
/// * a cell with 17 blocks of continuous dampening-15 material directly above it
///   and dampening-15 material 16 blocks out on all four horizontal sides cannot
///   be reached by any sky source, so its sky light is exactly `0`. 16, not 15:
///   light costs at least one level per block crossed, so 15 levels die inside 15
///   blocks and the 16th is unreachable.
fn expectations<'a>(
    state_at: &dyn Fn(usize, i32, usize) -> &'a str,
    min_y: i32,
    height: i32,
) -> Vec<Expectation> {
    let mut out = Vec::new();
    let top = min_y + height - 1;

    for &(x, z) in &[(3usize, 3usize), (8usize, 11usize)] {
        let mut open_y = None;
        for y in (min_y..=top).rev() {
            let id = resolve_state_id(state_at(x, y, z));
            if lodestone_data::light_props::dampening(id) != 0 {
                open_y = Some(y + 1);
                break;
            }
        }
        if let Some(y) = open_y
            && y <= top
        {
            out.push(Expectation {
                x,
                y,
                z,
                sky: 15,
                why: "first cell above the highest dampening block is open to the sky",
            });
        }
    }

    'search: for y in (min_y + 8)..(min_y + 40) {
        for z in 2..14usize {
            for x in 2..14usize {
                let capped = (0..=16i32).all(|dy| {
                    let id = resolve_state_id(state_at(x, y + dy, z));
                    lodestone_data::light_props::dampening(id) == 15
                });
                let walled = (1..=16i32).all(|d| {
                    [(d, 0), (-d, 0), (0, d), (0, -d)].iter().all(|&(dx, dz)| {
                        let nx = x as i32 + dx;
                        let nz = z as i32 + dz;
                        // Outside the column the isolated compute cannot receive
                        // light anyway; the exact compute's neighbours are judged
                        // by the seam gate, not here.
                        if !(0..16).contains(&nx) || !(0..16).contains(&nz) {
                            return true;
                        }
                        let id = resolve_state_id(state_at(nx as usize, y, nz as usize));
                        lodestone_data::light_props::dampening(id) == 15
                    })
                });
                if capped && walled {
                    out.push(Expectation {
                        x,
                        y,
                        z,
                        sky: 0,
                        why: "17 blocks of dampening-15 rock above and 16 out on every side \
                              ⇒ no sky source can reach",
                    });
                    break 'search;
                }
            }
        }
    }

    out
}

/// Runs the located expectations, returning `Err` with the full diagnostic on the
/// first failure. A `Result` rather than `assert!`s **so the control can call it
/// and observe it fail** — an absence claim ("the wire is no longer
/// all-`Missing`") is worth only as much as the evidence the detector fires.
fn check_located(light: &ColumnLight, min_y: i32, want: &[Expectation]) -> Result<usize, String> {
    if want.is_empty() {
        return Err("no located expectations were derived — the fixture is vacuous".into());
    }
    for e in want {
        match sky_at(light, min_y, e.x, e.y, e.z) {
            None => {
                return Err(format!(
                    "sky light at ({}, {}, {}) is Missing on the wire — the client would default \
                     it to full daylight. Expected {} ({})",
                    e.x, e.y, e.z, e.sky, e.why
                ));
            }
            Some(got) if got != e.sky => {
                return Err(format!(
                    "sky light at ({}, {}, {}) is {got}, expected {} ({})",
                    e.x, e.y, e.z, e.sky, e.why
                ));
            }
            Some(_) => {}
        }
    }
    Ok(want.len())
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

#[test]
fn served_sky_light_reaches_the_wire_with_predicted_levels() {
    let shape = ChunkShape::overworld_1_21();
    let source = overworld_chunk_source(SEED);
    // A *separately constructed* generator supplies the terrain facts the
    // expectations are derived from, so nothing asserted here originates in the
    // encoder being judged.
    let independent = lodestone_server::overworld_generator(SEED);
    let generated = independent.column(CX, CZ);

    let want = expectations(
        &|x: usize, y: i32, z: usize| generated.block_state(x, y, z),
        shape.min_y,
        shape.world_height as i32,
    );
    let light = served_light(CX, CZ, &source, &shape);

    let checked = check_located(&light, shape.min_y, &want)
        .unwrap_or_else(|err| panic!("served light is wrong: {err}"));
    println!("located sky expectations checked at ({CX}, {CZ}) @ seed {SEED}: {checked}");
    assert!(
        want.iter().any(|e| e.sky == 0),
        "the fixture derived no sealed cell, so it never proves the server stopped sending \
         full-bright light underground — the assertion set would be vacuous"
    );
    assert!(
        want.iter().any(|e| e.sky == 15),
        "the fixture derived no open-sky cell"
    );

    // --- The control. -------------------------------------------------------
    // The same expectations against the payload shape this issue replaced:
    // `ColumnLight::new`, all-`Missing`. It MUST fail, and the observed message
    // is printed so the evidence is the failure text, not a description of one.
    let old = ColumnLight::new(shape.section_count);
    let message = check_located(&old, shape.min_y, &want).expect_err(
        "CONTROL DID NOT FIRE: the all-`Missing` light this issue removed passes the same \
         located assertions, so they prove nothing about the fix",
    );
    println!("control (all-`Missing` payload) failed as required: {message}");
    assert!(
        message.contains("Missing on the wire"),
        "the control fired for the wrong reason: {message}"
    );
}

/// The island check proper: the bytes on the wire must **be** the light engine's
/// output for the same column, section for section — not merely something that
/// looks lit.
#[test]
fn wire_light_is_the_engine_output_for_the_same_column() {
    let shape = ChunkShape::overworld_1_21();
    let source = overworld_chunk_source(SEED);
    let served = served_light(CX, CZ, &source, &shape);

    let column = to_world_column(&shape, &source.column(CX, CZ));
    let expected = compute_column_light(&column, &Props);

    assert_eq!(
        served.light_section_count(),
        shape.section_count + 2,
        "the wire must carry one light section per block section plus the two aprons"
    );
    let mut varied = 0usize;
    for s in 0..served.light_section_count() {
        assert_eq!(
            served.sky(s),
            expected.sky(s),
            "sky light section {s} on the wire differs from the engine's own output"
        );
        assert_eq!(
            served.block(s),
            expected.block(s),
            "block light section {s} on the wire differs from the engine's own output"
        );
        if matches!(served.sky(s), LightData::Values(_)) {
            varied += 1;
        }
    }
    // Non-vacuity: a column whose every sky section collapsed to a `Uniform` tag
    // would satisfy the equality above while carrying no gradient at all, so the
    // comparison would say nothing about propagation.
    assert!(
        varied > 0,
        "no sky section on the wire carries a per-cell array — this fixture has no gradient, \
         so the equality above is satisfied by two identical constants"
    );
    println!(
        "wire == engine over {} light sections ({varied} carrying per-cell sky arrays)",
        served.light_section_count()
    );
}

/// Block light on the wire, with its own control.
///
/// The generated overworld places almost no emitter near an arbitrary chunk (the
/// survey found 0–171 block-lit cells across ten chunks, and 0 at the fixture
/// used above), so hoping for lava in frame is exactly the *world* species of
/// vacuous test. This places a glowstone through the real
/// `ChunkSource::set_block` — the same path a player's placement takes — and
/// predicts the halo from vanilla's rule: the source cell holds its full emission
/// regardless of its own opacity (a glowstone is opaque yet lit), and each air
/// step costs `max(1, dampening) = 1`.
///
/// The control is the same three cells **before** the edit: they must read 0, or
/// the assertion is measuring pre-existing light rather than the emitter.
#[test]
fn served_block_light_carries_a_placed_emitters_halo() {
    let shape = ChunkShape::overworld_1_21();
    let source = overworld_chunk_source(SEED);

    // High above any terrain (world height 384, surface well under y = 200), so
    // the cells around the emitter are open air and the predicted halo is not
    // clipped by rock. Chunk-local (8, 8) of the fixture column.
    let (lx, lz) = (8usize, 8usize);
    let y = 200;
    let (wx, wz) = (CX * 16 + lx as i32, CZ * 16 + lz as i32);

    let before = served_light(CX, CZ, &source, &shape);
    for dx in 0..3usize {
        let got = block_at(&before, shape.min_y, lx + dx, y, lz)
            .expect("block light section is present on the wire");
        assert_eq!(
            got,
            0,
            "CONTROL: block light at local ({}, {y}, {lz}) is {got}, not 0, before anything was \
             placed — the halo assertion below would be measuring something else",
            lx + dx
        );
    }
    println!("control: all three halo cells read 0 before the placement");

    source.set_block(wx, y, wz, "minecraft:glowstone");
    let after = served_light(CX, CZ, &source, &shape);

    for (dx, expect, why) in [
        (
            0usize,
            15u8,
            "the source cell holds its emission even though glowstone is opaque",
        ),
        (1, 14, "one air step costs max(1, dampening(air)) = 1"),
        (2, 13, "two air steps"),
    ] {
        let got = block_at(&after, shape.min_y, lx + dx, y, lz)
            .expect("block light section present after the placement");
        assert_eq!(
            got, expect,
            "block light at local ({}, {y}, {lz}) is {got}, expected {expect} ({why})",
            lx + dx
        );
    }
    // 15 levels of emission reach exactly 15 blocks; the 16th is dark. This is
    // the assertion a "just make it bright" implementation fails.
    let beyond = block_at(&after, shape.min_y, lx, y + 15, lz).expect("present");
    assert_eq!(
        beyond, 0,
        "block light 15 blocks above a 15-emitter must be 0, got {beyond} — emission that does \
         not terminate is not vanilla's rule"
    );
    println!("placed-glowstone halo on the wire: 15 / 14 / 13, and 0 at 15 blocks out");
}

/// The seam measurement: how far the shipped isolated compute sits from the exact
/// 3×3 compute, **where**, and in which direction.
#[test]
fn seam_residual_is_bounded_and_never_brighter_than_the_exact_compute() {
    let shape = ChunkShape::overworld_1_21();
    let source = overworld_chunk_source(SEED);
    let cols = neighbourhood_columns(&source, &shape, CX, CZ);

    let isolated = compute_column_light(centre_of(&cols), &Props);
    let exact = exact_light(&cols);

    let mut compared = 0usize;
    let mut differ = 0usize;
    let mut brighter = 0usize;
    let mut max_delta = 0u8;
    let mut bbox: Option<(usize, usize, i32, i32, usize, usize)> = None;
    let mut worst: Option<(&str, usize, i32, usize, u8, u8)> = None;
    let mut horizontal_gradient = 0usize;

    for s in 0..exact.light_section_count() {
        for y in 0..EDGE {
            let wy = shape.min_y + (s as i32 - 1) * EDGE as i32 + y as i32;
            for z in 0..EDGE {
                for x in 0..EDGE {
                    let i = NibbleArray::index(x, y, z);
                    for (layer, mine, theirs) in [
                        (
                            "sky",
                            isolated.sky(s).get(i).unwrap_or(0),
                            exact.sky(s).get(i).unwrap_or(0),
                        ),
                        (
                            "block",
                            isolated.block(s).get(i).unwrap_or(0),
                            exact.block(s).get(i).unwrap_or(0),
                        ),
                    ] {
                        compared += 1;
                        if mine == theirs {
                            continue;
                        }
                        differ += 1;
                        if mine > theirs {
                            brighter += 1;
                        }
                        let delta = mine.abs_diff(theirs);
                        if delta > max_delta {
                            max_delta = delta;
                            worst = Some((layer, x, wy, z, mine, theirs));
                        }
                        bbox = Some(match bbox {
                            None => (x, x, wy, wy, z, z),
                            Some((x0, x1, y0, y1, z0, z1)) => (
                                x0.min(x),
                                x1.max(x),
                                y0.min(wy),
                                y1.max(wy),
                                z0.min(z),
                                z1.max(z),
                            ),
                        });
                    }
                    // Non-vacuity, over the exact compute: a *horizontal* sky
                    // gradient. `1..=14` alone is not enough — vertical decay
                    // through water produces plenty of it while never exercising
                    // sideways propagation, which is how the seed-1234 fixture
                    // looked rigorous and proved nothing.
                    let sky = exact.sky(s).get(i).unwrap_or(0);
                    if x + 1 < EDGE
                        && exact
                            .sky(s)
                            .get(NibbleArray::index(x + 1, y, z))
                            .unwrap_or(0)
                            != sky
                    {
                        horizontal_gradient += 1;
                    }
                }
            }
        }
    }

    let fraction = differ as f64 / compared as f64;
    println!("seam residual (isolated vs exact 3x3), chunk ({CX}, {CZ}) @ seed {SEED}:");
    println!("  cells compared           : {compared}");
    println!(
        "  cells differing          : {differ} ({:.4}% ; ceiling {:.2}%)",
        fraction * 100.0,
        SEAM_RESIDUAL_CEILING * 100.0
    );
    println!("  isolated brighter        : {brighter} (hard-asserted 0)");
    println!("  max |delta|              : {max_delta}");
    println!("  bounding box (x0..x1, y0..y1, z0..z1): {bbox:?}");
    if let Some((layer, x, y, z, mine, theirs)) = worst {
        println!(
            "  worst cell               : {layer} at local ({x}, {y}, {z}) — isolated {mine} vs \
             exact {theirs}"
        );
    }
    println!("  horizontal sky gradient  : {horizontal_gradient} cells (must be > 0)");

    assert!(
        horizontal_gradient > 0,
        "no cell's sky level differs from its +x neighbour at the same y, so this fixture never \
         exercises horizontal sky propagation — every claim here would be vacuous. Pick a chunk \
         with real relief (the module docs record the survey that chose this one)."
    );

    // The hard direction claim. Adding sources can only *raise* light, so the
    // isolated compute must be ≤ the exact compute everywhere. A cell where it is
    // brighter cannot come from a missing neighbour, so it is a real defect.
    assert_eq!(
        brighter, 0,
        "the isolated compute is BRIGHTER than the exact 3x3 compute at {brighter} cell(s) — \
         impossible from a missing neighbour, so this is an over-propagation / nibble-order / \
         section off-by-one defect. Bounding box {bbox:?}, worst cell {worst:?}"
    );

    assert!(
        fraction <= SEAM_RESIDUAL_CEILING,
        "cross-chunk seam residual {differ}/{compared} ({:.4}%) exceeds the pinned ceiling \
         {:.2}%. Bounding box {bbox:?}, worst cell {worst:?}. Either the generator grew terrain \
         whose light genuinely spans a chunk border (land the brokered lodestone-server patch in \
         DESIGN.md §12.117, which takes this to 0) or the isolated compute regressed.",
        fraction * 100.0,
        SEAM_RESIDUAL_CEILING * 100.0
    );
}

/// The control for the gate above: proof the seam detector **fires**.
///
/// "The residual is under 0.2%" is an absence claim, and an absence claim is worth
/// what the evidence says the detector would have caught. This constructs the
/// exact situation the isolated compute cannot see — a neighbour column holding
/// the only light source within reach of the centre's border — and requires the
/// comparison to report it, at the predicted level and the predicted `x`.
///
/// Before believing it, ask what else paints here: the cell sits well above
/// terrain in open air, where **sky** light is already 15 in both computes, so a
/// difference can only come from the *block* layer. That is why the assertions
/// name the layer rather than reading "the light".
#[test]
fn seam_detector_fires_when_a_neighbour_holds_the_only_source() {
    let shape = ChunkShape::overworld_1_21();
    let source = overworld_chunk_source(SEED);

    // Glowstone in the west neighbour at its local x = 15 — the cell sharing a
    // face with the centre's local x = 0 — high in open air.
    let y = 200;
    let z_local = 8usize;
    let wx = (CX - 1) * 16 + 15;
    let wz = CZ * 16 + z_local as i32;
    source.set_block(wx, y, wz, "minecraft:glowstone");

    let cols = neighbourhood_columns(&source, &shape, CX, CZ);
    let isolated = compute_column_light(centre_of(&cols), &Props);
    let exact = exact_light(&cols);

    let iso_edge = block_at(&isolated, shape.min_y, 0, y, z_local).expect("present");
    let exact_edge = block_at(&exact, shape.min_y, 0, y, z_local).expect("present");
    println!(
        "seam detector control: centre local (0, {y}, {z_local}) block light — isolated \
         {iso_edge}, exact {exact_edge}"
    );

    assert_eq!(
        iso_edge, 0,
        "the isolated compute must see nothing across the seam; got {iso_edge}, so this control \
         is not measuring what it claims"
    );
    assert_eq!(
        exact_edge, 14,
        "the exact compute must carry the neighbour's glowstone across the seam at 15 − 1 = 14; \
         got {exact_edge}"
    );
    // …and one block further in, 13. Two cells rather than one, so a detector that
    // happened to fire on a single boundary artefact cannot satisfy this.
    assert_eq!(
        block_at(&exact, shape.min_y, 1, y, z_local).expect("present"),
        13,
        "two air steps from the neighbour's emitter"
    );
    assert_eq!(
        block_at(&isolated, shape.min_y, 1, y, z_local).expect("present"),
        0,
        "still nothing in the isolated compute"
    );
}

/// Light's per-column cost, as a **ratio against work the serve path already
/// did**, both arms timed in the same process.
///
/// An absolute millisecond figure on this machine gets attributed to the wrong
/// cause — three other agents build concurrently, and a debug-vs-release story
/// here was once pure machine load. So the measurement is a ratio against column
/// *generation*, the dominant term the serve path pays unconditionally, timed in
/// the same run over freshly-coordinated columns.
///
/// Measured in release while landing server-side light: `compute_column_light` ≈ **1.0 ms per
/// column**, column generation ≈ **61 ms**, so light is ~1.6% of generation and
/// ~2% of a 50 ms tick. For contrast the exact 3×3 compute is ≈ **9.7 ms per
/// column** — fine *in the chunk source*, alongside generation on the blocking
/// pool; not fine on the net task, which is the other half of why the shipped
/// compute is the isolated one.
#[test]
fn light_cost_per_column_stays_a_small_fraction_of_generation() {
    let shape = ChunkShape::overworld_1_21();
    let source = overworld_chunk_source(SEED);

    // Fresh coordinates per rep so generation is really paid each time.
    const REPS: i32 = 3;
    let t_gen = Instant::now();
    let mut columns = Vec::new();
    for i in 0..REPS {
        columns.push(source.column(400 + i * 7, 400 + i * 11));
    }
    let gen_elapsed = t_gen.elapsed();

    let world: Vec<WorldColumn> = columns.iter().map(|c| to_world_column(&shape, c)).collect();

    let t_light = Instant::now();
    for column in &world {
        std::hint::black_box(compute_column_light(column, &Props));
    }
    let light = t_light.elapsed();

    let ratio = light.as_secs_f64() / gen_elapsed.as_secs_f64();
    println!("per-column cost over {REPS} freshly generated columns, same process:");
    println!("  column generation   : {:?}", gen_elapsed / REPS as u32);
    println!("  compute_column_light: {:?}", light / REPS as u32);
    println!("  light / generation  : {ratio:.4}");

    assert!(
        gen_elapsed.as_secs_f64() > 0.0 && light.as_secs_f64() > 0.0,
        "one arm measured zero time — the ratio would be meaningless"
    );
    assert!(
        ratio <= LIGHT_OVER_GENERATION_CEILING,
        "the light flood costs {ratio:.4} of column generation ({:?} vs {:?} per column), over \
         the {:.0}% ceiling. The serve path is the path that until recently never finished \
         streaming a view, so light must not become one of its dominant terms.",
        light / REPS as u32,
        gen_elapsed / REPS as u32,
        LIGHT_OVER_GENERATION_CEILING * 100.0
    );
}
