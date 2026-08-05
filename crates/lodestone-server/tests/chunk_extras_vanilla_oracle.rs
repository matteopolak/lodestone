//! `block_entities` / `block_ticks` / `fluid_ticks` read from bytes **Mojang's
//! own server wrote** (issue #468).
//!
//! # Why the fixtures are real region bytes and not our own output
//!
//! `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings,
//! and this schema has two traps that are *exactly* that shape:
//!
//! * `p` carries vanilla's `-3..3` priority **value**, but our `TickPriority`
//!   is declaration-ordered so `Normal`'s ordinal is `3` and its value is `0`.
//!   A writer emitting the ordinal and a reader parsing it back as an ordinal
//!   agree perfectly, and every normal tick in the world silently becomes
//!   `EXTREMELY_LOW` the moment a real server reads it.
//! * `t` is a **signed** delay relative to game time at save. A writer and
//!   reader that both assumed unsigned agree perfectly too, right up to the
//!   first overdue tick.
//!
//! Neither is visible to a round trip through our own code, so both are gated
//! here against files a real 26.2 server produced.
//!
//! # The fixtures
//!
//! Both are the **decompressed, byte-for-byte unmodified** chunk NBT lifted
//! out of `.cache/mc/survival/world`'s region files by an independent stdlib
//! `struct`+`zlib` parser — deliberately not by `lodestone-anvil`, so nothing
//! about the extraction shares an assumption with the code under test.
//!
//! | fixture | provenance | what it exercises |
//! |---|---|---|
//! | `vanilla_26_2_block_entity_chunk.nbt` | `r.0.0.mca` local `(6,12)`, chunk `(6,12)` | a real `minecraft:blast_furnace` |
//! | `vanilla_26_2_ticks_chunk.nbt` | `r.1.-1.mca` local `(24,3)`, chunk `(56,-29)` | 1 block tick + 16 fluid ticks, **all with negative `t`** |
//!
//! They were selected as the *smallest* real chunks meeting each requirement,
//! out of 22,488 scanned.

use lodestone_core::{Reader, read_named_nbt};
use lodestone_server::chunk_nbt::{
    self, ChunkExtras, SavedTick, tick_priority_from_value, tick_priority_value,
};
use lodestone_server::{BlockEntity, FurnaceKind, TickPriority};

const BLOCK_ENTITY_CHUNK: &[u8] = include_bytes!("support/vanilla_26_2_block_entity_chunk.nbt");
const TICKS_CHUNK: &[u8] = include_bytes!("support/vanilla_26_2_ticks_chunk.nbt");

fn extras(bytes: &[u8]) -> ChunkExtras {
    let mut reader = Reader::new(bytes);
    let (_, nbt) = read_named_nbt(&mut reader).expect("fixture is a valid named NBT tree");
    chunk_nbt::extras_from_nbt(&nbt)
}

/// A real vanilla `minecraft:blast_furnace` decodes into this crate's own
/// [`BlockEntity::Furnace`], at its absolute position.
///
/// Every expected value below was read out of the file by the independent
/// Python parser before any Rust was written — `x=97, y=-59, z=199`, all four
/// timers `0`, `Items` empty — so the assertion originates outside the code
/// under test.
#[test]
fn a_real_vanilla_blast_furnace_decodes_at_its_absolute_position() {
    let extras = extras(BLOCK_ENTITY_CHUNK);
    assert_eq!(
        extras.block_entities.len(),
        1,
        "the fixture chunk holds exactly one block entity"
    );
    let (pos, entity) = &extras.block_entities[0];
    assert_eq!((pos.x, pos.y, pos.z), (97, -59, 199));

    let BlockEntity::Furnace(furnace) = entity else {
        panic!("a minecraft:blast_furnace must decode as a furnace, got {entity:?}");
    };
    assert_eq!(
        furnace.kind(),
        FurnaceKind::BlastFurnace,
        "the id decides the kind; decoding every furnace-family id as a plain \
         furnace would pass a weaker assertion"
    );
    assert_eq!(furnace.burn_state(), (0, 0, 0, 0));
    assert_eq!(
        (furnace.input(), furnace.fuel(), furnace.output()),
        (None, None, None)
    );

    // The other half of the same file: this chunk has no ticks at all, which
    // is what makes the tick fixture below a genuinely separate case rather
    // than a second reading of the same bytes.
    assert!(extras.block_ticks.is_empty() && extras.fluid_ticks.is_empty());
}

