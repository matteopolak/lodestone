//! Issue #507's real fix, gated: `ChunkColumn`'s incrementally maintained
//! per-section random-tick counters must agree with the definitional scan they
//! replaced, at every step of a mutation storm and across both construction
//! entry points, and the O(1) decision must leave the position LCG on exactly
//! the sequence the scan would have.
//!
//! # Why the recount here is hand-written
//!
//! Every expected value below is produced by code in *this file*, written
//! against `ChunkColumn`'s two raw accessors (`raw_blocks`/`raw_palette`) and
//! the public `is_randomly_ticking` predicate — deliberately **not** by calling
//! `random_tick`'s own scan helpers. A shared bookkeeping bug cannot then pass
//! both arms. The predicate itself *is* shared, disclosed: it is the spec's
//! definition of "randomly ticking", and the thing under test is the
//! bookkeeping, not the classification.
//!
//! The cell layout the recount walks is restated from
//! `ChunkColumn::raw_blocks`'s own documented formula
//! (`blocks[(y_local * 16 + z) * 16 + x]`) rather than reusing production's
//! flattened `cell / 4096` shortcut, so an arithmetic slip in production's
//! section indexing shows up here as a disagreement instead of being copied.
//!
//! # The three controls, and what each proves
//!
//! | control | proves |
//! |---|---|
//! | `corrupting_a_counter_makes_the_parity_comparison_fail` | the count comparison detects a wrong **production** counter (not the recount comparing itself) |
//! | `corrupting_a_counter_trips_the_consumption_site_tripwire` | `tick_chunk`'s `debug_assert!` really fires, naming the section |
//! | `a_wrong_draw_count_does_not_reproduce_the_lcg_stream` | the LCG-stream equality is not satisfiable by an arbitrary draw count |

use lodestone_server::{
    ChunkColumn, ChunkSource, RandomTickEvent, RandomTickScheduler, ScheduledTickQueue,
    chunk_nbt, is_randomly_ticking, next_random_tick_pos, overworld_chunk_source,
};

/// Rows per implicit section. Restated here rather than imported: production's
/// own constant is crate-private, and a gate that shares the constant cannot
/// notice production changing it.
const SECTION_ROWS: i32 = 16;

/// A stage-1 sapling. Randomly ticking (`is_sapling`), and its handler is a
/// named no-op at stage 1 (`SaplingOutcome::TreeGrowthNotModeled`), so it never
/// mutates anything. That makes it the one ticking state a draw-sequence gate
/// can plant freely without the column changing underneath the replay.
const INERT_TICKING: &str = "minecraft:oak_sapling[stage=1]";
/// Grass with something **solid** above dies to dirt on its first hit and, because
/// `canStayAlive` is false, draws zero behaviour values and never spreads — one
/// event, no new ticking states anywhere.
///
/// "Solid" is load-bearing since issue #549: `canStayAlive` is
/// `dampening(above) < 15`, not "above is air", so a `short_grass` cap would leave
/// this grass **alive** and spending 12 behaviour draws per hit. Every fixture
/// below caps with [`STONE`] for that reason.
const GRASS: &str = "minecraft:grass_block[snowy=false]";
const STONE: &str = "minecraft:stone";
const DIRT: &str = "minecraft:dirt";

// ---------------------------------------------------------------------------
// The independent recount
// ---------------------------------------------------------------------------

/// How many cells in each 16-row window (counted from `min_y`) hold a
/// randomly-ticking state, recomputed from scratch. See this file's module doc
/// for why it is written this way.
fn recount_sections(column: &ChunkColumn) -> Vec<u32> {
    let ticking: Vec<bool> = column
        .raw_palette()
        .iter()
        .map(|state| is_randomly_ticking(state))
        .collect();
    let blocks = column.raw_blocks();
    let sections = (column.height as usize).div_ceil(SECTION_ROWS as usize);
    let mut counts = vec![0u32; sections];
    for y_local in 0..column.height {
        for z in 0..16i32 {
            for x in 0..16i32 {
                let cell = ((y_local * 16 + z) * 16 + x) as usize;
                if ticking[blocks[cell] as usize] {
                    counts[(y_local / SECTION_ROWS) as usize] += 1;
                }
            }
        }
    }
    counts
}

