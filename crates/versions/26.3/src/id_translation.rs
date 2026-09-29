//! Boundary translation for 26.2 canonical block states and items.
//!
//! The canonical game data stays in `lodestone-data`. These compact runs map
//! the same names and block-state properties to 26.3 wire IDs. New 26.3-only
//! entries cannot be represented by the canonical types and fail on decode.

use lodestone_data::{block_states::StateId, item::Item};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    source: u16,
    target: u16,
    len: u16,
}

impl Run {
    const fn new(source: u16, target: u16, len: u16) -> Self {
        Self { source, target, len }
    }
}

mod generated {
    use super::Run;
    include!("generated/id_translation.rs");
}

fn translate(runs: &[Run], source: u32) -> Option<u16> {
    let index = runs.partition_point(|run| u32::from(run.source) <= source);
    let run = runs.get(index.checked_sub(1)?)?;
    let offset = source - u32::from(run.source);
    if offset < u32::from(run.len) {
        Some(run.target + offset as u16)
    } else {
        None
    }
}

/// A validated block-state ID from the 26.3 wire registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireBlockStateId(u16);

impl WireBlockStateId {
    pub const COUNT: u32 = generated::WIRE_BLOCK_STATE_COUNT;

    #[must_use]
    pub fn new(raw: u32) -> Option<Self> {
        (raw < Self::COUNT).then_some(Self(raw as u16))
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0 as u32
    }
}

/// A validated item ID from the 26.3 wire registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireItemId(u16);

impl WireItemId {
    pub const COUNT: u32 = generated::WIRE_ITEM_COUNT;

    #[must_use]
    pub fn new(raw: u32) -> Option<Self> {
        (raw < Self::COUNT).then_some(Self(raw as u16))
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0 as u32
    }
}

/// The 26.3 block state exists, but 26.2 canonical game data has no match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedWireBlockState(pub WireBlockStateId);

/// The 26.3 item exists, but 26.2 canonical game data has no match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedWireItem(pub WireItemId);

/// Translate a canonical block state to its 26.3 wire ID.
#[must_use]
pub fn block_state_to_wire(state: StateId) -> WireBlockStateId {
    let raw = translate(generated::BLOCK_STATE_TO_WIRE, state.raw())
        .expect("every canonical block state must have a generated 26.3 mapping");
    WireBlockStateId(raw)
}

/// Translate a 26.3 block state, rejecting states introduced after 26.2.
pub fn block_state_from_wire(
    id: WireBlockStateId,
) -> Result<StateId, UnsupportedWireBlockState> {
    let canonical = translate(generated::BLOCK_STATE_FROM_WIRE, id.raw())
        .ok_or(UnsupportedWireBlockState(id))?;
    Ok(StateId::new(u32::from(canonical)).expect("generated canonical block state is in range"))
}

/// Translate a canonical item to its 26.3 wire ID.
#[must_use]
pub fn item_to_wire(item: Item) -> WireItemId {
    let raw = translate(generated::ITEM_TO_WIRE, u32::from(item.registry_id()))
        .expect("every canonical item must have a generated 26.3 mapping");
    WireItemId(raw)
}

/// Translate a 26.3 item, rejecting items introduced after 26.2.
pub fn item_from_wire(id: WireItemId) -> Result<Item, UnsupportedWireItem> {
    let canonical = translate(generated::ITEM_FROM_WIRE, id.raw())
        .ok_or(UnsupportedWireItem(id))?;
    Ok(Item::from_registry_id(canonical).expect("generated canonical item is in range"))
}
