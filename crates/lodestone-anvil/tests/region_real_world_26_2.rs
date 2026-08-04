//! The real-`.mca`-from-26.2 evidence `region_real_world.rs` didn't have:
//! this crate's `RegionFile` reader against a region file this repo's own
//! `creative` oracle wrote *just now*, over RCON, with no code from this
//! crate anywhere in the write path.
//!
//! # Exactly how this file was produced
//!
//! 1. `./scripts/live-oracles/creative.sh` (Apple `container`, not Docker —
//!    see that script) booted `.cache/mc/26.2/server.jar` fresh, game on
//!    `:25570`, RCON on `:25571`.
//! 2. Over RCON (`scripts/live-oracles/rcon-op.py 127.0.0.1 25571 lodestone
//!    "<command>"`, one frame per `sendall`, matching the one-`read()`-per-
//!    request constraint):
//!    - `setblock 5 90 5 minecraft:diamond_block` (chunk (0,0), region (0,0))
//!    - `setblock 200 90 200 minecraft:emerald_block` (chunk (12,12), region (0,0))
//!    - `setblock 1000 90 1000 minecraft:gold_block` (chunk (62,62), region (1,1))
//!    - `save-all flush`
//! 3. This produced real files at
//!    `.cache/mc/creative/world/dimensions/minecraft/overworld/region/{r.0.0,r.1.1}.mca`
//!    — note the path: this repo's *older* protocol families' oracles
//!    (`region_real_world.rs`'s 1.8.9/1.12.2/1.16.5 files) write the
//!    overworld straight to `world/region/`, but this 26.2 server instead
//!    nests it under `world/dimensions/minecraft/overworld/region/`. Not
//!    cited to a `file:line` — this was observed directly from the real
//!    directory listing this oracle produced, not read out of decompiled
//!    source — but worth recording here since it's exactly the kind of
//!    thing #437's wiring work needs to get right and would otherwise learn
//!    the hard way.
//!
//! # Where the expected values came from
//!
//! Independently, with a throwaway Python script
//! (`struct`/`zlib`, no code from this crate, no code from vanilla) that
//! parses the region header, decompresses the target chunk, walks the NBT
//! tree by hand, and decodes the `sections`/`block_states` palette +
//! bit-packed `data` long array at the target block's local coordinate —
//! the standard post-1.18 Anvil chunk schema. Confirmed, for all three
//! placed blocks, to report exactly the block this test placed
//! (`minecraft:diamond_block`, `minecraft:emerald_block`,
//! `minecraft:gold_block`) — not a coincidence achievable by a wrong
//! bit-packing formula three different times.
//!
//! This test's own in-Rust palette walk (`block_state_name_at`, below)
//! deliberately goes one step past what `lodestone-anvil` itself parses —
//! the crate's own boundary stops at handing back a decoded
//! [`lodestone_core::Nbt`] tree (see the crate doc: container format and
//! chunk *schema* are two different problems). The walk here exists only to
//! turn that tree into a human-checkable assertion in this one
//! verification test; it is not exposed as part of the crate's public API,
//! and issue [#437](https://github.com/matteopolak/lodestone/issues/437) is
//! where a real chunk-schema layer belongs.

use lodestone_anvil::region::RegionFile;
use lodestone_core::Nbt;
use std::path::{Path, PathBuf};

fn overworld_region_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../.cache/mc/creative/world/dimensions/minecraft/overworld/region",
    )
}

fn read_region(file_name: &str) -> RegionFile {
    let path = overworld_region_dir().join(file_name);
    RegionFile::read_from_file(&path).unwrap_or_else(|e| {
        panic!(
            "no real region file at {} ({e}); this is this repo's live creative oracle's own \
             world, not checked in — boot scripts/live-oracles/creative.sh, place a known block \
             with setblock over RCON, `save-all flush`, and point this test at the result",
            path.display()
        )
    })
}

fn compound_field<'a>(nbt: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(n, _)| n == name).map(|(_, v)| v),
        _ => None,
    }
}

fn as_list(nbt: &Nbt) -> &[Nbt] {
    match nbt {
        Nbt::List { elements, .. } => elements,
        _ => panic!("expected a List, got {nbt:?}"),
    }
}

fn as_int(nbt: &Nbt) -> i32 {
    match nbt {
        Nbt::Int(v) => *v,
        _ => panic!("expected an Int, got {nbt:?}"),
    }
}

/// A section's `"Y"` field is a `Byte` (section indices fit `-4..=19`, so
/// vanilla doesn't spend a full `Int` on it) while `xPos`/`zPos` are full
/// `Int`s — this repo's own real file made that distinction obvious the
/// first time this test ran against it (`expected an Int, got Byte(-5)`).
fn as_int_like(nbt: &Nbt) -> i32 {
    match nbt {
        Nbt::Byte(v) => i32::from(*v),
        Nbt::Short(v) => i32::from(*v),
        Nbt::Int(v) => *v,
        _ => panic!("expected an integer-like tag, got {nbt:?}"),
    }
}

