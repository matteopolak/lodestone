//! Version-free block-prediction sequence identities.

/// The client sequence attached to a predictive block action.
///
/// The protocol carries this value as a signed VarInt, but the sequence is a
/// wrapping 32-bit counter. Keeping the counter unsigned locally preserves
/// the wire bits and makes the rollover explicit instead of allowing signed
/// arithmetic to turn a valid post-rollover acknowledgement into a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PredictionSequence(u32);

impl PredictionSequence {
    /// The sequence before the first predicted action.
    pub const INITIAL: Self = Self(0);

    /// Creates a sequence from its canonical unsigned representation.
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

    /// Converts to the signed representation used by the protocol.
    #[must_use]
    pub const fn as_wire(self) -> i32 {
        self.0 as i32
    }

    /// Allocates the next sequence, wrapping across the full 32-bit domain.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// Whether `self` is no newer than an acknowledgement at `acknowledged`.
    ///
    /// Sequence ids use serial-number ordering rather than signed integer
    /// ordering. The subtraction is interpreted as a signed distance, so a
    /// pending `i32::MAX` is correctly settled by an acknowledgement at
    /// `i32::MIN` after rollover. As with every serial counter, distances of
    /// exactly half the domain are ambiguous; the client never keeps that many
    /// predictions outstanding.
    #[must_use]
    pub const fn is_at_or_before(self, acknowledged: Self) -> bool {
        (acknowledged.0.wrapping_sub(self.0) as i32) >= 0
    }

    /// Alias for [`Self::is_at_or_before`] that reads naturally at a ledger
    /// call site.
    #[must_use]
    pub const fn is_acknowledged_by(self, acknowledged: Self) -> bool {
        self.is_at_or_before(acknowledged)
    }
}

#[cfg(test)]
mod tests {
    use super::PredictionSequence;

    #[test]
    fn signed_wire_boundary_preserves_every_bit() {
        for wire in [0, 1, -1, i32::MIN, i32::MAX] {
            assert_eq!(PredictionSequence::from_wire(wire).as_wire(), wire);
        }
        assert_eq!(PredictionSequence::from_wire(-1).raw(), u32::MAX);
    }

    #[test]
    fn counter_wraps_from_the_full_unsigned_maximum() {
        assert_eq!(PredictionSequence::new(u32::MAX).next(), PredictionSequence::INITIAL);
        assert_eq!(PredictionSequence::INITIAL.next(), PredictionSequence::new(1));
    }

    #[test]
    fn acknowledgement_order_crosses_signed_wire_boundary() {
        let last = PredictionSequence::new(i32::MAX as u32);
        let first_after_wrap = last.next();
        assert!(last.is_at_or_before(first_after_wrap));
        assert!(first_after_wrap.is_at_or_before(first_after_wrap));
        assert!(!first_after_wrap.is_at_or_before(last));
    }

    #[test]
    fn old_ack_does_not_settle_newer_wrapped_prediction() {
        let old_ack = PredictionSequence::new(i32::MAX as u32);
        let newer = old_ack.next();
        assert!(!newer.is_acknowledged_by(old_ack));
    }
}
