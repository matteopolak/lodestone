//! Rules for the blocks that hold items without a menu: the jukebox, the
//! chiseled bookshelf and the decorated pot, plus the shulker-box contents
//! that travel with the dropped item.
//!
//! # What it is
//!
//! Pure functions over a block entity's save compound and the block state. The
//! I/O half (sounds, packets, the player's inventory) is
//! `server::stored_items_use`; this module decides what an interaction does.
//!
//! # How it works
//!
//! Jukebox, bookshelf and pot live as [`BlockEntity::Opaque`] compounds in the
//! shape they are saved in, so nothing here needs a second representation:
//!
//! * jukebox: `RecordItem`, `ticks_since_song_started`; block state `has_record`.
//! * chiseled bookshelf: `Items` (slot-tagged), `last_interacted_slot`; block
//!   state `slot_N_occupied`. The comparator reads `last_interacted_slot + 1`.
//! * decorated pot: `sherds` (four item ids, back/left/right/front, a plain
//!   brick where undecorated) and `item`.
//!
//! A jukebox song's wire id is its position in the alphabetically ordered song
//! registry, which is also the order of [`crate::container_signal`]'s disc
//! table, so one table serves the level event and the comparator.
//!
//! # How to change it
//!
//! A new stored-item block gets a constant here, a `fresh` arm and a branch in
//! `server::stored_items_use`. The block entity type must also appear in
//! [`crate::container_signal`]'s analog list or comparators will not wake.
//!
//! # Dependencies
//!
//! [`crate::item_nbt`] for stack save forms, [`crate::chunk_nbt`] for the slot
//! list codec, [`crate::redstone`] for block-state properties.

use std::collections::BTreeMap;

use lodestone_core::{Nbt, NbtTag};
use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack, PotDecorations};

use crate::block_entities::BlockEntity;
use crate::neighbor_update::Direction;

pub(crate) const JUKEBOX_ID: &str = "minecraft:jukebox";
pub(crate) const BOOKSHELF_ID: &str = "minecraft:chiseled_bookshelf";
pub(crate) const POT_ID: &str = "minecraft:decorated_pot";

/// Slots in a chiseled bookshelf: two rows of three.
pub(crate) const BOOKSHELF_SLOTS: usize = 6;
const BOOKSHELF_COLUMNS: usize = 3;
const BOOKSHELF_ROWS: usize = 2;

/// Level event that starts a jukebox song; `data` is the song's registry id.
pub(crate) const JUKEBOX_PLAY_EVENT: i32 = 1010;
/// Level event that stops the song at a position.
pub(crate) const JUKEBOX_STOP_EVENT: i32 = 1011;

/// The `minecraft:bookshelf_books` item tag.
const BOOKSHELF_BOOKS: [&str; 5] = [
    "minecraft:book",
    "minecraft:written_book",
    "minecraft:enchanted_book",
    "minecraft:writable_book",
    "minecraft:knowledge_book",
];

/// How a decorated pot moves when something is put in or refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wobble {
    /// An item went in.
    Positive = 0,
    /// The click was refused.
    Negative = 1,
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
        _ => None,
    }
}

fn set_field(nbt: &mut Nbt, key: &str, value: Nbt) {
    if let Nbt::Compound(fields) = nbt {
        match fields.iter_mut().find(|(name, _)| name == key) {
            Some((_, slot)) => *slot = value,
            None => fields.push((key.to_owned(), value)),
        }
    }
}

fn remove_field(nbt: &mut Nbt, key: &str) {
    if let Nbt::Compound(fields) = nbt {
        fields.retain(|(name, _)| name != key);
    }
}

/// A fresh save compound for the block entity `id` at `pos`.
fn base(id: &str, pos: BlockPos) -> Nbt {
    Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String(id.to_owned())),
        ("x".to_owned(), Nbt::Int(pos.x)),
        ("y".to_owned(), Nbt::Int(pos.y)),
        ("z".to_owned(), Nbt::Int(pos.z)),
    ])
}

/// The empty block entity a freshly placed stored-item block starts with, or
/// `None` for any other block entity type. A pot placed from a decorated item
/// takes that item's sherds.
#[must_use]
pub(crate) fn fresh(id: &str, pos: BlockPos, held: Option<&ItemStack>) -> Option<BlockEntity> {
    if !matches!(id, JUKEBOX_ID | BOOKSHELF_ID | POT_ID) {
        return None;
    }
    let mut nbt = base(id, pos);
    if id == POT_ID
        && let Some(decorations) = held.and_then(|stack| stack.components.pot_decorations.as_ref())
    {
        set_field(&mut nbt, "sherds", sherds_to_nbt(decorations));
    }
    Some(BlockEntity::Opaque { id: id.to_owned().into(), nbt })
}

