//! Item-prototype component census: hermetic checks over the committed table,
//! plus an `#[ignore]`d drift guard that regenerates it from the committed JVM
//! dump and asserts byte-for-byte equality — modelled on `hardness.rs` and
//! `tools.rs`. The generator lives here so the checked-in table can never
//! silently drift from the game data.
//!
//! # Data provenance
//!
//! `tests/support/item_prototype_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server and reading `Item.components()` for every one of
//! the 1,537 registered items (`ItemPrototypeOracle.java`, walking
//! `BuiltInRegistries.ITEM`).
//!
//! These three components are **prototype** components: a clientbound
//! `ItemStack` carries only the `DataComponentPatch` — the delta from the item's
//! built-in component map — and vanilla keeps `minecraft:max_stack_size`,
//! `minecraft:max_damage` and `minecraft:equippable` in that map. So they are
//! never on the wire, and no packet capture at any level of effort can supply
//! them. `registries.json` carries item *names and ids* but no components, and
//! `blocks.json` is block properties only. "Boot the jar and ask it" is the only
//! authoritative source, exactly as for hardness, collision shapes and the tool
//! census.
//!
//! The dump is committed as the external anchor (§ "an expected value must
//! originate outside the code under test"): the table is derived from it, so a
//! transposed column or a misread integer fails
//! [`committed_table_matches_the_committed_dump`] rather than silently shipping.
//!
//! # Cross-check against a second artifact
//!
//! [`dump_ids_and_names_match_the_registries_json_table`] reconciles the dump's
//! registry ids and names against `crate::items::item_name`, which is generated
//! by `cargo xtask gen-registries` from Mojang's own
//! `generated/reports/registries.json`. Two independently produced artifacts have
//! to agree on all 1,537 (id, name) pairs; neither restates the other. That is
//! what makes indexing the table by registry id safe.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (keep the `#` header when copying over the
//!    committed dump):
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/ItemPrototypeOracle.java /work/ && javac -cp "$CP" -d /work /work/ItemPrototypeOracle.java
//!   java -cp "/work:$CP" ItemPrototypeOracle'
//! ```
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v770 --test item_prototypes \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_model::EquipmentSlot;
use lodestone_v770::{item_prototypes, items};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/item_prototypes.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/item_prototype_jvm.txt");

/// One authoritative row of the dump.
struct Row {
    id: usize,
    name: String,
    max_stack_size: u32,
    max_damage: Option<u32>,
    has_damage: bool,
    /// `Equippable.slot().getSerializedName()`, or `None`.
    equip_slot: Option<String>,
    /// The raw `allowedEntities` column: `-` (any), `#tag` or `=a,b`.
    allowed_entities: String,
}

/// Maps vanilla's `EquipmentSlot.getSerializedName()` to the model enum.
///
/// The names come from `EquipmentSlot`'s own constructor arguments
/// (`EquipmentSlot.java:13-20`); an unknown one is a hard failure rather than a
/// silent `None`, because "this item is not equippable" and "this build does not
/// know that slot" must not look the same.
fn slot_from_name(name: &str) -> EquipmentSlot {
    match name {
        "mainhand" => EquipmentSlot::MainHand,
        "offhand" => EquipmentSlot::OffHand,
        "feet" => EquipmentSlot::Feet,
        "legs" => EquipmentSlot::Legs,
        "chest" => EquipmentSlot::Chest,
        "head" => EquipmentSlot::Head,
        "body" => EquipmentSlot::Body,
        "saddle" => EquipmentSlot::Saddle,
        other => panic!("unknown vanilla EquipmentSlot serialized name {other:?}"),
    }
}

