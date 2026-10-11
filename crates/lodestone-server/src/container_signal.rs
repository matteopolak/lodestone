//! The analog output a comparator reads from a block entity.
//!
//! # What it is
//!
//! The comparator-side half of every block entity that has an output: how full
//! a container is, which book page a lectern shows, the disc a jukebox holds.
//! [`signal`] answers for one block; [`carries_analog`] is the cheap "does this
//! entity have an output at all" test the registry's change tracking uses.
//!
//! # How it works
//!
//! A *fill* container (chest, barrel, hopper, furnaces, brewing stand, shulker
//! box, decorated pot, dispenser, dropper) sums `count / min(99, item stack
//! limit)` over its slots in `f32`, divides by the slot count, then maps the
//! fraction `t` to `floor(t * 14) + (t > 0)`: any item at all reads at least 1,
//! and a full container reads 15. Other kinds read their own rule: a crafter
//! counts slots holding an item or disabled, a lectern is `floor(page progress
//! * 14) + 1`, a jukebox reads its disc's own value, a chiseled bookshelf the
//! last-touched slot plus one, a command block its last success count.
//!
//! A chest is the one kind that reads other cells: a double chest is one
//! 54-slot inventory, and a chest with a conductor above it (either half)
//! reads 0 because it cannot be opened.
//!
//! Containers this crate keeps as raw save data ([`BlockEntity::Opaque`]: a
//! decorated pot, shulker box, jukebox, chiseled bookshelf, shelf) are read
//! from that data with the same rules, so a loaded world's pot reads as the
//! original did.
//!
//! # How to change it
//!
//! A new block entity with an output needs three edits: [`has_analog_output`]
//! (the block), [`carries_analog`] (the entity, so the registry marks it dirty)
//! and an arm in [`entity_signal`]. Its mutators must go through
//! [`BlockEntityRegistry::get_mut`](crate::block_entities::BlockEntityRegistry::get_mut)
//! or `insert`; those mark the position dirty, and the tick loop turns each
//! dirty position into a comparator update with no per-tick scan.
//!
//! # Dependencies
//!
//! `crate::block_entities` for the data, `crate::redstone` for the property
//! readers, `lodestone_data::redstone_conductor` for the real conductor table.

use lodestone_core::Nbt;
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, PropertyKey};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack};

use crate::block_entities::{BlockEntity, BlockEntityRegistry};
use crate::neighbor_update::Direction;
use crate::redstone::{base_name, get_str_property, property_direction};

/// The per-slot cap a container applies on top of the item's own stack limit.
const CONTAINER_STACK_CAP: u32 = 99;

/// The block-entity ids kept as raw save data that still have an output.
const OPAQUE_ANALOG_IDS: &[&str] = &[
    "minecraft:decorated_pot",
    "minecraft:shulker_box",
    "minecraft:jukebox",
    "minecraft:chiseled_bookshelf",
    "minecraft:shelf",
];

/// Each music disc's comparator value, from the real song definitions; the
/// disc item is `music_disc_<song>`.
pub(crate) const DISC_SIGNALS: &[(&str, u8)] = &[
    ("11", 11),
    ("13", 1),
    ("5", 15),
    ("blocks", 3),
    ("bounce", 8),
    ("cat", 2),
    ("chirp", 4),
    ("creator", 12),
    ("creator_music_box", 11),
    ("far", 5),
    ("lava_chicken", 9),
    ("mall", 6),
    ("mellohi", 7),
    ("otherside", 14),
    ("pigstep", 13),
    ("precipice", 13),
    ("relic", 14),
    ("stal", 8),
    ("strad", 9),
    ("tears", 10),
    ("wait", 12),
    ("ward", 10),
];

/// Whether `block` reports an analog output to a comparator at all. This is a
/// property of the block, true even while the container is empty or its entity
/// is missing, and it decides whether a comparator takes the block's reading
/// (possibly 0) over the plain diode input.
#[must_use]
pub(crate) fn has_analog_output(block: Block) -> bool {
    matches!(
        block,
        Block::Chest
            | Block::TrappedChest
            | Block::Barrel
            | Block::Hopper
            | Block::Dispenser
            | Block::Dropper
            | Block::Furnace
            | Block::Smoker
            | Block::BlastFurnace
            | Block::BrewingStand
            | Block::Crafter
            | Block::ChiseledBookshelf
            | Block::Lectern
            | Block::Jukebox
            | Block::DecoratedPot
            | Block::CommandBlock
            | Block::ChainCommandBlock
            | Block::RepeatingCommandBlock
    ) || is_shulker_box(block)
        || is_shelf(block)
}

fn is_shulker_box(block: Block) -> bool {
    block.name().ends_with("shulker_box")
}

