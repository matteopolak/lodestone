//! The 26.3 terrain source, for every dimension.
//!
//! [`Terrain263ChunkSource`] serves columns from `lodestone_worldgen::terrain263`: the 26.3
//! noise fill, surface rules, carvers, biomes, structures and the placed-feature decorator of
//! the Overworld presets, the Nether or the End.
//! It is a child of `chunk` so it can build a [`ChunkColumn`] from its private storage directly.
//!
//! A target's final column is a pure function of the seed and its coordinates: its own shaped
//! terrain, then the decoration of the nine chunks around it applied in one fixed source
//! order, each decoration reading the blocks the earlier ones left in the 5x5 chunks around the
//! target. Nothing is retained between targets, so the content a chunk gets does not depend on
//! which other chunks were requested with it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lodestone_data::block_states::StateId;
use lodestone_worldgen::stage_schedule::{OVERWORLD_SOURCES, SourceCompletion};
use lodestone_data::entity_type::{EntityType, EntityTypeRef};
use lodestone_worldgen::overworld::block_entities::{BeeOccupant, GeneratedBlockEntity};
use lodestone_worldgen::structure::CodedLoot;
use lodestone_worldgen::terrain263::{PlacedBlockEntity, Shaped, State, Terrain263};

use super::{ChunkColumn, ChunkGenerationStage, ChunkSource, VersionedAdmissionColumn};

/// The loot table a monster-room chest defers to.
const DUNGEON_LOOT_TABLE: &str = "minecraft:chests/simple_dungeon";

/// Chunk radius of the area a target's decoration reads and writes: its nine sources each read
/// one chunk beyond themselves.
const WINDOW_RADIUS: i32 = 2;

/// The most shaped chunks a batch warms up front; beyond it the shaped cache could not hold them.
const WARM_LIMIT: usize = 256;

/// Columns generated per parallel group of a batch (a 10x10 patch needs a 14x14 window).
const GROUP: usize = 100;

/// The production generator of one 26.3 dimension.
pub struct Terrain263ChunkSource {
    terrain: Arc<Terrain263>,
    dimension: crate::dimension::Dimension,
    edits: Mutex<HashMap<(i32, i32), VersionedAdmissionColumn>>,
    generation_inputs: Mutex<HashMap<(i32, i32), VersionedAdmissionColumn>>,
    admission_version_sequence: std::sync::atomic::AtomicU64,
    /// Chunks whose generation-time creatures were already proposed. A column generated again
    /// (it left every cache unedited) gets none, so its animals spawn once per world.
    populated: Mutex<HashSet<(i32, i32)>>,
    pending_population: crate::generation_population::PendingGenerationPopulationPublication,
    /// Whether this End's dragon fight was started; only read for the End.
    dragon_fight_started: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Terrain263ChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terrain263ChunkSource").finish_non_exhaustive()
    }
}

impl Terrain263ChunkSource {
    /// Builds the 26.3 Overworld for `seed`.
    ///
    /// # Errors
    /// If the bundled 26.3 data fails to compile.
    pub fn new(seed: i64) -> Result<Self, lodestone_worldgen::terrain263::Terrain263Error> {
        Self::with_settings(seed, "overworld")
    }

    /// Builds the noise settings `settings`: an Overworld preset (`overworld`, `large_biomes` or
    /// `amplified`), `nether` or `end`.
    ///
    /// # Errors
    /// If the bundled 26.3 data fails to compile.
    pub fn with_settings(seed: i64, settings: &str) -> Result<Self, lodestone_worldgen::terrain263::Terrain263Error> {
        let dimension = match settings {
            "nether" => crate::dimension::Dimension::Nether,
            "end" => crate::dimension::Dimension::End,
            _ => crate::dimension::Dimension::Overworld,
        };
        Ok(Self::from_terrain(Arc::new(Terrain263::with_settings(seed, settings)?), dimension))
    }

    /// Wraps an already compiled generator of `dimension`, with fresh source-local mutable state.
    #[must_use]
    pub fn from_terrain(terrain: Arc<Terrain263>, dimension: crate::dimension::Dimension) -> Self {
        Self {
            terrain,
            dimension,
            edits: Mutex::new(HashMap::new()),
            generation_inputs: Mutex::new(HashMap::new()),
            admission_version_sequence: std::sync::atomic::AtomicU64::new(0),
            populated: Mutex::new(HashSet::new()),
            pending_population: crate::generation_population::PendingGenerationPopulationPublication::default(),
            dragon_fight_started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The lowest world `y` this source's columns contain.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.terrain.min_y()
    }

    /// How many `y` levels this source's columns contain.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.terrain.height()
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
        let (states, entities, _) = self.decorated(cx, cz);
        (states, entities)
    }

