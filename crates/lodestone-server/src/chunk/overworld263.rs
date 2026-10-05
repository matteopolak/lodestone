//! The 26.3 Overworld terrain source.
//!
//! [`Overworld263ChunkSource`] serves columns from `lodestone_worldgen::terrain263`: the 26.3
//! noise fill, surface rules, carvers, real multi-noise biomes and the placed-feature decorator.
//! It is a child of `chunk` so it can build a [`ChunkColumn`] from its private storage directly.
//!
//! A target's final column is a pure function of the seed and its coordinates: its own shaped
//! terrain, then the decoration of the nine chunks around it applied in the Overworld source
//! order, each decoration reading the blocks the earlier ones left in the 5x5 chunks around the
//! target. Nothing is retained between targets, so the content a chunk gets does not depend on
//! which other chunks were requested with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_data::block_states::StateId;
use lodestone_worldgen::stage_schedule::{OVERWORLD_SOURCES, SourceCompletion};
use lodestone_data::entity_type::{EntityType, EntityTypeRef};
use lodestone_worldgen::overworld::block_entities::{BeeOccupant, GeneratedBlockEntity};
use lodestone_worldgen::terrain263::{HEIGHT, MIN_Y, PlacedBlockEntity, Shaped, State, Terrain263};

use super::{ChunkColumn, ChunkGenerationStage, ChunkSource, VersionedAdmissionColumn};

/// Chunk radius of the area a target's decoration reads and writes: its nine sources each read
/// one chunk beyond themselves.
/// The loot table a monster-room chest defers to.
const DUNGEON_LOOT_TABLE: &str = "minecraft:chests/simple_dungeon";

const WINDOW_RADIUS: i32 = 2;

/// The most shaped chunks a batch warms up front; beyond it the shaped cache could not hold them.
const WARM_LIMIT: usize = 256;

/// Columns generated per parallel group of a batch (a 10x10 patch needs a 14x14 window).
const GROUP: usize = 100;

/// The production Overworld for 26.3 worlds.
pub struct Overworld263ChunkSource {
    terrain: Arc<Terrain263>,
    edits: Mutex<HashMap<(i32, i32), VersionedAdmissionColumn>>,
    generation_inputs: Mutex<HashMap<(i32, i32), VersionedAdmissionColumn>>,
    admission_version_sequence: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for Overworld263ChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overworld263ChunkSource").finish_non_exhaustive()
    }
}

impl Overworld263ChunkSource {
    /// Builds the 26.3 Overworld for `seed`.
    ///
    /// # Errors
    /// If the bundled 26.3 data fails to compile.
    pub fn new(seed: i64) -> Result<Self, lodestone_worldgen::terrain263::Terrain263Error> {
        Self::with_settings(seed, "overworld")
    }

    /// Builds the Overworld preset `settings` (`overworld`, `large_biomes` or `amplified`).
    ///
    /// # Errors
    /// If the bundled 26.3 data fails to compile.
    pub fn with_settings(seed: i64, settings: &str) -> Result<Self, lodestone_worldgen::terrain263::Terrain263Error> {
        Ok(Self::from_terrain(Arc::new(Terrain263::with_settings(seed, settings)?)))
    }

