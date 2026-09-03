//! The chunk *schema*: `ChunkColumn` ↔ the NBT tree an Anvil region file holds
//! (issue [#437](https://github.com/matteopolak/lodestone/issues/437)).
//!
//! # What it is
//!
//! `lodestone-anvil` deliberately stops at the *container* — it hands back "an
//! arbitrary NBT blob at a given chunk coordinate" and parses no chunk schema
//! at all (its own module doc says so, and issue #298 names the separation as a
//! trap to preserve). This module is the other half: the mapping between that
//! blob and [`crate::chunk::ChunkColumn`], i.e. vanilla's own chunk
//! serialization territory, which issue #437 is where it "gets decided".
//!
//! # How it works
//!
//! The schema was **read off a real 26.2 world**, not derived from the
//! decompiled source and not guessed: `.cache/mc/survival/world/dimensions/
//! minecraft/overworld/region/r.0.0.mca`, written by a real Mojang server,
//! dumped field-by-field. What that dump established, and every part of it is
//! load-bearing:
//!
//! | observation | consequence here |
//! |---|---|
//! | `DataVersion = 4903` | [`DATA_VERSION`] |
//! | `yPos = Int(-4)` — the min *section*, not the min block | `min_y = yPos * 16` |
//! | palette len 6/8/9/13 ⇒ `data` is **256** longs | `bits = max(4, ceil_log2(len))`, 16 per long |
//! | palette len 20 ⇒ `data` is **342** longs, not 320 | packing is **non-spanning**: `⌈4096/12⌉ = 342`, entries never straddle a long |
//! | palette len 1 ⇒ **no `data` field at all** | [`pack_indices`] returns `None`, and a reader must treat absent `data` as "every cell is palette[0]" |
//! | `biomes` palette len 2 ⇒ `data` is **1** long | biomes are 64 cells at `bits = max(1, ceil_log2(len))`, a different floor from block states' 4 |
//! | palette entries are `{Name, Properties?}` | [`state_to_palette_entry`] / [`palette_entry_to_state`] |
//!
//! The non-spanning rule is the one that silently corrupts everything if
//! guessed wrong, and 342-vs-320 is the *only* place the difference shows up in
//! an otherwise identical-looking file — every palette of 16 or fewer entries
//! divides 64 evenly and reads correctly under either rule.
//!
//! Block order inside a section is `(y << 8) | (z << 4) | x`, which is
//! **already** [`ChunkColumn`]'s own layout (`blocks[(y_local * 16 + z) * 16 +
//! x]`) restricted to one 16-row slice — so a section is a contiguous
//! `y_local` window and needs no index shuffle, only a palette remap.
//!
//! # How to change it, and the gotchas
//!
//! - **Heightmaps are deliberately not written.** Vanilla re-primes any
//!   heightmap missing from the file — its own chunk-load path re-primes
//!   any type not present in the saved data — so omitting them
//!   is a supported input, whereas writing a *wrong* one is trusted and
//!   corrupts terrain silently. Computing `MOTION_BLOCKING` correctly needs a
//!   per-state "blocks motion" census this crate does not have; `WORLD_SURFACE`
//!   we could compute, but a half-filled `Heightmaps` compound is worse than an
//!   absent one because `status.heightmapsAfter()` decides per type. Do not add
//!   one without the census.
//!   The read direction still *uses* vanilla's heightmaps — as an oracle, in
//!   `tests/chunk_nbt_vanilla_oracle.rs`, never as data.
//! - **`Status` is the one genuinely mandatory field.** `parse` returns `null`
//!   for an empty `Status` and defaults literally everything else. We write
//!   `minecraft:full`, because anything less makes a real server re-run
//!   worldgen over our terrain.
//! - **Properties are sorted by name** when a palette entry is turned back into
//!   a canonical state string. That is not cosmetic: `lodestone_data::
//!   block_states::properties` documents its slice as sorted, and the worldgen
//!   canonical strings this server compares against are sorted too, so an
//!   unsorted reconstruction produces a string that is `!=` the identical state
//!   and every downstream `match` misses.
//! - **A section outside the world's vertical extent is skipped, not an error.**
//!   Vanilla does exactly this (`y >= getMinSectionY() && y <= getMaxSectionY()`)
//!   because it writes light-only sections one past each end. Hence
//!   [`column_from_nbt`] takes the extent from its caller rather than inferring
//!   it from the section list, which would inflate the column by 32 rows.
//!
//! # Block entities and scheduled ticks
//!
//! [`ChunkExtras`] carries the two lists this module wrote empty until issue
//! [#468](https://github.com/matteopolak/lodestone/issues/468) — so a saved
//! container came back empty and a pending tick was lost. Both halves were
//! read off real 26.2 worlds with an independent stdlib parser (22,488 chunks
//! across `.cache/mc/{survival,creative,26.2,terrain}`), and the measurement
//! contradicted the obvious reading of the decompiled source twice:
//!
//! | measured | consequence |
//! |---|---|
//! | `p` is `Int(0)` on all 133,051 saved ticks | [`tick_priority_value`] writes the **value**, not the ordinal — `Normal` is value `0` and ordinal `3` |
//! | `t` is negative on 1,584 of them, down to `-1046` | [`SavedTick::delay`] is `i32`; loading is `game_time + delay` |
//! | items are `{Slot: Byte, id: String, count: Int}` | `count` is an `Int`, not the pre-1.20.5 `Count: Byte` |
//! | every entry carries `keepPacked: Byte` and `components: Compound` | written unconditionally, matching vanilla |
//!
//! Both tick traps fail *silently* under a round trip through our own writer,
//! because the writer and the reader would share the mistake — which is why
//! `tests/chunk_extras_vanilla_oracle.rs` reads bytes Mojang wrote.
//!
//! Two of the four block-entity kinds this crate simulates are written under
//! **namespaced** ids or fields, and each has a reason recorded at its own
//! definition: [`COMPOSTER_ID`] (vanilla has no composter block entity at all)
//! and [`RECIPES_USED_FIELD`] (our keys are not vanilla recipe ids). The
//! furnace family, the hopper and the brewing stand are written under their
//! real vanilla ids with vanilla's own fields.
//!
//! # Configuration
//!
//! None. [`DATA_VERSION`] is a constant, not a setting.
//!
//! # Dependencies
//!
//! `lodestone-core` for the `Nbt` tree, [`crate::chunk::ChunkColumn`], and —
//! for the block-entity half — [`crate::block_entities`] and the four
//! simulations behind it. No filesystem access: this module is pure
//! tree-to-struct, which is what lets it be tested against bytes a real server
//! wrote without any I/O harness.

use std::collections::HashMap;

use lodestone_core::{Nbt, NbtTag};
use lodestone_model::{BlockPos, ItemStack};
use lodestone_worldgen::overworld::block_entities::GeneratedBlockEntity;

use crate::block_entities::BlockEntity;
use crate::brewing::{Bottle, BottleKind, BrewingStand};
use crate::chunk::ChunkColumn;
use crate::composter::Composter;
use crate::furnace::{Furnace, FurnaceKind};
use crate::hopper::Hopper;
use crate::mob_spawner::{SpawnData, SpawnerState, WeightedSpawnData};
use crate::scheduled_tick::TickPriority;

/// The `DataVersion` a 26.2 server stamps on every chunk it writes, read
/// directly off `.cache/mc/survival/world`'s `r.0.0.mca` rather than from any
/// table.
pub const DATA_VERSION: i32 = 4903;

/// Cells along one section edge.
const SECTION_EDGE: usize = 16;
/// Blocks in one 16×16×16 section.
const SECTION_VOLUME: usize = SECTION_EDGE * SECTION_EDGE * SECTION_EDGE;
/// Biome cells in one section: 4×4×4 quarts.
const BIOME_CELLS: usize = 4 * 4 * 4;
/// Minimum bits per block-state index, regardless of how small the palette is.
/// Vanilla's `PalettedContainer` floor; confirmed by the 6-entry palette that
/// still measured 256 longs (4 bits) rather than 128 (3 bits).
const BLOCK_BITS_FLOOR: u32 = 4;
/// Minimum bits per biome index. A *different* floor from block states, and the
/// 2-entry biome palette measuring exactly one long is what pins it to 1.
const BIOME_BITS_FLOOR: u32 = 1;

/// What can go wrong turning an on-disk NBT tree into a column.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The root tag was not a compound, so nothing else could be read.
    #[error("chunk root is not a compound")]
    RootNotCompound,
    /// A field this decoder needs was absent or held the wrong tag.
    #[error("chunk field {field:?} is missing or has the wrong type")]
    BadField {
        /// The NBT path that failed, for example `sections[3].block_states`.
        field: String,
    },
    /// A packed index pointed past the end of its own palette.
    #[error("palette index {index} out of range for a {len}-entry palette in section Y={y}")]
    PaletteIndexOutOfRange {
        /// The offending index.
        index: usize,
        /// Palette length it was indexed against.
        len: usize,
        /// Section Y, for locating the damage.
        y: i32,
    },
    /// `data` was present but too short for 4096 (or 64) non-spanning entries.
    #[error("packed data for section Y={y} holds {got} longs, need {need} at {bits} bits/entry")]
    PackedTooShort {
        /// Section Y.
        y: i32,
        /// Longs actually present.
        got: usize,
        /// Longs required.
        need: usize,
        /// Bits per entry in force.
        bits: u32,
    },
}

fn field<'a>(compound: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match compound {
        Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn bad(field_path: &str) -> Error {
    Error::BadField {
        field: field_path.to_owned(),
    }
}

/// Bits needed to index a palette of `len` entries, at or above `floor`.
///
/// `len <= 1` returns `floor` but callers must not pack at all in that case —
/// vanilla omits `data` entirely for a single-valued container, which this
/// module reproduces in [`pack_indices`].
#[must_use]
fn bits_for(len: usize, floor: u32) -> u32 {
    let needed = if len <= 1 {
        1
    } else {
        usize::BITS - (len - 1).leading_zeros()
    };
    needed.max(floor)
}

