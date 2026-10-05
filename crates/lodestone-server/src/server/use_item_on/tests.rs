//! Tests for block placement helpers and propagation.

use super::*;
use lodestone_model::Vec3;
use crate::server::tests::stack;

#[test]
fn selected_placement_item_validates_the_held_stack_once() {
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, Some(stack("minecraft:redstone", 1)));

    assert_eq!(
        selected_placement_item(&inventory, 0),
        Some(Item::Redstone),
        "a built-in held item must enter placement as its registry type"
    );

    inventory.set_native(
        0,
        Some(ItemStack::new(
            ResourceKey::new("example", "custom_block").expect("valid custom key"),
            1,
        )),
    );
    assert_eq!(
        selected_placement_item(&inventory, 0),
        None,
        "an item outside the built-in registry cannot enter the typed placement path"
    );
}

/// The yaw → horizontal-facing map is vanilla's own yaw-to-direction conversion
/// (vanilla's own per-variant direction field table): yaw 0 = south, 90 = west, ±180 = north,
/// -90 = east, split at the 45° midpoints (the value at which
/// `floor(yaw / 90 + 0.5) & 3` rolls over). This is the facing a placed
/// diode then inverts so the block faces the player.
#[test]
fn horizontal_look_direction_matches_vanilla_from_y_rot() {
    assert_eq!(horizontal_look_direction(0.0), Direction::South);
    assert_eq!(horizontal_look_direction(90.0), Direction::West);
    assert_eq!(horizontal_look_direction(180.0), Direction::North);
    assert_eq!(horizontal_look_direction(-90.0), Direction::East);
    // The 45°/135°/225°/315° midpoints land exactly as the bit-mask's
    // `floor` does.
    assert_eq!(horizontal_look_direction(44.0), Direction::South);
    assert_eq!(horizontal_look_direction(45.0), Direction::West);
    assert_eq!(horizontal_look_direction(135.0), Direction::North);
    assert_eq!(horizontal_look_direction(225.0), Direction::East);
    assert_eq!(horizontal_look_direction(315.0), Direction::South);
    assert_eq!(horizontal_look_direction(-45.0), Direction::South);
    assert_eq!(horizontal_look_direction(-135.0), Direction::East);
    assert_eq!(horizontal_look_direction(-225.0), Direction::North);
    assert_eq!(horizontal_look_direction(-315.0), Direction::West);
    // Wraps around rather than clamping at ±180.
    assert_eq!(horizontal_look_direction(450.0), Direction::West);
    assert_eq!(horizontal_look_direction(-450.0), Direction::East);
}

// `gamemode_command_parses_names_aliases_and_ids` was here, testing
// `parse_gamemode_command` — a hand-rolled string split that has been
// deleted. `/gamemode` is now a real Brigadier command in
// `crate::commands::gamemode`, gated against the captured vanilla tree
// (`crates/protocol/v770/tests/builtin_command_parity.rs`) and driven
// end-to-end by `tests/builtin_commands.rs`.
//
// Worth recording rather than silently dropping: the deleted test asserted
// that `gamemode c` and `gamemode 1` parse as creative. **26.2 accepts
// neither.** vanilla's own game-type name lookup is an exact match against the four
// `getSerializedName` values, so the old parser — and the test that pinned
// it — were *more* permissive than vanilla. No test could have caught that,
// because the failure only ever made a command work that should have failed.