/// Compares the column's maintained counters against [`recount_sections`].
///
/// Returns `Err` with the disagreeing sections named, rather than asserting, so
/// the *same* comparison serves the passing arms (`.expect`) and the negative
/// control (`assert!(… .is_err())`). One comparison, two directions — a control
/// that runs a different comparison from the gate proves nothing about the gate.
fn compare_counters(column: &ChunkColumn, what: &str) -> Result<(), String> {
    let expected = recount_sections(column);
    let actual = column.section_ticking_counts();
    if actual.len() != expected.len() {
        return Err(format!(
            "{what}: counter table has {} sections, the recount found {}",
            actual.len(),
            expected.len()
        ));
    }
    let mismatches: Vec<String> = expected
        .iter()
        .zip(actual)
        .enumerate()
        .filter(|(_, (e, a))| u32::from(**a) != **e)
        .map(|(i, (e, a))| {
            format!(
                "section {i} (y {}..{}): counter {a}, recount {e}",
                column.min_y + i as i32 * SECTION_ROWS,
                column.min_y + (i as i32 + 1) * SECTION_ROWS
            )
        })
        .collect();
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!("{what}: {} section(s) disagree — {}", mismatches.len(), mismatches.join("; ")))
    }
}

/// Which sections the *definition* says randomly tick, from the recount.
fn definitional_booleans(column: &ChunkColumn) -> Vec<bool> {
    recount_sections(column).into_iter().map(|c| c > 0).collect()
}

/// A real generator column at a surface chunk, through the source production
/// serves (`OverworldChunkSource::column` → `ChunkColumn::from_generated` →
/// `recalc_ticking_counts`). Not a hand-rolled `ChunkSource`: §12.43's question
/// is "which implementation does this test's transport resolve to", and this is
/// the one.
fn generated_surface_column(seed: i64, cx: i32, cz: i32) -> ChunkColumn {
    let source = overworld_chunk_source(seed);
    ChunkSource::column(&source, cx, cz)
}

/// Caps some surface grass with stone and exposes a dirt cell beside other grass,
/// so a generated column has a **named** reason to mutate under ticking.
///
/// See `counters_survive_real_ticking_over_a_generated_column` for why this exists
/// rather than trusting the terrain. Both triggers are asserted, so a generator
/// change that stops putting grass at the surface fails here loudly instead of
/// quietly producing a zero-mutation run.
fn plant_a_named_mutation_source(column: &mut ChunkColumn) {
    let top_grass = |column: &ChunkColumn, lx: i32, lz: i32| -> Option<i32> {
        (column.min_y..column.min_y + column.height)
            .rev()
            .find(|&y| column.block_state(lx, y, lz).starts_with("minecraft:grass_block"))
    };

    // Trigger 1: stone directly above grass. `canStayAlive` is false for a full
    // solid (dampening 15), so each of these dies to dirt on its first hit —
    // zero behaviour draws, one event.
    //
    // A **6x6 patch**, not three cells, and the size is a probability argument
    // rather than taste: the caller makes `24 * 64 = 1536` position picks over a
    // 4,096-cell section, so `n` capped cells give `1536 * n / 4096` expected
    // deaths. Three cells is `1.1` expected and `P(zero) ~ 33%` — a flaky test.
    // Thirty-six is `13.5` expected and `P(zero) ~ 1.4e-6`.
    let mut capped = 0usize;
    for lx in 2..8 {
        for lz in 2..8 {
            if let Some(y) = top_grass(column, lx, lz) {
                column.set_block(lx, y + 1, lz, STONE);
                capped += 1;
            }
        }
    }
    assert!(
        capped >= 30,
        "only {capped} of 36 cells in the patch had surface grass to cap; below \
         ~30 the expected death count drops far enough that a zero-event run \
         becomes plausible and the assertion downstream turns flaky"
    );

    // Trigger 2: bare dirt beside live grass, which the spread branch can claim.
    // Outside the capped patch, so the two triggers cannot be confused.
    let mut exposed = 0usize;
    for (lx, lz) in [(11, 11), (11, 12)] {
        if let Some(y) = top_grass(column, lx, lz) {
            column.set_block(lx, y + 1, lz, "minecraft:air");
            column.set_block(lx, y, lz, DIRT);
            exposed += 1;
        }
    }
    assert!(
        exposed > 0,
        "no dirt target could be exposed next to grass, so the spread branch is \
         unreachable in this fixture"
    );
}