/// A pot's update tag: what a client needs to draw it, its sherds.
#[must_use]
pub(crate) fn update_tag(nbt: &Nbt) -> Nbt {
    let mut tag = Nbt::Compound(Vec::new());
    if let Some(sherds) = field(nbt, "sherds") {
        set_field(&mut tag, "sherds", sherds.clone());
    }
    tag
}

fn sherd_name(key: &Option<lodestone_model::ResourceKey>) -> String {
    key.as_ref().map_or_else(|| "minecraft:brick".to_owned(), ToString::to_string)
}

fn sherds_to_nbt(decorations: &PotDecorations) -> Nbt {
    let names = [&decorations.back, &decorations.left, &decorations.right, &decorations.front]
        .map(|key| Nbt::String(sherd_name(key)));
    Nbt::List { element_type: NbtTag::String, elements: names.to_vec() }
}

/// The four sherd item ids of a pot's save compound, back/left/right/front;
/// a pot without the list is four plain bricks.
#[must_use]
pub(crate) fn sherds(nbt: &Nbt) -> [String; 4] {
    let mut out = std::array::from_fn(|_| "minecraft:brick".to_owned());
    if let Some(Nbt::List { elements, .. }) = field(nbt, "sherds") {
        for (slot, element) in out.iter_mut().zip(elements) {
            if let Nbt::String(name) = element {
                slot.clone_from(name);
            }
        }
    }
    out
}

// ---- jukebox ------------------------------------------------------------

/// The song a disc item plays: its registry id and comparator output.
#[must_use]
pub(crate) fn song_of(stack: &ItemStack) -> Option<(i32, u8)> {
    let item = stack.item.to_string();
    let song = item.strip_prefix("minecraft:music_disc_")?;
    crate::container_signal::DISC_SIGNALS
        .iter()
        .position(|(name, _)| *name == song)
        .map(|id| (id as i32, crate::container_signal::DISC_SIGNALS[id].1))
}

/// The disc a jukebox holds.
#[must_use]
pub(crate) fn jukebox_disc(nbt: &Nbt) -> Option<ItemStack> {
    field(nbt, "RecordItem").and_then(crate::item_nbt::stack_from_nbt)
}

/// Puts `disc` into, or (with `None`) takes the disc out of, a jukebox compound.
pub(crate) fn set_jukebox_disc(nbt: &mut Nbt, disc: Option<&ItemStack>) {
    match disc {
        Some(disc) => {
            let mut one = disc.clone();
            one.count = 1;
            set_field(nbt, "RecordItem", Nbt::Compound(crate::item_nbt::stack_to_nbt(&one).fields));
            set_field(nbt, "ticks_since_song_started", Nbt::Long(0));
        }
        None => {
            remove_field(nbt, "RecordItem");
            remove_field(nbt, "ticks_since_song_started");
        }
    }
}

/// Each song's length in seconds, in registry order like
/// [`crate::container_signal::DISC_SIGNALS`].
const SONG_SECONDS: &[(&str, f32)] = &[
    ("11", 71.0),
    ("13", 178.0),
    ("5", 178.0),
    ("blocks", 345.0),
    ("bounce", 234.0),
    ("cat", 185.0),
    ("chirp", 185.0),
    ("creator", 176.0),
    ("creator_music_box", 73.0),
    ("far", 174.0),
    ("lava_chicken", 134.0),
    ("mall", 197.0),
    ("mellohi", 96.0),
    ("otherside", 195.0),
    ("pigstep", 149.0),
    ("precipice", 299.0),
    ("relic", 218.0),
    ("stal", 150.0),
    ("strad", 188.0),
    ("tears", 175.0),
    ("wait", 238.0),
    ("ward", 251.0),
];

/// Ticks a song lasts after it starts: its length plus a second of padding.
#[must_use]
pub(crate) fn song_ticks(stack: &ItemStack) -> Option<u64> {
    let item = stack.item.to_string();
    let song = item.strip_prefix("minecraft:music_disc_")?;
    let (_, seconds) = SONG_SECONDS.iter().find(|(name, _)| *name == song)?;
    Some((seconds * 20.0).ceil() as u64 + 20)
}

/// Whether the jukebox compound is mid-song.
#[must_use]
pub(crate) fn jukebox_is_playing(nbt: &Nbt) -> bool {
    field(nbt, "ticks_since_song_started").is_some()
}

