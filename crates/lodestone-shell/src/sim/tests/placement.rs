use super::*;
mod placement_oracle {
    /// `chest[type=single,facing=north,waterlogged=false]` — the registered
    /// default produced by the placement rules when facing north.
    pub const CHEST_NORTH: u32 = 3988;
    /// `chest[type=single,facing=south,waterlogged=false]`.
    pub const CHEST_SOUTH: u32 = 3994;
    /// `oak_slab[type=bottom,waterlogged=false]`.
    pub const OAK_SLAB_BOTTOM: u32 = 13333;
    /// `oak_slab[type=top,waterlogged=false]`.
    pub const OAK_SLAB_TOP: u32 = 13331;
    /// `oak_log[axis=y]`.
    pub const OAK_LOG_Y: u32 = 137;
    /// `stone` — the one propertyless case.
    pub const STONE: u32 = 1;
    /// `oak_door[facing=south,half=lower,hinge=left,open=false,powered=false]`.
    pub const OAK_DOOR_LOWER: u32 = 5682;
    /// The corresponding upper cell.
    pub const OAK_DOOR_UPPER: u32 = 5674;
    /// `white_bed[facing=south,occupied=false,part=foot]`.
    pub const WHITE_BED_FOOT: u32 = 1938;
    /// The corresponding head cell.
    pub const WHITE_BED_HEAD: u32 = 1937;
    /// `small_dripleaf[facing=north,half=lower,waterlogged=false]`.
    pub const SMALL_DRIPLEAF_LOWER: u32 = 30399;
    /// The corresponding upper cell.
    pub const SMALL_DRIPLEAF_UPPER: u32 = 30397;
}

/// The production seam, not a re-spelling of it — [`predicted_placement_state`]
/// is what `use_item_live` resolves through and what the pixel gate drives.
fn resolve(block: &str, placed: PlacedState) -> Option<u32> {
    predicted_placement_state(block, &placed)
}