/// Renders the model-enum path a generated literal uses.
fn slot_path(slot: EquipmentSlot) -> &'static str {
    match slot {
        EquipmentSlot::MainHand => "EquipmentSlot::MainHand",
        EquipmentSlot::OffHand => "EquipmentSlot::OffHand",
        EquipmentSlot::Feet => "EquipmentSlot::Feet",
        EquipmentSlot::Legs => "EquipmentSlot::Legs",
        EquipmentSlot::Chest => "EquipmentSlot::Chest",
        EquipmentSlot::Head => "EquipmentSlot::Head",
        EquipmentSlot::Body => "EquipmentSlot::Body",
        EquipmentSlot::Saddle => "EquipmentSlot::Saddle",
    }
}

fn parse_dump(text: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        assert_eq!(tok.next(), Some("P"), "unexpected record kind on {line:?}");
        let id: usize = tok
            .next()
            .expect("id column")
            .parse()
            .expect("id is a usize");
        let name = tok.next().expect("name column").to_owned();
        let max_stack_size: u32 = tok
            .next()
            .expect("max-stack-size column")
            .parse()
            .expect("max stack size is a u32");
        assert!(
            (1..=99).contains(&max_stack_size),
            "max stack size {max_stack_size} out of vanilla's 1..=99 on {line:?}"
        );
        let max_damage = match tok.next().expect("max-damage column") {
            "-" => None,
            value => Some(value.parse::<u32>().expect("max damage is a u32")),
        };
        let has_damage = match tok.next().expect("has-damage column") {
            "0" => false,
            "1" => true,
            other => panic!("has-damage must be 0 or 1, got {other:?} on {line:?}"),
        };
        let equip_slot = match tok.next().expect("equip-slot column") {
            "-" => None,
            value => Some(value.to_owned()),
        };
        let allowed_entities = tok
            .next()
            .expect("allowed-entities column")
            .to_owned();
        assert!(tok.next().is_none(), "unexpected trailing tokens on {line:?}");
        rows.push(Row {
            id,
            name,
            max_stack_size,
            max_damage,
            has_damage,
            equip_slot,
            allowed_entities,
        });
    }
    rows.sort_by_key(|row| row.id);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.id, index, "dump ids are not a dense 0..N (gap at {index})");
    }
    rows
}