/// Packs `indices` into vanilla's **non-spanning** long array, or `None` when
/// the palette has a single entry and vanilla would omit `data` altogether.
///
/// Non-spanning means `64 / bits` entries per long with the remaining high bits
/// left as padding, *not* a dense bit stream. This is the rule that the 20-entry
/// palette's 342-long array proves (a dense stream would be 320) and the one
/// that silently corrupts a world if implemented as a dense stream, because
/// every palette of 16 or fewer entries reads identically under both.
#[must_use]
fn pack_indices(indices: &[u16], palette_len: usize, floor: u32) -> Option<Vec<i64>> {
    if palette_len <= 1 {
        return None;
    }
    let bits = bits_for(palette_len, floor);
    let per_long = (64 / bits) as usize;
    let long_count = indices.len().div_ceil(per_long);
    let mut out = vec![0i64; long_count];
    for (i, &index) in indices.iter().enumerate() {
        let long = i / per_long;
        let shift = (i % per_long) as u32 * bits;
        out[long] |= ((u64::from(index)) << shift) as i64;
    }
    Some(out)
}

/// Unpacks `count` non-spanning entries of `bits` each out of `data`.
fn unpack_indices(data: &[i64], count: usize, bits: u32, y: i32) -> Result<Vec<u16>, Error> {
    let per_long = (64 / bits) as usize;
    let need = count.div_ceil(per_long);
    if data.len() < need {
        return Err(Error::PackedTooShort {
            y,
            got: data.len(),
            need,
            bits,
        });
    }
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let long = data[i / per_long] as u64;
        let shift = (i % per_long) as u32 * bits;
        out.push(((long >> shift) & mask) as u16);
    }
    Ok(out)
}

/// Splits a canonical state string into its block name and raw property body:
/// `minecraft:deepslate[axis=y]` → `("minecraft:deepslate", Some("axis=y"))`.
fn split_state(state: &str) -> (&str, Option<&str>) {
    match state.find('[') {
        Some(open) if state.ends_with(']') => {
            (&state[..open], Some(&state[open + 1..state.len() - 1]))
        }
        _ => (state, None),
    }
}

/// Turns a canonical state string into vanilla's `{Name, Properties?}` palette
/// entry.
#[must_use]
pub fn state_to_palette_entry(state: &str) -> Nbt {
    let (name, props) = split_state(state);
    let mut fields = vec![("Name".to_owned(), Nbt::String(name.to_owned()))];
    if let Some(body) = props.filter(|b| !b.is_empty()) {
        let pairs: Vec<(String, Nbt)> = body
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_owned(), Nbt::String(v.to_owned())))
            .collect();
        if !pairs.is_empty() {
            fields.push(("Properties".to_owned(), Nbt::Compound(pairs)));
        }
    }
    Nbt::Compound(fields)
}

