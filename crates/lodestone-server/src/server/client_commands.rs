//! Small client-initiated commands: difficulty and game-rule changes, client status commands, the carried-item slot and creative slot sets.

use super::*;

/// Per-connection difficulty and game-rule session state.
///
/// The world handle stores the shared difficulty and rule values; packet
/// handlers validate requests there and send confirmations through the
/// connection's protocol. Permission checks occur at packet dispatch, while
/// this helper only reads or writes the accepted world state.
/// Applies a difficulty-change request (`ServerBound::DifficultyChanged`).
/// The dispatch layer has already applied the permission gate. This helper
/// reads the shared difficulty and lock state and confirms it to this client.
pub(super) async fn apply_difficulty_change<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    world: &crate::world_state::WorldStateHandle,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let (difficulty, locked) = world.difficulty();
    let directive = proto.encode_change_difficulty(difficulty, locked);
    apply(conn, state, directive).await
}

/// Applies a game-rule change request (`ServerBound::GameRuleChanged`).
/// Permission filtering occurs in packet dispatch, so an empty `entries` list
/// produces an empty confirmation. Each key and value is parsed by
/// [`crate::world_state::WorldStateHandle::set_rule`]; unknown keys and invalid
/// values are omitted rather than stored verbatim.
pub(super) async fn apply_game_rule_changed<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    world: &crate::world_state::WorldStateHandle,
    entries: Vec<(String, String)>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    // Confirm only entries accepted by the world-rule parser, so a rejected key
    // is visibly absent from the reply rather than silently acknowledged.
    let accepted: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(key, value)| {
            world
                .set_rule(key, value)
                .ok()
                .map(|parsed| (key.clone(), parsed.serialize()))
        })
        .collect();
    let directive = proto.encode_game_rule_values(&accepted);
    apply(conn, state, directive).await
}