/// Renders the committed `item_prototypes.rs` source from the parsed dump.
///
/// Unlike the hardness and collision tables this does **not** de-duplicate:
/// 1,537 entries of five small fields is ~12 KiB of rodata, and a direct
/// `[ItemPrototypeDef; ITEM_COUNT]` indexed by registry id keeps the hot lookup a
/// single bounds-checked index with no indirection.
fn generate(rows: &[Row]) -> String {
    let count = rows.len();
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v770 --test item_prototypes -- --ignored`\n\
         // from tests/support/item_prototype_jvm.txt (a headless 26.2 server dump of every\n\
         // item's built-in prototype minecraft:max_stack_size / minecraft:max_damage /\n\
         // minecraft:equippable, protocol 776 / Minecraft 26.2). DO NOT EDIT BY HAND.\n\
         // Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated item-prototype component table for protocol 776 (Minecraft 26.2),\n\
         //! indexed by network `minecraft:item` registry id. Consumed by\n\
         //! [`crate::item_prototypes`].\n\n",
    );
    out.push_str("use lodestone_model::EquipmentSlot;\n\n");
    out.push_str("use crate::item_prototypes::ItemPrototypeDef;\n\n");

    let _ = writeln!(out, "/// Number of items (registry ids are `0..ITEM_COUNT`).");
    let _ = writeln!(out, "pub const ITEM_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// Every item's built-in prototype components, indexed by registry id."
    );
    let _ = writeln!(
        out,
        "pub static ITEM_PROTOTYPES: [ItemPrototypeDef; {count}] = ["
    );
    for row in rows {
        let max_stack_size = u8::try_from(row.max_stack_size).expect("max stack size fits u8");
        let max_damage = match row.max_damage {
            None => "None".to_owned(),
            Some(value) => format!(
                "Some({})",
                u16::try_from(value).expect("max damage fits u16")
            ),
        };
        let equip_slot = match &row.equip_slot {
            None => "None".to_owned(),
            Some(name) => format!("Some({})", slot_path(slot_from_name(name))),
        };
        let any_entity = row.allowed_entities == "-";
        let _ = writeln!(
            out,
            "    // {}\n    \
             ItemPrototypeDef {{ max_stack_size: {max_stack_size}, max_damage: {max_damage}, \
             equip_slot: {equip_slot}, has_damage: {}, equippable_by_any_entity: {any_entity} }},",
            row.name, row.has_damage
        );
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (anchored to the committed dump)
// ---------------------------------------------------------------------------

#[test]
fn committed_table_matches_the_committed_dump() {
    // The strongest check: every value the shipped accessor returns equals what
    // the real server produced. Non-vacuous by construction — it iterates all
    // 1,537 items and compares every field, so a single transposed column or
    // misread integer fails.
    let rows = parse_dump(DUMP);
    assert_eq!(
        rows.len(),
        item_prototypes::ITEM_COUNT as usize,
        "dump/table item count mismatch"
    );
    let mut checked = 0usize;
    for row in &rows {
        let def = item_prototypes::prototype_by_id(row.id as i32)
            .unwrap_or_else(|| panic!("id {} ({}) missing from table", row.id, row.name));
        assert_eq!(
            u32::from(def.max_stack_size),
            row.max_stack_size,
            "max_stack_size mismatch for {} (id {})",
            row.name,
            row.id
        );
        assert_eq!(
            def.max_damage.map(u32::from),
            row.max_damage,
            "max_damage mismatch for {} (id {})",
            row.name,
            row.id
        );
        assert_eq!(
            def.has_damage, row.has_damage,
            "has_damage mismatch for {} (id {})",
            row.name, row.id
        );
        assert_eq!(
            def.equip_slot,
            row.equip_slot.as_deref().map(slot_from_name),
            "equip_slot mismatch for {} (id {})",
            row.name,
            row.id
        );
        assert_eq!(
            def.equippable_by_any_entity,
            row.allowed_entities == "-",
            "equippable_by_any_entity mismatch for {} (id {})",
            row.name,
            row.id
        );
        // And the same values are reachable by name, which is what the version
        // seam resolves through.
        let by_name = item_prototypes::prototype(&row.name)
            .unwrap_or_else(|| panic!("{} not resolvable by name", row.name));
        assert_eq!(
            by_name.max_stack_size, def.max_stack_size,
            "by-name lookup disagrees with by-id for {}",
            row.name
        );
        checked += 1;
    }
    assert_eq!(checked, 1_537, "expected 1,537 items checked, got {checked}");
}

/// Reconciles the JVM dump against `registries.json` — a *second*, independently
/// produced Mojang artifact — on every (id, name) pair. Neither restates the
/// other, so agreement is real evidence that indexing this table by registry id
/// is safe. Mismatched registry orders are a live hazard in this crate: the
/// block registry's alphabetical-vs-registration confusion silently mis-named
/// every `block_event` until `875f452`.
#[test]
fn dump_ids_and_names_match_the_registries_json_table() {
    let rows = parse_dump(DUMP);
    assert_eq!(rows.len(), items::ITEM_COUNT as usize, "item count mismatch");
    for row in &rows {
        assert_eq!(
            items::item_name(row.id as i32),
            Some(row.name.as_str()),
            "registries.json and the JVM dump disagree at item id {}",
            row.id
        );
    }
}

#[test]
fn out_of_range_ids_are_none() {
    assert!(
        item_prototypes::prototype_by_id(item_prototypes::ITEM_COUNT as i32).is_none(),
        "one past the end must miss"
    );
    assert!(item_prototypes::prototype_by_id(-1).is_none(), "negative must miss");
    assert!(item_prototypes::prototype_by_id(i32::MAX).is_none());
    assert!(item_prototypes::prototype("minecraft:not_an_item").is_none());
}

/// `ItemStack.isDamageableItem` is `has(MAX_DAMAGE) && !has(UNBREAKABLE) &&
/// has(DAMAGE)` (`ItemStack.java:416-418`) — three components, not one. This
/// pins that the first and third always agree in 26.2's prototypes, which is why
/// [`lodestone_model::ItemPrototype`] carries only `max_damage`. If a future
/// version separates them, this fails instead of the model quietly answering
/// `isDamageableItem` wrong.
#[test]
fn max_damage_and_damage_components_always_agree() {
    let rows = parse_dump(DUMP);
    for row in &rows {
        assert_eq!(
            row.max_damage.is_some(),
            row.has_damage,
            "{} has max_damage={:?} but has_damage={}",
            row.name,
            row.max_damage,
            row.has_damage
        );
    }
}

// ---------------------------------------------------------------------------
// The values that actually broke gameplay, pinned individually
// ---------------------------------------------------------------------------

/// Armour is equippable at all — the whole point of the census. `may_place` on an
/// armour slot is `equippable_slot(stack) == Some(target)`, so before this table
/// existed it was `None == Some(_)` for every stack off the wire.
#[test]
fn armour_declares_its_slot() {
    for (item, slot) in [
        ("minecraft:diamond_helmet", EquipmentSlot::Head),
        ("minecraft:diamond_chestplate", EquipmentSlot::Chest),
        ("minecraft:diamond_leggings", EquipmentSlot::Legs),
        ("minecraft:diamond_boots", EquipmentSlot::Feet),
        ("minecraft:turtle_helmet", EquipmentSlot::Head),
        // Not armour, but wearable on the head — and it stacks to 64, so a
        // slot's effective cap must be min(slot cap, item cap), not the item's.
        ("minecraft:carved_pumpkin", EquipmentSlot::Head),
        // Chest slot, and the reason `Equippable` is not "armour": elytra are
        // HUMANOID_ARMOR-typed and share the chestplate slot.
        ("minecraft:elytra", EquipmentSlot::Chest),
    ] {
        let def = item_prototypes::prototype(item).unwrap_or_else(|| panic!("{item} present"));
        assert_eq!(def.equip_slot, Some(slot), "{item} equip slot");
    }
}

/// `EquipmentSlot::Body` is animal armour and is **not** the chest slot. Vanilla
/// gates humanoid armour on `EquipmentSlot.Type.HUMANOID_ARMOR`
/// (`InventoryMenu.java:122`), which covers FEET/LEGS/CHEST/HEAD and excludes
/// BODY (`EquipmentSlot.java:15-19`). A consumer that folds `"body"` into `Chest`
/// makes wolf armour and horse armour placeable in a player's chestplate slot —
/// this test is the record of which items would do it.
#[test]
fn animal_armour_is_body_not_chest() {
    for item in [
        "minecraft:wolf_armor",
        "minecraft:leather_horse_armor",
        "minecraft:iron_horse_armor",
        "minecraft:golden_horse_armor",
        "minecraft:diamond_horse_armor",
    ] {
        let def = item_prototypes::prototype(item).unwrap_or_else(|| panic!("{item} present"));
        assert_eq!(
            def.equip_slot,
            Some(EquipmentSlot::Body),
            "{item} must be Body, never Chest"
        );
        assert!(
            !def.equippable_by_any_entity,
            "{item} restricts allowedEntities, so even in Body it is not universally wearable"
        );
    }
    let saddle = item_prototypes::prototype("minecraft:saddle").expect("saddle present");
    assert_eq!(saddle.equip_slot, Some(EquipmentSlot::Saddle));
    assert!(!saddle.equippable_by_any_entity);

    // Every entity-restricted item in 26.2 is in a non-humanoid slot, which is
    // what lets `ItemPrototype` omit the `allowedEntities` set: the slot check
    // already excludes it from any player armour slot. If a *humanoid*-slot item
    // ever gains a restriction, this fails and the set has to be carried.
    let rows = parse_dump(DUMP);
    for row in &rows {
        if row.allowed_entities == "-" {
            continue;
        }
        let slot = row
            .equip_slot
            .as_deref()
            .map(slot_from_name)
            .expect("allowedEntities implies an equippable slot");
        assert!(
            matches!(slot, EquipmentSlot::Body | EquipmentSlot::Saddle),
            "{} restricts allowedEntities in humanoid slot {slot:?}; ItemPrototype must now \
             carry the allowed-entity set",
            row.name
        );
    }
}

/// 64 is not a safe default for `max_stack_size`. These are the caps a
/// drag/quick-move prediction gets wrong while the census is missing.
#[test]
fn per_item_stack_caps_are_not_all_64() {
    for (item, cap) in [
        ("minecraft:stone", 64),
        ("minecraft:water_bucket", 1),
        ("minecraft:lava_bucket", 1),
        ("minecraft:shulker_box", 1),
        ("minecraft:white_shulker_box", 1),
        ("minecraft:egg", 16),
        ("minecraft:snowball", 16),
        ("minecraft:ender_pearl", 16),
        ("minecraft:diamond_pickaxe", 1),
        ("minecraft:diamond_helmet", 1),
        ("minecraft:oak_sign", 16),
        ("minecraft:bucket", 16),
    ] {
        let def = item_prototypes::prototype(item).unwrap_or_else(|| panic!("{item} present"));
        assert_eq!(
            u32::from(def.max_stack_size),
            cap,
            "{item} max stack size"
        );
    }

    // A negative control for the "everything is 64" bug: 295 of the 1,537 items
    // (19%) really are not 64, so a table that lost the column — or a consumer
    // that keeps defaulting to 64 — is wrong about one stack in five, not about
    // a handful of exotica.
    let non_64 = (0..item_prototypes::ITEM_COUNT as i32)
        .filter_map(item_prototypes::prototype_by_id)
        .filter(|def| def.max_stack_size != 64)
        .count();
    assert_eq!(
        non_64, 295,
        "the population of items with a non-64 stack cap changed"
    );
}

/// `max_damage` gates `isDamageableItem` and therefore `isStackable`: without it
/// two swords merge. These are the durabilities that stop that.
#[test]
fn damageable_items_carry_their_durability() {
    for (item, durability) in [
        ("minecraft:diamond_pickaxe", Some(1561)),
        ("minecraft:netherite_pickaxe", Some(2031)),
        ("minecraft:wooden_sword", Some(59)),
        ("minecraft:diamond_helmet", Some(363)),
        ("minecraft:elytra", Some(432)),
        ("minecraft:wolf_armor", Some(64)),
        ("minecraft:turtle_helmet", Some(275)),
        // Not damageable at all.
        ("minecraft:stone", None),
        ("minecraft:water_bucket", None),
        ("minecraft:carved_pumpkin", None),
    ] {
        let def = item_prototypes::prototype(item).unwrap_or_else(|| panic!("{item} present"));
        assert_eq!(
            def.max_damage.map(u32::from),
            durability,
            "{item} max damage"
        );
    }
}

/// `minecraft:air` is a real registry entry with a prototype; it must resolve
/// rather than miss, so a decoder that sees id 0 does not treat it as unknown.
#[test]
fn air_resolves_and_is_not_equippable() {
    let air = item_prototypes::prototype("minecraft:air").expect("air present");
    assert_eq!(u32::from(air.max_stack_size), 64);
    assert_eq!(air.equip_slot, None);
    assert_eq!(air.max_damage, None);
}

// ---------------------------------------------------------------------------
// Drift guard (regenerates from the committed dump; `#[ignore]`d for parity
// with the other generated tables, though it needs no external artifact)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let rows = parse_dump(DUMP);
    let generated = generate(&rows);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/item_prototypes.rs is stale vs the JVM dump; regenerate with \
         LODESTONE_REGEN=1"
    );
}
