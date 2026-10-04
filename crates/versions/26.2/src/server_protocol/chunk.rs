//! Chunk and dimension-aware light encoding.
//!
//! This private module is part of the V770ServerProtocol facade. Its
//! re-exported helpers preserve the existing public API and wire behaviour.

use super::*;
use lodestone_core::{Nbt, Writer, write_network_nbt};
use lodestone_world::{
    BlockVolume, ResidentLightError, ResidentLightFootprint, ResidentLightInputs,
    ResidentLightJob, ResidentLightProgress,
};

/// Converts one `lodestone-server` [`ServerChunkColumn`] into the
/// version-free [`WorldChunkColumn`] the wire codec speaks, carrying the
/// **real** per-block state the source already computed (grass, dirt,
/// deepslate, gravel, water, …) rather than a solid/air classification, and
/// the source's real per-quart
/// biome assignment rather than one constant id everywhere. Every block
/// cell is read as an **integer** via [`ServerChunkColumn::block_state_id`];
/// every biome **cell** via [`ServerChunkColumn::biome_cell_index`] through
/// [`biome_registry_id`] — a real per-`y` grid, not one surface
/// sample broadcast down the column.
///
/// State ids are read directly from the canonical column palette, so the inner
/// block loop is a range check and two array indexes.
///
/// [`biome_registry_id`] uses a cached name-to-id map, and it is called once per
/// entry in the column's own biome *palette* (`biome_palette_ids` below), never
/// once per cell. That is what makes
/// a 1,536-cell 3-D grid cost strictly less than the 16 calls the old
/// vertically-broadcast surface array made. It is now the only string work left
/// in this function.
///
/// Iterates section-major (matching wire order) and skips sections that end
/// up entirely default (air-only, default biome), since
/// [`WorldChunkColumn::set_section`] already elides those.
pub(super) fn build_world_column(shape: &ChunkShape, source: &ServerChunkColumn) -> WorldChunkColumn {
    build_world_column_with(shape, source, |state| state.raw(), biome_registry_id)
}

/// [`build_world_column`] for a column framed in a release's own ids: block
/// states and biome holders are translated as each cell is read.
pub(super) fn build_wire_column(
    wire: Wire,
    shape: &ChunkShape,
    source: &ServerChunkColumn,
) -> WorldChunkColumn {
    build_world_column_with(shape, source, |state| wire.state(state), |name| wire.biome(name))
}

fn build_world_column_with(
    shape: &ChunkShape,
    source: &ServerChunkColumn,
    state_id: impl Fn(lodestone_data::block_states::StateId) -> u32,
    biome_id: impl Fn(&str) -> u32,
) -> WorldChunkColumn {
    let mut column = WorldChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    // This column's real 3-D biome grid. The column stores its
    // cells as indices into a small per-column palette — a handful of entries,
    // never the 1,536 cells — so resolving that palette once and indexing per
    // cell is *cheaper* than the 16 `biome_registry_id` calls this replaced,
    // while carrying a per-`y` answer instead of one broadcast vertically.
    // Broadcasting was what erased `lush_caves`/`dripstone_caves`/`deep_dark`
    // from every column the server sent.
    let biome_palette_ids: Vec<u32> = source
        .biome_cell_palette()
        .iter()
        .map(|name| biome_id(name))
        .collect();

    let mut block_values = vec![
        shape.air_id;
        ChunkSection::EDGE * ChunkSection::EDGE * ChunkSection::EDGE
    ];
    let mut biome_values = vec![shape.biome_id; 4 * 4 * 4];
    for section_index in 0..shape.section_count {
        let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
        for ly in 0..ChunkSection::EDGE {
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    let index = (ly << 8) | (lz << 4) | lx;
                    block_values[index] = state_id(source.block_state_id(lx as i32, wy, lz as i32));
                }
            }
        }
        for qy in 0..4usize {
            let column_qy = section_index * 4 + qy;
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let cell = source.biome_cell_index(qx, column_qy, qz) as usize;
                    biome_values[(qy << 4) | (qz << 2) | qx] = biome_palette_ids[cell];
                }
            }
        }
        let section = ChunkSection::from_containers(
            lodestone_world::PalettedContainer::from_values(shape.block_kind, &block_values),
            lodestone_world::PalettedContainer::from_values(shape.biome_kind, &biome_values),
            shape.air_id,
        );
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }

    column
}

#[cfg(test)]
mod conversion_tests {
    use super::*;

    fn cellwise_column(shape: &ChunkShape, source: &ServerChunkColumn) -> WorldChunkColumn {
        let mut column = WorldChunkColumn::new(
            shape.min_y,
            shape.section_count,
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        let biome_ids: Vec<u32> = source
            .biome_cell_palette()
            .iter()
            .map(|name| biome_registry_id(name))
            .collect();
        for section_index in 0..shape.section_count {
            let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
            let mut section = ChunkSection::new(
                shape.block_kind,
                shape.biome_kind,
                shape.air_id,
                shape.biome_id,
            );
            for ly in 0..ChunkSection::EDGE {
                for lz in 0..ChunkSection::EDGE {
                    for lx in 0..ChunkSection::EDGE {
                        let id = source
                            .block_state_id(lx as i32, base_y + ly as i32, lz as i32)
                            .raw();
                        if id != shape.air_id {
                            section.set_block(lx, ly, lz, id);
                        }
                    }
                }
            }
            for qy in 0..4usize {
                for qz in 0..4usize {
                    for qx in 0..4usize {
                        let cell = source.biome_cell_index(qx, section_index * 4 + qy, qz);
                        section.set_biome(qx, qy, qz, biome_ids[cell as usize]);
                    }
                }
            }
            if !section.is_empty(shape.biome_id) {
                column.set_section(section_index, Some(section));
            }
        }
        column
    }

    fn first_cell_difference(left: &WorldChunkColumn, right: &WorldChunkColumn) -> Option<String> {
        if left.min_y() != right.min_y() {
            return Some(format!("min_y differs: {} != {}", left.min_y(), right.min_y()));
        }
        if left.section_count() != right.section_count() {
            return Some(format!(
                "section count differs: {} != {}",
                left.section_count(),
                right.section_count()
            ));
        }
        for section_index in 0..left.section_count() {
            let section_y = left.min_y() + (section_index * ChunkSection::EDGE) as i32;
            for index in 0..ChunkSection::EDGE.pow(3) {
                let x = index & 15;
                let z = (index >> 4) & 15;
                let y = index >> 8;
                let world_y = section_y + y as i32;
                let left_id = left.get_block(x, world_y, z);
                let right_id = right.get_block(x, world_y, z);
                if left_id != right_id {
                    return Some(format!(
                        "block differs at section {section_index}, local ({x}, {y}, {z}): {left_id} != {right_id}"
                    ));
                }
            }
            for index in 0..ChunkSection::BIOME_EDGE.pow(3) {
                let x = index & 3;
                let z = (index >> 2) & 3;
                let y = index >> 4;
                let world_y = section_y + (y * 4) as i32;
                let left_id = left.get_biome(x, world_y, z);
                let right_id = right.get_biome(x, world_y, z);
                if left_id != right_id {
                    return Some(format!(
                        "biome differs at section {section_index}, local ({x}, {y}, {z}): {left_id} != {right_id}"
                    ));
                }
            }
        }
        None
    }

    fn encoded_section(shape: &ChunkShape, column: &WorldChunkColumn, index: usize) -> Vec<u8> {
        let synthesized;
        let section = match column.section(index) {
            Some(section) => section,
            None => {
                synthesized = ChunkSection::new(
                    shape.block_kind,
                    shape.biome_kind,
                    shape.air_id,
                    shape.biome_id,
                );
                &synthesized
            }
        };
        let counts = section_packet_counts(Wire::BASE, section);
        let mut writer = Writer::default();
        writer.i16(counts.non_empty as i16);
        writer.i16(counts.fluid as i16);
        section.block_states().encode(&mut writer);
        section.biomes().encode(&mut writer);
        writer.into_vec()
    }

    #[test]
    fn buffered_conversion_matches_cellwise_control_across_shapes() {
        for shape in [ChunkShape::overworld_1_21(), ChunkShape::nether_or_end_1_21()] {
            let mut source = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
            source.set_block_id(
                1,
                shape.min_y,
                2,
                lodestone_data::block_states::StateId::from_state_str("minecraft:stone")
                    .expect("stone state"),
            );
            source.set_block_id(
                3,
                shape.min_y + 17,
                4,
                lodestone_data::block_states::StateId::from_state_str("minecraft:water[level=7]")
                    .expect("water state"),
            );
            source.set_block_id(
                5,
                shape.min_y + shape.world_height as i32 - 1,
                6,
                lodestone_data::block_states::StateId::from_state_str("minecraft:cave_air")
                    .expect("cave air state"),
            );
            source.set_biome_cell(0, 0, 0, "minecraft:desert");
            let buffered = build_world_column(&shape, &source);
            let cellwise = cellwise_column(&shape, &source);
            if let Some(difference) = first_cell_difference(&buffered, &cellwise) {
                panic!(
                    "buffered conversion differs for shape min_y={} sections={}: {}",
                    shape.min_y,
                    shape.section_count,
                    difference
                );
            }
            for section_index in 0..shape.section_count {
                let buffered_bytes = encoded_section(&shape, &buffered, section_index);
                let cellwise_bytes = encoded_section(&shape, &cellwise, section_index);
                if buffered_bytes != cellwise_bytes {
                    let byte_index = buffered_bytes
                        .iter()
                        .zip(&cellwise_bytes)
                        .position(|(left, right)| left != right)
                        .unwrap_or(buffered_bytes.len().min(cellwise_bytes.len()));
                    panic!(
                        "encoded section {section_index} differs for shape min_y={} sections={} at byte {byte_index}: lengths {} != {}",
                        shape.min_y,
                        shape.section_count,
                        buffered_bytes.len(),
                        cellwise_bytes.len()
                    );
                }
            }
        }
    }

    #[test]
    fn direct_relight_matches_buffered_columns_in_every_dimension() {
        let stone = StateId::from_state_str("minecraft:stone").expect("stone state");
        let glowstone = StateId::from_state_str("minecraft:glowstone").expect("glowstone state");
        for dimension in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            let shape = shape_for_dimension(dimension);
            let mut center = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
            let mut east = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
            let y = shape.min_y + 72;
            for x in 8..16 {
                center.set_block_id(x, y + 2, 8, stone);
            }
            center.set_block_id(15, y, 8, glowstone);
            east.set_block_id(0, y + 1, 8, stone);
            let neighbours = [(0, 0, &center), (1, 0, &east)];
            let direct = compute_served_light_with_neighbours(&center, &neighbours, dimension);
            let buffered_center = build_world_column(&shape, &center);
            let buffered_east = build_world_column(&shape, &east);
            let buffered = compute_column_light_with_neighbours(
                &Neighbourhood::new(&buffered_center).with(1, 0, &buffered_east),
                &V770LightProps {
                    has_skylight: dimension.has_skylight(),
                },
            );
            assert_eq!(direct, buffered, "{dimension:?}");
        }
    }

