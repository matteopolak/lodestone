//! Version-free clientbound event model, organized by payload domain.

//! The private submodules keep the public `lodestone_model::event::*` surface
//! stable while making the large event vocabulary easier to navigate. New
//! clientbound carriers belong in `client.rs`; their supporting payload types
//! should live beside the subsystem that owns their semantics.

#[path = "event/chat.rs"]
mod chat;
#[path = "event/client.rs"]
mod client;
#[path = "event/entity.rs"]
mod entity;
#[path = "event/routing.rs"]
mod routing;
#[path = "event/scoreboard.rs"]
mod scoreboard;
#[path = "event/session.rs"]
mod session;
#[path = "event/world.rs"]
mod world;

pub use chat::*;
pub use client::*;
pub use entity::*;
pub use routing::*;
pub use scoreboard::*;
pub use session::*;
pub use world::*;
