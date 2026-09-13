//! libFuzzer target: `lodestone_data::block_states::state_id` must never
//! panic on arbitrary bytes.
//!
//! `state_id` parses strings shaped like `minecraft:oak_log[axis=y]` against
//! the static 32,366-entry 26.2 block-state table. It has no `[[bin]]`-level
//! network reach today (`crates/lodestone-command-mc`'s `BlockArg` v1
//! explicitly refuses the `[...]` property grammar — see that crate's
//! `block.rs` module doc), but it is the parser the property grammar will
//! route through once that lands, and it is already reachable from anything
//! that stringifies a wire block-state id and re-parses it (chunk-parity
//! fixtures, structure NBT tooling). Fuzzing it now is cheap insurance
//! against exactly the panic-on-malformed-bracket-syntax class a hand-rolled
//! splitter (`split_once('[')`, `strip_suffix(']')`, comma/`=` splitting) is
//! prone to.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_data::block_states::state_id;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = state_id(text);
});