/// **The negative-delay gate.** Every one of the 17 saved ticks in a real
/// vanilla chunk carries a negative `t`, and the exact values come from the
/// independent parser.
///
/// A `u32`/`u64` delay field does not merely mis-order these — it panics in a
/// debug build and wraps to roughly 4.29 billion in release, which schedules
/// the tick about 6,800 years out. There is no "close enough" failure mode, so
/// this asserts the exact extremes rather than a sign.
#[test]
fn every_saved_tick_in_a_real_vanilla_chunk_has_a_negative_delay() {
    let extras = extras(TICKS_CHUNK);
    assert_eq!(
        (extras.block_ticks.len(), extras.fluid_ticks.len()),
        (1, 16),
        "the fixture's own counts, read with an independent parser"
    );

    let block = &extras.block_ticks[0];
    assert_eq!(
        block,
        &SavedTick {
            pos: (908, 20, -452),
            kind: "minecraft:gravel".to_owned(),
            delay: -83,
            priority: TickPriority::Normal,
        },
        "the one block tick, field for field"
    );

    // The exact multiset, not a range and not a sign: ten entries 80 ticks
    // overdue and six 79 ticks overdue. An off-by-one in the delay decode
    // would still satisfy "all negative", and a sign flip would still satisfy
    // "sixteen distinct values".
    let mut delays: Vec<i32> = extras.fluid_ticks.iter().map(|tick| tick.delay).collect();
    delays.sort_unstable();
    assert_eq!(
        delays,
        vec![-80; 10]
            .into_iter()
            .chain(vec![-79; 6])
            .collect::<Vec<i32>>(),
        "the fixture's own delay multiset, read with an independent parser"
    );

    let mut kinds: Vec<&str> = extras
        .fluid_ticks
        .iter()
        .map(|tick| tick.kind.as_str())
        .collect();
    kinds.sort_unstable();
    assert_eq!(
        (
            kinds.iter().filter(|k| **k == "minecraft:flowing_water").count(),
            kinds.iter().filter(|k| **k == "minecraft:water").count(),
        ),
        (14, 2),
        "the fluid queue carries fluid ids — and `water` and `flowing_water` \
         are different ids, not one normalised to the other"
    );
}

/// **The priority-value gate**, and the one assertion here whose expected
/// value cannot come from our own encoder.
///
/// A real vanilla chunk's `p` is `0`, and leaf decay and fluid spread — which
/// is all 17 of the fixture's ticks — are scheduled at `NORMAL`. So `p == 0`
/// must decode to [`TickPriority::Normal`].
///
/// The **control** is the second assertion: `Normal`'s *ordinal* is `3`, and
/// `3` decodes to `ExtremelyLow`. If this crate ever wrote the ordinal, every
/// ordinary tick in every saved world would come back as the lowest priority
/// in the game, and a round trip through our own reader would still be green.
#[test]
fn vanilla_p_is_the_priority_value_and_the_ordinal_would_be_a_different_priority() {
    let extras = extras(TICKS_CHUNK);
    assert!(
        extras
            .block_ticks
            .iter()
            .chain(&extras.fluid_ticks)
            .all(|tick| tick.priority == TickPriority::Normal),
        "every real vanilla tick in this chunk is p=0, which is NORMAL"
    );

    // The control: what the ordinal hypothesis would have produced.
    assert_eq!(tick_priority_value(TickPriority::Normal), 0);
    assert_eq!(
        tick_priority_from_value(3),
        TickPriority::ExtremelyLow,
        "3 is Normal's *ordinal* and ExtremelyLow's *value* — writing the \
         ordinal would silently demote every tick in the world"
    );

    // The full table, against `TickPriority.java:6-12`.
    for (priority, value) in [
        (TickPriority::ExtremelyHigh, -3),
        (TickPriority::VeryHigh, -2),
        (TickPriority::High, -1),
        (TickPriority::Normal, 0),
        (TickPriority::Low, 1),
        (TickPriority::VeryLow, 2),
        (TickPriority::ExtremelyLow, 3),
    ] {
        assert_eq!(tick_priority_value(priority), value);
        assert_eq!(tick_priority_from_value(value), priority);
    }

    // `TickPriority.byValue`'s own out-of-range clamp (`:21-29`).
    assert_eq!(tick_priority_from_value(-99), TickPriority::ExtremelyHigh);
    assert_eq!(tick_priority_from_value(99), TickPriority::ExtremelyLow);
}

