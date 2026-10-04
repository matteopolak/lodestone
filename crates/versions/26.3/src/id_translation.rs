//! Typed 26.3 wire boundaries over the canonical state and item census.
//!
//! `GameDataVersion::V26_3` owns runtime mappings, including appended identities.
//! The compact report table in `src/generated/id_translation.rs` and its
//! `tools/gen_id_translation.py` generator remain a report-map drift fixture;
//! this module does not link that table into the runtime.

use lodestone_data::{GameDataVersion, block_states::StateId, item::Item};

/// A validated block-state ID from the 26.3 wire registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireBlockStateId(u16);

impl WireBlockStateId {
    pub const COUNT: u32 = GameDataVersion::V26_3.state_count();

    #[must_use]
    pub fn new(raw: u32) -> Option<Self> {
        if raw < Self::COUNT {
            u16::try_from(raw).ok().map(Self)
        } else {
            None
        }
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
    pub const COUNT: u32 = GameDataVersion::V26_3.item_count();

    #[must_use]
    pub fn new(raw: u32) -> Option<Self> {
        if raw < Self::COUNT {
            u16::try_from(raw).ok().map(Self)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0 as u32
    }
}

/// A validated wire state has no identity in the selected canonical data profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedWireBlockState(pub WireBlockStateId);

/// A validated wire item has no identity in the selected canonical data profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedWireItem(pub WireItemId);

/// Translate a canonical block state to its 26.3 wire ID.
#[must_use]
pub fn block_state_to_wire(state: StateId) -> WireBlockStateId {
    let raw = GameDataVersion::V26_3.state_to_wire(state)
        .expect("every canonical block state has a 26.3 identity");
    WireBlockStateId::new(raw).expect("selected profile emits a valid 26.3 state ID")
}

/// Translate a 26.3 block state through the canonical union.
pub fn block_state_from_wire(
    id: WireBlockStateId,
) -> Result<StateId, UnsupportedWireBlockState> {
    GameDataVersion::V26_3.state_from_wire(id.raw()).ok_or(UnsupportedWireBlockState(id))
}

/// Translate a canonical item to its 26.3 wire ID.
#[must_use]
pub fn item_to_wire(item: Item) -> WireItemId {
    let raw = GameDataVersion::V26_3.item_to_wire(item)
        .expect("every canonical item has a 26.3 identity");
    WireItemId::new(raw).expect("selected profile emits a valid 26.3 item ID")
}

/// Translate a 26.3 item through the canonical union.
pub fn item_from_wire(id: WireItemId) -> Result<Item, UnsupportedWireItem> {
    GameDataVersion::V26_3.item_from_wire(id.raw()).ok_or(UnsupportedWireItem(id))
}
