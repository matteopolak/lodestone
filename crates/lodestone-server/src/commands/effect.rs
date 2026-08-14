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
    /// Set health to zero and run the death sequence — `/kill`
    /// (`Entity.kill()` → `hurtServer(damageSources().genericKill(), MAX_VALUE)`).
    Kill,
    /// `/experience add` — vanilla's `giveExperiencePoints`/`giveExperienceLevels`,
    /// `levels` selecting which.
    GiveExperience {
        levels: bool,
        amount: i32,
    },
    /// `/experience set` — an *absolute* value, applied by zeroing the target's
    /// experience first rather than by diffing against its current value, so the
    /// result does not depend on read-then-write ordering across two commands aimed
    /// at the same tick.
    SetExperience {
        levels: bool,
        amount: i32,
    },
    /// `/clear` — remove items from the target's own inventory (hotbar, main
    /// storage, armour and the off-hand — `Inventory.clearOrCountMatchingItems`'s
    /// own scope). `item` is a canonical id filter (`None` clears everything);
    /// `max_count` caps how many stacks' worth are removed (`None` is vanilla's
    /// "no cap" `-1`).
    ClearInventory {
        item: Option<String>,
        max_count: Option<i32>,
    },
    /// `/setblock` — **always self-targeted** (the executor that produces this
    /// always resolves `ctx.source.uuid()`, never a selector's target), because
    /// delivery needs the chunk source and block-tick feed that only the acting
    /// connection's own `ChatCommand` arm has. Never meaningfully reaches a
    /// *different* connection's queue; see `crate::server`'s handling of this
    /// variant for where it is actually applied, and [`super::registrar`]'s
    /// `apply_own_effect`-equivalent arm for why a stray one there is a no-op
    /// rather than a panic.
    SetBlock {
        pos: (i32, i32, i32),
        block: String,
    },
    /// `/fill` — same self-targeted delivery constraint as [`Self::SetBlock`], one
    /// block id over every position in the (already volume-capped) region.
    Fill {
        positions: Vec<(i32, i32, i32)>,
        block: String,
    },
    /// `/say`, `/me` — a line every connected player should see. Self-targeted for
    /// delivery, like `SetBlock`: it needs the player registry's broadcast, which
    /// no per-uuid effect can express, so it is applied inline by the issuing
    /// connection's own `ChatCommand` arm rather than drained by a target.
    ///
    /// `sender`/`message` rather than one pre-joined line, so delivery can reuse
    /// `PlayerRegistry::say`'s own `<sender> message` rendering
    /// ([`crate::players::ChatLine::rendered`]) instead of inventing a second,
    /// slightly different chat format for commands.
    Broadcast {
        sender: String,
        message: String,
    },
    /// `/spawnpoint` (self form only — see `crate::commands::world_spawn_commands`'s
    /// module doc for why there is no `<targets>` form yet). Self-targeted for
    /// delivery: the connection's own `respawn: &mut Option<RespawnPoint>` local is
    /// reachable only from the issuing connection's own `ChatCommand` arm.
    SetRespawnPoint {
        pos: lodestone_model::BlockPos,
    },
    /// `/tp`/`/teleport` — an ordinary directed effect, unlike `SetBlock`/
    /// `Broadcast`: a teleport can genuinely target *any* connected player, not
    /// just the caller, so it travels the same per-uuid outbox
    /// `SetGameMode`/`Kill` already use.
    ///
    /// `yaw`/`pitch` are `None` to mean "keep the target's current facing" —
    /// resolved at *application* time by whichever connection actually applies
    /// the effect (its own for a self-teleport, the target's own for a
    /// directed one), because that is the only place a live `player_rot` for
    /// that specific connection is ever in scope. A command executor
    /// structurally cannot resolve this itself for anyone but the caller: see
    /// `crate::commands::source::PlayerCandidate`, which carries a position but
    /// no rotation.
    Teleport {
        x: f64,
        y: f64,
        z: f64,
        yaw: Option<f32>,
        pitch: Option<f32>,
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
