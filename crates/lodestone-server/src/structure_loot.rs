//! Structure chests: the loot the template engine places but cannot fill
//! (issue #337).
//!
//! # What it is
//!
//! Shipwrecks, ocean ruins and igloos generate for real (`lodestone-worldgen`'s
//! `structure` S2 unit), and every one of them arrived with an *empty* chest —
//! or, for an ocean ruin, no chest at all. This module is vanilla's
//! `TemplateStructurePiece.postProcess` data-marker pass plus the three
//! `handleDataMarker` overrides it dispatches to, run on the server side of the
//! seam: it finds each piece's `structure_block` DATA markers, resolves the loot
//! table the marker names, rolls it, and attaches a filled
//! [`BlockEntity::Container`] to the column.
//!
//! # How it works
//!
//! ```text
//! column(cx, cz)
//!   -> structure_references(cx, cz)      which structures reach this chunk
//!   -> structure_starts(origin)          their pieces, positions and rotations
//!   -> raw template bytes                markers, which the parsed template drops
//!   -> transform(marker, rotation)       world position
//!   -> loot table id                     per (structure, marker metadata)
//!   -> roll + shuffle into 27 slots      the chest's contents
//! ```
//!
//! Two things are worth knowing about *why* the markers are re-read from the raw
//! `.nbt` bytes rather than taken off the already-parsed
//! `lodestone_worldgen::structure::template::StructureTemplate`:
//!
//! * That parser deliberately drops each block's `nbt` compound, and the
//!   `BlockIgnoreProcessor` drops the marker block itself — exactly as vanilla
//!   does, since the marker is not meant to be a block in the finished world.
//!   The *metadata* string only exists in the raw file.
//! * The bytes are already embedded in this crate (`assets/structure/`, via
//!   `build.rs`), so re-reading them costs a gunzip per piece and no I/O.
//!
//! **The roll is seeded from the chest position and from nothing else.**
//! [`crate::chunk::OverworldChunkSource`] regenerates an unedited column on every
//! request, so a chest whose contents depended on a per-connection RNG would hold
//! different loot each time the column was streamed. This is the one place in the
//! crate where determinism-by-position is a correctness requirement rather than a
//! nicety. The world seed is not mixed in and does not need to be: it already
//! decides *where* the structure lands, so two seeds never ask for a roll at the
//! same coordinates.
//!
//! # How to change it
//!
//! To support another structure's chests, add its `(structure id, marker)` pair
//! to [`marker_loot_table`] and, if the chest is *created* by the marker rather
//! than already present in the template, add it to [`marker_places_chest`]. Both
//! come straight from that structure's `handleDataMarker` in
//! `.cache/mc/26.2/src/net/minecraft/world/level/levelgen/structure/structures/`.
//!
//! ## Gotchas
//!
//! * **Shipwreck and igloo markers sit one block *above* the chest**
//!   (`position.below()` in both `handleDataMarker`s); an ocean ruin's marker is
//!   the chest position itself, and the chest does not exist until we place it.
//!   Getting this off by one puts loot in a block of air above the chest, where
//!   nothing can reach it and no test that only counts rolls would notice.
//! * A marker whose loot table is not bundled yields an **empty** chest, not a
//!   missing one — the same "no such table" tolerance
//!   [`crate::block_drops::drop_block_loot`] has.
//!
//! # Dependencies
//!
//! [`crate::loot`] for the tables and the roll, `lodestone-worldgen`'s
//! `structure` module for the piece list and the rotation transform, `flate2`
//! for the gzip wrapper Mojang ships templates with.

use std::io::Read as _;

use lodestone_core::{Nbt, Reader};
use lodestone_model::{BlockPos, ItemStack, ResourceKey};
use lodestone_worldgen::structure::StructureStart;
use lodestone_worldgen::structure::template::transform;