    #[tokio::test]
    async fn resident_batch_matches_independent_halos_in_every_dimension() {
        let stone = StateId::from_state_str("minecraft:stone").expect("stone state");
        let glowstone = StateId::from_state_str("minecraft:glowstone").expect("glowstone state");
        let cave_air = StateId::from_state_str("minecraft:cave_air").expect("cave air state");
        let outputs = [(42, -27), (41, -26), (41, -27)];
        let footprint = ResidentLightFootprint::new(outputs).unwrap();
        let protocol: Box<dyn ServerProtocol> = Box::new(V770ServerProtocol);
        for dimension in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            let shape = shape_for_dimension(dimension);
            let y = shape.min_y + 72;
            let mut columns = footprint.inputs().iter().map(|&(cx, cz)| {
                let mut column = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
                let roof = y + 8 + (cx + 2 * cz).rem_euclid(3) * 21;
                for z in 0..16 {
                    for x in 0..16 {
                        if (x + z) % 5 != 0 {
                            column.set_block_id(x, roof + (x % 3), z, stone);
                        }
                    }
                }
                (cx, cz, column)
            }).collect::<Vec<_>>();
            let (_, _, emitting) = columns.iter_mut()
                .find(|(cx, cz, _)| (*cx, *cz) == (41, -27)).unwrap();
            emitting.set_block_id(15, y, 8, glowstone);
            emitting.set_block_id(8, y + 3, 15, glowstone);
            emitting.set_block_id(3, y + 90, 3, cave_air);
            for (_, _, column) in &mut columns {
                column.prime_client_heightmaps();
            }

            let actual = protocol.compute_resident_light_batch(&outputs, &columns, dimension)
                .expect("resident computation supported").await.unwrap();
            let synchronous = protocol.detached_resident_light_compute()
                .expect("detached resident computation supported")(&outputs, &columns, dimension)
                .unwrap();
            assert_eq!(actual, synchronous, "cooperative and worker results: {dimension:?}");
            assert_eq!(
                actual.iter().map(|(position, _)| *position).collect::<Vec<_>>(),
                footprint.outputs(),
                "only explicit outputs are returned",
            );
            for &((cx, cz), ref light) in &actual {
                let center = &columns.iter()
                    .find(|(x, z, _)| (*x, *z) == (cx, cz)).unwrap().2;
                let neighbours = columns.iter().filter_map(|(x, z, column)| {
                    let (dx, dz) = (*x - cx, *z - cz);
                    ((dx, dz) != (0, 0) && dx.abs() <= 1 && dz.abs() <= 1)
                        .then_some((dx, dz, column))
                }).collect::<Vec<_>>();
                let expected = compute_served_light_with_neighbours(center, &neighbours, dimension);
                for section in 0..light.light_section_count() {
                    assert_eq!(light.sky(section), expected.sky(section),
                        "sky at column ({cx}, {cz}), section {section}, {dimension:?}");
                    assert_eq!(light.block(section), expected.block(section),
                        "block at column ({cx}, {cz}), section {section}, {dimension:?}");
                }
            }
            for (position, x, sample_y, z) in [((42, -27), 0, y, 8), ((41, -26), 8, y + 3, 0)] {
                let light = &actual.iter().find(|(p, _)| *p == position).unwrap().1;
                let relative_y = (sample_y - shape.min_y) as usize;
                assert_eq!(
                    light.block(relative_y / 16 + 1).get(NibbleArray::index(x, relative_y % 16, z)),
                    Some(14),
                    "one-step emission crossing at column {position:?}, cell ({x}, {sample_y}, {z})",
                );
            }
            let missing = &columns[..columns.len() - 1];
            assert_eq!(
                protocol.compute_resident_light_batch(&outputs, missing, dimension)
                    .expect("resident computation supported").await,
                Err(ResidentLightError::MissingInput),
            );
        }
    }

    #[test]
    fn served_light_keeps_air_variants_above_the_surface() {
        struct FullScan<'a> {
            column: &'a ServerChunkColumn,
            shape: &'a ChunkShape,
        }

        impl BlockVolume for FullScan<'_> {
            fn block(&self, x: usize, y: i32, z: usize) -> u32 {
                self.column.block_state_id(x as i32, y, z as i32).raw()
            }

            fn air_state(&self) -> u32 {
                self.shape.air_id
            }

            fn min_y(&self) -> i32 {
                self.shape.min_y
            }

            fn section_count(&self) -> usize {
                self.shape.section_count
            }
        }

        let dimension = Dimension::Overworld;
        let shape = shape_for_dimension(dimension);
        let mut center = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let mut east = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let stone = StateId::from_state_str("minecraft:stone").expect("stone state");
        let cave_air = StateId::from_state_str("minecraft:cave_air").expect("cave air state");
        let glowstone = StateId::from_state_str("minecraft:glowstone").expect("glowstone state");
        center.set_block_id(8, 68, 8, stone);
        center.set_block_id(8, 120, 8, cave_air);
        center.set_block_id(15, 77, 8, glowstone);
        east.set_block_id(0, 80, 8, stone);
        center.prime_client_heightmaps();
        east.prime_client_heightmaps();
        assert!(center.air_above_y() > 120);
        assert_eq!(
            center.client_heightmaps_raw().unwrap()[0][8 + 8 * 16],
            (69 - shape.min_y) as u16
        );

        let props = V770LightProps { has_skylight: true };
        let full_center = FullScan {
            column: &center,
            shape: &shape,
        };
        let full_east = FullScan {
            column: &east,
            shape: &shape,
        };
        let expected = compute_column_light_with_neighbours(
            &Neighbourhood::new(&full_center).with(1, 0, &full_east),
            &props,
        );
        let actual = compute_served_light_with_neighbours(&center, &[(1, 0, &east)], dimension);
        assert_eq!(actual, expected);
    }
}

#[cfg(test)]
mod packet_count_tests {
    use super::*;

    #[test]
    fn section_packet_counts_share_one_cell_census() {
        let kind = lodestone_world::PaletteKind::block_states();
        let biomes = lodestone_world::PaletteKind::biomes();
        let mut section = ChunkSection::new(kind, biomes, 0, 0);
        for (x, state) in [
            (0, "minecraft:stone"),
            (1, "minecraft:water[level=7]"),
            (2, "minecraft:cave_air"),
            (3, "minecraft:void_air"),
        ] {
            section.set_block(
                x,
                0,
                0,
                lodestone_data::block_states::state_id(state).expect("state"),
            );
        }
        assert_eq!(
            section_packet_counts(Wire::BASE, &section),
            SectionPacketCounts {
                non_empty: 2,
                fluid: 1,
            }
        );
    }
}

/// Encodes one [`WorldChunkColumn`] into the `level_chunk_with_light` body,
/// mirroring `LevelChunkWithLight`'s decode in `packets::chunk` exactly:
/// `x`, `z`, the three client heightmaps, the length-prefixed section blob (per
/// section two leading shorts — non-air count then fluid count — then the
/// block-state container then the biome container), the block-entity list
/// ([`encode_block_entities`]), then the trailing light payload.
///
/// Every client heightmap is freshly scanned from the exact state ids this
/// packet writes. That makes a generated, imported, or player-edited column
/// agree with its own payload; a retained generator `MOTION_BLOCKING` snapshot
/// is no longer a stale special case. `Heightmap::new` picks the 9-bit width
/// from the shape, so no width is chosen here.
///
/// **`light` is no longer all-`Missing`.** It is the caller's
/// computed [`ColumnLight`]; see [`compute_served_light`] for where it comes
/// from and what `Missing` used to mean on the client.
pub(super) fn encode_column_body(
    wire: Wire,
    cx: i32,
    cz: i32,
    shape: &ChunkShape,
    column: &WorldChunkColumn,
    light: &ColumnLight,
    source: &ServerChunkColumn,
) -> Vec<u8> {
    // Light is computed over canonical states; the packet carries the release's
    // own block-state and biome ids, so a release other than the built-in one
    // rebuilds the column in those ids.
    let wire_shape;
    let wire_column;
    let (shape, column) = if wire.release().is_some() {
        wire_shape = wire.shape(shape.clone());
        wire_column = build_wire_column(wire, &wire_shape, source);
        (&wire_shape, &wire_column)
    } else {
        (shape, column)
    };
    let mut w = Writer::default();
    w.i32(cx);
    w.i32(cz);

    let heightmaps = served_heightmaps(shape, source);
    heightmaps.encode(&mut w);

    let mut section_blob = Writer::default();
    for section_index in 0..shape.section_count {
        // A freshly synthesized empty section for indices the column elided
        // (all-air, default biome) — every section index still gets bytes on
        // the wire; there is no "skip empty section" shortcut.
        let synthesized;
        let section = match column.section(section_index) {
            Some(section) => section,
            None => {
                synthesized = ChunkSection::new(
                    shape.block_kind,
                    shape.biome_kind,
                    shape.air_id,
                    shape.biome_id,
                );
                &synthesized
            }
        };
        let counts = section_packet_counts(wire, section);
        section_blob.i16(counts.non_empty as i16);
        section_blob.i16(counts.fluid as i16);
        section.block_states().encode(&mut section_blob);
        section.biomes().encode(&mut section_blob);
    }
    let section_bytes = section_blob.into_vec();
    w.var_i32(section_bytes.len() as i32);
    w.bytes(&section_bytes);

    encode_block_entities(wire, &mut w, source);

    debug_assert_eq!(
        light.light_section_count(),
        shape.section_count + 2,
        "light must span the shape's `section_count + 2` light sections"
    );
    light.encode_with(wire.bit_set_wire(), &mut w);

    w.into_vec()
}