    /// [`Self::full_states_with_block_entities`] plus the loot containers structure placement
    /// seeded inside the target chunk.
    fn decorated(&self, cx: i32, cz: i32) -> (Vec<State>, Vec<GeneratedBlockEntity>, Vec<CodedLoot>) {
        let SourceCompletion::Fixed(offsets) = OVERWORLD_SOURCES.completion() else {
            unreachable!("the Overworld source window has a fixed order");
        };
        let (min_y, height) = (self.terrain.min_y(), self.terrain.height());
        let mut overlay: HashMap<(i32, i32), Vec<State>> = HashMap::new();
        let mut attached: Vec<PlacedBlockEntity> = Vec::new();
        let mut loot: Vec<CodedLoot> = Vec::new();
        for &(dx, dz) in offsets {
            let decoration = self
                .terrain
                .decorate_source_full((cx + dx, cz + dz), &mut |x, z| overlay.get(&(x, z)).cloned());
            attached.extend(decoration.entities);
            loot.extend(decoration.loot.into_iter().filter(|chest| chest.pos[0] >> 4 == cx && chest.pos[2] >> 4 == cz));
            for (x, y, z, state) in decoration.writes {
                let key = (x >> 4, z >> 4);
                if (key.0 - cx).abs() > WINDOW_RADIUS || (key.1 - cz).abs() > WINDOW_RADIUS || !(min_y..min_y + height).contains(&y) {
                    continue;
                }
                let blocks = overlay
                    .entry(key)
                    .or_insert_with(|| self.terrain.shaped(key.0, key.1).chunk.states.clone());
                blocks[((y - min_y) + ((x & 15) + (z & 15) * 16) * height) as usize] = state;
            }
        }
        let states = overlay
            .remove(&(cx, cz))
            .unwrap_or_else(|| self.terrain.shaped(cx, cz).chunk.states.clone());
        let entities = self.surviving_block_entities(cx, cz, &states, attached);
        (states, entities, loot)
    }

    /// Records the structure starts in `(cx, cz)` and the starts reaching it (the save format's
    /// `starts` and `References`), then gives those structures' blocks their entity data: the
    /// chests the templates and coded pieces name (each rolled from a seed of its own position),
    /// an ocean ruin's marker chest block, the mineshaft and fortress spawners, and every other
    /// templated entity block its template's own data (a banner's patterns, a brewing stand's
    /// potions). A chest entity is kept only while its block is a chest in the final column.
    fn attach_structures(&self, column: &mut ChunkColumn, cx: i32, cz: i32) {
        let refs = self.terrain.structure_refs(cx, cz);
        column.set_structures(self.terrain.structure_starts(cx, cz).to_vec(), refs.packed_by_structure());
        let starts: Vec<_> = refs.entries.iter().filter(|(_, _, start)| start.pieces_complete).map(|(_, _, start)| Arc::clone(start)).collect();
        if starts.is_empty() {
            return;
        }
        let chests = crate::structure_loot::chests_for_chunk(&starts, cx, cz, crate::block_drops::bundled_tables(), column);
        let spawners = crate::structure_loot::spawners_for_chunk(column, &starts, cx, cz);
        let templated = crate::structure_loot::template_block_entities_for_chunk(&starts, cx, cz);
        if chests.is_empty() && spawners.is_empty() && templated.is_empty() {
            return;
        }
        let mut entities = column.block_entities().to_vec();
        for chest in chests {
            let (lx, lz) = (chest.pos.x.rem_euclid(16), chest.pos.z.rem_euclid(16));
            if let Some(block) = chest.block {
                let state = lodestone_data::block::Block::from_name(block).map_or_else(lodestone_data::block_states::air_state, lodestone_data::block::Block::default_state);
                column.set_block_id(lx, chest.pos.y, lz, state);
            }
            if column.block_state_id(lx, chest.pos.y, lz).name() == "minecraft:chest" {
                entities.push((chest.pos, chest.entity));
            }
        }
        entities.extend(spawners);
        // A template's own entity data is kept only while the final block still carries that
        // kind of entity (a processor or a later feature may have replaced it), once per position.
        for (id, pos, entity) in templated {
            let state = column.block_state_id(pos.x.rem_euclid(16), pos.y, pos.z.rem_euclid(16));
            let kind = lodestone_data::block_entity_types::block_entity_type(state)
                .map(lodestone_data::block_entity_types::block_entity_type_name);
            if kind == Some(id.as_str()) && !entities.iter().any(|(existing, _)| *existing == pos) {
                entities.push((pos, entity));
            }
        }
        column.set_block_entities(entities);
    }