    /// Wraps an already compiled generator, with fresh source-local mutable state.
    #[must_use]
    pub fn from_terrain(terrain: Arc<Terrain263>) -> Self {
        Self {
            terrain,
            edits: Mutex::new(HashMap::new()),
            generation_inputs: Mutex::new(HashMap::new()),
            admission_version_sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The lowest world `y` this source's columns contain.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        MIN_Y
    }

    /// How many `y` levels this source's columns contain.
    #[must_use]
    pub fn height(&self) -> i32 {
        HEIGHT
    }

    /// The compiled generator, for diagnostics and parity checks.
    #[must_use]
    pub fn terrain(&self) -> &Terrain263 {
        &self.terrain
    }

    /// A target's blocks after its own terrain and the decoration of the nine chunks around it,
    /// in the decorator's state layout (`y + (x + z * 16) * height`).
    #[must_use]
    pub fn full_states(&self, cx: i32, cz: i32) -> Vec<State> {
        self.full_states_with_block_entities(cx, cz).0
    }

    /// [`Self::full_states`] plus the block entities decoration attached inside the target chunk,
    /// each kept only while the final block at its position is still the entity's own block (a
    /// later feature may have replaced it), one per position.
    #[must_use]
    pub fn full_states_with_block_entities(&self, cx: i32, cz: i32) -> (Vec<State>, Vec<GeneratedBlockEntity>) {
        let SourceCompletion::Fixed(offsets) = OVERWORLD_SOURCES.completion() else {
            unreachable!("the Overworld source window has a fixed order");
        };
        let mut overlay: HashMap<(i32, i32), Vec<State>> = HashMap::new();
        let mut attached: Vec<PlacedBlockEntity> = Vec::new();
        for &(dx, dz) in offsets {
            let (writes, entities) = self
                .terrain
                .decorate_source_full((cx + dx, cz + dz), &mut |x, z| overlay.get(&(x, z)).cloned());
            attached.extend(entities);
            for (x, y, z, state) in writes {
                let key = (x >> 4, z >> 4);
                if (key.0 - cx).abs() > WINDOW_RADIUS || (key.1 - cz).abs() > WINDOW_RADIUS || !(MIN_Y..MIN_Y + HEIGHT).contains(&y) {
                    continue;
                }
                let blocks = overlay
                    .entry(key)
                    .or_insert_with(|| self.terrain.shaped(key.0, key.1).chunk.states.clone());
                blocks[((y - MIN_Y) + ((x & 15) + (z & 15) * 16) * HEIGHT) as usize] = state;
            }
        }
        let states = overlay
            .remove(&(cx, cz))
            .unwrap_or_else(|| self.terrain.shaped(cx, cz).chunk.states.clone());
        let entities = self.surviving_block_entities(cx, cz, &states, attached);
        (states, entities)
    }

    /// The attached entities inside chunk `(cx, cz)` whose block is still in `states`.
    fn surviving_block_entities(&self, cx: i32, cz: i32, states: &[State], attached: Vec<PlacedBlockEntity>) -> Vec<GeneratedBlockEntity> {
        let blocks = &self.terrain.env().blocks;
        let named = |name: &str| blocks.block_by_name(name).expect("a block entity's block exists");
        let (chest, spawner, nest) = (named("chest"), named("spawner"), named("bee_nest"));
        let at = |x: i32, y: i32, z: i32| -> Option<State> {
            let inside = x >> 4 == cx && z >> 4 == cz && (MIN_Y..MIN_Y + HEIGHT).contains(&y);
            inside.then(|| states[((y - MIN_Y) + ((x & 15) + (z & 15) * 16) * HEIGHT) as usize])
        };
        let mut by_position: std::collections::BTreeMap<(i32, i32, i32), GeneratedBlockEntity> = std::collections::BTreeMap::new();
        for entity in attached {
            let (x, y, z) = match &entity {
                PlacedBlockEntity::Chest { x, y, z, .. } | PlacedBlockEntity::Spawner { x, y, z, .. } | PlacedBlockEntity::Beehive { x, y, z, .. } => (*x, *y, *z),
            };
            let Some(state) = at(x, y, z) else { continue };
            let block = blocks.block_of(state);
            let generated = match entity {
                PlacedBlockEntity::Chest { loot_seed, .. } if block == chest => GeneratedBlockEntity::DungeonChest {
                    x,
                    y,
                    z,
                    facing: blocks.get(state, "facing").unwrap_or("north").to_owned(),
                    loot_table: DUNGEON_LOOT_TABLE.to_owned(),
                    loot_table_seed: loot_seed,
                },
                PlacedBlockEntity::Spawner { mob, .. } if block == spawner => GeneratedBlockEntity::DungeonSpawner {
                    x,
                    y,
                    z,
                    entity_type: EntityTypeRef::from(match mob {
                        0 => EntityType::Skeleton,
                        1 | 2 => EntityType::Zombie,
                        _ => EntityType::Spider,
                    }),
                },
                PlacedBlockEntity::Beehive { bee_ticks, .. } if block == nest => GeneratedBlockEntity::Beehive {
                    x,
                    y,
                    z,
                    bees: bee_ticks.into_iter().map(|ticks_in_hive| BeeOccupant { ticks_in_hive, min_ticks_in_hive: 600 }).collect(),
                },
                _ => continue,
            };
            by_position.insert((x, y, z), generated);
        }
        by_position.into_values().collect()
    }

    fn generate(&self, cx: i32, cz: i32) -> ChunkColumn {
        let (states, block_entities) = self.full_states_with_block_entities(cx, cz);
        let mut column = self.column_from_states(&states, &self.terrain.shaped(cx, cz), ChunkGenerationStage::Full);
        column.add_generated_block_entities(&block_entities);
        // Blocks that carry an entity but were placed without data (a structure's chest or
        // spawner) get the default entity so they stay usable.
        column.populate_missing_block_entity_states(cx, cz);
        column
    }

    fn generate_shaped(&self, cx: i32, cz: i32) -> ChunkColumn {
        let shaped = self.terrain.shaped(cx, cz);
        self.column_from_states(&shaped.chunk.states, &shaped, ChunkGenerationStage::Shaped)
    }

    /// Adopts decorator-space blocks and the shaped chunk's biome cells as a server column.
    fn column_from_states(&self, states: &[State], shaped: &Shaped, stage: ChunkGenerationStage) -> ChunkColumn {
        let terrain = &self.terrain;
        let env = terrain.env();
        let mut remap = vec![u16::MAX; env.blocks.state_count()];
        let mut palette: Vec<StateId> = vec![lodestone_data::block_states::air_state()];
        remap[env.known.air as usize] = 0;
        let mut blocks = vec![0u16; (256 * HEIGHT) as usize];
        for z in 0..16usize {
            for x in 0..16usize {
                let base = (x + z * 16) * HEIGHT as usize;
                for ly in 0..HEIGHT as usize {
                    let state = states[base + ly];
                    let slot = &mut remap[state as usize];
                    if *slot == u16::MAX {
                        *slot = palette.len() as u16;
                        palette.push(terrain.canonical(state));
                    }
                    blocks[(ly * 16 + z) * 16 + x] = *slot;
                }
            }
        }
        let biome_name = |id| terrain.biome_name(id).to_owned();
        // The surface biome of each horizontal quart is read at the height of the quart's own
        // first column.
        let mut surface = std::array::from_fn(|_| String::new());
        for qz in 0..4usize {
            for qx in 0..4usize {
                let base = (qx * 4 + qz * 4 * 16) * HEIGHT as usize;
                let top = (0..HEIGHT as usize).rev().find(|&ly| states[base + ly] != env.known.air).map_or(0, |ly| ly as i32);
                let qy = ((top >> 2).max(0)).min(shaped.biomes.quarts_y - 1);
                surface[qz * 4 + qx] = biome_name(shaped.biomes.get(qx as i32, shaped.biomes.min_quart_y + qy, qz as i32));
            }
        }
        let mut column = ChunkColumn::from_raw_window(MIN_Y, HEIGHT, HEIGHT, palette, &blocks, surface, &[], true);
        column.generation_stage = stage;
        let mut names: Vec<String> = Vec::new();
        let mut index_of: HashMap<u32, u16> = HashMap::new();
        let mut cells = Vec::with_capacity(shaped.biomes.quarts_y as usize * 16);
        for qy in 0..shaped.biomes.quarts_y {
            for qz in 0..4 {
                for qx in 0..4 {
                    let id = shaped.biomes.get(qx, shaped.biomes.min_quart_y + qy, qz);
                    let index = *index_of.entry(id.0).or_insert_with(|| {
                        names.push(biome_name(id));
                        (names.len() - 1) as u16
                    });
                    cells.push(index);
                }
            }
        }
        column.biome_palette = names;
        column.biome_cells = cells;
        column
    }

    fn retained(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        if let Some(edited) = self.edits.lock().expect("chunk edit cache lock poisoned").get(&(cx, cz)) {
            return Some(edited.column.clone());
        }
        self.generation_inputs
            .lock()
            .expect("generation input lock poisoned")
            .get(&(cx, cz))
            .map(|input| input.column.clone())
    }

    fn next_version(&self) -> u64 {
        self.admission_version_sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }
}

impl ChunkSource for Overworld263ChunkSource {
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        Some(crate::dimension::Dimension::Overworld)
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        self.retained(cx, cz)
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.retained(cx, cz).unwrap_or_else(|| self.generate(cx, cz))
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        match (self.retained(cx, cz), stage) {
            (Some(column), _) => column,
            (None, ChunkGenerationStage::Shaped) => self.generate_shaped(cx, cz),
            (None, ChunkGenerationStage::Full) => self.generate(cx, cz),
        }
    }