/// The three client-visible heightmap type ids in the protocol-776 registry.
/// They are explicit ids, not declaration positions: the source registry gives
/// the world-generation-only forms ids 0, 2, and 3, leaving the client forms
/// at 1, 4, and 5.
pub(super) const WORLD_SURFACE_HEIGHTMAP_TYPE_ID: u32 = 1;
pub(super) const MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID: u32 = 5;

/// Builds every heightmap a completed chunk sends from its current state ids.
///
/// The stored value is the first free Y relative to the wire shape's minimum,
/// so the all-air answer is zero. Scanning the shape rather than the source's
/// allocation is deliberate: short test columns are padded with air on the
/// wire and must therefore have the same heightmaps as their decoded form.
pub(super) fn served_heightmaps(shape: &ChunkShape, source: &ServerChunkColumn) -> Heightmaps {
    if let Some(retained) = source.client_heightmaps() {
        return retained.clone();
    }
    let mut maps = Heightmaps::new();
    let type_ids = [
        WORLD_SURFACE_HEIGHTMAP_TYPE_ID,
        MOTION_BLOCKING_HEIGHTMAP_TYPE_ID,
        MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID,
    ];
    for type_id in type_ids {
        let mut map = Heightmap::new(shape.world_height);
        for z in 0..16i32 {
            for x in 0..16i32 {
                let stored = (shape.min_y..shape.min_y + shape.world_height as i32)
                    .rev()
                    .find(|&y| client_heightmap_includes(type_id, source.block_state_id(x, y, z)))
                    .map_or(0, |y| (y + 1 - shape.min_y) as u32);
                map.set(x as usize, z as usize, stored);
            }
        }
        maps.insert(type_id, map);
    }
    maps
}

pub(super) fn heightmap_world_surface(state: lodestone_data::block_states::StateId) -> bool {
    state != lodestone_data::block_states::air_state()
}

pub(super) fn heightmap_motion_blocking(state: lodestone_data::block_states::StateId) -> bool {
    lodestone_data::block_solidity::blocks_motion(state)
        || lodestone_data::snow_support::has_fluid_state(state)
}

pub(super) fn heightmap_motion_blocking_no_leaves(state: lodestone_data::block_states::StateId) -> bool {
    heightmap_motion_blocking(state) && !is_leaves(state.block())
}

/// Returns the authoritative inclusion predicate for one of the three
/// client-visible heightmap registry ids. The packet encoder and the
/// light-free parity record share this boundary, so a new block census cannot
/// make the two content paths disagree about leaves, fluids, or air.
pub fn client_heightmap_includes(
    type_id: u32,
    state: lodestone_data::block_states::StateId,
) -> bool {
    match type_id {
        WORLD_SURFACE_HEIGHTMAP_TYPE_ID => heightmap_world_surface(state),
        MOTION_BLOCKING_HEIGHTMAP_TYPE_ID => heightmap_motion_blocking(state),
        MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID => heightmap_motion_blocking_no_leaves(state),
        _ => false,
    }
}

pub(super) fn is_leaves(block: Block) -> bool {
    lodestone_data::tool::builtin_block_tag_contains("minecraft:leaves", block)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SectionPacketCounts {
    non_empty: u16,
    fluid: u16,
}

/// Counts both section header fields while reading each packed cell once.
fn section_packet_counts(wire: Wire, section: &ChunkSection) -> SectionPacketCounts {
    let mut counts = SectionPacketCounts {
        non_empty: 0,
        fluid: 0,
    };
    for index in 0..section.block_states().entry_count() {
        let Some(state) = wire.state_from_wire(section.block_states().get(index)) else {
            continue;
        };
        if !matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir) {
            counts.non_empty += 1;
        }
        if lodestone_data::snow_support::has_fluid_state(state) {
            counts.fluid += 1;
        }
    }
    counts
}

/// Writes the chunk packet's block-entity array: a VarInt count
/// then, per entry, the section-relative XZ packed into one byte (`x << 4 | z`),
/// the **absolute** Y as a big-endian short, the block-entity type's registry id
/// as a VarInt, and the network-NBT update payload. Exactly the layout
/// [`lodestone_world::BlockEntity::decode`] reads back.
///
/// This used to be a hardcoded `var_i32(0)` — every chunk claiming it held no
/// block entities at all, so a generated bee nest arrived as a decorative block
/// with nothing inside it and a chest loaded off disk arrived empty.
///
/// An entry is **skipped** — count included — when its type key does not resolve
/// in this version's registry, or when its NBT does not serialize. Both are
/// filtered *before* the count is written, which is the whole reason the payload
/// is built into a scratch buffer per entry rather than straight into `w`: a
/// wrong VarInt type id merely mis-draws one entity, while a count that does not
/// match the records that follow desynchronises the stream and takes the
/// connection down. An `Opaque` entity's update tree may come from a region
/// file we did not write, so "this NBT does not encode" is a real input, not an
/// invariant to `expect` on.
pub(super) fn encode_block_entities(wire: Wire, w: &mut Writer, source: &ServerChunkColumn) {
    let mut entries: Vec<(
        lodestone_model::BlockPos,
        lodestone_data::block_entity_types::BlockEntityType,
        i32,
        Vec<u8>,
    )> = source
        .block_entities()
        .iter()
        .filter_map(|(pos, entity)| {
            let type_id =
                lodestone_data::block_entity_types::block_entity_type_id(entity.type_id())?;
            let wire_type = wire.fixed(
                crate::dialect::FixedRegistryKind::BlockEntity,
                i32::try_from(type_id.raw()).ok()?,
            )?;
            let mut nbt = lodestone_server::chunk_nbt::block_entity_update_nbt(*pos, entity);
            // The external packet control stabilizes compounds recursively by
            // unsigned UTF-8 key order before writing them. This is the same
            // canonicalization used for the rest of the raw-packet fixture;
            // retaining the producer's field order would leave the payload
            // semantically equal but byte-different.
            stabilize_block_entity_nbt(&mut nbt);
            let mut body = Writer::default();
            write_network_nbt(&mut body, &nbt).ok()?;
            Some((*pos, type_id, wire_type, body.into_vec()))
        })
        .collect();

    // The external packet control orders the materialized chunk records by
    // packed local XZ, then absolute Y and registry id. Canonicalizing here
    // avoids depending on the generator's sidecar insertion order; it is not
    // a payload or coordinate-specific special case.
    entries.sort_unstable_by_key(|(pos, type_id, _, _)| {
        (
            ((pos.x & 15) << 4) | (pos.z & 15),
            pos.y,
            type_id.raw(),
        )
    });

    w.var_i32(entries.len() as i32);
    for (pos, _, wire_type, nbt) in entries {
        w.u8((((pos.x & 15) << 4) | (pos.z & 15)) as u8);
        w.i16(pos.y as i16);
        w.var_i32(wire_type);
        w.bytes(&nbt);
    }
}

/// Canonicalizes the compound field order used by the authenticated packet
/// controls. NBT compounds are maps semantically, but their wire encoding is
/// an ordered byte stream; recursively sorting keys keeps generated and
/// persisted payloads byte-identical without changing any values or list
/// element order.
pub(super) fn stabilize_block_entity_nbt(nbt: &mut Nbt) {
    match nbt {
        Nbt::Compound(fields) => {
            for (_, value) in fields.iter_mut() {
                stabilize_block_entity_nbt(value);
            }
            fields.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
        }
        Nbt::List { elements, .. } => {
            for value in elements {
                stabilize_block_entity_nbt(value);
            }
        }
        Nbt::End
        | Nbt::Byte(_)
        | Nbt::Short(_)
        | Nbt::Int(_)
        | Nbt::Long(_)
        | Nbt::Float(_)
        | Nbt::Double(_)
        | Nbt::ByteArray(_)
        | Nbt::String(_)
        | Nbt::IntArray(_)
        | Nbt::LongArray(_) => {}
    }
}

/// The 26.2 [`LightProperties`] the served-chunk light engine runs against:
/// `lodestone-data`'s per-block-state dampening/emission census, read straight
/// out of rodata.
///
/// A zero-sized adapter rather than a table this crate builds, so there is no
/// per-column setup cost and nothing to cache. See
/// [`lodestone_data::light_props`] for the provenance argument — in particular
/// that every gap in it darkens rather than brightens.
pub(super) struct V770LightProps {
    pub(super) has_skylight: bool,
}

impl LightProperties for V770LightProps {
    fn has_skylight(&self) -> bool {
        self.has_skylight
    }

    fn opacity(&self, state: u32) -> u8 {
        lodestone_data::block_states::StateId::new(state)
            .map_or(0, lodestone_data::light_props::dampening)
    }

    fn emission(&self, state: u32) -> u8 {
        lodestone_data::block_states::StateId::new(state)
            .map_or(0, lodestone_data::light_props::emission)
    }
}