use crate::block_entities::{BlockEntity, CONTAINER_9X3_SIZE};
use crate::loot::{LootContext, LootTableSet};
use crate::mob_spawn::SpawnRng;

/// One data marker read out of a template: its template-relative position and
/// its `metadata` string.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DataMarker {
    pos: [i32; 3],
    metadata: String,
}

/// A chest this pass wants placed: where, what it holds, and whether the block
/// itself has to be written (an ocean ruin's marker *creates* the chest).
#[derive(Debug, Clone, PartialEq)]
pub struct StructureChest {
    /// World position of the chest block.
    pub pos: BlockPos,
    /// The rolled contents, already distributed across the 27 slots.
    pub entity: BlockEntity,
    /// The block state to write, or `None` when the template already placed a
    /// chest here (shipwreck, igloo).
    pub block: Option<&'static str>,
}

/// The loot table a `(structure, marker metadata)` pair names, from that
/// structure's own `handleDataMarker`.
///
/// `big` selects an ocean ruin's two-table split
/// (`isLarge ? UNDERWATER_RUIN_BIG : UNDERWATER_RUIN_SMALL`).
#[must_use]
fn marker_loot_table(structure: &str, marker: &str, big: bool) -> Option<&'static str> {
    match (structure, marker) {
        // `ShipwreckPieces.MARKERS_TO_LOOT`.
        (_, "map_chest") => Some("minecraft:chests/shipwreck_map"),
        (_, "treasure_chest") => Some("minecraft:chests/shipwreck_treasure"),
        (_, "supply_chest") => Some("minecraft:chests/shipwreck_supply"),
        ("minecraft:igloo", "chest") => Some("minecraft:chests/igloo_chest"),
        ("minecraft:ocean_ruin_cold" | "minecraft:ocean_ruin_warm", "chest") => Some(if big {
            "minecraft:chests/underwater_ruin_big"
        } else {
            "minecraft:chests/underwater_ruin_small"
        }),
        _ => None,
    }
}

/// Whether this marker has to write the chest block itself, and at which offset
/// from the marker.
///
/// `OceanRuinPieces.handleDataMarker` calls `level.setBlock(position, CHEST)` —
/// the chest is not in the template. `ShipwreckPieces`/`IglooPieces` instead
/// decorate a chest the template already placed one block **below** the marker.
#[must_use]
fn marker_places_chest(structure: &str) -> (i32, Option<&'static str>) {
    match structure {
        "minecraft:ocean_ruin_cold" | "minecraft:ocean_ruin_warm" => {
            (0, Some("minecraft:chest[facing=north,type=single,waterlogged=true]"))
        }
        _ => (-1, None),
    }
}

/// Every chest the pieces reaching chunk `(cx, cz)` want filled.
///
/// `starts` is the set of structure starts whose boxes reach this chunk (the
/// caller resolves them from `structure_references`); a chest outside the chunk
/// is filtered out here, so a piece straddling a border contributes its chests to
/// whichever chunk each one actually lands in — the same clipping-is-the-grid
/// property the template placer relies on.
#[must_use]
pub fn chests_for_chunk(
    starts: &[std::sync::Arc<StructureStart>],
    cx: i32,
    cz: i32,
    tables: &LootTableSet,
) -> Vec<StructureChest> {
    let mut out = Vec::new();
    for start in starts {
        for piece in &start.pieces {
            let Some(placement) = piece.placement.as_ref() else {
                continue;
            };
            let Some(template_id) = piece.template.as_deref() else {
                continue;
            };
            let Some(bytes) = crate::worldgen_data::embedded_structure_template(template_id) else {
                continue;
            };
            let big = template_id.contains("/big_");
            for marker in data_markers(bytes) {
                let (offset, block) = marker_places_chest(&start.structure);
                let Some(table) = marker_loot_table(&start.structure, &marker.metadata, big) else {
                    continue;
                };
                let rel = transform(
                    marker.pos,
                    placement.settings.mirror,
                    placement.settings.rotation,
                    placement.settings.pivot,
                );
                let pos = BlockPos::new(
                    rel[0] + placement.position[0],
                    rel[1] + placement.position[1] + offset,
                    rel[2] + placement.position[2],
                );
                if pos.x.div_euclid(16) != cx || pos.z.div_euclid(16) != cz {
                    continue;
                }
                let Ok(table) = table.parse::<ResourceKey>() else {
                    continue;
                };
                let mut rng = SpawnRng::new(chest_seed(pos));
                let items = tables.roll(&table, &LootContext::default(), &mut rng);
                out.push(StructureChest {
                    pos,
                    entity: fill_container(items, &mut rng),
                    block,
                });
            }
        }
    }
    out
}

