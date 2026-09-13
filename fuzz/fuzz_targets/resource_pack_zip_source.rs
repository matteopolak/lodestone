//! libFuzzer target: `lodestone_assets::ZipSource::from_bytes` (and every
//! entry it indexes) must never panic or attempt an unbounded allocation on
//! arbitrary bytes.
//!
//! A resource pack is untrusted input the moment a server can supply one —
//! `minecraft:resource_pack_push` names a URL and a hash, and the bytes that
//! come back are a hostile-or-merely-corrupt zip/jar archive parsed entirely
//! client-side before any of its entries are used. `ZipSource::from_bytes` is
//! the single entry point every one of those bytes goes through: it parses
//! the central directory once at construction, and this target additionally
//! walks every entry `from_bytes` indexed and calls
//! [`lodestone_assets::ResourceSource::read`] on each one, chaining into the
//! decompression path central-directory parsing alone does not reach.
//!
//! This is exactly the untrusted-length-drives-allocation shape this
//! workspace's fuzzing has already found real bugs in (a 9-byte clientbound
//! packet driving an unchecked `Vec::with_capacity`, and — unrelated to zip —
//! unbounded item-component nesting exhausting the stack): a zip entry's
//! declared uncompressed size is read straight off the archive's own
//! metadata and is not required to match what its compressed bytes actually
//! decompress to, so a tiny input can *claim* to be enormous.
//!
//! The committed seed (`fuzz/seeds/resource_pack_zip_source/`) is a real zip
//! built from vanilla's own language files (`assets/minecraft/lang/en_us.json`
//! and `deprecated.json`, each truncated to 16 KiB), so the archive structure
//! — central directory, local headers, deflate streams — is genuine, even
//! though the *container* was assembled by this harness rather than shipped
//! by Mojang (unlike `unihex_font`, which has no available real-producer
//! source at all, a real one exists here and is used).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_assets::{ResourceSource, ZipSource};

fuzz_target!(|data: &[u8]| {
    let Ok(source) = ZipSource::from_bytes(data.to_vec()) else {
        return;
    };
    for name in source.list("") {
        let _ = source.read(&name);
    }
});
