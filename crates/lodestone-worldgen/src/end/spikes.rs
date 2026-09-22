//! The ten obsidian pillars ringing the End's main island — each carrying a
//! stationary end crystal, two of the ten caged in iron bars. Pure geometry
//! plus one legacy-random draw per spike (the size that decides its
//! radius/height/guarded flag); no block writes, no world, no entity.
//!
//! `mobs/end_crystal.rs`'s own module doc and `mobs/dragon.rs`'s
//! (`crate::dragon::fight`, in `lodestone-server`) both disclosed this
//! layout as entirely missing — "this codebase has no obsidian pillars
//! anywhere". [`end_spikes_for_seed`] is the piece that closes it; a caller
//! turns each [`EndSpike`] into actual obsidian/bedrock/crystal placement
//! and a real `MobSim::spawn_end_crystal` call.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue as V, Properties, PropertyKey};

use crate::rng::{LegacyRandomSource, RandomSource};

use super::podium::PodiumBlock;
use lodestone_data::block_states::StateId;

/// The number of spikes ringing the island.
pub const SPIKE_COUNT: usize = 10;

fn typed_state(block: Block, properties: &[(PropertyKey, V)]) -> StateId {
    let mut typed = Properties::empty();
    for &(key, value) in properties {
        typed = typed
            .with_builtin(key, value)
            .expect("generated block property is valid");
    }
    Properties::state_for_block(block, &typed).expect("generated block state is valid")
}

fn iron_bar_states() -> &'static [StateId; 16] {
    static STATES: OnceLock<[StateId; 16]> = OnceLock::new();
    STATES.get_or_init(|| {
        std::array::from_fn(|bits| {
            let north = bits & 1 != 0;
            let south = bits & 2 != 0;
            let west = bits & 4 != 0;
            let east = bits & 8 != 0;
            let bool_value = |value| if value { V::True } else { V::False };
            typed_state(
                Block::IronBars,
                &[
                    (PropertyKey::East, bool_value(east)),
                    (PropertyKey::North, bool_value(north)),
                    (PropertyKey::South, bool_value(south)),
                    (PropertyKey::Waterlogged, V::False),
                    (PropertyKey::West, bool_value(west)),
                ],
            )
        })
    })
}

#[derive(Clone, Copy)]
struct SpikeStates {
    air: StateId,
    obsidian: StateId,
    bedrock: StateId,
    fire: StateId,
}

fn spike_states() -> SpikeStates {
    static STATES: OnceLock<SpikeStates> = OnceLock::new();
    *STATES.get_or_init(|| SpikeStates {
        air: Block::Air.default_state(),
        obsidian: Block::Obsidian.default_state(),
        bedrock: Block::Bedrock.default_state(),
        fire: Block::Fire.default_state(),
    })
}

/// The ring radius every spike's centre sits on, in blocks.
const SPIKE_DISTANCE: f64 = 42.0;

/// Keep the seed-derived layout reusable without allowing a long-running
/// server to retain every world seed it has visited.
const SPIKE_CACHE_CEILING: usize = 1024;
const SPIKE_CACHE_SHARDS: usize = 16;

struct SpikeCache {
    entries: HashMap<i64, [EndSpike; SPIKE_COUNT]>,
    order: VecDeque<i64>,
}

impl SpikeCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn capacity() -> usize {
        SPIKE_CACHE_CEILING.div_ceil(SPIKE_CACHE_SHARDS)
    }
}

static SPIKE_CACHE: OnceLock<[Mutex<SpikeCache>; SPIKE_CACHE_SHARDS]> = OnceLock::new();

fn spike_cache() -> &'static [Mutex<SpikeCache>; SPIKE_CACHE_SHARDS] {
    SPIKE_CACHE.get_or_init(|| std::array::from_fn(|_| Mutex::new(SpikeCache::new())))
}

fn spike_cache_shard(seed: i64) -> usize {
    (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize % SPIKE_CACHE_SHARDS
}

/// One obsidian pillar: its centre `(x, z)`, radius, the height its crystal
/// sits at, and whether it is caged in iron bars (blocking a direct hit
/// until the cage is broken).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndSpike {
    pub center_x: i32,
    pub center_z: i32,
    pub radius: i32,
    pub height: i32,
    pub guarded: bool,
}

