//! libFuzzer target: `lodestone_anvil::region::RegionFile::parse` (and the
//! chunk payload it hands back) must never panic on arbitrary bytes.
//!
//! Issue #549's "obvious first targets" list names "chunk/region
//! deserialization in `lodestone-anvil`" directly — a `.mca` region file is
//! attacker-controlled the moment a player can supply one at all (importing a
//! world, loading a schematic-adjacent structure, or simply a corrupted save
//! on disk after a crash mid-write), and `RegionFile::parse` is the single
//! entry point every one of those paths goes through before any chunk NBT is
//! read. Region format decode is unusually panic-prone by construction: fixed
//! 8 KiB header, two raw `u32` tables read straight off disk
//! (`read_be_u32`), and a per-chunk sector length/offset pair that this
//! parser already sanitizes at open time (out-of-bounds locations zeroed) —
//! exactly the kind of hand-rolled bounds arithmetic this workspace's own
//! record shows is where a coordinate overflow or an off-by-one hides
//! (`v1-9`'s `multi_block_change` chunk-coordinate multiply, per
//! `docs/fuzz-harness.md`).
//!
//! This target does not stop at the header: after a successful parse it
//! walks every one of the 1024 `(local_x, local_z)` slots and calls
//! [`RegionFile::read_chunk_nbt_bytes`], which is the code path that
//! actually seeks into the file body, checks the declared sector length
//! against the buffer, decompresses (zlib or gzip, scheme byte read straight
//! from the fuzzer input), and returns the decompressed NBT bytes — the
//! decompression step is a second, independent attacker-controlled-length
//! surface the header sanitation does not cover. Whatever comes back is fed
//! straight into `lodestone_core::read_named_nbt`, chaining into the same
//! NBT reader the dedicated `nbt_decode` target exercises, so a chunk payload
//! that decompresses to a hostile NBT document is covered end to end in one
//! target rather than needing a second corpus.
//!
//! No corpus is bundled: unlike the packet decoders, no real captured `.mca`
//! bytes live in this repo today (`.cache/mc/*` holds real vanilla region
//! files under a live oracle's world save, but those are gitignored inputs,
//! not fixtures this crate can commit) — a human wanting a head start can
//! seed `fuzz/corpus/anvil_region_parse/` from a small saved region file
//! directly; the format is verbatim disk bytes, no wrapper needed.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_anvil::region::{CHUNKS_PER_SIDE, RegionFile};
use lodestone_core::{Reader, read_named_nbt};

fuzz_target!(|data: &[u8]| {
    let Ok(region) = RegionFile::parse(data) else {
        return;
    };
    for x in 0..CHUNKS_PER_SIDE as u8 {
        for z in 0..CHUNKS_PER_SIDE as u8 {
            let Ok(Some(nbt_bytes)) = region.read_chunk_nbt_bytes(x, z) else {
                continue;
            };
            let mut r = Reader::new(&nbt_bytes);
            let _ = read_named_nbt(&mut r);
        }
    }
});