/// Turns vanilla's `{Name, Properties?}` palette entry back into a canonical
/// state string, **sorting properties by name**.
///
/// The sort is required, not tidy: `lodestone_data::block_states::properties`
/// is documented as sorted and the worldgen strings this server compares
/// against are sorted, so reconstructing in the file's own field order would
/// produce a string unequal to the identical state.
pub fn palette_entry_to_state(entry: &Nbt, path: &str) -> Result<String, Error> {
    let Some(Nbt::String(name)) = field(entry, "Name") else {
        return Err(bad(&format!("{path}.Name")));
    };
    let mut props: Vec<(&str, &str)> = match field(entry, "Properties") {
        Some(Nbt::Compound(fields)) => fields
            .iter()
            .filter_map(|(k, v)| match v {
                Nbt::String(s) => Some((k.as_str(), s.as_str())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if props.is_empty() {
        return Ok(name.clone());
    }
    props.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut out = String::with_capacity(name.len() + props.len() * 12);
    out.push_str(name);
    out.push('[');
    for (i, (k, v)) in props.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out.push(']');
    Ok(out)
}

/// Encodes a column as the chunk NBT tree a 26.2 region file holds, with
/// empty `block_entities`/`block_ticks`/`fluid_ticks`.
///
/// Callers that have a world's live block entities and pending ticks to write
/// want [`column_to_nbt_with`] — this is the terrain-only shortcut, kept for
/// the tests and oracles that only ever had terrain.
#[must_use]
pub fn column_to_nbt(cx: i32, cz: i32, column: &ChunkColumn) -> Nbt {
    column_to_nbt_with(cx, cz, column, &ChunkExtras::default())
}

/// Encodes a column as the chunk NBT tree a 26.2 region file holds.
///
/// Writes `Status = "minecraft:full"` (the one field vanilla treats as
/// mandatory) and omits `Heightmaps` so vanilla re-primes them — see this
/// module's doc comment for why writing them would be worse than omitting them.
///
/// `extras` supplies the chunk's block entities and its pending block/fluid
/// ticks; see [`ChunkExtras`] for the two schema traps in the tick half.
/// Nothing here filters by chunk — the caller is expected to have grouped
/// already, matching vanilla's own `SavedTick.filterTickListForChunk`.
#[must_use]
pub fn column_to_nbt_with(cx: i32, cz: i32, column: &ChunkColumn, extras: &ChunkExtras) -> Nbt {
    let min_section = column.min_y.div_euclid(16);
    let section_count = (column.height as usize).div_ceil(SECTION_EDGE);
    let palette = column.raw_palette();
    // Reused across sections: `append_section_cells` materialises one section at a
    // time (`crate::chunk_blocks` has no flat grid to borrow), so this is one
    // allocation for the whole column rather than one per section.
    let mut section_cells: Vec<u16> = Vec::with_capacity(SECTION_VOLUME);

    let mut sections = Vec::with_capacity(section_count);
    for s in 0..section_count {
        section_cells.clear();
        column.append_section_cells(s, &mut section_cells);
        let cells = &section_cells[..];

        // Remap the column-wide palette down to just the states this section
        // actually uses. Vanilla's containers are per-section, and a
        // column-wide palette would inflate `bits` for every section — a
        // 20-entry column palette would force 5 bits on an all-air section
        // that vanilla stores with no `data` array at all.
        let mut local: Vec<&str> = Vec::new();
        let mut remap = vec![u16::MAX; palette.len()];
        let mut indices = Vec::with_capacity(SECTION_VOLUME);
        for &id in cells {
            let id = id as usize;
            if remap[id] == u16::MAX {
                remap[id] = local.len() as u16;
                local.push(&palette[id]);
            }
            indices.push(remap[id]);
        }

        let mut block_states = vec![(
            "palette".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: local
                    .iter()
                    .map(|state| state_to_palette_entry(state))
                    .collect(),
            },
        )];
        if let Some(data) = pack_indices(&indices, local.len(), BLOCK_BITS_FLOOR) {
            block_states.push(("data".to_owned(), Nbt::LongArray(data)));
        }

        // Biomes are a real 4×4×4 grid per section (issue #512), read out of the
        // column's own 3-D cells. This used to repeat the 16 surface quarts
        // across all four y-layers, which is why re-saving a vanilla world
        // erased every `lush_caves`/`dripstone_caves`/`deep_dark` cell it held:
        // the surface value overwrote them. The cell order — `(qy * 4 + qz) * 4
        // + qx` — is vanilla's biome container order and `ChunkColumn`'s alike,
        // so `cell` decomposes directly.
        let mut biome_local: Vec<&str> = Vec::new();
        let mut biome_indices = Vec::with_capacity(BIOME_CELLS);
        for cell in 0..BIOME_CELLS {
            let name = column.biome_cell(cell % 4, s * 4 + cell / 16, (cell / 4) % 4);
            let index = biome_local
                .iter()
                .position(|b| *b == name)
                .unwrap_or_else(|| {
                    biome_local.push(name);
                    biome_local.len() - 1
                });
            biome_indices.push(index as u16);
        }
        let mut biomes = vec![(
            "palette".to_owned(),
            Nbt::List {
                element_type: NbtTag::String,
                elements: biome_local
                    .iter()
                    .map(|b| Nbt::String((*b).to_owned()))
                    .collect(),
            },
        )];
        if let Some(data) = pack_indices(&biome_indices, biome_local.len(), BIOME_BITS_FLOOR) {
            biomes.push(("data".to_owned(), Nbt::LongArray(data)));
        }

        sections.push(Nbt::Compound(vec![
            ("Y".to_owned(), Nbt::Byte((min_section + s as i32) as i8)),
            ("block_states".to_owned(), Nbt::Compound(block_states)),
            ("biomes".to_owned(), Nbt::Compound(biomes)),
        ]));
    }

    Nbt::Compound(vec![
        ("DataVersion".to_owned(), Nbt::Int(DATA_VERSION)),
        ("xPos".to_owned(), Nbt::Int(cx)),
        ("yPos".to_owned(), Nbt::Int(min_section)),
        ("zPos".to_owned(), Nbt::Int(cz)),
        (
            "Status".to_owned(),
            Nbt::String("minecraft:full".to_owned()),
        ),
        ("LastUpdate".to_owned(), Nbt::Long(0)),
        ("InhabitedTime".to_owned(), Nbt::Long(0)),
        // Zero, not one: our sections carry no `SkyLight`/`BlockLight`, and
        // claiming the light is correct would have a real client render our
        // terrain pitch black rather than relight it.
        ("isLightOn".to_owned(), Nbt::Byte(0)),
        (
            "sections".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: sections,
            },
        ),
        (
            "block_entities".to_owned(),
            nbt_list(
                extras
                    .block_entities
                    .iter()
                    .map(|(pos, entity)| block_entity_to_nbt(*pos, entity))
                    .collect(),
            ),
        ),
        (
            "block_ticks".to_owned(),
            nbt_list(extras.block_ticks.iter().map(saved_tick_to_nbt).collect()),
        ),
        (
            "fluid_ticks".to_owned(),
            nbt_list(extras.fluid_ticks.iter().map(saved_tick_to_nbt).collect()),
        ),
        ("structures".to_owned(), structures_to_nbt(column)),
    ])
}

/// The chunk's `structures` compound (issue #514's S1): `starts` for the
/// structures whose origin is this chunk, `References` for the ones it merely
/// participates in.
///
/// **This shipped as two permanently empty compounds** — the same "populated
/// empty" defect as the omitted heightmaps: a field that exists, is well-formed,
/// and always says nothing, so no reader ever errors and the absence is invisible
/// until you look for a village that should be there.
///
/// Field names and shapes are vanilla's `StructureStart.createTag` /
/// `SerializableChunkData`: `starts` is keyed by structure id, each value
/// `{id, ChunkX, ChunkZ, references, Children}` — with `id: "INVALID"` for an
/// absent start, which is why an *incomplete* start must not be written at all
/// rather than written empty (see
/// [`StructureStart::pieces_complete`](lodestone_worldgen::structure::StructureStart::pieces_complete);
/// the generator's own `structure_starts` already filters those out). Each child
/// is `{id, BB, O, GD}` plus `Template` for a template-driven piece; `BB` is the
/// six-int `[minx, miny, minz, maxx, maxy, maxz]` array and `O` is `-1` for an
/// unoriented piece.
#[must_use]
fn structures_to_nbt(column: &ChunkColumn) -> Nbt {
    let starts: Vec<(String, Nbt)> = column
        .structure_starts()
        .iter()
        .map(|start| {
            let children: Vec<Nbt> = start
                .pieces
                .iter()
                .map(|piece| {
                    let mut fields = vec![
                        ("id".to_owned(), Nbt::String(piece.id.clone())),
                        (
                            "BB".to_owned(),
                            Nbt::IntArray(vec![
                                piece.bounding_box.min[0],
                                piece.bounding_box.min[1],
                                piece.bounding_box.min[2],
                                piece.bounding_box.max[0],
                                piece.bounding_box.max[1],
                                piece.bounding_box.max[2],
                            ]),
                        ),
                        ("O".to_owned(), Nbt::Int(piece.orientation.unwrap_or(-1))),
                        ("GD".to_owned(), Nbt::Int(piece.gen_depth)),
                    ];
                    if let Some(template) = &piece.template {
                        fields.push(("Template".to_owned(), Nbt::String(template.clone())));
                    }
                    Nbt::Compound(fields)
                })
                .collect();
            (
                start.structure.clone(),
                Nbt::Compound(vec![
                    ("id".to_owned(), Nbt::String(start.structure.clone())),
                    ("ChunkX".to_owned(), Nbt::Int(start.chunk_x)),
                    ("ChunkZ".to_owned(), Nbt::Int(start.chunk_z)),
                    ("references".to_owned(), Nbt::Int(start.references)),
                    ("Children".to_owned(), nbt_list(children)),
                ]),
            )
        })
        .collect();

    let references: Vec<(String, Nbt)> = column
        .structure_references()
        .iter()
        .map(|(id, packed)| (id.clone(), Nbt::LongArray(packed.clone())))
        .collect();

    Nbt::Compound(vec![
        ("References".to_owned(), Nbt::Compound(references)),
        ("starts".to_owned(), Nbt::Compound(starts)),
    ])
}

/// Decodes a chunk NBT tree into a column of the caller's vertical extent.
///
/// `min_y`/`height` come from the caller rather than from `yPos` on purpose:
/// vanilla writes light-only sections one past each end of the world, and
/// inferring the extent from the section list would produce a column 32 rows
/// taller than the world. Sections outside `[min_y, min_y + height)` are
/// skipped, exactly as `SerializableChunkData.parse` skips them.
pub fn column_from_nbt(nbt: &Nbt, min_y: i32, height: i32) -> Result<ChunkColumn, Error> {
    if !matches!(nbt, Nbt::Compound(_)) {
        return Err(Error::RootNotCompound);
    }
    let Some(Nbt::List {
        elements: sections, ..
    }) = field(nbt, "sections")
    else {
        return Err(bad("sections"));
    };

    let mut column = ChunkColumn::new(min_y, height);
    let min_section = min_y.div_euclid(16);
    let section_count = (height as usize).div_ceil(SECTION_EDGE);

    for (i, section) in sections.iter().enumerate() {
        let Some(&Nbt::Byte(y)) = field(section, "Y") else {
            return Err(bad(&format!("sections[{i}].Y")));
        };
        let section_index = i32::from(y) - min_section;
        if section_index < 0 || section_index as usize >= section_count {
            // Vanilla's own out-of-range skip, not an error.
            continue;
        }
        let Some(block_states) = field(section, "block_states") else {
            // A section with no `block_states` is all-air, which
            // `ChunkColumn::new` already gave us.
            continue;
        };
        let path = format!("sections[{i}].block_states");
        let Some(Nbt::List {
            elements: palette, ..
        }) = field(block_states, "palette")
        else {
            return Err(bad(&format!("{path}.palette")));
        };
        if palette.is_empty() {
            return Err(bad(&format!("{path}.palette")));
        }
        let states = palette
            .iter()
            .enumerate()
            .map(|(p, entry)| palette_entry_to_state(entry, &format!("{path}.palette[{p}]")))
            .collect::<Result<Vec<_>, _>>()?;

        let indices = match field(block_states, "data") {
            // Absent `data` is not a defect: vanilla omits it for a
            // single-valued section, which is most of the sky.
            None => vec![0u16; SECTION_VOLUME],
            Some(Nbt::LongArray(data)) => unpack_indices(
                data,
                SECTION_VOLUME,
                bits_for(states.len(), BLOCK_BITS_FLOOR),
                i32::from(y),
            )?,
            Some(_) => return Err(bad(&format!("{path}.data"))),
        };

        let y_base = min_y + section_index * SECTION_EDGE as i32;
        // Validate every index against the section's own (small) palette
        // before the bulk write below, which trusts `indices` and does not
        // bounds-check — one fast pass over up to 4096 `u16`s, not a scan of
        // anything proportional to the column-wide palette.
        if let Some(&bad_index) = indices.iter().find(|&&index| index as usize >= states.len()) {
            return Err(Error::PaletteIndexOutOfRange {
                index: bad_index as usize,
                len: states.len(),
                y: i32::from(y),
            });
        }
        // Interns this section's *local* palette into the column-wide one once
        // per distinct state (dozens at most), then writes all 4096 cells from
        // that remap — not once-per-cell through `ChunkColumn::set_block`,
        // which used to make every loaded column ~98,304 linear scans of the
        // column-wide palette. See `ChunkColumn::set_section_from_local_palette`.
        let local: Vec<&str> = states.iter().map(String::as_str).collect();
        column.set_section_from_local_palette(y_base, &local, &indices);

        // Biomes: every section's full 4×4×4 container, into the column's 3-D
        // grid (issue #512). Reading only section 0's y=0 layer — what this did
        // before — is the load half of the cave-biome erasure: the deepest
        // section's value was broadcast over the whole column, and every cave
        // biome above it was gone before the writer ever ran.
        //
        // The surface array (`set_biome_quarts`) still comes from the lowest
        // section that carries one. It is not the same question as the grid and
        // nothing derives one from the other, so it is left exactly as it was.
        if let Some(biomes) = field(section, "biomes")
            && let Some(Nbt::List {
                elements: biome_palette,
                ..
            }) = field(biomes, "palette")
            && !biome_palette.is_empty()
        {
            let names: Vec<String> = biome_palette
                .iter()
                .map(|e| match e {
                    Nbt::String(s) => s.clone(),
                    _ => String::new(),
                })
                .collect();
            let cells = match field(biomes, "data") {
                None => vec![0u16; BIOME_CELLS],
                Some(Nbt::LongArray(data)) => unpack_indices(
                    data,
                    BIOME_CELLS,
                    bits_for(names.len(), BIOME_BITS_FLOOR),
                    i32::from(y),
                )?,
                Some(_) => return Err(bad(&format!("sections[{i}].biomes.data"))),
            };
            for (cell, &index) in cells.iter().enumerate() {
                let name = names.get(index as usize).map_or("", String::as_str);
                if !name.is_empty() {
                    column.set_biome_cell(
                        cell % 4,
                        section_index as usize * 4 + cell / 16,
                        (cell / 4) % 4,
                        name,
                    );
                }
            }

            if section_index == 0 {
                let mut quarts: Vec<String> = Vec::with_capacity(16);
                for quart in 0..16 {
                    let index = cells[quart] as usize;
                    quarts.push(names.get(index).cloned().unwrap_or_default());
                }
                column.set_biome_quarts(&quarts);
            }
        }
    }

    Ok(column)
}

// ---------------------------------------------------------------------------
// Block entities and scheduled ticks (issue #468's remaining half)
// ---------------------------------------------------------------------------

/// One pending scheduled tick as it sits **on disk**, mirroring the real
/// per-tick save-data record: a type, a position, a relative delay, and a
/// priority.
///
/// Deliberately a different type from [`crate::scheduled_tick::ScheduledTick`],
/// exactly as it is in the real engine, because the two disagree about the one field
/// that matters: a live tick carries an **absolute** `trigger_tick`, a saved
/// one carries a **relative, signed** `delay`. The real engine converts with
/// its own unpack step, transcribed as the rule it implements: rebuild the
/// live tick record with the same type, position and priority, but with the
/// trigger tick computed as the current game time plus the saved delay, and
/// the sub-tick order taken from the current counter.
///
/// so a load is `trigger_tick = game_time_at_load + delay` and **`delay` is
/// routinely negative** — see [`Self::delay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTick {
    /// Absolute block position.
    pub pos: (i32, i32, i32),
    /// The block or fluid id being ticked — the real save format's `i` field.
    pub kind: String,
    /// Ticks from the game time **at save** until this tick is due, the real
    /// save format's `t` field.
    ///
    /// **Signed, and negative in real worlds.** Measured across 22,488 real
    /// chunks with an independent parser: 1,584 of 133,051 saved ticks
    /// carry a negative delay, the extreme being `-1046` for an overdue birch
    /// leaves decay and `-33` for an overdue lava tick. A world is saved
    /// mid-tick with a backlog and the real engine simply records how overdue each
    /// entry already was, so an unsigned field here panics or wraps on an
    /// ordinary survival world.
    pub delay: i32,
    /// Vanilla's `p`. See [`tick_priority_value`] for the trap.
    pub priority: TickPriority,
}

/// A chunk's contents that are not blocks: its block entities, and the block
/// and fluid ticks pending inside it.
///
/// Before this existed [`column_to_nbt`] wrote all three lists empty for every
/// chunk, so a saved container came back empty and a pending redstone or fluid
/// tick was lost outright (issue #468).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkExtras {
    /// Every block entity in the chunk, at its **absolute** position.
    pub block_entities: Vec<(BlockPos, BlockEntity)>,
    /// Pending entries of `ServerLevel.blockTicks` inside this chunk.
    pub block_ticks: Vec<SavedTick>,
    /// Pending entries of `ServerLevel.fluidTicks` inside this chunk.
    pub fluid_ticks: Vec<SavedTick>,
}

