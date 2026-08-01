//! The `VersionAdapter::block_bubble_column_drag` seam: proves the bubble column's
//! `drag` property actually reaches a version-free consumer **through the trait
//! object**. Issue #199.
//!
//! Every adapter here is bound as `&dyn VersionAdapter` before it is called, for the
//! same reason `tests/block_hardness_seam.rs` does it: calling the concrete
//! `V770Adapter` method directly would prove the lookup works and say nothing about
//! whether the override is *installed*. A missing `impl` override leaves the trait's
//! `None` default in place, and then `lodestone-physics` sees "there are no bubble
//! columns in this world" while the property sits right there in rodata — a working
//! impulse reaching zero pixels, which is this repo's dominant defect class.
//!
//! # Where the expected values come from
//!
//! Mojang's own generator, `.cache/mc/26.2/generated/reports/blocks.json`:
//!
//! ```text
//! "minecraft:bubble_column": {
//!   "properties": { "drag": ["true", "false"] },
//!   "states": [ { "default": true, "id": 15294, "properties": { "drag": "true" } },
//!               {                   "id": 15295, "properties": { "drag": "false" } } ]
//! }
//! ```
//!
//! The numeric ids are **not** asserted directly — they shift on a data bump, and
//! the repo's convention (`first_id_named`) is to look states up by name. What is
//! asserted is the part that cannot drift without the game changing: there are
//! exactly two bubble-column states, `drag=true` is the block's *default*, and
//! `true` means drag **down**.
//!
//! # The direction is the whole payload
//!
//! `Some(true)` is the magma-block drain and `Some(false)` the soul-sand lift.
//! Inverting them turns every elevator into a drowning trap and vice versa, and
//! nothing about the resulting motion looks malformed — it just goes the wrong way.
//! `BubbleColumnBlock.getColumnState` is the authority: `ENABLES_BUBBLE_COLUMN_PUSH_UP`
//! (soul sand) sets `DRAG_DOWN` to `false`, `ENABLES_BUBBLE_COLUMN_DRAG_DOWN`
//! (magma block) sets it to `true`.

use lodestone_data::block_states;
use lodestone_model::VersionAdapter;
use lodestone_v770::V770Adapter;

/// Binds the concrete adapter behind the trait object, so every assertion below
/// travels the same dynamic-dispatch path a version-free consumer uses after
/// `lodestone_registry::adapter_for_protocol`.
fn seam() -> Box<dyn VersionAdapter> {
    Box::new(V770Adapter::new())
}

/// The `drag` value the state table records for `id`, read independently of the
/// adapter — this is the cross-check the seam's answer is compared against.
fn table_drag(id: u32) -> Option<bool> {
    block_states::properties(id)?
        .iter()
        .find(|(name, _)| *name == "drag")
        .map(|(_, value)| *value == "true")
}

fn bubble_column_states() -> Vec<u32> {
    (0..block_states::STATE_COUNT)
        .filter(|&id| block_states::block_name(id) == Some("minecraft:bubble_column"))
        .collect()
}

/// The seam answers for both bubble-column states, and agrees with the state table.
#[test]
fn seam_reports_drag_for_both_bubble_column_states() {
    let a = seam();
    let states = bubble_column_states();
    assert_eq!(
        states.len(),
        2,
        "expected exactly two bubble-column states (drag true/false), got {states:?}"
    );

    for id in states {
        let via_seam = a.block_bubble_column_drag(id);
        assert_eq!(
            via_seam,
            table_drag(id),
            "state {id}: seam said {via_seam:?}, state table says {:?}",
            table_drag(id)
        );
        assert!(
            via_seam.is_some(),
            "state {id} is a bubble column but the seam reported None — the \
             `block_bubble_column_drag` override is missing and physics will see no \
             columns at all"
        );
    }
}

/// `drag=true` is the block's **default** state, per `blocks.json`'s `"default":
/// true` marker and `BubbleColumnBlock`'s constructor
/// (`registerDefaultState(… setValue(DRAG_DOWN, true))`, `BubbleColumnBlock.java:49`).
///
/// This is the id-independent way to pin the direction: the lower of the two state
/// ids is the default one, and it must be the drain.
#[test]
fn default_bubble_column_state_drags_down() {
    let a = seam();
    let states = bubble_column_states();
    let default_id = *states.iter().min().expect("two states");
    assert_eq!(
        a.block_bubble_column_drag(default_id),
        Some(true),
        "the default bubble-column state (id {default_id}) must be the drag-down \
         drain, not the push-up lift"
    );
    let other_id = *states.iter().max().expect("two states");
    assert_eq!(
        a.block_bubble_column_drag(other_id),
        Some(false),
        "the non-default bubble-column state (id {other_id}) must be the push-up lift"
    );
}

/// **The over-match control.** Exactly two states in the entire 32,366-state palette
/// may answer `Some`.
///
/// This is the assertion that would catch a lookup keyed on the `drag` property
/// alone: `drag` is not reserved to this block by anything structural, so a
/// property-only match would silently widen the moment another block gained one, and
/// every such cell would start shoving the player vertically. It is also what proves
/// the override is installed at all — with the trait's `None` default this count is
/// zero, not two.
#[test]
fn no_other_state_reports_a_bubble_column() {
    let a = seam();
    let answering: Vec<u32> = (0..block_states::STATE_COUNT)
        .filter(|&id| a.block_bubble_column_drag(id).is_some())
        .collect();
    assert_eq!(
        answering,
        bubble_column_states(),
        "the seam answered for {} states; only the two bubble-column states may",
        answering.len()
    );
}

/// Neighbouring blocks that are *involved* in bubble columns but are not columns
/// themselves must report `None`.
///
/// Soul sand and magma block are the two base blocks that *create* a column. It
/// would be an easy and invisible error to answer for them — they are the blocks a
/// reader associates with the feature — and the impulse would then fire while the
/// player stood on the sand rather than in the water above it.
#[test]
fn base_blocks_and_water_report_none() {
    let a = seam();
    for name in [
        "minecraft:soul_sand",
        "minecraft:magma_block",
        "minecraft:water",
        "minecraft:air",
        "minecraft:stone",
        "minecraft:kelp",
    ] {
        let id = (0..block_states::STATE_COUNT)
            .find(|&id| block_states::block_name(id) == Some(name))
            .unwrap_or_else(|| panic!("{name} present in the block-state table"));
        assert_eq!(
            a.block_bubble_column_drag(id),
            None,
            "{name} (state {id}) is not a bubble column"
        );
    }
}