/// Applies a `client_command` request (`ServerBound::ClientCommand`) for the
/// actions modeled by this server.
///
/// # `action == 1`, `REQUEST_STATS`
///
/// The statistics reply comes from [`AdvancementManager::stats_snapshot`] and
/// is encoded by [`ServerProtocol::encode_award_stats`]. Protocols without a
/// statistics encoder send no frame.
///
/// # `action == 0`, `PERFORM_RESPAWN`
///
/// **The respawn position is the player's bed or charged respawn anchor when it
/// remains usable**, and the world spawn otherwise. [`crate::respawn_anchor::resolve`]
/// re-reads the block at death time, so a broken, uncharged or obstructed one
/// falls back to the world spawn, clears the point and tells the player. An
/// anchor spends one charge and plays its depletion sound to this player only.
/// An anchor respawn in a dimension the connection is already viewing keeps the
/// connection there ([`DimensionReset::in_place`]).
///
/// Respawn resets the modeled player vitals and burn state, sends the
/// authoritative position, and refreshes the health and air displays. A request
/// from a living player is ignored.
///
/// # `action == 2`, `REQUEST_GAMERULE_VALUES`
///
/// Action `2` returns the accepted rule entries when the permission level allows
/// it. Rules that have not been set are absent from the reply.
#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_client_command<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    vitals: &mut PlayerVitals,
    burn: &mut crate::burning::BurnState,
    // The fall accumulator, reset whenever respawn changes the player's
    // position.
    fall: &mut FallTracker,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    // The world spawn resolved during the join sequence. It is the fallback
    // when no usable per-player bed position exists.
    //
    // The fallback for a missing or unusable per-player bed position.
    world_spawn: Vec3,
    // This player's bed or anchor point, if they have set one. Resolved against
    // `source` rather than used directly: see this function's own doc comment for
    // why the block is re-read at death time. `&mut` because an unusable point is
    // cleared.
    respawn: &mut Option<RespawnPoint>,
    // The dimension the connection is viewing now, and the one it joined in. The
    // respawn point is re-read in whichever dimension it names.
    source: &dyn ChunkSource,
    home: &dyn ChunkSource,
    world: &crate::world_state::WorldStateHandle,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    action: i32,
    // Permission level for the rule-values request and mutation requests.
    permission_level: u8,
    // The readiness marker must be received again after a respawn before
    // movement-dependent simulation resumes.
    client_loaded: &mut bool,
    // Set when the respawn lands outside the view the connection already has;
    // otherwise remains `None`.
    dimension_reset: &mut Option<connection_travel::DimensionReset>,
    // Records the perform-respawn that answers an End-exit win announcement.
    end_exit: &mut connection_travel::EndExit,
    // The dimension change a respawn that stays in a non-home dimension sends.
    game_mode: GameMode,
    // Where a spent anchor charge is published, and the feed its block change
    // reaches other viewers through.
    block_ticks: &BlockTickFeed,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    match action {
        // The credits were dismissed (or skipped): the connection loop moves
        // the player home, keeping everything they carry.
        0 if end_exit.is_won() => end_exit.request_respawn(),
        0 if vitals.health() <= 0.0 => {
            vitals.respawn();
            burn.reset();
            *client_loaded = !proto.sends_player_loaded();
            // The respawn point is re-read in its own dimension; a missing one
            // falls back to the world spawn. Everything the client needs to
            // follow the player there is sent before health and air so the HUD
            // refreshes for the updated state.
            if let Some(reset) = connection_travel::perform_respawn(
                conn,
                proto,
                state,
                home,
                source,
                respawn,
                world_spawn,
                game_mode,
                teleport_acknowledgements,
                block_ticks,
                false,
            )
            .await?
            {
                *dimension_reset = Some(reset);
            }
            apply(
                conn,
                state,
                proto.encode_set_health(
                    vitals.health(),
                    vitals.food().food_level(),
                    vitals.food().saturation(),
                ),
            )
            .await?;
            apply(conn, state, proto.encode_air_supply_update(vitals.air_supply())).await?;
            // The teleport above is a position snap, so the next `PlayerMoved`
            // sample must not be diffed against the y the player died at — a
            // death at y=70 respawning at y=64 would otherwise bank 6 blocks of
            // phantom fall distance against the next landing.
            fall.reset();
        }
        1 => {
            let snapshot = advancements.stats_snapshot(player_uuid);
            apply(conn, state, proto.encode_award_stats(&snapshot)).await?;
        }
        2 => {
            // A denied request produces no response; an allowed request returns
            // the accepted rule entries.
            if permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                apply(
                    conn,
                    state,
                    proto.encode_game_rule_values(&world.rule_entries()),
                )
                .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Applies a `SET_CARRIED_ITEM` request (`ServerBound::CarriedItemChanged`),
/// mirroring vanilla's own carried-item-set handler, which
/// writes straight into its own selected-slot setter and sends **no**
/// confirmation packet back — see that `ServerBound` variant's own doc
/// comment. A no-op if `slot` is already out of range (the protocol decoder
/// only ever constructs this variant with a validated slot, so this guard is
/// a second, defensive layer rather than the primary one — see
/// `PlayerInventory::set_selected_hotbar_slot`'s own doc comment for why it
/// degrades instead of panicking).
pub(super) fn apply_carried_item_changed(inventory: &mut PlayerInventory, slot: u8) {
    if let Some(slot) = HotbarSlot::new(slot) {
        inventory.select_hotbar_slot(slot);
    }
}

/// Applies a `SET_CREATIVE_MODE_SLOT` write (`ServerBound::CreativeModeSlotSet`).
/// The wire slot uses the same numbering as [`PlayerInventory::apply_menu_slot_change`];
/// unsupported and negative values are ignored. Only creative players may use
/// this packet, because it can write arbitrary inventory contents.
pub(super) fn apply_creative_mode_slot_set(
    inventory: &mut PlayerInventory,
    slot: i16,
    item: Option<ItemStack>,
    creative: bool,
) {
    if !creative {
        return;
    }
    if let Some(slot) = MenuSlot::from_raw(i32::from(slot)) {
        inventory.apply_menu_slot(slot, item);
    }
}

/// Reads one menu's slots in menu order, from whichever backing stores its
/// [`MenuLayout`] names.
///
/// `own` is the open block entity's own slots (empty for a menu with none), and
/// `grid` is the [`CraftingState`] behind the `Grid`/`Result` kinds.
pub(super) fn read_menu(
    layout: &MenuLayout,
    inventory: &PlayerInventory,
    grid: Option<&CraftingState>,
    own: &[Option<ItemStack>],
) -> Vec<Option<ItemStack>> {
    layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Container(index) => own.get(index).cloned().flatten(),
            SlotKind::Grid(cell) => grid.and_then(|g| g.input(cell).cloned()),
            SlotKind::Result => grid.and_then(|g| g.result().cloned()),
        })
        .collect()
}