/// Computes the sky and block light for one served column.
///
/// # Why this exists at all
///
/// Until this landed, every column the integrated server sent carried
/// `ColumnLight::new(section_count)` — all-`LightData::Missing`
/// (`lodestone_world::light`), for both layers, in every section. That is a
/// legal wire form, and it is *not* "no light": a client resolves an absent sky
/// section to its dimension default, which in the overworld is **full daylight**
/// (`lodestone_render::SkyDefault::Full`; vanilla's own client does the same
/// through `SkyLightSectionStorage`). So the symptom was a **fully bright**
/// world — caves and sealed rooms included — not a dark one. Anyone hunting this
/// bug by looking for blackness was looking for the wrong colour.
///
/// # Where light is computed, and what that costs
///
/// Here, at serve time, over the [`WorldChunkColumn`] `build_world_column` has
/// already materialised. That is not where it *belongs* — see the seam note
/// below — but it is the only place reachable from
/// [`ServerProtocol::encode_chunk`], whose signature carries one column and no
/// access to the [`lodestone_server::ChunkSource`] the neighbours live in.
///
/// The cost is bounded by construction: the flood is `O(cells)` over the
/// `(section_count + 2) * 4096` cells of one column with a 15-bucket queue, and
/// it runs on whatever thread already paid for `build_world_column`'s 98,304
/// `resolve_state_id` lookups — a far larger constant. `tests/server_light.rs`
/// measures the ratio of the two in one process, which is the only honest way to
/// state a cost on this machine (an absolute duration gets attributed to
/// concurrent load).
///
/// # The cross-chunk seam, and why this is the isolated compute
///
/// Sky and block light cross column boundaries, so the exact answer for a column
/// needs its eight neighbours — that is what
/// `lodestone_world::compute_column_light_with_neighbours` is for, and it is exact, because
/// light decays at least one level per block and `15 < 16` so no source beyond
/// the immediate neighbours can reach the centre.
///
/// The one-column `ChunkEncoder` remains isolated because it is movable into a
/// worker that only owns one generated column. The ordinary server path detects
/// this family's cross-column capability, keeps that work on the connection
/// task, and calls `try_encode_chunk_with_neighbours` with every already
/// resident neighbour. A missing neighbour is an opaque seam rather than an
/// implicit generation request, so join order never changes terrain or blocks
/// input handling. The same 3×3 computation serves light updates.
pub(super) fn compute_served_light(column: &WorldChunkColumn, dimension: Dimension) -> ColumnLight {
    compute_column_light(
        column,
        &V770LightProps {
            has_skylight: dimension.has_skylight(),
        },
    )
}

/// The initial chunk light packet is deliberately framed separately from a
/// later light update. The Overworld's compact form retains one full-sky
/// section above terrain. A freshly generated End fallback has no settled
/// storage snapshot yet, so it keeps the complete computed sky result and
/// applies the same sparse section-allocation rule as the light engine. The
/// production server normally supplies the exact snapshot captured at its
/// light fence, which remains authoritative. The Nether has no sky, so its
/// value is immaterial there.
pub(super) const fn initial_full_sky_sections(dimension: Dimension) -> usize {
    match dimension {
        Dimension::End => usize::MAX,
        Dimension::Overworld | Dimension::Nether => 1,
    }
}

/// Returns the exact block-light storage mask implied by a loaded 3x3 chunk
/// footprint.
///
/// The light engine allocates a zero-filled section for every non-air block
/// section and for each of its 26 immediately adjacent section nodes. In the
/// packet's coordinate system, a non-air block section therefore allocates its
/// own light section and one section below and above it. A neighbouring column
/// can consequently add one explicit empty section even when it has no
/// emissive block at that height. The result is intentionally a sparse mask:
/// unallocated holes remain Missing rather than being filled merely because a
/// higher section is allocated. Retained snapshots bypass this reconstruction
/// and remain authoritative.
pub(super) fn initial_block_light_storage_sections(
    center: &WorldChunkColumn,
    neighbours: &[WorldChunkColumn],
) -> Vec<bool> {
    let mut stored = vec![false; center.section_count() + 2];
    for column in std::iter::once(center).chain(neighbours) {
        for block_section in 0..column.section_count() {
            // A section with only a non-default biome is still empty to the
            // light storage allocator; only block occupancy changes status.
            if !column
                .section(block_section)
                .is_some_and(|section| !section.is_air_only())
            {
                continue;
            }
            for light_section in block_section..=block_section + 2 {
                if light_section < stored.len() {
                    stored[light_section] = true;
                }
            }
        }
    }
    stored
}

/// Describes a retained Nether light layer independently from its packet
/// values. A zero-valued layer can still be allocated, and a layer attached to
/// block data is distinct from a light-only dependency section.
pub(super) fn initial_nether_light_storage(
    center: &WorldChunkColumn,
    neighbours: &[WorldChunkColumn],
) -> LightStorage {
    initial_nether_light_storage_for_column(center, center, neighbours)
}

/// Builds the retained allocation metadata for one packed column in a shared
/// 3×3 admission. Allocation is a property of the complete loaded footprint,
/// while `LIGHT_AND_DATA` belongs to the selected column itself; keeping those
/// masks separate is what lets a dependency retain a light-only section rather
/// than accidentally borrowing the centre's block-data mask.
pub(super) fn initial_nether_light_storage_for_column(
    selected: &WorldChunkColumn,
    footprint_center: &WorldChunkColumn,
    neighbours: &[WorldChunkColumn],
) -> LightStorage {
    let allocated = initial_block_light_storage_sections(footprint_center, neighbours);
    let mut light_and_data = vec![false; allocated.len()];
    for block_section in 0..selected.section_count() {
        if selected
            .section(block_section)
            .is_some_and(|section| !section.is_air_only())
            // Light section zero is the lower apron, so block section `n`
            // carries its block data in light section `n + 1`.
            && let Some(slot) = light_and_data.get_mut(block_section + 1)
        {
            *slot = true;
        }
    }
    LightStorage::from_masks(allocated, light_and_data)
}

/// End's initial light engine retains a vertical propagation corridor in the
/// loaded footprint, not just the three sections adjacent to each non-air
/// section. A terrain section seeds the block and sky layers, and propagation
/// allocates the intervening empty sections from the lowest admitted non-air
/// section through the one-section light apron above the highest one. An
/// all-air footprint allocates no storage. The selected centre may itself be
/// all air: storage follows the admitted footprint, so a neighbouring terrain
/// column still contributes its corridor.
pub(super) fn initial_end_light_storage_sections(
    center: &WorldChunkColumn,
    neighbours: &[WorldChunkColumn],
) -> Vec<bool> {
    initial_end_light_storage_sections_with_prior(center, neighbours, None)
}

/// Extends End storage from an already retained centre snapshot when one
/// exists. A retained centre remains authoritative; fresh storage is derived
/// from the complete admitted footprint, including terrain in neighbouring
/// columns. This keeps a fresh all-air centre and its new dependencies on the
/// same coordinate-independent allocation rule.
pub(super) fn initial_end_light_storage_sections_with_prior(
    center: &WorldChunkColumn,
    neighbours: &[WorldChunkColumn],
    prior_center: Option<&ColumnLight>,
) -> Vec<bool> {
    let mut stored = vec![false; center.section_count() + 2];
    if let Some(prior_center) = prior_center {
        for section in 0..stored.len().min(prior_center.light_section_count()) {
            stored[section] = !matches!(prior_center.sky(section), LightData::Missing)
                || !matches!(prior_center.block(section), LightData::Missing);
        }
    }
    let mut lowest_non_air = None;
    let mut highest_non_air = None;
    for column in std::iter::once(center).chain(neighbours) {
        for block_section in 0..column.section_count() {
            if !column
                .section(block_section)
                .is_some_and(|section| !section.is_air_only())
            {
                continue;
            }
            lowest_non_air = Some(lowest_non_air.map_or(block_section, |lowest: usize| {
                lowest.min(block_section)
            }));
            highest_non_air = Some(highest_non_air.map_or(block_section, |highest: usize| {
                highest.max(block_section)
            }));
        }
    }

    let Some(highest_non_air) = highest_non_air else {
        return stored;
    };
    let first = lowest_non_air.expect("End storage has a non-air section");
    let last = highest_non_air
        .saturating_add(2)
        .min(stored.len().saturating_sub(1));
    for section in first..=last {
        stored[section] = true;
    }
    stored
}

/// Retains explicit zero block-light sections only where the light engine has
/// allocated storage, eliding unallocated sections. Values are never touched:
/// a non-uniform section carries a real source or propagation result even if
/// its first byte happens to be zero.
pub(super) fn retain_zero_block_light_for_storage(light: &mut ColumnLight, stored: &[bool]) {
    debug_assert_eq!(stored.len(), light.light_section_count());
    for section in 0..light.light_section_count() {
        if matches!(light.block(section), LightData::Uniform(0))
            && !stored.get(section).copied().unwrap_or(false)
        {
            *light.block_mut(section) = LightData::Missing;
        }
    }
}

/// Replaces the numeric block-light values in a freshly computed centre while
/// retaining that centre's `Missing`/allocated representation. A dependency
/// snapshot can carry the light-engine values from an earlier admission, but
/// its raw storage metadata is for the dependency's footprint and must not be
/// copied into the centre packet. In particular, a missing retained layer is
/// represented as zero inside an already allocated computed layer, while an
/// unallocated computed layer remains `Missing`.
pub(super) fn merge_retained_nether_block_values(
    computed: &ColumnLight,
    retained: &ColumnLight,
) -> ColumnLight {
    debug_assert_eq!(computed.light_section_count(), retained.light_section_count());
    let mut merged = computed.clone();
    for section in 0..computed.light_section_count() {
        let computed_layer = computed.block(section);
        let retained_layer = retained.block(section);
        let replacement = match computed_layer {
            LightData::Missing => LightData::Missing,
            LightData::Uniform(_) => match retained_layer {
                LightData::Missing => LightData::Uniform(0),
                LightData::Uniform(value) => LightData::Uniform(*value),
                LightData::Values(values) => match values.uniform_value() {
                    Some(value) => LightData::Uniform(value),
                    None => LightData::Values(values.clone()),
                },
            },
            LightData::Values(_) => {
                let values = match retained_layer {
                    LightData::Missing => NibbleArray::filled(0),
                    LightData::Uniform(value) => NibbleArray::filled(*value),
                    LightData::Values(values) => values.clone(),
                };
                LightData::Values(values)
            }
        };
        *merged.block_mut(section) = replacement;
    }
    merged
}

