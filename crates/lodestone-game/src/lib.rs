//! # lodestone-game
//!
//! The version-free **game-state** layer: everything that makes a Minecraft
//! client a *game* rather than a packet reader. This crate owns the canonical
//! state and the actions that mutate it; version-specific packet shapes live in
//! the protocol crates and are lowered into these types by adapters.
//!
//! ## Design rules (see the project plan §3)
//!
//! * **Version-free.** No dependency on any version crate. State is keyed by
//!   [`Identifier`](lodestone_model::Identifier)s and canonical enums, never
//!   numeric ids. The canonical model is designed on the *newest* concept
//!   (item components, the action-bitmask player list, the container state-id
//!   reconciliation) and older protocols translate *upward* into it.
//! * **Server-authoritative.** Container interaction predicts locally and
//!   reconciles against the server; the predict/reconcile seam is explicit in
//!   [`reconcile`], not assumed away.
//!
//! ## Modules
//!
//! * [`item`] — the version-free [`ItemStack`](item::ItemStack) and its
//!   components.
//! * [`container`] / [`menu`] — slot-indexed containers and the dual
//!   (player-native vs. menu) slot indexing.
//! * [`click`] — the full container click state machine, including the
//!   multi-stage drag-distribute protocol.
//! * [`reconcile`] — the optimistic predict-then-reconcile seam.
//! * [`recipe`] — shaped/shapeless/… recipes, tag resolution, and grid
//!   matching. JSON loading of Mojang data lives in [`recipe_json`] behind the
//!   `json` feature.
//! * [`scoreboard`] — objectives, scores, display slots, and teams.
//! * [`tablist`] — player info / tab list.
//! * [`player_state`] — HUD vitals, experience, game mode, difficulty, respawn,
//!   and the title / subtitle / action-bar system.
//! * [`bossbar`] — boss bars.
//! * [`progress`] — advancements and statistics.

pub mod bossbar;
pub mod chat;
pub mod chat_ack;
pub mod click;
pub mod container;
pub mod effect;
pub mod hud;
pub mod item;
pub mod menu;
pub mod player_state;
pub mod progress;
pub mod recipe;
#[cfg(feature = "json")]
pub mod recipe_json;
pub mod reconcile;
pub mod scoreboard;
pub mod tablist;
