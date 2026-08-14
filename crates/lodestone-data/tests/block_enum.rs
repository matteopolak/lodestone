//! Gates for the generated [`Block`] enum and the [`StateId`] newtype.
//!
//! # Provenance of every expected value here
//!
//! The anchor is `tests/support/tool_jvm.txt`, the committed headless-26.2-server
//! dump of `BuiltInRegistries.BLOCK`'s registration order — the same external
//! file the generator reads, but parsed here by an independent parser, and
//! compared against the *compiled* enum rather than against the generator's
//! output string. So a generator that emitted a plausible-but-wrong table would
//! still be caught: `committed_tables_match_dump` in `tools.rs` proves the file
//! on disk is what the generator produces, and this proves what the compiler
//! made of it is what the server said.
//!
//! The default-state expectations come from the same dump family — the
//! `is_default_state` column, which is the server's own
//! `state == state.getBlock().defaultBlockState()` answer — and are deliberately
//! taken at blocks where the *wrong* hypothesis (the lowest state id of the
//! block's span) gives a different number.

use lodestone_data::block::{Block, BlockKind, BlockRef, CustomBlockId};
use lodestone_data::block_states::{self, StateId};

/// The committed JVM dump, re-parsed here rather than shared with the generator.
const DUMP: &str = include_str!("support/tool_jvm.txt");

/// `(registry id, canonical name)` for every block, straight out of the dump's
/// `B` records.
fn dump_blocks() -> Vec<(u16, String)> {
    let mut rows: Vec<(u16, String)> = DUMP
        .lines()
        .filter_map(|line| {
            let mut tok = line.split(' ');
            (tok.next()? == "B").then(|| {
                let id: u16 = tok.next().expect("block id").parse().expect("u16");
                let name = tok.next().expect("block name").to_owned();
                (id, name)
            })
        })
        .collect();
    rows.sort_unstable();
    assert!(
        rows.len() > 1_000,
        "the dump yielded {} block records — it did not parse, which must not read as a pass",
        rows.len()
    );
    rows
}

/// The load-bearing identity: the enum discriminant *is* the wire's registry id,
/// and the name attached to it is the one the server registered there.
///
/// This is the assertion that makes every array-indexed-by-`Block` census in the
/// crate correct. It fails if the generator ever emits the enum in an order
/// other than registration order — which is exactly the mistake
/// `block_type_name` already shipped once in the other direction, when a
/// registry id was used to index a name-sorted table.
#[test]
fn discriminant_is_the_registry_id_and_names_match_the_server_dump() {
    let rows = dump_blocks();
    assert_eq!(rows.len(), Block::COUNT as usize, "dump/enum block count");

    let mut mismatches = Vec::new();
    for (id, name) in &rows {
        match Block::from_registry_id(*id) {
            Some(block) => {
                if block.registry_id() != *id || block.name() != name {
                    mismatches.push(format!(
                        "id {id} ({name}): enum gave {:?} = id {} name {}",
                        block,
                        block.registry_id(),
                        block.name()
                    ));
                }
            }
            None => mismatches.push(format!("id {id} ({name}): enum has no such registry id")),
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} blocks disagree with the dump:\n{}",
        mismatches.len(),
        rows.len(),
        mismatches.join("\n")
    );
    assert_eq!(Block::from_registry_id(Block::COUNT), None);
    assert_eq!(Block::all().len(), rows.len());
}

/// `Block::from_name` binary-searches a permutation sorted by *full* name and
/// compares *paths*. That is only sound because every built-in name shares the
/// one `minecraft:` prefix, so the two orders coincide — an assumption with an
/// expiry date, since a namespace was a plausible thing for a future version to
/// add.
#[test]
fn name_order_is_also_path_order() {
    let names: Vec<&'static str> = Block::all().map(Block::name).collect();
    let paths: Vec<&'static str> = Block::all().map(Block::path).collect();
    let mut by_name: Vec<usize> = (0..names.len()).collect();
    let mut by_path = by_name.clone();
    by_name.sort_by_key(|&i| names[i]);
    by_path.sort_by_key(|&i| paths[i]);
    assert_eq!(
        by_name, by_path,
        "sorting blocks by namespaced name and by bare path give different orders; \
         `Block::from_name`'s bare-path search is no longer sound"
    );
}

