//! Generator + drift guard for `src/generated/entity_types.rs`, and the wire
//! oracle that makes it an authority rather than a cross-check.
//!
//! # Why this file has an oracle the 1.14 era's equivalent does not
//!
//! 1.14.4's and 1.15.2's entity tables come from those jars' own `--reports`
//! registry dumps. **1.13.2's data generator emits no registry report at
//! all** — that provider arrived in 1.14 — so the only machine-readable
//! source for the numbering is `minecraft-data`, which this repo treats as
//! cross-check-grade.
//!
//! It is right to. `minecraft-data`'s 1.13.2 `entities.json` holds 123 rows
//! for a 95-entry registry: 28 of them are stale pre-1.13 *object* ids
//! carried forward from 1.12's second, separate id space, and one of those
//! carries the name `area_effect cloud`, with a space, which is not an
//! identifier in any version. Reading that file naively gives a table where
//! id 1 is both `armor_stand` and `boat` and id 3 is both `bat` and a
//! misspelling. The generator below keeps only the rows 1.13's *unified*
//! registry actually numbers (`type = "mob"`, dense `0..=94`) and drops the
//! legacy object rows, which is a judgement call — so it is checked, not
//! trusted.
//!
//! # The wire oracle
//!
//! `tests/captures/entity_types_1_13_2.txt` is a transcript of a real 1.13.2
//! server being asked, over RCON, to `summon minecraft:<name>` — one name at
//! a time — and the numeric type id it then put in the `spawn_entity_living`
//! or `spawn_entity` packet that followed. The name goes in from vanilla's
//! own registry (a name it refuses is recorded as refused, not guessed at)
//! and the id comes back off vanilla's own wire; this crate is on neither
//! side of the comparison. Every id the transcript covers must match the
//! committed table, and that check runs on every `cargo test`.
//!
//! Not every entity can be summoned — `player` and `fishing_bobber` have no
//! `/summon` form, and a few refuse to spawn in a flat overworld — so the
//! transcript covers most of the registry rather than all of it. The
//! coverage floor is asserted, so a transcript that silently stopped covering
//! anything would fail rather than pass trivially.
//!
//! # Regenerating
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-13 --test entity_types \
//!     committed_tables_match_their_sources -- --ignored --nocapture
//! ```
//!
//! and, for the oracle (needs `./scripts/live-oracles/legacy.sh 1.13.2`):
//!
//! ```text
//! cargo test -p lodestone-v1-13 --test entity_types -- --ignored --nocapture \
//!     record_entity_type_ids_from_the_wire
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_v1_13::entity_types;

/// `minecraft-data`'s own 1.13.2 entity list, read from the vendor tree.
///
/// A runtime read rather than an `include_str!`, because `/vendor/` is
/// git-ignored: it is a developer's local checkout of a third-party dataset,
/// not repo state. A compile-time read makes the whole test binary
/// unbuildable wherever that tree is absent — every CI runner, every fresh
/// clone — even though the only caller is the ignored generator below, which
/// a developer runs deliberately and with the dataset in hand. The wire
/// oracle in `tests/captures/` is what checks this table on an ordinary
/// `cargo test`, and it is committed.
///
/// `lodestone-data`'s own light-properties test states the same rule for the
/// opposite resolution: where a check must run everywhere, commit an extract
/// of the dataset instead of reaching into `vendor/`.
fn minecraft_data_entities() -> String {
    let path = manifest_dir().join("../../../vendor/minecraft-data/data/pc/1.13.2/entities.json");
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} is required to regenerate or drift-check the 1.13 entity table, and is \
             absent ({err}). `/vendor/` is git-ignored; populate it with a minecraft-data \
             checkout. 1.13.2's data generator emits no registry report, so this dataset \
             is the only machine-readable source for the numbering.",
            path.display()
        )
    })
}

/// The wire transcript this table is checked against.
const WIRE_ORACLE: &str = include_str!("captures/entity_types_1_13_2.txt");

/// Every entity name the 1.13.2 jar itself ships, extracted from its own
/// language file — the authority for *spellings* (never for ids or ordering).
const JAR_ENTITY_NAMES: &str = include_str!("support/entity_names_1_13_2_jar.txt");