/// The three redstone families keep the full property set the signal model
/// reads, and everything else falls through to `crate::block_placement`
/// (whose own tests cover the per-block conventions). The observer is
/// deliberately **not** inverted: it watches in the player's look direction,
/// unlike the diodes' single inversion, which makes them face the player.
#[test]
fn placed_block_state_faces_diodes_at_the_player_and_observers_with_the_player() {
    let looking = |yaw: Option<f32>| crate::block_placement::PlaceContext {
        target: BlockPos::new(0, 64, 0),
        face: BlockFace::Up,
        cursor: Vec3f {
            x: 0.5,
            y: 0.0,
            z: 0.5,
        },
        yaw,
        pitch: Some(0.0),
        sneaking: false,
    };
    let air = |_: BlockPos| WorldState::from(StateId::AIR);
    let state = |block: &str, yaw: Option<f32>| {
        placed_block_state(
            Block::from_name(block.strip_prefix("minecraft:").unwrap_or(block))
                .expect("fixture block is built in"),
            &looking(yaw),
            air,
        )
        .map(|placed| placed.state)
    };
    // Looking north (yaw 180): a repeater and comparator face the player —
    // south — while an observer watches north.
    let repeater = state("minecraft:repeater", Some(180.0)).expect("placed repeater");
    assert_eq!(repeater.block(), Block::Repeater);
    assert_eq!(crate::redstone::get_str_property(repeater, PropertyKey::Facing), Some(BuiltinPropertyValue::South));
    let comparator = state("minecraft:comparator", Some(180.0)).expect("placed comparator");
    assert_eq!(comparator.block(), Block::Comparator);
    assert_eq!(crate::redstone::get_str_property(comparator, PropertyKey::Facing), Some(BuiltinPropertyValue::South));
    let observer = state("minecraft:observer", Some(180.0)).expect("placed observer");
    assert_eq!(observer.block(), Block::Observer);
    assert_eq!(crate::redstone::get_str_property(observer, PropertyKey::Facing), Some(BuiltinPropertyValue::North));
    // Looking east (yaw -90): a repeater faces west.
    let repeater = state("minecraft:repeater", Some(-90.0)).expect("placed repeater");
    assert_eq!(repeater.block(), Block::Repeater);
    assert_eq!(crate::redstone::get_str_property(repeater, PropertyKey::Facing), Some(BuiltinPropertyValue::West));
    // Blocks without any orientation keep the bare census name.
    assert_eq!(state("minecraft:dirt", Some(0.0)), None);
    // And no yaw reported yet keeps the bare name for the directional
    // families too.
    assert_eq!(state("minecraft:repeater", None), None);
}

#[test]
fn rejected_two_cell_placement_still_sends_the_partner_correction() {
    let clicked = BlockPos::new(4, 64, 4);
    let target = BlockPos::new(4, 65, 4);
    let upper = BlockPos::new(4, 66, 4);
    let changed = Vec::new();
    let updates = placement_update_positions(clicked, target, &[target, upper], &changed);
    assert_eq!(updates, vec![clicked, target, upper]);

    // The accepted path's fan-out may mention the same positions repeatedly,
    // but the wire still carries one update per cell.
    let changed = vec![(upper, StateId::AIR), (target, StateId::AIR)];
    let updates = placement_update_positions(clicked, target, &[target, upper], &changed);
    assert_eq!(updates, vec![clicked, target, upper]);
}

// -----------------------------------------------------------------
// `placement_obstructs_placer` — the server-side obstruction check used by
// `apply_use_item_on`. A full block cannot be placed through a standing
// player. These tests exercise the pure geometry directly rather
// than driving the whole `apply_use_item_on` pipeline, which needs a live
// `ChunkSource`/`BlockEntityHandle`/`MobHandle` fixture this predicate
// does not touch.
// -----------------------------------------------------------------

/// A full block at the target cell occupied by the player must be refused.
#[test]
fn placement_obstructs_placer_refuses_a_full_block_at_the_players_feet() {
    let target = BlockPos::new(0, 64, 0);
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(placement_obstructs_placer(target, Block::Stone.default_state(), feet));
}

/// The discriminating arm: a state with an **empty** collision shape must
/// never be refused, even at the player's own feet — otherwise this is a
/// blanket "nothing inside the player" rule rather than a real
/// obstruction test. Empty-shape blocks such as torches, rails, pressure
/// plates, and redstone dust remain placeable at those coordinates.
#[test]
fn placement_obstructs_placer_allows_an_empty_shape_at_the_players_feet() {
    let target = BlockPos::new(0, 64, 0);
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(
        lodestone_data::collision_shapes::collision_boxes(
            lodestone_data::block_states::StateId::new(
                lodestone_data::block_states::state_id("minecraft:torch").unwrap(),
            ).expect("torch validates")
        )
        .is_empty(),
        "this test's premise: a torch has no collision boxes"
    );
    assert!(!placement_obstructs_placer(target, Block::Torch.default_state(), feet));
}