/// Applies only the dimension-specific representation rules for an initial
/// chunk packet. A later `light_update` has prior client state to clear and is
/// intentionally left in the fully explicit representation returned by the
/// light engine.
pub(super) fn normalize_initial_chunk_light(
    light: &mut ColumnLight,
    dimension: Dimension,
    block_light_storage: Option<&[bool]>,
) {
    match dimension {
        Dimension::Overworld => elide_zero_block_light_for_chunk(light),
        Dimension::Nether => {
            // A no-skylight initial chunk has no sky mask at all. This differs
            // from a later update, where explicit zero values clear old light.
            for section in 0..light.light_section_count() {
                *light.sky_mut(section) = LightData::Missing;
            }
            let storage = block_light_storage
                .expect("Nether initial light normalization needs storage allocation");
            retain_zero_block_light_for_storage(light, storage);
        }
        // A retained End snapshot bypasses this fallback and is consumed
        // verbatim by the encoder. For a generated column, mirror the sparse
        // storage mask so an unallocated lower apron is not mistaken for a
        // computed light value. This is only a representation rule: retained
        // snapshots remain authoritative and are never normalized here.
        Dimension::End => {
            let storage = block_light_storage
                .expect("End initial light normalization needs storage allocation");
            debug_assert_eq!(storage.len(), light.light_section_count());
            for (section, &is_stored) in storage.iter().enumerate() {
                if !is_stored {
                    *light.sky_mut(section) = LightData::Missing;
                    *light.block_mut(section) = LightData::Missing;
                }
            }
        }
    }
}

/// Restores the explicit zero layers inside a retained End allocation span.
///
/// End's initial storage is a contiguous vertical corridor. Persistence can
/// omit an all-zero sky and block pair in the middle of that corridor, while
/// retaining the non-zero layers on either side. The wire snapshot still has
/// to name that allocated section as empty for both layers; otherwise a fresh
/// client sees a shorter mask after reopening. Only interior gaps are filled,
/// so a genuinely sparse lower or upper apron remains absent.
pub(super) fn restore_end_retained_storage_gaps(light: &mut ColumnLight) {
    let mut first = None;
    let mut last = None;
    for section in 0..light.light_section_count() {
        let allocated = !matches!(light.sky(section), LightData::Missing)
            || !matches!(light.block(section), LightData::Missing);
        if allocated {
            first = Some(first.map_or(section, |first: usize| first.min(section)));
            last = Some(last.map_or(section, |last: usize| last.max(section)));
        }
    }
    let (Some(first), Some(last)) = (first, last) else {
        return;
    };
    for section in first..=last {
        if matches!(light.sky(section), LightData::Missing) {
            *light.sky_mut(section) = LightData::Uniform(0);
        }
        if matches!(light.block(section), LightData::Missing) {
            *light.block_mut(section) = LightData::Uniform(0);
        }
    }
}

/// Rebuilds the End allocation mask that is not persisted with a retained
/// snapshot. A saved `CentreSettled` column carries its explicit sky and block
/// arrays, but an all-zero allocated layer may have no section tag on disk. The
/// current terrain footprint recovers that allocation; allocated missing layers
/// become explicit zero while layers outside the corridor stay Missing.
pub(super) fn restore_end_retained_storage_allocation(light: &mut ColumnLight, stored: &[bool]) {
    debug_assert_eq!(stored.len(), light.light_section_count());
    if (0..light.light_section_count())
        .all(|section| matches!(light.sky(section), LightData::Missing)
            && matches!(light.block(section), LightData::Missing))
    {
        return;
    }
    for section in 0..light.light_section_count() {
        if !stored.get(section).copied().unwrap_or(false) {
            *light.sky_mut(section) = LightData::Missing;
            *light.block_mut(section) = LightData::Missing;
            continue;
        }
        if matches!(light.sky(section), LightData::Missing) {
            *light.sky_mut(section) = LightData::Uniform(0);
        }
        if matches!(light.block(section), LightData::Missing) {
            *light.block_mut(section) = LightData::Uniform(0);
        }
    }
}

pub(super) fn compute_served_initial_light(column: &WorldChunkColumn, dimension: Dimension) -> ColumnLight {
    let mut light = compute_column_light_for_initial_chunk(
        column,
        &V770LightProps {
            has_skylight: dimension.has_skylight(),
        },
        initial_full_sky_sections(dimension),
    );
    let block_light_storage = (dimension == Dimension::Nether || dimension == Dimension::End)
        .then(|| initial_block_light_storage_sections(column, &[]));
    normalize_initial_chunk_light(&mut light, dimension, block_light_storage.as_deref());
    light
}

/// Initial chunk packets omit block-light sections whose computed values are
/// uniformly zero. A present zero section is meaningful for a light update,
/// where it clears a previously known value, but the initial chunk packet has
/// no prior block-light state to clear. Keep non-zero arrays and uniform sky
/// sections unchanged; only the initial-chunk wire representation uses this
/// elision.
pub(super) fn elide_zero_block_light_for_chunk(light: &mut ColumnLight) {
    for section in 0..light.light_section_count() {
        if matches!(light.block(section), LightData::Uniform(0)) {
            *light.block_mut(section) = LightData::Missing;
        }
    }
}

/// Computes a served light update with the loaded 3×3 neighbourhood. The
/// server supplies every neighbouring source column after a block change; this
/// conversion keeps state resolution and the light census on the same side of
/// the version seam as the ordinary column encoder.
pub(super) fn compute_served_light_with_neighbours(
    column: &ServerChunkColumn,
    neighbours: &[(i32, i32, &ServerChunkColumn)],
    dimension: Dimension,
) -> ColumnLight {
    let shape = shape_for_dimension(dimension);
    let center = ServerLightVolume {
        source: column,
        shape: &shape,
    };
    let neighbour_volumes = neighbours
        .iter()
        .map(|(_, _, neighbour)| ServerLightVolume {
            source: neighbour,
            shape: &shape,
        })
        .collect::<Vec<_>>();
    let mut neighbourhood = Neighbourhood::new(&center);
    for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_volumes) {
        if (*dx, *dz) == (0, 0) {
            continue;
        }
        neighbourhood = neighbourhood.with(*dx, *dz, neighbour);
    }
    compute_column_light_with_neighbours(
        &neighbourhood,
        &V770LightProps {
            has_skylight: dimension.has_skylight(),
        },
    )
}

pub(super) async fn compute_served_resident_light_batch(
    outputs: &[(i32, i32)],
    columns: &[(i32, i32, ServerChunkColumn)],
    dimension: Dimension,
) -> Result<Vec<((i32, i32), ColumnLight)>, ResidentLightError> {
    use lodestone_server::worldgen_progress::time_resident_light_step;

    let shape = shape_for_dimension(dimension);
    let props = V770LightProps { has_skylight: dimension.has_skylight() };
    let volumes = columns.iter().map(|(_, _, source)| ServerLightVolume {
        source,
        shape: &shape,
    }).collect::<Vec<_>>();
    let inputs = resident_light_inputs(outputs, columns, &volumes)?;
    let mut job = time_resident_light_step(0, || ResidentLightJob::new(inputs, &props));
    let budget = std::num::NonZeroUsize::new(16_384).expect("nonzero resident light slice");
    let mut items = outputs.len() as u32;
    loop {
        let progress = time_resident_light_step(items, || job.step(budget));
        items = 0;
        if progress == ResidentLightProgress::Ready {
            break;
        }
        #[cfg(target_arch = "wasm32")]
        lodestone_time::browser_yield().await;
    }
    match job.into_result() {
        Ok(result) => Ok(result.columns),
        Err(_) => unreachable!("a completed resident solve has a result"),
    }
}

pub(super) fn compute_served_resident_light_batch_sync(
    outputs: &[(i32, i32)],
    columns: &[(i32, i32, ServerChunkColumn)],
    dimension: Dimension,
) -> Result<Vec<((i32, i32), ColumnLight)>, ResidentLightError> {
    use lodestone_server::worldgen_progress::time_resident_light_step;

    let shape = shape_for_dimension(dimension);
    let props = V770LightProps { has_skylight: dimension.has_skylight() };
    let volumes = columns.iter().map(|(_, _, source)| ServerLightVolume {
        source,
        shape: &shape,
    }).collect::<Vec<_>>();
    let inputs = resident_light_inputs(outputs, columns, &volumes)?;
    let job = time_resident_light_step(0, || ResidentLightJob::new(inputs, &props));
    Ok(time_resident_light_step(outputs.len() as u32, || job.finish()).columns)
}

fn resident_light_inputs<'a>(
    outputs: &[(i32, i32)],
    columns: &[(i32, i32, ServerChunkColumn)],
    volumes: &'a [ServerLightVolume<'a>],
) -> Result<ResidentLightInputs<'a, ServerLightVolume<'a>>, ResidentLightError> {
    let footprint = ResidentLightFootprint::new(outputs.iter().copied())?;
    ResidentLightInputs::new(
        footprint,
        columns.iter().zip(volumes).map(|((cx, cz, _), volume)| (*cx, *cz, volume)),
    )
}

pub(super) struct ServerLightVolume<'a> {
    source: &'a ServerChunkColumn,
    shape: &'a ChunkShape,
}

impl BlockVolume for ServerLightVolume<'_> {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        self.source.block_state_id(x as i32, y, z as i32).raw()
    }

    fn air_state(&self) -> u32 {
        self.shape.air_id
    }

    fn min_y(&self) -> i32 {
        self.shape.min_y
    }

    fn section_count(&self) -> usize {
        self.shape.section_count
    }

    fn air_above_y(&self) -> i32 {
        self.source.air_above_y()
    }
}

/// Computes the initial packet value and its allocation mask from a supplied
/// footprint. `admit_neighbour_sources` is false for partial packet inputs,
/// making fresh neighbour emission wait for a complete retained admission.
pub(super) fn compute_served_initial_light_with_neighbours(
    center: &WorldChunkColumn,
    shape: &ChunkShape,
    neighbours: &[(i32, i32, &ServerChunkColumn)],
    dimension: Dimension,
    admit_neighbour_sources: bool,
) -> ColumnLight {
    let neighbour_columns = neighbours
        .iter()
        .map(|(_, _, neighbour)| build_world_column(shape, neighbour))
        .collect::<Vec<_>>();
    let mut neighbourhood = Neighbourhood::new(center);
    for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_columns) {
        if (*dx, *dz) != (0, 0) {
            neighbourhood = neighbourhood.with(*dx, *dz, neighbour);
        }
    }
    let mut light = compute_column_light_with_neighbours_for_initial_chunk(
        &neighbourhood,
        &V770LightProps {
            has_skylight: dimension.has_skylight(),
        },
        initial_full_sky_sections(dimension),
    );
    if dimension == Dimension::Nether {
        // The initial no-skylight packet admits centre and cardinal block-light
        // sources. Diagonal sources wait for the later light-update pass. Keep
        // the all-neighbour computation above because its storage footprint is
        // still needed by the packet representation below.
        let mut admitted_neighbourhood = Neighbourhood::new(center);
        if admit_neighbour_sources {
            for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_columns) {
                if dx.abs() + dz.abs() == 1 {
                    admitted_neighbourhood = admitted_neighbourhood.with(*dx, *dz, neighbour);
                }
            }
        }
        let admitted_light = compute_column_light_with_neighbours_for_initial_chunk(
            &admitted_neighbourhood,
            &V770LightProps { has_skylight: false },
            initial_full_sky_sections(dimension),
        );
        for section in 0..light.light_section_count() {
            *light.block_mut(section) = admitted_light.block(section).clone();
        }
    }
    let block_light_storage = (dimension == Dimension::Nether || dimension == Dimension::End)
        .then(|| initial_block_light_storage_sections(center, &neighbour_columns));
    normalize_initial_chunk_light(&mut light, dimension, block_light_storage.as_deref());
    light
}