impl ChunkExtras {
    /// `true` when there is nothing at all to write — the common case, and
    /// what lets a caller skip building the lists for most chunks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.block_entities.is_empty() && self.block_ticks.is_empty() && self.fluid_ticks.is_empty()
    }
}

/// Vanilla's `-3..3` [`TickPriority`] **value**, which is what `p` holds on
/// disk — **not** the ordinal.
///
/// # The trap, and why it is invisible without an outside oracle
///
/// Vanilla's own codec for the priority enum maps the wire int through a
/// by-value lookup one way and a plain accessor the other, so the int written is
/// that accessor's return, not the ordinal. Our [`TickPriority`] is declared in vanilla's order *on
/// purpose*, so that `#[derive(Ord)]` reproduces vanilla's own ordinal-based
/// comparison for free — which makes `Normal`'s **ordinal 3** and its
/// **value 0**. Writing the ordinal would therefore turn every ordinary tick
/// in the world into `EXTREMELY_LOW`, and a round-trip gate against our own
/// writer could never see it because the reader would make the same mistake.
///
/// The independent measurement that settles it: all 133,051 saved ticks across
/// 22,488 real vanilla chunks carry `p: Int(0)`, and leaf decay — the
/// overwhelming majority of them — is scheduled at `NORMAL`.
#[must_use]
pub fn tick_priority_value(priority: TickPriority) -> i32 {
    match priority {
        TickPriority::ExtremelyHigh => -3,
        TickPriority::VeryHigh => -2,
        TickPriority::High => -1,
        TickPriority::Normal => 0,
        TickPriority::Low => 1,
        TickPriority::VeryLow => 2,
        TickPriority::ExtremelyLow => 3,
    }
}

/// The inverse of [`tick_priority_value`], clamping out-of-range values
/// exactly as vanilla's own by-value lookup does (below
/// `-3` saturates to `EXTREMELY_HIGH`, anything else out of range to
/// `EXTREMELY_LOW`) rather than erroring — a corrupt `p` should cost a
/// mis-ordered tick, never a chunk that will not load.
#[must_use]
pub fn tick_priority_from_value(value: i32) -> TickPriority {
    match value {
        -3 => TickPriority::ExtremelyHigh,
        -2 => TickPriority::VeryHigh,
        -1 => TickPriority::High,
        0 => TickPriority::Normal,
        1 => TickPriority::Low,
        2 => TickPriority::VeryLow,
        3 => TickPriority::ExtremelyLow,
        v if v < -3 => TickPriority::ExtremelyHigh,
        _ => TickPriority::ExtremelyLow,
    }
}

/// Vanilla writes an *empty* list with element type `End`, and a populated one
/// with `Compound`. Reproduced rather than always writing `Compound`, because
/// every real file this repo has read does it this way.
fn nbt_list(elements: Vec<Nbt>) -> Nbt {
    Nbt::List {
        element_type: if elements.is_empty() {
            NbtTag::End
        } else {
            NbtTag::Compound
        },
        elements,
    }
}

fn int_field(compound: &Nbt, key: &str) -> Option<i32> {
    match field(compound, key)? {
        Nbt::Int(v) => Some(*v),
        Nbt::Short(v) => Some(i32::from(*v)),
        Nbt::Byte(v) => Some(i32::from(*v)),
        _ => None,
    }
}

fn string_field<'a>(compound: &'a Nbt, key: &str) -> Option<&'a str> {
    match field(compound, key)? {
        Nbt::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Encodes one [`SavedTick`] as vanilla's `{i, x, y, z, t, p}` compound.
#[must_use]
fn saved_tick_to_nbt(tick: &SavedTick) -> Nbt {
    Nbt::Compound(vec![
        ("i".to_owned(), Nbt::String(tick.kind.clone())),
        ("x".to_owned(), Nbt::Int(tick.pos.0)),
        ("y".to_owned(), Nbt::Int(tick.pos.1)),
        ("z".to_owned(), Nbt::Int(tick.pos.2)),
        ("t".to_owned(), Nbt::Int(tick.delay)),
        (
            "p".to_owned(),
            Nbt::Int(tick_priority_value(tick.priority)),
        ),
    ])
}

/// Decodes one saved tick, or `None` if the compound is missing a field the
/// record has no default for (`i`/`x`/`y`/`z`).
///
/// `t` and `p` both default to `0`, matching `SavedTick.probe`'s own
/// `(0, TickPriority.NORMAL)` — and note `0` is the correct default for `p`
/// precisely *because* it is a value rather than an ordinal.
fn saved_tick_from_nbt(nbt: &Nbt) -> Option<SavedTick> {
    Some(SavedTick {
        pos: (
            int_field(nbt, "x")?,
            int_field(nbt, "y")?,
            int_field(nbt, "z")?,
        ),
        kind: string_field(nbt, "i")?.to_owned(),
        delay: int_field(nbt, "t").unwrap_or(0),
        priority: tick_priority_from_value(int_field(nbt, "p").unwrap_or(0)),
    })
}

/// Encodes a container's slots as vanilla's `Items` list: one compound per
/// **occupied** slot, `{Slot: Byte, id: String, count: Int}`.
///
/// Empty slots are omitted rather than written as air, which is why `Slot` is
/// carried explicitly. The `count: Int` is 1.20.5-and-later shaped (it was a
/// `Byte` named `Count` before the component rewrite); read off real 26.2
/// dispenser and hopper entries rather than from any table.
///
/// **Item components are not persisted.** A stack's
/// [`lodestone_model::ItemComponents`] is the wire's decoded patch and has no
/// encoder in this crate; writing a partial one would be worse than writing
/// none, on the same argument this module's doc comment makes about
/// heightmaps. A saved iron sword comes back as an undamaged, unnamed iron
/// sword.
fn items_to_nbt(slots: &[Option<ItemStack>]) -> Nbt {
    let elements: Vec<Nbt> = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let stack = slot.as_ref()?;
            Some(Nbt::Compound(vec![
                ("Slot".to_owned(), Nbt::Byte(index as i8)),
                ("id".to_owned(), Nbt::String(stack.item.to_string())),
                (
                    "count".to_owned(),
                    Nbt::Int(i32::try_from(stack.count).unwrap_or(i32::MAX)),
                ),
            ]))
        })
        .collect();
    nbt_list(elements)
}

/// Decodes an `Items` list into a `len`-slot array. Entries whose `Slot` is
/// out of range, or whose `id` is not a parseable resource key, are dropped —
/// one unreadable stack must not cost the whole container.
fn items_from_nbt(nbt: Option<&Nbt>, len: usize) -> Vec<Option<ItemStack>> {
    let mut out = vec![None; len];
    let Some(Nbt::List { elements, .. }) = nbt else {
        return out;
    };
    for entry in elements {
        let Some(slot) = int_field(entry, "Slot") else {
            continue;
        };
        let Ok(slot) = usize::try_from(slot) else {
            continue;
        };
        if slot >= len {
            continue;
        }
        let Some(id) = string_field(entry, "id") else {
            continue;
        };
        let Ok(key) = id.parse() else {
            continue;
        };
        let count = int_field(entry, "count").unwrap_or(1).max(0) as u32;
        out[slot] = Some(ItemStack::new(key, count));
    }
    out
}

/// Decodes one spawner spawn-entry compound (vanilla's own spawn-data NBT
/// shape: `{entity: {id: "...", ...}, custom_spawn_rules?: {...},
/// equipment?: {...}}`) into the reduced form `crate::mob_spawner` acts on.
///
/// Only `entity.id` is read — see that module's doc for why
/// `custom_spawn_rules`/`equipment` and the rest of the `entity` compound
/// (equipment, custom NBT, age) are not modelled. A missing `entity` compound,
/// a missing `id`, or an `id` this crate's entity-type registry does not know
/// all resolve to [`SpawnData::default`] (`entity_type: None`) rather than a
/// load failure — matching vanilla's own tolerance for a `SpawnData` whose
/// `EntityType.by` lookup comes back empty.
fn spawn_data_from_nbt(nbt: &Nbt) -> SpawnData {
    let entity_type = field(nbt, "entity")
        .and_then(|entity| string_field(entity, "id"))
        .and_then(crate::mob_spawner::entity_type_from_id_field);
    SpawnData { entity_type }
}

/// The inverse of [`spawn_data_from_nbt`]. An unresolved [`SpawnData`] (`entity_type:
/// None`) round-trips as an empty `entity` compound — vanilla's own
/// stripped-constructor shape for "no id" — rather than a placeholder id.
fn spawn_data_to_nbt(data: &SpawnData) -> Nbt {
    let entity = match &data.entity_type {
        Some(entity_type) => Nbt::Compound(vec![(
            "id".to_owned(),
            Nbt::String(entity_type.to_string()),
        )]),
        None => Nbt::Compound(Vec::new()),
    };
    Nbt::Compound(vec![("entity".to_owned(), entity)])
}

/// Decodes a `SpawnPotentials` list — `WeightedList<SpawnData>`'s own wire
/// shape, one `{data: <SpawnData>, weight: <non-negative int>}` compound per
/// entry (`Weighted.codec`). A missing or malformed list decodes as empty;
/// an entry with no `data` compound is dropped, matching [`items_from_nbt`]'s
/// own "one unreadable entry must not cost the whole list" precedent.
fn spawn_potentials_from_nbt(nbt: Option<&Nbt>) -> Vec<WeightedSpawnData> {
    let Some(Nbt::List { elements, .. }) = nbt else {
        return Vec::new();
    };
    elements
        .iter()
        .filter_map(|entry| {
            let data = field(entry, "data")?;
            let weight = int_field(entry, "weight").unwrap_or(0).max(0) as u32;
            Some(WeightedSpawnData {
                weight,
                data: spawn_data_from_nbt(data),
            })
        })
        .collect()
}

