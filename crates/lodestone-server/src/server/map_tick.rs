//! The per-tick map duty of one connection: tick the maps the player carries and send what they are owed.

use super::*;

/// Attaches the world's map folder (once, from the first connection that has one) and starts this
/// connection's map bookkeeping.
pub(super) fn start_map_session<S: ChunkSource + ?Sized>(
    world: &crate::world_state::WorldStateHandle,
    source: &S,
    player_uuid: uuid::Uuid,
    username: &str,
) -> crate::maps::MapSession {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(store) = player_store(source) {
        world.maps().attach_directory(store.world_dir().join("data").join("minecraft").join("maps"));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = source;
    crate::maps::MapSession::new(world.maps().clone(), player_uuid, username)
}

/// Ticks every filled map in `inventory`, then writes the packets this player is owed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn tick_maps<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    session: &mut crate::maps::MapSession,
    world: &crate::world_state::WorldStateHandle,
    source: &S,
    dimension: crate::dimension::Dimension,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    inventory: &PlayerInventory,
    mobs: &MobHandle,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let Some((x, _, z)) = player_pos else { return Ok(()) };
    let pose = crate::maps::PlayerPose {
        x,
        z,
        yaw: player_rot.map_or(0.0, |rotation| rotation.yaw),
        dimension,
        game_time: world.time().game_time,
    };
    let frames = mobs.with(|sim| sim.framed_maps());
    for update in session.tick(pose, inventory, source, &frames) {
        apply(conn, state, proto.encode_map_item_data(&update)).await?;
    }
    Ok(())
}

/// Uses the empty map in native slot `native`: one is consumed (none in creative), the filled map it
/// becomes takes its place when the stack is gone and is otherwise added to the inventory or dropped.
/// Returns the slots that changed.
#[allow(clippy::too_many_arguments)]
pub(super) fn use_empty_map(
    world: &crate::world_state::WorldStateHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    inventory: &mut PlayerInventory,
    native: usize,
    game_mode: GameMode,
    feet: Vec3,
    player_rot: Option<Rotation>,
    drops_rng: &mut SpawnRng,
    dimension: crate::dimension::Dimension,
) -> Vec<usize> {
    let map_id = world.maps().create_for_use(feet.x.floor() as i32, feet.z.floor() as i32, dimension);
    let mut map = ItemStack::new("minecraft:filled_map".parse().expect("valid item key"), 1);
    map.components.map_id = Some(map_id);
    block_ticks.publish_effect(crate::effects::WorldEffect::Sound {
        sound: "minecraft:ui.cartography_table.take_result".to_owned(),
        category: lodestone_model::SoundCategory::Player,
        pos: feet,
        volume: 1.0,
        pitch: 1.0,
        seed: i64::from(map_id),
    });
    let mut touched = vec![native];
    consume_one(inventory, native, game_mode);
    if inventory.native(native).is_none() {
        inventory.set_native(native, Some(map));
        return touched;
    }
    let (written, leftover) = inventory.add(map);
    touched.extend(written);
    if let Some(leftover) = leftover {
        spawn_dropped_stacks(mobs, Some((feet.x, feet.y, feet.z)), player_rot, drops_rng, vec![leftover]);
    }
    touched
}

/// Handles a block click made with a filled map: a banner under the click is marked on, or removed
/// from, the map. Returns whether the click was consumed (the clicked block is a banner), so the
/// caller skips its ordinary block use.
pub(super) fn filled_map_banner_click<S: ChunkSource + ?Sized>(
    world: &crate::world_state::WorldStateHandle,
    source: &S,
    inventory: &PlayerInventory,
    native: usize,
    pos: BlockPos,
) -> bool {
    let Some(map_id) = inventory.native(native).and_then(crate::maps::filled_map_id) else { return false };
    let Some(banner) = crate::maps::banner_at(source.block_state_id(pos.x, pos.y, pos.z), pos) else {
        return false;
    };
    world.maps().with(|store| {
        if let Some(map) = store.get(map_id) {
            map.toggle_banner(Some(banner), pos);
        }
    });
    true
}
