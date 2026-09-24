/// The player's current game mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameMode {
    /// Survival mode.
    Survival,
    /// Creative mode.
    Creative,
    /// Adventure mode.
    Adventure,
    /// Spectator mode.
    Spectator,
}

/// A player's hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hand {
    /// Main hand.
    Main,
    /// Off hand.
    Off,
}

impl Hand {
    /// Resolves the two hand ordinals used by protocol packets.
    #[must_use]
    pub const fn from_wire_ordinal(ordinal: i32) -> Option<Self> {
        match ordinal {
            0 => Some(Self::Main),
            1 => Some(Self::Off),
            _ => None,
        }
    }

    /// Returns the protocol ordinal for this hand.
    #[must_use]
    pub const fn wire_ordinal(self) -> i32 {
        match self {
            Self::Main => 0,
            Self::Off => 1,
        }
    }
}

#[cfg(test)]
mod hand_tests {
    use super::Hand;

    #[test]
    fn wire_ordinals_are_validated_at_the_boundary() {
        assert_eq!(Hand::from_wire_ordinal(0), Some(Hand::Main));
        assert_eq!(Hand::from_wire_ordinal(1), Some(Hand::Off));
        assert_eq!(Hand::from_wire_ordinal(-1), None);
        assert_eq!(Hand::from_wire_ordinal(2), None);
    }

    #[test]
    fn every_hand_round_trips_through_its_wire_ordinal() {
        for hand in [Hand::Main, Hand::Off] {
            assert_eq!(Hand::from_wire_ordinal(hand.wire_ordinal()), Some(hand));
        }
    }
}

/// World difficulty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Difficulty {
    /// Peaceful difficulty.
    Peaceful,
    /// Easy difficulty.
    Easy,
    /// Normal difficulty.
    Normal,
    /// Hard difficulty.
    Hard,
}