/// Whether the supplied packet neighbourhood contains every immediate
/// neighbour. A partial neighbourhood is an incomplete admission boundary:
/// its centre can be encoded, but fresh neighbour emissions must wait for the
/// retained-light settlement that has a complete footprint.
pub(super) fn neighbours_have_complete_footprint(
    neighbours: &[(i32, i32, &ServerChunkColumn)],
) -> bool {
    const OFFSETS: [(i32, i32); 8] = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    OFFSETS
        .iter()
        .all(|offset| neighbours.iter().any(|(dx, dz, _)| (*dx, *dz) == *offset))
}

pub(super) fn compute_served_initial_lights_with_neighbours_and_storage(
    center: InitialLightVolume<'_>,
    shape: &ChunkShape,
    neighbours: &[(i32, i32, &ServerChunkColumn)],
    stored: &[Option<&ColumnLight>; 9],
    statuses: &[Option<RetainedLightStatus>; 9],
    dimension: Dimension,
) -> [ColumnLight; 9] {
    if dimension == Dimension::Overworld {
        return compute_overworld_initial_lights_borrowed(center, shape, neighbours);
    }
    let InitialLightVolume::Buffered(center) = center else {
        unreachable!("dimension-specific initial storage requires a buffered centre");
    };
    let neighbour_columns = neighbours
        .iter()
        .map(|(_, _, neighbour)| build_world_column(shape, neighbour))
        .collect::<Vec<_>>();
    let mut neighbourhood = Neighbourhood::new(center);
    for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_columns) {
        if (*dx, *dz) != (0, 0) {
            neighbourhood = neighbourhood.with(*dx, *dz, neighbour);
        }
    }
    let fresh_storage = [None; 9];
    let storage = if dimension == Dimension::Nether {
        stored
    } else {
        &fresh_storage
    };
    let mut lights = compute_column_lights_with_neighbours_and_storage(
        &neighbourhood,
        &V770LightProps {
            has_skylight: dimension.has_skylight(),
        },
        storage,
        initial_full_sky_sections(dimension),
    );
    let retained_nether_centre = match (dimension, statuses[4], stored[4]) {
        (
            Dimension::Nether,
            Some(RetainedLightStatus::DependencyInitialized),
            Some(light),
        ) => Some((*light).clone()),
        _ => None,
    };
    let admitted_neighbours = neighbours
        .iter()
        .zip(&neighbour_columns)
        .filter(|((dx, dz, _), _)| {
            let slot = ((*dz + 1) * 3 + (*dx + 1)) as usize;
            statuses
                .get(slot)
                .copied()
                .flatten()
                .is_some_and(retained_light_is_initialized)
        })
        .map(|((dx, dz, _), column)| (*dx, *dz, column.clone()))
        .collect::<Vec<_>>();
    if dimension == Dimension::Nether {
        let mut admitted_neighbourhood = Neighbourhood::new(center);
        for (dx, dz, column) in &admitted_neighbours {
            admitted_neighbourhood = admitted_neighbourhood.with(*dx, *dz, column);
        }
        let section_min_y = center.min_y();
        let seeded = compute_column_light_with_neighbours_seeded(
            &admitted_neighbourhood,
            &V770LightProps { has_skylight: false },
            initial_full_sky_sections(dimension),
            |dx, dz, x, y, z| {
                let slot = ((dz + 1) * 3 + (dx + 1)) as usize;
                let section = usize::try_from((y - section_min_y).div_euclid(16) + 1)
                    .expect("Nether light section index");
                statuses
                    .get(slot)
                    .copied()
                    .flatten()
                    .filter(|status| retained_light_is_initialized(*status))
                    .and_then(|_| stored.get(slot).and_then(Option::as_ref))
                    .and_then(|light| {
                        light
                            .block(section)
                            .get(NibbleArray::index(x, y.rem_euclid(16) as usize, z))
                    })
                    .unwrap_or(0)
            },
            |dx, dz| (dx, dz) == (0, 0),
        );
        lights[4] = seeded;
    }
    // End allocation follows the complete admitted terrain footprint. A
    // retained snapshot in one neighbour does not make a fresh terrain
    // neighbour disappear from the storage walk: using non-zero sky values as
    // a proxy for snapshot presence omitted the fresh column whenever another
    // column already supplied retained sky. Nether deliberately keeps its
    // lifecycle-aware admitted subset because an uninitialized dependency is
    // an opaque block-light seam.
    let storage_neighbours = if dimension == Dimension::Nether {
        admitted_neighbours
            .iter()
            .map(|(_, _, column)| column.clone())
            .collect::<Vec<_>>()
    } else {
        neighbour_columns.clone()
    };
    // A dependency-initialized layer records work done while another column
    // was the centre. Its raw allocation is therefore not the centre's
    // allocation. Only a centre-settled snapshot is an authoritative prior
    // mask for this centre; a sparse one must still be preserved verbatim.
    let prior_centre = (statuses[4] == Some(RetainedLightStatus::CentreSettled))
        .then_some(stored[4])
        .flatten();
    let block_light_storage = match dimension {
        Dimension::End => Some(initial_end_light_storage_sections_with_prior(
            center,
            &storage_neighbours,
            prior_centre,
        )),
        Dimension::Nether => Some(initial_block_light_storage_sections(center, &storage_neighbours)),
        Dimension::Overworld => None,
    };
    // Only the centre snapshot is serialized by this admission. The other
    // eight snapshots are retained light-engine state, so keep their raw
    // layers intact instead of applying the centre packet's sparse mask to
    // unrelated columns.
    normalize_initial_chunk_light(
        &mut lights[4],
        dimension,
        block_light_storage.as_deref(),
    );
    if dimension == Dimension::End {
        for (slot, light) in lights.iter_mut().enumerate() {
            if slot == 4 {
                continue;
            }
            if let Some(retained) = stored[slot].filter(|_| {
                statuses[slot].is_some_and(retained_light_is_initialized)
            }) {
                // A retained dependency snapshot remains authoritative for
                // that dependency, but never seeds the fresh centre flood.
                *light = retained.clone();
            }
        }
    }
    if dimension == Dimension::Nether {
        let storage = initial_nether_light_storage(center, &storage_neighbours);
        lights[4].set_storage(storage.clone());
        for (slot, light) in lights.iter_mut().enumerate() {
            if slot == 4 {
                continue;
            }
            if let Some(retained) = stored[slot] {
                if statuses[slot].is_some_and(retained_light_is_initialized) {
                    *light = retained.clone();
                    continue;
                }
            }
            let (target_dx, target_dz, selected) = neighbours
                .iter()
                .zip(&neighbour_columns)
                .find(|((dx, dz, _), _)| {
                    ((*dz + 1) * 3 + (*dx + 1)) as usize == slot
                })
                .map(|((dx, dz, _), column)| (*dx, *dz, column))
                .expect("every non-centre Nether light slot has a footprint column");
            *light = compute_nether_dependency_light(
                selected,
                target_dx,
                target_dz,
                center,
                neighbours,
                &neighbour_columns,
                stored,
                statuses,
            );
        }
    }
    if let Some(retained) = retained_nether_centre {
        // A dependency snapshot was already settled by an earlier footprint.
        // Promote its numeric block-light values when this column becomes the
        // centre, while retaining this admission's centre storage shape. Only
        // newly touched dependencies receive the fresh shared-admission result.
        lights[4] = merge_retained_nether_block_values(&lights[4], &retained);
    }
    lights
}

pub(super) enum InitialLightVolume<'a> {
    Buffered(&'a WorldChunkColumn),
    Borrowed(ServerLightVolume<'a>),
}

impl<'a> InitialLightVolume<'a> {
    pub(super) fn borrowed(source: &'a ServerChunkColumn, shape: &'a ChunkShape) -> Self {
        Self::Borrowed(ServerLightVolume { source, shape })
    }
}

impl BlockVolume for InitialLightVolume<'_> {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        match self {
            Self::Buffered(column) => column.block(x, y, z),
            Self::Borrowed(volume) => {
                if y < volume.shape.min_y
                    || y >= volume.shape.min_y + volume.shape.world_height as i32
                {
                    volume.air_state()
                } else {
                    volume.block(x, y, z)
                }
            }
        }
    }

    fn air_state(&self) -> u32 {
        match self {
            Self::Buffered(column) => column.air_state(),
            Self::Borrowed(volume) => volume.air_state(),
        }
    }

    fn min_y(&self) -> i32 {
        match self {
            Self::Buffered(column) => column.min_y(),
            Self::Borrowed(volume) => volume.min_y(),
        }
    }

    fn section_count(&self) -> usize {
        match self {
            Self::Buffered(column) => column.section_count(),
            Self::Borrowed(volume) => volume.section_count(),
        }
    }

    fn air_above_y(&self) -> i32 {
        match self {
            Self::Buffered(column) => column.air_above_y(),
            Self::Borrowed(volume) => volume.air_above_y().clamp(
                volume.shape.min_y,
                volume.shape.min_y + volume.shape.world_height as i32,
            ),
        }
    }
}

