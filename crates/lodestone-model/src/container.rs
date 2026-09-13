//! Version-free container synchronization identities.

/// A container revision counter shared between the predicted menu and the
/// server's authoritative menu state.
///
/// The protocol represents this counter as a signed VarInt while its behavior
/// is an unsigned wrapping counter. Keeping the canonical representation as
/// `u32` prevents a negative wire value from becoming an invalid local state;
/// [`from_wire`](Self::from_wire) and [`as_wire`](Self::as_wire) preserve the
/// same 32 bits at that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ContainerStateId(u32);

impl ContainerStateId {
    /// The state used before a server has supplied a revision.
    pub const INITIAL: Self = Self(0);

    /// Creates a state id from its canonical unsigned representation.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Converts the signed wire representation without changing its bits.
    #[must_use]
    pub const fn from_wire(wire: i32) -> Self {
        Self(wire as u32)
    }

    /// Returns the canonical unsigned counter value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Converts to the signed wire representation without changing its bits.
    #[must_use]
    pub const fn as_wire(self) -> i32 {
        self.0 as i32
    }

    /// Returns the next counter value, wrapping across the full `u32` domain.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerStateId;

    #[test]
    fn signed_wire_boundary_preserves_every_bit() {
        for wire in [0, 1, -1, i32::MIN, i32::MAX] {
            assert_eq!(ContainerStateId::from_wire(wire).as_wire(), wire);
        }
        assert_eq!(ContainerStateId::from_wire(-1).raw(), u32::MAX);
    }

    #[test]
    fn counter_wraps_from_the_full_unsigned_maximum() {
        assert_eq!(ContainerStateId::new(u32::MAX).next(), ContainerStateId::INITIAL);
    }
}
