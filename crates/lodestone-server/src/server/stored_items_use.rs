//! Using a shulker box, jukebox, chiseled bookshelf or decorated pot: the packet, inventory and effect half of [`crate::stored_items`].

use super::*;
use crate::effects::WorldEffect;
use crate::stored_items::{self as rules, Wobble};
use lodestone_data::block_properties::PropertyKey;
use lodestone_core::Nbt;
use lodestone_model::{BlockFace, ItemStack, Vec3, Vec3f};

/// What a click on one of these blocks resolved to.
enum Target {
    Shulker,
    Jukebox,
    Bookshelf,
    Pot,
}

fn target_of(block: Block) -> Option<Target> {
    match block {
        Block::Jukebox => Some(Target::Jukebox),
        Block::ChiseledBookshelf => Some(Target::Bookshelf),
        Block::DecoratedPot => Some(Target::Pot),
        block if crate::block_entities::is_shulker_box(block) => Some(Target::Shulker),
        _ => None,
    }
}

fn centre(pos: BlockPos) -> Vec3 {
    Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y) + 0.5, f64::from(pos.z) + 0.5)
}

/// Runs `edit` on the Opaque save compound at `pos`, creating the entity first
/// when the cell has none (terrain placed it, or an older save lacked it).
fn with_compound<R>(
    block_entities: &BlockEntityHandle,
    pos: BlockPos,
    id: &str,
    edit: impl FnOnce(&mut Nbt) -> R,
) -> Option<R> {
    block_entities.with(|reg| {
        if reg.get(pos).is_none() {
            reg.insert(pos, rules::fresh(id, pos, None)?);
        }
        match reg.get_mut(pos)? {
            BlockEntity::Opaque { nbt, .. } => Some(edit(nbt)),
            _ => None,
        }
    })
}

/// Pops `stack` out of the cell at `pos` like a dropped block resource.
fn pop(mobs: &MobHandle, stack: &ItemStack, pos: BlockPos, rng: &mut SpawnRng) {
    let (position, velocity) = crate::block_drops::pop_resource_placement(pos, rng);
    let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
    mobs.with(|sim| sim.spawn_item(stack, position, velocity, ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE)));
}

async fn sync_hand<T: Transport, P: ServerProtocol>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    inventory: &PlayerInventory,
    natives: &[usize],
) -> Result<(), ServerError> {
    for &native in natives {
        if let Some(menu_slot) = window_zero_menu_slot(native) {
            apply(conn, state, proto.encode_container_slot(0, 0, menu_slot, inventory.native(native))).await?;
        }
    }
    Ok(())
}

async fn write_state<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    block_ticks: &BlockTickFeed,
    pos: BlockPos,
    old: StateId,
    new: StateId,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    if old == new {
        return Ok(());
    }
    source.set_block(pos.x, pos.y, pos.z, new);
    apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, new)).await?;
    block_ticks.publish_change(pos.x, pos.y, pos.z, old, new);
    Ok(())
}

