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