/// Protocol this table serves.
const PROTOCOL: i32 = 404;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The set of names the jar itself ships.
fn jar_entity_names() -> std::collections::HashSet<&'static str> {
    JAR_ENTITY_NAMES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Corrects the three malformed names in `minecraft-data`'s 1.13.2 entity
/// list.
///
/// * id 80 reads `iron_golem}`, with a stray closing brace;
/// * id 33 reads `fireworks_rocket`, which vanilla spells `firework_rocket`;
/// * id 44 reads `commandblock_minecart`, which vanilla spells
///   `command_block_minecart`.
///
/// None of the replacements is a guess at what was meant. All three are
/// spellings the 1.13.2 jar's own language file carries and the malformed
/// forms do not (`tests/support/entity_names_1_13_2_jar.txt`), and vanilla's
/// own `/summon` rejects each malformed form outright — "Unknown entity:
/// minecraft:fireworks_rocket" — which is how they were found rather than
/// assumed.
///
/// It is a correction table rather than a general repair for a reason:
/// [`parse_unified_registry`] asserts that every *resulting* name is one the
/// jar ships, so a fourth corrupt row fails loudly rather than being silently
/// normalised into something plausible.
fn correct_name(name: &str) -> &str {
    match name {
        "iron_golem}" => "iron_golem",
        "fireworks_rocket" => "firework_rocket",
        "commandblock_minecart" => "command_block_minecart",
        other => other,
    }
}

/// Parses `minecraft-data`'s entity list into the unified 1.13 registry,
/// dropping the legacy object rows (see the module docs).
fn parse_unified_registry() -> Vec<(i32, String)> {
    let value: serde_json::Value =
        serde_json::from_str(&minecraft_data_entities()).expect("entities.json is valid JSON");
    let rows = value.as_array().expect("entities.json is an array");
    let jar = jar_entity_names();
    let mut by_id: BTreeMap<i32, String> = BTreeMap::new();
    for row in rows {
        if row["type"].as_str() != Some("mob") {
            continue;
        }
        let id = i32::try_from(row["id"].as_i64().expect("entity id is an integer"))
            .expect("entity id fits i32");
        let name = correct_name(row["name"].as_str().expect("entity name is a string"));
        assert!(
            jar.contains(name),
            "entity id {id} has the name {name:?}, which the 1.13.2 jar's own language \
             file does not ship; minecraft-data's 1.13.2 list is known to misspell \
             three, so a fourth needs its own outside source before it is normalised \
             away"
        );
        let previous = by_id.insert(id, format!("minecraft:{name}"));
        assert!(
            previous.is_none(),
            "duplicate unified entity id {id}: {previous:?} and {name}"
        );
    }
    let ids: Vec<i32> = by_id.keys().copied().collect();
    assert_eq!(
        ids,
        (0..i32::try_from(ids.len()).expect("registry fits i32")).collect::<Vec<_>>(),
        "the unified registry must be dense from 0; a gap means the mob/object \
         split was filtered wrongly"
    );
    by_id.into_iter().collect()
}

