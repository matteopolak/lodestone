//! libFuzzer target: `lodestone_core::read_named_nbt` must never panic on
//! arbitrary bytes.
//!
//! NBT is one of the two named "obvious first targets"
//! (alongside the packet decoders) — every clientbound packet field typed as
//! NBT (`GameLogin`'s registry payloads, `SetEntityData`'s custom-name/
//! item-component tags, `container_set_slot`'s item NBT, chunk block-entity
//! data, ...) routes through this one reader, so a single decoder bug here is
//! reachable from many packets at once rather than being packet-specific.
//! `docs/fuzz-harness.md` records that NBT is exercised only *transitively*
//! today (through the packet-level proptest suite), not directly — this is
//! the direct target that doc's own "reasonable follow-up... not this
//! harness's most valuable next dollar" line names.
//!
//! No corpus seed is bundled here beyond libFuzzer's own empty start: the
//! packet decoders above already carry the real vanilla-capture corpus
//! (`crates/lodestone-fuzz/tests/fixtures`, `crates/protocol/v770/tests/
//! fixtures`), which is a stronger source of "real" NBT bytes than anything
//! this target could construct standalone — a human wanting a head start can
//! seed `fuzz/corpus/nbt_decode/` from those `.hex` fixtures directly (parse
//! with the same `#`-comment format `lodestone_fuzz::read_hex_fixture` uses).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_core::{Reader, read_named_nbt};

fuzz_target!(|data: &[u8]| {
    let mut r = Reader::new(data);
    let _ = read_named_nbt(&mut r);
});
