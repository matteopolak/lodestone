//! Bounded inventory and fixed-layout slot domains.
//!
//! Protocol packets carry raw signed integers, including sentinels such as an
//! outside-window click.  Those values are validated at the adapter or UI
//! ingress.  Everything after that boundary carries one of these domains, so a
//! hotbar position cannot accidentally select a menu cell or a renderer layout
//! position.

/// One of the nine native player-hotbar positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct HotbarSlot(u8);

impl HotbarSlot {
    /// Number of selectable positions in a player hotbar.
    pub const COUNT: u8 = 9;

    /// Validates a native hotbar position.
    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if raw < Self::COUNT { Some(Self(raw)) } else { None }
    }

    /// Returns this position in the native inventory layout.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// Returns this position as an array index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl From<HotbarSlot> for u8 {
    fn from(slot: HotbarSlot) -> Self { slot.raw() }
}

impl From<HotbarSlot> for usize {
    fn from(slot: HotbarSlot) -> Self { slot.index() }
}

impl From<HotbarSlot> for i32 {
    fn from(slot: HotbarSlot) -> Self { i32::from(slot.raw()) }
}

impl From<HotbarSlot> for u32 {
    fn from(slot: HotbarSlot) -> Self { u32::from(slot.raw()) }
}

impl PartialEq<u8> for HotbarSlot {
    fn eq(&self, other: &u8) -> bool { self.raw() == *other }
}

/// A non-negative menu slot that fits the protocol's signed-short encoding.
///
/// This says only that a value can name a menu cell.  [`Self::within`] also
/// validates it against one particular open menu's dynamic length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MenuSlot(u16);

impl MenuSlot {
    /// Validates a raw slot number that crossed a wire or UI boundary.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        if raw >= 0 && raw <= i16::MAX as i32 {
            Some(Self(raw as u16))
        } else {
            None
        }
    }

    /// Validates a raw slot number for an open menu with `len` cells.
    #[must_use]
    pub const fn within(raw: i32, len: usize) -> Option<Self> {
        match Self::from_raw(raw) {
            Some(slot) if slot.index() < len => Some(slot),
            _ => None,
        }
    }

    /// Builds a slot from an already-bounded local collection index.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        if index <= i16::MAX as usize {
            Some(Self(index as u16))
        } else {
            None
        }
    }

    /// Returns this slot as a local collection index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the protocol's signed representation.
    #[must_use]
    pub const fn as_wire(self) -> i16 {
        self.0 as i16
    }
}

/// A slot in one particular backing container.
///
/// Container sizes are dynamic, so construction takes that container's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContainerSlot(usize);

impl ContainerSlot {
    /// Validates `index` against a container length.
    #[must_use]
    pub const fn new(index: usize, len: usize) -> Option<Self> {
        if index < len { Some(Self(index)) } else { None }
    }

    /// Returns the backing-container index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// A selected item inside a bundle's shown-content list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundleItemSlot(usize);

impl BundleItemSlot {
    /// Validates a selected item index against the bundle's shown item count.
    #[must_use]
    pub const fn new(index: usize, shown_items: usize) -> Option<Self> {
        if index < shown_items { Some(Self(index)) } else { None }
    }

    /// Validates a non-negative selected-item wire value.
    ///
    /// The bundle contents are looked up only when a click consumes this
    /// selection, so [`Self::new`] performs that second, dynamic bounds check
    /// at the collection boundary.
    #[must_use]
    pub const fn from_wire(raw: i32) -> Option<Self> {
        if raw >= 0 {
            Some(Self(raw as usize))
        } else {
            None
        }
    }

    /// Returns the shown-content index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }

    /// Returns the wire representation.
    #[must_use]
    pub const fn as_wire(self) -> i32 {
        self.0 as i32
    }
}

/// One of a brewing stand's three bottle cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrewingBottleSlot(u8);

impl BrewingBottleSlot {
    /// Number of bottle cells in a brewing stand.
    pub const COUNT: u8 = 3;

    /// Validates a bottle-cell position.
    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if raw < Self::COUNT { Some(Self(raw)) } else { None }
    }

    /// Returns the bottle-cell index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One of a crafter's nine input cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CrafterSlot(u8);

impl CrafterSlot {
    /// Number of crafter input cells.
    pub const COUNT: u8 = 9;

    /// Validates a crafter-cell position.
    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if raw < Self::COUNT { Some(Self(raw)) } else { None }
    }

    /// Returns the crafter-cell index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One of a campfire's four cooking positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CampfireSlot(u8);

impl CampfireSlot {
    /// Number of campfire cooking positions.
    pub const COUNT: u8 = 4;

    /// Validates a campfire cooking position.
    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if raw < Self::COUNT { Some(Self(raw)) } else { None }
    }

    /// Returns the fixed render-layout index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One of a shelf's three display positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShelfSlot(u8);

impl ShelfSlot {
    /// Number of shelf display positions.
    pub const COUNT: u8 = 3;

    /// Validates a shelf display position.
    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if raw < Self::COUNT { Some(Self(raw)) } else { None }
    }

    /// Returns the fixed render-layout index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_slot_domains_reject_their_first_invalid_value() {
        assert!(HotbarSlot::new(8).is_some());
        assert!(HotbarSlot::new(9).is_none());
        assert!(BrewingBottleSlot::new(2).is_some());
        assert!(BrewingBottleSlot::new(3).is_none());
        assert!(CrafterSlot::new(8).is_some());
        assert!(CrafterSlot::new(9).is_none());
        assert!(CampfireSlot::new(3).is_some());
        assert!(CampfireSlot::new(4).is_none());
        assert!(ShelfSlot::new(2).is_some());
        assert!(ShelfSlot::new(3).is_none());
    }

    #[test]
    fn menu_and_dynamic_slots_validate_their_real_bounds() {
        assert_eq!(MenuSlot::within(45, 46).map(MenuSlot::index), Some(45));
        assert!(MenuSlot::within(-1, 46).is_none());
        assert!(MenuSlot::within(46, 46).is_none());
        assert!(MenuSlot::from_raw(i16::MAX as i32).is_some());
        assert!(MenuSlot::from_raw(i16::MAX as i32 + 1).is_none());
        assert_eq!(ContainerSlot::new(2, 3).map(ContainerSlot::index), Some(2));
        assert!(ContainerSlot::new(3, 3).is_none());
        assert_eq!(BundleItemSlot::from_wire(1).map(BundleItemSlot::index), Some(1));
        assert!(BundleItemSlot::from_wire(-1).is_none());
        assert!(BundleItemSlot::new(2, 2).is_none());
    }
}
