//! [`Effect`] — what a built-in command *asks for*, rather than what it does.
//!
//! # Why an enum instead of just doing the thing
//!
//! This is not indirection for its own sake; it is forced, and by something
//! concrete. A player's game mode lives in a **local variable** in
//! `crate::server`'s `dispatch_play_packet` (`game_mode: &mut GameMode`), and an
//! executor is a shared `Arc` closure with no access to any connection's stack
//! frame. So an executor physically cannot write it. The same is true of the
//! player's inventory and of every `proto.encode_*` directive, which needs the
//! connection's own `ServerProtocol` and transport.
//!
//! An executor therefore returns typed *requests*, and exactly one place applies
//! them — the place that holds the connection.
//!
//! # Two delivery paths, and only one of them is new
//!
//! | target | path |
//! |---|---|
//! | the caller's own connection | applied inline by the `ChatCommand` arm, through `proto`, exactly as the hand-rolled `/gamemode` arm already did |
//! | any *other* player | queued on the shared [`crate::PlayerRegistry`] and drained by that player's own connection loop |
//!
//! The second is the genuinely new mechanism. `outgoing_chat` →
//! `PlayerRegistry::say` → every connection's `chat_since` is the precedent, but
//! chat is a *broadcast*: every connection reads every line through its own
//! cursor. An effect is **directed** — `/gamemode creative Steve` must reach
//! Steve and nobody else — so it is a per-uuid queue rather than a shared log,
//! and it is drained (not cursored) because a second reader must not also
//! receive it.
//!
//! `/give @a` needs this as much as `/gamemode <target>` does, which is why it
//! is built now rather than when a second command asks for it.

use lodestone_model::{GameMode, ItemStack};
use uuid::Uuid;

/// One thing a command wants done to one player.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Set the player's game mode, and send them the mode + abilities pair.
    ///
    /// The pair must never be split — a client told it is in creative without
    /// the abilities packet is in creative and cannot fly — which is why
    /// `crate::server::game_mode_directives` exists and why this carries the
    /// mode rather than two separate effects.
    SetGameMode(GameMode),
    /// Put these stacks in the player's inventory, spilling nothing.
    ///
    /// A `Vec` rather than one stack because `GiveCommand` splits a count across
    /// whole stacks at the item's own max stack size, and the split is the
    /// command's business, not the applier's.
    GiveItems(Vec<ItemStack>),
    /// Send the player a system-chat line — the `gameMode.changed` notification
    /// a target receives when *someone else* changes their mode.
    Message(String),
    /// Apply a status effect (`/effect give`) — issue #259's producer.
    ///
    /// `duration` is in **ticks**, already multiplied out from the command's seconds
    /// argument, and [`crate::mob_effects::INFINITE_DURATION`] is vanilla's default
    /// for the two-argument form. `amplifier` is zero-based: `0` is level I.
    ApplyEffect {
        /// A namespaced effect id, e.g. `minecraft:poison`.
        effect: String,
        duration: i32,
        amplifier: u32,
    },
    /// Remove one status effect, or every one (`/effect clear`).
    ///
    /// `None` is the clear-everything form. A `Some` naming an effect the target does
    /// not have is a no-op rather than an error, matching vanilla's own return of `0`
    /// affected entities.
    ClearEffects {
        effect: Option<String>,
    },
}

/// An [`Effect`] plus who it is for.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectedEffect {
    /// The target's profile uuid.
    pub target: Uuid,
    pub effect: Effect,
}

impl DirectedEffect {
    #[must_use]
    pub fn new(target: Uuid, effect: Effect) -> Self {
        Self { target, effect }
    }
}
