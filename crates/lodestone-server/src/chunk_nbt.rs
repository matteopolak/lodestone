//! The chunk *schema*: `ChunkColumn` ↔ the NBT tree an Anvil region file holds
//! (issue [#437](https://github.com/matteopolak/lodestone/issues/437)).
//!
//! # What it is
//!
//! `lodestone-anvil` deliberately stops at the *container* — it hands back "an
//! arbitrary NBT blob at a given chunk coordinate" and parses no chunk schema
//! at all (its own module doc says so, and issue #298 names the separation as a
//! trap to preserve). This module is the other half: the mapping between that
//! blob and [`crate::chunk::ChunkColumn`], i.e. `SerializableChunkData.java`'s
//! territory, which issue #437 is where it "gets decided".
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
//!   heightmap missing from the file — `SerializableChunkData.java` lines
//!   291–302, `Heightmap.primeHeightmaps(chunk, toPrime)` — so omitting them
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

use crate::block_entities::BlockEntity;
use crate::brewing::{Bottle, BottleKind, BrewingStand};
use crate::chunk::ChunkColumn;
use crate::composter::Composter;
use crate::furnace::{Furnace, FurnaceKind};
use crate::hopper::Hopper;
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
    let blocks = column.raw_blocks();

    let mut sections = Vec::with_capacity(section_count);
    for s in 0..section_count {
        let base = s * SECTION_VOLUME;
        let cells = &blocks[base..(base + SECTION_VOLUME).min(blocks.len())];

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

        // Biomes are one climate sample per horizontal quart, constant in y
        // (issue #405 — see `ChunkColumn::biome_state`), so all four y-layers
        // of the 4×4×4 grid repeat the same 16 values.
        let quarts = column.biome_quarts();
        let mut biome_local: Vec<&str> = Vec::new();
        let mut biome_indices = Vec::with_capacity(BIOME_CELLS);
        for cell in 0..BIOME_CELLS {
            let quart = cell % 16;
            let name = quarts[quart].as_str();
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
        (
            "structures".to_owned(),
            Nbt::Compound(vec![
                ("References".to_owned(), Nbt::Compound(Vec::new())),
                ("starts".to_owned(), Nbt::Compound(Vec::new())),
            ]),
        ),
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
        for (cell, &index) in indices.iter().enumerate() {
            let index = index as usize;
            let Some(state) = states.get(index) else {
                return Err(Error::PaletteIndexOutOfRange {
                    index,
                    len: states.len(),
                    y: i32::from(y),
                });
            };
            // The section's own `(y << 8) | (z << 4) | x` order, which is
            // `ChunkColumn`'s layout restricted to one 16-row window.
            let ly = cell >> 8;
            let lz = (cell >> 4) & 15;
            let lx = cell & 15;
            column.set_block(
                lx as i32,
                y_base + ly as i32,
                lz as i32,
                state,
            );
        }

        // Biomes: take the y=0 layer of the lowest section that carries one,
        // matching this column type's one-sample-per-quart model.
        if section_index == 0
            && let Some(biomes) = field(section, "biomes")
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
            let mut quarts: Vec<String> = Vec::with_capacity(16);
            for quart in 0..16 {
                let index = cells[quart] as usize;
                quarts.push(names.get(index).cloned().unwrap_or_default());
            }
            column.set_biome_quarts(&quarts);
        }
    }

    Ok(column)
}

// ---------------------------------------------------------------------------
// Block entities and scheduled ticks (issue #468's remaining half)
// ---------------------------------------------------------------------------

/// One pending scheduled tick as it sits **on disk**, mirroring vanilla's
/// `net.minecraft.world.ticks.SavedTick` record
/// (`SavedTick.java:13`, `record SavedTick<T>(T type, BlockPos pos, int delay,
/// TickPriority priority)`).
///
/// Deliberately a different type from [`crate::scheduled_tick::ScheduledTick`],
/// exactly as it is in the jar, because the two disagree about the one field
/// that matters: a live tick carries an **absolute** `trigger_tick`, a saved
/// one carries a **relative, signed** `delay`. Vanilla converts with
/// `SavedTick::unpack` (`SavedTick.java:52`):
///
/// ```text
/// return new ScheduledTick<>(this.type, this.pos, currentTick + this.delay, this.priority, currentSubTick);
/// ```
///
/// so a load is `trigger_tick = game_time_at_load + delay` and **`delay` is
/// routinely negative** — see [`Self::delay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTick {
    /// Absolute block position.
    pub pos: (i32, i32, i32),
    /// The block or fluid id being ticked — vanilla's `i` field.
    pub kind: String,
    /// Ticks from the game time **at save** until this tick is due, vanilla's
    /// `t`.
    ///
    /// **Signed, and negative in real worlds.** Measured across 22,488 real
    /// vanilla chunks with an independent parser: 1,584 of 133,051 saved ticks
    /// carry a negative delay, the extreme being `-1046` for an overdue birch
    /// leaves decay and `-33` for an overdue lava tick. A world is saved
    /// mid-tick with a backlog and vanilla simply records how overdue each
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
/// `TickPriority.CODEC` is `Codec.INT.xmap(TickPriority::byValue,
/// TickPriority::getValue)` (`TickPriority.java:14`), so the int written is
/// `getValue()`. Our [`TickPriority`] is declared in vanilla's order *on
/// purpose*, so that `#[derive(Ord)]` reproduces Java's ordinal-based
/// `compareTo` for free — which makes `Normal`'s **ordinal 3** and its
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
/// exactly as `TickPriority.byValue` does (`TickPriority.java:21-29`: below
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

/// Encodes one block entity as the chunk NBT list element vanilla holds.
///
/// The `x`/`y`/`z` are **absolute** and `keepPacked` is `0`, both matching
/// every real entry measured. `components` is written as an empty compound
/// because vanilla writes one unconditionally and this crate models no
/// block-entity components.
#[must_use]
fn block_entity_to_nbt(pos: BlockPos, entity: &BlockEntity) -> Nbt {
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
/// of chests, vaults, spawners and decorated pots this crate has no model for
/// — 1,608 of the 1,613 block entities measured across `.cache/mc`'s worlds
/// are kinds we do not simulate. Dropping them silently is what vanilla itself
/// does with an id it cannot resolve, and it is why loading a real vanilla
/// region never fails on this path.
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
