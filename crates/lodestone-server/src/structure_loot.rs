//! Structure chests: the loot the template engine places but cannot fill.
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
//! # Three passes, not one, because vanilla has three mechanisms
//!
//! The pipeline above is the **marker** pass, and on its own it left every village,
//! bastion, trial chamber, ancient city, ruined portal and pillager outpost
//! generating with empty chests while shipwrecks worked. The reason is that a data
//! marker is only one of three ways vanilla attaches a loot table:
//!
//! | pass | mechanism | reads |
//! |---|---|---|
//! | marker | a `structure_block` DATA marker the piece's `handleDataMarker` interprets | [`data_markers`] |
//! | self-named | the container block's own `nbt.LootTable` field | [`template_loot`] |
//! | coded | a piece with no template calling `createChest` directly | `piece.loot` |
//!
//! Measured over the 1,212 bundled templates, **132** use the self-named form
//! (village 62, bastion 26, trial_chambers 19, ruined_portal 13, ancient_city 10,
//! pillager_outpost 2) and those templates generally carry no marker at all — while
//! `shipwreck/with_mast` is the mirror image. **Neither pass subsumes the other**,
//! and a coverage figure from one says nothing about the other. That is the shape of
//! mistake worth remembering here: the marker pass was complete, correct and
//! verified against its own structures, and 132 templates were invisible to it.
//!
//! # How to change it
//!
//! To support another structure's **marker** chests, add its `(structure id,
//! marker)` pair to [`marker_loot_table`] and, if the chest is *created* by the
//! marker rather than already present in the template, add it to
//! [`marker_places_chest`]. Both come straight from that structure's
//! `handleDataMarker` in vanilla's own per-structure processor classes.
//!
//! The other two passes need **no per-structure table at all** — they read the table
//! id out of the data — so a new structure using either form works the day its
//! templates are bundled. What can still be missing is the loot table itself: a
//! table this crate does not bundle yields an empty container rather than a missing
//! one, so check `assets/loot_table/chests/` before concluding a structure's chests
//! are unwired.
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
            // Pass 2 first, because it needs neither a placement nor a template: a
            // **coded** piece (a desert pyramid, a jungle temple, a stronghold room)
            // has no template at all, so both passes below structurally cannot see
            // its containers. Vanilla's coded `postProcess` calls
            // `createChest`/`setLootTable` directly, and `lodestone-worldgen`
            // already records each of those as a `CodedLoot` with an **absolute**
            // world position and vanilla's own `random.nextLong()` seed — so there
            // is no transform to apply here, which is exactly why this arm looks
            // shorter than the template ones rather than less complete.
            for coded in &piece.loot {
                let pos = BlockPos::new(coded.pos[0], coded.pos[1], coded.pos[2]);
                if pos.x.div_euclid(16) != cx || pos.z.div_euclid(16) != cz {
                    continue;
                }
                let Ok(table) = coded.table.parse::<ResourceKey>() else {
                    continue;
                };
                // Vanilla's seed, not the position hash: a coded piece drew it from
                // the structure's own stream, so using it keeps the roll on the
                // generator's specification rather than substituting ours. Still
                // deterministic per column, which is the property the module doc
                // requires.
                let mut rng = SpawnRng::new(coded.seed as u64);
                let items = tables.roll(&table, &LootContext::default(), &mut rng);
                out.push(StructureChest {
                    pos,
                    entity: fill_container(items, &mut rng),
                    // The coded piece already wrote the container block itself,
                    // through the same `CodedBlock` list that builds the rest of it.
                    block: None,
                });
            }
            let Some(placement) = piece.placement.as_ref() else {
                continue;
            };
            let Some(template_id) = piece.template.as_deref() else {
                continue;
            };
            let Some(bytes) = crate::worldgen_data::embedded_structure_template(template_id) else {
                continue;
            };
            // Pass 1: containers that name their own `LootTable`. Transformed by the
            // *same* `placement.settings` the marker pass below uses — reusing that
            // transform rather than writing a second one is what stops a rotated
            // village chest landing in the wrong cell.
            for loot in template_loot(bytes) {
                let rel = transform(
                    loot.pos,
                    placement.settings.mirror,
                    placement.settings.rotation,
                    placement.settings.pivot,
                );
                let pos = BlockPos::new(
                    rel[0] + placement.position[0],
                    rel[1] + placement.position[1],
                    rel[2] + placement.position[2],
                );
                if pos.x.div_euclid(16) != cx || pos.z.div_euclid(16) != cz {
                    continue;
                }
                let Ok(table) = loot.table.parse::<ResourceKey>() else {
                    continue;
                };
                let mut rng = SpawnRng::new(
                    loot.seed.map_or_else(|| chest_seed(pos), |s| s as u64),
                );
                let items = tables.roll(&table, &LootContext::default(), &mut rng);
                out.push(StructureChest {
                    pos,
                    entity: fill_container(items, &mut rng),
                    // The template placed the container; only its contents are
                    // missing. Writing a block here would replace a barrel or a
                    // decorated pot with a chest.
                    block: None,
                });
            }
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

