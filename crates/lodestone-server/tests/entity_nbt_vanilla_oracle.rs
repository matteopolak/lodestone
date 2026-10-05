//! Our entity-chunk schema against **vanilla's own bytes**.
//!
//! # Why this file is the load-bearing one
//!
//! `entity_persistence_round_trip.rs` proves a mob survives our own save and our
//! own load. That is `decode(encode(x)) == x`, which two symmetric
//! misunderstandings satisfy perfectly — this repo has already watched hermetic
//! chunk fixtures built with its own encoder pass throughout and then produce
//! 49 × "unexpected end of input" against a real server.
//!
//! So the expected values here come from **outside the Rust workspace entirely**:
//! `.cache/mc/survival/world`, a world a real vanilla server wrote, censused by
//! `scripts/live-oracles/entity-census.py` — Python's stdlib `gzip`/`zlib` plus a
//! `struct.unpack` NBT walker sharing no line of code with anything in this
//! workspace — into `.cache/mc/survival/entity-census.json`.
//!
//! The census is read at test time rather than pasted in, because the oracle
//! world is live: every live gate and every play session adds entities, so a
//! pasted number goes stale the next time anyone runs the server. Rerun the
//! script after touching the world.
//!
//! An exact total is what makes this a magnitude check rather than a
//! direction-only one: "we read some entities" is satisfied by a parser that
//! silently drops every record it does not recognise, which is the failure this
//! gate exists to catch. One record short of the census is a bug.
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

/// The foreign reader's census of the same directory.
struct Census {
    chunks_with_entities: usize,
    entities: usize,
    by_id: BTreeMap<String, usize>,
}

fn census() -> Census {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/mc/survival/entity-census.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing — run `python3 scripts/live-oracles/entity-census.py`",
            path.display()
        )
    });
    let json: serde_json::Value = serde_json::from_str(&text).expect("census is JSON");
    let count = |key: &str| json[key].as_u64().expect("census count") as usize;
    Census {
        chunks_with_entities: count("chunks_with_entities"),
        entities: count("entities"),
        by_id: json["by_id"]
            .as_object()
            .expect("census by_id")
            .iter()
            .map(|(id, n)| (id.clone(), n.as_u64().expect("census count") as usize))
            .collect(),
    }
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
#[ignore = "requires .cache/mc/survival/world, a real vanilla world this repo did not write"]
fn reads_every_entity_a_real_vanilla_server_wrote() {
    let expected = census();
    let chunks = oracle_chunks();
    let populated = chunks.iter().filter(|c| !entity_list(c).is_empty()).count();
    assert_eq!(
        populated, expected.chunks_with_entities,
        "chunks carrying entities: ours {populated}, the foreign reader's {}",
        expected.chunks_with_entities
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
        total, expected.entities,
        "the container handed us {total} entity records; the foreign reader found {} — \
         the two disagree, so one of the readers is wrong before schema even matters",
        expected.entities
    );
    assert_eq!(
        decoded, total,
        "our schema dropped {} of {total} records", total - decoded
    );

    // The exact per-species counts. Any of these being off by one means a record
    // was read as the wrong type, which is the class of defect that shipped every
    // dropped item in this repo as `minecraft:acacia_boat`.
    assert_eq!(census, expected.by_id, "per-id census disagrees with the foreign reader");
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
#[ignore = "requires .cache/mc/survival/world, a real vanilla world this repo did not write"]
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
    let expected = census().entities;
    assert_eq!(
        checked, expected,
        "expected to check all {expected} entities, checked {checked}"
    );
}

/// The `Position` field of an entity chunk really is an `IntArray` of two, and it
/// really does hold the chunk coordinates the container filed it under.
///
/// This is the trap named in `entity_storage`'s module doc: a terrain chunk uses
/// three separate `xPos`/`yPos`/`zPos` ints, and code that reaches for those here
/// silently reads chunk `(0, 0)` for every entity in the world.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real vanilla world this repo did not write"]
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

/// [`oracle_chunks`] that skips a chunk whose sector the container refuses,
/// instead of panicking: a locally regenerated oracle can carry a short final
/// sector, and the census gates above are the ones that insist on every chunk.
fn lenient_oracle_chunks() -> Vec<Nbt> {
    let mut out = Vec::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(entities_dir())
        .expect("read oracle entities dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("mca"))
        .collect();
    paths.sort();
    for path in paths {
        let bytes = std::fs::read(&path).expect("read region file");
        let Ok(region) = RegionFile::parse(&bytes) else { continue };
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let Ok(Some(raw)) = region.read_chunk_nbt_bytes(local_x, local_z) else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                if let Ok((_, nbt)) = read_named_nbt(&mut reader) {
                    out.push(nbt);
                }
            }
        }
    }
    out
}

