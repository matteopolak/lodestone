//! libFuzzer target: `lodestone_model::text::Text::from_nbt` must never
//! panic on an arbitrary (but well-formed-enough-to-decode) NBT document.
//!
//! The modern-protocol sibling of `text_chat_json`: `v770` (and any future
//! post-1.20.3 family) sends chat, sign text, book pages and hover-event
//! contents as binary NBT rather than a JSON string, decoded by this
//! function. `from_nbt`'s own doc claims "non-panicking and depth-limited"
//! the same way `from_json`'s does — the same self-reported property,
//! turned into the same kind of measurement.
//!
//! Chained through two real decoders rather than constructing an `Nbt` value
//! by hand: raw bytes go through `lodestone_core::read_network_nbt` first
//! (the same reader `nbt_decode.rs` fuzzes directly, and what a
//! `SET_ENTITY_DATA`/chat/sign packet's NBT field decodes through in
//! production), and only a value that already decoded cleanly is handed to
//! `Text::from_nbt`. This means a panic found here localizes to the
//! `Nbt` -> `Text` fold specifically, not to the NBT reader underneath it
//! (which `nbt_decode.rs` already covers and would otherwise get blamed for
//! any crash this target found).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_core::{Reader, read_network_nbt};
use lodestone_model::text::Text;

fuzz_target!(|data: &[u8]| {
    let mut r = Reader::new(data);
    let Ok(nbt) = read_network_nbt(&mut r) else {
        return;
    };
    let _ = Text::from_nbt(&nbt);
});
