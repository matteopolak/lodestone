//! The `VersionAdapter::block_hardness` seam: proves the version-owned
//! hardness census (`lodestone_data::hardness`, 32,366 real block states dumped
//! from a headless 26.2 server) actually reaches a version-free consumer
//! **through the trait object**.
//!
//! Every adapter here is bound as `&dyn VersionAdapter` before it is called.
//! That is the whole point: `tests/hardness.rs` already covers the concrete
//! table bit-for-bit, so a test that called `V770Adapter::block_hardness`
//! directly would prove nothing new. What can silently break is the *seam* —
//! a missing `impl` override leaves the trait's `None` default in place and the
//! shell sees "this version has no hardness data" while the table sits right
//! there, which is exactly the shape of the bug this seam exists to prevent.
//!
//! # The trap this seam is documented against
//!
//! `BlockHardness::requires_correct_tool` is `vanilla's own block state's own requires correct tool for drops`
//! — a property of the *block*. `lodestone-game`'s `BreakInputs.correct_tool` is
//! `vanilla's own player's own has correct tool for drops` — a property of the *player's held item vs.
//! the block*. Bare-handed they are near-opposites
//! (`correct_tool == !requires_correct_tool`), so assigning this field straight
//! across makes stone break in 45 ticks instead of 151. See the doc comment on
//! [`lodestone_model::BlockHardness`].

use lodestone_model::VersionAdapter;
use lodestone_data::{block_states, hardness};
use lodestone_v26_2::V770Adapter;

/// Binds the concrete adapter behind the trait object, so every assertion below
/// travels the same dynamic-dispatch path a version-free consumer uses after
/// `lodestone_registry::adapter_for_protocol`.
fn seam() -> Box<dyn VersionAdapter> {
    Box::new(V770Adapter::new())
}

/// First state id whose block name matches `name` — robust to id shifts across
/// data bumps, matching `tests/hardness.rs`.
fn first_id_named(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} present in the block-state table"))
}

#[test]
fn seam_returns_real_values_not_the_trait_default() {
    let adapter = seam();

    let bedrock = adapter
        .block_hardness(first_id_named("minecraft:bedrock"))
        .expect("bedrock resolves through the trait object");
    assert_eq!(
        bedrock.hardness, -1.0,
        "bedrock must be unbreakable through the seam"
    );

    let obsidian = adapter
        .block_hardness(first_id_named("minecraft:obsidian"))
        .expect("obsidian resolves through the trait object");
    assert_eq!(obsidian.hardness, 50.0, "obsidian hardness through the seam");
    assert!(
        obsidian.requires_correct_tool,
        "obsidian must require the correct tool for drops"
    );

    let dirt = adapter
        .block_hardness(first_id_named("minecraft:dirt"))
        .expect("dirt resolves through the trait object");
    assert_eq!(dirt.hardness, 0.5, "dirt hardness through the seam");
    assert!(
        !dirt.requires_correct_tool,
        "dirt must not require the correct tool for drops"
    );

    let stone = adapter
        .block_hardness(first_id_named("minecraft:stone"))
        .expect("stone resolves through the trait object");
    assert!(
        stone.requires_correct_tool,
        "stone must require the correct tool for drops"
    );
    assert!(
        stone.hardness > 0.0,
        "stone must be breakable, got {}",
        stone.hardness
    );
}

#[test]
fn air_is_state_zero_and_costs_nothing_to_break() {
    // Vanilla air is state 0; a consumer that indexes an empty chunk section
    // asks for exactly this id, so it must resolve rather than report unknown.
    assert_eq!(block_states::block_name(0), Some("minecraft:air"));
    let entry = seam().block_hardness(0).expect("air resolves");
    assert_eq!(entry.hardness, 0.0);
    assert!(!entry.requires_correct_tool);
}

#[test]
fn seam_covers_the_whole_state_id_space() {
    let adapter = seam();
    for id in 0..hardness::STATE_COUNT {
        assert!(
            adapter.block_hardness(id).is_some(),
            "state {id} did not resolve through the trait object"
        );
    }
}

#[test]
fn seam_agrees_with_the_version_table_for_every_state() {
    // Guards the delegation itself: a transposed field or a stray offset in the
    // `impl` would pass the spot checks above but fail here.
    let adapter = seam();
    for id in 0..hardness::STATE_COUNT {
        let direct = hardness::hardness(
            lodestone_data::block_states::StateId::new(id).expect("state id is in range"),
        );
        let through_seam = adapter.block_hardness(id).expect("seam resolves");
        assert_eq!(
            (through_seam.hardness.to_bits(), through_seam.requires_correct_tool),
            (direct.hardness.to_bits(), direct.requires_correct_tool),
            "seam disagrees with the version table at state {id}"
        );
    }
}

#[test]
fn out_of_range_state_ids_are_none_through_the_seam() {
    let adapter = seam();
    assert_eq!(adapter.block_hardness(hardness::STATE_COUNT), None);
    assert_eq!(adapter.block_hardness(u32::MAX), None);
}