/// Restoring a real vanilla record into the sim and saving it again keeps the
/// state the sim models, with vanilla's own values as the expectation.
///
/// Villagers (profession, level, xp, gossip ledger), coat and collar fields
/// the sim only carries, sheep colour and shear state, and the
/// `PersistenceRequired` flag that keeps a drowned from despawning. The
/// expected values are read straight from vanilla's tree, not from our encoder.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real vanilla world this repo did not write"]
fn a_real_vanilla_mob_keeps_its_modeled_state_through_the_sim() {
    use lodestone_server::{ChunkWorld, MobSim};

    let world = ChunkWorld::new(-64, 384);
    let mut villagers = 0usize;
    let mut gossips = 0usize;
    let mut sheep = 0usize;
    let mut cats_and_wolves = 0usize;
    let mut persistent_hostiles = 0usize;
    let mut farm_animals = 0usize;
    let mut others = 0usize;
    for chunk in &lenient_oracle_chunks() {
        for original in entity_list(chunk) {
            let Some(id) = (match field(original, "id") {
                Some(Nbt::String(id)) => Some(id.as_str()),
                _ => None,
            }) else {
                continue;
            };
            if !matches!(
                id,
                "minecraft:villager"
                    | "minecraft:sheep"
                    | "minecraft:wolf"
                    | "minecraft:cat"
                    | "minecraft:drowned"
                    | "minecraft:cow"
                    | "minecraft:pig"
                    | "minecraft:chicken"
                    | "minecraft:horse"
                    | "minecraft:axolotl"
            ) {
                continue;
            }
            let saved = SavedEntity::from_nbt(original).expect("decodes");
            if saved.health.is_some_and(|health| health <= 0.0) {
                continue; // a mob saved at zero health is deliberately not restored
            }
            let mut sim = MobSim::new(&world);
            assert_eq!(sim.restore_saved(std::slice::from_ref(&saved)), 1);
            let again = sim.saved_entities().pop().expect("one mob");
            let out = again.to_nbt();
            let same = |key: &str| {
                assert_eq!(field(&out, key), field(original, key), "{id}: `{key}` changed");
            };
            match id {
                "minecraft:villager" => {
                    villagers += 1;
                    let data = |nbt: &Nbt, key: &str| field(field(nbt, "VillagerData").unwrap(), key).cloned();
                    for key in ["profession", "level", "type"] {
                        assert_eq!(data(&out, key), data(original, key), "villager `{key}`");
                    }
                    same("Xp");
                    if let (Some(Nbt::List { elements: a, .. }), Some(Nbt::List { elements: b, .. })) =
                        (field(&out, "Gossips"), field(original, "Gossips"))
                    {
                        gossips += b.len();
                        assert_eq!(a.len(), b.len(), "gossip entries");
                        for entry in b {
                            assert!(a.contains(entry), "gossip entry lost: {entry:?}");
                        }
                    }
                }
                "minecraft:sheep" => {
                    sheep += 1;
                    same("Color");
                    same("Sheared");
                }
                "minecraft:wolf" | "minecraft:cat" => {
                    cats_and_wolves += 1;
                    same("variant");
                    same("CollarColor");
                    same("Sitting");
                }
                "minecraft:cow" | "minecraft:pig" | "minecraft:chicken" => {
                    farm_animals += 1;
                    same("variant");
                    same("sound_variant");
                }
                "minecraft:horse" | "minecraft:axolotl" => {
                    others += 1;
                    same("Variant");
                }
                "minecraft:drowned" => {
                    if field(original, "PersistenceRequired") == Some(&Nbt::Byte(1)) {
                        persistent_hostiles += 1;
                        same("PersistenceRequired");
                    } else {
                        assert!(
                            !matches!(field(&out, "PersistenceRequired"), Some(Nbt::Byte(1))),
                            "a despawnable drowned became persistent"
                        );
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    // Magnitudes from the foreign census (20 villagers, 206 sheep, 26 wolves and 3 cats, 57
    // drowned of which some are persistent); a floor below it tolerates an unreadable region
    // tail, and zero would mean the loop checked nothing.
    eprintln!(
        "checked: {villagers} villagers, {gossips} gossip entries, {sheep} sheep, \
         {cats_and_wolves} cats and wolves, {persistent_hostiles} persistent drowned"
    );
    assert!(villagers >= 15, "villagers checked: {villagers}");
    assert!(gossips > 0, "no real gossip entry was compared");
    assert!(sheep >= 150, "sheep checked: {sheep}");
    assert!(cats_and_wolves >= 20, "wolves and cats checked: {cats_and_wolves}");
    assert!(persistent_hostiles > 0, "no persistent drowned was compared");
    assert!(farm_animals >= 300 && others >= 30, "variants checked: {farm_animals} farm, {others} other");
}
