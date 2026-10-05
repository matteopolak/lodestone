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
/// choice of placement cell (`BlockPlaceContext`'s constructor: place at the
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
