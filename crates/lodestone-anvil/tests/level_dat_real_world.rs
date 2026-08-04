//! Reads real `level.dat` files this crate did not write, across every one
//! of this repo's 26.2 oracle worlds — the direct evidence for issue #300's
//! own verification bar ("round-trip against a `level.dat` produced by a
//! real 26.2 server").
//!
//! # Where the expected value came from
//!
//! `4903` — every test below asserts exactly this — came from an
//! independent Python script (`gzip.decompress` + `struct.unpack_from`,
//! stdlib only, no code from this crate) run against each file directly:
//! find the byte offset of the ASCII string `"DataVersion"`, then read the
//! big-endian `i32` starting 11 bytes later (past the field's own name).
//! Cross-checked against all five 26.2 oracle worlds this checkout
//! currently has on disk (`creative`, `terrain`, `survival`, `online262`,
//! `oracle`) — all five agree, which is expected: they all came from the
//! same `.cache/mc/26.2` server jar's `SharedConstants` data version, not a
//! coincidence.
//!
//! `#[ignore]`d for the same reason as `region_real_world.rs`: these are
//! oracle-generated worlds, not checked-in fixtures, and a test that
//! silently downgrades a missing precondition to a pass is the thing this
//! repo's own standing rule warns against. Run explicitly with `cargo test
//! -p lodestone-anvil --test level_dat_real_world -- --ignored`.

use lodestone_anvil::level_dat;
use std::path::{Path, PathBuf};

fn oracle_level_dat(world: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/mc")
        .join(world)
        .join("world/level.dat")
}

fn assert_real_data_version(world: &str) {
    let path = oracle_level_dat(world);
    let level = level_dat::read_from_file(&path).unwrap_or_else(|e| {
        panic!(
            "no real level.dat at {} ({e}); this repo's .cache/mc/{world} oracle world is not \
             checked in — boot scripts/live-oracles/{world}.sh (or terrain.sh/survival.sh) first",
            path.display()
        )
    });
    assert_eq!(
        level.data_version().expect("has a DataVersion field"),
        4903,
        "expected the real 26.2 server's own DataVersion, independently verified with a \
         standalone Python gzip+struct parse of {}",
        path.display()
    );
}

#[test]
#[ignore = "requires .cache/mc/creative/world/level.dat, a real file this crate did not write"]
fn creative_oracle_level_dat_has_the_real_data_version() {
    assert_real_data_version("creative");
}

#[test]
#[ignore = "requires .cache/mc/terrain/world/level.dat, a real file this crate did not write"]
fn terrain_oracle_level_dat_has_the_real_data_version() {
    assert_real_data_version("terrain");
}

#[test]
#[ignore = "requires .cache/mc/survival/world/level.dat, a real file this crate did not write"]
fn survival_oracle_level_dat_has_the_real_data_version() {
    assert_real_data_version("survival");
}

#[test]
#[ignore = "requires .cache/mc/creative/world/level.dat, a real file this crate did not write"]
fn modifying_a_real_level_dat_and_writing_it_back_preserves_every_other_field() {
    // The part of issue #300's verification bar this crate CAN clear without
    // a live server round trip: read a real file, change exactly the field
    // this crate models (DataVersion), write it back out, and confirm
    // nothing else moved. This does not prove "the real server still opens
    // the world" (that needs an actual server run, out of scope for a
    // hermetic-ish `#[ignore]`d unit test) — it proves our writer doesn't
    // corrupt or drop the ~500 bytes of fields it doesn't understand.
    let path = oracle_level_dat("creative");
    let mut level = level_dat::read_from_file(&path).unwrap_or_else(|e| {
        panic!("no real level.dat at {} ({e})", path.display())
    });
    let original = level.clone();
    assert_eq!(original.data_version().expect("has DataVersion"), 4903);

    level.set_data_version(99999).expect("field exists to update");
    let bytes = level_dat::write(&level).expect("encodes");
    let reread = level_dat::read(&bytes).expect("decodes");

    assert_eq!(reread.data_version().expect("has DataVersion"), 99999);

    // Every other field must be untouched: rebuild `original` with just the
    // DataVersion patched in-place and compare trees, rather than assuming
    // encoder byte-stability (gzip/NBT don't guarantee identical bytes for
    // an identical tree across two encode calls in general, though this
    // implementation happens to be deterministic — the tree comparison is
    // the real invariant either way).
    let mut expected = original;
    expected.set_data_version(99999).expect("field exists");
    assert_eq!(reread, expected);
}
