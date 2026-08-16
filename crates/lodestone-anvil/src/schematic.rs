//! Third-party schematic/structure formats: Litematica's `.litematic`,
//! Sponge's `.schem`, and vanilla's own structure-block `.nbt`.
//!
//! ## What it is
//!
//! A reader for the three schematic containers a real, publicly-downloaded
//! redstone build is likely to arrive in, each turned into the same flat
//! output: a list of non-air `(x, y, z, canonical_block_state)` placements in
//! the schematic's own local coordinate space. Nothing here knows about a
//! live world, a chunk, or a tick loop — this module's whole job stops at
//! "what does the file say was here", matching this crate's existing rule
//! (see the crate root doc) that container parsing and in-memory world schema
//! are two different problems. A caller (a benchmark harness, a world editor)
//! decides where the placements land and what writes them.
//!
//! ## How it works
//!
//! All three formats are gzip-wrapped named NBT — detected by magic bytes
//! (`1f 8b`), not by trusting the file extension, the same sniff
//! `lodestone_worldgen::structure::template::StructureTemplate::parse`
//! already uses for vanilla's own `.nbt` — decoded with this crate's shared
//! [`lodestone_core::read_named_nbt`] codec, then walked by hand into
//! [`Schematic`]:
//!
//! - **Litematica (`.litematic`)**: `Regions` is a compound of named regions,
//!   each with `Position`, `Size` (any axis may be negative, meaning the
//!   region extends in that direction from `Position`), a
//!   `BlockStatePalette` list of `{Name, Properties?}` compounds, and a
//!   `BlockStates` `LongArray`. The long array is **dense/spanning**: entry
//!   `i` occupies bits `[i*bits, (i+1)*bits)` of a single flat bitstream and
//!   *can* straddle two longs — unlike this repo's own 26.2 chunk-section
//!   packing (`lodestone_server::chunk_nbt`'s `unpack_indices`, deliberately
//!   **non**-spanning since 1.16). Confirmed against a real downloaded file
//!   rather than assumed: a 20×154×12 region with a 94-entry palette needs
//!   `bits = 7` (`ceil(log2(94))`, floored at 2), and `36960 * 7 = 258720`
//!   bits packs into `ceil(258720 / 64) = 4043` longs — exactly the long
//!   count the file carries. The non-spanning formula would need 4107. See
//!   [`read_dense_entry`].
//! - **Sponge Schematic (`.schem`, versions 1–3)**: `Palette` is a compound
//!   mapping an already-canonical state string directly to its integer id —
//!   no `Name`/`Properties` reconstruction needed, unlike the other two
//!   formats — and `BlockData` (`Data` under a nested `Blocks` compound in
//!   v3) is a byte array of LEB128 varints, one per cell, each a palette id.
//! - **Vanilla structure (`.nbt`)**: `size`, a `palette` list (or the first
//!   of a `palettes` list, for a multi-variant structure), and a `blocks`
//!   list of `{pos, state}` entries — `state` an index into the palette.
//!   Simplest of the three: blocks carry explicit positions, no bit-packing.
//!   Same schema `StructureTemplate::parse` already reads in
//!   `lodestone-worldgen` (read there to confirm the field names below
//!   rather than guessed).
//!
//! Property order is never reconstructed to match vanilla's own canonical
//! `toString` — `lodestone_server::redstone`'s state parsers scan
//! `key=value` pairs split on `,`, order-independent (see
//! `crate::redstone::own_signal` and friends), so this module only needs a
//! `BTreeMap` for determinism, not for correctness.
//!
//! ## How to change it, and the gotchas
//!
//! - **The legacy MCEdit `.schematic` format is a different, older thing and
//!   is not supported.** It predates the 1.13 flattening (numeric block
//!   id + damage-value `Blocks`/`Data` byte arrays, no block-state strings at
//!   all), so a translation table this crate would have to own and keep in
//!   sync with `lodestone-canonical`'s pre-1.13 flattening — out of scope
//!   here. [`detect_format`] returns `None` for a `.schematic` extension
//!   rather than silently mis-parsing it as a Sponge `.schem`, whose
//!   extension it is easy to confuse at a glance.
//! - **Air is dropped, always.** `minecraft:air`/`cave_air`/`void_air`
//!   entries never reach [`Schematic::blocks`] — a caller stamping a
//!   contraption into an existing world should not need to special-case
//!   "wrote air over what was already there" for every one of a schematic's
//!   often-mostly-empty bounding box.
//! - **A schematic's own declared size/block-count fields are not
//!   trustworthy on their own** — [`Schematic::size`] and
//!   [`Schematic::reported_total_blocks`] are kept for *reporting*
//!   (comparing what the file claims against what this parser actually
//!   placed), never substituted for `blocks.len()`.
//!
//! ## Configuration
//!
//! None.
//!
//! ## Dependencies
//!
//! `lodestone-core` (the shared NBT codec) and `flate2` (gzip), both already
//! this crate's dependencies — no new one added for this module.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use lodestone_core::{Nbt, NbtTag, Reader, read_named_nbt};