/// Handles a use-item-on click on a stored-item block. `Ok(false)` means the
/// click is not this module's and falls through to the ordinary
/// interactions and placement.
pub(super) async fn apply_stored_item_use<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pos: BlockPos,
    face: BlockFace,
    cursor: Vec3f,
    sneaking: bool,
    player: uuid::Uuid,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    hand: u8,
    game_mode: GameMode,
    rng: &mut SpawnRng,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
) -> Result<bool, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let clicked = source.block_state_id(pos.x, pos.y, pos.z);
    let Some(target) = target_of(clicked.block()) else { return Ok(false) };
    // Secondary use with anything in either hand skips the block's own use.
    let usable_hand = |native: usize| inventory.native(native).is_some();
    if sneaking && (usable_hand(usize::from(inventory.selected_hotbar_slot())) || usable_hand(OFFHAND_NATIVE)) {
        return Ok(false);
    }
    let hand_native = if hand == 1 { OFFHAND_NATIVE } else { usize::from(inventory.selected_hotbar_slot()) };
    let held = inventory.native(hand_native).cloned();
    match target {
        Target::Shulker => {
            let facing = crate::redstone::property_direction(clicked, PropertyKey::Facing);
            let already_open = block_entities.with(|reg| reg.lid_viewers.is_open(pos));
            // The lid needs the cell it swings into to be clear.
            let blocked = !already_open
                && facing.is_some_and(|facing| {
                    let at = facing.relative(pos);
                    let lid = source.block_state_id(at.x, at.y, at.z);
                    !(crate::chunk::is_air_or_fluid_id(lid) || crate::block_placement::is_replaceable_for_placement(lid))
                });
            if blocked {
                return Ok(true);
            }
            let changes = block_entities.with(|reg| reg.lid_viewers.open(player, pos, clicked.block().name()));
            for effect in changes.iter().flat_map(crate::lid_viewers::LidChange::effects) {
                block_ticks.publish_effect(effect);
            }
            open_container_screen(
                conn, proto, state, block_entities, inventory, pos, "minecraft:shulker_box",
                next_window_id, open_container, container_sync,
            )
            .await?;
            Ok(true)
        }
        Target::Jukebox => {
            let has_record = crate::redstone::property_bool(clicked, PropertyKey::HasRecord) == Some(true);
            if has_record {
                let disc = with_compound(block_entities, pos, rules::JUKEBOX_ID, |nbt| {
                    let disc = rules::jukebox_disc(nbt);
                    rules::set_jukebox_disc(nbt, None);
                    disc
                })
                .flatten();
                let new = rules::with_property(clicked, "has_record", "false").unwrap_or(clicked);
                write_state(conn, proto, source, state, block_ticks, pos, clicked, new).await?;
                block_ticks.publish_effect(WorldEffect::LevelEvent {
                    event: rules::JUKEBOX_STOP_EVENT,
                    pos,
                    data: 0,
                    global: false,
                });
                if let Some(disc) = disc {
                    let (jitter_x, jitter_z) = (f64::from(rng.next_f32() - 0.5) * 0.7, f64::from(rng.next_f32() - 0.5) * 0.7);
                    let at = Vec3::new(f64::from(pos.x) + 0.5 + jitter_x, f64::from(pos.y) + 1.01, f64::from(pos.z) + 0.5 + jitter_z);
                    let velocity = crate::block_drops::dropped_item_velocity(rng);
                    mobs.with(|sim| sim.spawn_item(&disc, at, velocity, ItemLifecycle::newly_dropped(1, DEFAULT_MAX_STACK_SIZE)));
                }
                return Ok(true);
            }
            let Some((song, _)) = held.as_ref().and_then(rules::song_of) else { return Ok(false) };
            let disc = held.expect("a disc was matched above");
            with_compound(block_entities, pos, rules::JUKEBOX_ID, |nbt| rules::set_jukebox_disc(nbt, Some(&disc)));
            let new = rules::with_property(clicked, "has_record", "true").unwrap_or(clicked);
            write_state(conn, proto, source, state, block_ticks, pos, clicked, new).await?;
            block_ticks.publish_effect(WorldEffect::LevelEvent {
                event: rules::JUKEBOX_PLAY_EVENT,
                pos,
                data: song,
                global: false,
            });
            if consume_one(inventory, hand_native, game_mode) {
                sync_hand(conn, proto, state, inventory, &[hand_native]).await?;
            }
            Ok(true)
        }
        Target::Bookshelf => {
            let facing = crate::redstone::property_direction(clicked, PropertyKey::Facing);
            let slot = facing.and_then(|facing| {
                rules::hit_slot(facing, crate::block_placement::face_direction(face), (cursor.x, cursor.y, cursor.z))
            });
            let Some(slot) = slot else { return Ok(false) };
            let occupied = with_compound(block_entities, pos, rules::BOOKSHELF_ID, |nbt| {
                rules::bookshelf_items(nbt)[slot].is_some()
            })
            .unwrap_or(false);
            let book = held.as_ref().filter(|stack| rules::is_shelvable_book(&stack.item.to_string()));
            let (sound, taken) = if let (false, Some(book)) = (occupied, book) {
                let mut one = book.clone();
                one.count = 1;
                let enchanted = one.item.to_string() == "minecraft:enchanted_book";
                with_compound(block_entities, pos, rules::BOOKSHELF_ID, |nbt| rules::set_bookshelf_slot(nbt, slot, Some(one)));
                if consume_one(inventory, hand_native, game_mode) {
                    sync_hand(conn, proto, state, inventory, &[hand_native]).await?;
                }
                (if enchanted { "insert.enchanted" } else { "insert" }, None)
            } else if occupied {
                let taken = with_compound(block_entities, pos, rules::BOOKSHELF_ID, |nbt| {
                    let taken = rules::bookshelf_items(nbt)[slot].clone();
                    rules::set_bookshelf_slot(nbt, slot, None);
                    taken
                })
                .flatten();
                let enchanted = taken.as_ref().is_some_and(|stack| stack.item.to_string() == "minecraft:enchanted_book");
                (if enchanted { "pickup.enchanted" } else { "pickup" }, taken)
            } else {
                // An empty slot with nothing shelvable in hand: the click is
                // spent, and no block is placed against the face.
                return Ok(true);
            };
            let items = with_compound(block_entities, pos, rules::BOOKSHELF_ID, |nbt| rules::bookshelf_items(nbt)).unwrap_or_default();
            if let Some(new) = rules::bookshelf_state(clicked, &items) {
                write_state(conn, proto, source, state, block_ticks, pos, clicked, new).await?;
            }
            if let Some(effect) = crate::effects::block_sound(&format!("minecraft:block.chiseled_bookshelf.{sound}"), pos, 1.0, 1.0, 0) {
                block_ticks.publish_effect(effect);
            }
            if let Some(book) = taken {
                let (written, leftover) = inventory.add(book);
                sync_hand(conn, proto, state, inventory, &written).await?;
                if let Some(leftover) = leftover {
                    pop(mobs, &leftover, pos, rng);
                }
            }
            Ok(true)
        }
        Target::Pot => {
            let current = with_compound(block_entities, pos, rules::POT_ID, |nbt| rules::pot_item(nbt)).flatten();
            let insert = held.as_ref().and_then(|held| rules::pot_insert(current.as_ref(), held));
            let block = clicked.block().name();
            let wobble = |style: Wobble| WorldEffect::BlockEvent { pos, block: block.to_owned(), kind: 1, value: style as u8 };
            if let Some((next, fill)) = insert {
                with_compound(block_entities, pos, rules::POT_ID, |nbt| rules::set_pot_item(nbt, Some(&next)));
                block_ticks.publish_effect(wobble(Wobble::Positive));
                if consume_one(inventory, hand_native, game_mode) {
                    sync_hand(conn, proto, state, inventory, &[hand_native]).await?;
                }
                if let Some(effect) = crate::effects::block_sound("minecraft:block.decorated_pot.insert", pos, 1.0, 0.7 + 0.5 * fill, 0) {
                    block_ticks.publish_effect(effect);
                }
                let above = centre(pos);
                block_ticks.publish_effect(WorldEffect::Particles {
                    particle: "minecraft:dust_plume".to_owned(),
                    pos: Vec3::new(above.x, f64::from(pos.y) + 1.2, above.z),
                    offset: Vec3f { x: 0.0, y: 0.0, z: 0.0 },
                    max_speed: 0.0,
                    count: 7,
                    long_distance: false,
                });
            } else {
                if let Some(effect) = crate::effects::block_sound("minecraft:block.decorated_pot.insert_fail", pos, 1.0, 1.0, 0) {
                    block_ticks.publish_effect(effect);
                }
                block_ticks.publish_effect(wobble(Wobble::Negative));
            }
            Ok(true)
        }
    }
}