/// The resolver must hit the block's own placement state exactly — including
/// the two properties the census cannot default (`waterlogged`, a chest's
/// `type`), because "lowest state id for this block" gets **both** wrong:
/// `BooleanProperty`'s value order is `{true, false}`, so the lowest chest id
/// is a *waterlogged* chest and the lowest slab id is a *top* slab.
#[test]
fn placement_states_resolve_to_the_jar_oracle() {
    assert_eq!(
        resolve(
            "minecraft:chest",
            PlacedState {
                facing: Some(BlockFace::North),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::CHEST_NORTH),
        "a chest facing north must resolve to type=single, waterlogged=false"
    );
    assert_eq!(
        resolve(
            "minecraft:chest",
            PlacedState {
                facing: Some(BlockFace::South),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::CHEST_SOUTH),
        "facing must actually reach the resolved state, not be dropped"
    );
    assert_eq!(
        resolve(
            "minecraft:oak_slab",
            PlacedState {
                half: Some(Half::Bottom),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::OAK_SLAB_BOTTOM)
    );
    assert_eq!(
        resolve(
            "minecraft:oak_slab",
            PlacedState {
                half: Some(Half::Top),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::OAK_SLAB_TOP),
        "the slab's half must select type=top, not the block's default"
    );
    assert_eq!(
        resolve(
            "minecraft:oak_log",
            PlacedState {
                axis: Some(Axis::Y),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::OAK_LOG_Y)
    );
    assert_eq!(
        resolve("minecraft:stone", PlacedState::default()),
        Some(placement_oracle::STONE)
    );
}

#[test]
fn two_cell_states_resolve_both_cells_from_one_prediction() {
    let door = block_states_of("minecraft:oak_door").expect("door is in the census");
    let door_placed = PlacedState {
        facing: Some(BlockFace::South),
        ..PlacedState::default()
    };
    assert_eq!(
        orientation_for_placement("minecraft:oak_door", &door),
        Some(OrientationKind::Door)
    );
    assert_eq!(
        state_for_placement(
            "minecraft:oak_door",
            &door,
            OrientationKind::Door,
            &door_placed,
        ),
        Some(placement_oracle::OAK_DOOR_LOWER)
    );
    assert_eq!(
        state_for_extra_placement(
            "minecraft:oak_door",
            &door,
            OrientationKind::Door,
            &door_placed,
        ),
        Some(placement_oracle::OAK_DOOR_UPPER)
    );

    let bed = block_states_of("minecraft:white_bed").expect("bed is in the census");
    let bed_placed = PlacedState {
        facing: Some(BlockFace::South),
        ..PlacedState::default()
    };
    assert_eq!(
        state_for_placement("minecraft:white_bed", &bed, OrientationKind::Bed, &bed_placed),
        Some(placement_oracle::WHITE_BED_FOOT)
    );
    assert_eq!(
        state_for_extra_placement("minecraft:white_bed", &bed, OrientationKind::Bed, &bed_placed),
        Some(placement_oracle::WHITE_BED_HEAD)
    );

    let dripleaf = block_states_of("minecraft:small_dripleaf").expect("dripleaf is in the census");
    let dripleaf_placed = PlacedState {
        facing: Some(BlockFace::North),
        half: Some(Half::Bottom),
        ..PlacedState::default()
    };
    assert_eq!(
        state_for_placement(
            "minecraft:small_dripleaf",
            &dripleaf,
            OrientationKind::SmallDripleaf,
            &dripleaf_placed,
        ),
        Some(placement_oracle::SMALL_DRIPLEAF_LOWER)
    );
    assert_eq!(
        state_for_extra_placement(
            "minecraft:small_dripleaf",
            &dripleaf,
            OrientationKind::SmallDripleaf,
            &dripleaf_placed,
        ),
        Some(placement_oracle::SMALL_DRIPLEAF_UPPER)
    );
}

/// The declines, and why each one is a decline rather than a guess. Without
/// these the resolver would look "complete" while writing states the server
/// immediately contradicts.
#[test]
fn unclassifiable_placements_decline_rather_than_guess() {
    for (block, why) in [
        // A 4-way `facing` the census cannot tell from a chest's, and vanilla
        // points it *toward* the player.
        ("minecraft:ladder", "FacingHorizontal is not classified"),
        // `shape` comes from the neighbouring rails.
        ("minecraft:rail", "neighbour-derived shape"),
        // `persistent` is set *true* for a player-placed leaf, so the
        // registered default would be actively wrong.
        ("minecraft:oak_leaves", "persistent is placement-derived"),
        // Not in the horizontal-facing list — and its `mode` has no
        // consistent default across the blocks that carry one either.
        ("minecraft:comparator", "unclassified 4-way facing"),
        // Not a block at all.
        ("minecraft:diamond_sword", "not a block item"),
    ] {
        assert_eq!(
            resolve(block, PlacedState::default()),
            None,
            "{block} must decline ({why}); predicting it would write a state the \
             server contradicts one round trip later"
        );
    }
}

/// A right-click on a solid cell with a chest in hand must decide `Place` into
/// the adjacent air cell, and a right-click on the chest itself must decide
/// `Interact` — the branch that keeps the prediction from dropping a ghost
/// chest beside the one you meant to open.
#[test]
fn placement_facts_drive_the_place_versus_interact_decision() {
    let clicked = BlockPos::new(4, 64, 9);
    let target = BlockPos::new(4, 65, 9);
    let solid_ground = PlacementFacts {
        clicked,
        target,
        clicked_replaceable: false,
        clicked_interactable: false,
        target_replaceable: true,
        target_obstructed: false,
    };
    let chest = PlacementFacts {
        clicked_interactable: true,
        ..solid_ground
    };
    let ctx = UseOnContext {
        hand: Hand::Main,
        clicked,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 1.0, 0.5),
        inside_block: false,
        rotation: Rotation::new(0.0, 0.0),
        sneaking: false,
        has_item_in_hand: true,
        placing: Some("minecraft:chest".parse().expect("identifier")),
        orientation: OrientationKind::FacingHorizontalOpposite,
    };

    let mut placement = Placement::new();
    let decision = placement.use_on(&ctx, &solid_ground);
    let UseOnDecision::Place { prediction, .. } = &decision else {
        panic!("a chest onto solid ground must place, got {decision:?}");
    };
    assert_eq!(prediction.pos, target, "the placement goes into the air cell");
    assert_eq!(
        state_for_placement(
            "minecraft:chest",
            &block_states_of("minecraft:chest").expect("chest is a block"),
            OrientationKind::FacingHorizontalOpposite,
            &prediction.state,
        ),
        // Yaw 0 faces +Z (south), and a chest faces *away* from the player.
        Some(placement_oracle::CHEST_NORTH),
        "the prediction's geometry must survive into the resolved state"
    );
    assert_eq!(placement.pending().len(), 1);

    let mut placement = Placement::new();
    assert!(
        matches!(
            placement.use_on(&ctx, &chest),
            UseOnDecision::Interact { .. }
        ),
        "clicking an interactable block must not predict a placement"
    );
    assert!(
        placement.pending().is_empty(),
        "an interaction records nothing to reconcile"
    );

    // Obstruction and an unloaded/solid target both decline, which is what
    // keeps a prediction from landing inside the player or in a cell we cannot
    // see.
    for facts in [
        PlacementFacts {
            target_obstructed: true,
            ..solid_ground
        },
        PlacementFacts {
            target_replaceable: false,
            ..solid_ground
        },
    ] {
        assert!(
            matches!(
                Placement::new().use_on(&ctx, &facts),
                UseOnDecision::Nothing { .. }
            ),
            "an illegal target must not predict: {facts:?}"
        );
    }
}

/// A container is interactable through the block-entity census, not through
/// the name list — that is what makes the list's gaps cost a round trip
/// instead of a wrong right-click on a chest.
#[test]
fn every_container_is_interactable_and_plain_terrain_is_not() {
    let state = |name: &str| {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
            .unwrap_or_else(|| panic!("{name} is not in the 26.2 census"))
    };
    let typed_state = |name| {
        lodestone_data::block_states::StateId::new(state(name))
            .unwrap_or_else(|| panic!("{name} must be a generated state"))
    };
    for name in [
        "minecraft:chest",
        "minecraft:barrel",
        "minecraft:furnace",
        "minecraft:hopper",
        "minecraft:oak_door",
        "minecraft:crafting_table",
    ] {
        assert!(
            is_interactable_state(typed_state(name)),
            "{name} must suppress the placement prediction"
        );
    }
    for name in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_planks"] {
        assert!(
            !is_interactable_state(typed_state(name)),
            "{name} must not suppress it — this is the 95% case"
        );
    }
    assert!(is_air_state(typed_state("minecraft:air")));
    assert!(!is_air_state(typed_state("minecraft:water")));
}

#[test]
fn placement_facts_validate_raw_chunk_states_before_classification() {
    let clicked = BlockPos::new(4, 64, 9);
    let face = BlockFace::Up;
    let invalid = placement_facts(
        clicked,
        face,
        |_| Some(lodestone_data::block_states::STATE_COUNT),
        |_| false,
    );
    assert!(
        !invalid.clicked_replaceable && !invalid.clicked_interactable && !invalid.target_replaceable,
        "an out-of-census chunk value must decline prediction rather than borrow a built-in row"
    );

    let air = lodestone_data::block_states::air_state_id();
    let valid = placement_facts(clicked, face, |_| Some(air), |_| false);
    assert!(valid.clicked_replaceable, "canonical air remains replaceable");
    assert!(valid.target_replaceable, "in-place air uses the same validated value");
    assert!(
        !valid.clicked_interactable,
        "the valid-air control must not pass by turning every accepted state into an interaction"
    );
}

#[test]
fn hotbar_selection_updates_and_echoes_to_the_server() {
    use lodestone_client::ClientAction;
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    // Vanilla default is slot 0, and selecting it again is a no-op (no
    // redundant packet).
    assert_eq!(sim.selected_slot(), 0);
    sim.select_slot(0);

    // A direct selection moves and echoes exactly one SetCarriedItem.
    sim.select_slot(3);
    assert_eq!(sim.selected_slot(), 3);

    // Out-of-range is ignored (no 10th slot), leaving selection and the
    // wire untouched.
    sim.select_slot(9);
    assert_eq!(sim.selected_slot(), 3);

    // Scroll wraps at both ends: +1 from 3 → 4, and from 8 → 0.
    sim.cycle_slot(1);
    assert_eq!(sim.selected_slot(), 4);
    sim.select_slot(8);
    sim.cycle_slot(1);
    assert_eq!(
        sim.selected_slot(),
        0,
        "scroll past the last slot wraps to 0"
    );
    sim.cycle_slot(-1);
    assert_eq!(
        sim.selected_slot(),
        8,
        "scroll before the first slot wraps to 8"
    );

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    // Every *change* echoes SetCarriedItem; the no-op select_slot(0) and the
    // rejected select_slot(9) send nothing, so the wire shows only the moves.
    assert_eq!(
        sent,
        vec![
            ClientAction::SetCarriedItem { slot: 3 },
            ClientAction::SetCarriedItem { slot: 4 },
            ClientAction::SetCarriedItem { slot: 8 },
            ClientAction::SetCarriedItem { slot: 0 },
            ClientAction::SetCarriedItem { slot: 8 },
        ],
        "only real selection changes reach the outbound action seam"
    );
}