use crate::{Error, Result};

/// One non-air placement recovered from a schematic file, in the
/// schematic's own local coordinate space — region/schematic origin at
/// `(0, 0, 0)`, independent of wherever a caller later stamps it into a
/// live world.
#[derive(Debug, Clone)]
pub struct SchematicBlock {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// A canonical `name` or `name[key=value,...]` block-state string — the
    /// same shape `lodestone_server::chunk::ChunkColumn::set_block` and
    /// `ChunkSource::set_block` take, and the same shape
    /// `lodestone_server::redstone::own_signal` parses back out. Property
    /// order is alphabetical (a [`BTreeMap`] internally) but that is only for
    /// determinism — see this module's own doc for why order does not affect
    /// correctness here.
    pub state: String,
}

/// One `PendingBlockTicks` entry recovered from a Litematica region — a
/// captured mid-cycle scheduled tick (a repeater between its own delay and
/// firing, a lit fire block waiting to spread), in the schematic's own local
/// coordinate space. `docs/redstone-benchmark-harness.md`'s own findings
/// record why this matters: a schematic stamped with
/// `ChunkSource::set_block` alone carries no perturbation, so a contraption's
/// *ongoing* redstone cost reads as zero regardless of the engine's real
/// per-notification cost — re-injecting these is what lets a caller resume a
/// captured circuit instead of only measuring an inert one.
///
/// Only `PendingBlockTicks` is read here, not `PendingFluidTicks` — every
/// fixture this parser has been checked against (`docs/redstone-benchmark-harness.md`'s
/// provenance table) carries an empty fluid-tick list, so there is no real
/// example to validate a fluid-tick reader against yet; add one when a
/// fixture actually needs it rather than guessing the schema unchecked.
#[derive(Debug, Clone)]
pub struct SchematicPendingTick {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// The scheduled block's own name (e.g. `"minecraft:repeater"`), **not**
    /// a full state string — Litematica's own `PendingBlockTicks` entries
    /// carry only `Block`, not the state the block was in when scheduled.
    pub block: String,
    /// Ticks remaining until this tick was due to fire, relative to the
    /// moment the region was captured — Litematica's own `Time` field,
    /// already relative in the source file (not an absolute world tick, so
    /// no capture-time base is needed to reinterpret it).
    pub time: i64,
    /// Vanilla's `TickPriority` ordinal (`EXTREMELY_HIGH = -3` .. `EXTREMELY_LOW = 3`,
    /// `NORMAL = 0`) — Litematica's own `Priority` field, copied verbatim
    /// from vanilla's `ScheduledTick` NBT schema. A caller mapping this onto
    /// `lodestone_server::TickPriority` needs the ordinal-to-variant table;
    /// this module does not depend on `lodestone-server` so it hands back
    /// the raw number rather than guessing a mapping here.
    pub priority: i32,
}

/// Which of the three schematic containers a file was parsed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchematicFormat {
    Litematica,
    SpongeSchematic,
    VanillaStructure,
}

