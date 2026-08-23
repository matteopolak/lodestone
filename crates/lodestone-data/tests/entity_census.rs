//! Per-entity-type push census: hermetic checks over the committed table, the
//! reduction from the JVM dump's raw facts to a boolean, and an `#[ignore]`d
//! drift guard that regenerates the table and asserts byte-for-byte equality.
//! Modelled on `hardness.rs` and `entity_dimensions.rs`.
//!
//! # Data provenance
//!
//! `tests/support/entity_census_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server and, for every one of the 158 registered entity
//! types, reporting its implementation class, whether that class is a
//! `LivingEntity`, which class in its hierarchy declares `pushEntities()` and
//! `doPush(Entity)`, and its base `EntityDimensions`
//! (`EntityCensusOracle.java`, walking `BuiltInRegistries.ENTITY_TYPE`). It is
//! committed as the external anchor (§ "an expected value must originate outside
//! the code under test").
//!
//! Note what the dump does and does not contain. Every column is a *mechanical*
//! fact about the jar — a class name, an `isAssignableFrom`, a raw `f32` bit
//! pattern. None of it is a boolean the oracle decided. The reduction to "can
//! this type shove the local player" lives in [`PUSH_MODEL`] below, next to the
//! `26.2` source citations it was read from, and **fails closed**: an override
//! site the model has never seen is a hard error at generate time, not a silent
//! default. That is the property a hand-written name list cannot have.
//!
//! # Two dumps, cross-checked
//!
//! The width/height columns duplicate `tests/support/entity_dimensions_jvm.txt`,
//! which was produced by a different program on a different run.
//! [`census_dimensions_match_the_committed_dimension_table`] asserts the two
//! agree bit-for-bit on all 158 × 2 floats, so each dump anchors the other and a
//! bad classpath or a truncated run cannot pass quietly.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server:
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/EntityCensusOracle.java /work/ && javac -cp "$CP" -d /work /work/EntityCensusOracle.java
//!   java -cp "/work:$CP" EntityCensusOracle'
//! ```
//!
//!    then copy its stdout over `tests/support/entity_census_jvm.txt` (keeping
//!    the `#` header).
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test entity_census \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//!    If the dump introduced an override site [`PUSH_MODEL`] does not know, this
//!    step **panics** naming the class. Read the new `pushEntities`/`doPush` in
//!    the 26.2 tree, add the row with its citation, and re-run. Do not add a
//!    permissive catch-all.

use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::entity_census::{can_be_collided_with, is_mob, pushes_players, TYPE_COUNT};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/entity_census.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/entity_census_jvm.txt");

/// The committed base-dimension dump, for the cross-check.
const DIMENSIONS_DUMP: &str = include_str!("support/entity_dimensions_jvm.txt");

/// One authoritative row of the census dump.
struct Row {
    id: usize,
    name: String,
    /// Implementation class simple name (`Outer.Inner` when nested). Not used by
    /// the reduction; dumped so the boat/minecart passes can be modelled later
    /// without a re-dump, and carried here so a future model can reach it.
    impl_class: String,
    living: bool,
    /// `Mob.class.isAssignableFrom(impl)` — strictly narrower than
    /// [`living`](Self::living), and the class that declares the flags byte
    /// behind `isAggressive()`. Shipped unreduced as `ENTITY_IS_MOB`.
    mob: bool,
    /// Class declaring `pushEntities()`, or `None` when nothing in the hierarchy
    /// does (the expected value for a plain `Entity` subclass).
    push_entities_decl: Option<String>,
    /// Class declaring `doPush(Entity)`, same convention.
    do_push_decl: Option<String>,
    width_bits: u32,
    height_bits: u32,
}

// ---------------------------------------------------------------------------
// The reduction: raw jar facts -> "can this type shove the local player"
// ---------------------------------------------------------------------------

/// What a `pushEntities()` / `doPush(Entity)` override at a given class does to
/// the crowd pass, as read from the 26.2 source.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Effect {
    /// The pass still reaches `player.push(neighbour)`. Either the inherited
    /// `LivingEntity` implementation, or an override that decorates and then
    /// calls `super`.
    Reaches,
    /// The override can never push a `Player`.
    Blocks,
}