    /// The attached entities inside chunk `(cx, cz)` whose block is still in `states`.
    fn surviving_block_entities(&self, cx: i32, cz: i32, states: &[State], attached: Vec<PlacedBlockEntity>) -> Vec<GeneratedBlockEntity> {
        let blocks = &self.terrain.env().blocks;
        let named = |name: &str| blocks.block_by_name(name).expect("a block entity's block exists");
        let (chest, spawner, nest, gateway) = (named("chest"), named("spawner"), named("bee_nest"), named("end_gateway"));
        let (min_y, height) = (self.terrain.min_y(), self.terrain.height());
        let at = |x: i32, y: i32, z: i32| -> Option<State> {
            let inside = x >> 4 == cx && z >> 4 == cz && (min_y..min_y + height).contains(&y);
            inside.then(|| states[((y - min_y) + ((x & 15) + (z & 15) * 16) * height) as usize])
        };
        let mut by_position: std::collections::BTreeMap<(i32, i32, i32), GeneratedBlockEntity> = std::collections::BTreeMap::new();
        for entity in attached {
            let (x, y, z) = match &entity {
                PlacedBlockEntity::Chest { x, y, z, .. }
                | PlacedBlockEntity::Spawner { x, y, z, .. }
                | PlacedBlockEntity::Beehive { x, y, z, .. }
                | PlacedBlockEntity::EndGateway { x, y, z, .. } => (*x, *y, *z),
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
                PlacedBlockEntity::EndGateway { exit, exact, .. } if block == gateway => GeneratedBlockEntity::EndGateway {
                    x,
                    y,
                    z,
                    exit: (exit.x, exit.y, exit.z),
                    exact,
                },
                _ => continue,
            };
            by_position.insert((x, y, z), generated);
        }
        by_position.into_values().collect()
    }

    fn generate(&self, cx: i32, cz: i32) -> ChunkColumn {
        let (states, block_entities, loot) = self.decorated(cx, cz);
        let mut column = self.column_from_states(&states, &self.terrain.shaped(cx, cz), ChunkGenerationStage::Full);
        column.add_generated_block_entities(&block_entities);
        let placement_chests = crate::structure_loot::chests_from_coded(&loot, crate::block_drops::bundled_tables(), &column);
        if !placement_chests.is_empty() {
            let mut entities = column.block_entities().to_vec();
            entities.extend(placement_chests.into_iter().map(|chest| (chest.pos, chest.entity)));
            column.set_block_entities(entities);
        }
        self.attach_structures(&mut column, cx, cz);
        // Blocks that carry an entity but were placed without data (a structure's chest or
        // spawner) get the default entity so they stay usable.
        column.populate_missing_block_entity_states(cx, cz);
        // The client heightmaps and the retained MOTION_BLOCKING map, from the final blocks.
        column.derive_heightmaps();
        self.propose_generation_spawns(&mut column, cx, cz);
        column
    }