/// The result of parsing one schematic file.
#[derive(Debug, Clone)]
pub struct Schematic {
    pub format: SchematicFormat,
    /// The file's own declared bounding size, `(width, height, length)` —
    /// kept for reporting only; see this module's doc for why it is not a
    /// substitute for measuring [`Schematic::blocks`] directly. `(0, 0, 0)`
    /// when the format/file did not declare one anywhere this parser looked.
    pub size: (i32, i32, i32),
    /// Every non-air block this parser found, already filtered of air.
    pub blocks: Vec<SchematicBlock>,
    /// A human-readable name, when the file's own metadata names one
    /// (Litematica's `Metadata.Name`).
    pub name: Option<String>,
    /// The build's credited author, when the file's own metadata names one
    /// (Litematica's `Metadata.Author`).
    pub author: Option<String>,
    /// The file's own claimed non-air block count (Litematica's
    /// `Metadata.TotalBlocks`), when present. A cross-check against
    /// `blocks.len()`, not a substitute for it.
    pub reported_total_blocks: Option<i64>,
    /// Every `PendingBlockTicks` entry from every region, in the schematic's
    /// own local coordinate space — see [`SchematicPendingTick`]. Empty for
    /// every format but Litematica (Sponge and vanilla-structure `.nbt` do
    /// not carry this data in a shape this parser reads) and for a
    /// Litematica file whose regions were all captured fully settled.
    pub pending_block_ticks: Vec<SchematicPendingTick>,
}

/// Picks a [`SchematicFormat`] from a path's extension. Returns `None` for
/// an unrecognised extension, **including** the legacy MCEdit `.schematic`
/// format — see this module's doc for why that one is deliberately refused
/// rather than mis-parsed.
#[must_use]
pub fn detect_format(path: &Path) -> Option<SchematicFormat> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("litematic") => Some(SchematicFormat::Litematica),
        Some("schem") => Some(SchematicFormat::SpongeSchematic),
        Some("nbt") => Some(SchematicFormat::VanillaStructure),
        _ => None,
    }
}

/// Reads and parses `path` as a schematic file, picking the format from its
/// extension via [`detect_format`].
///
/// # Errors
///
/// [`Error::SchematicUnknownFormat`] for an unrecognised extension,
/// [`Error::Io`]/[`Error::Nbt`] for a read or NBT-decode failure, and
/// [`Error::SchematicMalformed`] for a file whose NBT decodes but is missing
/// a field this format requires.
pub fn load_schematic_file(path: &Path) -> Result<Schematic> {
    let format = detect_format(path)
        .ok_or_else(|| Error::SchematicUnknownFormat(path.display().to_string()))?;
    let bytes = std::fs::read(path).map_err(Error::Io)?;
    load_schematic_bytes(format, &bytes)
}

/// Parses `bytes` (a whole schematic file's contents, gzip-wrapped as every
/// real one ships, or bare NBT) as `format`.
pub fn load_schematic_bytes(format: SchematicFormat, bytes: &[u8]) -> Result<Schematic> {
    let decoded = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(Error::Io)?;
        out
    } else {
        bytes.to_vec()
    };
    let mut reader = Reader::new(&decoded);
    let (_, root_nbt) = read_named_nbt(&mut reader).map_err(Error::Nbt)?;
    let root =
        compound(&root_nbt).ok_or_else(|| Error::SchematicMalformed("<root compound>".into()))?;

    match format {
        SchematicFormat::Litematica => parse_litematica(root),
        SchematicFormat::SpongeSchematic => parse_sponge_schematic(root),
        SchematicFormat::VanillaStructure => parse_vanilla_structure(root),
    }
}

// ---------------------------------------------------------------------
// Shared NBT navigation helpers
// ---------------------------------------------------------------------

fn compound(value: &Nbt) -> Option<&[(String, Nbt)]> {
    match value {
        Nbt::Compound(fields) => Some(fields),
        _ => None,
    }
}

fn field<'a>(fields: &'a [(String, Nbt)], name: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value)
}

fn as_list(value: &Nbt) -> Option<&[Nbt]> {
    match value {
        Nbt::List { elements, .. } => Some(elements),
        _ => None,
    }
}