/// The world-species precondition, as a hard failure rather than a skip: a
/// fixture with nothing ticking, or with *everything* ticking, structurally
/// cannot exercise a per-section ticking counter. An all-stone column would pass
/// every assertion below while proving nothing.
fn assert_fixture_can_exercise_the_counters(column: &ChunkColumn, what: &str) {
    let counts = recount_sections(column);
    let ticking = counts.iter().filter(|&&c| c > 0).count();
    let quiet = counts.iter().filter(|&&c| c == 0).count();
    assert!(
        ticking >= 1,
        "{what}: no section holds a randomly-ticking block, so this fixture cannot exercise \
         the counter at all (counts: {counts:?})"
    );
    assert!(
        quiet >= 1,
        "{what}: every section already ticks, so a 0<->1 crossing cannot be observed \
         (counts: {counts:?})"
    );
}

// ---------------------------------------------------------------------------
// Gate A — incremental counters vs. an independent recount
// ---------------------------------------------------------------------------

/// **Gate A.** After *every step* of a scripted mutation storm, and across an
/// NBT round trip, the maintained counters must equal the independent recount.
///
/// The storm's coverage is asserted, not assumed — each transition sets a flag
/// and the flags are checked at the end, so a future edit that accidentally
/// drops (say) the `1 -> 0` crossing fails here instead of silently narrowing
/// the gate.
///
/// Both construction entry points are covered because they are genuinely
/// different mechanisms, not two spellings of one: `from_generated` bulk-adopts
/// a populated grid and then runs the one counting pass
/// (`recalc_ticking_counts`), while the NBT/region loader builds through
/// `ChunkColumn::new` + a per-cell `set_block` and so is covered by incremental
/// maintenance alone, with no recount anywhere.
#[test]
fn incremental_counters_match_an_independent_recount_through_a_mutation_storm() {
    let mut column = generated_surface_column(1_507, 4, -7);
    assert_fixture_can_exercise_the_counters(&column, "generated surface column");
    compare_counters(&column, "as generated (from_generated + recalc)").expect("baseline");

    let min_y = column.min_y;
    let top_section_min_y = min_y + ((column.height - 1) / SECTION_ROWS) * SECTION_ROWS;

    // A section that currently ticks nothing — the subject of the 0<->1
    // crossings. Picked from the recount, so it is the *definition* choosing it,
    // and deliberately **not** the bottom or top window: those two are the
    // subjects of the partial-window arm at the end, and if all three arms
    // landed on one section index the storm would never exercise the section
    // arithmetic it exists to exercise. (Measured: a generated surface column
    // has exactly one ticking section, so this is a real constraint rather than
    // a theoretical one — `position(|c| c == 0)` alone returns index 0.)
    let counts = recount_sections(&column);
    let last_index = counts.len() - 1;
    let quiet_index = (1..last_index)
        .find(|&i| counts[i] == 0)
        .expect("fixture must have a quiet section that is neither the bottom nor the top window");
    let quiet_min_y = min_y + quiet_index as i32 * SECTION_ROWS;

    let mut crossed_up = false;
    let mut crossed_down = false;

    let mut step = 0usize;
    let mut apply = |column: &mut ChunkColumn, x, y, z, state: &str, note: &str| {
        column.set_block(x, y, z, state);
        step += 1;
        compare_counters(column, &format!("step {step}: {note} ({state} at ({x}, {y}, {z}))"))
            .expect("counter parity");
    };

    // 0 -> 1, then 1 -> 2, in a section that held nothing ticking.
    let before = recount_sections(&column)[quiet_index];
    apply(&mut column, 1, quiet_min_y + 2, 1, GRASS, "0 -> 1 in a quiet section");
    let after_first = recount_sections(&column)[quiet_index];
    crossed_up = crossed_up || (before == 0 && after_first == 1);
    apply(&mut column, 2, quiet_min_y + 3, 2, GRASS, "1 -> 2 in the same section");

    // 2 -> 1 -> 0. Dirt does not tick (only grass does), so each of these is a
    // real decrement and the second one crosses back through zero.
    apply(&mut column, 1, quiet_min_y + 2, 1, DIRT, "2 -> 1");
    apply(&mut column, 2, quiet_min_y + 3, 2, DIRT, "1 -> 0 (last ticking block removed)");
    crossed_down = crossed_down || recount_sections(&column)[quiet_index] == 0;

    // ticking -> ticking: a crop age advance. The count must be *unchanged*.
    apply(&mut column, 5, quiet_min_y + 5, 5, "minecraft:wheat[age=3]", "plant a crop");
    let with_crop = recount_sections(&column)[quiet_index];
    apply(&mut column, 5, quiet_min_y + 5, 5, "minecraft:wheat[age=4]", "ticking -> ticking");
    let after_age = recount_sections(&column)[quiet_index];
    assert_eq!(
        with_crop, after_age,
        "a ticking -> ticking replacement must not change the count"
    );
    assert_eq!(
        u32::from(column.section_ticking_counts()[quiet_index]),
        after_age,
        "…and the maintained counter must agree"
    );
    let ticking_to_ticking = with_crop == after_age && with_crop > 0;

    // Same-state rewrite: a no-op delta.
    apply(&mut column, 5, quiet_min_y + 5, 5, "minecraft:wheat[age=4]", "same-state rewrite");
    let same_state_rewrite =
        u32::from(column.section_ticking_counts()[quiet_index]) == after_age;

    // non-ticking -> non-ticking. Both classifications are checked here so this
    // step cannot silently become a ticking transition if the predicate widens.
    assert!(!is_randomly_ticking(STONE) && !is_randomly_ticking(DIRT));
    apply(&mut column, 7, quiet_min_y + 7, 7, STONE, "seed a non-ticking cell");
    let before_quiet = column.section_ticking_counts()[quiet_index];
    apply(&mut column, 7, quiet_min_y + 7, 7, DIRT, "non-ticking -> non-ticking");
    let quiet_to_quiet = column.section_ticking_counts()[quiet_index] == before_quiet;

    // Partial-window indexing: the bottom and top sections, written and cleared.
    // The flags below are *observations of the counter moving in the intended
    // window*, not literal `true`s — a hardcoded flag here would assert nothing
    // about section arithmetic, which is the whole point of this arm. Both
    // indices are distinct from `quiet_index` by construction (see its pick).
    assert_eq!(column.section_ticking_counts()[0], 0, "bottom window must start quiet");
    apply(&mut column, 0, min_y, 0, GRASS, "bottom section, 0 -> 1");
    let bottom_rose = column.section_ticking_counts()[0] == 1;
    apply(&mut column, 0, min_y, 0, DIRT, "bottom section, 1 -> 0");
    let wrote_bottom_section = bottom_rose && column.section_ticking_counts()[0] == 0;

    let top_y = (top_section_min_y + 1).min(min_y + column.height - 1);
    assert_eq!(
        (top_y - min_y) as usize / SECTION_ROWS as usize,
        last_index,
        "the top-section write must land in the last window"
    );
    assert_eq!(column.section_ticking_counts()[last_index], 0, "top window must start quiet");
    apply(&mut column, 15, top_y, 15, GRASS, "top section, 0 -> 1");
    let top_rose = column.section_ticking_counts()[last_index] == 1;
    apply(&mut column, 15, top_y, 15, DIRT, "top section, 1 -> 0");
    let wrote_top_section = top_rose && column.section_ticking_counts()[last_index] == 0;

    assert!(step >= 12, "the storm ran only {step} steps");
    assert!(crossed_up, "the storm never crossed a section from 0 to 1");
    assert!(crossed_down, "the storm never crossed a section from 1 back to 0");
    assert!(ticking_to_ticking, "the storm never made a ticking -> ticking replacement");
    assert!(same_state_rewrite, "the storm never made a same-state rewrite");
    assert!(quiet_to_quiet, "the storm never made a non-ticking -> non-ticking write");
    assert!(
        wrote_bottom_section,
        "the bottom window's counter did not go 0 -> 1 -> 0 with the writes aimed at it"
    );
    assert!(
        wrote_top_section,
        "the top window's counter did not go 0 -> 1 -> 0 with the writes aimed at it"
    );
    assert_ne!(
        quiet_index, 0,
        "the 0<->1 arm and the bottom-window arm must be different sections"
    );
    assert_ne!(
        quiet_index, last_index,
        "the 0<->1 arm and the top-window arm must be different sections"
    );

    // --- The other construction entry point: `new` + per-cell `set_block`. ---
    let nbt = chunk_nbt::column_to_nbt(4, -7, &column);
    let loaded = chunk_nbt::column_from_nbt(&nbt, column.min_y, column.height)
        .expect("the column we just wrote must read back");
    compare_counters(&loaded, "after an NBT round trip (new + per-cell set_block)")
        .expect("counter parity across the round trip");
    assert_eq!(
        loaded.section_ticking_counts(),
        column.section_ticking_counts(),
        "counters must survive a save/load round trip — the loader builds through `set_block`, \
         so this is incremental maintenance being checked against a recalc'd column"
    );
    assert_fixture_can_exercise_the_counters(&loaded, "round-tripped column");
}