/// Control: the same full block, far from the player, must not be
/// refused — proves the detector is a real geometric test and not an
/// unconditional `true`.
#[test]
fn placement_obstructs_placer_allows_a_full_block_far_from_the_player() {
    let target = BlockPos::new(50, 64, 50);
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(!placement_obstructs_placer(target, Block::Stone.default_state(), feet));
}

/// Boundary control: a full block exactly adjacent to the player (sharing
/// only a face) must not be refused — two boxes that only touch are not
/// intersecting, the same strict-inequality convention
/// `lodestone_shell::sim::placement::block_intersects_player` uses for
/// the client's own prediction of this rule.
#[test]
fn placement_obstructs_placer_allows_a_full_block_touching_but_not_overlapping() {
    let target = BlockPos::new(1, 64, 0);
    // Feet at x=0.5, half-width 0.3: the player's box is x in
    // [0.2, 0.8], which shares the x=1.0 boundary with `target` (x in
    // [1.0, 2.0]) without entering it.
    let feet = Vec3::new(0.5, 64.0, 0.5);
    assert!(!placement_obstructs_placer(target, Block::Stone.default_state(), feet));
}

/// The state-shaped case a full-cube approximation gets wrong: a top
/// slab occupies only the *upper* half of its cell, so a player whose own
/// box just clears that upper half is not obstructed by it — while an
/// (otherwise identically positioned) full block still would be. The
/// slab's real bottom edge is read from the live collision-shape table
/// rather than assumed, and the player's feet are derived from it
/// algebraically so the test holds regardless of the shape's exact
/// height.
#[test]
fn placement_obstructs_placer_lets_a_top_slab_clear_the_players_head_where_a_full_block_would_not()
{
    let target = BlockPos::new(0, 66, 0);
    let top_slab = "minecraft:oak_slab[type=top,waterlogged=false]";
    let id = lodestone_data::block_states::state_id(top_slab)
        .expect("minecraft:oak_slab[type=top,waterlogged=false] is a real 26.2 state");
    let state = lodestone_data::block_states::StateId::new(id).expect("top slab validates");
    let boxes = lodestone_data::collision_shapes::collision_boxes(state);
    assert!(!boxes.is_empty(), "a top slab has real collision geometry");
    let box_min_y = boxes
        .iter()
        .map(|b| b.min[1])
        .fold(f32::INFINITY, f32::min);
    assert!(
        box_min_y > 0.1,
        "a top slab must not fill the bottom half of its cell, got {box_min_y}"
    );
    // Stand so the player's own box top lands just under the slab's real
    // bottom edge (clears the slab) but still inside the target cell
    // (so an equivalently placed full block, whose bottom edge is the
    // cell floor, still hits the player).
    let feet_y = f64::from(target.y) + f64::from(box_min_y) - 1.8 - 0.05;
    let feet = Vec3::new(0.5, feet_y, 0.5);
    assert!(
        !placement_obstructs_placer(target, state, feet),
        "a top slab should clear the player's head here"
    );
    assert!(
        placement_obstructs_placer(target, Block::Stone.default_state(), feet),
        "a full block at the same position should still hit the player's head"
    );
}

/// `player_overlaps_piston_sweep` verifies the overlap test used for
/// connection-side piston self-correction. A player standing in either the
/// source or destination cell must overlap; one standing a full block clear
/// of both must not.
#[test]
fn player_overlaps_piston_sweep_matches_source_and_dest_but_not_clear_ground() {
    let source = BlockPos::new(4, 0, 0);
    let dest = BlockPos::new(5, 0, 0);

    assert!(
        player_overlaps_piston_sweep(5.5, 0.0, 0.5, source, dest),
        "a player standing in the destination cell must overlap"
    );
    assert!(
        player_overlaps_piston_sweep(4.5, 0.0, 0.5, source, dest),
        "a player standing in the source cell must overlap too"
    );
    assert!(
        !player_overlaps_piston_sweep(10.5, 0.0, 0.5, source, dest),
        "control: a player well clear of both cells must not overlap"
    );
}