fn as_str(value: &Nbt) -> Option<&str> {
    match value {
        Nbt::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Any NBT integer type widened to `i32` — schematic writers are
/// inconsistent about whether a small field (`Width`, a coordinate) is a
/// `Byte`, `Short`, or `Int`.
fn as_i32(value: &Nbt) -> Option<i32> {
    match *value {
        Nbt::Byte(v) => Some(i32::from(v)),
        Nbt::Short(v) => Some(i32::from(v)),
        Nbt::Int(v) => Some(v),
        Nbt::Long(v) => i32::try_from(v).ok(),
        _ => None,
    }
}

fn as_i64(value: &Nbt) -> Option<i64> {
    match *value {
        Nbt::Byte(v) => Some(i64::from(v)),
        Nbt::Short(v) => Some(i64::from(v)),
        Nbt::Int(v) => Some(i64::from(v)),
        Nbt::Long(v) => Some(v),
        _ => None,
    }
}

/// An `{x, y, z}` compound of integers, as Litematica's `Position`/`Size`
/// use — distinct from the `[x, y, z]` list/array form vanilla structures use
/// ([`int_triple`]).
fn xyz_compound(fields: &[(String, Nbt)]) -> Option<(i32, i32, i32)> {
    let c = fields;
    Some((
        field(c, "x").and_then(as_i32)?,
        field(c, "y").and_then(as_i32)?,
        field(c, "z").and_then(as_i32)?,
    ))
}

/// A `[x, y, z]` list or int-array of three integers, as vanilla structure
/// `size`/`pos` fields and Sponge `Offset` use.
fn int_triple(value: &Nbt) -> Option<(i32, i32, i32)> {
    match value {
        Nbt::List { elements, .. } if elements.len() >= 3 => Some((
            as_i32(&elements[0])?,
            as_i32(&elements[1])?,
            as_i32(&elements[2])?,
        )),
        Nbt::IntArray(values) if values.len() >= 3 => Some((values[0], values[1], values[2])),
        _ => None,
    }
}

fn format_state(name: &str, properties: &BTreeMap<String, String>) -> String {
    if properties.is_empty() {
        return name.to_owned();
    }
    let body = properties
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}[{body}]")
}

/// A `{Name, Properties?}` compound (Litematica's `BlockStatePalette` entries
/// and vanilla structure `palette` entries share this exact shape) turned
/// into a canonical state string.
fn name_properties_entry_to_state(entry: &Nbt) -> String {
    let Some(c) = compound(entry) else {
        return "minecraft:air".to_owned();
    };
    let name = field(c, "Name").and_then(as_str).unwrap_or("minecraft:air");
    let mut properties = BTreeMap::new();
    if let Some(Nbt::Compound(fields)) = field(c, "Properties") {
        for (key, value) in fields {
            if let Nbt::String(v) = value {
                properties.insert(key.clone(), v.clone());
            }
        }
    }
    format_state(name, &properties)
}

/// Whether a canonical state string names one of the three air variants —
/// stripped of any `[...]` body first, though air carries none in practice.
fn is_air(state: &str) -> bool {
    let name = state.split('[').next().unwrap_or(state);
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

// ---------------------------------------------------------------------
// Litematica dense/spanning bit array
// ---------------------------------------------------------------------

/// `bitsPerEntry = max(2, ceil(log2(paletteLen)))` — `LitematicaBitArray`'s
/// own floor, confirmed by the worked example in this module's doc comment.
fn litematica_bits_for(palette_len: usize) -> u32 {
    if palette_len <= 1 {
        return 2;
    }
    let ceil_log2 = usize::BITS - (palette_len - 1).leading_zeros();
    ceil_log2.max(2)
}

/// Reads dense/spanning entry `index` (each `bits` wide, no per-long
/// padding — entries may straddle two longs) out of `data`. Mirrors
/// `LitematicaBitArray.getAt`, **not**
/// `lodestone_server::chunk_nbt`'s non-spanning `unpack_indices`; see this
/// module's doc comment for the measured difference between the two.
fn read_dense_entry(data: &[i64], index: usize, bits: u32) -> Option<u64> {
    let bit_index = (index as u64) * u64::from(bits);
    let start_long = (bit_index / 64) as usize;
    let start_offset = (bit_index % 64) as u32;
    let end_long = (((index as u64 + 1) * u64::from(bits) - 1) / 64) as usize;
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };

    let start_word = *data.get(start_long)? as u64;
    if start_long == end_long {
        Some((start_word >> start_offset) & mask)
    } else {
        let end_word = *data.get(end_long)? as u64;
        let end_offset = 64 - start_offset;
        Some(((start_word >> start_offset) | (end_word << end_offset)) & mask)
    }
}

// ---------------------------------------------------------------------
// Litematica
// ---------------------------------------------------------------------

fn parse_litematica(root: &[(String, Nbt)]) -> Result<Schematic> {
    let metadata = field(root, "Metadata").and_then(compound);
    let name = metadata
        .and_then(|m| field(m, "Name"))
        .and_then(as_str)
        .map(str::to_owned);
    let author = metadata
        .and_then(|m| field(m, "Author"))
        .and_then(as_str)
        .map(str::to_owned);
    let reported_total_blocks = metadata
        .and_then(|m| field(m, "TotalBlocks"))
        .and_then(as_i64);
    let size = metadata
        .and_then(|m| field(m, "EnclosingSize"))
        .and_then(compound)
        .and_then(xyz_compound)
        .unwrap_or((0, 0, 0));

    let regions = field(root, "Regions")
        .and_then(compound)
        .ok_or_else(|| Error::SchematicMalformed("Regions".into()))?;

    let mut blocks = Vec::new();
    let mut pending_block_ticks = Vec::new();
    for (region_name, region_value) in regions {
        let region = compound(region_value).ok_or_else(|| {
            Error::SchematicMalformed(format!("Regions.{region_name}"))
        })?;
        let (px, py, pz) = field(region, "Position")
            .and_then(compound)
            .and_then(xyz_compound)
            .unwrap_or((0, 0, 0));
        let (sx, sy, sz) = field(region, "Size")
            .and_then(compound)
            .and_then(xyz_compound)
            .ok_or_else(|| Error::SchematicMalformed(format!("Regions.{region_name}.Size")))?;
        let (width, height, length) =
            (sx.unsigned_abs() as usize, sy.unsigned_abs() as usize, sz.unsigned_abs() as usize);

        // `PendingBlockTicks`/`PendingFluidTicks` entries carry `x`/`y`/`z` in
        // the same **raw, pre-sign-flip local index space** the block loop
        // below computes `local_x`/`local_y`/`local_z` in — confirmed against
        // a real downloaded file (`raid_farm.litematic`, whose `Position` is
        // `(0, 0, 0)` and every `Size` axis positive, removing the sign
        // ambiguity entirely): its two `PendingBlockTicks` entries at
        // `(9, 117, 8)`/`(10, 117, 8)` decode, via this exact
        // local-index-then-palette-lookup path, to real
        // `minecraft:repeater` states — not air, and not some other block —
        // so the coordinate space matches. A region with a negative `Size`
        // axis has not been checked against a real file the same way; the
        // same `dx`/`dy`/`dz` sign-adjust the block loop applies is used here
        // too, on the reasoning that keeping this in the exact coordinate
        // space [`SchematicBlock::x`]/`y`/`z` already use is the least
        // surprising choice for a caller re-injecting a tick against blocks
        // this same parser placed.
        if let Some(entries) = field(region, "PendingBlockTicks").and_then(as_list) {
            for entry in entries {
                let Some(c) = compound(entry) else { continue };
                let (Some(lx), Some(ly), Some(lz)) = (
                    field(c, "x").and_then(as_i32),
                    field(c, "y").and_then(as_i32),
                    field(c, "z").and_then(as_i32),
                ) else {
                    continue;
                };
                let block = field(c, "Block").and_then(as_str).unwrap_or("minecraft:air").to_owned();
                let time = field(c, "Time").and_then(as_i64).unwrap_or(0);
                let priority = field(c, "Priority").and_then(as_i32).unwrap_or(0);
                let dx = if sx >= 0 { lx } else { -lx };
                let dy = if sy >= 0 { ly } else { -ly };
                let dz = if sz >= 0 { lz } else { -lz };
                pending_block_ticks.push(SchematicPendingTick {
                    x: px + dx,
                    y: py + dy,
                    z: pz + dz,
                    block,
                    time,
                    priority,
                });
            }
        }

        let palette_entries = field(region, "BlockStatePalette")
            .and_then(as_list)
            .ok_or_else(|| {
                Error::SchematicMalformed(format!("Regions.{region_name}.BlockStatePalette"))
            })?;
        let palette: Vec<String> =
            palette_entries.iter().map(name_properties_entry_to_state).collect();
        let bits = litematica_bits_for(palette.len());

        let volume = width.saturating_mul(height).saturating_mul(length);
        if volume == 0 {
            continue;
        }
        // A single-entry palette (an all-air or fully-uniform region) omits
        // `BlockStates` entirely in some writers; the "no long array" case is
        // then unambiguous — every cell is palette index 0 — so only require
        // the array when the palette actually needs to disambiguate.
        let block_states: &[i64] = match field(region, "BlockStates") {
            Some(Nbt::LongArray(v)) => v.as_slice(),
            _ if palette.len() <= 1 => &[],
            _ => {
                return Err(Error::SchematicMalformed(format!(
                    "Regions.{region_name}.BlockStates"
                )));
            }
        };

        for index in 0..volume {
            let local_x = index % width;
            let local_z = (index / width) % length;
            let local_y = index / (width * length);

            let palette_id = if palette.len() <= 1 {
                0
            } else {
                let Some(id) = read_dense_entry(block_states, index, bits) else {
                    continue;
                };
                id as usize
            };
            let Some(state) = palette.get(palette_id) else {
                continue;
            };
            if is_air(state) {
                continue;
            }

            let dx = if sx >= 0 { local_x as i32 } else { -(local_x as i32) };
            let dy = if sy >= 0 { local_y as i32 } else { -(local_y as i32) };
            let dz = if sz >= 0 { local_z as i32 } else { -(local_z as i32) };
            blocks.push(SchematicBlock {
                x: px + dx,
                y: py + dy,
                z: pz + dz,
                state: state.to_string(),
            });
        }
    }

    Ok(Schematic {
        format: SchematicFormat::Litematica,
        size,
        blocks,
        name,
        author,
        reported_total_blocks,
        pending_block_ticks,
    })
}

// ---------------------------------------------------------------------
// Sponge Schematic (v1/v2/v3)
// ---------------------------------------------------------------------

fn parse_sponge_schematic(root: &[(String, Nbt)]) -> Result<Schematic> {
    // v3 nests Palette/Data under a "Blocks" compound; v1/v2 keep them at the
    // root alongside "BlockData".
    let (palette_field, data_field) = if let Some(blocks) = field(root, "Blocks").and_then(compound)
    {
        (field(blocks, "Palette"), field(blocks, "Data"))
    } else {
        (field(root, "Palette"), field(root, "BlockData"))
    };

    let width = field(root, "Width").and_then(as_i32).ok_or_else(|| {
        Error::SchematicMalformed("Width".into())
    })? as usize;
    let height = field(root, "Height").and_then(as_i32).ok_or_else(|| {
        Error::SchematicMalformed("Height".into())
    })? as usize;
    let length = field(root, "Length").and_then(as_i32).ok_or_else(|| {
        Error::SchematicMalformed("Length".into())
    })? as usize;
    let (ox, oy, oz) = field(root, "Offset").and_then(int_triple).unwrap_or((0, 0, 0));

    let palette = palette_field
        .and_then(compound)
        .ok_or_else(|| Error::SchematicMalformed("Palette".into()))?;
    let mut id_to_state: Vec<String> = Vec::new();
    for (state, id_value) in palette {
        let Some(id) = as_i32(id_value).and_then(|v| usize::try_from(v).ok()) else {
            continue;
        };
        if id >= id_to_state.len() {
            id_to_state.resize(id + 1, String::new());
        }
        id_to_state[id] = state.clone();
    }

    let data: &[i8] = match data_field {
        Some(Nbt::ByteArray(bytes)) => bytes,
        _ => return Err(Error::SchematicMalformed("Data/BlockData".into())),
    };

    let volume = width.saturating_mul(height).saturating_mul(length);
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    let mut cell = 0usize;
    while cell < volume && cursor < data.len() {
        // LEB128 varint, per WorldEdit's `VarInt` codec: 7 payload bits per
        // byte, high bit is the continuation flag.
        let mut value: u32 = 0;
        let mut shift = 0u32;
        loop {
            let Some(&byte) = data.get(cursor) else {
                return Err(Error::SchematicMalformed(
                    "Data/BlockData (truncated varint)".into(),
                ));
            };
            cursor += 1;
            let byte = byte as u8;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }

        let name = id_to_state
            .get(value as usize)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| "minecraft:air".to_owned());
        if !is_air(&name) {
            let x = (cell % width) as i32;
            let z = ((cell / width) % length) as i32;
            let y = (cell / (width * length)) as i32;
            blocks.push(SchematicBlock {
                x: ox + x,
                y: oy + y,
                z: oz + z,
                state: name,
            });
        }
        cell += 1;
    }

    Ok(Schematic {
        format: SchematicFormat::SpongeSchematic,
        size: (width as i32, height as i32, length as i32),
        blocks,
        name: None,
        author: None,
        reported_total_blocks: None,
        pending_block_ticks: Vec::new(),
    })
}