/// **Control for Gate A**, permanent. Corrupts the **production** counter and
/// asserts the same comparison the gate uses reports a mismatch.
///
/// Corrupting the recount instead would be the weaker control: if the gate ever
/// accidentally compared the recount to itself, a recount-side corruption would
/// move both arms together and the control would still "pass".
#[test]
fn corrupting_a_counter_makes_the_parity_comparison_fail() {
    let mut column = generated_surface_column(1_507, 4, -7);
    compare_counters(&column, "before corruption").expect("must start in agreement");

    let quiet_index = recount_sections(&column)
        .iter()
        .position(|&c| c == 0)
        .expect("a generated surface column must have at least one quiet section");
    column.debug_corrupt_section_ticking_count(quiet_index, 1);

    let verdict = compare_counters(&column, "with section counter corrupted by +1");
    let message = verdict.expect_err(
        "control failed: the parity comparison did not notice a counter corrupted by +1, so \
         every passing arm of Gate A proves nothing",
    );
    println!("Gate A control observed: {message}");
    assert!(
        message.contains(&format!("section {quiet_index}")),
        "the failure must name the corrupted section, got: {message}"
    );
}

// ---------------------------------------------------------------------------
// Gate B — the position-LCG draw sequence is unchanged
// ---------------------------------------------------------------------------