/// Renders the committed generated file from the parsed registry.
fn generate(entries: &[(i32, String)]) -> String {
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-13 --test entity_types -- --ignored`\n",
    );
    out.push_str("// from vendor/minecraft-data/data/pc/1.13.2/entities.json (protocol 404 /\n");
    out.push_str("// Minecraft 1.13.2). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1\n");
    out.push_str("// (see the test module docs).\n");
    out.push_str("//! Generated entity-type id->name table for protocol 404 (Minecraft 1.13.2).\n");
    out.push_str("//!\n");
    out.push_str("//! Maps the unified numeric entity-type id space to canonical\n");
    out.push_str("//! `minecraft:*` identifiers. Consumed by [`crate::entity_types`].\n\n");
    out.push_str("/// Number of entity_types type entries.\n");
    let _ = writeln!(out, "pub const ENTITY_TYPE_COUNT: usize = {};\n", entries.len());
    out.push_str("/// `(type id, canonical identifier)` pairs, sorted by id for binary search.\n");
    let _ = writeln!(
        out,
        "pub static ENTITY_TYPES: [(i32, &str); {}] = [",
        entries.len()
    );
    for (id, name) in entries {
        let _ = writeln!(out, "    ({id}, \"{name}\"),");
    }
    out.push_str("];\n");
    out
}

/// Renders `src/generated/object_types.rs` from the wire transcript alone.
///
/// The object id space has **no** machine-readable source this repo trusts:
/// 1.13.2's jar emits no registry report, and `minecraft-data`'s own object
/// rows carry names that are not identifiers (`area_effect cloud`,
/// `eye_of ender`, `thrown_exp bottle`, `armorstand`, `falling_objects`). The
/// transcript has neither problem, because the name in it is the one vanilla's
/// `/summon` accepted and the id is the one vanilla then put on the wire.
///
/// Coverage is therefore partial by construction — only entities that can be
/// summoned and that spawn through `spawn_entity` appear — and that is the
/// honest outcome: an id the table does not carry makes the adapter report an
/// unknown object type rather than name the wrong entity.
fn generate_object_table(rows: &[(String, bool, i32)]) -> String {
    let mut by_id: BTreeMap<i32, String> = BTreeMap::new();
    for (name, living, id) in rows {
        if *living {
            continue;
        }
        match by_id.get(id) {
            None => {
                by_id.insert(*id, name.clone());
            }
            Some(existing) if existing == name => {}
            Some(existing) => {
                let family = object_family(*id).unwrap_or_else(|| {
                    panic!(
                        "object id {id} names both {existing} and {name} in the transcript, and \
                         is not a known family id; re-record rather than picking a winner"
                    )
                });
                by_id.insert(*id, family.to_owned());
            }
        }
    }
    // A family id must survive even if its own base name happened to be
    // recorded last.
    for (id, name) in &mut by_id {
        if let Some(family) = object_family(*id) {
            *name = family.to_owned();
        }
    }

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-13 --test entity_types -- --ignored`\n",
    );
    out.push_str("// from tests/captures/entity_types_1_13_2.txt -- ids a real 1.13.2 server put\n");
    out.push_str("// on the wire. DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1\n");
    out.push_str("// (see the test module docs).\n");
    out.push_str("//! Generated object-type id->name table for protocol 404 (Minecraft 1.13.2).\n");
    out.push_str("//!\n");
    out.push_str("//! A **second** id space, distinct from the unified entity registry: at 404\n");
    out.push_str("//! `spawn_entity` still carries the pre-1.13 object type numbering. Consumed\n");
    out.push_str("//! by [`crate::entity_types`].\n\n");
    out.push_str("/// Number of object type entries the wire oracle covers.\n");
    let _ = writeln!(out, "pub const OBJECT_TYPE_COUNT: usize = {};\n", by_id.len());
    out.push_str("/// `(object type id, canonical identifier)` pairs, sorted by id.\n");
    let _ = writeln!(
        out,
        "pub static OBJECT_TYPES: [(i32, &str); {}] = [",
        by_id.len()
    );
    for (id, name) in &by_id {
        let _ = writeln!(out, "    ({id}, \"{name}\"),");
    }
    out.push_str("];\n");
    out
}

/// Parses the wire transcript into `(name, which spawn packet, type id)`
/// rows, skipping comments and the `refused` rows.
fn parse_wire_oracle() -> Vec<(String, bool, i32)> {
    let mut out = Vec::new();
    for line in WIRE_ORACLE.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let name = parts.next().expect("oracle line has a name").to_owned();
        let kind = parts.next().expect("oracle line has a kind");
        if kind == "refused" {
            continue;
        }
        let id = parts.next().expect("oracle line has an id");
        out.push((
            name,
            kind == "living",
            id.parse().expect("oracle id is an integer"),
        ));
    }
    out
}