// ---------------------------------------------------------------------
// Vanilla structure .nbt
// ---------------------------------------------------------------------

fn parse_vanilla_structure(root: &[(String, Nbt)]) -> Result<Schematic> {
    let size = field(root, "size")
        .and_then(int_triple)
        .ok_or_else(|| Error::SchematicMalformed("size".into()))?;

    let palette: Vec<String> = if let Some(Nbt::List { elements, .. }) = field(root, "palettes") {
        // A multi-variant structure (jigsaw pieces with alternate palettes);
        // any one of them names the same block set at different states, so
        // the first is representative for a benchmark's purposes.
        elements
            .first()
            .and_then(as_list)
            .map(|entries| entries.iter().map(name_properties_entry_to_state).collect())
            .unwrap_or_default()
    } else if let Some(Nbt::List { elements, .. }) = field(root, "palette") {
        elements.iter().map(name_properties_entry_to_state).collect()
    } else {
        Vec::new()
    };
    if palette.is_empty() {
        return Err(Error::SchematicMalformed("palette/palettes".into()));
    }

    let block_entries = field(root, "blocks")
        .and_then(as_list)
        .ok_or_else(|| Error::SchematicMalformed("blocks".into()))?;

    let mut blocks = Vec::with_capacity(block_entries.len());
    for entry in block_entries {
        let Some(c) = compound(entry) else { continue };
        let Some((x, y, z)) = field(c, "pos").and_then(int_triple) else {
            continue;
        };
        let state_index = field(c, "state").and_then(as_i32).unwrap_or(0);
        let Some(state) = usize::try_from(state_index).ok().and_then(|i| palette.get(i)) else {
            continue;
        };
        if is_air(state) {
            continue;
        }
        blocks.push(SchematicBlock { x, y, z, state: state.to_string() });
    }

    Ok(Schematic {
        format: SchematicFormat::VanillaStructure,
        size,
        blocks,
        name: None,
        author: None,
        reported_total_blocks: None,
        pending_block_ticks: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn litematica_bits_matches_the_measured_worked_example() {
        // The 20x154x12 raid-farm region documented in this module's own doc
        // comment: a 94-entry palette needs 7 bits, not the 8 a naive
        // "always round up to a byte" guess would give, and not the 6 a
        // floor-only formula (log2 without ceiling) would give.
        assert_eq!(litematica_bits_for(94), 7);
        // Small palettes floor at 2, matching `LitematicaBitArray`'s own
        // `Math.max(2, ...)` — never 0 or 1 even for a 1- or 2-entry palette.
        assert_eq!(litematica_bits_for(1), 2);
        assert_eq!(litematica_bits_for(2), 2);
        assert_eq!(litematica_bits_for(3), 2);
        assert_eq!(litematica_bits_for(4), 2);
        assert_eq!(litematica_bits_for(5), 3);
    }

    #[test]
    fn dense_entry_reads_straddle_a_long_boundary() {
        // 3-bit entries, so entry 21 starts at bit 63 and straddles longs 0
        // and 1: low bit in long 0's top bit, remaining 2 bits in long 1's
        // bottom. Hand-built rather than round-tripped through a packer, so
        // this cannot pass by construction the way `decode(encode(x)) == x`
        // would.
        let low = 1u64 << 63; // entry 21's one bit that lives in long 0
        let high = 0b10u64; // entry 21's remaining two bits, at long 1's bottom
        let data = [low as i64, high as i64];
        assert_eq!(read_dense_entry(&data, 21, 3), Some(0b101));
    }

    /// A hand-built Litematica root carrying one 1x1x1-volume region (so the
    /// block-placement half is trivial and this test isolates
    /// `PendingBlockTicks`) with two tick entries, one of them at a negative
    /// coordinate to exercise `as_i32`'s `Byte`/`Short` widening. The
    /// `x=9,y=117,z=8` / `minecraft:repeater` values mirror what
    /// `raid_farm.litematic`'s own `PendingBlockTicks` really contains
    /// (confirmed by hand-decoding that file's `BlockStatePalette` at those
    /// coordinates — see this module's own doc comment on the sign-flip
    /// transform for the full account), so a regression here would also be a
    /// regression against real, previously-verified data.
    #[test]
    fn pending_block_ticks_are_read_from_every_region_with_position_and_priority_intact() {
        let region = Nbt::Compound(vec![
            ("Position".into(), Nbt::Compound(vec![
                ("x".into(), Nbt::Int(0)),
                ("y".into(), Nbt::Int(0)),
                ("z".into(), Nbt::Int(0)),
            ])),
            ("Size".into(), Nbt::Compound(vec![
                ("x".into(), Nbt::Int(1)),
                ("y".into(), Nbt::Int(1)),
                ("z".into(), Nbt::Int(1)),
            ])),
            ("BlockStatePalette".into(), Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("Name".into(), Nbt::String("minecraft:air".into())),
                ])],
            }),
            ("PendingBlockTicks".into(), Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![
                    Nbt::Compound(vec![
                        ("x".into(), Nbt::Int(9)),
                        ("y".into(), Nbt::Int(117)),
                        ("z".into(), Nbt::Int(8)),
                        ("Block".into(), Nbt::String("minecraft:repeater".into())),
                        ("Time".into(), Nbt::Int(1)),
                        ("Priority".into(), Nbt::Byte(-2)),
                    ]),
                    Nbt::Compound(vec![
                        ("x".into(), Nbt::Short(-3)),
                        ("y".into(), Nbt::Int(64)),
                        ("z".into(), Nbt::Int(0)),
                        ("Block".into(), Nbt::String("minecraft:fire".into())),
                        ("Time".into(), Nbt::Int(23)),
                        ("Priority".into(), Nbt::Int(0)),
                    ]),
                ],
            }),
        ]);
        let root = vec![(
            "Regions".to_owned(),
            Nbt::Compound(vec![("only".to_owned(), region)]),
        )];

        let schematic = parse_litematica(&root).expect("hand-built region must parse");
        assert_eq!(schematic.pending_block_ticks.len(), 2, "{:?}", schematic.pending_block_ticks);

        let repeater = &schematic.pending_block_ticks[0];
        assert_eq!((repeater.x, repeater.y, repeater.z), (9, 117, 8));
        assert_eq!(repeater.block, "minecraft:repeater");
        assert_eq!(repeater.time, 1);
        assert_eq!(repeater.priority, -2, "Byte(-2) must widen to i32 -2, not wrap");

        let fire = &schematic.pending_block_ticks[1];
        assert_eq!((fire.x, fire.y, fire.z), (-3, 64, 0), "Short(-3) must widen to i32 -3");
        assert_eq!(fire.block, "minecraft:fire");
        assert_eq!(fire.time, 23);
        assert_eq!(fire.priority, 0);
    }

    #[test]
    fn format_state_matches_the_shape_redstone_rs_parses() {
        let mut props = BTreeMap::new();
        props.insert("power".to_owned(), "10".to_owned());
        assert_eq!(
            format_state("minecraft:redstone_wire", &props),
            "minecraft:redstone_wire[power=10]"
        );
        assert_eq!(format_state("minecraft:air", &BTreeMap::new()), "minecraft:air");
    }

    #[test]
    fn detect_format_refuses_legacy_mcedit_extension() {
        assert_eq!(
            detect_format(Path::new("build.litematic")),
            Some(SchematicFormat::Litematica)
        );
        assert_eq!(
            detect_format(Path::new("build.schem")),
            Some(SchematicFormat::SpongeSchematic)
        );
        assert_eq!(
            detect_format(Path::new("build.nbt")),
            Some(SchematicFormat::VanillaStructure)
        );
        // The legacy MCEdit ".schematic" format is a different, older
        // schema this module does not read — see its own doc comment.
        assert_eq!(detect_format(Path::new("build.schematic")), None);
    }

    #[test]
    fn is_air_strips_the_property_body() {
        assert!(is_air("minecraft:air"));
        assert!(is_air("minecraft:cave_air"));
        assert!(!is_air("minecraft:redstone_wire[power=0]"));
    }
}
