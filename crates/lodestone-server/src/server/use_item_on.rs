//! Using an item on a block: placement, per-block interactions, and propagation of the placement or removal to neighbouring blocks, pistons and entities.

use super::*;

/// Vanilla's own yaw-to-direction conversion restricted to the
/// four horizontal directions, from a player yaw in degrees.
///
/// The 2d-data layout is `south=0, west=1, north=2, east=3`
/// (vanilla's own per-variant direction field table), so `floor(yaw / 90 + 0.5) & 3` maps yaw `0` →
/// south, `90` → west, `±180` → north, `-90` → east — the same "yaw 0 =
/// south, increasing clockwise" convention the shell's `camera_rig`/`hud`
/// use for the yaw this server receives from `move_player_rot`. Implemented
/// as a range match on the wrapped `[0, 360)` value rather than the bit-mask
/// formula, with the 45°/135°/225°/315° midpoints landing exactly as the
/// mask's `floor` does.
///
/// The returned direction is the one the player is **looking**, matching the
/// horizontal component of vanilla's own nearest-looking-direction getter —
/// a placed diode then applies `.opposite()` so the block faces the player.
#[must_use]
pub(super) fn horizontal_look_direction(yaw: f32) -> Direction {
    match yaw.rem_euclid(360.0) {
        y if (45.0..135.0).contains(&y) => Direction::West,
        y if (135.0..225.0).contains(&y) => Direction::North,
        y if (225.0..315.0).contains(&y) => Direction::East,
        _ => Direction::South,
    }
}

/// Selects the state for the block a player just placed, or
/// `None` when no convention applies and the caller should keep the census's
/// bare default-state name.
///
/// The per-block table lives in [`crate::block_placement`]; this wrapper exists
/// only to keep the three redstone families ahead of it. They are not a
/// different convention — a repeater uses the opposite horizontal direction
/// like a furnace — but the redstone model reads `delay`/`locked`/`powered`
/// straight off the state *string*, so their placement must name the full
/// property set rather than leaving it to be defaulted downstream.
///
/// The observer is deliberately still yaw-only here; the observer model can
/// resolve horizontal facing but not a vertical facing.
/// `crate::redstone_observer` models horizontal observers only, so a
/// `facing=up` observer would be a state the signal model cannot read.
pub(super) fn placed_block_state<F>(
    block: Block,
    ctx: &crate::block_placement::PlaceContext,
    block_at: F,
) -> Option<crate::block_placement::Placement>
where
    F: Fn(BlockPos) -> WorldState,
{
    if let Some(yaw) = ctx.yaw {
        let look = horizontal_look_direction(yaw);
        let full = match block {
            Block::Repeater => Some(set_repeater(look.opposite(), 1, false, false)),
            Block::Comparator => Some(set_comparator(look.opposite(), false, false, 0)),
            Block::Observer => Some(set_observer(look, false)),
            _ => None,
        };
        if let Some(state) = full {
            return Some(crate::block_placement::Placement {
                state,
                extra: Vec::new(),
            });
        }
    }
    crate::block_placement::placement(block.name(), ctx, block_at)
}

/// Whether a player standing with feet at `(px, py, pz)` overlaps the swept
/// region of a `moving_piston` cell travelling between `source` and `dest`
/// (the piston entity-push integration). The same box `crate::mobs::piston_shove::mob_aabb`
/// gives a mob (`0.6` wide, `1.8` tall — vanilla's own standing player
/// hitbox), against the same union-of-two-unit-cells region
/// `crate::mobs::piston_shove::swept_cell_aabb` builds for a mob; there is no
/// shared type between this crate's per-connection player state and its
/// `MobSim` world to call the mob version directly, so this is that same
/// arithmetic restated over plain floats rather than a second `Aabb` type
/// dependency.
pub(super) fn player_overlaps_piston_sweep(px: f64, py: f64, pz: f64, source: BlockPos, dest: BlockPos) -> bool {
    const HALF_WIDTH: f64 = 0.3;
    const HEIGHT: f64 = 1.8;
    let min_x = f64::from(source.x.min(dest.x));
    let max_x = f64::from(source.x.max(dest.x)) + 1.0;
    let min_y = f64::from(source.y.min(dest.y));
    let max_y = f64::from(source.y.max(dest.y)) + 1.0;
    let min_z = f64::from(source.z.min(dest.z));
    let max_z = f64::from(source.z.max(dest.z)) + 1.0;
    (px - HALF_WIDTH) < max_x
        && (px + HALF_WIDTH) > min_x
        && py < max_y
        && (py + HEIGHT) > min_y
        && (pz - HALF_WIDTH) < max_z
        && (pz + HALF_WIDTH) > min_z
}

/// Checks whether the clicked slab can be replaced:
/// `true` when placing `held` onto `clicked` should turn it into a double slab
/// rather than start a new one in the next cell.
///
/// This predicate is asked about the clicked block itself, so the whole rule is
/// "same slab, not already double, and the click was on the side the existing
/// half does not already fill".
#[must_use]
pub(super) fn slab_doubles(clicked: StateId, held: Block, face: BlockFace, cursor: Vec3f) -> bool {
    if crate::redstone::base_name(clicked) != held {
        return false;
    }
    let above_middle = cursor.y > 0.5;
    let horizontal = !matches!(face, BlockFace::Up | BlockFace::Down);
    match crate::redstone::get_str_property(clicked, PropertyKey::Type) {
        Some(BuiltinPropertyValue::Bottom) => matches!(face, BlockFace::Up) || (above_middle && horizontal),
        Some(BuiltinPropertyValue::Top) => matches!(face, BlockFace::Down) || (!above_middle && horizontal),
        _ => false,
    }
}