/// The **closed** classification of every class that declares `pushEntities()`
/// or `doPush(Entity)` in the 26.2 tree, with the citation it was read from.
///
/// Closed is the point: [`classify`] errors on a declaring class absent from
/// this table, so a version bump that adds an override site cannot silently
/// inherit a permissive default. Adding a row is a deliberate act with a source
/// line attached.
///
/// `ServerPlayer.pushEntities` (a tick-rate-manager
/// gate around `super`) is intentionally absent: `EntityType<Player>` is typed on
/// `Player`, so a client-side census never reaches `ServerPlayer`, and a row here
/// would be dead.
const PUSH_MODEL: &[(&str, Effect, &str)] = &[
    // `pushEntities()` declarers.
    (
        "LivingEntity",
        Effect::Reaches,
        "LivingEntity.java:3222 — the crowd pass itself; \
         LivingEntity.java:3271 doPush -> entity.push(this)",
    ),
    (
        "Bat",
        Effect::Blocks,
        "Bat.java:95 — `protected void pushEntities() {}`, an empty body; \
         Bat.java:91 doPush is likewise empty",
    ),
    (
        "ArmorStand",
        Effect::Blocks,
        "ArmorStand.java:178 — pushEntities() iterates only RIDABLE_MINECARTS, \
         never a player; ArmorStand.java:174 doPush is empty",
    ),
    // `doPush(Entity)` declarers that are not also `pushEntities()` declarers.
    (
        "Parrot",
        Effect::Blocks,
        "Parrot.java:390 — `if (!(entity instanceof Player)) super.doPush(entity);`, \
         so a parrot pushes everything except a player",
    ),
    (
        "IronGolem",
        Effect::Reaches,
        "IronGolem.java:106 — may retarget an Enemy, then calls super.doPush",
    ),
    (
        "SulfurCube",
        Effect::Reaches,
        "SulfurCube.java:731 — calls super.doPush, then applyContactDamage",
    ),
    (
        "Warden",
        Effect::Reaches,
        "Warden.java:529 — records a disturbance, then calls super.doPush",
    ),
];

/// Implementation classes whose `canBeCollidedWith` override can return true.
/// The dump carries the concrete class, so every wood variant follows its
/// shared boat implementation without a hand-maintained entity-name list.
const HARD_COLLISION_CLASSES: &[(&str, &str)] = &[
    ("Boat", "AbstractBoat.canBeCollidedWith — unconditional true"),
    ("ChestBoat", "inherits AbstractBoat.canBeCollidedWith"),
    ("Raft", "inherits AbstractBoat.canBeCollidedWith"),
    ("ChestRaft", "inherits AbstractBoat.canBeCollidedWith"),
    ("Shulker", "Shulker.canBeCollidedWith — isAlive()"),
    (
        "HappyGhast",
        "HappyGhast.canBeCollidedWith — true in eligible runtime states",
    ),
];

fn classify_hard_collision(row: &Row) -> bool {
    HARD_COLLISION_CLASSES
        .iter()
        .any(|(class, _)| *class == row.impl_class)
}

fn effect_of(class: &str) -> Effect {
    PUSH_MODEL
        .iter()
        .find(|(name, _, _)| *name == class)
        .map(|(_, effect, _)| *effect)
        .unwrap_or_else(|| {
            panic!(
                "unknown push override site `{class}`: the JVM dump names a class that declares \
                 pushEntities()/doPush(Entity) and PUSH_MODEL has never seen. Read its body in \
                 .cache/mc/26.2/src and add a row with its citation. Do NOT add a permissive \
                 catch-all — see the module docs."
            )
        })
}

/// Whether an entity of this type can shove the local player through
/// `LivingEntity.pushEntities()`.
///
/// The three vanilla facts, in order:
///
/// 1. Only `LivingEntity` runs the pass at all (`LivingEntity.aiStep` is the
///    sole caller of `pushEntities()` in the tree), so a non-living type is
///    `false` regardless of anything else. This is also the **default-deny**
///    hinge: a future non-living type needs no table entry to be excluded.
/// 2. Its `pushEntities()` must still be able to see a player.
/// 3. Its `doPush(Entity)` must still reach `entity.push(this)` for a player.
fn classify(row: &Row) -> bool {
    if !row.living {
        // Cross-check the dump's own consistency while here: a non-living class
        // cannot declare either method anywhere in its hierarchy, because both
        // are introduced by `LivingEntity`.
        assert!(
            row.push_entities_decl.is_none() && row.do_push_decl.is_none(),
            "{} is not a LivingEntity yet declares a crowd-pass method",
            row.name
        );
        return false;
    }

    let push_entities = row
        .push_entities_decl
        .as_deref()
        .unwrap_or_else(|| panic!("{} is a LivingEntity but declares no pushEntities()", row.name));
    let do_push = row
        .do_push_decl
        .as_deref()
        .unwrap_or_else(|| panic!("{} is a LivingEntity but declares no doPush()", row.name));

    effect_of(push_entities) == Effect::Reaches && effect_of(do_push) == Effect::Reaches
}