/// Advances a jukebox compound's song one tick. Returns true on the tick the
/// song ends, which clears the clock; the disc stays.
pub(crate) fn tick_jukebox(nbt: &mut Nbt) -> bool {
    let Some(Nbt::Long(elapsed)) = field(nbt, "ticks_since_song_started") else { return false };
    let elapsed = u64::try_from(*elapsed).unwrap_or(0);
    let length = jukebox_disc(nbt).as_ref().and_then(song_ticks);
    if length.is_none_or(|length| elapsed >= length) {
        remove_field(nbt, "ticks_since_song_started");
        return true;
    }
    set_field(nbt, "ticks_since_song_started", Nbt::Long(elapsed as i64 + 1));
    false
}

// ---- chiseled bookshelf ---------------------------------------------------

/// Whether `item` may be shelved.
#[must_use]
pub(crate) fn is_shelvable_book(item: &str) -> bool {
    BOOKSHELF_BOOKS.contains(&item)
}

/// Which shelf slot a click on the front face selected, from the block-local
/// hit position, or `None` when the click was not on the front face.
///
/// The face is split into a 3 x 2 grid of equal sections, row 0 on top and
/// column 0 on the viewer's left, so the horizontal coordinate is mirrored for
/// the faces whose left-hand side runs against the world axis.
#[must_use]
pub(crate) fn hit_slot(facing: Direction, clicked: Direction, cursor: (f32, f32, f32)) -> Option<usize> {
    if facing != clicked {
        return None;
    }
    let (x, y, z) = cursor;
    let across = match facing {
        Direction::North => 1.0 - x,
        Direction::South => x,
        Direction::West => z,
        Direction::East => 1.0 - z,
        Direction::Up | Direction::Down => return None,
    };
    let section = |coordinate: f32, sections: usize| {
        ((coordinate * 16.0 / (16.0 / sections as f32)).floor().max(0.0) as usize).min(sections - 1)
    };
    let row = section(1.0 - y, BOOKSHELF_ROWS);
    let column = section(across, BOOKSHELF_COLUMNS);
    Some(column + row * BOOKSHELF_COLUMNS)
}

/// The six slots of a bookshelf compound.
#[must_use]
pub(crate) fn bookshelf_items(nbt: &Nbt) -> Vec<Option<ItemStack>> {
    crate::chunk_nbt::items_from_nbt(field(nbt, "Items"), BOOKSHELF_SLOTS)
}

/// Writes `slot` of a bookshelf compound and records it as the last one touched.
pub(crate) fn set_bookshelf_slot(nbt: &mut Nbt, slot: usize, book: Option<ItemStack>) {
    let mut items = bookshelf_items(nbt);
    items[slot] = book;
    set_field(nbt, "Items", crate::chunk_nbt::items_to_nbt(&items));
    set_field(nbt, "last_interacted_slot", Nbt::Int(slot as i32));
}

/// `state` with the `slot_N_occupied` flags matching `items`.
#[must_use]
pub(crate) fn bookshelf_state(state: StateId, items: &[Option<ItemStack>]) -> Option<StateId> {
    let mut parts = properties_of(state);
    for slot in 0..BOOKSHELF_SLOTS {
        let held = items.get(slot).is_some_and(Option::is_some);
        parts.insert(format!("slot_{slot}_occupied"), held.to_string());
    }
    StateId::from_exact_parts(state.block().name(), &parts)
}

fn properties_of(state: StateId) -> BTreeMap<String, String> {
    state.properties().iter().map(|&(key, value)| (key.to_owned(), value.to_owned())).collect()
}

/// `state` with one named property replaced.
#[must_use]
pub(crate) fn with_property(state: StateId, key: &str, value: &str) -> Option<StateId> {
    let mut parts = properties_of(state);
    parts.insert(key.to_owned(), value.to_owned());
    StateId::from_exact_parts(state.block().name(), &parts)
}

// ---- decorated pot --------------------------------------------------------

/// The stack a pot holds.
#[must_use]
pub(crate) fn pot_item(nbt: &Nbt) -> Option<ItemStack> {
    field(nbt, "item").and_then(crate::item_nbt::stack_from_nbt)
}

fn stack_limit(stack: &ItemStack) -> u32 {
    crate::container_click::max_stack_size(stack).max(1)
}