/// The inverse of [`spawn_potentials_from_nbt`].
fn spawn_potentials_to_nbt(potentials: &[WeightedSpawnData]) -> Nbt {
    let elements: Vec<Nbt> = potentials
        .iter()
        .map(|entry| {
            Nbt::Compound(vec![
                ("data".to_owned(), spawn_data_to_nbt(&entry.data)),
                ("weight".to_owned(), Nbt::Int(entry.weight as i32)),
            ])
        })
        .collect();
    nbt_list(elements)
}

/// This crate's own id for a composter block entity.
///
/// **Namespaced, because vanilla has no composter block entity at all.** A
/// vanilla composter keeps its fill level in the block state
/// (`minecraft:composter[level=0..8]`) and its ready delay as a scheduled
/// block tick; this crate models it as a block entity instead
/// ([`crate::block_entities`]), and that model has no vanilla field to live
/// in. Writing it under `minecraft:composter` would be a claim vanilla
/// disagrees with the moment Mojang adds a real one; under this id, a real
/// server does what it does with any unrecognised id — logs a skip and drops
/// it — and our own reader gets the level back exactly.
const COMPOSTER_ID: &str = "lodestone:composter";

/// Our furnace's banked recipe-use counts, namespaced.
///
/// Vanilla's `RecipesUsed` is keyed by **recipe id**; this crate's
/// [`Furnace`] banks by its own `"kind:ingredient"` string
/// ([`Furnace::recipes_used`]), which is not a recipe id and would not
/// resolve. A namespaced field carries it losslessly for us and is ignored by
/// vanilla's codec, which reads only the keys it knows.
const RECIPES_USED_FIELD: &str = "lodestone:recipes_used";