/// The ten spikes for a world `seed`. Deterministic and seed-derived, not
/// resampled per call: index `i`'s centre is always at ring angle
/// `2 * (-pi + pi/10 * i)` regardless of seed; only the
/// radius/height/guarded triple — drawn from a Fisher-Yates shuffle of
/// `0..10` — varies with it.
///
/// The key derivation chains two generators, exactly as vanilla's own
/// `getSpikesForLevel`/`SpikeCacheLoader` do: a `seed`-seeded generator
/// draws one `i64`, masked to its low 16 bits, and *that* reseeds a second
/// generator the shuffle actually draws from.
/// [`LegacyRandomSource::new`] is bit-identical to vanilla's own
/// `RandomSource.createThreadLocalInstance` (both wrap the same linear
/// congruential generator with no thread-safety layer over it), so this
/// reproduces the chain exactly.
#[must_use]
pub fn end_spikes_for_seed(seed: i64) -> [EndSpike; SPIKE_COUNT] {
    let cache = &spike_cache()[spike_cache_shard(seed)];
    if let Some(spikes) = cache
        .lock()
        .expect("End spike cache poisoned")
        .entries
        .get(&seed)
        .copied()
    {
        return spikes;
    }

    let spikes = compute_end_spikes_for_seed(seed);
    let mut cache = cache.lock().expect("End spike cache poisoned");
    if let Some(cached) = cache.entries.get(&seed).copied() {
        return cached;
    }
    if cache.entries.len() >= SpikeCache::capacity() {
        if let Some(evicted) = cache.order.pop_front() {
            cache.entries.remove(&evicted);
        }
    }
    cache.order.push_back(seed);
    cache.entries.insert(seed, spikes);
    spikes
}