/// One tick's expected draw count and the resulting shadow LCG state, replayed
/// from public primitives only: the definitional per-section boolean
/// ([`definitional_booleans`], which never consults the counters) plus
/// [`next_random_tick_pos`]. Returns how many draws it issued.
fn replay_one_tick(column: &ChunkColumn, shadow: &mut i32, cx: i32, cz: i32, tick_speed: u32) -> u32 {
    let booleans = definitional_booleans(column);
    let mut draws = 0;
    for (i, ticks) in booleans.iter().enumerate() {
        if !ticks {
            continue;
        }
        let section_min_y = column.min_y + i as i32 * SECTION_ROWS;
        for _ in 0..tick_speed {
            let _ = next_random_tick_pos(shadow, cx * 16, section_min_y, cz * 16, 15);
            draws += 1;
        }
    }
    draws
}

/// Builds Gate B's fixture: a column with one section holding inert ticking
/// content plus a few grass blocks that die to dirt (so real events are
/// produced), and several sections holding nothing.
///
/// Grass is capped with stone deliberately: `above_is_air` is then false, so it
/// dies to dirt with **zero** behaviour draws and **never spreads**. That is
/// what keeps the shadow replay exact — no tick can create a ticking state in a
/// section the replay has already decided about. The inert sapling keeps the
/// section's boolean `true` for the whole run regardless of how many grass
/// blocks have already died, so within-tick decision changes are impossible by
/// construction, and the gate asserts that afterwards anyway.
fn gate_b_fixture() -> ChunkColumn {
    let mut column = ChunkColumn::new(0, 96);
    // A full 16x16 grass layer capped with stone, all inside section 1 (y
    // 16..32). The layer is a whole y-row on purpose: `next_random_tick_pos`
    // picks y uniformly over the section's 16 rows and (x, z) uniformly over
    // 16x16, so one full row is hit with probability 1/16 per draw. With
    // 24 ticks x 7 draws that is ~10 expected hits — the event stream is
    // non-empty by design, not by luck, and the run is deterministic anyway.
    for z in 0..16 {
        for x in 0..16 {
            column.set_block(x, 18, z, GRASS);
            column.set_block(x, 19, z, STONE);
        }
    }
    // The section's boolean must not depend on how many grass blocks are left,
    // or a mid-tick 1 -> 0 crossing would invalidate the replay's pre-tick
    // snapshot. This one inert ticking cell pins it `true` for the whole run.
    column.set_block(8, 20, 8, INERT_TICKING);
    column
}