fn is_shelf(block: Block) -> bool {
    matches!(
        block,
        Block::AcaciaShelf
            | Block::BambooShelf
            | Block::BirchShelf
            | Block::CherryShelf
            | Block::CrimsonShelf
            | Block::DarkOakShelf
            | Block::JungleShelf
            | Block::MangroveShelf
            | Block::OakShelf
            | Block::PaleOakShelf
            | Block::SpruceShelf
            | Block::WarpedShelf
    )
}

/// Whether this entity has a comparator output, so a mutation of it must wake
/// the comparators around it.
#[must_use]
pub(crate) fn carries_analog(entity: &BlockEntity) -> bool {
    match entity {
        BlockEntity::Container { .. }
        | BlockEntity::Hopper(_)
        | BlockEntity::Furnace(_)
        | BlockEntity::BrewingStand(_)
        | BlockEntity::Crafter { .. }
        | BlockEntity::Lectern(_)
        | BlockEntity::CommandBlock(_) => true,
        BlockEntity::Opaque { id, .. } => OPAQUE_ANALOG_IDS.contains(&id.name()),
        _ => false,
    }
}

/// The part of the output a ticking entity can change on its own: only the
/// furnaces and the brewing stand consume or produce items as they run.
#[must_use]
pub(crate) fn ticking_signal(entity: &BlockEntity) -> Option<u8> {
    matches!(entity, BlockEntity::Furnace(_) | BlockEntity::BrewingStand(_))
        .then(|| entity_signal(entity))
        .flatten()
}

/// `floor(t * 14) + (t > 0)`: the fraction of a container filled, mapped onto
/// `0..=15` so that any item reads 1 and a full container reads 15.
fn fill_level(total: f32) -> u8 {
    let level = (total * 14.0).floor() as i64 + i64::from(total > 0.0);
    level.clamp(0, 15) as u8
}

fn stack_limit(item: &str) -> u32 {
    lodestone_data::item_prototypes::prototype(item).map_or(64, |p| u32::from(p.max_stack_size)).max(1)
}

fn stack_share(stack: &ItemStack) -> f32 {
    let limit = stack
        .components
        .max_stack_size
        .map_or_else(|| stack_limit(&stack.item.to_string()), |size| size.max(1));
    stack.count as f32 / CONTAINER_STACK_CAP.min(limit) as f32
}

/// The fill level of a container of `size` slots holding `slots`.
fn fill<'a>(slots: impl IntoIterator<Item = &'a Option<ItemStack>>, size: usize) -> u8 {
    if size == 0 {
        return 0;
    }
    let total: f32 = slots.into_iter().flatten().filter(|stack| stack.count > 0).map(stack_share).sum();
    fill_level(total / size as f32)
}

/// What a lectern reads: the page reached, spread over `0..=14`, plus one for
/// holding a book at all.
fn lectern_signal(lectern: &crate::block_entities::LecternData) -> u8 {
    let Some(book) = &lectern.book else { return 0 };
    let pages = book
        .components
        .written_book_content
        .as_ref()
        .map(|content| content.pages.len())
        .or_else(|| book.components.writable_book_content.as_ref().map(Vec::len))
        .unwrap_or(0);
    let progress = if pages > 1 { lectern.page.max(0) as f32 / (pages as f32 - 1.0) } else { 1.0 };
    ((progress * 14.0).floor() as i64 + 1).clamp(0, 15) as u8
}

fn brewing_signal(stand: &crate::brewing::BrewingStand) -> u8 {
    // A potion bottle stacks to one.
    let bottles = (0..3).filter(|&slot| stand.bottle(slot).is_some()).count() as f32;
    let share = |held: Option<(&str, u32)>| {
        held.filter(|(_, count)| *count > 0)
            .map_or(0.0, |(item, count)| count as f32 / CONTAINER_STACK_CAP.min(stack_limit(item)) as f32)
    };
    fill_level((bottles + share(stand.ingredient()) + share(stand.fuel_item())) / 5.0)
}

fn nbt_field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
        _ => None,
    }
}

fn nbt_int(nbt: &Nbt, key: &str) -> Option<i32> {
    match nbt_field(nbt, key)? {
        Nbt::Int(v) => Some(*v),
        Nbt::Short(v) => Some(i32::from(*v)),
        Nbt::Byte(v) => Some(i32::from(*v)),
        _ => None,
    }
}

fn opaque_signal(id: &str, nbt: &Nbt, side: Direction, facing: Option<Direction>) -> Option<u8> {
    Some(match id {
        "minecraft:decorated_pot" => {
            let stack = nbt_field(nbt, "item").and_then(crate::item_nbt::stack_from_nbt);
            fill([&stack], 1)
        }
        "minecraft:shulker_box" => fill(&crate::chunk_nbt::items_from_nbt(nbt_field(nbt, "Items"), 27), 27),
        "minecraft:chiseled_bookshelf" => (nbt_int(nbt, "last_interacted_slot").unwrap_or(-1) + 1).clamp(0, 15) as u8,
        "minecraft:jukebox" => nbt_field(nbt, "RecordItem")
            .and_then(crate::item_nbt::stack_from_nbt)
            .and_then(|disc| {
                let item = disc.item.to_string();
                let song = item.strip_prefix("minecraft:music_disc_")?;
                DISC_SIGNALS.iter().find(|(name, _)| *name == song).map(|&(_, signal)| signal)
            })
            .unwrap_or(0),
        // A shelf reports only to a comparator on the face it points away
        // from, as the three shelf slots read in binary.
        "minecraft:shelf" => {
            if facing.map(Direction::opposite) != Some(side) {
                return Some(0);
            }
            let items = crate::chunk_nbt::items_from_nbt(nbt_field(nbt, "Items"), 3);
            items.iter().enumerate().map(|(slot, held)| u8::from(held.is_some()) << slot).sum()
        }
        _ => return None,
    })
}

