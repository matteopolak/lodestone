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
//! # Configuration
//!
//! None. [`DATA_VERSION`] is a constant, not a setting.
//!
//! # Dependencies
//!
//! `lodestone-core` for the `Nbt` tree, and [`crate::chunk::ChunkColumn`]. No
//! filesystem access — this module is pure tree-to-struct, which is what lets
//! it be tested against bytes a real server wrote without any I/O harness.

use lodestone_core::{Nbt, NbtTag};

use crate::chunk::ChunkColumn;

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

/// Encodes a column as the chunk NBT tree a 26.2 region file holds.
///
/// Writes `Status = "minecraft:full"` (the one field vanilla treats as
/// mandatory) and omits `Heightmaps` so vanilla re-primes them — see this
/// module's doc comment for why writing them would be worse than omitting them.
#[must_use]
pub fn column_to_nbt(cx: i32, cz: i32, column: &ChunkColumn) -> Nbt {
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
            Nbt::List {
                element_type: NbtTag::End,
                elements: Vec::new(),
            },
        ),
        (
            "block_ticks".to_owned(),
            Nbt::List {
                element_type: NbtTag::End,
                elements: Vec::new(),
            },
        ),
        (
            "fluid_ticks".to_owned(),
            Nbt::List {
                element_type: NbtTag::End,
                elements: Vec::new(),
            },
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