    /// A shaped column has no decoration, and a neighbour's decoration reaches into it, so a
    /// packet needs the full column.
    fn packet_generation_stage(&self, stage: ChunkGenerationStage) -> Option<ChunkGenerationStage> {
        (stage == ChunkGenerationStage::Shaped).then_some(ChunkGenerationStage::Full)
    }

    fn generation_request_dependency_radius(&self, _target: lodestone_worldgen::stage_schedule::GenerationTarget) -> u8 {
        WINDOW_RADIUS as u8
    }

    /// Fans the batch out over the worldgen pool in groups of `GROUP` columns. Each group first
    /// builds the union of its targets' windows in parallel, so overlapping windows share one
    /// shaped chunk instead of every thread building its own copy, and the union stays inside the
    /// shaped-chunk cache.
    fn columns(&self, coords: &[(i32, i32)]) -> Vec<ChunkColumn> {
        let mut out = Vec::with_capacity(coords.len());
        for group in coords.chunks(GROUP) {
            let mut window: Vec<(i32, i32)> = group
                .iter()
                .flat_map(|&(cx, cz)| (-WINDOW_RADIUS..=WINDOW_RADIUS).flat_map(move |dz| (-WINDOW_RADIUS..=WINDOW_RADIUS).map(move |dx| (cx + dx, cz + dz))))
                .collect();
            window.sort_unstable();
            window.dedup();
            if group.len() > 1 && window.len() <= WARM_LIMIT {
                let _ = crate::run_worldgen_jobs(window, |(cx, cz)| drop(self.terrain.shaped(cx, cz)));
            }
            out.extend(crate::run_worldgen_jobs(group.to_vec(), |(cx, cz)| self.column(cx, cz)));
        }
        out
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.column(x.div_euclid(16), z.div_euclid(16)).block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16)).biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16)).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let entry = edits
            .entry((cx, cz))
            .or_insert_with(|| VersionedAdmissionColumn { column: self.generate(cx, cz), version: 0 });
        entry.column.set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
        entry.version = self.next_version();
    }

    fn try_store_resident_edit(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        let mut edits = match self.edits.try_lock() {
            Ok(edits) => edits,
            Err(std::sync::TryLockError::WouldBlock) => return Some(crate::chunk_store::TryResidentEdit::Busy),
            Err(std::sync::TryLockError::Poisoned(_)) => panic!("chunk edit cache lock poisoned"),
        };
        edits.insert((cx, cz), VersionedAdmissionColumn { column: column.clone(), version: self.next_version() });
        Some(crate::chunk_store::TryResidentEdit::Applied)
    }

    fn retain_generation_input(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        self.generation_inputs
            .lock()
            .expect("generation input lock poisoned")
            .insert((cx, cz), VersionedAdmissionColumn { column: column.clone(), version: self.next_version() });
        true
    }

    fn release_generation_input(&self, cx: i32, cz: i32) {
        self.generation_inputs.lock().expect("generation input lock poisoned").remove(&(cx, cz));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control for the survival rule: an entity attached to a position whose final block is
    /// not its own is dropped, and the same entity is kept once its block is there.
    #[test]
    fn an_attached_entity_is_kept_only_while_its_block_stands() {
        let source = Overworld263ChunkSource::new(42).expect("bundled data compiles");
        let (cx, cz) = (-24, -33);
        let mut states = source.full_states(cx, cz);
        let (x, z) = (cx * 16 + 3, cz * 16 + 5);
        let y = 100;
        let index = ((y - MIN_Y) + ((x & 15) + (z & 15) * 16) * HEIGHT) as usize;
        let chest = {
            let blocks = &source.terrain.env().blocks;
            blocks.default_state(blocks.block_by_name("chest").unwrap())
        };
        let fake = || vec![PlacedBlockEntity::Chest { x, y, z, loot_seed: 7 }];
        assert_ne!(states[index], chest, "the cell starts as something other than a chest");
        assert!(source.surviving_block_entities(cx, cz, &states, fake()).is_empty(), "no chest block, no entity");
        states[index] = chest;
        let kept = source.surviving_block_entities(cx, cz, &states, fake());
        assert_eq!(kept.len(), 1, "the chest block brings the entity back");
        assert_eq!(kept[0].position(), (x, y, z));
    }
}