fn as_string(nbt: &Nbt) -> &str {
    match nbt {
        Nbt::String(v) => v,
        _ => panic!("expected a String, got {nbt:?}"),
    }
}

fn as_long_array(nbt: &Nbt) -> &[i64] {
    match nbt {
        Nbt::LongArray(v) => v,
        _ => panic!("expected a LongArray, got {nbt:?}"),
    }
}

/// The standard post-1.18 Anvil chunk schema's block lookup: find the
/// section covering `y`, then either return its single-entry palette name
/// outright, or unpack `data`'s `bits`-wide fields (`bits = max(4,
/// ceil(log2(palette.len())))`, section-relative Y/Z/X major order — see
/// this file's doc for how this was independently cross-checked). Kept
/// local to this test rather than promoted into `region.rs` — see the doc
/// above for why.
fn block_state_name_at(chunk: &Nbt, x: i32, y: i32, z: i32) -> String {
    let section_y = y >> 4;
    let sections = as_list(compound_field(chunk, "sections").expect("chunk has sections"));
    let section = sections
        .iter()
        .find(|s| as_int_like(compound_field(s, "Y").expect("section has Y")) == section_y)
        .unwrap_or_else(|| panic!("no section for y={y} (section_y={section_y})"));

    let block_states = compound_field(section, "block_states")
        .expect("section has block_states");
    let palette = as_list(compound_field(block_states, "palette").expect("palette present"));
    if palette.len() == 1 {
        return as_string(compound_field(&palette[0], "Name").expect("palette entry has Name"))
            .to_string();
    }

    let data = as_long_array(compound_field(block_states, "data").expect("data present"));
    // `bits = max(4, ceil(log2(palette.len())))` — matches the
    // independently-verified Python reference this test's doc describes.
    let bits = {
        let mut b = 4u32;
        while (1u64 << b) < palette.len() as u64 {
            b += 1;
        }
        b
    };
    let (local_x, local_y, local_z) = ((x & 15) as u64, (y & 15) as u64, (z & 15) as u64);
    let block_index = (local_y * 16 + local_z) * 16 + local_x;
    let values_per_long = 64 / bits as u64;
    let long_index = (block_index / values_per_long) as usize;
    let bit_offset = (block_index % values_per_long) * bits as u64;
    let raw = data[long_index] as u64;
    let value = ((raw >> bit_offset) & ((1u64 << bits) - 1)) as usize;
    as_string(compound_field(&palette[value], "Name").expect("palette entry has Name")).to_string()
}

fn decode_chunk(region: &RegionFile, local_x: u8, local_z: u8) -> Nbt {
    let raw = region
        .read_chunk_nbt_bytes(local_x, local_z)
        .expect("reads without a corrupt-input error")
        .unwrap_or_else(|| panic!("chunk ({local_x}, {local_z}) unexpectedly absent"));
    let mut reader = lodestone_core::Reader::new(&raw);
    let (_, nbt) = lodestone_core::read_named_nbt(&mut reader).expect("decodes as named NBT");
    nbt
}

#[test]
#[ignore = "requires this repo's own creative oracle world with a known block placed via RCON — see this file's doc for the exact commands"]
fn reads_two_real_blocks_placed_by_this_repos_own_26_2_oracle() {
    let region_0_0 = read_region("r.0.0.mca");

    let chunk_0_0 = decode_chunk(&region_0_0, 0, 0);
    assert_eq!(as_int(compound_field(&chunk_0_0, "xPos").expect("xPos")), 0);
    assert_eq!(as_int(compound_field(&chunk_0_0, "zPos").expect("zPos")), 0);
    assert_eq!(
        block_state_name_at(&chunk_0_0, 5, 90, 5),
        "minecraft:diamond_block"
    );

    let chunk_12_12 = decode_chunk(&region_0_0, 12, 12);
    assert_eq!(
        as_int(compound_field(&chunk_12_12, "xPos").expect("xPos")),
        12
    );
    assert_eq!(
        as_int(compound_field(&chunk_12_12, "zPos").expect("zPos")),
        12
    );
    assert_eq!(
        block_state_name_at(&chunk_12_12, 200, 90, 200),
        "minecraft:emerald_block"
    );
}

#[test]
#[ignore = "requires this repo's own creative oracle world with a known block placed via RCON — see this file's doc for the exact commands"]
fn reads_a_real_block_from_a_second_real_26_2_region_file() {
    // A different region file than the test above (r.1.1, not r.0.0) —
    // chunk (62,62) is region-local (30,30) within region (1,1)
    // (`62 >> 5 == 1`, `62 & 31 == 30`), proving this isn't just the one
    // region file this crate happened to get right.
    let region_1_1 = read_region("r.1.1.mca");
    let chunk = decode_chunk(&region_1_1, 30, 30);
    assert_eq!(as_int(compound_field(&chunk, "xPos").expect("xPos")), 62);
    assert_eq!(as_int(compound_field(&chunk, "zPos").expect("zPos")), 62);
    assert_eq!(
        block_state_name_at(&chunk, 1000, 90, 1000),
        "minecraft:gold_block"
    );
}
