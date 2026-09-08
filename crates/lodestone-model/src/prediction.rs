//! Version-free block-prediction sequence identities.

/// The wrapping sequence attached to a client-side block prediction.
///
/// Protocol packets carry this value as a signed VarInt, but the sequence is
/// an unsigned 32-bit counter. Keeping its bits intact here makes rollover
/// explicit and leaves signed conversion at the wire boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PredictionSequence(u32);

impl PredictionSequence {
    /// The sequence before the first predicted action.
    pub const INITIAL: Self = Self(0);

    /// Creates a sequence from its canonical counter representation.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Converts the signed wire representation without changing its bits.
    #[must_use]
    pub const fn from_wire(wire: i32) -> Self {
        Self(wire as u32)
    }

    /// Returns the canonical counter value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Converts to the signed representation used by the protocol.
    #[must_use]
    pub const fn as_wire(self) -> i32 {
        self.0 as i32
    }

    /// Allocates the next sequence, wrapping over the full 32-bit domain.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// Whether `self` is no newer than `acknowledged` in serial-number order.
    #[must_use]
    pub const fn is_at_or_before(self, acknowledged: Self) -> bool {
        (acknowledged.0.wrapping_sub(self.0) as i32) >= 0
    }
}

#[cfg(test)]
mod tests {
    use super::PredictionSequence;

    #[test]
    fn wire_conversion_preserves_signed_varint_bits() {
        for wire in [0, 1, -1, i32::MIN, i32::MAX] {
            assert_eq!(PredictionSequence::from_wire(wire).as_wire(), wire);
        }
    }

    #[test]
    fn counter_wraps_and_acknowledgement_crosses_signed_boundary() {
        let last = PredictionSequence::new(i32::MAX as u32);
        let first_after_wrap = last.next();
        assert_eq!(first_after_wrap.as_wire(), i32::MIN);
        assert!(last.is_at_or_before(first_after_wrap));
        assert!(!first_after_wrap.is_at_or_before(last));
    }
}