/// Collapses the object ids the wire gives several names, and says why.
///
/// Exactly one id needs it, measured: **10**, which the transcript records for
/// `minecart`, `chest_minecart`, `furnace_minecart`, `hopper_minecart`,
/// `spawner_minecart` and `tnt_minecart` alike. That is not contamination, it
/// is what the pre-1.13 object id space *is*: one id for the family, with the
/// variant carried in the `spawn_entity` packet's own `object_data` field. An
/// adapter that reads only the type id can honestly name the family and no
/// more, so that is what the generated table says.
///
/// Any *other* colliding id would be a defect in the recording, and
/// [`generate_object_table`] fails on one rather than picking a winner.
fn object_family(id: i32) -> Option<&'static str> {
    match id {
        10 => Some("minecraft:minecart"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hermetic tests (always run).
// ---------------------------------------------------------------------------

/// The committed table agrees with every id a real 1.13.2 server put on the
/// wire.
///
/// This is what turns a community dataset into an authority for this table:
/// the expected value on each row was chosen by vanilla, twice over — the
/// name by its own `/summon` parser, the number by its own entity registry.
#[test]
fn the_committed_tables_agree_with_the_wire() {
    let table = entity_types::table_for(PROTOCOL);
    let oracle = parse_wire_oracle();
    let living = oracle.iter().filter(|(_, living, _)| *living).count();
    let objects = oracle.len() - living;
    assert!(
        living >= 50 && objects >= 15,
        "the wire transcript covers {living} living and {objects} object spawns; \
         it is supposed to cover most of a 95-entry registry, so this is a \
         truncated recording rather than a passing check"
    );
    for (name, is_living, id) in &oracle {
        if *is_living {
            assert_eq!(
                table.mob_type_name(*id),
                Some(name.as_str()),
                "a real 1.13.2 server spawned {name} in spawn_entity_living with type \
                 id {id}, and the committed unified registry disagrees"
            );
        } else {
            let expected = object_family(*id).unwrap_or(name.as_str());
            assert_eq!(
                table.object_type_name(*id),
                Some(expected),
                "a real 1.13.2 server spawned {name} in spawn_entity with type id {id}, \
                 and the committed object table disagrees"
            );
        }
    }
}

/// **The finding this file exists for.** The two spawn packets index two
/// *different* id spaces at 404, and the transcript proves it rather than
/// asserting it.
///
/// 1.13 unified the entity *registry* — one alphabetical table where 1.12 had
/// a mob table and an object table — but `spawn_entity` at protocol 404 still
/// carries the **pre-1.13 object numbering**, and only `spawn_entity_living`
/// uses the unified one. Measured, from the transcript: a real server spawned
/// `armor_stand` through `spawn_entity` with type id **78**, and id 78 in the
/// unified registry is `vex`; it spawned `boat` with id **1**, where the
/// unified registry has `armor_stand`. An adapter that resolved an object
/// spawn through the unified table would therefore name a real, wrong entity
/// for every object on the wire — no error anywhere.
///
/// The probes below are chosen so no pair of answers can coincide.
#[test]
fn the_object_id_space_is_not_the_unified_registry() {
    let table = entity_types::table_for(PROTOCOL);
    for (id, object, unified) in [
        (1, "minecraft:boat", "minecraft:armor_stand"),
        (78, "minecraft:armor_stand", "minecraft:vex"),
        (70, "minecraft:falling_block", "minecraft:squid"),
        (60, "minecraft:arrow", "minecraft:shulker_bullet"),
    ] {
        assert_eq!(table.object_type_name(id), Some(object));
        assert_eq!(table.mob_type_name(id), Some(unified));
        assert_ne!(object, unified);
    }
}

/// One object id names a whole family, and the table says so rather than
/// picking one of them.
///
/// Every minecart variant shares object type **10** on this wire — the variant
/// travels in `spawn_entity`'s own `object_data` field, which the type id
/// alone cannot recover. The transcript records six names against that id;
/// the generated table records the family.
#[test]
fn the_minecart_family_shares_one_object_id() {
    let table = entity_types::table_for(PROTOCOL);
    assert_eq!(table.object_type_name(10), Some("minecraft:minecart"));
    let minecarts = parse_wire_oracle()
        .into_iter()
        .filter(|(name, living, id)| !*living && *id == 10 && name.contains("minecart"))
        .count();
    assert!(
        minecarts >= 5,
        "the transcript should carry several minecart variants at object id 10, \
         which is the evidence that the id is a family; it carries {minecarts}"
    );
}

/// The generated file's two halves cannot drift apart unnoticed.
#[test]
fn the_declared_length_matches_the_table() {
    let table = entity_types::table_for(PROTOCOL);
    assert_eq!(table.len(), table.declared_len());
    assert_eq!(table.object_len(), table.declared_object_len());
    assert!(!table.is_empty());
    assert_eq!(table.len(), 95, "1.13.2's unified entity registry has 95 entries");
    assert!(
        table.object_len() >= 20,
        "the object table covers only {} ids; it is generated from the wire \
         transcript, so a shrinking count means a truncated recording",
        table.object_len()
    );
}

/// The unified registry's own bounds.
#[test]
fn the_unified_registry_is_dense_and_bounded() {
    let table = entity_types::table_for(PROTOCOL);
    assert_eq!(table.mob_type_name(1), Some("minecraft:armor_stand"));
    assert_eq!(table.mob_type_name(94), Some("minecraft:trident"));
    assert_eq!(table.mob_type_name(95), None);
    assert_eq!(table.mob_type_name(-1), None);
}

/// Resolving a protocol this family does not serve must panic rather than
/// answer with a neighbouring registry, which would name a real but wrong
/// mob.
#[test]
#[should_panic(expected = "outside this family's PROTOCOLS")]
fn a_foreign_protocol_has_no_entity_table() {
    let _ = entity_types::table_for(498);
}

// ---------------------------------------------------------------------------
// Generator (ignored).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed tables; run explicitly"]
fn committed_tables_match_their_sources() {
    let regen = std::env::var_os("LODESTONE_REGEN").is_some();
    for (relative, generated) in [
        (
            "src/generated/entity_types.rs",
            generate(&parse_unified_registry()),
        ),
        (
            "src/generated/object_types.rs",
            generate_object_table(&parse_wire_oracle()),
        ),
    ] {
        let path = manifest_dir().join(relative);
        if regen {
            std::fs::write(&path, &generated).expect("write committed table");
            eprintln!("regenerated {}", path.display());
            continue;
        }
        let committed = std::fs::read_to_string(&path).expect("committed table present");
        assert_eq!(
            generated, committed,
            "{relative} is stale vs its source; regenerate with LODESTONE_REGEN=1 \
             (see the test module docs)"
        );
    }
}

// ---------------------------------------------------------------------------
// Wire oracle recorder (ignored; needs a live server).
// ---------------------------------------------------------------------------

/// Asks a real 1.13.2 server to summon each entity in the committed table and
/// records the numeric type id it puts on the wire.
///
/// Deliberately drives the join with this crate's adapter but reads the type
/// id out of the **raw** packet body rather than out of a decoded event: the
/// point is to measure what the server sent, and routing it through
/// `entity_types` first would make the transcript agree with the table by
/// construction.
#[tokio::test]
#[ignore = "records against a live 1.13.2 server: ./scripts/live-oracles/legacy.sh 1.13.2"]
async fn record_entity_type_ids_from_the_wire() {
    use lodestone_model::{
        ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
    };
    use lodestone_net::Connection;
    use lodestone_testsupport::{RconClient, unique_username};
    use lodestone_world::World;
    use std::time::Duration;

    /// The oracle's game and RCON ports, from `scripts/live-oracles/legacy.sh`.
    const GAME_PORT: u16 = 25590;
    const RCON_PORT: u16 = 25591;
    /// Tag put on each summoned entity so its uuid can be read back.
    const PROBE_TAG: &str = "lodestoneprobe";

    /// NBT a few entities need before vanilla will summon them at all.
    ///
    /// `/summon minecraft:item` and `/summon minecraft:potion` both answer
    /// "Unable to summon entity" with no NBT, because an item entity with no
    /// stack and a potion with no bottle are not valid entities. Supplying
    /// the minimum each needs is what puts them in the transcript rather than
    /// leaving two of the commonest object ids uncovered. The contents are
    /// irrelevant to what is being measured — only the type id is read — so
    /// the cheapest valid stack is used.
    fn extra_summon_nbt(name: &str) -> &'static str {
        match name {
            "minecraft:item" => "Item:{id:\"minecraft:stone\",Count:1b},",
            "minecraft:potion" => "Potion:{id:\"minecraft:splash_potion\",Count:1b},",
            _ => "",
        }
    }

    /// Reads the tagged probe entity's own uuid out of vanilla's entity NBT.
    ///
    /// `data get entity ... UUIDMost` answers with a line ending in the long
    /// and an `L` suffix, so the number is taken from the last whitespace-
    /// separated token rather than by position.
    fn probe_uuid(rcon: &mut RconClient) -> Option<[u8; 16]> {
        fn long(reply: &str) -> Option<i64> {
            reply
                .split_whitespace()
                .next_back()?
                .trim_end_matches('L')
                .parse()
                .ok()
        }
        let most = long(
            &rcon
                .command(&format!(
                    "data get entity @e[tag={PROBE_TAG},limit=1] UUIDMost"
                ))
                .ok()?,
        )?;
        let least = long(
            &rcon
                .command(&format!(
                    "data get entity @e[tag={PROBE_TAG},limit=1] UUIDLeast"
                ))
                .ok()?,
        )?;
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&most.to_be_bytes());
        bytes[8..].copy_from_slice(&least.to_be_bytes());
        Some(bytes)
    }
    let table = entity_types::table_for(PROTOCOL);
    let names: Vec<(i32, &'static str)> = (0..i32::try_from(table.len()).expect("fits i32"))
        .filter_map(|id| table.entity_type_name(id).map(|name| (id, name)))
        .collect();

    let adapter = lodestone_v1_13::adapter_for(PROTOCOL);
    let profile = LoginProfile {
        username: unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: GAME_PORT,
    };
    let mut conn = Connection::connect(("127.0.0.1", GAME_PORT))
        .await
        .expect("connect to the 1.13.2 oracle");
    let mut world = World::new();
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        match directive {
            Directive::Send { packet_id, payload } => {
                conn.write_packet(packet_id, &payload).await.expect("write");
            }
            Directive::SetState(next) => state = next,
            Directive::SetCompression(threshold) => conn.set_compression(threshold),
            _ => {}
        }
    }
    // Drain until Play, answering whatever the login choreography needs.
    while state != ConnectionState::Play {
        let (id, payload) = tokio::time::timeout(Duration::from_secs(10), conn.read_packet())
            .await
            .expect("login did not stall")
            .expect("read")
            .expect("server closed during login");
        for directive in adapter
            .handle_packet(&mut world, state, id, &payload)
            .expect("login packet")
        {
            match directive {
                Directive::SetState(next) => state = next,
                Directive::SetCompression(threshold) => conn.set_compression(threshold),
                Directive::Send { packet_id, payload } => {
                    conn.write_packet(packet_id, &payload).await.expect("write");
                }
                _ => {}
            }
        }
    }

    let spawn_living = clientbound_id("minecraft:spawn_entity_living");
    let spawn_object = clientbound_id("minecraft:spawn_entity");
    let mut observed: Vec<(String, Option<(Spawn, i32)>, String)> = Vec::new();
    let mut known: std::collections::HashSet<[u8; 16]> = std::collections::HashSet::new();

    // Correlating a summon with the spawn packet it caused is the whole
    // difficulty here, and timing alone does not do it: the socket backs up
    // between summons, so a naive "read until the next spawn packet" reads the
    // *previous* entity's. The loop below removes the ambiguity instead of
    // racing it — drain until the socket is quiet, summon exactly one thing,
    // then collect every spawn packet in the next window and insist there was
    // exactly one.
    //
    // It deliberately does **not** sweep the world with `kill @e` between
    // summons, which is the obvious way to keep the window clean: driving an
    // entity sweep over RCON while a 1.13.2 server ticks entities crashed it
    // outright once during a full pass, with a `ConcurrentModificationException`
    // inside vanilla's own entity-tick loop. The oracle script gives this
    // version a peaceful, spawn-free flat world instead, so nothing arrives in
    // the window that was not asked for.
    for (_, name) in &names {
        let mut discard = Vec::new();
        pump(
            &mut conn,
            &adapter,
            &mut world,
            Duration::from_millis(300),
            spawn_living,
            spawn_object,
            &mut discard,
            &mut known,
        )
        .await;

        // `execute at @p run summon` rather than a literal position: the
        // client's own spawn point is what the server tracks entities
        // against, and a summon outside its tracking range produces no spawn
        // packet at all -- silently, which is how a first pass of this
        // recorder produced a transcript of nothing.
        // A fresh RCON connection per command: vanilla performs exactly one
        // read per request and closes on anything it does not like, and a
        // half-dead reused connection shows up here as a run of bogus
        // "refused" rows rather than as an error.
        let mut rcon = RconClient::connect(("127.0.0.1", RCON_PORT), "lodestone")
            .expect("connect RCON -- start ./scripts/live-oracles/legacy.sh 1.13.2");
        // Summoned with a tag, so the entity can be asked for its own uuid
        // straight afterwards. That uuid is what correlates the command with
        // the packet: an already-populated chunk produces item drops and
        // secondary spawns of its own, and "the first new entity after the
        // summon" picks one of those often enough to matter (measured: five of
        // ninety-five rows, each landing on the same wrong id, which is how the
        // heuristic was caught).
        let reply = rcon
            .command(&format!(
                "execute at @p run summon {name} ~ ~1 ~ {{{}Tags:[\"{PROBE_TAG}\"]}}",
                extra_summon_nbt(name)
            ))
            .expect("rcon summon");
        if !reply.contains("Summoned") {
            observed.push(((*name).to_owned(), None, reply.trim().to_owned()));
            continue;
        }
        let Some(wanted) = probe_uuid(&mut rcon) else {
            observed.push((
                (*name).to_owned(),
                None,
                "summoned, but vanilla reported no tagged entity to read a uuid from".to_owned(),
            ));
            continue;
        };
        let _ = rcon.command(&format!("tag @e[tag={PROBE_TAG}] remove {PROBE_TAG}"));

        let mut seen: Vec<([u8; 16], Spawn, i32)> = Vec::new();
        pump(
            &mut conn,
            &adapter,
            &mut world,
            Duration::from_millis(700),
            spawn_living,
            spawn_object,
            &mut seen,
            &mut known,
        )
        .await;
        let matched = seen
            .iter()
            .find(|(uuid, _, _)| *uuid == wanted)
            .map(|&(_, spawn, ty)| (spawn, ty));
        observed.push((
            (*name).to_owned(),
            matched,
            if matched.is_none() {
                format!(
                    "no spawn packet carried the summoned entity's own uuid ({} other new \
                     entities in the window)",
                    seen.len()
                )
            } else {
                String::new()
            },
        ));
    }

    let mut out = String::new();
    out.push_str("# Entity type ids a real Minecraft 1.13.2 server put on the wire.\n");
    out.push_str("# Recorded by tests/entity_types.rs against\n");
    out.push_str("# ./scripts/live-oracles/legacy.sh 1.13.2: each name was handed to vanilla's\n");
    out.push_str("# own `/summon` over RCON, and the number is the type field of the\n");
    out.push_str("# spawn_entity_living / spawn_entity packet that followed, read out of the\n");
    out.push_str("# raw body, together with which of the two packets carried it.\n");
    out.push_str("# A `refused` row records vanilla's own reply verbatim instead.\n");
    out.push_str("# <name> <living|object> <type id>   |   <name> refused <reason>\n");
    for (name, id, note) in &observed {
        match id {
            Some((Spawn::Living, id)) => {
                let _ = writeln!(out, "{name} living {id}");
            }
            Some((Spawn::Object, id)) => {
                let _ = writeln!(out, "{name} object {id}");
            }
            None => {
                let _ = writeln!(out, "{name} refused {note}");
            }
        }
    }
    let path = manifest_dir().join("tests/captures/entity_types_1_13_2.txt");
    std::fs::write(&path, out).expect("write oracle");
    eprintln!(
        "wrote {} ({} observed, {} refused)",
        path.display(),
        observed.iter().filter(|(_, id, _)| id.is_some()).count(),
        observed.iter().filter(|(_, id, _)| id.is_none()).count(),
    );
}

