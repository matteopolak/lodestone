//! Chunk and dimension-aware light encoding.
//!
//! This private module is part of the V770ServerProtocol facade. Its
//! re-exported helpers preserve the existing public API and wire behaviour.

use super::*;

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
/// # This function does no string work at all, and that is recent
///
/// It used to read [`ServerChunkColumn::block_state`] — a `&str` — 98,304 times
/// per column, probe each through a per-column `HashMap<&str, u32>` (std's
/// SipHash), and resolve each *distinct* entry through what was then a
/// 32,366-row scan doing a string compare per row: order 10⁶ string comparisons
/// per served column, paid on every join and every view-tracker resend. It was
/// invisible to the 21-unit worldgen optimisation drive because the generation
/// cost metric excludes protocol encode by definition.
///
/// The resolution now happens **once per palette entry, on the server side**, at
/// column-adoption time (`ChunkColumn::palette_state_ids`), so the inner loop
/// here is a range check and two array indexes. `resolve_state_id` still exists
/// for the per-*edit* callers and is the same `lodestone_data` function the
/// palette resolves through, so the two cannot drift. `DESIGN.md` §12.131 has
/// the measurement.
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
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    let id = source.block_state_id(lx as i32, wy, lz as i32);
                    // `air_id` is already the container's own default (see
                    // `ChunkSection::new`), so a cell resolving to it needs
                    // no explicit write — same short-circuit the old
                    // `is_solid` check gave air cells, just derived from the
                    // real state now.
                    if id != shape.air_id {
                        section.set_block(lx, ly, lz, id);
                    }
                }
            }
        }
        for qy in 0..4usize {
            let column_qy = section_index * 4 + qy;
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let cell = source.biome_cell_index(qx, column_qy, qz) as usize;
                    section.set_biome(qx, qy, qz, biome_palette_ids[cell]);
                }
            }
        }
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }

    column
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
    cx: i32,
    cz: i32,
    shape: &ChunkShape,
    column: &WorldChunkColumn,
    light: &ColumnLight,
    source: &ServerChunkColumn,
) -> Vec<u8> {
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
        section_blob.i16(packet_non_empty_block_count(section) as i16);
        section_blob.i16(fluid_count(section) as i16);
        section.block_states().encode(&mut section_blob);
        section.biomes().encode(&mut section_blob);
    }
    let section_bytes = section_blob.into_vec();
    w.var_i32(section_bytes.len() as i32);
    w.bytes(&section_bytes);

    encode_block_entities(&mut w, source);

    debug_assert_eq!(
        light.light_section_count(),
        shape.section_count + 2,
        "light must span the shape's `section_count + 2` light sections"
    );
    light.encode(&mut w);

    w.into_vec()
}

/// The three client-visible heightmap type ids in the protocol-776 registry.
/// They are explicit ids, not declaration positions: the source registry gives
/// the world-generation-only forms ids 0, 2, and 3, leaving the client forms
/// at 1, 4, and 5.
const WORLD_SURFACE_HEIGHTMAP_TYPE_ID: u32 = 1;
const MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID: u32 = 5;

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
                    .find(|&y| client_heightmap_includes(type_id, source.resolved_block_state_id(x, y, z)))
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

/// Counts states with a non-empty fluid state in one section. The field counts
/// waterlogged blocks as well as liquid blocks, and is derived from the exact
/// container the encoder writes so it cannot disagree with the following state
/// bytes even when a short test column is padded to a dimension window.
pub(super) fn fluid_count(section: &ChunkSection) -> u16 {
    (0..section.block_states().entry_count())
        .filter(|&index| {
            lodestone_data::block_states::StateId::new(section.block_states().get(index))
                .is_some_and(lodestone_data::snow_support::has_fluid_state)
        })
        .count() as u16
}

/// Counts the states the protocol considers non-empty for its section header.
/// The world section deliberately has one version-neutral default-air count;
/// this wire field additionally excludes cave air and void air, whose payload
/// states must still be retained in an otherwise air-only section.
pub(super) fn packet_non_empty_block_count(section: &ChunkSection) -> u16 {
    (0..section.block_states().entry_count())
        .filter(|&index| is_non_air_state_id(section.block_states().get(index)))
        .count() as u16
}

