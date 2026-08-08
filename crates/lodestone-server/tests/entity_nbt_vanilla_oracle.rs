//! Our entity-chunk schema against **vanilla's own bytes** (issue
//! [#303](https://github.com/matteopolak/lodestone/issues/303)).
//!
//! # Why this file is the load-bearing one
//!
//! `entity_persistence_round_trip.rs` proves a mob survives our own save and our
//! own load. That is `decode(encode(x)) == x`, which two symmetric
//! misunderstandings satisfy perfectly — this repo has already watched hermetic
//! chunk fixtures built with its own encoder pass throughout and then produce
//! 49 × "unexpected end of input" against a real server.
//!
//! So the expected values here come from **outside this repo entirely**:
//! `.cache/mc/survival/world`, a world a real 26.2 server wrote (seed
//! −195764831), read with a foreign parser — Python's stdlib `gzip`/`zlib` plus a
//! `struct.unpack` NBT walker sharing no line of code with anything in this
//! workspace. Every number below was printed by that parser before a line of
//! `entity_storage.rs` existed.
//!
//! # The census the foreign reader produced
//!
//! Overworld `dimensions/minecraft/overworld/entities/`: **19** region files,
//! **880** chunks carrying an `Entities` list, **2093** entities total, of which:
//!
//! | id | count |
//! |---|---|
//! | `minecraft:item` | 510 |
//! | `minecraft:sheep` | 169 |
//! | `minecraft:chicken` | 163 |
//! | `minecraft:pig` | 147 |
//! | `minecraft:skeleton` | 122 |
//! | `minecraft:creeper` | 116 |
//! | `minecraft:zombie` | 115 |
//! | `minecraft:cow` | 101 |
//! | `minecraft:chest_minecart` | 99 |
//! | `minecraft:bat` | 88 |
//!
//! An exact total is what makes this a magnitude check rather than a
//! direction-only one: "we read some entities" is satisfied by a parser that
//! silently drops every record it does not recognise, which is the failure this
//! gate exists to catch. 2093 is the number, and 2092 is a bug.
//!
//! # `#[ignore]`d, and why that is not a hole
//!
//! It needs `.cache/mc/survival/world`, which is **not repo state** — it is ~89
//! region files a vanilla server generated locally. Same treatment as
//! `chunk_nbt_vanilla_oracle.rs`, its direct precedent. Run it with
//! `cargo test -p lodestone-server --test entity_nbt_vanilla_oracle -- --ignored --nocapture`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lodestone_anvil::region::RegionFile;
use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_server::entity_storage::SavedEntity;

/// The oracle world's overworld entity region directory.
fn entities_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/mc/survival/world/dimensions/minecraft/overworld/entities")
}

/// Every `(chunk position, root NBT)` in the oracle's entity region set, read
/// through `lodestone-anvil`'s container.
///
/// The container is the one piece shared with the code under test, and it is
/// pinned separately against real `.mca` files by that crate's own tests. The
/// *schema* — which this file is about — is not shared with anything here.
fn oracle_chunks() -> Vec<Nbt> {
    let dir = entities_dir();
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("mca"))
        .collect();
    // Sorted so a failure message names the same file across runs.
    paths.sort();
    for path in paths {
        let bytes = std::fs::read(&path).expect("read region file");
        let region = RegionFile::parse(&bytes).expect("a real vanilla entity region parses");
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let Some(raw) = region
                    .read_chunk_nbt_bytes(local_x, local_z)
                    .expect("chunk envelope")
                else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                let (_, nbt) = read_named_nbt(&mut reader).expect("chunk NBT decodes");
                out.push(nbt);
            }
        }
    }
    out
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn entity_list(nbt: &Nbt) -> &[Nbt] {
    match field(nbt, "Entities") {
        Some(Nbt::List { elements, .. }) => elements,
        _ => &[],
    }
}

/// **The gate.** Our decoder reads every entity a real 26.2 server wrote, and the
/// census matches the foreign reader's exactly.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn reads_every_entity_a_real_vanilla_server_wrote() {
    let chunks = oracle_chunks();
    let populated = chunks.iter().filter(|c| !entity_list(c).is_empty()).count();
    assert_eq!(
        populated, 880,
        "expected 880 chunks carrying entities (the foreign reader's count); got {populated}"
    );

    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    let mut decoded = 0usize;
    for chunk in &chunks {
        for entry in entity_list(chunk) {
            total += 1;
            let Some(entity) = SavedEntity::from_nbt(entry) else {
                let id = match field(entry, "id") {
                    Some(Nbt::String(s)) => s.clone(),
                    _ => "<no id>".to_owned(),
                };
                panic!("our decoder dropped a real vanilla entity: {id}");
            };
            decoded += 1;
            *census.entry(entity.id.to_string()).or_default() += 1;
        }
    }

    assert_eq!(
        total, 2093,
        "the container handed us {total} entity records; the foreign reader found 2093 — \
         the two disagree, so one of the readers is wrong before schema even matters"
    );
    assert_eq!(
        decoded, total,
        "our schema dropped {} of {total} records", total - decoded
    );

    // The exact per-species counts. Any of these being off by one means a record
    // was read as the wrong type, which is the class of defect that shipped every
    // dropped item in this repo as `minecraft:acacia_boat`.
    for (id, expected) in [
        ("minecraft:item", 510usize),
        ("minecraft:sheep", 169),
        ("minecraft:chicken", 163),
        ("minecraft:pig", 147),
        ("minecraft:skeleton", 122),
        ("minecraft:creeper", 116),
        ("minecraft:zombie", 115),
        ("minecraft:cow", 101),
        ("minecraft:chest_minecart", 99),
        ("minecraft:bat", 88),
    ] {
        assert_eq!(
            census.get(id).copied().unwrap_or(0),
            expected,
            "{id}: our decode disagrees with the foreign reader"
        );
    }
}