    /// Proposes the creature packs a freshly generated Overworld chunk starts with, the first
    /// time the chunk is generated, and publishes them for the tick loop to place.
    ///
    /// The packs are drawn from the biome at the chunk's minimum corner at the top of the world,
    /// and each candidate stands on the first free block of the motion-blocking map (parrots and
    /// ocelots, which perch in trees) or the no-leaves map (everything else). The tick loop
    /// re-checks each candidate's placement before spawning it. The Nether's packs (striders)
    /// are not proposed: their standing position needs the ceiling-aware downward scan, which
    /// the candidate stage does not model, and the End generates none.
    fn propose_generation_spawns(&self, column: &mut ChunkColumn, cx: i32, cz: i32) {
        if self.dimension != crate::dimension::Dimension::Overworld
            || !self.populated.lock().expect("population ledger lock poisoned").insert((cx, cz))
        {
            return;
        }
        let Some(maps) = column.client_heightmaps_raw() else { return };
        let min_y = self.terrain.min_y();
        let top = min_y + self.terrain.height() - 1;
        let Some(biome) = lodestone_data::biomes::BuiltinBiome::from_name(column.biome_state_at(0, top, 0)) else { return };
        let biome = lodestone_data::biomes::BiomeRef::builtin(biome);
        let spawns = lodestone_worldgen::spawn_stage::spawn_candidates_for_chunk(
            |_, _| biome,
            |species, lx, lz| {
                let perches = matches!(species.builtin_or_none(), Some(EntityType::Parrot | EntityType::Ocelot));
                min_y + i32::from(maps[if perches { 1 } else { 2 }][lx + lz * 16])
            },
            crate::worldgen_data::bundled_spawners_by_builtin(),
            self.terrain.seed(),
            cx,
            cz,
        );
        column.set_generation_spawns(spawns);
        if let Some(batch) = column.generation_spawn_batch() {
            self.pending_population.publish(batch);
        }
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
        let (min_y, height) = (terrain.min_y(), terrain.height());
        let mut blocks = vec![0u16; (256 * height) as usize];
        for z in 0..16usize {
            for x in 0..16usize {
                let base = (x + z * 16) * height as usize;
                for ly in 0..height as usize {
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
                let base = (qx * 4 + qz * 4 * 16) * height as usize;
                let top = (0..height as usize).rev().find(|&ly| states[base + ly] != env.known.air).map_or(0, |ly| ly as i32);
                let qy = ((top >> 2).max(0)).min(shaped.biomes.quarts_y - 1);
                surface[qz * 4 + qx] = biome_name(shaped.biomes.get(qx as i32, shaped.biomes.min_quart_y + qy, qz as i32));
            }
        }
        let mut column = ChunkColumn::from_raw_window(min_y, height, height, palette, &blocks, surface, &[], true);
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

impl ChunkSource for Terrain263ChunkSource {
    fn dragon_fight_started(&self) -> Option<bool> {
        (self.dimension == crate::dimension::Dimension::End)
            .then(|| self.dragon_fight_started.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Only the End has a fight to claim; every other dimension answers the trait's default.
    fn claim_dragon_fight_start(&self) -> bool {
        self.dimension != crate::dimension::Dimension::End
            || self
                .dragon_fight_started
                .compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire)
                .is_ok()
    }

    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        Some(self.dimension)
    }

    /// Generation publishes each chunk's creatures itself, once per world, so nothing needs
    /// retaining here.
    fn retain_generation_population(&self, _cx: i32, _cz: i32, _column: &mut ChunkColumn) -> bool {
        true
    }

    fn pending_generation_spawn_batches(&self, limit: usize) -> Vec<Arc<crate::generation_population::GenerationSpawnBatch>> {
        self.pending_population.pending(limit)
    }

    fn locate_stronghold(&self, from: lodestone_model::BlockPos) -> Option<lodestone_model::BlockPos> {
        if self.dimension != crate::dimension::Dimension::Overworld {
            return None;
        }
        super::nearest_ring_start(&self.terrain.ring_origins("minecraft:strongholds"), from)
    }

    fn horizon_sample(&self, x: i32, z: i32) -> Option<super::HorizonSample> {
        const LAND_RGB565: u16 = 0x5A85;
        const WATER_RGB565: u16 = 0x2D9B;
        if self.dimension != crate::dimension::Dimension::Overworld {
            return None;
        }
        let terrain_y = self.terrain.preliminary_surface_level(x, z);
        let sea_level = self.terrain.sea_level();
        let water_y = (terrain_y < sea_level).then_some(sea_level);
        Some(super::HorizonSample {
            terrain_y,
            water_y,
            surface_rgb565: if water_y.is_some() { WATER_RGB565 } else { LAND_RGB565 },
            flags: 0,
        })
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

    /// The production source fills a structure's chests: the seed-42 shipwreck the real 26.3
    /// server starts at chunk (48, -17) (box x 760..787, z -261..-253, from the structure-start
    /// oracle fixture) serves at least one chest block whose entity rolled items.
    #[test]
    fn a_shipwreck_serves_filled_chests() {
        use crate::chunk::ChunkSource as _;

        let source = crate::worldgen_data::overworld_chunk_source_of_type(42, crate::worldgen_data::WorldType::Overworld);
        let mut filled = 0;
        for cx in 47..=49 {
            for cz in -17..=-16 {
                let column = source.column(cx, cz);
                for (pos, entity) in column.block_entities() {
                    if entity.kind() != crate::block_entities::BlockEntityKind::Chest {
                        continue;
                    }
                    assert_eq!(
                        source.block_state_id(pos.x, pos.y, pos.z).block(),
                        lodestone_data::block::Block::Chest,
                        "a chest entity at {pos:?} stands on another block"
                    );
                    if entity.container_slots().iter().any(Option::is_some) {
                        filled += 1;
                    }
                }
            }
        }
        assert!(filled > 0, "no filled chest in the seed-42 shipwreck at chunk (48, -17)");
    }

    /// The control for the survival rule: an entity attached to a position whose final block is
    /// not its own is dropped, and the same entity is kept once its block is there.
    #[test]
    fn an_attached_entity_is_kept_only_while_its_block_stands() {
        let source = Terrain263ChunkSource::new(42).expect("bundled data compiles");
        let (cx, cz) = (-24, -33);
        let mut states = source.full_states(cx, cz);
        let (x, z) = (cx * 16 + 3, cz * 16 + 5);
        let y = 100;
        let index = ((y - source.min_y()) + ((x & 15) + (z & 15) * 16) * source.height()) as usize;
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