fn compute_overworld_initial_lights_borrowed(
    center: InitialLightVolume<'_>,
    shape: &ChunkShape,
    neighbours: &[(i32, i32, &ServerChunkColumn)],
) -> [ColumnLight; 9] {
    let neighbour_volumes = neighbours
        .iter()
        .map(|(_, _, source)| InitialLightVolume::Borrowed(ServerLightVolume {
            source: *source,
            shape,
        }))
        .collect::<Vec<_>>();
    let mut neighbourhood = Neighbourhood::new(&center);
    for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_volumes) {
        if (*dx, *dz) != (0, 0) {
            neighbourhood = neighbourhood.with(*dx, *dz, neighbour);
        }
    }
    let mut lights = compute_column_lights_with_neighbours_and_storage(
        &neighbourhood,
        &V770LightProps { has_skylight: true },
        &[None; 9],
        initial_full_sky_sections(Dimension::Overworld),
    );
    normalize_initial_chunk_light(&mut lights[4], Dimension::Overworld, None);
    lights
}

#[cfg(test)]
mod initial_light_view_tests {
    use super::*;

    fn buffered_lights(
        center: &WorldChunkColumn,
        shape: &ChunkShape,
        neighbours: &[(i32, i32, &ServerChunkColumn)],
    ) -> [ColumnLight; 9] {
        let columns = neighbours
            .iter()
            .map(|(_, _, column)| build_world_column(shape, column))
            .collect::<Vec<_>>();
        let mut neighbourhood = Neighbourhood::new(center);
        for ((dx, dz, _), column) in neighbours.iter().zip(&columns) {
            if (*dx, *dz) != (0, 0) {
                neighbourhood = neighbourhood.with(*dx, *dz, column);
            }
        }
        let mut lights = compute_column_lights_with_neighbours_and_storage(
            &neighbourhood,
            &V770LightProps { has_skylight: true },
            &[None; 9],
            initial_full_sky_sections(Dimension::Overworld),
        );
        normalize_initial_chunk_light(&mut lights[4], Dimension::Overworld, None);
        lights
    }

    #[test]
    fn borrowed_overworld_initial_light_matches_buffered_roof_seam_and_air() {
        let shape = shape_for_dimension(Dimension::Overworld);
        let stone = StateId::from_state_str("minecraft:stone").expect("stone state");
        let glowstone = StateId::from_state_str("minecraft:glowstone").expect("glowstone state");
        let cave_air = StateId::from_state_str("minecraft:cave_air").expect("cave air state");
        let air = StateId::from_state_str("minecraft:air").expect("air state");
        let mut columns: [ServerChunkColumn; 9] = std::array::from_fn(|slot| {
            let mut column = if slot == 4 || slot == 5 {
                ServerChunkColumn::new(shape.min_y - 16, shape.world_height as i32 + 32)
            } else {
                ServerChunkColumn::new(shape.min_y, shape.world_height as i32)
            };
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block_id(x, 0, z, stone);
                    column.set_block_id(x, 4, z, stone);
                }
            }
            column
        });
        columns[4].set_block_id(5, 4, 8, air);
        columns[5].set_block_id(0, 1, 8, glowstone);
        columns[2].set_block_id(13, 158, 9, cave_air);
        columns[5].set_block_id(1, shape.min_y - 1, 8, glowstone);
        columns[5].set_block_id(1, shape.min_y + shape.world_height as i32, 8, glowstone);
        columns[4].set_block_id(1, shape.min_y - 1, 8, glowstone);
        columns[4].set_block_id(1, shape.min_y + shape.world_height as i32, 8, glowstone);
        columns[4].set_block_id(1, shape.min_y, 8, air);
        let center = build_world_column(&shape, &columns[4]);
        let neighbours = columns
            .iter()
            .enumerate()
            .filter(|(slot, _)| *slot != 4)
            .map(|(slot, column)| (slot as i32 % 3 - 1, slot as i32 / 3 - 1, column))
            .collect::<Vec<_>>();
        let expected = buffered_lights(&center, &shape, &neighbours);
        let mut retained = ColumnLight::new(shape.section_count);
        for section in 0..retained.light_section_count() {
            *retained.sky_mut(section) = LightData::Uniform(15);
        }
        let mut stored = [None; 9];
        stored[5] = Some(&retained);
        let actual = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_and_storage_in_dimension(
                &columns[4],
                &neighbours,
                &stored,
                Dimension::Overworld,
            )
            .expect("initial Overworld footprint");
        for slot in 0..9 {
            assert_eq!(actual[slot], expected[slot], "light footprint slot {slot}");
        }
        let section = ((1 - shape.min_y).div_euclid(16) + 1) as usize;
        let cell = NibbleArray::index(15, 1, 8);
        assert_eq!(actual[4].block(section).get(cell), Some(14));
        let clipped = InitialLightVolume::borrowed(&columns[4], &shape);
        let top_y = shape.min_y + shape.world_height as i32;
        assert_eq!(clipped.block(1, shape.min_y - 1, 8), shape.air_id);
        assert_eq!(clipped.block(1, top_y, 8), shape.air_id);
        assert_eq!(clipped.air_above_y(), top_y);
        let mut raw_shape = shape.clone();
        raw_shape.min_y -= 16;
        raw_shape.world_height += 32;
        raw_shape.section_count += 2;
        let raw_volumes = columns.iter().map(|source| ServerLightVolume {
            source,
            shape: &raw_shape,
        }).collect::<Vec<_>>();
        let mut raw_neighbourhood = Neighbourhood::new(&raw_volumes[4]);
        for (slot, volume) in raw_volumes.iter().enumerate() {
            if slot != 4 {
                raw_neighbourhood = raw_neighbourhood.with(
                    slot as i32 % 3 - 1, slot as i32 / 3 - 1, volume,
                );
            }
        }
        let raw = compute_column_lights_with_neighbours_and_storage(
            &raw_neighbourhood,
            &V770LightProps { has_skylight: true },
            &[None; 9],
            initial_full_sky_sections(Dimension::Overworld),
        );
        for (y, layer) in [(shape.min_y, 1), (top_y - 1, shape.section_count)] {
            let cell = NibbleArray::index(1, y.rem_euclid(16) as usize, 8);
            let raw_layer = ((y - raw_shape.min_y).div_euclid(16) + 1) as usize;
            assert_eq!(columns[4].block_state_id(1, y, 8), air,
                "exterior emitter control target must be air at y={y}");
            assert_eq!(actual[4].block(layer).get(cell).unwrap_or(0), 0,
                "clipped exterior emitter at y={y}");
            assert_eq!(raw[4].block(raw_layer).get(cell), Some(14),
                "unclipped emitter control at y={y}");
            assert_ne!(raw[4].block(raw_layer).get(cell).unwrap_or(0),
                actual[4].block(layer).get(cell).unwrap_or(0),
                "exterior emitters must affect the control at y={y}");
        }
        let settlement = V770ServerProtocol
            .compute_initial_column_lights_with_neighbours_in_dimension(
                &columns[4], &neighbours, Dimension::Overworld,
            )
            .expect("initial Overworld settlement");
        assert_eq!(settlement.centre_light(), &expected[4]);
        assert_eq!(settlement.dependency_lights().count(), 8);
        for ((dx, dz), light) in settlement.dependency_lights() {
            let slot = ((dz + 1) * 3 + dx + 1) as usize;
            assert_eq!(light, &empty_light_storage_like(&expected[slot]),
                "fresh dependency storage at ({dx}, {dz})");
        }
        let mut settled_center = columns[4].clone();
        settled_center.set_retained_light_with_status(
            settlement.centre_light().clone(), RetainedLightStatus::CentreSettled,
        );
        let ServerDirective::Send { packet_id, payload } = V770ServerProtocol
            .try_encode_chunk_with_neighbours_in_dimension(
                7, -3, &settled_center, &neighbours, Dimension::Overworld,
            )
            .expect("settled Overworld packet")
        else {
            panic!("settled Overworld chunk must send a packet");
        };
        assert_eq!(packet_id, play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
        assert_eq!(payload, encode_column_body(
            Wire::BASE, 7, -3, &shape, &center, &expected[4], &columns[4],
        ), "settlement packet must match the buffered reference bytes");
        if let Ok(value) = std::env::var("LODESTONE_LIGHT_VIEW_PERF_ITERATIONS") {
            let iterations = value.parse::<usize>()
                .expect("LODESTONE_LIGHT_VIEW_PERF_ITERATIONS must be an integer in 1..=128");
            assert!((1..=128).contains(&iterations),
                "LODESTONE_LIGHT_VIEW_PERF_ITERATIONS must be in 1..=128");
            let mut calls = [0_usize; 2];
            let mut sums = [std::time::Duration::ZERO; 2];
            #[cfg(target_os = "macos")]
            let mut retired_sums = [(0_u64, 0_u64); 2];
            for iteration in 0..iterations {
                let order = if iteration % 2 == 0 { [0, 1] } else { [1, 0] };
                for arm in order {
                    #[cfg(target_os = "macos")]
                    let retired_started = lodestone_testsupport::process_counters::ProcessCounters::read()
                        .expect("initial light retired counters available");
                    let started = std::time::Instant::now();
                    let output = std::hint::black_box(if arm == 0 {
                        let buffered_center = build_world_column(&shape, &columns[4]);
                        compute_overworld_initial_lights_borrowed(
                            InitialLightVolume::Buffered(&buffered_center),
                            &shape,
                            &neighbours,
                        )
                    } else {
                        compute_overworld_initial_lights_borrowed(
                            InitialLightVolume::borrowed(&columns[4], &shape),
                            &shape,
                            &neighbours,
                        )
                    });
                    sums[arm] += started.elapsed();
                    #[cfg(target_os = "macos")]
                    {
                        let retired = lodestone_testsupport::process_counters::ProcessCounters::read()
                            .expect("initial light retired counters available")
                            .since(retired_started).expect("monotonic retired counters");
                        retired_sums[arm].0 += retired.instructions;
                        retired_sums[arm].1 += retired.cycles;
                    }
                    calls[arm] += 1;
                    drop(output);
                }
            }
            #[cfg(target_os = "macos")]
            let retired_report = format!(
                "buffered_sum_instructions={} borrowed_sum_instructions={} \
                 buffered_sum_cycles={} borrowed_sum_cycles={}",
                retired_sums[0].0, retired_sums[1].0, retired_sums[0].1, retired_sums[1].1,
            );
            #[cfg(not(target_os = "macos"))]
            let retired_report = "retired_counters=unavailable";
            eprintln!(
                "LIGHT_VIEW_PERF boundary=initial_overworld_centre iterations={iterations} \
                 buffered_calls={} borrowed_calls={} \
                 buffered_sum_ms={:.3} borrowed_sum_ms={:.3} {retired_report}",
                calls[0], calls[1], sums[0].as_secs_f64() * 1000.0,
                sums[1].as_secs_f64() * 1000.0,
            );
        }
        drop(neighbours);
        columns[5].set_block_id(0, 1, 8, air);
        let neighbours = columns
            .iter()
            .enumerate()
            .filter(|(slot, _)| *slot != 4)
            .map(|(slot, column)| (slot as i32 % 3 - 1, slot as i32 / 3 - 1, column))
            .collect::<Vec<_>>();
        let unlit = compute_overworld_initial_lights_borrowed(
            InitialLightVolume::borrowed(&columns[4], &shape), &shape, &neighbours,
        );
        assert_eq!(unlit[4].block(section), &LightData::Missing);
        assert_ne!(actual[4], unlit[4], "seam emitter must affect the detector");
    }
}

