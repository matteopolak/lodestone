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
//! * [`mining`] — block breaking, the dig state machine and item pickup.
//! * [`placement`] — block placement / item use: the interaction-vs-placement
//!   ordering, target resolution, geometry-derived block state, and its own
//!   predict-then-reconcile seam.
//! * [`recipe`] — shaped/shapeless/… recipes, tag resolution, grid matching,
//!   and the [`RecipeBook`](recipe::RecipeBook) corpus aggregate. Loading
//!   Mojang's datapack JSON lives in [`recipe_json`] behind the `json` feature.
//!   Note the division of labour: an open crafting menu's **result slot is the
//!   server's**, pushed as a `container_set_slot` and reconciled like any other
//!   slot; the book is for the recipe UI, ghosts, and prediction.
//! * [`scoreboard`] — objectives, scores, display slots, and teams.
//! * [`tablist`] — player info / tab list.
//! * [`player_state`] — HUD vitals, experience, game mode, difficulty, respawn,
//!   and the title / subtitle / action-bar system.
//! * [`bossbar`] — boss bars.
//! * [`progress`] — advancements and statistics.

pub mod advancement;
pub mod bossbar;
pub mod chat;
pub mod chat_ack;
pub mod click;
pub mod container;
pub mod custom_item;
pub mod effect;
pub mod hud;
pub mod item;
pub mod levelstate;
pub mod maps;
pub mod menu;
pub mod menus;
pub mod mining;
pub mod player_state;
pub mod placement;
pub mod progress;
pub mod recipe;
#[cfg(feature = "json")]
pub mod recipe_json;
pub mod reconcile;
pub mod scoreboard;
pub mod tablist;
pub mod text;
pub mod worldborder;