/// Which of the two spawn packets carried a type id.
///
/// The distinction is the whole point of the transcript: at 404 the two
/// packets index **different** id spaces, and a recorder that flattened them
/// would produce a table that names a real but wrong entity for every object.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Spawn {
    /// `spawn_entity_living`, indexing 1.13's unified entity registry.
    Living,
    /// `spawn_entity`, still indexing the pre-1.13 *object* id space at 404.
    Object,
}

/// Reads for `window`, noting the type field of every spawn packet seen and
/// otherwise keeping the session alive.
///
/// The keep-alive half is not incidental: a 1.13.2 server disconnects a client
/// that stops answering, and a recorder that merely drained the socket got
/// kicked partway through its first pass and recorded nothing for every name
/// after that -- with no error, because a closed socket just stops producing
/// spawn packets.
#[cfg(test)]
async fn pump(
    conn: &mut lodestone_net::Connection<tokio::net::TcpStream>,
    adapter: &lodestone_v1_13::V404Adapter,
    world: &mut lodestone_world::World,
    window: std::time::Duration,
    spawn_living: i32,
    spawn_object: i32,
    seen: &mut Vec<([u8; 16], Spawn, i32)>,
    known: &mut std::collections::HashSet<[u8; 16]>,
) {
    use lodestone_model::{ClientAction, ClientEvent, ConnectionState, Directive, VersionAdapter};
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let read = tokio::time::timeout(Duration::from_millis(120), conn.read_packet()).await;
        let (id, payload) = match read {
            Ok(Ok(Some(packet))) => packet,
            Ok(Ok(None)) => panic!("the oracle disconnected mid-recording"),
            Ok(Err(err)) => panic!("read error: {err}"),
            Err(_) => continue,
        };
        // A spawn packet is only evidence about *this* summon if its entity
        // is one the session has never seen: the server re-sends a spawn every
        // time an already-summoned entity re-enters tracking range, so within
        // a minute of summoning ninety-five entities into one chunk any
        // fixed-length window contains a dozen of them. The uuid is what makes
        // "new" decidable without sweeping the world between summons -- which
        // is the alternative, and the one that crashed the server.
        if id == spawn_living || id == spawn_object {
            let living = id == spawn_living;
            if let (Some(uuid), Some(ty)) = (
                read_uuid_after_entity_id(&payload),
                read_type_after(&payload, 16, living),
            ) && known.insert(uuid)
            {
                seen.push((uuid, if living { Spawn::Living } else { Spawn::Object }, ty));
            }
        }
        let Ok(directives) = adapter.handle_packet(world, ConnectionState::Play, id, &payload)
        else {
            continue;
        };
        for directive in directives {
            match &directive {
                Directive::Emit(ClientEvent::KeepAlive { id }) => {
                    if let Ok(Some((packet_id, body))) = adapter.encode_action(
                        ConnectionState::Play,
                        &ClientAction::KeepAliveResponse { id: *id },
                    ) {
                        conn.write_packet(packet_id, &body).await.expect("ack");
                    }
                }
                Directive::Send { packet_id, payload } => {
                    conn.write_packet(*packet_id, payload).await.expect("send");
                }
                _ => {}
            }
        }
    }
}