pub(super) fn is_non_air_state_id(id: u32) -> bool {
    lodestone_data::block_states::StateId::new(id).is_some_and(|state| {
        !matches!(
            state.block(),
            Block::Air | Block::CaveAir | Block::VoidAir
        )
    })
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
pub(super) fn encode_block_entities(w: &mut Writer, source: &ServerChunkColumn) {
    let mut entries: Vec<(
        lodestone_model::BlockPos,
        lodestone_data::block_entity_types::BlockEntityType,
        Vec<u8>,
    )> = source
        .block_entities()
        .iter()
        .filter_map(|(pos, entity)| {
            let type_id =
                lodestone_data::block_entity_types::block_entity_type_id(entity.type_id())?;
            let mut nbt = lodestone_server::chunk_nbt::block_entity_update_nbt(*pos, entity);
            // The external packet control stabilizes compounds recursively by
            // unsigned UTF-8 key order before writing them. This is the same
            // canonicalization used for the rest of the raw-packet fixture;
            // retaining the producer's field order would leave the payload
            // semantically equal but byte-different.
            stabilize_block_entity_nbt(&mut nbt);
            let mut body = Writer::default();
            write_network_nbt(&mut body, &nbt).ok()?;
            Some((*pos, type_id, body.into_vec()))
        })
        .collect();

    // The external packet control orders the materialized chunk records by
    // packed local XZ, then absolute Y and registry id. Canonicalizing here
    // avoids depending on the generator's sidecar insertion order; it is not
    // a payload or coordinate-specific special case.
    entries.sort_unstable_by_key(|(pos, type_id, _)| {
        (
            ((pos.x & 15) << 4) | (pos.z & 15),
            pos.y,
            type_id.raw(),
        )
    });

    w.var_i32(entries.len() as i32);
    for (pos, type_id, nbt) in entries {
        w.u8((((pos.x & 15) << 4) | (pos.z & 15)) as u8);
        w.i16(pos.y as i16);
        w.var_i32(type_id.raw() as i32);
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
    has_skylight: bool,
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
const fn initial_full_sky_sections(dimension: Dimension) -> usize {
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
    neighbours: &[(i32, i32, ServerChunkColumn)],
    dimension: Dimension,
) -> ColumnLight {
    let shape = shape_for_dimension(dimension);
    let center = build_world_column(&shape, column);
    let neighbour_columns = neighbours
        .iter()
        .map(|(_, _, neighbour)| build_world_column(&shape, neighbour))
        .collect::<Vec<_>>();
    let mut neighbourhood = Neighbourhood::new(&center);
    for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_columns) {
        if (*dx, *dz) == (0, 0) {
            // Persistent-save callers carry a full 3×3 snapshot including the
            // centre; `Neighbourhood` already owns that one separately.
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

pub(super) fn compute_served_initial_light_with_neighbours(
    center: &WorldChunkColumn,
    shape: &ChunkShape,
    neighbours: &[(i32, i32, ServerChunkColumn)],
    dimension: Dimension,
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
        let mut cardinal_neighbourhood = Neighbourhood::new(center);
        for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_columns) {
            if dx.abs() + dz.abs() == 1 {
                cardinal_neighbourhood = cardinal_neighbourhood.with(*dx, *dz, neighbour);
            }
        }
        let cardinal_light = compute_column_light_with_neighbours_for_initial_chunk(
            &cardinal_neighbourhood,
            &V770LightProps { has_skylight: false },
            initial_full_sky_sections(dimension),
        );
        for section in 0..light.light_section_count() {
            *light.block_mut(section) = cardinal_light.block(section).clone();
        }
    }
    let block_light_storage = (dimension == Dimension::Nether || dimension == Dimension::End)
        .then(|| initial_block_light_storage_sections(center, &neighbour_columns));
    normalize_initial_chunk_light(&mut light, dimension, block_light_storage.as_deref());
    light
}

pub(super) fn compute_served_initial_lights_with_neighbours_and_storage(
    center: &WorldChunkColumn,
    shape: &ChunkShape,
    neighbours: &[(i32, i32, ServerChunkColumn)],
    stored: &[Option<&ColumnLight>; 9],
    statuses: &[Option<RetainedLightStatus>; 9],
    dimension: Dimension,
) -> [ColumnLight; 9] {
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
    // End sky light is a terrain-derived field, not a retained source. A
    // dependency snapshot belongs to the admission that made that column a
    // centre; seeding the new centre's flood from it lets an old full-sky
    // layer bypass the current terrain's attenuation. Recompute the shared
    // 3x3 End field from current blocks, then restore retained dependencies
    // below. Nether keeps its lifecycle-aware retained block-light path.
    let fresh_end_storage = [None; 9];
    let storage = if dimension == Dimension::End {
        &fresh_end_storage
    } else {
        stored
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
    neighbours: &[(i32, i32, ServerChunkColumn)],
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
    let payload = encode_column_body(cx, cz, &shape, &world_column, &light, column);
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
        encode_chunk_in_dimension(cx, cz, column, Dimension::Overworld)
    }

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ServerChunkColumn,
        dimension: Dimension,
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        Ok(encode_chunk_in_dimension(cx, cz, column, dimension))
    }
}
