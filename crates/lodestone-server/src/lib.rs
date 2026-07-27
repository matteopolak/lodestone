//! Lodestone's integrated server — the singleplayer host.
//!
//! In vanilla, singleplayer *is* an integrated server the client connects to.
//! Lodestone adopts that shape deliberately (plan §8): the integrated server
//! speaks the **same** [`lodestone_net::Connection`] over a [`Transport`], so
//! singleplayer and multiplayer exercise the same code path and open-to-LAN
//! falls out for free. Over an in-memory
//! [`memory_pair`](lodestone_net::memory_pair) duplex the client and server run
//! in one process; swap the transport for a `TcpStream` and the identical loop
//! serves LAN.
//!
//! # What is version-free here, and what is a seam
//!
//! This crate is **version-free**, exactly like [`lodestone_worldgen`]. It owns:
//!
//! * [`ChunkSource`] — how the server obtains terrain for a chunk column.
//!   [`OverworldChunkSource`] (built by [`overworld_chunk_source`]) backs it
//!   with the composed, JVM-verified overworld generator, so a served chunk
//!   carries real vanilla block states; [`WorldgenChunkSource`] is a
//!   solidity-only stand-in kept only for the transport tests.
//! * [`ServerProtocol`] — the **seam** a protocol/version crate must implement
//!   to lower client-bound packets and lift server-bound ones. It is the mirror
//!   of the client's `VersionAdapter`: this crate never names a wire format,
//!   packet id, or NBT layout, so dropping a version drops its adapter, never
//!   this loop.
//! * [`serve_connection`] — the generic driver that runs the handshake → login
//!   → play sequence over any [`Transport`] using a [`ServerProtocol`] and a
//!   [`ChunkSource`].
//! * [`IntegratedServer`] — the reachable lifecycle wrapper a shell holds to
//!   *start* singleplayer (in-memory) or open-to-LAN (TCP), with a clean
//!   shutdown that never leaks the serving task.
//! * [`MobSim`] / [`ChunkWorld`] — the server-side mob simulation. In vanilla
//!   the *server* ticks mob AI and streams positions; the client interpolates
//!   and runs none. So this crate is mob AI's home: [`ChunkWorld`] adapts the
//!   version-free solid/air terrain into `lodestone-entity`'s `PathWorld` seam,
//!   and [`MobSim`] ticks goal-driven `NavigatingMob`s over it. Streaming the
//!   result to a client is a separate (encoder) seam.
//!
//! Wiring a real vanilla client to this server end-to-end requires the version
//! crate to provide client-bound *encoders* (join game, registry data,
//! `level_chunk_with_light`) and server-bound *decoders*. The client stack is
//! decode-only today, so that encoder half is a reported seam (see the crate
//! README notes / task report), not something this version-free crate may
//! implement itself without coupling to a protocol number.
//!
//! [`Transport`]: lodestone_net::Transport

mod chunk;
mod integrated;
mod mob_spawn;
mod mobs;
mod protocol;
mod server;
mod spawn;
mod worldgen_data;

pub use chunk::{ChunkColumn, ChunkSource, OverworldChunkSource, WorldgenChunkSource};
pub use integrated::IntegratedServer;
pub use mob_spawn::{
    DespawnOutcome, MAGIC_NUMBER, MobCategory, SpawnCandidate, SpawnCandidateSource, SpawnRng,
    SpawnState, check_despawn, resolve_mob_shape,
};
pub use mobs::{ChunkWorld, MobSim, SimMob};
pub use protocol::{EntitySnapshot, ServerBound, ServerDirective, ServerProtocol};
pub use server::{EntitySource, NoEntities, ServeSummary, ServerError, serve_connection};
pub use worldgen_data::{overworld_chunk_source, overworld_generator};

// Re-exported so a caller (e.g. the shell's local world) can name the generator
// and its output without depending on `lodestone-worldgen` directly.
pub use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};
