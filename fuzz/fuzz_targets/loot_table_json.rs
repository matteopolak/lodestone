//! libFuzzer target: `lodestone_server::loot::LootTable::from_json` must
//! never panic on arbitrary bytes.
//!
//! Loot tables are one of `crates/lodestone-fuzz`'s own "obvious first
//! targets" (alongside NBT and the packet decoders) — 1355 of them ship as
//! bundled datapack JSON today (`loot_corpus.rs`), and a server can override
//! or add tables via a datapack, so a malformed document reaching this parser
//! is a real (if lower-frequency than a wire packet) untrusted-input path,
//! not just a defensive-programming exercise. `docs/fuzz-harness.md`'s
//! record of `block_state_property` evaluating as a constant `false` (154
//! loot tables silently taking the wrong branch) shows this exact file is
//! where past bugs of this shape have lived — that one was a *logic* bug a
//! fuzzer with no oracle cannot catch, but a parser *panic* on the same file
//! family is exactly this target's property.
//!
//! Input is treated as UTF-8 text (lossy) and handed straight to
//! `from_json` with a fixed resource key — the key only affects
//! self-reference resolution inside the table, not parse safety, so it does
//! not need to vary with the fuzzer input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_model::ids::ResourceKey;
use lodestone_server::loot::LootTable;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let id: ResourceKey = "minecraft:fuzz/table".parse().unwrap();
    let _ = LootTable::from_json(&id, text);
});