/// The per-chest roll seed: the chest's own coordinates, so a regenerated column
/// produces the same chest twice. See the module doc for why this must not come
/// from a connection's RNG, and why the world seed is not part of it.
fn chest_seed(pos: BlockPos) -> u64 {
    let mut hash = 0x9E37_79B9_7F4A_7C15u64;
    for value in [pos.x, pos.y, pos.z] {
        hash = hash
            .rotate_left(17)
            .wrapping_mul(0x1000_0000_1B3)
            .wrapping_add(value as i64 as u64);
    }
    hash
}

/// Distributes `items` across a fresh `generic_9x3` container the way vanilla's
/// `LootTable.fill` does: empty stacks dropped, and the rest scattered over
/// random free slots rather than packed from slot `0`
/// (`LootTable.shuffleAndSplitItems`).
fn fill_container(items: Vec<ItemStack>, rng: &mut SpawnRng) -> BlockEntity {
    let mut slots: Vec<Option<ItemStack>> = vec![None; CONTAINER_9X3_SIZE];
    let mut free: Vec<usize> = (0..CONTAINER_9X3_SIZE).collect();
    // Fisher-Yates over the free-slot list, vanilla's `Util.shuffle`.
    for i in (1..free.len()).rev() {
        let j = rng.next_int(i as i32 + 1) as usize;
        free.swap(i, j);
    }
    for item in items {
        if item.count == 0 {
            continue;
        }
        let Some(slot) = free.pop() else { break };
        slots[slot] = Some(item);
    }
    BlockEntity::Container {
        id: "minecraft:chest".to_owned(),
        slots,
    }
}

/// Reads every `structure_block` DATA marker out of a raw template file.
///
/// This is `StructureTemplate.filterBlocks(…, Blocks.STRUCTURE_BLOCK)` plus the
/// `mode == StructureMode.DATA` test from
/// `TemplateStructurePiece.postProcess:95-100`. Palette membership is checked by
/// *name*, so a marker in any of a multi-palette template's palettes is found
/// (every shipwreck ships 8 palettes and the marker is in all of them).
fn data_markers(bytes: &[u8]) -> Vec<DataMarker> {
    let Some(root) = decode_template(bytes) else {
        return Vec::new();
    };
    let root = match &root {
        Nbt::Compound(fields) => fields,
        _ => return Vec::new(),
    };

    // Which palette indices are `minecraft:structure_block`. A template's
    // palettes are parallel, so a union over all of them is right.
    let mut marker_states: Vec<u16> = Vec::new();
    let mut note_palette = |palette: &Nbt| {
        if let Nbt::List { elements, .. } = palette {
            for (index, entry) in elements.iter().enumerate() {
                if let Some(Nbt::String(name)) = compound_field(entry, "Name") {
                    if name == "minecraft:structure_block" {
                        if let Ok(index) = u16::try_from(index) {
                            marker_states.push(index);
                        }
                    }
                }
            }
        }
    };
    match field(root, "palettes") {
        Some(Nbt::List { elements, .. }) => {
            for palette in elements {
                note_palette(palette);
            }
        }
        _ => {
            if let Some(palette) = field(root, "palette") {
                note_palette(palette);
            }
        }
    }
    if marker_states.is_empty() {
        return Vec::new();
    }

    let mut markers = Vec::new();
    if let Some(Nbt::List { elements, .. }) = field(root, "blocks") {
        for entry in elements {
            let state = match compound_field(entry, "state") {
                Some(Nbt::Int(i)) => u16::try_from(*i).unwrap_or(u16::MAX),
                _ => continue,
            };
            if !marker_states.contains(&state) {
                continue;
            }
            let Some(pos) = compound_field(entry, "pos").and_then(int_triple) else {
                continue;
            };
            let Some(nbt) = compound_field(entry, "nbt") else {
                continue;
            };
            // `mode` is a plain string in the file (`"DATA"`), not the ordinal
            // its `LEGACY_CODEC` also accepts.
            if compound_field(nbt, "mode") != Some(&Nbt::String("DATA".to_owned())) {
                continue;
            }
            let metadata = match compound_field(nbt, "metadata") {
                Some(Nbt::String(s)) => s.clone(),
                _ => continue,
            };
            markers.push(DataMarker { pos, metadata });
        }
    }
    markers
}