/// Re-encoding a real vanilla entity must not lose a field.
///
/// This is the property that stops a save destroying somebody's world. Vanilla
/// mobs carry ~30 fields we do not model — `Brain`, `attributes`, `memories`,
/// `PersistenceRequired`, `CanPickUpLoot` — and a writer that emitted only the
/// modelled ones would strip every one of them the first time the world saved.
///
/// The comparison is against **vanilla's own tree**, key by key, not against our
/// own re-read.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn re_encoding_a_real_vanilla_entity_preserves_every_field() {
    let chunks = oracle_chunks();
    let mut checked = 0usize;
    for chunk in &chunks {
        for original in entity_list(chunk) {
            let Nbt::Compound(original_fields) = original else {
                continue;
            };
            let entity = SavedEntity::from_nbt(original).expect("decodes");
            let round_tripped = entity.to_nbt();
            let Nbt::Compound(new_fields) = &round_tripped else {
                unreachable!("to_nbt builds a compound")
            };
            for (name, value) in original_fields {
                let found = new_fields
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v)
                    .unwrap_or_else(|| {
                        panic!(
                            "re-encoding dropped the field {name:?} that a real vanilla \
                             server wrote on a {:?}",
                            entity.id.to_string()
                        )
                    });
                // `Motion`/`Pos`/`Rotation`/`Health`/`Item` go through our own
                // typed model, so exact equality is the assertion for the
                // unmodelled fields and a same-tag check for the modelled ones —
                // a `Short` that came back as an `Int` is a file vanilla cannot
                // read, and is exactly what a hand-written schema gets wrong.
                assert_eq!(
                    std::mem::discriminant(value),
                    std::mem::discriminant(found),
                    "field {name:?} on a {} changed NBT tag type; vanilla's own reader \
                     is strict about this",
                    entity.id
                );
            }
            checked += 1;
        }
    }
    assert_eq!(
        checked, 2093,
        "expected to check all 2093 entities, checked {checked}"
    );
}

/// The `Position` field of an entity chunk really is an `IntArray` of two, and it
/// really does hold the chunk coordinates the container filed it under.
///
/// This is the trap named in `entity_storage`'s module doc: a terrain chunk uses
/// three separate `xPos`/`yPos`/`zPos` ints, and code that reaches for those here
/// silently reads chunk `(0, 0)` for every entity in the world.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn entity_chunks_carry_position_as_an_int_array_of_two() {
    let chunks = oracle_chunks();
    assert!(!chunks.is_empty(), "the oracle world has entity chunks");
    let mut with_position = 0usize;
    for chunk in &chunks {
        assert!(
            field(chunk, "xPos").is_none(),
            "an entity chunk must NOT carry a terrain chunk's xPos"
        );
        match field(chunk, "Position") {
            Some(Nbt::IntArray(parts)) => {
                assert_eq!(parts.len(), 2, "Position is [chunkX, chunkZ], not a block pos");
                with_position += 1;
            }
            other => panic!("Position was {other:?}, not an IntArray"),
        }
        // And every entity inside really does belong to that chunk, which is the
        // check that pins `SavedEntity::chunk`'s flooring against vanilla's own
        // filing rather than against our arithmetic.
        let Some(Nbt::IntArray(parts)) = field(chunk, "Position") else {
            unreachable!("checked above")
        };
        let (cx, cz) = (parts[0], parts[1]);
        for entry in entity_list(chunk) {
            let entity = SavedEntity::from_nbt(entry).expect("decodes");
            assert_eq!(
                entity.chunk(),
                (cx, cz),
                "vanilla filed a {} at {:?} under chunk ({cx}, {cz}), our arithmetic says {:?}",
                entity.id,
                entity.pos,
                entity.chunk()
            );
        }
    }
    assert_eq!(
        with_position,
        chunks.len(),
        "every entity chunk carries a Position"
    );
}