#[test]
fn names_and_paths_round_trip_through_from_name() {
    let mut failures = Vec::new();
    for block in Block::all() {
        if Block::from_name(block.name()) != Some(block) {
            failures.push(format!("{}: namespaced form did not round-trip", block.name()));
        }
        if Block::from_name(block.path()) != Some(block) {
            failures.push(format!("{}: bare path did not round-trip", block.path()));
        }
        if block.name() != format!("minecraft:{}", block.path()) {
            failures.push(format!("{}: name/path disagree", block.name()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} round-trip failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Negative arms. A name from another namespace is never a built-in block even
/// when its path is one — a plugin's `mypack:stone` must not resolve to
/// `Block::Stone`, which is the whole reason `from_name` inspects the namespace
/// rather than splitting and discarding it.
#[test]
fn from_name_rejects_foreign_namespaces_and_unknown_paths() {
    assert_eq!(Block::from_name("minecraft:stone"), Some(Block::Stone));
    assert_eq!(Block::from_name("stone"), Some(Block::Stone));
    assert_eq!(Block::from_name("mypack:stone"), None);
    assert_eq!(Block::from_name("minecraft:not_a_block"), None);
    assert_eq!(Block::from_name(""), None);
    assert_eq!(Block::from_name("minecraft:"), None);
}

/// The representation claims, as numbers.
///
/// `Option<Block>` costing the same as `Block` is not a nicety: it is what makes
/// a `[Option<Block>; N]` census — like `block_items`' — half the width of the
/// `[Option<&str>; N]` it replaced on a 32-bit target and a quarter on a 64-bit
/// one, with no relocations at all.
#[test]
fn the_representation_is_the_size_it_claims() {
    assert_eq!(size_of::<Block>(), 2, "Block is a u16 discriminant");
    assert_eq!(
        size_of::<Option<Block>>(),
        2,
        "Option<Block> must use the 64,340 unused discriminants as its niche"
    );
    assert_eq!(size_of::<BlockRef>(), 4);
    assert_eq!(size_of::<StateId>(), 4);
    // The alternative this design rejects, for scale: a name column row.
    assert_eq!(size_of::<Option<&'static str>>(), size_of::<usize>() * 2);
}

/// `BlockRef` must cost the built-in path nothing and still carry a custom id
/// losslessly, including at the boundary where the two encodings meet.
#[test]
fn block_ref_separates_builtin_from_custom_without_aliasing() {
    for block in Block::all() {
        let reference = BlockRef::builtin(block);
        assert_eq!(reference.kind(), BlockKind::Builtin(block));
        assert_eq!(reference.builtin_or_none(), Some(block));
    }
    // The boundary: custom index 0 sits immediately above the last built-in id
    // and must not read as one.
    for index in [0u32, 1, 7, 1_000_000, u32::MAX - Block::COUNT as u32] {
        let id = CustomBlockId::from_index(index);
        let reference = BlockRef::custom(id);
        assert_eq!(reference.kind(), BlockKind::Custom(id));
        assert_eq!(reference.builtin_or_none(), None);
        assert_eq!(
            reference.kind(),
            BlockKind::Custom(CustomBlockId::from_index(index)),
            "custom index {index} did not survive the round trip"
        );
    }
    assert_ne!(
        BlockRef::builtin(Block::from_registry_id(Block::COUNT - 1).expect("last block")),
        BlockRef::custom(CustomBlockId::from_index(0)),
        "the last built-in block and the first custom block collided"
    );
}

/// `Block::default_state` must be the server's default, not the block's lowest
/// state id.
///
/// The inputs are chosen so the two hypotheses differ: for each block the test
/// asserts the default is the state the dumped census marks *and* records
/// whether the lowest id would have given a different answer, then requires that
/// discriminating case to have actually occurred — otherwise this would be a
/// test whose input cannot distinguish what it exists to distinguish.
#[test]
fn default_state_is_the_servers_default_not_the_lowest_id() {
    // Lowest state id per block, computed independently of the default column.
    let mut lowest: Vec<Option<u32>> = vec![None; Block::COUNT as usize];
    for raw in 0..block_states::STATE_COUNT {
        let state = StateId::new(raw).expect("in range");
        let slot = &mut lowest[state.block().registry_id() as usize];
        if slot.is_none() {
            *slot = Some(raw);
        }
    }

    let mut wrong_answers = Vec::new();
    let mut discriminating = 0usize;
    for block in Block::all() {
        let default = block.default_state();
        if !default.is_default() {
            wrong_answers.push(format!(
                "{}: default_state {} is not marked default by the census",
                block.name(),
                default.raw()
            ));
        }
        if default.block() != block {
            wrong_answers.push(format!(
                "{}: default_state {} belongs to {}",
                block.name(),
                default.raw(),
                default.block().name()
            ));
        }
        if lowest[block.registry_id() as usize] != Some(default.raw()) {
            discriminating += 1;
        }
    }
    assert!(
        wrong_answers.is_empty(),
        "{} blocks have a wrong default state:\n{}",
        wrong_answers.len(),
        wrong_answers.join("\n")
    );
    assert_eq!(
        discriminating, 661,
        "the number of blocks where the lowest state id is NOT the default changed; if this \
         reaches 0 the test can no longer tell the two hypotheses apart"
    );

    // Two of the three shipped bugs the lowest-id hypothesis caused, as named
    // cases: spread grass rendered snowy, and redstone dust rendered climbing.
    assert_eq!(
        Block::GrassBlock
            .default_state()
            .properties()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![("snowy", "false")]
    );
    assert!(
        Block::RedstoneWire
            .default_state()
            .properties()
            .iter()
            .all(|&(key, value)| key == "power" || value == "none"),
        "redstone dust's default connections must be `none`, not `up`"
    );
}

/// `StateId`'s reason to exist: validation once, total accessors afterwards.
#[test]
fn state_id_validates_once_and_is_total_afterwards() {
    assert_eq!(StateId::new(block_states::STATE_COUNT), None);
    assert_eq!(StateId::new(u32::MAX), None);
    let air = StateId::new(block_states::air_state_id()).expect("air is in range");
    assert_eq!(air.block(), Block::Air);
    assert_eq!(air.properties(), &[]);
    assert!(air.is_default());

    // Every state resolves to a block, and the block agrees with the free
    // function's name — the two paths reach the registry through different
    // tables (registration-ordered vs name-sorted), so this is a real join.
    let mut mismatches = 0usize;
    for raw in 0..block_states::STATE_COUNT {
        let state = StateId::new(raw).expect("in range");
        if Some(state.block().name()) != block_states::block_name(raw) {
            mismatches += 1;
        }
    }
    assert_eq!(mismatches, 0, "StateId::block disagrees with block_name");
}

/// The typed placement accessor and the string one must be the same fact.
#[test]
fn block_items_typed_and_string_accessors_agree() {
    use lodestone_data::block_items;
    assert_eq!(
        block_items::block_placed_by("minecraft:redstone"),
        Some(Block::RedstoneWire),
        "the census's flagship disagreement with a name match"
    );
    assert_eq!(block_items::block_placed_by("minecraft:diamond_sword"), None);
    let mut checked = 0usize;
    for id in 0..block_items::ITEM_COUNT {
        let typed = block_items::block_for_item_id(id as i32);
        assert_eq!(typed.map(Block::name), {
            let name = lodestone_data::items::item_name(id as i32);
            name.and_then(block_items::block_for_item)
        });
        if typed.is_some() {
            checked += 1;
        }
    }
    assert_eq!(checked, 1054, "placeable-item count changed");
}
