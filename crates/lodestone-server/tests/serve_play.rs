//! Hermetic proof of the three things that make a served [`State::Play`]
//! connection survive, keep time, and follow the player, over the same
//! in-memory `Connection`/`Transport` path `integrated_memory.rs` already
//! exercises for the join sequence.
//!
//! This uses a small stand-in [`ServerProtocol`] with its own wire format, so
//! the assertions here are about `lodestone-server`'s own scheduling logic
//! (`ViewTracker`, `serve_play`'s keep-alive/time-sync timers), not about
//! wire-layout fidelity.
//!
//! # Controls
//!
//! An assertion of an absence needs a control proving the detector actually
//! fires:
//!
//! * [`silent_client_is_disconnected_after_keep_alive_timeout`] is the
//!   **positive** control — the keep-alive mechanism actually firing and
//!   disconnecting a genuinely unresponsive peer.
//! * [`responsive_client_survives_multiple_keep_alive_intervals`] is the
//!   **negative** control run against the *same* mechanism — a peer that
//!   answers every challenge must **not** be disconnected, across several
//!   intervals, not just one.
//!
//! Both run under `#[tokio::test(start_paused = true)]`: the 15-second
//! keep-alive interval and 1-second time-sync interval are real
//! `tokio::time` intervals, but with the clock paused and auto-advancing
//! whenever the runtime is otherwise idle, both tests resolve in a fraction
//! of a second of wall-clock time — the same pattern already established in
//! `crates/lodestone-net/src/connection.rs`'s own
//! `read_packet_timeout_fires_when_peer_is_silent` test.

mod common;
mod join;
mod connection;
mod vitals;
mod streaming;
mod breaking;
mod pickup;
mod ban;
mod encoding;
mod misc;