/// **Gate B.** `RandomTickScheduler::tick_chunk` driven for K ticks at a
/// **non-default** `tick_speed` must leave the position LCG in exactly the state
/// a replay from `next_random_tick_pos` reaches, with scripted `set_block` edits
/// flipping a section `0 -> 1` (before tick 6) and `1 -> 0` (before tick 16)
/// between ticks.
///
/// `tick_speed = 7`, not `DEFAULT_RANDOM_TICK_SPEED`, on purpose: issue #508
/// (the `random_tick_speed` game rule is still inert — `tick.rs:1166` passes the
/// hardcoded default) is a separate, multi-file wiring task, and nothing here may
/// assume 3. When #508 lands, only the value's *source* changes.
///
/// # Why an equal LCG state is the whole property
///
/// The per-(column, section, tick) boolean is the only input deciding whether
/// `tick_speed` position draws happen. Identical booleans ⇒ identical draw count
/// and order ⇒ identical `position_state` ⇒ identical picked positions ⇒
/// identical behaviour-RNG draw pattern. RNG **draw order and count** is the
/// spec, not just the final world, which is why this compares the LCG state and
/// not only the events.
#[test]
fn the_counter_decision_reproduces_the_definitional_draw_sequence() {
    const TICK_SPEED: u32 = 7;
    const TICKS: usize = 24;
    let (cx, cz) = (2, -3);

    let mut column = gate_b_fixture();
    assert_fixture_can_exercise_the_counters(&column, "gate B fixture");

    let mut scheduler = RandomTickScheduler::new(24_601, 99);
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let mut shadow = 24_601i32;
    let mut events: Vec<RandomTickEvent> = Vec::new();
    let mut total_draws = 0u32;
    let mut per_tick_draws: Vec<u32> = Vec::new();

    // The section the script flips, chosen from the recount so the *definition*
    // picks it. Section 0 (y 0..16) holds nothing in this fixture.
    let flip_section_index = definitional_booleans(&column)
        .iter()
        .position(|&t| !t)
        .expect("fixture must have a quiet section to flip");
    let flip_y = flip_section_index as i32 * SECTION_ROWS + 4;

    for tick in 0..TICKS {
        // Scripted edits BETWEEN ticks: 0 -> 1 before tick 6, 1 -> 0 before tick 16.
        if tick == 6 {
            column.set_block(6, flip_y, 6, INERT_TICKING);
            assert!(
                definitional_booleans(&column)[flip_section_index],
                "the 0 -> 1 flip did not take"
            );
        }
        if tick == 16 {
            column.set_block(6, flip_y, 6, STONE);
            assert!(
                !definitional_booleans(&column)[flip_section_index],
                "the 1 -> 0 flip did not take"
            );
        }

        let booleans_before = definitional_booleans(&column);
        let draws = replay_one_tick(&column, &mut shadow, cx, cz, TICK_SPEED);
        per_tick_draws.push(draws);
        total_draws += draws;

        events.extend(scheduler.tick_chunk(&mut column, cx, cz, TICK_SPEED, &mut block_ticks, 0));

        // The replay above read the column as it stood at the start of the tick.
        // That is exact only if no mutation inside the tick changed any section's
        // decision — asserted, not assumed. The fixture is built so it cannot
        // (see `gate_b_fixture`), and this is what would catch a fixture that
        // stopped satisfying it.
        assert_eq!(
            definitional_booleans(&column),
            booleans_before,
            "tick {tick} changed a section's ticking decision mid-tick, so the shadow replay's \
             pre-tick snapshot is no longer an exact expectation — fix the fixture, not this \
             assertion"
        );
        assert_eq!(
            scheduler.position_state(),
            shadow,
            "tick {tick}: the scheduler's position LCG diverged from the replay. The counter \
             decision put the `tick_speed` draws on a different sequence than the definitional \
             scan would have"
        );
        compare_counters(&column, &format!("after tick {tick}")).expect("counter parity");
    }

    // --- Vacuity guards, asserted rather than assumed. ---
    assert!(total_draws > 0, "the whole run issued zero position draws");
    assert_eq!(
        shadow,
        scheduler.position_state(),
        "final LCG state must match the replay exactly"
    );
    // A flip that changed no decision would mean the script never exercised the
    // transition the counter exists to track.
    assert!(
        per_tick_draws[6] > per_tick_draws[5],
        "the 0 -> 1 flip did not increase the next tick's draw count ({} -> {})",
        per_tick_draws[5],
        per_tick_draws[6]
    );
    assert!(
        per_tick_draws[16] < per_tick_draws[15],
        "the 1 -> 0 flip did not decrease the next tick's draw count ({} -> {})",
        per_tick_draws[15],
        per_tick_draws[16]
    );
    assert_eq!(
        per_tick_draws[6] - per_tick_draws[5],
        TICK_SPEED,
        "one section flipping on must add exactly `tick_speed` draws"
    );
    // The events are the second half of the parity claim: same decisions must
    // also mean the same world changes. A run with no events would leave that
    // half untested.
    assert!(
        !events.is_empty(),
        "the run produced no `RandomTickEvent` at all, so the event-stream half of this gate \
         is vacuous — the grass blocks were never hit"
    );
    assert!(
        events.iter().any(|e| e.to == DIRT),
        "expected at least one grass -> dirt event, got: {events:?}"
    );
}