/// One container a template placed with its own `LootTable` already set: its
/// template-relative position, the table id, and vanilla's optional
/// `LootTableSeed`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplateLoot {
    pos: [i32; 3],
    table: String,
    seed: Option<i64>,
}

/// Every block in a raw template whose own `nbt` compound carries a `LootTable`
/// string.
///
/// # Why this is a second pass and not a case of the marker pass
///
/// These are a **different mechanism**, and conflating them is why every village,
/// bastion, trial chamber, ancient city, ruined portal and pillager outpost
/// generated with empty chests while shipwrecks worked. A `structure_block` DATA
/// marker is an instruction to the piece's `handleDataMarker`, which looks the
/// table up from the marker's *metadata string* and a per-structure table
/// ([`marker_loot_table`]). A `LootTable` field is the container **naming its own
/// table**, resolved by `StructureTemplate.placeInWorld` writing the block entity
/// straight through — no marker, no `handleDataMarker`, nothing for the marker pass
/// to find.
///
/// The two are near-disjoint in the corpus: measured over the 1,212 bundled
/// templates, **132** carry a `LootTable` field, and the templates that do
/// generally carry no marker at all — while `shipwreck/with_mast` is the mirror
/// image, carrying markers and no `LootTable`. So neither pass subsumes the other,
/// and a coverage number from one says nothing about the other.
///
/// Blocks are matched by the presence of the field rather than by block name, which
/// is what makes this cover barrels, dispensers and decorated pots as well as
/// chests — vanilla's own condition is the field, not the block.
fn template_loot(bytes: &[u8]) -> Vec<TemplateLoot> {
    let Some(root) = decode_template(bytes) else {
        return Vec::new();
    };
    let Nbt::Compound(root) = &root else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(Nbt::List { elements, .. }) = field(root, "blocks") {
        for entry in elements {
            let Some(nbt) = compound_field(entry, "nbt") else {
                continue;
            };
            let Some(Nbt::String(table)) = compound_field(nbt, "LootTable") else {
                continue;
            };
            let Some(pos) = compound_field(entry, "pos").and_then(int_triple) else {
                continue;
            };
            // `LootTableSeed` is a `Long` in vanilla's `RandomizableContainer`
            // codec and is **optional**: absent, or present and zero, both mean
            // "roll from a fresh seed", which for us means the position-derived one.
            // Treating a zero as a real seed would give every unseeded container in
            // the world identical contents.
            let seed = match compound_field(nbt, "LootTableSeed") {
                Some(Nbt::Long(s)) if *s != 0 => Some(*s),
                _ => None,
            };
            out.push(TemplateLoot {
                pos,
                table: table.clone(),
                seed,
            });
        }
    }
    out
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
    /// marker names are the literal keys in `ShipwreckPieces.MARKERS_TO_LOOT`.
    #[test]
    fn a_shipwreck_template_carries_its_three_chest_markers() {
        let bytes = crate::worldgen_data::embedded_structure_template("minecraft:shipwreck/with_mast")
            .expect("with_mast is bundled");
        let markers = data_markers(bytes);
        let mut names: Vec<&str> = markers.iter().map(|m| m.metadata.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["map_chest", "supply_chest", "treasure_chest"]);
    }

    /// **The self-named pass, against Mojang's own bytes.** A village template that
    /// carries a `LootTable` field must be read, and the pair of assertions here is
    /// what proves the two passes are genuinely different mechanisms rather than one
    /// wearing two names.
    ///
    /// The expected values come from the file: a plains village house names
    /// `minecraft:chests/village/village_plains_house`, which is a table this crate
    /// bundles. The control is `data_markers` over the **same bytes** returning
    /// nothing — that emptiness is precisely why the marker pass left every village
    /// chest empty, and reading it as "this template has no loot" is the mistake.
    #[test]
    fn a_village_template_carries_self_named_loot_the_marker_pass_cannot_see() {
        // Any bundled village template with a chest. Resolved by search rather than
        // hardcoded, so this does not become a test about one file name.
        let candidates = [
            "minecraft:village/plains/houses/plains_small_house_1",
            "minecraft:village/plains/houses/plains_big_house_1",
            "minecraft:village/plains/houses/plains_butcher_shop_1",
            "minecraft:village/plains/houses/plains_fletcher_house_1",
            "minecraft:village/plains/houses/plains_tool_smith_1",
            "minecraft:village/plains/houses/plains_armorer_house_1",
        ];
        let mut found: Option<(&str, Vec<TemplateLoot>)> = None;
        for id in candidates {
            let Some(bytes) = crate::worldgen_data::embedded_structure_template(id) else {
                continue;
            };
            let loot = template_loot(bytes);
            if !loot.is_empty() {
                found = Some((id, loot));
                break;
            }
        }
        let (id, loot) = found.expect(
            "at least one bundled plains village template must carry a LootTable field — if this \
             panics, the 132-template measurement this pass was built on no longer holds and the \
             candidate list above is the first thing to check",
        );

        // Every entry names a real, `minecraft:`-namespaced table.
        for entry in &loot {
            assert!(
                entry.table.starts_with("minecraft:chests/"),
                "{id} names {}, which is not a chest table",
                entry.table
            );
            assert!(
                entry.table.parse::<ResourceKey>().is_ok(),
                "{id} names an unparseable table {}",
                entry.table
            );
        }

        // The control, and the whole point: the marker pass sees nothing here.
        let bytes = crate::worldgen_data::embedded_structure_template(id).expect("bundled");
        assert!(
            data_markers(bytes).is_empty(),
            "{id} carries a data marker too, so it is the wrong template to prove the two \
             passes are disjoint — pick one that does not"
        );
        // And the mirror image, so the control above is not measuring a broken
        // `data_markers`: the shipwreck has markers and no self-named loot.
        let mast = crate::worldgen_data::embedded_structure_template("minecraft:shipwreck/with_mast")
            .expect("with_mast is bundled");
        assert!(
            !data_markers(mast).is_empty(),
            "control: data_markers must still find the shipwreck's markers"
        );
        assert!(
            template_loot(mast).is_empty(),
            "control: the shipwreck is the mirror image and carries no LootTable field"
        );
    }

    /// Every self-named table across the whole bundled template corpus either
    /// resolves to a bundled loot table or is named here as blocked.
    ///
    /// This is the honest-coverage gate the addendum asked for: it reports, by
    /// measurement rather than by claim, which of the self-named containers actually
    /// roll. A table this crate does not bundle yields an *empty* container, which is
    /// indistinguishable from an unwired one from inside the game — so the split has
    /// to be asserted somewhere or it silently rots.
    #[test]
    fn self_named_loot_tables_are_either_bundled_or_named_as_blocked() {
        use std::collections::{BTreeMap, BTreeSet};

        let set = crate::loot::LootTableSet::load_bundled();
        let mut bundled: BTreeSet<String> = BTreeSet::new();
        let mut missing: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0usize;
        for id in crate::worldgen_data::embedded_structure_template_ids() {
            let Some(bytes) = crate::worldgen_data::embedded_structure_template(id) else {
                continue;
            };
            for entry in template_loot(bytes) {
                total += 1;
                let Ok(key) = entry.table.parse::<ResourceKey>() else {
                    *missing.entry(entry.table.clone()).or_default() += 1;
                    continue;
                };
                if set.get(&key).is_some() {
                    bundled.insert(entry.table);
                } else {
                    *missing.entry(entry.table).or_default() += 1;
                }
            }
        }

        assert!(
            total > 100,
            "the corpus must carry the ~132 self-named containers this pass exists for, found \
             {total} — a low number here means the field name or the block list changed"
        );
        assert!(
            !bundled.is_empty(),
            "at least the village and trial-chamber tables are bundled; none resolved, which \
             means the lookup rather than the bundle is broken"
        );
        // The tables that are genuinely not bundled yet. Listed by name so the
        // blocked set is a reviewable fact rather than a tolerance: adding one of
        // these files to `assets/loot_table/chests/` must make this fail and be
        // deleted from the list, and a *new* unbundled table must fail too.
        // Measured, not assumed: this list is what the scan above actually reported
        // as unbundled. A brief handed to this work claimed all twelve of these had
        // already been bundled; `ls assets/loot_table/chests/` and this gate both say
        // otherwise, which is why the split is asserted here rather than described in
        // a doc.
        //
        // Note `chests/ruined_portal` and the sixteen `chests/village/*` **are**
        // bundled, so those structures' containers do roll — the blocked set is
        // narrower than "everything that was empty before".
        let known_blocked: BTreeSet<&str> = [
            "minecraft:chests/ancient_city",
            "minecraft:chests/bastion_bridge",
            "minecraft:chests/bastion_hoglin_stable",
            "minecraft:chests/bastion_other",
            "minecraft:chests/bastion_treasure",
            "minecraft:chests/pillager_outpost",
            // The trial chambers bundle only `entrance` and the four `reward*`
            // tables; these five are the ones its corridors and dispensers name.
            "minecraft:chests/trial_chambers/corridor",
            "minecraft:chests/trial_chambers/intersection",
            "minecraft:chests/trial_chambers/intersection_barrel",
            "minecraft:chests/trial_chambers/supply",
            "minecraft:dispensers/trial_chambers/chamber",
        ]
        .into_iter()
        .collect();
        let unexpected: Vec<&String> = missing
            .keys()
            .filter(|table| !known_blocked.contains(table.as_str()))
            .collect();
        assert!(
            unexpected.is_empty(),
            "these self-named tables are neither bundled nor on the known-blocked list, so \
             their containers generate empty with nothing recording why: {unexpected:?}"
        );
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
