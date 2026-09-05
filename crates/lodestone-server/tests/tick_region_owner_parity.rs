//! A finite ownership-parity gate for the serial chunk tick plan.
//!
//! The production arm advances a seeded set of real [`ChunkColumn`]s through
//! [`TickRegionPlan::owned_chunks`], exactly as the random-tick pass does. The
//! reference arm does not use that plan: it assigns those columns to two-cell
//! physical regions, lets those region owners produce their local block-update
//! batches independently, then restores the fixture's serial publication order.
//! The compared result is the client-visible block-update stream plus the
//! resulting watched block states, rather than owner-visit counters.
//!
//! The fixture plants one capped grass block at each position selected by the
//! known seeded draw. Every correct visit therefore emits a grass-to-dirt update.
//! Swapping or duplicating a visit changes which column receives a draw and
//! leaves a different update stream and state behind; the detector controls at
//! the end prove the comparison observes both faults.

use std::collections::BTreeMap;

use lodestone_server::{
    ChunkColumn, ChunkSource, RandomTickEvent, RandomTickScheduler, ScheduledTickQueue,
    next_random_tick_pos,
    tick_region::{TickOwnedChunk, TickOwner, TickRegionPlan},
};

type Chunk = (i32, i32);
type BlockPos = (i32, i32, i32);

const POSITION_SEED: i32 = 19_867;
const BEHAVIOR_SEED: u64 = 0x5eed_cafe;
const REGION_EDGE_CHUNKS: i32 = 2;
const GRASS: &str = "minecraft:grass_block[snowy=false]";
const STONE: &str = "minecraft:stone";
const DIRT: &str = "minecraft:dirt";

// Deliberately not physical-region order: the reference owners must merge their
// separately scheduled work back into this canonical producer order.
const FIXTURE_ORDER: [Chunk; 4] = [(0, 1), (-1, 0), (1, 0), (0, 0)];

/// The seeded terrain and the one client-visible cell each visit must change.
#[derive(Clone)]
struct Fixture {
    columns: BTreeMap<Chunk, ChunkColumn>,
    watched: BTreeMap<Chunk, BlockPos>,
    expected_events: Vec<RandomTickEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TickOutcome {
    updates: Vec<RandomTickEvent>,
    watched_states: BTreeMap<BlockPos, String>,
}

#[derive(Debug, Clone)]
struct ReferenceJob {
    serial: usize,
    owner: Chunk,
    event: RandomTickEvent,
}

/// A [`ChunkSource`] with no neighbour columns, matching the isolated random
/// tick fixture: the capped grass only mutates its own retained column.
struct NoNeighbors;

impl ChunkSource for NoNeighbors {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        unreachable!("the fixture reports every neighbour column unavailable")
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        unreachable!("the fixture reports every neighbour column unavailable")
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        unreachable!("the fixture reports every neighbour column unavailable")
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        unreachable!("the fixture reports every neighbour column unavailable")
    }

    fn is_column_resident(&self, _cx: i32, _cz: i32) -> bool {
        false
    }
}

fn fixture() -> Fixture {
    let mut position_state = POSITION_SEED;
    let mut columns = BTreeMap::new();
    let mut watched = BTreeMap::new();
    let mut expected_events = Vec::new();

    for &(cx, cz) in &FIXTURE_ORDER {
        let pos = next_random_tick_pos(&mut position_state, cx * 16, 0, cz * 16, 15);
        // The selected y may be 15, so leave a second empty section for the
        // solid cap. Only the first section has ticking content and therefore
        // consumes the one seeded position draw for this chunk.
        let mut column = ChunkColumn::new(0, 32);
        column.set_block(pos.0 - cx * 16, pos.1, pos.2 - cz * 16, GRASS);
        column.set_block(pos.0 - cx * 16, pos.1 + 1, pos.2 - cz * 16, STONE);
        assert!(columns.insert((cx, cz), column).is_none(), "fixture chunks must be unique");
        assert!(watched.insert((cx, cz), pos).is_none(), "fixture watched cells must be unique");
        expected_events.push(RandomTickEvent {
            pos,
            from: GRASS.to_owned(),
            to: DIRT.to_owned(),
        });
    }

    Fixture {
        columns,
        watched,
        expected_events,
    }
}

fn region_owner(chunk: Chunk) -> Chunk {
    (
        chunk.0.div_euclid(REGION_EDGE_CHUNKS),
        chunk.1.div_euclid(REGION_EDGE_CHUNKS),
    )
}