fn compute_end_spikes_for_seed(seed: i64) -> [EndSpike; SPIKE_COUNT] {
    let mut key_source = LegacyRandomSource::new(seed);
    let key = key_source.next_long() & 65535;
    let mut random = LegacyRandomSource::new(key);

    // A Fisher-Yates shuffle of `0..10`: swap `[i-1]` with a uniformly drawn
    // `[0, i)` index, walking `i` down from `SPIKE_COUNT` to `2` — vanilla's
    // own `Util.shuffle`/`toShuffledList`.
    let mut sizes = [0i32, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    for i in (2..=SPIKE_COUNT).rev() {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        sizes.swap(i - 1, swap_to);
    }

    let mut spikes = [EndSpike { center_x: 0, center_z: 0, radius: 0, height: 0, guarded: false }; SPIKE_COUNT];
    for (i, spike) in spikes.iter_mut().enumerate() {
        let angle = 2.0 * (-std::f64::consts::PI + (std::f64::consts::PI / 10.0) * i as f64);
        let center_x = (SPIKE_DISTANCE * angle.cos()).floor() as i32;
        let center_z = (SPIKE_DISTANCE * angle.sin()).floor() as i32;
        let size = sizes[i];
        *spike = EndSpike {
            center_x,
            center_z,
            // `2 + size / 3` — vanilla's own integer division, not a float
            // divide: `size` in `0..10` gives radius `2..=5`.
            radius: 2 + size / 3,
            height: 76 + size * 3,
            guarded: size == 1 || size == 2,
        };
    }
    spikes
}

/// One spike's own block writes: the solid obsidian column, the cleared air
/// above the terrain around it, its iron-bars cage if [`EndSpike::guarded`],
/// and the bedrock/fire pair supporting the crystal that stands at
/// `(center_x, height + 1, center_z)` — `EndSpikeFeature.placeSpike`, minus
/// the crystal entity itself (spawning it is
/// `MobSim::spawn_end_crystal`'s job, not this pure function's; see
/// `mobs::dragon`'s own doc, in `lodestone-server`, for the caller that
/// pairs the two).
///
/// `min_y` is the dimension's own lowest generatable y (vanilla iterates
/// `level.getMinY()` up); this crate has no `ChunkSource` to read it from,
/// so the caller supplies it.
///
/// Writes are in placement order — later entries overwrite earlier ones at
/// the same position, exactly as repeated `setBlock` calls at one position
/// do in vanilla (a caller applying them in order reproduces this without
/// needing to dedupe). The bedrock-under/fire-at-crystal pair is pushed
/// last for that reason: both cells fall inside the "clear air above the
/// terrain" pass above (`y > 65`, outside the solid cylinder), and the
/// pair must win.
#[must_use]
pub fn end_spike_blocks(spike: &EndSpike, min_y: i32) -> Vec<PodiumBlock> {
    let mut writes = Vec::new();
    let states = spike_states();
    let radius = spike.radius;
    let max_y = spike.height + 10;
    let radius_sq_plus_one = radius * radius + 1;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let horizontal_sq = dx * dx + dz * dz;
            for y in min_y..=max_y {
                let x = spike.center_x + dx;
                let z = spike.center_z + dz;
                if horizontal_sq <= radius_sq_plus_one && y < spike.height {
                    writes.push(PodiumBlock { x, y, z, state: states.obsidian });
                } else if y > 65 {
                    writes.push(PodiumBlock { x, y, z, state: states.air });
                }
            }
        }
    }

    if spike.guarded {
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                for dy in 0i32..=3 {
                    let is_x_side = dx.abs() == 2;
                    let is_z_side = dz.abs() == 2;
                    let top = dy == 3;
                    if !(is_x_side || is_z_side || top) {
                        continue;
                    }
                    let x_edge = dx == -2 || dx == 2 || top;
                    let z_edge = dz == -2 || dz == 2 || top;
                    let north = x_edge && dz != -2;
                    let south = x_edge && dz != 2;
                    let west = z_edge && dx != -2;
                    let east = z_edge && dx != 2;
                    let bits = u8::from(north)
                        | (u8::from(south) << 1)
                        | (u8::from(west) << 2)
                        | (u8::from(east) << 3);
                    writes.push(PodiumBlock {
                        x: spike.center_x + dx,
                        y: spike.height + dy,
                        z: spike.center_z + dz,
                        state: iron_bar_states()[bits as usize],
                    });
                }
            }
        }
    }

    let crystal_x = spike.center_x;
    let crystal_y = spike.height + 1;
    let crystal_z = spike.center_z;
    writes.push(PodiumBlock {
        x: crystal_x,
        y: crystal_y - 1,
        z: crystal_z,
        state: states.bedrock,
    });
    // The block below is always the bedrock just written above (never soul
    // sand/soil), so vanilla's `FireBlock.getState` soul-fire branch never
    // fires here — plain fire is the only reachable outcome.
    writes.push(PodiumBlock {
        x: crystal_x,
        y: crystal_y,
        z: crystal_z,
        state: states.fire,
    });

    writes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring geometry is seed-independent (only the size-derived triple
    /// varies), so it can be checked against values computed independently
    /// (a standalone Python `math.cos`/`math.sin` script, not this crate's
    /// own arithmetic) rather than against this function's own output.
    #[test]
    fn spike_centres_match_the_ring_geometry_independent_of_seed() {
        const EXPECTED: [(i32, i32); SPIKE_COUNT] = [
            (42, 0),
            (33, 24),
            (12, 39),
            (-13, 39),
            (-34, 24),
            (-42, -1),
            (-34, -25),
            (-13, -40),
            (12, -40),
            (33, -25),
        ];
        for seed in [0i64, 1, -1, 12345, i64::MIN, i64::MAX] {
            let spikes = end_spikes_for_seed(seed);
            let mut mismatches = Vec::new();
            for (i, spike) in spikes.iter().enumerate() {
                let got = (spike.center_x, spike.center_z);
                if got != EXPECTED[i] {
                    mismatches.push(format!("seed {seed} spike {i}: expected {:?}, got {got:?}", EXPECTED[i]));
                }
            }
            assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        }
    }

    /// The size-derived triple is a permutation of `0..10`, so **exactly
    /// two** spikes are guarded (`size == 1` and `size == 2` each occur
    /// exactly once) for every seed — an invariant a magnitude-shaped
    /// assertion can check without predicting *which* two.
    #[test]
    fn exactly_two_spikes_are_guarded_for_any_seed() {
        for seed in [0i64, 1, -1, 12345, 987_654_321, i64::MIN, i64::MAX] {
            let spikes = end_spikes_for_seed(seed);
            let guarded = spikes.iter().filter(|s| s.guarded).count();
            assert_eq!(guarded, 2, "seed {seed}: expected exactly 2 guarded spikes, got {guarded}");
        }
    }

    /// Every spike's radius and height must land in the range the size
    /// formula (`size` in `0..10`) can actually produce: radius `2..=5`,
    /// height `76..=103`. A bug that produced e.g. a negative radius or a
    /// height outside vanilla's own range would still pass the two tests
    /// above, so this is a distinct, magnitude-shaped check.
    #[test]
    fn every_spikes_radius_and_height_are_in_the_real_vanilla_range() {
        for seed in [0i64, 42, -999, i64::MIN, i64::MAX] {
            for spike in end_spikes_for_seed(seed) {
                assert!((2..=5).contains(&spike.radius), "seed {seed}: radius {} out of range", spike.radius);
                assert!((76..=103).contains(&spike.height), "seed {seed}: height {} out of range", spike.height);
            }
        }
    }

    /// **Control**: a broken shuffle (e.g. one that never swaps) would still
    /// pass the guarded-count and range checks above by coincidence in the
    /// identity-permutation case, so this checks the *set* of sizes drawn
    /// really is a permutation of `0..10`, not merely ten values in range.
    #[test]
    fn the_ten_sizes_are_a_permutation_not_merely_in_range() {
        for seed in [0i64, 7, -42, i64::MIN, i64::MAX] {
            let spikes = end_spikes_for_seed(seed);
            // Recover each spike's `size` from its radius/height pair — the
            // two formulas (`2 + size/3`, `76 + size*3`) are both strictly
            // monotonic in `size` over `0..10`, so `height` alone identifies
            // it uniquely.
            let mut sizes: Vec<i32> = spikes.iter().map(|s| (s.height - 76) / 3).collect();
            sizes.sort_unstable();
            assert_eq!(sizes, (0..10).collect::<Vec<_>>(), "seed {seed}: sizes are not a permutation of 0..10");
        }
    }

    fn layout_checksum(spikes: &[EndSpike; SPIKE_COUNT]) -> u64 {
        spikes.iter().fold(0xcbf2_9ce4_8422_2325, |checksum, spike| {
            let mut checksum = checksum ^ spike.center_x as u32 as u64;
            checksum = checksum.wrapping_mul(0x1000_0000_01b3);
            checksum ^= spike.center_z as u32 as u64;
            checksum = checksum.wrapping_mul(0x1000_0000_01b3);
            checksum ^= spike.radius as u32 as u64;
            checksum = checksum.wrapping_mul(0x1000_0000_01b3);
            checksum ^= spike.height as u32 as u64;
            checksum = checksum.wrapping_mul(0x1000_0000_01b3);
            checksum ^ u64::from(spike.guarded)
        })
    }

    #[test]
    fn cached_layout_matches_the_uncached_layout_checksum() {
        for seed in [0i64, 1, -1, 42, 12345, i64::MIN, i64::MAX] {
            let cached = end_spikes_for_seed(seed);
            let uncached = compute_end_spikes_for_seed(seed);
            assert_eq!(layout_checksum(&cached), layout_checksum(&uncached), "seed {seed}");
            assert_eq!(cached, uncached, "cache changed spike geometry for seed {seed}");
        }
    }

    #[test]
    fn spike_cache_stays_within_its_global_bound() {
        for seed in 0..(SPIKE_CACHE_CEILING as i64 * 2) {
            let _ = end_spikes_for_seed(seed);
        }
        let retained = spike_cache()
            .iter()
            .map(|shard| shard.lock().expect("End spike cache poisoned").entries.len())
            .sum::<usize>();
        assert!(retained <= SPIKE_CACHE_CEILING, "retained {retained} spike layouts");
    }

    fn find<'a>(writes: &'a [PodiumBlock], x: i32, y: i32, z: i32) -> Option<&'a PodiumBlock> {
        writes.iter().rev().find(|w| w.x == x && w.y == y && w.z == z)
    }

    /// The centre column below the spike's own height is solid obsidian
    /// (distance-from-centre `0 <= radius² + 1` for any `radius >= 0`);
    /// directly above the crystal's height, the always-pushed bedrock/fire
    /// pair must win over the "clear air above 65" pass, exactly as
    /// [`end_spike_blocks`]'s own doc claims for write ordering.
    #[test]
    fn the_crystal_support_wins_over_the_cleared_air_above_it() {
        let spike = EndSpike { center_x: 0, center_z: 0, radius: 3, height: 85, guarded: false };
        let writes = end_spike_blocks(&spike, -64);
        // crystal_y = height + 1 = 86; one below is height itself (85).
        assert_eq!(find(&writes, 0, 85, 0).unwrap().state.canonical_state(), "minecraft:bedrock", "one below the crystal");
        assert_eq!(find(&writes, 0, 86, 0).unwrap().state, Block::Fire.default_state(), "at the crystal's own cell");
        assert_eq!(find(&writes, 0, 85 - 20, 0).unwrap().state.canonical_state(), "minecraft:obsidian", "well below the crystal, inside the column");
    }

    /// An unguarded spike must write **zero** iron bars — the negative
    /// control for the guarded case below, since without it a formula that
    /// always emits bars (ignoring `guarded`) could still pass a
    /// guarded-only assertion.
    #[test]
    fn an_unguarded_spike_places_no_iron_bars() {
        let spike = EndSpike { center_x: 5, center_z: -5, radius: 2, height: 76, guarded: false };
        let writes = end_spike_blocks(&spike, -64);
        assert!(writes.iter().all(|w| w.state.block() != Block::IronBars), "an unguarded spike must place no cage");
    }

    /// A guarded spike's cage: exactly the expected cell count (the 5x5x4
    /// cube's surface minus its interior and its open bottom — `100 - 27
    /// (dx,dz in -1..=1, dy in 0..=2) = 73`), and one concrete corner cell's
    /// exact iron-bars connection state, hand-derived from the same formula
    /// this function ports rather than from its own output.
    #[test]
    fn a_guarded_spike_places_the_expected_cage() {
        let spike = EndSpike { center_x: 0, center_z: 0, radius: 2, height: 76, guarded: true };
        let writes = end_spike_blocks(&spike, -64);
        let bars: Vec<&PodiumBlock> = writes.iter().filter(|w| w.state.block() == Block::IronBars).collect();
        assert_eq!(bars.len(), 73, "cage cell count");

        // dx=2, dz=-2, dy=0 (a bottom corner, on both the x-side and z-side,
        // not the top): is_x_side=true, is_z_side=true, top=false ->
        // x_edge = true (dx==2), z_edge = true (dz==-2) ->
        // north = x_edge && dz != -2 = true && false = false
        // south = x_edge && dz != 2  = true && true  = true
        // west  = z_edge && dx != -2 = true && true  = true
        // east  = z_edge && dx != 2  = true && false = false
        let corner = find(&writes, 2, 76, -2).expect("corner cage cell must be written");
        assert_eq!(corner.state.canonical_state(), "minecraft:iron_bars[east=false,north=false,south=true,waterlogged=false,west=true]");
    }

    /// **Control**: translating the spike must translate every write —
    /// every gate above uses a spike centred at the origin, which alone
    /// could not catch a hardcoded-to-zero offset.
    #[test]
    fn every_spike_write_translates_with_the_spikes_own_centre() {
        let base = EndSpike { center_x: 0, center_z: 0, radius: 2, height: 80, guarded: true };
        let shifted = EndSpike { center_x: 42, center_z: -1, radius: 2, height: 80, guarded: true };
        let base_writes = end_spike_blocks(&base, -64);
        let shifted_writes = end_spike_blocks(&shifted, -64);
        assert_eq!(base_writes.len(), shifted_writes.len());
        for (b, s) in base_writes.iter().zip(shifted_writes.iter()) {
            assert_eq!(s.x, b.x + 42);
            assert_eq!(s.z, b.z - 1);
            assert_eq!(s.y, b.y);
            assert_eq!(s.state, b.state);
        }
    }
}
