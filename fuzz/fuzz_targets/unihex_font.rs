//! libFuzzer target: `lodestone_assets::font::read_hex_entries` must never
//! panic on arbitrary bytes.
//!
//! The unihex parser is a GNU Unifont `.hex` line-format reader
//! (`UnihexProvider.readFromStream`'s Rust port). Resource packs — including
//! ones a server can push via `minecraft:resource_pack_push` — can ship a
//! `.hex` file, so a hostile or malformed pack is the realistic untrusted
//! source. `read_hex_entries` already returns `Result<_, FontError>` for
//! every malformed-line case its module doc lists (bad codepoint digit
//! count, bad bitmap digit count, non-hex digit) rather than panicking, so
//! this target's job is to confirm no case was missed by that enumeration —
//! coverage-guided search is far more likely to find a missed arm than a
//! hand-written fixture list.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_assets::font::read_hex_entries;

fuzz_target!(|data: &[u8]| {
    let _ = read_hex_entries(data, |_, _| {});
});