/// Turns one [`GeneratedBlockEntity`] into the `(position, block entity)` pair
/// [`ChunkColumn`] carries (issue #520).
///
/// The generator's typed enum becomes a [`BlockEntity::Opaque`] holding the full
/// vanilla save-form compound: this crate has no beehive *simulation* to put the
/// occupants into, and `Opaque` is exactly the variant for "a real block entity
/// we preserve verbatim but do not tick". Both consumers — the region writer
/// ([`block_entity_to_nbt`], which returns an `Opaque`'s tree unchanged) and the
/// chunk packet's block-entity array — want that same tree.
///
/// # Schema provenance
///
/// Vanilla's own beehive block entity save routine stores one field, `bees`, through
/// its occupant list codec, whose records are
/// `{entity_data, ticks_in_hive, min_ticks_in_hive}`. `entity_data`
/// is a typed-entity wrapper whose codec writes the type under the key `"id"` into
/// the entity tag, and every generated
/// occupant's tag is an otherwise-empty compound, so
/// `{id: "minecraft:bee"}` is the whole of it — not a guess about what a bee's
/// NBT looks like.
///
/// `flower_pos` is `storeNullable` and a generated nest has none, so it is
/// omitted rather than written null — which is what vanilla's own writer does.
#[must_use]
pub fn generated_block_entity(entity: &GeneratedBlockEntity) -> (BlockPos, BlockEntity) {
    let (x, y, z) = entity.position();
    let id = entity.type_id().to_owned();
    let mut fields: Vec<(String, Nbt)> = vec![
        ("id".to_owned(), Nbt::String(id.clone())),
        ("x".to_owned(), Nbt::Int(x)),
        ("y".to_owned(), Nbt::Int(y)),
        ("z".to_owned(), Nbt::Int(z)),
        ("keepPacked".to_owned(), Nbt::Byte(0)),
        ("components".to_owned(), Nbt::Compound(Vec::new())),
    ];
    match entity {
        GeneratedBlockEntity::Beehive { bees, .. } => {
            fields.push((
                "bees".to_owned(),
                nbt_list(
                    bees.iter()
                        .map(|bee| {
                            Nbt::Compound(vec![
                                (
                                    "entity_data".to_owned(),
                                    Nbt::Compound(vec![(
                                        "id".to_owned(),
                                        Nbt::String("minecraft:bee".to_owned()),
                                    )]),
                                ),
                                ("ticks_in_hive".to_owned(), Nbt::Int(bee.ticks_in_hive)),
                                (
                                    "min_ticks_in_hive".to_owned(),
                                    Nbt::Int(bee.min_ticks_in_hive),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
    }
    (
        BlockPos::new(x, y, z),
        BlockEntity::Opaque {
            id,
            nbt: Nbt::Compound(fields),
        },
    )
}

/// Encodes one block entity as the chunk NBT list element vanilla holds.
///
/// The `x`/`y`/`z` are **absolute** and `keepPacked` is `0`, both matching
/// every real entry measured. `components` is written as an empty compound
/// because vanilla writes one unconditionally and this crate models no
/// block-entity components.
///
/// `pub` because the **chunk packet** wants the same tree the region file does:
/// a `ServerProtocol::encode_chunk` writes it as the block entity's network NBT
/// (issue #520). The extra `id`/`x`/`y`/`z`/`keepPacked` fields are redundant
/// there — position and type travel in the record header — but harmless, since
/// both our own decoder and vanilla's `BlockEntity.loadWithComponents` read the
/// fields they know and ignore the rest.
#[must_use]
pub fn block_entity_to_nbt(pos: BlockPos, entity: &BlockEntity) -> Nbt {
    let (id, mut extra): (&str, Vec<(String, Nbt)>) = match entity {
        BlockEntity::Opaque { nbt, .. } => return nbt.clone(),
        BlockEntity::Furnace(f) => {
            let (lit_remaining, lit_total, cooking_spent, cooking_total) = f.burn_state();
            let recipes: Vec<(String, Nbt)> = {
                let mut pairs: Vec<(String, Nbt)> = f
                    .recipes_used()
                    .iter()
                    .map(|(k, v)| (k.clone(), Nbt::Int(i32::try_from(*v).unwrap_or(i32::MAX))))
                    .collect();
                // A `HashMap` iterated straight into a file is a
                // nondeterministic byte stream, which makes any
                // byte-comparison gate flap. Sorted, so identical state
                // always encodes identically.
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                pairs
            };
            (
                match f.kind() {
                    FurnaceKind::Furnace => "minecraft:furnace",
                    FurnaceKind::Smoker => "minecraft:smoker",
                    FurnaceKind::BlastFurnace => "minecraft:blast_furnace",
                },
                vec![
                    (
                        "lit_time_remaining".to_owned(),
                        Nbt::Short(lit_remaining as i16),
                    ),
                    ("lit_total_time".to_owned(), Nbt::Short(lit_total as i16)),
                    (
                        "cooking_time_spent".to_owned(),
                        Nbt::Short(cooking_spent as i16),
                    ),
                    (
                        "cooking_total_time".to_owned(),
                        Nbt::Short(cooking_total as i16),
                    ),
                    (
                        "Items".to_owned(),
                        items_to_nbt(&[f.input().cloned(), f.fuel().cloned(), f.output().cloned()]),
                    ),
                    ("RecipesUsed".to_owned(), Nbt::Compound(Vec::new())),
                    (RECIPES_USED_FIELD.to_owned(), Nbt::Compound(recipes)),
                ],
            )
        }
        BlockEntity::Hopper(h) => (
            "minecraft:hopper",
            vec![
                ("TransferCooldown".to_owned(), Nbt::Int(h.cooldown())),
                ("Items".to_owned(), items_to_nbt(h.slots())),
            ],
        ),
        BlockEntity::Container { id, slots } => {
            (id.as_str(), vec![("Items".to_owned(), items_to_nbt(slots))])
        }
        BlockEntity::Composter(c) => (
            COMPOSTER_ID,
            vec![
                ("level".to_owned(), Nbt::Byte(c.level() as i8)),
                (
                    "ticks_until_ready".to_owned(),
                    // `-1` for "no delay running", so the field is always
                    // present and a missing one is unambiguously an old file
                    // rather than a composter that is not counting down.
                    Nbt::Byte(c.ticks_until_ready().map_or(-1, |t| t as i8)),
                ),
            ],
        ),
        BlockEntity::BrewingStand(b) => {
            // Vanilla's 5-slot `BrewingStandMenu` order: 3 bottles, then the
            // ingredient, then the fuel (`BrewingStandBlockEntity`'s own
            // `items` list). Bottles become real potion items, which is what
            // makes this entry vanilla-readable rather than namespaced like
            // the composter above.
            let mut slots: Vec<Option<ItemStack>> = Vec::with_capacity(5);
            for index in 0..3 {
                slots.push(b.bottle(index).map(|bottle| {
                    ItemStack::new(
                        bottle_item_id(bottle.kind)
                            .parse()
                            .expect("bottle item ids are literal, valid resource keys"),
                        1,
                    )
                }));
            }
            slots.push(
                b.ingredient()
                    .and_then(|(id, count)| Some(ItemStack::new(id.parse().ok()?, count))),
            );
            slots.push(
                b.fuel_item()
                    .and_then(|(id, count)| Some(ItemStack::new(id.parse().ok()?, count))),
            );
            // The potion *identity* each bottle holds is a data component
            // (`minecraft:potion_contents`) that `items_to_nbt` deliberately
            // does not write, so it is carried alongside as three strings.
            let potions: Vec<Nbt> = (0..3)
                .map(|index| {
                    Nbt::String(
                        b.bottle(index)
                            .map(|bottle| bottle.potion.clone())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            (
                "minecraft:brewing_stand",
                vec![
                    ("BrewTime".to_owned(), Nbt::Short(b.brew_progress() as i16)),
                    ("Fuel".to_owned(), Nbt::Byte(b.fuel_charges() as i8)),
                    ("Items".to_owned(), items_to_nbt(&slots)),
                    (
                        "lodestone:potions".to_owned(),
                        Nbt::List {
                            element_type: NbtTag::String,
                            elements: potions,
                        },
                    ),
                ],
            )
        }
        // `BaseCommandBlock.save` plus `CommandBlockEntity.saveAdditional`'s
        // own three extra booleans, folded into one field list the way every
        // other entry in this match already folds its block's own save
        // method in. `LastOutput`/`LastExecution` are written only when their
        // own governing flag is set, matching vanilla's own conditional
        // `output.storeNullable`/`output.putLong` calls exactly.
        BlockEntity::CommandBlock(data) => {
            let mut fields = vec![
                ("Command".to_owned(), Nbt::String(data.command.clone())),
                ("SuccessCount".to_owned(), Nbt::Int(data.success_count)),
                ("TrackOutput".to_owned(), Nbt::Byte(i8::from(data.track_output))),
                ("UpdateLastExecution".to_owned(), Nbt::Byte(i8::from(data.update_last_execution))),
                ("powered".to_owned(), Nbt::Byte(i8::from(data.powered))),
                ("conditionMet".to_owned(), Nbt::Byte(i8::from(data.condition_met))),
                ("auto".to_owned(), Nbt::Byte(i8::from(data.auto))),
            ];
            if data.track_output {
                if let Some(last_output) = &data.last_output {
                    fields.push(("LastOutput".to_owned(), Nbt::String(last_output.clone())));
                }
            }
            if data.update_last_execution {
                if let Some(last_execution) = data.last_execution {
                    fields.push(("LastExecution".to_owned(), Nbt::Long(last_execution)));
                }
            }
            ("minecraft:command_block", fields)
        }
        BlockEntity::Spawner(s) => {
            let (delay, min_delay, max_delay, count, max_nearby, required_range, spawn_range, potentials, next) =
                s.saved_fields();
            let mut fields = vec![
                ("Delay".to_owned(), Nbt::Short(delay as i16)),
                ("MinSpawnDelay".to_owned(), Nbt::Short(min_delay as i16)),
                ("MaxSpawnDelay".to_owned(), Nbt::Short(max_delay as i16)),
                ("SpawnCount".to_owned(), Nbt::Short(count as i16)),
                ("MaxNearbyEntities".to_owned(), Nbt::Short(max_nearby as i16)),
                ("RequiredPlayerRange".to_owned(), Nbt::Short(required_range as i16)),
                ("SpawnRange".to_owned(), Nbt::Short(spawn_range as i16)),
                ("SpawnPotentials".to_owned(), spawn_potentials_to_nbt(potentials)),
            ];
            if let Some(next) = next {
                fields.push(("SpawnData".to_owned(), spawn_data_to_nbt(next)));
            }
            ("minecraft:spawner", fields)
        }
        // `SignText.DIRECT_CODEC`: `messages`/`color`/`has_glowing_text` per
        // side, under `front_text`/`back_text`, plus a sibling `is_waxed`.
        //
        // A `messages` element is a `Component` under
        // `ComponentSerialization.CODEC`, whose encoder collapses a plain,
        // unstyled, sibling-less component to a **bare string holding the
        // text verbatim** (`tryCollapseToString`) and only falls back to a
        // structural compound otherwise. Every line this server writes is a
        // plain `String` with no style at all, so the collapsed form is the
        // right — and the only correct — encoding: `Nbt::String(line)`, no
        // quoting.
        //
        // This used to write `serde_json::to_string(line)` instead, storing
        // `hello` as the seven-character string `"hello"`, on the strength
        // of a wire probe that had itself set the sign over RCON with SNBT
        // that already contained those quotes. `lodestone_world::sign_text`
        // parsed it back symmetrically, so the pair round-tripped perfectly
        // and neither half matched what a real server sends — see that
        // module's doc for the closed loop and what it cost.
        //
        // Colour/glow are not modelled (see `SignData`'s own doc for why),
        // so every side is written black and unglowing — the codec's own
        // defaults, and what `SignText::parse` falls back to for an absent
        // field anyway.
        BlockEntity::Sign(sign) => {
            let side = |lines: &[String; 4]| {
                Nbt::Compound(vec![
                    ("has_glowing_text".to_owned(), Nbt::Byte(0)),
                    ("color".to_owned(), Nbt::String("black".to_owned())),
                    (
                        "messages".to_owned(),
                        Nbt::List {
                            element_type: NbtTag::String,
                            elements: lines
                                .iter()
                                .map(|line| Nbt::String(line.clone()))
                                .collect(),
                        },
                    ),
                ])
            };
            (
                if sign.hanging {
                    "minecraft:hanging_sign"
                } else {
                    "minecraft:sign"
                },
                vec![
                    ("front_text".to_owned(), side(&sign.front)),
                    ("back_text".to_owned(), side(&sign.back)),
                    ("is_waxed".to_owned(), Nbt::Byte(i8::from(sign.waxed))),
                ],
            )
        }
        // `BeaconBlockEntity.saveAdditional`: `primary_effect`/`secondary_effect`
        // as bare strings (only written when set — `storeEffect`'s own
        // `if (effect != null)` guard), `Levels` an int. The payment slot is
        // menu-only scratch space in vanilla too (`BeaconMenu`'s own
        // `SimpleContainer`, never part of `BeaconBlockEntity`'s saved state —
        // `removed` drops it back to the player on menu close), so it is
        // deliberately not written here.
        BlockEntity::Beacon(beacon) => {
            let mut fields = vec![("Levels".to_owned(), Nbt::Int(i32::from(beacon.levels)))];
            if let Some(primary) = &beacon.primary_effect {
                fields.push(("primary_effect".to_owned(), Nbt::String(primary.clone())));
            }
            if let Some(secondary) = &beacon.secondary_effect {
                fields.push(("secondary_effect".to_owned(), Nbt::String(secondary.clone())));
            }
            ("minecraft:beacon", fields)
        }
        // `CrafterBlockEntity.saveAdditional`: `Items` (`ContainerHelper.saveAllItems`),
        // `disabled_slots` (an int array of the disabled indices —
        // `addDisabledSlots`), `triggered` (always `0`, see this variant's
        // own doc for why nothing here ever sets it). `crafting_ticks_remaining`
        // is not written: it is the auto-crafting trigger's own countdown,
        // which never starts without the trigger itself.
        BlockEntity::Crafter { slots, disabled } => (
            "minecraft:crafter",
            vec![
                ("Items".to_owned(), items_to_nbt(slots.as_slice())),
                (
                    "disabled_slots".to_owned(),
                    Nbt::IntArray(
                        disabled
                            .iter()
                            .enumerate()
                            .filter_map(|(i, &d)| d.then_some(i as i32))
                            .collect(),
                    ),
                ),
                ("triggered".to_owned(), Nbt::Int(0)),
            ],
        ),
    };

    let mut fields = vec![
        ("id".to_owned(), Nbt::String(id.to_owned())),
        ("x".to_owned(), Nbt::Int(pos.x)),
        ("y".to_owned(), Nbt::Int(pos.y)),
        ("z".to_owned(), Nbt::Int(pos.z)),
        ("keepPacked".to_owned(), Nbt::Byte(0)),
        ("components".to_owned(), Nbt::Compound(Vec::new())),
    ];
    fields.append(&mut extra);
    Nbt::Compound(fields)
}

/// The vanilla item id a [`BottleKind`] is stored as.
#[must_use]
fn bottle_item_id(kind: BottleKind) -> &'static str {
    match kind {
        BottleKind::Potion => "minecraft:potion",
        BottleKind::Splash => "minecraft:splash_potion",
        BottleKind::Lingering => "minecraft:lingering_potion",
    }
}

/// The inverse of [`bottle_item_id`], or `None` for any other item.
#[must_use]
fn bottle_kind_for_item(id: &str) -> Option<BottleKind> {
    match id {
        "minecraft:potion" => Some(BottleKind::Potion),
        "minecraft:splash_potion" => Some(BottleKind::Splash),
        "minecraft:lingering_potion" => Some(BottleKind::Lingering),
        _ => None,
    }
}

/// Decodes one block entity, or `None` for any id this crate does not
/// simulate.
///
/// **Returning `None` is the normal case, not an error.** A real world is full
/// of vaults and decorated pots this crate has no model for — chests and
/// spawners are now modelled, but the measured 1,608-of-1,613 ratio (across
/// `.cache/mc`'s worlds) that motivated `Opaque` predates that, so most block
/// entities are still an unmodelled kind. Dropping them silently is what
/// vanilla itself does with an id it cannot resolve, and it is why loading a
/// real vanilla region never fails on this path.
///
/// It is also a **real, named gap**: a chest in a world Lodestone opens and
/// re-saves loses its contents, because we drop it here and then write a chunk
/// without it. Closing that needs a passthrough for unmodelled entries, not a
/// change to this function.
fn block_entity_from_nbt(nbt: &Nbt) -> Option<(BlockPos, BlockEntity)> {
    let id = string_field(nbt, "id")?;
    let pos = BlockPos::new(
        int_field(nbt, "x")?,
        int_field(nbt, "y")?,
        int_field(nbt, "z")?,
    );

    let entity = match id {
        "minecraft:furnace" | "minecraft:smoker" | "minecraft:blast_furnace" => {
            let kind = match id {
                "minecraft:smoker" => FurnaceKind::Smoker,
                "minecraft:blast_furnace" => FurnaceKind::BlastFurnace,
                _ => FurnaceKind::Furnace,
            };
            let items = items_from_nbt(field(nbt, "Items"), 3);
            let mut recipes: HashMap<String, u32> = HashMap::new();
            if let Some(Nbt::Compound(pairs)) = field(nbt, RECIPES_USED_FIELD) {
                for (key, value) in pairs {
                    if let Nbt::Int(count) = value {
                        recipes.insert(key.clone(), (*count).max(0) as u32);
                    }
                }
            }
            BlockEntity::Furnace(Furnace::restore(
                kind,
                items[0].clone(),
                items[1].clone(),
                items[2].clone(),
                int_field(nbt, "lit_time_remaining").unwrap_or(0),
                int_field(nbt, "lit_total_time").unwrap_or(0),
                int_field(nbt, "cooking_time_spent").unwrap_or(0),
                int_field(nbt, "cooking_total_time").unwrap_or(0),
                recipes,
            ))
        }
        "minecraft:hopper" => {
            let items = items_from_nbt(field(nbt, "Items"), 5);
            let mut slots: [Option<ItemStack>; 5] = [const { None }; 5];
            for (slot, item) in slots.iter_mut().zip(items) {
                *slot = item;
            }
            BlockEntity::Hopper(Hopper::restore(
                slots,
                int_field(nbt, "TransferCooldown").unwrap_or(0),
            ))
        }
        COMPOSTER_ID => {
            let level = int_field(nbt, "level").unwrap_or(0).clamp(0, 8) as u8;
            let until = match int_field(nbt, "ticks_until_ready") {
                Some(t) if t >= 0 => Some(t as u8),
                _ => None,
            };
            BlockEntity::Composter(Composter::restore(level, until))
        }
        "minecraft:brewing_stand" => {
            let items = items_from_nbt(field(nbt, "Items"), 5);
            let potions: Vec<String> = match field(nbt, "lodestone:potions") {
                Some(Nbt::List { elements, .. }) => elements
                    .iter()
                    .map(|e| match e {
                        Nbt::String(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut bottles: [Option<Bottle>; 3] = [const { None }; 3];
            for (index, bottle) in bottles.iter_mut().enumerate() {
                let Some(stack) = items.get(index).and_then(Option::as_ref) else {
                    continue;
                };
                let Some(kind) = bottle_kind_for_item(&stack.item.to_string()) else {
                    continue;
                };
                *bottle = Some(Bottle::new(
                    kind,
                    potions.get(index).cloned().unwrap_or_default(),
                ));
            }
            let ingredient = items[3]
                .as_ref()
                .map(|s| (s.item.to_string(), s.count));
            let fuel_item = items[4].as_ref().map(|s| (s.item.to_string(), s.count));
            let brew_time = int_field(nbt, "BrewTime").unwrap_or(0);
            // Vanilla reconstructs the in-flight ingredient from slot 3
            // rather than persisting it (`BrewingStandBlockEntity.
            // loadAdditional:200-202`, `if (this.brewTime > 0) this.ingredient
            // = this.items.get(3).getItem();`). Reproduced exactly, including
            // its consequence: an ingredient swapped while the world was
            // closed is *not* detected as a mid-brew swap, in vanilla either.
            let locked = if brew_time > 0 {
                ingredient.as_ref().map(|(id, _)| id.clone())
            } else {
                None
            };
            BlockEntity::BrewingStand(BrewingStand::restore(
                bottles,
                ingredient,
                fuel_item,
                int_field(nbt, "Fuel").unwrap_or(0),
                brew_time,
                locked,
            ))
        }
        "minecraft:chest" | "minecraft:trapped_chest" | "minecraft:barrel" => {
            BlockEntity::Container {
                id: id.to_owned(),
                slots: items_from_nbt(
                    field(nbt, "Items"),
                    crate::block_entities::CONTAINER_9X3_SIZE,
                ),
            }
        }
        "minecraft:spawner" => {
            // `BaseSpawner.load`: `SpawnData` (if present) is parsed
            // unconditionally; `SpawnPotentials`, if *absent*, falls back to a
            // one-entry weighted list built from that same `SpawnData` (or a
            // fresh empty one) — not to an empty list. Reproduced exactly
            // rather than simplified, since a data-pack spawner minted with
            // only a `SpawnData` tag (no `SpawnPotentials` at all) is common.
            let spawn_data_tag = field(nbt, "SpawnData").map(spawn_data_from_nbt);
            let spawn_potentials = match field(nbt, "SpawnPotentials") {
                Some(list) => spawn_potentials_from_nbt(Some(list)),
                None => vec![WeightedSpawnData {
                    weight: 1,
                    data: spawn_data_tag.clone().unwrap_or_default(),
                }],
            };
            BlockEntity::Spawner(SpawnerState::restore(
                int_field(nbt, "Delay").unwrap_or(20),
                int_field(nbt, "MinSpawnDelay").unwrap_or(200),
                int_field(nbt, "MaxSpawnDelay").unwrap_or(800),
                int_field(nbt, "SpawnCount").unwrap_or(4),
                int_field(nbt, "MaxNearbyEntities").unwrap_or(6),
                int_field(nbt, "RequiredPlayerRange").unwrap_or(16),
                int_field(nbt, "SpawnRange").unwrap_or(4),
                spawn_potentials,
                spawn_data_tag,
            ))
        }
        // The inverse of the `BlockEntity::CommandBlock` arm above — note the
        // block-entity type `id` here is always `minecraft:command_block`
        // regardless of which of the three command-block *blocks* it came
        // from (see that arm's own comment).
        "minecraft:command_block" => {
            let track_output = int_field(nbt, "TrackOutput").unwrap_or(1) != 0;
            let update_last_execution = int_field(nbt, "UpdateLastExecution").unwrap_or(1) != 0;
            let last_execution = if update_last_execution {
                match field(nbt, "LastExecution") {
                    Some(Nbt::Long(v)) => Some(*v),
                    _ => None,
                }
            } else {
                None
            };
            BlockEntity::CommandBlock(crate::command_block::CommandBlockData {
                command: string_field(nbt, "Command").unwrap_or_default().to_owned(),
                success_count: int_field(nbt, "SuccessCount").unwrap_or(0),
                track_output,
                last_output: if track_output {
                    string_field(nbt, "LastOutput").map(str::to_owned)
                } else {
                    None
                },
                powered: int_field(nbt, "powered").unwrap_or(0) != 0,
                auto: int_field(nbt, "auto").unwrap_or(0) != 0,
                condition_met: int_field(nbt, "conditionMet").unwrap_or(0) != 0,
                update_last_execution,
                last_execution,
            })
        }
        "minecraft:beacon" => BlockEntity::Beacon(crate::block_entities::BeaconData {
            levels: int_field(nbt, "Levels").unwrap_or(0).clamp(0, 4) as u8,
            primary_effect: string_field(nbt, "primary_effect").map(str::to_owned),
            secondary_effect: string_field(nbt, "secondary_effect").map(str::to_owned),
            payment: None,
        }),
        // The inverse of the `BlockEntity::Crafter` write arm above.
        // `crafting_ticks_remaining`/`triggered` are read off disk but
        // dropped, matching that arm's own reasoning for never writing them
        // as anything but the countdown's rest state.
        "minecraft:crafter" => {
            let mut slots: [Option<ItemStack>; 9] = [None, None, None, None, None, None, None, None, None];
            for (i, item) in items_from_nbt(field(nbt, "Items"), 9).into_iter().enumerate().take(9) {
                slots[i] = item;
            }
            let mut disabled = [false; 9];
            if let Some(Nbt::IntArray(indices)) = field(nbt, "disabled_slots") {
                for &i in indices {
                    if let Some(slot) = usize::try_from(i).ok().filter(|&s| s < 9) {
                        disabled[slot] = true;
                    }
                }
            }
            BlockEntity::Crafter { slots: Box::new(slots), disabled }
        }
        _ => BlockEntity::Opaque {
            id: id.to_owned(),
            nbt: nbt.clone(),
        },
    };
    Some((pos, entity))
}

/// Reads a chunk's block entities and pending ticks back out of its NBT tree.
///
/// Total and non-failing by design: an entry this crate cannot understand is
/// skipped, never an error, so a chunk written by a real server always loads.
/// See [`block_entity_from_nbt`] for what that costs and what would close it.
#[must_use]
pub fn extras_from_nbt(nbt: &Nbt) -> ChunkExtras {
    let list = |key: &str| -> &[Nbt] {
        match field(nbt, key) {
            Some(Nbt::List { elements, .. }) => elements.as_slice(),
            _ => &[],
        }
    };
    ChunkExtras {
        block_entities: list("block_entities")
            .iter()
            .filter_map(block_entity_from_nbt)
            .collect(),
        block_ticks: list("block_ticks")
            .iter()
            .filter_map(saved_tick_from_nbt)
            .collect(),
        fluid_ticks: list("fluid_ticks")
            .iter()
            .filter_map(saved_tick_from_nbt)
            .collect(),
    }
}

/// Counter-based proof for the load-path defect this module used to have
/// (issue #510): `column_from_nbt` called `ChunkColumn::set_block` once per
/// cell — 98,304 per column — each a linear scan of the *column-wide* palette.
/// `ChunkColumn::set_section_from_local_palette` interns a section's own
/// (small) local palette once per distinct state instead.
///
/// Uses the same real, vanilla-written region file
/// `tests/chunk_nbt_vanilla_oracle.rs` uses as its outside source — a fixture
/// this crate's own encoder produced could not tell a real defect from a
/// self-consistent misunderstanding of it. `#[ignore]`d for the same reason
/// that oracle is: it requires `.cache/mc/survival/world`, which is not repo
/// state.
/// [`block_entity_to_nbt`]'s `Sign` arm, round-tripped through the real
/// client-side decoder (`lodestone_world::sign_text::SignText::parse`).
///
/// **Read what this does and does not prove.** Two different agents wrote
/// the two halves, but both worked from the *same* wire probe, and that
/// probe was wrong about the element encoding — so for a while this gate
/// was green while neither half matched a real server, which is the closed
/// `decode(encode(x)) == x` loop this repo's evidence standards forbid.
/// What settles the encoding is outside both: `SignText.LINES_CODEC` is
/// `ComponentSerialization.CODEC.listOf()`, whose encoder collapses an
/// unstyled component to a bare string. This round trip is still worth
/// keeping — it catches a field-name or front/back transposition — but it
/// is a consistency check, not evidence about the wire.
#[cfg(test)]
mod sign_nbt_tests {
    use lodestone_world::{SignDyeColor, SignText};

    use super::block_entity_to_nbt;
    use crate::block_entities::{BlockEntity, SignData};
    use lodestone_model::BlockPos;

    /// A round trip through the real client parser: two distinct lines on
    /// the front (pairwise-distinct from the back's own placeholder, so a
    /// front/back transposition cannot survive), the back left blank, waxed
    /// set — every field this arm writes, read back by the type that will
    /// actually render it.
    #[test]
    fn a_signs_nbt_round_trips_through_the_real_client_decoder() {
        let sign = SignData {
            front: ["LODESTONE".to_owned(), "PROBE".to_owned(), String::new(), String::new()],
            back: Default::default(),
            waxed: true,
            editor: None,
            hanging: false,
        };
        let nbt = block_entity_to_nbt(BlockPos::new(1, 65, 1), &BlockEntity::Sign(sign));

        // `SignSide::lines` carries styled spans now, so compare on the
        // flattened plain text: this gate is about the NBT round trip, not
        // about span structure.
        let plain = |line: &[lodestone_world::SignTextSpan]| -> String {
            line.iter().map(|s| s.text.as_str()).collect()
        };

        let text = SignText::parse(&nbt);
        assert_eq!(plain(&text.front.lines[0]), "LODESTONE");
        assert_eq!(plain(&text.front.lines[1]), "PROBE");
        assert_eq!(plain(&text.front.lines[2]), "");
        assert_eq!(text.front.color, SignDyeColor::Black);
        assert!(!text.front.glowing);
        assert_eq!(
            text.back.lines.each_ref().map(|l| plain(l)),
            ["", "", "", ""],
            "the back side must stay blank"
        );
        assert!(text.waxed, "the waxed flag must survive the round trip");

        // Control: an unwaxed sign must decode as unwaxed — proves the
        // `true` above is really carrying the field, not a decoder that
        // always reads waxed.
        let unwaxed = block_entity_to_nbt(
            BlockPos::new(1, 65, 1),
            &BlockEntity::Sign(SignData::default()),
        );
        assert!(!SignText::parse(&unwaxed).waxed);
    }

    /// A hanging sign gets the distinct `minecraft:hanging_sign` id — read
    /// straight off the encoded `id` field, since [`SignText::parse`] itself
    /// does not distinguish the two (it parses the same `front_text`/
    /// `back_text`/`is_waxed` shape either way).
    #[test]
    fn a_hanging_sign_encodes_its_own_block_entity_type() {
        let nbt = block_entity_to_nbt(
            BlockPos::new(0, 70, 0),
            &BlockEntity::Sign(SignData { hanging: true, ..SignData::default() }),
        );
        let lodestone_core::Nbt::Compound(fields) = &nbt else {
            panic!("must be a compound")
        };
        let id = fields
            .iter()
            .find(|(name, _)| name == "id")
            .map(|(_, value)| value.clone());
        assert_eq!(id, Some(lodestone_core::Nbt::String("minecraft:hanging_sign".to_owned())));
    }
}

#[cfg(test)]
mod beacon_nbt_tests {
    use super::{block_entity_from_nbt, block_entity_to_nbt};
    use crate::block_entities::{BeaconData, BlockEntity};
    use lodestone_model::BlockPos;

    /// A round trip through this file's own decoder — no independent second
    /// implementation exists for beacon NBT elsewhere in this crate (unlike
    /// the sign arm beside this one, which validates against
    /// `lodestone_world::SignText::parse`), so this is `decode(encode(x)) ==
    /// x` plus a direct field-shape assertion below, not a two-implementation
    /// join. Primary and secondary are pairwise-distinct effects and levels
    /// is a non-round, non-zero, non-default value (`3`), so a transposition
    /// or a stuck-at-default bug cannot survive unnoticed.
    #[test]
    fn a_beacons_nbt_round_trips_through_this_files_own_decoder() {
        let beacon = BeaconData {
            levels: 3,
            primary_effect: Some("minecraft:strength".to_owned()),
            secondary_effect: Some("minecraft:regeneration".to_owned()),
            payment: None,
        };
        let nbt = block_entity_to_nbt(BlockPos::new(4, 70, -2), &BlockEntity::Beacon(beacon.clone()));
        let (pos, decoded) = block_entity_from_nbt(&nbt).expect("must decode");
        assert_eq!(pos, BlockPos::new(4, 70, -2));
        assert_eq!(decoded, BlockEntity::Beacon(beacon));
    }

    /// **Control**: an unset secondary must decode back to `None`, not a
    /// stale or default effect string — without this, an encoder that always
    /// wrote *some* secondary field would still pass the round trip above,
    /// since that fixture's own secondary happens to be `Some`.
    #[test]
    fn an_unset_secondary_effect_round_trips_to_none() {
        let beacon = BeaconData {
            levels: 1,
            primary_effect: Some("minecraft:speed".to_owned()),
            secondary_effect: None,
            payment: None,
        };
        let nbt = block_entity_to_nbt(BlockPos::new(0, 64, 0), &BlockEntity::Beacon(beacon.clone()));
        let (_, decoded) = block_entity_from_nbt(&nbt).expect("must decode");
        assert_eq!(decoded, BlockEntity::Beacon(beacon));
    }

    /// The `id` field must name the beacon's own block-entity type, the same
    /// direct field-shape assertion `hanging_sign_nbt_names_its_own_block_entity_type`
    /// makes for the sign arm beside this one.
    #[test]
    fn beacon_nbt_names_its_own_block_entity_type() {
        let nbt = block_entity_to_nbt(BlockPos::new(0, 64, 0), &BlockEntity::Beacon(BeaconData::default()));
        let lodestone_core::Nbt::Compound(fields) = &nbt else {
            panic!("must be a compound")
        };
        let id = fields
            .iter()
            .find(|(name, _)| name == "id")
            .map(|(_, value)| value.clone());
        assert_eq!(id, Some(lodestone_core::Nbt::String("minecraft:beacon".to_owned())));
    }
}

#[cfg(test)]
mod crafter_nbt_tests {
    use super::{block_entity_from_nbt, block_entity_to_nbt};
    use crate::block_entities::BlockEntity;
    use lodestone_model::{BlockPos, ItemStack};

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    /// A round trip through this file's own encoder/decoder, with a real item
    /// in one slot and two *non-adjacent* disabled indices (`1` and `6`) —
    /// adjacent or coincident indices would not catch an off-by-one in
    /// `disabled_slots`' int-array encoding the way spread-out ones do.
    #[test]
    fn a_crafters_nbt_round_trips_items_and_disabled_slots() {
        let mut crafter = BlockEntity::crafter();
        crafter.set_container_slot(0, Some(stack("minecraft:redstone", 3)));
        assert!(crafter.set_crafter_slot_state(1, false));
        assert!(crafter.set_crafter_slot_state(6, false));

        let nbt = block_entity_to_nbt(BlockPos::new(4, 70, -2), &crafter);
        let (pos, decoded) = block_entity_from_nbt(&nbt).expect("must decode");
        assert_eq!(pos, BlockPos::new(4, 70, -2));
        assert_eq!(decoded, crafter);
    }

    /// **Control**: a crafter with *no* disabled slots must decode back to
    /// all-enabled, not stuck reading the fixture above's `disabled_slots` —
    /// without this, an encoder/decoder pair that always marked every slot
    /// disabled would still pass the round trip above by coincidence.
    #[test]
    fn a_crafter_with_no_disabled_slots_round_trips_to_all_enabled() {
        let crafter = BlockEntity::crafter();
        let nbt = block_entity_to_nbt(BlockPos::new(0, 64, 0), &crafter);
        let (_, decoded) = block_entity_from_nbt(&nbt).expect("must decode");
        assert_eq!(decoded, crafter);
        assert_eq!(decoded.data_properties(), vec![0; 10]);
    }

    /// The `id` field must name the crafter's own block-entity type, the same
    /// direct field-shape assertion the beacon/sign arms elsewhere in this
    /// file make.
    #[test]
    fn crafter_nbt_names_its_own_block_entity_type() {
        let nbt = block_entity_to_nbt(BlockPos::new(0, 64, 0), &BlockEntity::crafter());
        let lodestone_core::Nbt::Compound(fields) = &nbt else {
            panic!("must be a compound")
        };
        let id = fields
            .iter()
            .find(|(name, _)| name == "id")
            .map(|(_, value)| value.clone());
        assert_eq!(id, Some(lodestone_core::Nbt::String("minecraft:crafter".to_owned())));
    }
}

#[cfg(test)]
mod intern_bound_tests {
    use std::path::{Path, PathBuf};

    use lodestone_core::{Reader, read_named_nbt};

    use super::column_from_nbt;
    use crate::chunk::{intern_calls, reset_intern_calls};

    fn region_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../.cache/mc/survival/world/dimensions/minecraft/overworld/region/r.0.0.mca",
        )
    }

    /// A real column's `intern` calls must be bounded by its distinct states
    /// per section — never by its cell count, 98,304 — which is the exact
    /// gate issue #510 names. The second assertion is the control: run
    /// against the pre-fix cell-by-cell path (`set_block` called once per
    /// cell) and it fails, because `calls == 98_304` for every populated
    /// column regardless of how few distinct states it holds.
    #[test]
    #[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
    fn loading_a_real_column_interns_by_distinct_state_not_by_cell() {
        let bytes = std::fs::read(region_path()).expect("read the real region file");
        let region = lodestone_anvil::region::RegionFile::parse(&bytes).expect("parse region");

        const MIN_Y: i32 = -64;
        const HEIGHT: i32 = 384;
        const CELLS_PER_COLUMN: u64 = 16 * 16 * 384;

        let mut columns_checked = 0usize;
        let mut total_calls = 0u64;
        let mut max_calls = 0u64;
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let Some(raw) = region
                    .read_chunk_nbt_bytes(local_x, local_z)
                    .expect("read chunk")
                else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                let (_, nbt) = read_named_nbt(&mut reader).expect("decode chunk nbt");

                reset_intern_calls();
                let column = column_from_nbt(&nbt, MIN_Y, HEIGHT).expect("decode column");
                let calls = intern_calls();

                let distinct_states = column.raw_palette().len() as u64;
                let sections = column.section_count() as u64;
                let bound = distinct_states * sections;

                assert!(
                    calls <= bound,
                    "chunk ({local_x}, {local_z}): {calls} intern calls exceeds the \
                     distinct-states-per-section bound ({distinct_states} states x {sections} \
                     sections = {bound}) — the load path is scanning per cell again"
                );
                assert!(
                    calls < CELLS_PER_COLUMN,
                    "chunk ({local_x}, {local_z}): {calls} intern calls is not below the old \
                     per-cell cost ({CELLS_PER_COLUMN}); this is the control arm and it must fail \
                     under the pre-fix cell-by-cell path, where calls == {CELLS_PER_COLUMN} for \
                     every populated column"
                );
                total_calls += calls;
                max_calls = max_calls.max(calls);
                columns_checked += 1;
            }
        }
        assert!(
            columns_checked > 100,
            "expected a populated real region; found {columns_checked} columns"
        );
        eprintln!(
            "intern calls: {columns_checked} columns, mean {:.1}, max {max_calls}, old per-column \
             cost would have been {CELLS_PER_COLUMN}",
            total_calls as f64 / columns_checked as f64
        );
    }
}