// ---------------------------------------------------------------------------
// Dump parsing
// ---------------------------------------------------------------------------

fn parse_dump(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok.next().expect("id column").parse().expect("id is a usize");
        let name = tok.next().expect("name column").to_owned();
        let impl_class = tok.next().expect("impl class column").to_owned();
        let living = match tok.next().expect("living column") {
            "true" => true,
            "false" => false,
            other => panic!("living column is not a boolean: {other:?}"),
        };
        let mob = match tok.next().expect("mob column") {
            "true" => true,
            "false" => false,
            other => panic!("mob column is not a boolean: {other:?}"),
        };
        let decl = |token: &str| match token {
            "-" => None,
            class => Some(class.to_owned()),
        };
        let push_entities_decl = decl(tok.next().expect("pushEntities decl column"));
        let do_push_decl = decl(tok.next().expect("doPush decl column"));
        let width_bits =
            u32::from_str_radix(tok.next().expect("width bits column"), 16).expect("width hex");
        let height_bits =
            u32::from_str_radix(tok.next().expect("height bits column"), 16).expect("height hex");
        assert!(tok.next().is_none(), "unexpected trailing tokens on {line:?}");
        rows.push(Row {
            id,
            name,
            impl_class,
            living,
            mob,
            push_entities_decl,
            do_push_decl,
            width_bits,
            height_bits,
        });
    }
    rows.sort_by_key(|row| row.id);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.id, index, "dump ids are not a dense 0..N (gap at {index})");
    }
    rows
}

/// The dimension dump's `(id, width_bits, height_bits)`, for the cross-check.
fn parse_dimensions_dump(text: &str) -> Vec<(usize, u32, u32)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok.next().expect("id column").parse().expect("id is a usize");
        let _name = tok.next().expect("name column");
        let width =
            u32::from_str_radix(tok.next().expect("width bits column"), 16).expect("width hex");
        let height =
            u32::from_str_radix(tok.next().expect("height bits column"), 16).expect("height hex");
        rows.push((id, width, height));
    }
    rows.sort_by_key(|row| row.0);
    rows
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