fn decode_template(bytes: &[u8]) -> Option<Nbt> {
    let decoded = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out).ok()?;
        out
    } else {
        bytes.to_vec()
    };
    let mut reader = Reader::new(&decoded);
    lodestone_core::read_named_nbt(&mut reader).ok().map(|(_, root)| root)
}

fn field<'a>(fields: &'a [(String, Nbt)], name: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value)
}

fn compound_field<'a>(value: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    match value {
        Nbt::Compound(fields) => field(fields, name),
        _ => None,
    }
}

fn int_triple(value: &Nbt) -> Option<[i32; 3]> {
    match value {
        Nbt::List { elements, .. } if elements.len() >= 3 => {
            let mut out = [0i32; 3];
            for (slot, element) in out.iter_mut().zip(elements) {
                match element {
                    Nbt::Int(i) => *slot = *i,
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The markers really are in vanilla's own shipwreck template, with the
    /// three metadata strings `ShipwreckPieces.MARKERS_TO_LOOT` keys on.
    ///
    /// The expected values come from the file and from the decompiled source,
    /// not from this module: `with_mast.nbt` is Mojang's own, and the three
    /// marker names are the literal keys at `ShipwreckPieces.java:69-71`.
    #[test]
    fn a_shipwreck_template_carries_its_three_chest_markers() {
        let bytes = crate::worldgen_data::embedded_structure_template("minecraft:shipwreck/with_mast")
            .expect("with_mast is bundled");
        let markers = data_markers(bytes);
        let mut names: Vec<&str> = markers.iter().map(|m| m.metadata.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["map_chest", "supply_chest", "treasure_chest"]);
    }

    /// An igloo's chest lives in `bottom` alone, and a template with no marker
    /// at all yields none — the control that [`data_markers`] is reading the
    /// file rather than returning a plausible constant.
    #[test]
    fn igloo_markers_are_only_in_the_basement() {
        let bottom = crate::worldgen_data::embedded_structure_template("minecraft:igloo/bottom")
            .expect("igloo/bottom is bundled");
        assert_eq!(
            data_markers(bottom)
                .iter()
                .map(|m| m.metadata.clone())
                .collect::<Vec<_>>(),
            vec!["chest".to_string()]
        );
        let top = crate::worldgen_data::embedded_structure_template("minecraft:igloo/top")
            .expect("igloo/top is bundled");
        assert!(data_markers(top).is_empty());
    }

    /// The six structure-chest tables are bundled and roll real items.
    ///
    /// The expected values come from vanilla's own `igloo_chest.json`, not from
    /// our roller: two pools, `rolls: uniform 2..8` and `rolls: 1`, no `empty`
    /// entry anywhere, and the second pool's single entry is `golden_apple`. So a
    /// roll yields 3..=9 stacks and **always** contains exactly one golden apple —
    /// the part a wrong pool loop would get wrong while the count still looked
    /// plausible.
    #[test]
    fn the_structure_chest_tables_are_bundled_and_roll() {
        let tables = crate::block_drops::bundled_tables();
        for id in [
            "minecraft:chests/shipwreck_map",
            "minecraft:chests/shipwreck_supply",
            "minecraft:chests/shipwreck_treasure",
            "minecraft:chests/igloo_chest",
            "minecraft:chests/underwater_ruin_small",
            "minecraft:chests/underwater_ruin_big",
        ] {
            let key: ResourceKey = id.parse().expect("literal key");
            assert!(tables.get(&key).is_some(), "{id} is not bundled");
        }

        let igloo: ResourceKey = "minecraft:chests/igloo_chest".parse().unwrap();
        let mut rng = SpawnRng::new(0xF00D);
        for _ in 0..64 {
            let rolled = tables.roll(&igloo, &LootContext::default(), &mut rng);
            assert!(
                (3..=9).contains(&rolled.len()),
                "igloo_chest.json rolls uniform 2..8 plus 1, so 3..=9 stacks, got {rolled:?}"
            );
            assert_eq!(
                rolled
                    .iter()
                    .filter(|s| s.item.to_string() == "minecraft:golden_apple")
                    .count(),
                1,
                "the second pool is one guaranteed golden apple, got {rolled:?}"
            );
        }
    }

    /// End to end through the real chunk source: a shipwreck that vanilla itself
    /// placed at this seed arrives with a chest **block** and a **filled** chest
    /// block entity in the same column.
    ///
    /// The coordinates are an outside expectation:
    /// `crates/lodestone-worldgen/tests/support/structure_starts_survival.txt`
    /// lists `minecraft:shipwreck -21 -6` read out of the vanilla-authored survival
    /// oracle world at seed `-195764831`. Both halves are asserted because they
    /// fail independently — a marker offset that is off by one still produces a
    /// filled entity, floating in the air above the chest.
    #[test]
    fn a_generated_shipwreck_arrives_with_a_filled_chest() {
        use crate::chunk::ChunkSource as _;

        let source = crate::worldgen_data::overworld_chunk_source(-195764831);
        let mut chests = 0;
        for dx in -2..=2 {
            for dz in -2..=2 {
                let column = source.column(-21 + dx, -6 + dz);
                for (pos, entity) in column.block_entities() {
                    if entity.type_id() != "minecraft:chest" {
                        continue;
                    }
                    chests += 1;
                    assert!(
                        source
                            .block_state(pos.x, pos.y, pos.z)
                            .starts_with("minecraft:chest["),
                        "loot was attached to {pos:?}, which is not a chest block"
                    );
                    assert!(
                        entity.container_slots().iter().any(Option::is_some),
                        "the chest at {pos:?} rolled nothing"
                    );
                }
            }
        }
        assert!(chests > 0, "no shipwreck chest found around chunk (-21, -6)");
    }

    /// A filled container holds exactly the rolled stacks, and scatters them —
    /// `fill` is not "pack from slot 0". The scatter is what makes a generated
    /// chest look generated.
    #[test]
    fn fill_scatters_the_rolled_stacks_over_the_27_slots() {
        let items: Vec<ItemStack> = (1..=3)
            .map(|n| ItemStack::new("minecraft:coal".parse().unwrap(), n))
            .collect();
        let mut rng = SpawnRng::new(7);
        let BlockEntity::Container { slots, .. } = fill_container(items, &mut rng) else {
            panic!("fill_container builds a Container");
        };
        assert_eq!(slots.len(), CONTAINER_9X3_SIZE);
        let occupied: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|_| i))
            .collect();
        assert_eq!(occupied.len(), 3);
        assert_ne!(occupied, vec![0, 1, 2], "stacks must be scattered, not packed");
    }
}