/// **Control for Gate B's LCG assertion.** A replay with the wrong draw count
/// per section must *not* reproduce the scheduler's state — otherwise the
/// equality above would be satisfied by anything.
#[test]
fn a_wrong_draw_count_does_not_reproduce_the_lcg_stream() {
    const TICK_SPEED: u32 = 7;
    let (cx, cz) = (2, -3);
    let mut column = gate_b_fixture();
    let mut scheduler = RandomTickScheduler::new(24_601, 99);
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();

    let mut right = 24_601i32;
    let mut wrong = 24_601i32;
    for _ in 0..4 {
        let _ = replay_one_tick(&column, &mut right, cx, cz, TICK_SPEED);
        let _ = replay_one_tick(&column, &mut wrong, cx, cz, TICK_SPEED + 1);
        scheduler.tick_chunk(&mut column, cx, cz, TICK_SPEED, &mut block_ticks, 0);
    }
    assert_eq!(scheduler.position_state(), right, "the correct replay must match");
    assert_ne!(
        scheduler.position_state(), wrong,
        "control failed: a replay drawing `tick_speed + 1` positions per section reached the \
         same LCG state, so the equality assertion in Gate B cannot distinguish draw counts"
    );
    println!(
        "Gate B control observed: correct replay {right}, wrong-draw-count replay {wrong} — \
         they differ, so the equality is discriminating"
    );
}

/// **Control for the consumption-site tripwire.** With a counter corrupted,
/// `tick_chunk`'s `debug_assert!` must fire and name the section.
///
/// This is the permanent evidence that the tripwire — the thing standing between
/// a future in-file mutation path that forgets the counters and a silently
/// non-ticking world — actually discriminates. `debug_assert!` is compiled out
/// in a release build, so in that configuration the same corruption is asserted
/// to change the *decision* instead, which is the defect the tripwire exists to
/// catch.
#[test]
fn corrupting_a_counter_trips_the_consumption_site_tripwire() {
    let mut column = gate_b_fixture();
    // Corrupt a *quiet* section upward: the counter now claims a section ticks
    // that the definitional scan says does not.
    let quiet_index = definitional_booleans(&column)
        .iter()
        .position(|&t| !t)
        .expect("fixture must have a quiet section");
    column.debug_corrupt_section_ticking_count(quiet_index, 1);
    let section_min_y = column.min_y + quiet_index as i32 * SECTION_ROWS;

    if cfg!(debug_assertions) {
        let mut scheduler = RandomTickScheduler::new(7, 7);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scheduler.tick_chunk(&mut column, 0, 0, 3, &mut block_ticks, 0);
        }));
        let payload = outcome.expect_err(
            "control failed: `tick_chunk` accepted a corrupted counter without tripping its \
             debug tripwire, so the tripwire proves nothing about a future bypass",
        );
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_default();
        println!("tripwire control observed: {message}");
        assert!(
            message.contains("random-tick counter desync"),
            "the tripwire fired but with an unexpected message: {message}"
        );
        assert!(
            message.contains(&format!("section_min_y {section_min_y}")),
            "the tripwire must name the desynced section, got: {message}"
        );
    } else {
        // Release: no `debug_assert!`. The corruption is then only observable as
        // a wrong decision — which is exactly the failure mode being guarded.
        assert!(
            column.section_is_randomly_ticking(section_min_y),
            "control failed: a corrupted counter did not even change the decision, so the \
             corruption hook is not reaching the value production reads"
        );
        assert!(
            !definitional_booleans(&column)[quiet_index],
            "control premise false: the definitional scan agrees the section ticks, so nothing \
             was corrupted"
        );
    }
}

// ---------------------------------------------------------------------------
// Production terrain, real mutations
// ---------------------------------------------------------------------------

