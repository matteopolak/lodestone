//! The kinds of non-player entity a mob's targeting and look goals can name.
//!
//! The seam hands goals positions, never entities, so a goal that wants "the
//! nearest villager" asks for a [`TargetClass`] and the host answers with the
//! position of the nearest member that satisfies the class's own predicate
//! (baby-and-on-land for a turtle, for instance).

/// A class of entity a goal can ask the host for the nearest member of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetClass {
    /// A villager or a wandering trader.
    Villager,
    IronGolem,
    /// A baby turtle that is out of water.
    BabyLandTurtle,
    Axolotl,
    /// A piglin or a piglin brute.
    Piglin,
    Rabbit,
    /// What an untamed wolf hunts: sheep, rabbits and foxes.
    WolfPrey,
    /// Any skeleton variant.
    Skeleton,
    Endermite,
    /// A guardian or an elder guardian.
    Guardian,
    /// Any hostile mob.
    Hostile,
    /// Any mob of a different species from the asking mob.
    OtherSpecies,
}

impl TargetClass {
    /// Every class, in discriminant order.
    pub const ALL: [TargetClass; 12] = [
        Self::Villager,
        Self::IronGolem,
        Self::BabyLandTurtle,
        Self::Axolotl,
        Self::Piglin,
        Self::Rabbit,
        Self::WolfPrey,
        Self::Skeleton,
        Self::Endermite,
        Self::Guardian,
        Self::Hostile,
        Self::OtherSpecies,
    ];

    /// The number of classes.
    pub const COUNT: usize = Self::ALL.len();

    /// This class's slot in a per-class table.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// A set of [`TargetClass`]es.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TargetClassSet(u16);

impl TargetClassSet {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// Adds `class`.
    pub fn insert(&mut self, class: TargetClass) {
        self.0 |= 1 << class.index();
    }

    /// Whether `class` is in the set.
    #[must_use]
    pub const fn contains(self, class: TargetClass) -> bool {
        self.0 & (1 << class.index()) != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}