/// Reads the sixteen uuid bytes that follow a spawn packet's leading VarInt
/// entity id. Both spawn packets put the uuid in the same place.
#[cfg(test)]
fn read_uuid_after_entity_id(payload: &[u8]) -> Option<[u8; 16]> {
    let mut cursor = 0usize;
    while cursor < payload.len() {
        let byte = payload[cursor];
        cursor += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    payload.get(cursor..cursor + 16)?.try_into().ok()
}

/// Reads the type field that follows a leading VarInt entity id and
/// `uuid_len` bytes of UUID, as either a VarInt or a signed byte.
#[cfg(test)]
fn read_type_after(payload: &[u8], uuid_len: usize, varint: bool) -> Option<i32> {
    let mut cursor = 0usize;
    // Skip the entity id VarInt.
    while cursor < payload.len() {
        let byte = payload[cursor];
        cursor += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    cursor += uuid_len;
    if cursor >= payload.len() {
        return None;
    }
    if !varint {
        return Some(i32::from(payload[cursor] as i8));
    }
    let mut value = 0i32;
    for index in 0..5 {
        let byte = *payload.get(cursor + index)?;
        value |= i32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            break;
        }
    }
    Some(value)
}

/// Resolves a clientbound play packet name to its 404 id.
#[cfg(test)]
fn clientbound_id(name: &str) -> i32 {
    lodestone_v1_13::packet_ids::play::clientbound::ENTRIES
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol 404 carries no {name}"))
}