fn generate(rows: &[Row]) -> String {
    let count = rows.len();

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test entity_census -- --ignored`\n\
         // from tests/support/entity_census_jvm.txt (a headless 26.2 server dump of each\n\
         // entity type's implementation class, LivingEntity-ness, and pushEntities()/\n\
         // doPush(Entity) declaring classes, protocol 776 / Minecraft 26.2). DO NOT EDIT\n\
         // BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-entity-type push census for protocol 776 (Minecraft 26.2),\n\
         //! indexed by network entity-type registry id. `true` means an entity of that\n\
         //! type can shove the local player through vanilla `LivingEntity.pushEntities()`.\n\
         //! Default-deny: see [`crate::entity_census`] for the reduction and its citations.\n\n",
    );

    let _ = writeln!(
        out,
        "/// Number of entity types (network ids are `0..TYPE_COUNT`)."
    );
    let _ = writeln!(out, "pub const TYPE_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// Whether this type can push the local player, by network registry id."
    );
    let _ = writeln!(out, "pub static ENTITY_PUSHES_PLAYERS: [bool; {count}] = [");
    for row in rows {
        // The trailing comment records the *inputs* the boolean came from, so a
        // reviewer can check the reduction from the generated file alone.
        let pushes = classify(row);
        let via = if row.living {
            format!(
                "{} pushEntities={} doPush={}",
                row.impl_class,
                row.push_entities_decl.as_deref().unwrap_or("-"),
                row.do_push_decl.as_deref().unwrap_or("-"),
            )
        } else {
            format!("{} (not a LivingEntity)", row.impl_class)
        };
        let literal = format!("{pushes},");
        let _ = writeln!(out, "    {literal:<7}// {} {} — {via}", row.id, row.name);
    }
    out.push_str("];\n");

    let _ = writeln!(
        out,
        "\n/// Whether this type can participate in another entity's hard movement collision."
    );
    let _ = writeln!(
        out,
        "pub static ENTITY_CAN_BE_COLLIDED_WITH: [bool; {count}] = ["
    );
    for row in rows {
        let collidable = classify_hard_collision(row);
        let literal = format!("{collidable},");
        let _ = writeln!(
            out,
            "    {literal:<7}// {} {} — {}",
            row.id, row.name, row.impl_class
        );
    }
    out.push_str("];\n");

    // The raw `living` column, shipped unreduced. It is a *different* fact from
    // `ENTITY_PUSHES_PLAYERS` above and cannot be recovered from it: `bat`,
    // `armor_stand` and `parrot` are all `LivingEntity` subclasses that do not
    // push (`tests` `the_three_living_types_that_cannot_reach_a_player_do_not_push`),
    // so reading the push table as an is-living test gets those three wrong.
    //
    // Its consumer is metadata decode, not physics: `DATA_LIVING_ENTITY_FLAGS`
    // sits at metadata index 8, and so does `AbstractArrow.ID_FLAGS` — both
    // plain bytes, indistinguishable by serializer. A version adapter needs this
    // to know whether an index-8 byte is a using-item bitfield or an arrow's
    // crit flag.
    let _ = writeln!(
        out,
        "\n/// Whether this type's implementation class is a `LivingEntity`, by network\n\
         /// registry id."
    );
    let _ = writeln!(out, "pub static ENTITY_IS_LIVING: [bool; {count}] = [");
    for row in rows {
        let literal = format!("{},", row.living);
        let _ = writeln!(
            out,
            "    {literal:<7}// {} {} — {}",
            row.id, row.name, row.impl_class
        );
    }
    out.push_str("];\n");

    // The raw `mob` column, likewise unreduced — and a *third* distinct fact.
    // `Mob` is where `DATA_MOB_FLAGS_ID` (metadata index 15, the aggressive bit)
    // is declared, and index 15 is *also* `ArmorStand.DATA_CLIENT_FLAGS` and
    // `Display.DATA_BILLBOARD_RENDER_CONSTRAINTS_ID`, all three `BYTE`. An armour
    // stand's `0x04` is "show arms" where a mob's is "aggressive", so an
    // is-*living* guard is not enough for index 15 the way it is for index 8:
    // `ArmorStand` is living. See `tests/support/entity_data_index_jvm.txt` in
    // the `v770` crate for the collision, read off the jar.
    let _ = writeln!(
        out,
        "\n/// Whether this type's implementation class is a `Mob`, by network registry id."
    );
    let _ = writeln!(out, "pub static ENTITY_IS_MOB: [bool; {count}] = [");
    for row in rows {
        let literal = format!("{},", row.mob);
        let _ = writeln!(
            out,
            "    {literal:<7}// {} {} — {}",
            row.id, row.name, row.impl_class
        );
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (anchored to the committed dump)
// ---------------------------------------------------------------------------

#[test]
fn committed_table_matches_the_committed_dump_row_for_row() {
    // The strongest check: the shipped accessor equals the reduction of the raw
    // server facts for all 158 types. Non-vacuous by construction — it iterates
    // every type and every row's inputs came from the jar.
    let rows = parse_dump(DUMP);
    assert_eq!(rows.len(), TYPE_COUNT as usize, "dump/table type count mismatch");
    let mut checked = 0usize;
    for row in &rows {
        let expected = classify(row);
        let actual = pushes_players(row.id as i32)
            .unwrap_or_else(|| panic!("id {} ({}) missing from table", row.id, row.name));
        assert_eq!(
            actual, expected,
            "push mismatch for {} (id {}): table {actual}, dump says {expected} \
             (living={}, pushEntities={:?}, doPush={:?})",
            row.name, row.id, row.living, row.push_entities_decl, row.do_push_decl
        );
        assert_eq!(
            can_be_collided_with(row.id as i32),
            Some(classify_hard_collision(row)),
            "hard-collision mismatch for {} (id {}, class {})",
            row.name,
            row.id,
            row.impl_class
        );
        checked += 1;
    }
    assert_eq!(checked, 158, "expected 158 entity types checked, got {checked}");
}

#[test]
fn hard_collision_is_a_separate_default_deny_capability() {
    let rows = parse_dump(DUMP);
    for name in [
        "minecraft:oak_boat",
        "minecraft:oak_chest_boat",
        "minecraft:bamboo_raft",
        "minecraft:bamboo_chest_raft",
        "minecraft:shulker",
        "minecraft:happy_ghast",
    ] {
        let row = rows
            .iter()
            .find(|row| row.name == name)
            .expect("known collider");
        assert_eq!(can_be_collided_with(row.id as i32), Some(true), "{name}");
    }
    for name in ["minecraft:player", "minecraft:zombie", "minecraft:item"] {
        let row = rows
            .iter()
            .find(|row| row.name == name)
            .expect("known control");
        assert_eq!(can_be_collided_with(row.id as i32), Some(false), "{name}");
    }
    assert_eq!(can_be_collided_with(TYPE_COUNT as i32), None);
}

#[test]
fn the_committed_is_mob_column_matches_the_dump_row_for_row() {
    // `is_mob` ships the dump's `mob` column with no reduction at all, so the
    // check is equality over all 158 rows.
    let rows = parse_dump(DUMP);
    let mut mobs = 0usize;
    for row in &rows {
        let actual = is_mob(row.id as i32)
            .unwrap_or_else(|| panic!("id {} ({}) missing from table", row.id, row.name));
        assert_eq!(
            actual, row.mob,
            "mob mismatch for {} (id {}): table {actual}, dump {} ({})",
            row.name, row.id, row.mob, row.impl_class
        );
        mobs += usize::from(row.mob);
    }
    // Non-vacuity: the column must actually separate the population, not be all
    // one value (which would satisfy the equality above just as well).
    assert!(
        (40..rows.len()).contains(&mobs),
        "{mobs} of {} types are mobs — implausible, the column is probably degenerate",
        rows.len()
    );
    assert_eq!(is_mob(TYPE_COUNT as i32), None);
    assert_eq!(is_mob(i32::MAX), None);
}

#[test]
fn is_mob_is_strictly_narrower_than_is_living_and_the_gap_is_named() {
    // The whole reason a third column exists. Metadata index 15 is
    // `Mob.DATA_MOB_FLAGS_ID` on a mob and `ArmorStand.DATA_CLIENT_FLAGS` on an
    // armour stand, both `BYTE` — and bit `0x04` is "aggressive" on one and "show
    // arms" on the other. `ArmorStand` is a `LivingEntity`, so unlike index 8 the
    // is-living guard does **not** resolve this one, and reading `is_living` as an
    // is-mob test would make every armour stand with arms report itself as an
    // aggressive mob.
    let rows = parse_dump(DUMP);
    for row in &rows {
        assert!(
            !row.mob || row.living,
            "{} is a mob but not living, which is impossible (Mob extends LivingEntity)",
            row.name
        );
    }
    let living_non_mobs: Vec<&str> = rows
        .iter()
        .filter(|row| row.living && !row.mob)
        .map(|row| row.name.as_str())
        .collect();
    // Read off the 26.2 tree: `LivingEntity`'s direct non-`Mob` descendants.
    // Asserted by name because the *identity* of the gap is the finding, not its
    // size — a later version adding one must be looked at, not absorbed.
    assert_eq!(
        living_non_mobs,
        vec![
            "minecraft:armor_stand",
            "minecraft:mannequin",
            "minecraft:player",
        ],
        "the living-but-not-Mob set changed; each of these has its own claimant on \
         metadata index 15 and needs reading before the guard is widened"
    );
    // And the positive side: the mobs whose arm pose the aggressive bit drives.
    for name in [
        "minecraft:skeleton",
        "minecraft:stray",
        "minecraft:bogged",
        "minecraft:wither_skeleton",
        "minecraft:zombie",
        "minecraft:drowned",
        "minecraft:husk",
        "minecraft:pillager",
    ] {
        let row = rows
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the dump"));
        assert!(row.mob, "{name} must be a Mob");
    }
}

#[test]
fn census_dimensions_match_the_committed_dimension_table() {
    // Two independently-run dumps, cross-checked: the census carries width/height
    // so a bad classpath, a truncated run or a mis-sorted file in *either* dump
    // shows up as a disagreement rather than passing quietly. Also asserts the
    // shipped `entity_dimensions` accessor agrees with both.
    let census = parse_dump(DUMP);
    let dimensions = parse_dimensions_dump(DIMENSIONS_DUMP);
    assert_eq!(
        census.len(),
        dimensions.len(),
        "the two dumps cover different type counts"
    );
    let mut checked = 0usize;
    for (row, &(id, width_bits, height_bits)) in census.iter().zip(&dimensions) {
        assert_eq!(row.id, id, "dump ids diverge at {id}");
        assert_eq!(
            (row.width_bits, row.height_bits),
            (width_bits, height_bits),
            "the two dumps disagree on {}'s hitbox bits",
            row.name
        );
        let shipped = lodestone_data::entity_dimensions::base_dimensions(row.id as i32)
            .unwrap_or_else(|| panic!("id {} missing from the dimension table", row.id));
        assert_eq!(shipped.width.to_bits(), width_bits, "{} width", row.name);
        assert_eq!(shipped.height.to_bits(), height_bits, "{} height", row.name);
        checked += 1;
    }
    assert_eq!(checked, 158, "expected 158 cross-checked types, got {checked}");
}

#[test]
fn every_push_model_row_is_reachable_from_the_dump() {
    // The mirror of `effect_of`'s panic. That one catches an override site the
    // model lacks; this one catches a model row nothing in the jar declares —
    // i.e. a stale entry left behind by a refactor, which would otherwise sit
    // there indefinitely looking authoritative.
    let rows = parse_dump(DUMP);
    for (class, _, citation) in PUSH_MODEL {
        let seen = rows.iter().any(|row| {
            row.push_entities_decl.as_deref() == Some(class)
                || row.do_push_decl.as_deref() == Some(class)
        });
        assert!(
            seen,
            "PUSH_MODEL row `{class}` ({citation}) is not a declaring class anywhere in the \
             dump — the override it cites is gone; drop the row"
        );
    }
}

#[test]
fn the_reduction_is_default_deny() {
    // The polarity, stated as a property rather than a comment: a type this model
    // knows nothing about must come back `false`. Synthesised rows, since the jar
    // has no unknown types by definition.
    let unknown_non_living = Row {
        id: 0,
        name: "test:unknown_gadget".to_owned(),
        impl_class: "SomeFutureGadget".to_owned(),
        living: false,
        mob: false,
        push_entities_decl: None,
        do_push_decl: None,
        width_bits: 0,
        height_bits: 0,
    };
    assert!(
        !classify(&unknown_non_living),
        "an unrecognised non-living type must default to not-a-pusher"
    );

    // And the id space itself is closed: an id past the census is a miss, not a
    // permissive `true`.
    assert_eq!(pushes_players(TYPE_COUNT as i32), None);
    assert_eq!(pushes_players(i32::MAX), None);
}

// ---------------------------------------------------------------------------
// Negative control: the detector would fail if the reduction were inverted
// ---------------------------------------------------------------------------

#[test]
fn negative_control_an_inverted_reduction_is_caught() {
    // "A dropped item is not pushable" is an assertion of an *absence*, and per
    // the repo's evidence standard needs a control proving the detector works.
    // `classify_inverted` is `classify` with the living check flipped — the exact
    // mistake a denylist makes, treating an unrecognised type as a pusher. The
    // real gates must reject it, at the item/arrow/boat rows specifically.
    fn classify_inverted(row: &Row) -> bool {
        // The denylist's polarity: default `true`, subtract known-inert names.
        !matches!(row.name.as_str(), "minecraft:experience_orb")
    }

    let rows = parse_dump(DUMP);
    let mut disagreements = Vec::new();
    for row in &rows {
        if classify(row) != classify_inverted(row) {
            disagreements.push(row.name.as_str());
        }
    }
    for expected in [
        "minecraft:item",
        "minecraft:arrow",
        "minecraft:oak_boat",
        "minecraft:bat",
        "minecraft:parrot",
        "minecraft:armor_stand",
    ] {
        assert!(
            disagreements.contains(&expected),
            "the control did not separate {expected}: the real reduction and an inverted one \
             agree on it, so this test could not have detected the inversion"
        );
    }
    // And the control must *not* fire on a genuine pusher, or it would be
    // separating everything and proving nothing.
    assert!(
        !disagreements.contains(&"minecraft:zombie"),
        "the control fires on zombie, so it is not isolating the inversion"
    );
    assert!(
        disagreements.len() >= 60,
        "expected the inversion to move most of the 68 non-pushers, moved {}",
        disagreements.len()
    );
}

// ---------------------------------------------------------------------------
// Drift guard
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
        "src/generated/entity_census.rs is stale vs the JVM dump; regenerate with LODESTONE_REGEN=1"
    );
}