/// The output of `entity` on its own, or `None` when it has none. A double
/// chest and a shelf's facing need the world and are handled by [`signal`].
#[must_use]
pub(crate) fn entity_signal(entity: &BlockEntity) -> Option<u8> {
    match entity {
        BlockEntity::Container { slots, .. } => Some(fill(slots, slots.len())),
        BlockEntity::Hopper(hopper) => Some(fill(hopper.slots(), hopper.slots().len())),
        BlockEntity::Furnace(furnace) => {
            Some(fill(&[furnace.input().cloned(), furnace.fuel().cloned(), furnace.output().cloned()], 3))
        }
        BlockEntity::BrewingStand(stand) => Some(brewing_signal(stand)),
        BlockEntity::Crafter { slots, disabled } => {
            Some(slots.iter().zip(disabled).filter(|(held, off)| held.is_some() || **off).count() as u8)
        }
        BlockEntity::Lectern(lectern) => Some(lectern_signal(lectern)),
        BlockEntity::CommandBlock(data) => Some(data.success_count.clamp(0, 15) as u8),
        BlockEntity::Opaque { id, nbt } => opaque_signal(id.name(), nbt, Direction::Up, None),
        _ => None,
    }
}

fn above(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y + 1, pos.z)
}

/// The cell holding the other half of the double chest at `pos` in `state`,
/// or `None` for a single chest.
#[must_use]
pub(crate) fn chest_partner(pos: BlockPos, state: StateId) -> Option<BlockPos> {
    if !matches!(base_name(state), Block::Chest | Block::TrappedChest) {
        return None;
    }
    let facing = property_direction(state, PropertyKey::Facing)?;
    let toward = match get_str_property(state, PropertyKey::Type)? {
        BuiltinPropertyValue::Left => facing.clockwise(),
        BuiltinPropertyValue::Right => facing.counterclockwise(),
        _ => return None,
    };
    Some(toward.relative(pos))
}

fn chest_signal(registry: &BlockEntityRegistry, state_at: &dyn Fn(BlockPos) -> StateId, pos: BlockPos, state: StateId) -> u8 {
    let blocked = |cell: BlockPos| lodestone_data::redstone_conductor::conducts(state_at(above(cell)));
    if blocked(pos) {
        return 0;
    }
    let Some(BlockEntity::Container { slots, .. }) = registry.get(pos) else { return 0 };
    if let Some(other) = chest_partner(pos, state) {
        let neighbour = state_at(other);
        let facing = property_direction(state, PropertyKey::Facing);
        let mirrored = get_str_property(neighbour, PropertyKey::Type) != get_str_property(state, PropertyKey::Type)
            && matches!(
                get_str_property(neighbour, PropertyKey::Type),
                Some(BuiltinPropertyValue::Left | BuiltinPropertyValue::Right)
            );
        if base_name(neighbour) == base_name(state)
            && mirrored
            && property_direction(neighbour, PropertyKey::Facing) == facing
        {
            if blocked(other) {
                return 0;
            }
            if let Some(BlockEntity::Container { slots: more, .. }) = registry.get(other) {
                return fill(slots.iter().chain(more), slots.len() + more.len());
            }
        }
    }
    fill(slots, slots.len())
}

/// The output of the block at `pos` (in `state`) as a comparator on `side`
/// reads it, or `None` for a block with no output of this kind. `state_at`
/// reads the world around it (a double chest's partner, the cell above).
#[must_use]
pub(crate) fn signal(
    registry: &BlockEntityRegistry,
    state_at: &dyn Fn(BlockPos) -> StateId,
    pos: BlockPos,
    state: StateId,
    side: Direction,
) -> Option<u8> {
    let block = base_name(state);
    if !has_analog_output(block) {
        return None;
    }
    Some(match block {
        Block::Chest | Block::TrappedChest => chest_signal(registry, state_at, pos, state),
        _ if is_shelf(block) => match registry.get(pos) {
            Some(BlockEntity::Opaque { id, nbt }) => {
                opaque_signal(id.name(), nbt, side, property_direction(state, PropertyKey::Facing)).unwrap_or(0)
            }
            _ => 0,
        },
        _ => registry.get(pos).and_then(entity_signal).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests;