/// A real vanilla chunk is **full of block entities this crate does not
/// simulate**, and reading one must skip them rather than fail.
///
/// The fixture chunks happen to hold only kinds we model or none at all, so
/// this drives the decoder with a hand-built list of the ids actually measured
/// in `.cache/mc` — chest, vault, mob spawner, decorated pot, brushable block
/// — and requires the modelled one in the same list to survive.
///
/// Before issue #477, every unmodelled block entity was silently dropped —
/// a chest loaded and re-saved lost its contents. The `Opaque` variant now
/// preserves every entry verbatim so the whole set round-trips.
#[test]
fn unmodelled_block_entity_ids_are_skipped_rather_than_failing_the_chunk() {
    use lodestone_core::Nbt;

    let entry = |id: &str, x: i32| {
        Nbt::Compound(vec![
            ("id".to_owned(), Nbt::String(id.to_owned())),
            ("x".to_owned(), Nbt::Int(x)),
            ("y".to_owned(), Nbt::Int(64)),
            ("z".to_owned(), Nbt::Int(0)),
        ])
    };
    let nbt = Nbt::Compound(vec![(
        "block_entities".to_owned(),
        Nbt::List {
            element_type: lodestone_core::NbtTag::Compound,
            elements: vec![
                entry("minecraft:chest", 0),
                entry("minecraft:vault", 1),
                entry("minecraft:mob_spawner", 2),
                entry("minecraft:decorated_pot", 3),
                entry("minecraft:brushable_block", 4),
                entry("minecraft:hopper", 5),
            ],
        },
    )]);

    let extras = chunk_nbt::extras_from_nbt(&nbt);
    assert_eq!(
        extras.block_entities.len(),
        6,
        "all six entries survive — modelled and unmodelled alike"
    );
    // The hopper (modelled) is at x=5 and resolves as a concrete variant.
    assert!(matches!(
        extras.block_entities[5].1,
        BlockEntity::Hopper(_)
    ));
    // The chest (unmodelled) is at x=0 and preserved verbatim as Opaque.
    assert!(matches!(
        extras.block_entities[0].1,
        BlockEntity::Opaque { .. }
    ));
}

/// The terrain decoder still reads a fixture carrying block entities and
/// ticks — the control that adding three lists to the schema did not disturb
/// the half issue #437 already gated.
#[test]
fn the_terrain_half_still_decodes_from_the_same_bytes() {
    for bytes in [BLOCK_ENTITY_CHUNK, TICKS_CHUNK] {
        let mut reader = Reader::new(bytes);
        let (_, nbt) = read_named_nbt(&mut reader).expect("valid named NBT");
        let column = chunk_nbt::column_from_nbt(&nbt, -64, 384).expect("terrain still decodes");
        assert_eq!(column.min_y, -64);
        assert_eq!(column.height, 384);
    }
}
