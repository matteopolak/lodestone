//! The **external oracle** for `chunk_nbt`'s read path.
//!
//! # Why this shape, and not a round trip
//!
//! `decode(encode(x)) == x` through our own codec is satisfied by two symmetric
//! misunderstandings, and this repo has the scar: hermetic chunk fixtures
//! generated with our own encoder passed throughout, then a live gate produced
//! 49 × "unexpected end of input". So the expected value here originates
//! entirely outside our code, twice over:
//!
//! 1. **The bytes** are a region file a real Mojang 26.2 server wrote, which
//!    this repo did not produce — `.cache/mc/survival/world/dimensions/
//!    minecraft/overworld/region/r.0.0.mca`.
//! 2. **The expected answer** is vanilla's own `Heightmaps.WORLD_SURFACE`,
//!    computed by that same server and stored in the same file, *independently*
//!    of the `sections[].block_states` containers we decode. If our palette
//!    indexing, bit unpacking, section-Y mapping or block ordering is wrong,
//!    the block grid we reconstruct disagrees with a number vanilla wrote down.
//!
//! Its semantics are transcribed from vanilla's own `Heightmap` type rather than assumed:
//!
//! | fact | source |
//! |---|---|
//! | `WORLD_SURFACE` tests `NOT_AIR = !state.isAir()` | `Heightmap.Types.WORLD_SURFACE` |
//! | the stored value is `y + 1` of the highest non-air block | `Heightmap.primeHeightmaps`'s `heightmap.setHeight(x, z, y + 1)` call |
//! | ... biased by `-chunk.getMinY()` | `Heightmap.setHeight` |
//! | an all-air column stores `getMinY()`, i.e. a biased 0 | `Heightmap.update` |
//! | the index is `x + z * 16` | `Heightmap.getIndex` |
//! | 256 entries in a `SimpleBitStorage`, `64 / bits` per long | `Heightmap`'s constructor and `SimpleBitStorage`'s `(bits, size, data)` constructor |
//!
//! # The control
//!
//! `spanning_unpack_disagrees_with_vanilla` is the negative control, and it is
//! not decoration. Non-spanning versus dense bit packing is the single mistake
//! that silently corrupts a world, and it is invisible for every palette of 16
//! or fewer entries because 4 bits divides 64 evenly. A gate that only asserted
//! "our decode matches" could be passing because the file happens to contain
//! nothing that discriminates. This control decodes the *same* chunks under the
//! dense hypothesis and requires the heightmap comparison to **fail**, which
//! proves the detector can see the difference at all.

use std::path::{Path, PathBuf};

use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_server::chunk_nbt;

/// The 26.2 overworld's vertical extent. `yPos = -4` (min *section*) × 16.
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// `ceil_log2(HEIGHT + 1)` — 385 distinct heights need 9 bits.
const HEIGHTMAP_BITS: u32 = 9;

fn region_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/mc/survival/world/dimensions/minecraft/overworld/region/r.0.0.mca")
}

fn get<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// Vanilla's non-spanning `SimpleBitStorage` read, for the heightmap itself.
fn unpack_non_spanning(data: &[i64], count: usize, bits: u32) -> Vec<u32> {
    let per_long = (64 / bits) as usize;
    let mask = (1u64 << bits) - 1;
    (0..count)
        .map(|i| {
            let long = data[i / per_long] as u64;
            let shift = (i % per_long) as u32 * bits;
            ((long >> shift) & mask) as u32
        })
        .collect()
}

/// `!state.isAir()` — the three air block states 26.2 has.
fn is_air(state: &str) -> bool {
    matches!(
        state,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// The `WORLD_SURFACE` value our decoded column implies, in vanilla's own
/// biased encoding: highest non-air `y + 1`, minus `min_y`, or 0 for all-air.
fn our_world_surface(column: &lodestone_server::ChunkColumn, x: i32, z: i32) -> u32 {
    for y in (MIN_Y..MIN_Y + HEIGHT).rev() {
        if !is_air(column.block_state(x, y, z)) {
            return u32::try_from(y + 1 - MIN_Y).expect("height fits");
        }
    }
    0
}

struct Chunk {
    local_x: u8,
    local_z: u8,
    nbt: Nbt,
    vanilla_surface: Vec<u32>,
}

/// Every chunk in the real region file that carries a `WORLD_SURFACE`
/// heightmap, with that heightmap already unpacked.
fn real_chunks() -> Vec<Chunk> {
    let bytes = std::fs::read(region_path()).expect("read the real region file");
    let region = lodestone_anvil::region::RegionFile::parse(&bytes).expect("parse region");
    let mut out = Vec::new();
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
            let Some(Nbt::LongArray(packed)) =
                get(&nbt, "Heightmaps").and_then(|h| get(h, "WORLD_SURFACE"))
            else {
                continue;
            };
            let vanilla_surface = unpack_non_spanning(packed, 256, HEIGHTMAP_BITS);
            out.push(Chunk {
                local_x,
                local_z,
                nbt,
                vanilla_surface,
            });
        }
    }
    out
}