/// The counters must survive real ticking over a real generator column — grass
/// spreading, crops, leaves, whatever the surface actually holds — with the
/// debug tripwire live inside `tick_chunk` the whole time.
///
/// This is the arm the hand-built fixture above cannot be: its terrain comes
/// from the production generator, so it exercises mutation shapes (spread into a
/// neighbouring section, cascades through `propagate_and_react`) that a scripted
/// column would not contain.
/// # Why the mutation source is now planted rather than assumed (issue #549)
///
/// This arm used to rely on "whatever the surface actually holds" producing
/// mutations, and that premise **silently became false**. Before #549, grass died
/// to dirt under *any* non-air block, and vanilla's own vegetation step covers
/// grass with `short_grass` — so a generated surface column mutated constantly, by
/// accident, because of a bug. With `canStayAlive` modelled properly the surface
/// is stable: grass under short grass survives, and there is no exposed dirt next
/// to it to spread onto. Twenty-four ticks produced **zero** events and the
/// assertion below fired.
///
/// That is a premise-false control, so the fix is to make the source explicit and
/// assert it, not to weaken the assertion: the helper caps a few surface grass
/// blocks with stone (a configuration a player creates constantly) and exposes a
/// dirt cell beside another, each checked as a precondition. The terrain is still
/// the production generator's; only the trigger is named.
#[test]
fn counters_survive_real_ticking_over_a_generated_column() {
    let mut column = generated_surface_column(1_507, 4, -7);
    assert_fixture_can_exercise_the_counters(&column, "generated column under real ticking");
    plant_a_named_mutation_source(&mut column);

    let mut scheduler = RandomTickScheduler::new(4_242, 4_242);
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let mut events = Vec::new();
    for tick in 0..24 {
        // A deliberately large `tick_speed`: the point is to land real hits on
        // real surface blocks within a short run, so mutations (and their
        // neighbour cascades) actually happen.
        events.extend(scheduler.tick_chunk(&mut column, 4, -7, 64, &mut block_ticks, 0));
        compare_counters(&column, &format!("generated column after tick {tick}"))
            .expect("counter parity under real mutations");
    }
    // `1536` picks over a 4,096-cell section with ~36 capped grass cells gives
    // ~13.5 expected deaths, so a handful is the prediction and zero would mean
    // the planted trigger never fired. Not asserted exactly, because the pick
    // stream is shared with every other ticking state in the column.
    assert!(
        events.len() >= 3,
        "24 ticks at tick_speed 64 over a real surface column with ~36 capped grass \
         cells produced only {} mutation(s); ~13.5 were predicted, and a near-zero \
         count means the planted trigger is not firing — this arm would then prove \
         nothing about maintenance under real ticking",
        events.len(),
    );
    println!(
        "generated column: {} events over 24 ticks, counters {:?}",
        events.len(),
        column.section_ticking_counts()
    );
}

// ---------------------------------------------------------------------------
// The test hook must never become production code
// ---------------------------------------------------------------------------

/// `ChunkColumn::debug_corrupt_section_ticking_count` is plain `pub` (an
/// integration test cannot see a `#[cfg(test)]` item), so nothing but this
/// census stops it drifting into `src/`. It deliberately breaks the counter
/// invariant; a production caller would make every random-tick decision in the
/// world unsound.
#[test]
fn no_production_code_corrupts_the_ticking_counter() {
    const HOOK: &str = "debug_corrupt_section_ticking_count";
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src/ must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    // An audit that scans nothing is a failure to run, not a pass.
    assert!(files.len() > 20, "only {} source files found under {src:?}", files.len());

    let mut hits: Vec<(String, usize, String)> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).expect("source must be readable");
        for (i, line) in text.lines().enumerate() {
            if line.contains(HOOK) {
                hits.push((
                    path.strip_prefix(&src).unwrap_or(path).display().to_string(),
                    i + 1,
                    line.trim().to_string(),
                ));
            }
        }
    }
    // Exactly two: the `#[doc(hidden)]`-annotated definition and the one
    // sentence of its doc comment that names it. Anything else is a caller.
    let callers: Vec<&(String, usize, String)> = hits
        .iter()
        .filter(|(_, _, line)| !line.starts_with("///") && !line.starts_with("pub fn"))
        .collect();
    assert!(
        callers.is_empty(),
        "{HOOK} is called from production source — it corrupts the random-tick counters by \
         design and must exist only for the negative controls in this file: {callers:?}"
    );
    assert!(
        hits.iter().any(|(f, _, _)| f == "chunk.rs"),
        "the census found no definition at all, so it is scanning the wrong tree: {hits:?}"
    );
}