/// Applies a right-click placement, mirroring
/// vanilla's own use-item-on handler's replace-vs-relative
/// choice of placement cell (the block place context's constructor: place at the
/// clicked block if it `canBeReplaced`, otherwise at its `face`-neighbour) —
/// simplified per this crate's documented scope (`docs/block-edit.md`): no
/// survival/collision validation beyond "is the target cell currently
/// replaceable" (air or a fluid — see [`is_air_or_fluid`], plus
/// [`slab_doubles`] for the one `canBeReplaced` override a hand placement can
/// hit). Per-block orientation now goes through [`crate::block_placement`],
/// which carries each family's own `getStateForPlacement` convention.
///
/// **Placement honours the held item for every block in the game.**
/// `inventory`'s currently selected item is resolved through
/// [`lodestone_data::block_items::block_placed_by`] — the 26.2 census of
/// vanilla's own block-item block getter, dumped from the real jar — which decides both
/// whether a placement happens and which block it writes.
///
/// The block-item census gates placement and names the block. The
/// [`block_entity_for_item`] lookup then inserts the live
/// [`crate::block_entities::BlockEntity`] for the six ticking block types;
/// ordinary blocks use the census result without that extra record.
///
/// **A non-placeable item places nothing.** A sword, a bucket, a spawn egg or
/// an empty hand leaves the world untouched. The `block_update` for both cells
/// is sent below, so a client
/// that predicted a placement is corrected rather than left desynchronised.
///
/// **Block *state* comes from the block's own convention.** The clicked face,
/// the cursor hit within it and the placing player's yaw/pitch all reach
/// [`crate::block_placement`], so a stair faces the way the player does and is
/// upper or lower depending on where in the face they clicked, a chest and a
/// furnace face the *other* way, an anvil is turned a quarter further, and a
/// torch clicked against a wall becomes a `wall_torch`. Two-cell placements (a
/// door's upper half, a bed's head, a paired chest's partner) travel out as
/// `Placement::extra` and are written and notified with the primary cell.
///
/// Sends [`ServerProtocol::encode_block_update`] for **both** `pos` and its
/// `face`-neighbour unconditionally, matching vanilla's own
/// use-item-on handler, which
/// sends both regardless of whether the placement succeeded — this doubles
/// as the correction for a client that predicted a placement the server
/// rejected.
///
/// **Right-clicking a block that already has an *openable* container opens
/// its screen instead of attempting a placement at all** — the closing half
/// of the block-entity interaction section in `docs/block-entities.md`. The
/// interaction order is:
/// clicked-block hand use (which is what opens a furnace/hopper's menu)
/// **before** any placement logic, and a block
/// that opens a menu never falls through to placement.
///
/// **A brewing stand at `pos` is this "clicked block's own use" step too,
/// but without a menu**: it cannot be opened — `menu_name` answers `None`,
/// because its bottle slots are not real `ItemStack`s — so
/// [`insert_into_brewing_stand`] stands in for the menu with a direct
/// one-item-per-click insert, the same interaction shape used for
/// the composter (which also has no menu). A held item that
/// belongs in a brewing stand is routed into the matching slot and consumed;
/// an unrelated held item still falls through to the placement logic below
/// and leaves unrelated held items to the placement logic.
///
/// Whether writing `state` at `target` would intersect the placer's own
/// bounding box, narrowed to the one entity this server can currently name
/// at a placement site: the placer, from `player_pos`. A full
/// The complete check would test every entity's bounding box in the cell and
/// exclude spectators; this crate has no per-connection entity-bounding-box
/// registry to query the rest of, so another player or a mob standing in the
/// target cell is not yet refused — see `docs/block-edit.md`.
///
/// A state with an **empty** collision shape (a torch, a rail, a pressure
/// plate, redstone dust…) is never obstructed — placing one at your own feet is
/// legal here.
///
/// The placer's box uses player dimensions (`0.6 x 1.8`, centred
/// horizontally on `feet`, `feet.y..feet.y + 1.8` vertically) —
/// the unobstructed check reads the entity's own bounding-box getter at click time, which does
/// not shrink for the sneaking pose (`1.5`), so this does not model pose
/// either.
pub(super) fn placement_obstructs_placer(target: BlockPos, state: StateId, feet: Vec3) -> bool {
    let boxes = lodestone_data::collision_shapes::collision_boxes(state);
    let (px0, px1) = (feet.x - 0.3, feet.x + 0.3);
    let (py0, py1) = (feet.y, feet.y + 1.8);
    let (pz0, pz1) = (feet.z - 0.3, feet.z + 0.3);
    boxes.iter().any(|b| {
        let bx0 = f64::from(target.x) + f64::from(b.min[0]);
        let bx1 = f64::from(target.x) + f64::from(b.max[0]);
        let by0 = f64::from(target.y) + f64::from(b.min[1]);
        let by1 = f64::from(target.y) + f64::from(b.max[1]);
        let bz0 = f64::from(target.z) + f64::from(b.min[2]);
        let bz1 = f64::from(target.z) + f64::from(b.max[2]);
        // Strict inequalities: two boxes that only share a face are touching,
        // not intersecting — the same convention
        // `lodestone_shell::sim::placement::block_intersects_player` uses for
        // the client's own (coarser, full-cell) prediction of this same rule.
        bx1 > px0 && bx0 < px1 && by1 > py0 && by0 < py1 && bz1 > pz0 && bz0 < pz1
    })
}