/// What a pot does with one item from `held`: `Some(fill)` (the held
/// fraction of a full stack after the insert, which bends the pitch) and the
/// pot's new stack, or `None` when the pot refuses it. A pot takes one
/// kind of item, up to that item's stack size.
#[must_use]
pub(crate) fn pot_insert(current: Option<&ItemStack>, held: &ItemStack) -> Option<(ItemStack, f32)> {
    if held.count == 0 {
        return None;
    }
    let next = match current {
        None => {
            let mut one = held.clone();
            one.count = 1;
            one
        }
        Some(stack) if stack.item == held.item && stack.components == held.components && stack.count < stack_limit(stack) => {
            let mut grown = stack.clone();
            grown.count += 1;
            grown
        }
        Some(_) => return None,
    };
    let fill = next.count as f32 / stack_limit(&next) as f32;
    Some((next, fill))
}

/// Stores `item` in a pot compound.
pub(crate) fn set_pot_item(nbt: &mut Nbt, item: Option<&ItemStack>) {
    match item {
        Some(item) => set_field(nbt, "item", Nbt::Compound(crate::item_nbt::stack_to_nbt(item).fields)),
        None => remove_field(nbt, "item"),
    }
}

/// Whether breaking a pot with `tool` shatters it: a sword, axe, pickaxe,
/// shovel, hoe, trident or mace without silk touch.
#[must_use]
pub(crate) fn tool_shatters_pot(tool: Option<&ItemStack>) -> bool {
    let Some(tool) = tool else { return false };
    let item = tool.item.to_string();
    let name = item.strip_prefix("minecraft:").unwrap_or(&item);
    let breaks = ["_sword", "_axe", "_pickaxe", "_shovel", "_hoe"].iter().any(|suffix| name.ends_with(suffix))
        || matches!(name, "trident" | "mace");
    let silk = crate::enchantment_data::id_of("minecraft:silk_touch");
    breaks && !tool.components.enchantments.iter().any(|enchantment| Some(enchantment.id) == silk)
}

// ---- drops ----------------------------------------------------------------

/// The stacks a block gives up when it is removed, besides what its loot table
/// already rolls.
///
/// Contents spill whatever removed the block: every slot of a chest, hopper,
/// furnace and the like, a pot's item, a jukebox's disc, a bookshelf's books. A
/// shulker box is the exception, because its contents travel on the item it
/// drops (trailing empty slots trimmed, as the item's component holds them); a
/// pot drops its sherds (bricks for the plain faces) when cracked, otherwise a
/// pot item carrying them. `item_drops` is `false` when the world's block-drop
/// rule is off, which keeps only the contents that spill on their own.
#[must_use]
pub(crate) fn broken_drops(broken: StateId, entity: Option<&BlockEntity>, item_drops: bool) -> Vec<ItemStack> {
    let block = broken.block();
    let mut out = Vec::new();
    match (block, entity) {
        (block, Some(BlockEntity::Container { slots, .. })) if crate::block_entities::is_shulker_box(block) => {
            if item_drops && let Ok(key) = block.name().parse() {
                let mut stack = ItemStack::new(key, 1);
                let kept = slots.iter().rposition(Option::is_some).map_or(0, |last| last + 1);
                stack.components.container = slots[..kept].to_vec();
                out.push(stack);
            }
        }
        (Block::DecoratedPot, Some(BlockEntity::Opaque { nbt, .. })) => {
            if item_drops {
                let [back, left, right, front] = sherds(nbt);
                if crate::redstone::property_bool(broken, lodestone_data::block_properties::PropertyKey::Cracked) == Some(true) {
                    out.extend([back, left, right, front].into_iter().filter_map(|name| name.parse().ok()).map(|key| ItemStack::new(key, 1)));
                } else {
                    let mut pot = ItemStack::new("minecraft:decorated_pot".parse().expect("static id"), 1);
                    let key = |name: String| (name != "minecraft:brick").then(|| name.parse().ok()).flatten();
                    let decorations = PotDecorations { back: key(back), left: key(left), right: key(right), front: key(front) };
                    pot.components.pot_decorations = (decorations != PotDecorations::default()).then_some(decorations);
                    out.push(pot);
                }
            }
            out.extend(pot_item(nbt));
        }
        (Block::Jukebox, Some(BlockEntity::Opaque { nbt, .. })) => out.extend(jukebox_disc(nbt)),
        (Block::ChiseledBookshelf, Some(BlockEntity::Opaque { nbt, .. })) => out.extend(bookshelf_items(nbt).into_iter().flatten()),
        (_, Some(entity)) => out.extend(entity.spilled_contents()),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests;