/// Independently schedules physical-region owners, then applies their effects
/// at the old serial publication edge. It intentionally has no `TickRegionPlan`
/// input, so a changed production owner sequence cannot be copied into this arm.
fn independently_scheduled_region_reference(fixture: &Fixture) -> TickOutcome {
    let mut owner_batches: BTreeMap<Chunk, Vec<ReferenceJob>> = BTreeMap::new();
    for (serial, (&chunk, event)) in FIXTURE_ORDER.iter().zip(&fixture.expected_events).enumerate() {
        let owner = region_owner(chunk);
        owner_batches.entry(owner).or_default().push(ReferenceJob {
            serial,
            owner,
            event: event.clone(),
        });
    }
    assert_eq!(owner_batches.len(), 2, "fixture must cross two physical-region owners");

    let mut emitted = Vec::new();
    let mut watched_states = BTreeMap::new();
    for (owner, jobs) in owner_batches {
        for job in jobs {
            assert_eq!(job.owner, owner, "a reference job escaped its physical owner");
            watched_states.insert(job.event.pos, job.event.to.clone());
            emitted.push((job.serial, job.event));
        }
    }
    // A region worker may finish in owner order, but the central publisher must
    // retain the pre-existing serial effect sequence.
    emitted.sort_by_key(|(serial, _)| *serial);

    TickOutcome {
        updates: emitted.into_iter().map(|(_, event)| event).collect(),
        watched_states,
    }
}

/// Executes the current production owner sequence against the real random-tick
/// scheduler. The returned events are the same block updates the live loop sends
/// to its block-update feed after it persists them.
fn run_owner_sequence(sequence: &[TickOwnedChunk], mut fixture: Fixture) -> TickOutcome {
    let mut scheduler = RandomTickScheduler::new(POSITION_SEED, BEHAVIOR_SEED);
    let mut block_ticks = ScheduledTickQueue::new();
    let mut updates = Vec::new();
    for owned in sequence {
        let TickOwner::Chunk { cx, cz } = owned.owner else {
            panic!("the parity fixture requires chunk-local owners");
        };
        assert_eq!(owned.chunk, (cx, cz), "an owner must advance its own chunk");
        let column = fixture
            .columns
            .get_mut(&owned.chunk)
            .expect("every owner sequence entry must name a fixture column");
        updates.extend(scheduler.tick_chunk(
            column,
            cx,
            cz,
            1,
            &mut block_ticks,
            0,
            &NoNeighbors,
        ));
    }

    let watched_states = fixture
        .watched
        .into_iter()
        .map(|(chunk, pos)| {
            let column = fixture.columns.get(&chunk).expect("watched chunk must remain resident");
            (
                pos,
                column
                    .block_state(pos.0 - chunk.0 * 16, pos.1, pos.2 - chunk.1 * 16)
                    .to_owned(),
            )
        })
        .collect();
    TickOutcome {
        updates,
        watched_states,
    }
}

fn compare_outcomes(actual: &TickOutcome, reference: &TickOutcome) -> Result<(), String> {
    if actual.updates != reference.updates {
        return Err(format!(
            "client-visible block updates differ: actual {:?}, reference {:?}",
            actual.updates, reference.updates
        ));
    }
    if actual.watched_states != reference.watched_states {
        return Err(format!(
            "watched post-tick blocks differ: actual {:?}, reference {:?}",
            actual.watched_states, reference.watched_states
        ));
    }
    Ok(())
}

#[test]
fn serial_chunk_owner_plan_matches_independently_scheduled_region_reference() {
    let fixture = fixture();
    let plan = TickRegionPlan::chunk_owned(FIXTURE_ORDER.to_vec());
    let reference = independently_scheduled_region_reference(&fixture);
    let actual = run_owner_sequence(plan.owned_chunks(), fixture);

    assert_eq!(actual.updates.len(), FIXTURE_ORDER.len(), "every seeded owner must publish one update");
    assert!(
        actual.watched_states.values().all(|state| state == DIRT),
        "each watched grass block must persist as dirt after its published update"
    );
    compare_outcomes(&actual, &reference).expect("serial owner plan parity");
}

#[test]
fn parity_detector_rejects_swapped_or_duplicated_owner_visits() {
    let plan = TickRegionPlan::chunk_owned(FIXTURE_ORDER.to_vec());
    let reference_fixture = fixture();
    let reference = independently_scheduled_region_reference(&reference_fixture);

    let mut swapped = plan.owned_chunks().to_vec();
    swapped.swap(0, 1);
    let swapped_outcome = run_owner_sequence(&swapped, fixture());
    assert!(
        compare_outcomes(&swapped_outcome, &reference).is_err(),
        "control failed: swapping owner visits still matched the reference"
    );

    let mut duplicated = plan.owned_chunks().to_vec();
    duplicated[1] = duplicated[0];
    assert_eq!(duplicated[0].chunk, duplicated[1].chunk, "control must duplicate an owner visit");
    let duplicated_outcome = run_owner_sequence(&duplicated, fixture());
    assert!(
        compare_outcomes(&duplicated_outcome, &reference).is_err(),
        "control failed: duplicating an owner visit still matched the reference"
    );
}