/// Resolves the selected stack's built-in item once for a placement attempt.
///
/// Custom registry entries have no built-in [`Item`] value, so they cannot
/// enter the built-in placement census.
pub(super) fn selected_placement_item(inventory: &PlayerInventory, native_slot: usize) -> Option<Item> {
    let item = &inventory.native(native_slot)?.item;
    (item.namespace() == "minecraft")
        .then(|| Item::from_name(item.path()))
        .flatten()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_use_item_on<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pending_relights: Option<&mut PendingRelights>,
    pos: BlockPos,
    face: BlockFace,
    // The block-local hit position within `pos`. `crate::block_placement` reads
    // its `y` for every `half`/`type`-bearing block (a stair, slab or trapdoor
    // clicked high on a side face is an upper one) and its `x`/`z` for a door's
    // hinge tie-break.
    cursor: Vec3f,
    // The player's world-space position, for the bed-respawn
    // reach test (bed ±3 x/z and ±2 y). `None` until
    // the first `PlayerMoved` packet arrives; a bed click before any move
    // skips the reach test rather than rejecting (see
    // [`is_legal_bed_respawn`]'s doc comment).
    player_pos: Option<Vec3>,
    // The player's per-player respawn point, written when a legal
    // bed is right-clicked (see the bed arm below). `&mut`: the set writes
    // through this slot.
    respawn: &mut Option<RespawnPoint>,
    // The placing player's yaw, so the directional families can
    // derive their `facing` (see [`placed_block_state`]). `None` until the
    // first packet carrying angles arrives; placement then falls back to the
    // block's default state.
    player_yaw: Option<f32>,
    // Pitch, for the direction-sensitive families alone (a dispenser
    // or piston placed while looking down points up). `None` on the same
    // terms as `player_yaw`.
    player_pitch: Option<f32>,
    // Whether this click used the secondary-use (sneak) input. A block item
    // held while sneaking bypasses the clicked container's own use.
    sneaking: bool,
    // The placing player, for the place sound's `except` argument (see the
    // `block_placed` call below).
    placer: uuid::Uuid,
    // `&mut`, not `&`: a brewing-stand insertion consumes one item from the
    // player's selected hotbar stack, and only a mutable
    // inventory can write the remainder back.
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // The composter interaction: `mobs` so a level-8 extraction
    // can spawn its bone-meal item entity, and `roll` — a fresh `[0.0, 1.0)`
    // draw from the connection's [`SpawnRng`], one per right-click, so the
    // fill machine's per-item chance sees a live sample rather than a constant
    // (the caller-supplied-roll shape `Composter::insert` documents).
    mobs: &MobHandle,
    roll: f64,
    // The delayed half: `propagate_placement` below resolves
    // everything synchronous (dust) against a `ScheduledTickQueue` it then
    // discards; a torch/repeater/comparator/observer instead *schedules*, and
    // only `tick::run_tick_loop` owns a queue those can land in. This asks the
    // loop to redo the fan-out on its next iteration, where the schedule
    // survives. See `BlockTickFeed`'s own doc comment.
    block_ticks: &BlockTickFeed,
    // The night-skip vote, written on a bed click (the bed arm
    // above — `lay_down`), and the key it stores this connection's player
    // under — see `dispatch_play_packet`'s parameter comment.
    sleep_vote: &SleepVote,
    player_entity_id: i32,
    // This connection's bone-meal roll source. A whole `SpawnRng` rather than a
    // pre-drawn value like the composter's `roll` above, because
    // `crate::bone_meal::apply_bone_meal` draws a *variable* number of values —
    // one for a crop, one for a sapling, none for a non-target — and the draw
    // count per use is part of the specification its own tests pin. Pre-drawing
    // would fix the count at one and desynchronise the stream.
    bone_meal_rng: &mut SpawnRng,
    // The world difficulty controls which spawn-egg species are permitted on
    // Peaceful. Passed by value because this function needs only the scalar;
    // taking the whole `WorldStateHandle` would add an unrelated read.
    difficulty: lodestone_model::Difficulty,
    // The acting player's game mode controls item consumption: creative
    // placement writes the block without consuming the held item. See the
    // consumption arm at the end of the placement branch.
    game_mode: GameMode,
    // A fresh `[0, i32::MAX)` draw from `dispatch_play_packet`'s `drops_rng`,
    // the same pre-drawn-value shape the composter `roll` above already
    // uses. Only consumed if this click opens an enchanting table (see
    // `open_enchanting_screen`'s own parameter comment); drawn unconditionally
    // by the caller anyway, matching the composter roll's own "one draw per
    // right-click, whatever block was hit" reasoning.
    enchant_seed_roll: i64,
    // `ServerBound::UseItemOn::hand` (`0` main, `1` off) selects the held
    // slot that `held_item` below reads from. Both hands therefore use the same
    // spawn-egg, flint-and-steel, and block-placement paths.
    hand: u8,
    // Only the narrow crafting-station hook registry, not the
    // whole `WorldStateHandle` — see `difficulty`'s own comment above for why
    // this function takes the scalar/handle it actually needs rather than a
    // handle that would invite a second, unrelated read.
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    // A chest placed by terrain generation (a shipwreck, igloo, or ocean ruin)
    // lives in the column, not the live registry. Hydrate it on the first click
    // so the generated loot opens correctly. Check the block kind first so an
    // ordinary right-click does not pay for the lookup.
    let container_here = block_entities.with(|reg| reg.get(pos).is_some());
    if !container_here {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        let block = clicked.block();
        if let Some(name) = crate::block_entities::container_type_for_block(block) {
            let generated = source
                .block_entity(pos.x, pos.y, pos.z)
                .unwrap_or_else(|| BlockEntity::container(name));
            block_entities.with(|reg| reg.insert(pos, generated));
        }
    }

    // A beacon's pyramid tier is recomputed fresh from the world on every
    // open — see `BeaconData::levels`'s own doc for why nothing refreshes it
    // in the background instead.
    block_entities.with(|reg| {
        if let Some(BlockEntity::Beacon(beacon)) = reg.get_mut(pos) {
            beacon.levels = crate::beacon::beacon_levels(source, pos.x, pos.y, pos.z);
        }
    });

    // Secondary use is what lets a block item pass through a clicked chest (or
    // other block entity with a menu) and place into the adjacent cell. Keep
    // the ordinary empty-hand sneak click as a menu open: only a real block
    // item has the alternate placement meaning.
    let hand_native_for_use = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let holding_block_item = selected_placement_item(inventory, hand_native_for_use)
        .is_some_and(|item| block_items::block_placed_by(item).is_some())
        || inventory
            .native(hand_native_for_use)
            .is_some_and(|stack| crate::cushion::color_for_stack(stack).is_some());
    let existing_menu = block_entities.with(|reg| reg.get(pos).and_then(BlockEntity::menu_name));
    if let Some(menu) = existing_menu.filter(|_| !sneaking || !holding_block_item) {
        return open_container_screen(
            conn,
            proto,
            state,
            block_entities,
            inventory,
            pos,
            menu,
            next_window_id,
            open_container,
            container_sync,
        )
        .await;
    }

    // A crafting table opens a *virtual* menu. It is not a block
    // entity, so the `existing_menu` branch above structurally cannot reach it —
    // see `open_crafting_table_screen`. Ahead of the placement branch for the same
    // reason the `hand_use` block is: right-clicking a table while holding a block
    // opens the table rather than building.
    if source.block_state_id(pos.x, pos.y, pos.z).block() == Block::CraftingTable {
        return open_crafting_table_screen(
            conn,
            proto,
            state,
            inventory,
            pos,
            next_window_id,
            open_container,
            container_sync,
        )
        .await;
    }

    // Workstation menus use per-menu input slots rather than block-entity
    // storage. The `existing_menu` branch therefore cannot find these stations;
    // dispatch them through their virtual menu implementations below.
    let clicked_block = source.block_state_id(pos.x, pos.y, pos.z).block();
    if let Some(station) = match clicked_block {
        Block::Anvil | Block::ChippedAnvil | Block::DamagedAnvil => Some(Station::Anvil),
        Block::Grindstone => Some(Station::Grindstone),
        Block::SmithingTable => Some(Station::Smithing),
        Block::Loom => Some(Station::Loom),
        Block::Stonecutter => Some(Station::Stonecutter),
        _ => None,
    } {
        return open_workstation_screen(
            conn,
            proto,
            state,
            inventory,
            pos,
            station,
            next_window_id,
            open_container,
            container_sync,
            hooks,
        )
        .await;
    }
    if clicked_block == Block::EnchantingTable {
        return open_enchanting_screen(
            conn,
            proto,
            source,
            state,
            inventory,
            pos,
            next_window_id,
            open_container,
            container_sync,
            enchant_seed_roll,
        )
        .await;
    }

    // A brewing-stand right-click routes the held item into the matching slot
    // (fuel, bottle, or ingredient) and consumes one from the player's hand. See
    // [`insert_into_brewing_stand`]'s doc comment for the three outcomes.
    match insert_into_brewing_stand(block_entities, inventory, pos) {
        BrewingInsertOutcome::Inserted(selected) => {
            // The stand consumed an item. Tell the client's window-0 hotbar
            // slot (menu slots `36..=44` -> native `0..=8`, vanilla's
            // The inventory menu) so the held count visibly drops — the same
            // server-initiated window-0 slot update vanilla broadcasts after
            // a composter click consumes one. `state_id` is `0`: this crate
            // applies a container click's own diff verbatim and never
            // validates a stale id (`apply_container_clicked`), so the
            // client adopting the value is harmless.
            let hotbar_slot = i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
            apply(conn, state, proto.encode_container_slot(0, 0, hotbar_slot, selected.as_ref())).await?;
            return Ok(());
        }
        BrewingInsertOutcome::Consumed => {
            // The stand ate the click but nothing moved (the matching slot
            // was full). No placement may follow — some ingredients are
            // themselves placeable blocks, and a full stand must not place one.
            return Ok(());
        }
        BrewingInsertOutcome::NotBrewing => {
            // Fall through to the ordinary placement logic below.
        }
    }

    // A composter right-click feeds the seven-tier fill state machine. See
    // [`apply_composter_use`]'s doc comment for the four outcomes. A handled
    // click returns before placement; only `NotComposter` reaches that branch.
    match apply_composter_use(block_entities, inventory, mobs, pos, roll) {
        ComposterUseOutcome::Consumed {
            remainder,
            block_state,
        } => {
            // Write the new fill level — only when it actually advanced; a
            // failed roll consumed the item but left the state alone.
            if let Some(block_state) = block_state {
                source.set_block(pos.x, pos.y, pos.z, block_state);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, block_state)).await?;
            }
            // Tell the client's window-0 hotbar slot (menu slots `36..=44` ->
            // native `0..=8`, vanilla's inventory menu) so the held count
            // visibly drops — the same server-initiated window-0 slot update
            // vanilla broadcasts after a composter click consumes one.
            // `state_id` is `0`, as in the brewing arm above (this crate
            // applies a container diff verbatim and never validates a stale
            // id).
            let hotbar_slot = i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
            apply(conn, state, proto.encode_container_slot(0, 0, hotbar_slot, remainder.as_ref())).await?;
            return Ok(());
        }
        ComposterUseOutcome::Extracted { block_state } => {
            source.set_block(pos.x, pos.y, pos.z, block_state);
            apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, block_state)).await?;
            return Ok(());
        }
        ComposterUseOutcome::Noop => return Ok(()),
        ComposterUseOutcome::NotComposter => {
            // Fall through to the ordinary placement logic below.
        }
    }

    // Bone meal on a growable block — the bone meal item's use on, the consuming half
    // of [`crate::bone_meal`]'s rule layer. Ahead of the placement branch for
    // the same reason the composter and brewing arms are: bone meal is not a
    // block item, but the *clicked* cell is often air-adjacent and a fall-through
    // would try to place whatever else is in hand.
    //
    // The three outcomes are not two: `ConsumedNoChange` is a real vanilla
    // result, because bone meal item shrinks the stack *outside* the success
    // branch — a failed sapling roll (55% of them) eats the item for nothing,
    // and treating that as a no-op would make bone meal infinitely efficient.
    // `NotModelled` deliberately consumes nothing: the grass-block and
    // stage-1-sapling paths need a worldgen feature placer this crate does not
    // have, and a partial version would consume a *different* number of RNG
    // draws and desynchronise every later use in the same stream.
    if inventory
        .selected_item()
        .is_some_and(|held| held.item.to_string() == crate::bone_meal::BONE_MEAL)
    {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        // The cell above supplies the light input for growth checks; it is
        // resolved here because `bone_meal` has no world access of its own.
        let above = source.block_state_id(pos.x, pos.y + 1, pos.z);
        let outcome = crate::bone_meal::apply_bone_meal(clicked, above, bone_meal_rng);
        // One helper for both consuming arms, performing the same one-item
        // shrink as the composter's `Consumed` arm.
        let consume = |inventory: &mut PlayerInventory| {
            let native = usize::from(inventory.selected_hotbar_slot());
            let remainder = inventory.native(native).cloned().and_then(|mut stack| {
                stack.count -= 1;
                (stack.count > 0).then_some(stack)
            });
            inventory.set_native(native, remainder.clone());
            remainder
        };
        match outcome {
            crate::bone_meal::BoneMealOutcome::Grew { state: new_state } => {
                source.set_block(pos.x, pos.y, pos.z, new_state);
                apply(
                    conn,
                    state,
                    proto.encode_block_update(pos.x, pos.y, pos.z, new_state),
                )
                .await?;
                let remainder = consume(inventory);
                let hotbar_slot =
                    i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, hotbar_slot, remainder.as_ref()),
                )
                .await?;
                return Ok(());
            }
            crate::bone_meal::BoneMealOutcome::ConsumedNoChange => {
                let remainder = consume(inventory);
                let hotbar_slot =
                    i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, hotbar_slot, remainder.as_ref()),
                )
                .await?;
                return Ok(());
            }
            // Not a target, or a family whose growth this crate cannot model:
            // fall through to the ordinary placement logic below, consuming
            // nothing.
            crate::bone_meal::BoneMealOutcome::NotBonemealable
            | crate::bone_meal::BoneMealOutcome::NotModelled { .. } => {}
        }
    }

    // Right-clicking a bed records a per-player respawn point and registers
    // the player for the sleep vote. A bed click is an interaction, not a
    // placement, so it returns before the inventory-placement logic. The
    // legality gate applies the three checks in [`is_legal_bed_respawn`].
    // Notify the client only when the stored point changes; a repeat click on
    // the same bed is silent. This crate has no localization table or action-bar
    // encoder, so the notification uses a plain system-chat line.
    let clicked_bed = source.block_state_id(pos.x, pos.y, pos.z);
    if is_bed_block(clicked_bed) || crate::world_spawn::is_straw_bed(clicked_bed) {
        // Register the player in the night-skip vote. Bed-entry gates for
        // day/night, nearby monsters, and already-sleeping state are outside
        // this interaction; the 100-tick deep-sleep threshold prevents a
        // single daytime click from advancing the vote. Registration is
        // idempotent, so a repeat click does not double-count.
        sleep_vote.lay_down(player_entity_id);
        let bed_dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
        if is_bed_block(clicked_bed)
            && crate::respawn::bed_works_in(bed_dimension)
            && is_legal_bed_respawn(source, pos, player_pos)
            && !respawn.is_some_and(|existing| existing.same_block(pos, bed_dimension))
        {
            *respawn = Some(RespawnPoint::block(pos, bed_dimension, player_yaw.unwrap_or(0.0)));
            apply(conn, state, proto.encode_system_chat("Respawn point set")).await?;
        }
        return Ok(());
    }

    // Right-clicking a respawn anchor: glowstone charges it, a charged anchor
    // sets the respawn point in the Nether and blasts anywhere else. See
    // [`crate::respawn_anchor`] for the decision table. A click that decides
    // `FallThrough` carries on to the ordinary placement logic.
    if crate::respawn_anchor::is_anchor(source.block_state_id(pos.x, pos.y, pos.z)) {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        let dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
        let main_native = usize::from(inventory.selected_hotbar_slot());
        let is_glowstone = |native: usize| {
            selected_placement_item(inventory, native) == Some(Item::Glowstone)
        };
        let holds_anything = |native: usize| inventory.native(native).is_some();
        let hand_native = if hand == 1 { OFFHAND_NATIVE } else { main_native };
        let outcome = crate::respawn_anchor::decide_use(crate::respawn_anchor::UseContext {
            charges: crate::respawn_anchor::charges(clicked),
            works_here: crate::respawn_anchor::works_in(dimension),
            clicking_hand_glowstone: is_glowstone(hand_native),
            main_hand: hand != 1,
            off_hand_glowstone: is_glowstone(OFFHAND_NATIVE),
            sneaking_with_item: sneaking
                && (holds_anything(main_native) || holds_anything(OFFHAND_NATIVE)),
            already_this_point: respawn.is_some_and(|existing| existing.same_block(pos, dimension)),
        });
        use crate::respawn_anchor::AnchorUse;
        match outcome {
            AnchorUse::FallThrough => {}
            AnchorUse::Defer | AnchorUse::AlreadySet => return Ok(()),
            AnchorUse::Charge => {
                let new_state = crate::respawn_anchor::with_charges(
                    crate::respawn_anchor::charges(clicked) + 1,
                );
                source.set_block(pos.x, pos.y, pos.z, new_state);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, new_state)).await?;
                block_ticks.publish_change(pos.x, pos.y, pos.z, clicked, new_state);
                // A comparator beside the anchor follows its charge.
                let (changed, scheduled) = propagate_placement_with_entities(source, pos, Some(block_entities));
                block_ticks.request_scheduled_ticks(scheduled);
                for (at, changed_state) in changed {
                    apply(conn, state, proto.encode_block_update(at.x, at.y, at.z, changed_state)).await?;
                }
                block_ticks.publish_effect(crate::respawn_anchor::block_sound("charge", pos));
                if consume_one(inventory, hand_native, game_mode)
                    && let Some(menu_slot) = crate::inventory::window_zero_menu_slot(hand_native)
                {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, inventory.native(hand_native)),
                    )
                    .await?;
                }
                return Ok(());
            }
            AnchorUse::SetSpawn => {
                *respawn = Some(RespawnPoint::block(pos, dimension, 0.0));
                apply(conn, state, proto.encode_system_chat(crate::respawn_anchor::RESPAWN_SET_MESSAGE)).await?;
                block_ticks.publish_effect(crate::respawn_anchor::block_sound("set_spawn", pos));
                return Ok(());
            }
            AnchorUse::Explode => {
                // The block goes first, so the blast does not shield itself.
                let air = crate::chunk::air_state();
                source.set_block(pos.x, pos.y, pos.z, air);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, air)).await?;
                block_ticks.publish_change(pos.x, pos.y, pos.z, clicked, air);
                let centre = Vec3::new(
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                );
                let smothered = crate::respawn_anchor::smothered_by_water(source, pos);
                mobs.with(|sim| sim.queue_blast(centre, crate::respawn_anchor::BLAST_POWER, !smothered, !smothered));
                return Ok(());
            }
        }
    }

    // The hand-use branch runs **ahead of the placement branch**: a door or
    // other usable block must handle a right-click before block placement;
    // otherwise the block would build instead of opening it. See `crate::hand_use` for the five
    // families and the rules each comes from.
    //
    // Returns early like the bed arm: a click that operated a block is not also a
    // placement.
    {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        if crate::hand_use::is_hand_usable(clicked) {
            // The door's partner half, read here because `hand_use` has no world
            // access. `None` for every other family, and for a door whose partner
            // is missing (a half-broken door, which vanilla also tolerates).
            let other_half = crate::redstone_openable::other_door_half_pos(pos, clicked)
                .map(|p| (p, source.block_state_id(p.x, p.y, p.z)));
            if let Some(used) = crate::hand_use::hand_use(pos, clicked, other_half, player_yaw) {
                let mut fanout: Vec<BlockPos> = Vec::new();
                for (p, new_state) in &used.changes {
                    source.set_block(p.x, p.y, p.z, *new_state);
                    fanout.push(*p);
                }
                // The placement fan-out notifies neighbouring blocks, so a lever
                // powers the wire beside it rather than merely looking flipped.
                // Without this notification, the redstone model stays correct but
                // is unreachable from a player's hand.
                let mut changed: Vec<(BlockPos, lodestone_data::block_states::StateId)> = Vec::new();
                let mut piston_records: Vec<(BlockPos, lodestone_core::Nbt)> = Vec::new();
                for p in &fanout {
                    let (mut more, scheduled) = propagate_placement_with_entities(source, *p, Some(block_entities));
                    piston_records.extend(moving_piston_records(&scheduled));
                    block_ticks.request_scheduled_ticks(scheduled);
                    changed.append(&mut more);
                }
                // A pressed button releases itself. Scheduled through the same
                // relative-delay feed a placement's delayed families use, so
                // `run_tick_loop` rebases it onto its own counter.
                if let Some(delay) = used.release_after {
                    // Built through a throwaway queue rather than a struct literal
                    // because `ScheduledTick`'s `sub_tick_order` is private — the
                    // same idiom `propagate_placement` uses to produce its own
                    // relative-delay batch, and for the same reason.
                    let mut pending: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
                    pending.schedule(
                        (pos.x, pos.y, pos.z),
                        crate::hand_use::TICK_BUTTON,
                        delay,
                        crate::scheduled_tick::TickPriority::Normal,
                    );
                    block_ticks.request_scheduled_ticks(pending.drain_due(u64::MAX, usize::MAX));
                }
                // Every cell the click rewrote, then every cell the fan-out did.
                let mut notify: Vec<BlockPos> = fanout;
                for (p, _) in &changed {
                    if !notify.contains(p) {
                        notify.push(*p);
                    }
                }
                for p in notify {
                    let current = source.block_state_id(p.x, p.y, p.z);
                    apply(conn, state, proto.encode_block_update(p.x, p.y, p.z, current)).await?;
                    if let Some((_, nbt)) = piston_records.iter().find(|(pos, _)| *pos == p) {
                        let directive = proto.encode_block_entity_data(
                            p,
                            crate::piston::PISTON_BLOCK_ENTITY,
                            nbt,
                        );
                        apply(conn, state, directive).await?;
                    }
                }
                return Ok(());
            }
            // `hand_use` said no (an iron door, or an already-pressed button).
            // Vanilla returns PASS/CONSUME, and in neither case does it fall
            // through to placement against the clicked cell — an iron door is not
            // replaceable, so the placement branch would do nothing anyway, but
            // returning here says why.
            return Ok(());
        }
    }

    let neighbour = relative(pos, face);
    let clicked = source.block_state_id(pos.x, pos.y, pos.z);
    // Which native slot this click reads from. The spawn-egg, flint-and-steel,
    // and block-placement branches below share this one
    // resolution point via `held_item`, so an item held only in the off hand
    // now reaches them instead of the main hand's slot always winning.
    let hand_native = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let held_item = selected_placement_item(inventory, hand_native);

    // Spawn-egg handling runs between clicked-block hand use and generic block
    // placement: an egg held over air must not place a block, while a lever
    // click must not consume the egg. See
    // `crate::spawn_egg` for the placement rule and `docs/spawn-eggs.md` for why
    // the item-to-entity mapping is a checked derivation rather than a table.
    //
    // A block entity at the clicked position is consulted first. Spawners are
    // not simulated here, so the guard is "there is a spawner here, do
    // nothing"; it prevents the egg from creating an unsupported mob.
    if let Some(item) = held_item {
        let spawner_here = block_entities.with(|reg| {
            reg.get(pos)
                .is_some_and(|entity| entity.kind() == BlockEntityKind::MobSpawner)
        });
        if !spawner_here {
            match crate::spawn_egg::apply_spawn_egg(
                item.name(),
                difficulty,
                pos,
                face,
                &|x, y, z| source.block_state_id(x, y, z),
                mobs,
            ) {
                // Not an egg: fall through to the placement branch below.
                crate::spawn_egg::SpawnEggApplied::NotSpawnEgg => {}
                // Vanilla `FAIL`: no entity, no placement, and the stack is
                // untouched. Returning here rather than falling through is the
                // load-bearing half — a refused egg must not place a block.
                crate::spawn_egg::SpawnEggApplied::Refused => return Ok(()),
                crate::spawn_egg::SpawnEggApplied::Spawned { .. } => {
                    // Consume one item *after* the spawn succeeds — the same
                    // shrink-and-report pair the composter and brewing
                    // arms above perform, including the window-0 hotbar slot
                    // update so the held count visibly drops.
                    //
                    // Routed through `consume_one` so creative players keep their
                    // eggs while survival players lose one.
                    let native = hand_native;
                    if consume_one(inventory, native, game_mode) && game_mode != GameMode::Creative {
                        let remainder = inventory.native(native).cloned();
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                            )
                            .await?;
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    // Minecart-item handling is a rail-targeted placement, checked ahead of the
    // generic block-placement branch: a minecart item is not a block, so that
    // branch cannot place one. A non-rail target is refused rather than falling
    // through to anything else.
    if let Some(item) = held_item {
        if let Some(kind) = crate::mobs::minecart::MinecartKind::from_item(item.name()) {
            let clicked = source.block_state_id(pos.x, pos.y, pos.z);
            if crate::mobs::minecart::is_rail_block(clicked) {
                let shape = crate::mobs::minecart::rail_shape(clicked);
                let position = crate::mobs::minecart::placement_position(pos, shape);
                mobs.with(|sim| {
                    sim.spawn_minecart(kind, position);
                });
                let native = hand_native;
                if consume_one(inventory, native, game_mode) && game_mode != GameMode::Creative {
                    let remainder = inventory.native(native).cloned();
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                        )
                        .await?;
                    }
                }
            }
            return Ok(());
        }
    }

    // A cushion item places an entity against the top face of the clicked block.
    // It is not a block item, so the placement branch below cannot reach it; a
    // refused placement keeps the stack and ends the click.
    if let Some(stack) = inventory.native(hand_native).cloned() {
        let eye = player_pos.map(|feet| Vec3::new(feet.x, feet.y + EYE_HEIGHT, feet.z));
        match crate::cushion::apply_cushion_item(
            &stack,
            pos,
            face,
            cursor,
            eye,
            player_yaw.unwrap_or(0.0),
            &|x, y, z| source.block_state_id(x, y, z),
            mobs,
        ) {
            crate::cushion::CushionApplied::NotACushion => {}
            crate::cushion::CushionApplied::Refused => return Ok(()),
            crate::cushion::CushionApplied::Placed { position, color, burned, .. } => {
                crate::cushion::publish_sound(block_ticks, crate::effects::CushionSound::Place, position);
                if burned {
                    crate::cushion::publish_broken(block_ticks, position, color);
                }
                let native = hand_native;
                if consume_one(inventory, native, game_mode) && game_mode != GameMode::Creative {
                    let remainder = inventory.native(native).cloned();
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                        )
                        .await?;
                    }
                }
                return Ok(());
            }
        }
    }

    // Lighting a nether portal. **Ahead of the placement branch**, for the same
    // reason the `hand_use` block above is: `flint_and_steel` is not a block item,
    // so the placement branch below cannot reach it at all.
    //
    // The flint-and-steel route places a fire cell, then runs the frame search **from the
    // cell the fire went in**, not from the block that was clicked — so the search
    // origin is `relative(pos, face)`. Clicking the top face of a frame's bottom
    // obsidian therefore searches from the lowest interior cell, which is what makes
    // the ordinary way of lighting a portal work.
    //
    // The fire itself is deliberately *not* placed when there is no frame: fire
    // spread needs `crate::fire::ticks_after_edit` and a live block-tick queue, and
    // an inert fire block would look like a working one. Flint and steel therefore
    // lights portals and nothing else here, and it takes no durability damage — both
    // gaps, both documented in `docs/nether-portals.md`, neither a regression (this
    // item did nothing at all before).
    if held_item == Some(Item::FlintAndSteel) {
        let dimension = source
            .dimension()
            .unwrap_or(crate::dimension::Dimension::Overworld);
        if let Some(cells) = crate::portal::ignite(source, dimension, neighbour) {
            for (cell, cell_state) in &cells {
                source.set_block(cell.x, cell.y, cell.z, *cell_state);
                apply(
                    conn,
                    state,
                    proto.encode_block_update(cell.x, cell.y, cell.z, *cell_state),
                )
                .await?;
            }
            // Publishing to the index is not bookkeeping — it is what lets the
            // *return* trip find this portal instead of building a second one beside
            // it. See `crate::portal::PortalIndex`.
            if let Some(index) = source.portal_index() {
                index.extend(dimension, cells.iter().map(|(cell, _)| *cell));
            }
            return Ok(());
        }
    }
    // Flint and steel or a fire charge clicked directly on a TNT block primes
    // it and clears the block. Checked
    // against the **clicked** cell (`pos`), not `neighbour` the portal arm
    // above reads: the action belongs to the block that was actually clicked,
    // not the face it was clicked from.
    //
    // No `tnt_explodes` gamerule gate here — this call site has no
    // `WorldStateHandle` in scope, matching the portal arm just above, which
    // takes no durability-damage gate either (this crate's own item stacks
    // carry no durability at all — see that arm's own comment). Both are
    // therefore true unconditionally, which is the default here.
    if matches!(held_item, Some(Item::FlintAndSteel | Item::FireCharge)) {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        if clicked.block() == Block::Tnt {
            let air = Block::Air.default_state();
            source.set_block(pos.x, pos.y, pos.z, air);
            apply(
                conn,
                state,
                proto.encode_block_update(pos.x, pos.y, pos.z, air),
            )
            .await?;
            mobs.with(|sim| {
                sim.spawn_tnt(
                    Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y), f64::from(pos.z) + 0.5),
                    crate::mobs::tnt::DEFAULT_FUSE_TIME,
                );
            });
            // A fire charge consumes one stack item. Flint and steel wear is
            // outside this crate's item model, so only the charge is shrunk.
            if held_item == Some(Item::FireCharge)
                && consume_one(inventory, hand_native, game_mode)
                && game_mode != GameMode::Creative
            {
                let remainder = inventory.native(hand_native).cloned();
                if let Some(menu_slot) = window_zero_menu_slot(hand_native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
    }
    // Vanilla's own ender-eye-item use-on routine: an eye of ender placed into an unfired
    // `end_portal_frame`. Also ahead of the placement branch — `ender_eye` is
    // not a block item, so the census below cannot reach it at all.
    //
    // `crate::portal::ignite_end_portal_frame` is the pure decision (its own
    // doc derives the ring's "every rim frame faces the centre" rule from
    // vanilla's own block-pattern engine, rather than porting that generic engine); this
    // call site owns every write, the same split `ignite` above uses. Vanilla
    // always writes `eye=true` and consumes the eye on any unfired frame,
    // whether or not a ring completes; the 3x3 `end_portal` fill only follows
    // when this eye is the twelfth.
    if held_item == Some(Item::EnderEye) {
        if let Some(ignition) = crate::portal::ignite_end_portal_frame(source, pos) {
            let (frame_pos, frame_state) = &ignition.frame;
            source.set_block(frame_pos.x, frame_pos.y, frame_pos.z, *frame_state);
            apply(
                conn,
                state,
                proto.encode_block_update(frame_pos.x, frame_pos.y, frame_pos.z, *frame_state),
            )
            .await?;
            if let Some(fill) = &ignition.portal_fill {
                for (cell, cell_state) in fill {
                    source.set_block(cell.x, cell.y, cell.z, *cell_state);
                    apply(
                        conn,
                        state,
                        proto.encode_block_update(cell.x, cell.y, cell.z, *cell_state),
                    )
                    .await?;
                }
            }
            // Vanilla's own item-stack shrink(1), unconditional in vanilla rather than
            // routed through `consume(1, user)` — but `consume_one`'s
            // creative no-op is still the right behaviour either way.
            if consume_one(inventory, hand_native, game_mode) && game_mode != GameMode::Creative {
                let remainder = inventory.native(hand_native).cloned();
                if let Some(menu_slot) = window_zero_menu_slot(hand_native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
    }
    // The census is the gate: it decides *whether* a placement happens at
    // all and *which* block it writes. `block_entity_for_item` only supplies
    // the live `BlockEntity` for
    // the six items this crate ticks, and is consulted second.
    let placed = held_item
        .and_then(|item| block_items::block_placed_by(item).map(|block| (item, block)));
    // Vanilla's own slab-block can-be-replaced check is the one
    // `canBeReplaced` override a hand placement can hit, and without it a slab
    // clicked onto a matching half-slab lands in the cell *above* instead of
    // doubling. Air, fluids, and tagged replaceable blocks target the clicked cell.
    let doubling_slab = placed.is_some_and(|(_, block)| slab_doubles(clicked, block, face, cursor));
    let clicked_replaceable = crate::chunk::is_air_or_fluid_id(clicked)
        || crate::block_placement::is_replaceable_for_placement(clicked);
    let target = if clicked_replaceable || doubling_slab {
        pos
    } else {
        neighbour
    };
    let target_state = source.block_state_id(target.x, target.y, target.z);
    // Every cell the placement's neighbour fan-out rewrote —
    // empty unless a placement actually happened below.
    let mut changed: Vec<(BlockPos, StateId)> = Vec::new();
    // Every cell a multi-cell attempt claimed before the atomic legality gate.
    // Re-sending these cells on rejection clears any optimistic partner half
    // the client may have shown, just as the primary and clicked cells do.
    let mut attempted_cells: Vec<BlockPos> = Vec::new();
    // Paired with the `block_update` packets in the notify loop below — see
    // `moving_piston_records`.
    let mut piston_records: Vec<(BlockPos, lodestone_core::Nbt)> = Vec::new();
    // The remainder of the held stack after a successful placement consumed one
    // from it, `None` when nothing was placed or the game mode does not consume.
    // Held out here rather than sent inside the placement block because `state` is
    // *shadowed* in there by the placed block state — see the `let (state, extra)`
    // below — so `apply` cannot be reached from inside it.
    let mut placement_remainder: Option<Option<ItemStack>> = None;
    let target_replaceable = crate::chunk::is_air_or_fluid_id(target_state)
        || crate::block_placement::is_replaceable_for_placement(target_state)
        || doubling_slab;
    if target_replaceable {
        if let Some((item, block)) = placed {
            // `placed_block_state` applies the block's own
            // `getStateForPlacement` convention (`crate::block_placement`);
            // a block with no convention keeps the census's bare default
            // state, which `resolve_state_id` resolves faithfully. Resolved
            // ahead of the block-entity registration below (moved up from
            // its original position after it) because the obstruction check
            // needs the real placed *state* — a wall-mounted variant's
            // collision box is not a free-standing one's — and nothing may be
            // registered or written until that check passes.
            let ctx = crate::block_placement::PlaceContext {
                target,
                face,
                cursor,
                yaw: player_yaw,
                pitch: player_pitch,
                sneaking,
            };
            let placement = match placed_block_state(block, &ctx, |p| {
                source.block_state_id(p.x, p.y, p.z)
            }) {
                Some(placement) => placement,
                None => crate::block_placement::Placement {
                    state: block.default_state(),
                    extra: Vec::new(),
                },
            };
            let placement = crate::block_placement::apply_waterlogging(
                placement,
                target,
                |p| source.block_state_id(p.x, p.y, p.z),
            );
            let occupied: Vec<(BlockPos, StateId)> = std::iter::once((target, placement.state))
                .chain(placement.extra.iter().copied())
                .collect();
            for (cell, _) in &occupied {
                if !attempted_cells.contains(cell) {
                    attempted_cells.push(*cell);
                }
            }
            let in_build_height = occupied.iter().all(|(p, _)| {
                source
                    .column(p.x.div_euclid(16), p.z.div_euclid(16))
                    .contains_y(p.y)
            });
            // Vanilla's own block-item can-place → level unobstructed check: a placement that
            // would collide with the placer's own body is refused, not
            // written — see `placement_obstructs_placer`'s own doc for what
            // this does and does not cover yet. `player_pos` is `None` until
            // the first movement packet arrives; skipped rather than refused
            // in that case, the same conservative-elsewhere-but-permissive-
            // here direction `is_legal_bed_respawn` documents for the same
            // gap.
            let obstructed = player_pos.is_some_and(|feet| {
                occupied
                    .iter()
                    .any(|(p, state)| placement_obstructs_placer(*p, *state, feet))
            });
            let placement_legal = in_build_height
                && crate::block_placement::validate_placement(
                    &placement,
                    target,
                    target_replaceable,
                    |p| source.block_state_id(p.x, p.y, p.z),
                )
                && !obstructed;
            if placement_legal {
                let crate::block_placement::Placement { state, extra } = placement;
                let state_id = state;
            if let Some((entity_state, mut entity)) = block_entity_for_item(item.name()) {
                // The two sources must agree on the block name, or we would
                // register a furnace at a position holding some other block.
                // `lodestone-data`'s `the_block_entity_blocks_still_resolve_
                // to_themselves` asserts they do for all six today; this
                // catches a future divergence instead of silently trusting
                // the older table.
                debug_assert_eq!(
                    entity_state.block(), block,
                    "block-entity table and item census disagree on {item:?}"
                );
                // A newly placed sign records the placing player as its editor,
                // allowing the following sign-update packet to pass validation.
                if let crate::block_entities::BlockEntity::Sign(sign) = &mut entity {
                    sign.editor = Some(placer);
                }
                block_entities.with(|registry| registry.insert(target, entity));
            } else if let Some(type_name) = lodestone_data::block_entity_types::block_entity_type(state_id)
                .map(lodestone_data::block_entity_types::block_entity_type_name)
            {
                // State-defined block entities need a registry record even when
                // the item has no specialized constructor. The client renders
                // these positions from the record, so an opaque empty payload
                // keeps the placed state visible and survives save/load handling.
                block_entities.with(|registry| {
                    registry.insert(
                        target,
                        crate::block_entities::BlockEntity::Opaque {
                            id: type_name.to_owned().into(),
                            nbt: lodestone_core::Nbt::End,
                        },
                    );
                });
            }
            source.set_block(target.x, target.y, target.z, state_id);
            // Publish the placement sound to every viewer except the placer.
            // `roll` supplies the per-click seed for choosing the sound variant.
            if let Some(effect) =
                crate::effects::block_placed(target, state_id, roll.to_bits() as i64)
            {
                block_ticks.publish_effect_except(placer, effect);
            }
            // A door's upper half, a bed's head, a chest partner's re-typing:
            // cells the placement owns but the client did not predict, so each
            // needs its own `block_update` below.
            for (p, s) in &extra {
                let extra_id = *s;
                source.set_block(p.x, p.y, p.z, extra_id);
                changed.push((*p, extra_id));
            }
            // A carved pumpkin or jack o'lantern can complete a snow- or
            // iron-golem pattern. The mob simulation reports the consumed
            // pattern cells; this caller clears them to air.
            if matches!(block, Block::CarvedPumpkin | Block::JackOLantern) {
                let construction = mobs.with(|sim| {
                    sim.try_construct_golem(
                        &|x, y, z| source.block_state_id(x, y, z),
                        (target.x, target.y, target.z),
                    )
                });
                if let Some(construction) = construction {
                    for cell in &construction.consumed {
                        let air = Block::Air.default_state();
                        source.set_block(cell.x, cell.y, cell.z, air);
                        changed.push((*cell, air));
                    }
                }
            }
            // A wither skeleton skull or wall skull can complete the
            // soul-sand-and-skull pattern. The mob simulation reports consumed
            // cells; this caller clears them to air.
            if matches!(block, Block::WitherSkeletonSkull | Block::WitherSkeletonWallSkull) {
                let construction = mobs.with(|sim| {
                    sim.try_construct_wither(
                        &|x, y, z| source.block_state_id(x, y, z),
                        (target.x, target.y, target.z),
                    )
                });
                if let Some(construction) = construction {
                    for cell in &construction.consumed {
                        let air = Block::Air.default_state();
                        source.set_block(cell.x, cell.y, cell.z, air);
                        changed.push((*cell, air));
                    }
                }
            }
            // Block placement notifies neighboring cells so redstone state can
            // react immediately. Without this fan-out, dust beside a powered
            // line stays at `power=0`.
            let mut owned_cells = Vec::with_capacity(extra.len() + 1);
            owned_cells.push(target);
            owned_cells.extend(extra.iter().map(|(p, _)| *p));
            for cell in owned_cells {
                let (mut fanout, scheduled) =
                    propagate_placement_with_entities(source, cell, Some(block_entities));
                changed.append(&mut fanout);
                piston_records.extend(moving_piston_records(&scheduled));
                // Delayed reactions are returned through the scheduled-tick queue
                // owned by the world tick loop. Publish that queue even when the
                // synchronous fan-out changed no cells.
                block_ticks.request_scheduled_ticks(scheduled);
                // A fluid at any owned cell needs the same edit notification as
                // the primary cell. The queue deduplicates repeated neighbours,
                // so this remains one logical wake-up per position.
                block_ticks.request_fluid_scheduled_ticks(crate::fluid::ticks_after_edit(
                    source,
                    fluid_env_at(source, cell),
                    cell,
                ));
            }
            // Sand and gravel schedule a gravity check two ticks out. Other
            // placed blocks produce no entry in this feed.
            //
            // The scheduled event makes a sand or gravel block fall when it is
            // placed in air. `state` is used instead of the item name because
            // `gravity_tick::is_gravity_state` matches the resolved block state.
            block_ticks.request_scheduled_ticks(crate::gravity_tick::ticks_after_place_id(target, state_id));
            // A successful placement consumes one held item. Without this update
            // **every placement would be free** — the block would be written,
            // the client would predict its own hotbar and the server would never
            // agree, so the stack would return on the next window sync.
            //
            // Creative placement consumes nothing, so the gate is explicit rather
            // than implied: survival decrements the stack and creative does not.
            //
            // `consume_one` clears the slot outright at a count of one rather than
            // leaving a zero-count stack naming an item, which renders as a block
            // you can place forever.
            if game_mode != GameMode::Creative {
                let native = hand_native;
                if consume_one(inventory, native, game_mode) {
                    placement_remainder = Some(inventory.native(native).cloned());
                }
            }
            } // !obstructed
        }
    }
    // Tell the client's window-0 hotbar slot what the server thinks is left —
    // menu slots `36..=44` map onto native `0..=8` (vanilla's inventory menu),
    // the same server-initiated slot update the composter, brewing-stand,
    // bone-meal and spawn-egg arms above send after they consume. `state_id` is
    // `0`: this crate applies a container diff verbatim and never validates a
    // stale id (`apply_container_clicked`).
    if let Some(remainder) = placement_remainder
        && let Some(menu_slot) = window_zero_menu_slot(hand_native)
    {
        apply(
            conn,
            state,
            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
        )
        .await?;
    }
    // `pos`/`neighbour` first (the clicked face and the placed cell), then every
    // cell owned by the attempted placement, then every cell the fan-out
    // actually rewrote. This includes partner cells after rejection, so a
    // client prediction cannot leave a ghost upper half behind. Deduped because
    // `target` is always one of the first two.
    let notify = placement_update_positions(pos, neighbour, &attempted_cells, &changed);
    for p in notify {
        let current = source.block_state_id(p.x, p.y, p.z);
        let directive = proto.encode_block_update(p.x, p.y, p.z, current);
        apply(conn, state, directive).await?;
        if let Some((_, nbt)) = piston_records.iter().find(|(pos, _)| *pos == p) {
            let directive =
                proto.encode_block_entity_data(p, crate::piston::PISTON_BLOCK_ENTITY, nbt);
            apply(conn, state, directive).await?;
        }
    }
    // Placing a torch has to light the column, and the `block_update` packets
    // above carry no light. Read back out of `source` rather than reusing the
    // placed state string, because the fan-out may have rewritten the cell since
    // (and because `placed_block_state`'s own result is shadowed inside the
    // placement block above). `target_state` is the cell as it was *before* the
    // placement, captured before the `set_block`.
    {
        let placed_state = source.block_state_id(target.x, target.y, target.z);
        resend_column_for_light(conn, proto, source, state, pending_relights, target_state, placed_state, target)
            .await?;
    }
    Ok(())
}

/// Return the authoritative block-update positions for one use-on attempt.
///
/// The clicked and adjacent cells are always present, while attempted partner
/// cells are included even when the atomic placement gate rejects the write.
/// Fan-out changes are appended last. Keeping this ordering in one production
/// helper makes the correction contract testable without a socket fixture.
pub(super) fn placement_update_positions(
    clicked: BlockPos,
    neighbour: BlockPos,
    attempted: &[BlockPos],
    changed: &[(BlockPos, StateId)],
) -> Vec<BlockPos> {
    let mut notify = vec![clicked, neighbour];
    for pos in attempted {
        if !notify.contains(pos) {
            notify.push(*pos);
        }
    }
    for (pos, _) in changed {
        if !notify.contains(pos) {
            notify.push(*pos);
        }
    }
    notify
}

/// Converts scheduled piston ticks into block-entity update payloads.
///
/// A `moving_piston` block update marks an animated cell; the payload from the
/// scheduled completion tick identifies the moving state. Send the payload
/// after the cell's `block_update` so the client has the matching cell record.
pub(super) fn moving_piston_records(
    scheduled: &[ScheduledTick<ScheduledTickKind>],
) -> Vec<(BlockPos, lodestone_core::Nbt)> {
    scheduled
        .iter()
        .filter(|pending| crate::piston::is_finish_kind(&pending.kind))
        .filter_map(|pending| {
            let entity = crate::piston::parse_finish_kind(&pending.kind)?;
            Some((
                BlockPos::new(pending.pos.0, pending.pos.1, pending.pos.2),
                entity.update_tag(),
            ))
        })
        .collect()
}

/// Runs the neighbor-update fan-out for a block placed at `target`, persists
/// each resulting change through `source`, and returns those changes for the
/// client update path.
///
/// The fan-out handles synchronous redstone reactions inline and returns
/// delayed reactions as scheduled ticks for the world tick loop. This keeps
/// player placement and world-tick updates on the same state-transition path.
///
/// # Delayed reactions
///
/// The local scheduled-tick queue records relative delays. Dust resolves
/// synchronously, with a measured zero-tick reaction against the live 26.2
/// oracle; torches, repeaters, comparators, and observers schedule checks two
/// or more ticks out. The world tick loop owns those delayed entries and drains
/// them from the feed.
///
/// # The delayed half travels out with the return value
///
/// The second element contains every scheduled block tick. `trigger_tick` is a
/// relative delay; the world tick loop rebases it onto its own counter after
/// [`BlockTickFeed`] receives the entries.
///
/// Publish the scheduled entries rather than invoking the fan-out a second
/// time: the first pass consumes the synchronous change, while a second pass
/// sees settled state and misses delayed reactions. A repeater measured at four
/// delay settings confirms the distinction: the inline path finishes
/// `powered=false` with output dust at `0`, while a second fan-out finishes
/// `powered=true` at `15`. The test
/// `redstone_placement_gate::the_split_between_the_synchronous_and_delayed_halves_changes_no_outcome`
/// covers this boundary.
///
/// Changes are sent to this connection through the `encode_block_update` loop;
/// the shared tick feed carries only delayed reactions.
///
/// Test helper for placement fan-out without a block-entity registry. Production
/// callers use [`propagate_placement_with_entities`] when command-block state
/// must participate in neighbor reactions.
#[cfg(test)]
pub(crate) fn propagate_placement<S>(
    source: &S,
    target: BlockPos,
) -> (Vec<(BlockPos, StateId)>, Vec<ScheduledTick<ScheduledTickKind>>)
where
    S: ChunkSource + ?Sized,
{
    propagate_placement_with_entities(source, target, None)
}

/// [`propagate_placement`], with an optional [`BlockEntityHandle`] for
/// command-block state during neighbor reactions. `None` has the same behavior
/// as [`propagate_placement`] itself.
pub(crate) fn propagate_placement_with_entities<S>(
    source: &S,
    target: BlockPos,
    block_entities: Option<&BlockEntityHandle>,
) -> (Vec<(BlockPos, StateId)>, Vec<ScheduledTick<ScheduledTickKind>>)
where
    S: ChunkSource + ?Sized,
{
    let cx = target.x.div_euclid(16);
    let cz = target.z.div_euclid(16);
    let (min_x, min_z) = (cx * 16, cz * 16);
    // Reflects the `set_block` just performed — `ChunkSource::column`'s own
    // contract is that it includes any edit already applied.
    let mut column = source.column(cx, cz);
    if target.y < column.min_y || target.y >= column.min_y + column.height {
        return (Vec::new(), Vec::new());
    }
    let mut block_ticks: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
    // `react_at_placement`, not `propagate_and_react`: the placed block owes
    // itself a `setPlacedBy` reaction that the neighbour pass structurally
    // cannot deliver. See that function's own doc comment.
    let events = crate::random_tick::react_at_placement_with_entities(
        &mut column,
        min_x,
        min_z,
        // The live world, so the placed block's own reactions and the
        // neighbour fan-out both reach an already-loaded neighbouring
        // column. `&source` rather than `source`: `S` is `?Sized` here (a
        // connection is served a type-erased `dyn ChunkSource`), and `&S`
        // is what unsizes to the `&dyn ChunkSource` this wants — see
        // `chunk`'s borrowed-source forwarding impl.
        &source,
        target.x,
        target.y,
        target.z,
        &mut block_ticks,
        // Zero, so every `trigger_tick` below *is* the delay — see the doc
        // comment above.
        0,
        block_entities,
    );
    // `drain_due`, not `iter`: this queue is a `BinaryHeap` and `iter` yields in
    // unspecified order, while `drain_due` yields `DRAIN_ORDER`. The loop
    // re-`schedule`s each entry and so assigns it a fresh `sub_tick_order`, which
    // makes *this* order the one that decides tie-breaks later — so it has to be
    // deterministic. `u64::MAX` drains everything regardless of delay.
    let scheduled: Vec<ScheduledTick<ScheduledTickKind>> =
        block_ticks.drain_due(u64::MAX, usize::MAX);
    let changed = events
        .into_iter()
        .map(|event| {
            let (ex, ey, ez) = event.pos;
            source.set_block(ex, ey, ez, event.to);
            (BlockPos::new(ex, ey, ez), event.to)
        })
        .collect();
    (changed, scheduled)
}

/// Vanilla's own tripwire-block affect-neighbors-after-removal routine's bridge from a [`ChunkSource`]
/// to [`crate::random_tick::react_at_removal`] — the block-**removal** twin
/// of [`propagate_placement_with_entities`], same column-snapshot shape.
/// `wire_state_before_removal` is the removed block's own state just before
/// the caller overwrote the cell; anything other than a tripwire is a fast
/// no-op via `react_at_removal`'s own guard, so a caller may call this
/// unconditionally on every break.
pub(crate) fn propagate_removal_with_entities<S>(
    source: &S,
    target: BlockPos,
    wire_state_before_removal: StateId,
) -> (Vec<(BlockPos, StateId)>, Vec<ScheduledTick<ScheduledTickKind>>)
where
    S: ChunkSource + ?Sized,
{
    let cx = target.x.div_euclid(16);
    let cz = target.z.div_euclid(16);
    let (min_x, min_z) = (cx * 16, cz * 16);
    // Reflects the removal already applied — same contract
    // `propagate_placement_with_entities` relies on for its own placement.
    let mut column = source.column(cx, cz);
    if target.y < column.min_y || target.y >= column.min_y + column.height {
        return (Vec::new(), Vec::new());
    }
    let mut block_ticks: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
    let events = crate::random_tick::react_at_removal(
        &mut column,
        min_x,
        min_z,
        // Same live world, same reason as `propagate_placement_with_entities`:
        // a tripwire's controlling hook is up to 41 cells away, so it is
        // usually not in the column holding the cell that was broken.
        &source,
        target.x,
        target.y,
        target.z,
        wire_state_before_removal,
        &mut block_ticks,
        0,
    );
    let scheduled: Vec<ScheduledTick<ScheduledTickKind>> =
        block_ticks.drain_due(u64::MAX, usize::MAX);
    let changed = events
        .into_iter()
        .map(|event| {
            let (ex, ey, ez) = event.pos;
            source.set_block(ex, ey, ez, event.to);
            (BlockPos::new(ex, ey, ez), event.to)
        })
        .collect();
    (changed, scheduled)
}

#[cfg(test)]
mod tests;