pub(super) fn retained_light_is_initialized(status: RetainedLightStatus) -> bool {
    matches!(
        status,
        RetainedLightStatus::DependencyInitialized | RetainedLightStatus::CentreSettled
    )
}

/// Settles a newly initialized Nether dependency from the part of the current
/// admission footprint that is visible from that dependency's own centre.
///
/// The shared centre flood deliberately activates only its centre terrain: an
/// uninitialized dependency must not illuminate that packet. A dependency is a
/// different admission boundary, however. Its local 3×3 footprint activates
/// every supplied terrain source, including a source in a future neighbour
/// that is diagonal to the original centre. Retained layers are still used as
/// seeds only for columns that were already initialized; missing columns stay
/// opaque seams. The selected dependency receives storage masks for this
/// remapped footprint rather than the original centre's footprint.
pub(super) fn compute_nether_dependency_light(
    target: &WorldChunkColumn,
    target_dx: i32,
    target_dz: i32,
    center: &WorldChunkColumn,
    neighbours: &[(i32, i32, &ServerChunkColumn)],
    neighbour_columns: &[WorldChunkColumn],
    stored: &[Option<&ColumnLight>; 9],
    statuses: &[Option<RetainedLightStatus>; 9],
) -> ColumnLight {
    let mut neighbourhood = Neighbourhood::new(target);
    let mut storage_neighbours = Vec::new();

    let center_dx = -target_dx;
    let center_dz = -target_dz;
    if (-1..=1).contains(&center_dx)
        && (-1..=1).contains(&center_dz)
        && (center_dx, center_dz) != (0, 0)
    {
        neighbourhood = neighbourhood.with(center_dx, center_dz, center);
        storage_neighbours.push(center.clone());
    }
    for ((source_dx, source_dz, _), column) in neighbours.iter().zip(neighbour_columns) {
        let local_dx = *source_dx - target_dx;
        let local_dz = *source_dz - target_dz;
        if (-1..=1).contains(&local_dx)
            && (-1..=1).contains(&local_dz)
            && (local_dx, local_dz) != (0, 0)
        {
            neighbourhood = neighbourhood.with(local_dx, local_dz, column);
            storage_neighbours.push(column.clone());
        }
    }

    let section_min_y = target.min_y();
    let mut light = compute_column_light_with_neighbours_seeded(
        &neighbourhood,
        &V770LightProps { has_skylight: false },
        initial_full_sky_sections(Dimension::Nether),
        |local_dx, local_dz, x, y, z| {
            let source_dx = target_dx + local_dx;
            let source_dz = target_dz + local_dz;
            if !(-1..=1).contains(&source_dx) || !(-1..=1).contains(&source_dz) {
                return 0;
            }
            let slot = ((source_dz + 1) * 3 + (source_dx + 1)) as usize;
            if !statuses[slot].is_some_and(retained_light_is_initialized) {
                return 0;
            }
            let section = usize::try_from((y - section_min_y).div_euclid(16) + 1)
                .expect("Nether light section index");
            stored[slot]
                .and_then(|light| {
                    light
                        .block(section)
                        .get(NibbleArray::index(x, y.rem_euclid(16) as usize, z))
                })
                .unwrap_or(0)
        },
        |_local_dx, _local_dz| true,
    );
    light.set_storage(initial_nether_light_storage_for_column(
        target,
        target,
        &storage_neighbours,
    ));
    light
}

pub(super) fn empty_light_storage_like(light: &ColumnLight) -> ColumnLight {
    // `ColumnLight::new` takes the number of block sections and adds the two
    // boundary sections used by the wire light window.  Preserve the source
    // window here; passing its already-expanded length would add the boundary
    // sections a second time (18 -> 20 for a 16-section Nether column).
    let block_section_count = light
        .light_section_count()
        .checked_sub(2)
        .expect("a light window always includes its two boundary sections");
    let mut empty = ColumnLight::new(block_section_count);
    debug_assert_eq!(empty.light_section_count(), light.light_section_count());
    for section in 0..empty.light_section_count() {
        *empty.sky_mut(section) = LightData::Uniform(0);
        *empty.block_mut(section) = LightData::Uniform(0);
    }
    empty
}



/// The [`ChunkShape`] a served column is framed against, taken from **the column
/// itself** rather than from a hardcoded overworld constant.
///
/// # Why the column and not the connection
///
/// A chunk packet's section count is a property of the *dimension*, and the client
/// derives its own from the `dimension_type` holder id `login`/`respawn` carried
/// (`V770Adapter::enter_dimension`). Once the server can host more than one
/// dimension, a constant here is wrong for one of them: a Nether column is
/// `min_y 0, height 256` (16 sections) against the overworld's `-64, 384` (24), and
/// serving one through the other's shape mis-slices every section — a decode error
/// on the client, not a cosmetic one.
///
/// `ServerChunkColumn` already carries `min_y` and `height`, and `lodestone-server`
/// builds every column with the dimension's own window (see that crate's
/// `NetherChunkSource::WINDOW_HEIGHT`, which is the dimension type's 256 and
/// deliberately not the generator's 128). So reading them off the column keeps the
/// wire framing and the terrain that fills it derived from **one** number, rather
/// than from two that must be kept in agreement by hand.
///
/// Only the vertical window comes from the column: the palette framing and the
/// air/biome ids are properties of the protocol family, exactly as
/// `V770Adapter::enter_dimension` documents for the receiving side.
///
/// # This recognises the two real windows and defaults to the overworld's
///
/// It is deliberately **not** `section_count = height / 16` for an arbitrary column.
/// A chunk's section count is a property of the *dimension the client resolved*, and
/// nothing else: a client framed against the overworld reads exactly 24 sections
/// whatever the server's column happens to contain.
///
/// That distinction was measured. Several of this crate's own loopback fixtures serve
/// deliberately tiny columns — `combat_live`'s `AirSource` is `ChunkColumn::new(0,
/// 16)`, one section — because the test is about combat and no block is ever read.
/// Framing that column against its own height emitted a one-section packet to a
/// 24-section client, and six live tests failed with *"initial column never
/// arrived"*: the client joined, spawned, and silently could not decode a single
/// chunk. The short column was always fine against the overworld window (the missing
/// rows are simply empty sections), and it still is.
///
/// So the mapping is from a *known dimension window* to that dimension's shape, and
/// anything unrecognised keeps the overworld's — the exact behaviour every caller had
/// before the Nether existed.
pub(super) fn shape_for_column(column: &ServerChunkColumn) -> ChunkShape {
    let nether_or_end = ChunkShape::nether_or_end_1_21();
    if column.min_y == nether_or_end.min_y
        && column.height == nether_or_end.world_height as i32
    {
        return nether_or_end;
    }
    ChunkShape::overworld_1_21()
}

/// Selects the wire window from the dimension registry, not from the source
/// column's backing allocation. A persisted test world and a newly generated
/// world can expose the same storage height for more than one dimension; the
/// client-facing dimension is the authority for section framing.
pub(super) fn shape_for_dimension(dimension: Dimension) -> ChunkShape {
    match dimension {
        Dimension::Nether | Dimension::End => ChunkShape::nether_or_end_1_21(),
        Dimension::Overworld => ChunkShape::overworld_1_21(),
    }
}

pub(super) fn encode_chunk_in_dimension(
    wire: Wire,
    cx: i32,
    cz: i32,
    column: &ServerChunkColumn,
    dimension: Dimension,
) -> ServerDirective {
    let shape = shape_for_dimension(dimension);
    let world_column = build_world_column(&shape, column);
    let light = column
        .retained_light()
        .filter(|_| column.retained_light_status() == Some(RetainedLightStatus::CentreSettled))
        .filter(|light| light.light_section_count() == shape.section_count + 2)
        .cloned()
        .unwrap_or_else(|| compute_served_initial_light(&world_column, dimension));
    let payload = encode_column_body(wire, cx, cz, &shape, &world_column, &light, column);
    ServerDirective::Send {
        packet_id: play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
        payload,
    }
}

/// The single column-encode body in this crate.
///
/// It lives on the [`ChunkEncoder`] impl rather than on
/// [`ServerProtocol::encode_chunk`] because a `ChunkEncoder` is `'static` and
/// therefore movable into the `spawn_blocking` closure that generated the column
/// — which is where these 62 M instructions per column belong, and specifically
/// not on the connection task that owes the player a reply to their block break.
/// `ServerProtocol::encode_chunk` calls straight through for a one-column
/// caller, so those two forms cannot drift. The server uses its separate
/// neighbour-bearing encode hook when resident adjacent columns exist.
impl ChunkEncoder for V770ServerProtocol {
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerChunkColumn) -> ServerDirective {
        encode_chunk_in_dimension(self.wire, cx, cz, column, Dimension::Overworld)
    }

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ServerChunkColumn,
        dimension: Dimension,
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        Ok(encode_chunk_in_dimension(self.wire, cx, cz, column, dimension))
    }
}
