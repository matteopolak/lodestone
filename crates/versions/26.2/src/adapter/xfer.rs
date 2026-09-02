//! The `transfer` tracing target: a wire-side trace of every event that can
//! rubberband a player across a server switch.
//!
//! # What it is
//!
//! One `tracing` target, `transfer`, carrying a monotonically sequenced record
//! of the four things that decide whether the server accepts our claimed
//! position after it has moved us:
//!
//! | line | emitted by | says |
//! |---|---|---|
//! | `xfer: PLAYER_POSITION` | [`super::player`]'s `handle_player_position` | a teleport arrived, with its id, target and `relatives` mask, and that `ACCEPT_TELEPORTATION` is going out with the same id |
//! | `xfer: move packet` | [`super::V770Adapter::select_move_packet`] | an outbound `move_player_*` reached the wire, the position it claims, and how far that is from the last teleport target we accepted |
//! | `xfer: state` | [`super::connection`] | `START_CONFIGURATION` / `FINISH_CONFIGURATION` / `TRANSFER`, each labelled with the `path` it belongs to |
//! | `xfer: LOGIN` | [`super::chunk`] | a join packet, with its **ordinal on this connection** — the single field that says which path is in play |
//!
//! # Two paths, and they are not the same thing
//!
//! "Being moved to another server" has two mechanisms with almost nothing in
//! common, and every line here carries a `path` field naming which one fired:
//!
//! * **`path = "reconnect"`** — the `minecraft:transfer` packet. The server
//!   asks the client to disconnect and dial a *new address*. Everything
//!   per-connection is rebuilt, this adapter included, so a fresh adapter's
//!   `login_ordinal` is `1` again.
//! * **`path = "backend-swap"`** — a Velocity/BungeeCord proxy keeping **one**
//!   socket and swapping the backend behind it. No `TRANSFER` packet is ever
//!   sent: the client sees `START_CONFIGURATION`, a configuration round, and
//!   then a **second `LOGIN`** on the connection it already had. Every piece of
//!   per-connection state that a reconnect would rebuild is instead carried
//!   over, which is why `login_ordinal > 1` is the field to grep for first.
//!
//! A log with no `TRANSFER` line and a `login_ordinal` of `2` is a backend
//! swap, and settles which of the two the player is actually exercising.
//!
//! The shell emits its own `transfer` lines for the frames either side of the
//! wire (`crate`-external: `lodestone_shell`'s `net.rs`, `sim/net_apply.rs` and
//! `sim/step.rs`), so one filter shows the whole chain.
//!
//! To collect it:
//!
//! ```text
//! RUST_LOG=info,transfer=debug cargo run --release -p lodestone-shell --bin lodestone
//! ```
//!
//! # How it works
//!
//! Every line carries `seq`, from [`next_seq`] — a single process-wide counter,
//! so lines are strictly ordered even when the driver task and the shell's
//! frame thread interleave, and a gap in it is a line the subscriber dropped
//! rather than an event that did not happen. Wall-clock timestamps cannot do
//! that job here: the window this instrument exists to resolve is a fraction of
//! one frame.
//!
//! # The question it answers
//!
//! Vanilla's client applies a teleport, sends `ACCEPT_TELEPORTATION` and sends a
//! `move_player_pos_rot` at the *new* pose, all in one call on one thread
//! (`vanilla's own client packet listener's own handle move player` — transcribed in
//! `docs/transfer-tracing.md`).
//! Ours cannot: the accept is written by the driver the instant the packet
//! decodes, while the pose only reaches the simulation a channel hop and a frame
//! later, and the simulation queues an outbound `Move` every tick from whatever
//! pose it currently holds. A `Move` built before the teleport was applied but
//! written after the accept therefore claims a pre-teleport position at a moment
//! the server has already cleared `awaitingPositionFromClient` — which is the
//! one input `vanilla's own server game packet listener impl's own handle move player` answers with
//! *"moved wrongly!"* and a corrective teleport.
//!
//! [`super::V770Adapter::select_move_packet`] therefore reports
//! `moves_since_teleport` and `dist_from_teleport` on every outbound movement
//! packet, and escalates to `warn` when the *first* move after a teleport lands
//! far from that teleport's target. That line is the hypothesis, stated in the
//! log rather than in a doc: if it appears, the race happened on that run.
//!
//! # How to change it
//!
//! The distance verdict needs an absolute target to measure against, so
//! [`AcceptedTeleport`] is only recorded for a teleport whose `relatives` mask
//! is empty — the transfer/respawn/anti-cheat-correction shape. A relative
//! teleport still logs its own line (with the mask) but leaves the previous
//! target in place rather than inventing one this crate cannot resolve: the
//! adapter holds no player position of its own.
//!
//! Keep every message prefixed `xfer:`. The shell's default subscriber is built
//! with `.with_target(false)`, so the target name does not reach the output and
//! the prefix is the only thing a `grep` of a user-supplied log can key on.

use std::sync::atomic::{AtomicU64, Ordering};

use lodestone_model::Vec3;

/// Process-wide ordering counter for the `transfer` target. See the module doc.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Returns the next `transfer`-target sequence number.
///
/// `Relaxed` is deliberate and sufficient: the counter's only job is to totally
/// order the lines *within* the log, and each value is read once, into the line
/// that consumes it. Nothing else synchronises on it.
pub(crate) fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// The last fully-absolute teleport this connection accepted — the yardstick
/// [`super::V770Adapter::select_move_packet`] measures an outbound movement
/// packet's claimed position against.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AcceptedTeleport {
    /// The `transfer`-target [`next_seq`] value of the line that recorded it,
    /// so a move's log line points back at the exact teleport it is measured
    /// against rather than at "the last one, probably".
    pub(crate) seq: u64,
    /// The wire teleport id we echoed in `ACCEPT_TELEPORTATION`.
    pub(crate) id: i32,
    /// The absolute position the server placed us at.
    pub(crate) target: Vec3,
}

impl AcceptedTeleport {
    /// Distance from `pos` to the teleport target.
    pub(crate) fn distance_to(&self, pos: Vec3) -> f64 {
        let dx = pos.x - self.target.x;
        let dy = pos.y - self.target.y;
        let dz = pos.z - self.target.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

/// How far an outbound move may sit from the teleport target that immediately
/// precedes it before the instrument calls it out.
///
/// One tick of ordinary movement is well under half a block (sprint-jumping
/// tops out around `0.4`), so a first post-teleport move beyond a block did not
/// come from a simulation that had adopted the teleport. Deliberately a
/// *diagnostic* threshold — nothing branches on it but the log level.
pub(crate) const STALE_MOVE_BLOCKS: f64 = 1.0;