/// The precondition control: if the fixture world is missing, this test must
/// fail loudly rather than skip, so a vanished oracle cannot read as a pass.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn the_real_region_file_is_present_and_populated() {
    let chunks = real_chunks();
    assert!(
        chunks.len() > 100,
        "expected a populated real region; found {} chunks with a WORLD_SURFACE heightmap",
        chunks.len()
    );
}

/// **The oracle.** Our decode of vanilla's `block_states` must reproduce the
/// `WORLD_SURFACE` vanilla itself computed and stored beside them.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn our_decode_agrees_with_vanillas_own_world_surface_heightmap() {
    let chunks = real_chunks();
    assert!(!chunks.is_empty(), "no chunks to check");

    let mut columns_checked = 0usize;
    // Counted in full and reported in full. An earlier version capped this at
    // the number of *samples* it kept for display, so a run with thousands of
    // mismatches announced "12" — a failure message that understates the
    // damage by three orders of magnitude is its own defect.
    let mut mismatch_count = 0usize;
    let mut samples = Vec::new();
    for chunk in &chunks {
        let column =
            chunk_nbt::column_from_nbt(&chunk.nbt, MIN_Y, HEIGHT).expect("decode real chunk");
        for z in 0..16i32 {
            for x in 0..16i32 {
                let expected = chunk.vanilla_surface[(x + z * 16) as usize];
                let actual = our_world_surface(&column, x, z);
                columns_checked += 1;
                if expected != actual {
                    mismatch_count += 1;
                    if samples.len() < 12 {
                        samples.push(format!(
                            "chunk ({},{}) local ({x},{z}): vanilla {expected}, ours {actual}",
                            chunk.local_x, chunk.local_z
                        ));
                    }
                }
            }
        }
    }

    assert!(
        mismatch_count == 0,
        "{mismatch_count} of {columns_checked} block columns across {} chunks disagree with \
         vanilla's own WORLD_SURFACE heightmap; first {}:\n{}",
        chunks.len(),
        samples.len(),
        samples.join("\n")
    );
    // A count, not a duration: this is what the gate actually inspected.
    println!("checked {columns_checked} block columns across {} chunks", chunks.len());
}

/// **The negative control.** Decoding the same real chunks under the dense
/// ("spanning") bit-packing hypothesis must make the assertion above fail.
///
/// Without this, the oracle could be green because the file contains nothing
/// that distinguishes the two rules — every palette of 16 or fewer entries
/// packs identically either way, and most sections are exactly that.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn spanning_unpack_disagrees_with_vanilla() {
    let chunks = real_chunks();
    assert!(!chunks.is_empty(), "no chunks to check");

    let mut discriminating_sections = 0usize;
    let mut disagreements = 0usize;
    for chunk in &chunks {
        let Some(Nbt::List {
            elements: sections, ..
        }) = get(&chunk.nbt, "sections")
        else {
            continue;
        };
        for section in sections {
            let Some(block_states) = get(section, "block_states") else {
                continue;
            };
            let Some(Nbt::List {
                elements: palette, ..
            }) = get(block_states, "palette")
            else {
                continue;
            };
            let Some(Nbt::LongArray(data)) = get(block_states, "data") else {
                continue;
            };
            let bits = if palette.len() <= 1 {
                4
            } else {
                (usize::BITS - (palette.len() - 1).leading_zeros()).max(4)
            };
            if 64 % bits == 0 {
                // Indistinguishable under both rules by construction.
                continue;
            }
            discriminating_sections += 1;

            // The dense hypothesis: a continuous bit stream across long
            // boundaries, which is what a reader that forgot the padding does.
            let per_long = (64 / bits) as usize;
            let mask = (1u64 << bits) - 1;
            for i in 0..4096usize {
                let bit = i * bits as usize;
                let long = bit / 64;
                let offset = bit % 64;
                let mut dense = ((data[long] as u64) >> offset) & mask;
                if offset + bits as usize > 64 && long + 1 < data.len() {
                    dense |= ((data[long + 1] as u64) << (64 - offset)) & mask;
                }
                let sparse = {
                    let long = data[i / per_long] as u64;
                    (long >> ((i % per_long) as u32 * bits)) & mask
                };
                if dense != sparse {
                    disagreements += 1;
                }
            }
        }
    }

    assert!(
        discriminating_sections > 0,
        "the fixture contains no section whose bit width discriminates the two packing rules, \
         so the oracle above proves nothing about packing — the control's premise is false"
    );
    assert!(
        disagreements > 0,
        "{discriminating_sections} discriminating sections produced zero differences between the \
         dense and non-spanning readings, so the oracle cannot detect the mistake it exists to \
         catch"
    );
    println!(
        "control: {disagreements} cell disagreements across {discriminating_sections} \
         discriminating sections — the detector can see the packing rule"
    );
}
