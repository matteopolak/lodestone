//! The server `World`'s schedules and the public `SystemSet` anchors a
//! server-side plugin orders against (`docs/server-ecs.md`,
//! `docs/plans/server-ecs-migration.md` Phase 0).
//!
//! # Why these are the server's own labels, not `lodestone_ecs`'s
//!
//! `lodestone-ecs` publishes types with three of these exact names
//! (`NetIngest`, `GameTick`, `TickSet`). They are **not** reused here, and the
//! reason is the first decision in `docs/server-ecs.md`: there are two
//! `World`s, never one. A `ScheduleLabel` is a key into *one* `World`'s
//! `Schedules` resource, so sharing the type would buy nothing at runtime —
//! the server's `GameTick` and the client's `GameTick` can never be the same
//! `Schedule` value even if they are the same label type, because they live in
//! different `World`s.
//!
//! What sharing the type *would* buy is one import path for a plugin author,
//! and what it would cost is the whole client vocabulary
//! (`LocalPlayer`, `FrameClock`, `SessionMenus`, …) landing in
//! `lodestone-server`'s dependency graph — and in the browser bundle, which
//! links `lodestone-server` and does not link `lodestone-ecs` today. The
//! decision record already names the fix for that (splitting `lodestone-ecs`
//! into a substrate crate and a client-vocabulary crate) and already calls it
//! a **follow-up, not a prerequisite**. So Phase 0 takes the two bevy crates
//! and nothing else; if and when that split lands, `TickSet` below becomes a
//! re-export of the substrate crate's and no plugin has to change the set it
//! names.
//!
//! # Deliberately no `Extract`, and deliberately no frame anything
//!
//! `lodestone_ecs::CorePlugin` also installs `Update`'s
//! `FrameSet::{Input, Interpolate, Camera, Terrain}` chain and an `Extract`
//! schedule. Neither has a server-side meaning: there is no frame, no camera,
//! and no render loop to extract *to* — open-to-LAN has no render loop at all,
//! which is the premise the whole two-`World` decision rests on. See
//! [`crate::ecs::plugin::ServerCorePlugin`]'s own doc for the three resources
//! `CorePlugin` would smuggle in and why one of them (`LockHolds`) would be
//! worse than merely wrong.

use bevy_ecs::schedule::{ScheduleLabel, SystemSet};

/// Runs exactly **once**, when [`crate::ecs::ServerApp::bootstrap`] builds the
/// server's `World` — the server-side analogue of bevy's `Startup`, which
/// `App::empty()` does not provide (it comes with `MainSchedulePlugin`, and
/// installing that would drag in `Main`/`Update`, i.e. frame-shaped schedules
/// the server must not have).
///
/// # This is Phase 0's island detector, not decoration
///
/// `WindowApp.ecs` on the client (issue #37) is an `App` that is constructed
/// and never has a schedule run against it — "an inert scaffold nothing
/// reads", still open. A Phase 0 that only *constructed* an `App` would be
/// bit-for-bit the same defect. So this schedule exists, production runs it
/// (`crate::IntegratedServer::open_in_memory_with_mobs`), one system is
/// registered in it, and [`crate::ecs::schedule_runs`] counts the executions
/// so a test can assert the production path actually did it. Deleting either
/// the registration or the `run_schedule` call makes that assertion fail —
/// see `plugin.rs`'s test module.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ServerBoot;

/// Drains inbound, already-decoded [`crate::ServerBound`] traffic into the
/// `World`. Empty in Phase 0; Phase 2 fills it with the proposal queue.
///
/// Separate from [`GameTick`] because a connection task enqueues at whatever
/// rate its socket delivers, while the world advances at 20 Hz — one schedule
/// per cadence, matching `lodestone-ecs`'s own split.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct NetIngest;

/// The 20 Hz world tick. Phase 1 makes `crate::tick::run_tick_loop` run this
/// once per iteration; Phase 0 only defines it, chains [`TickSet`] inside it,
/// and registers the boot counter so the set chain has a member.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GameTick;

/// [`GameTick`]'s public ordering anchors — the server-side counterpart of
/// `lodestone_ecs::TickSet`, and the reason `docs/server-ecs.md`'s
/// "adjudication window" argument is expressible at all.
///
/// # The chain is the point, and [`TickSet::Adjudicate`] is why
///
/// Today a connection task calls `source.set_block(...)` inline from
/// `dispatch_play_packet` (`crate::server`'s `apply_block_action`), so by the
/// time anything could object, the block is already set — there is nowhere to
/// put a veto. Once packet-apply is a system, [`TickSet::Adjudicate`] sits
/// between [`TickSet::Drain`] and [`TickSet::Apply`], and a protection plugin,
/// an economy plugin or a minigame manager gets a place in the schedule to say
/// no *before* a proposal becomes world state. That ordering is the whole
/// architectural argument for this migration; Phase 2 populates it.
///
/// # Server-side, the plugin outranks the client
///
/// This is clause 4 of `docs/plugin-api.md` inverted, and it is the one clause
/// that genuinely changes across the seam. Client-side the human at the
/// keyboard is ground truth and outranks installed intent. Server-side there
/// *is* no local human: a remote client's input arrives here as a **proposal**,
/// and the plugin is precisely the thing entitled to overrule it.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TickSet {
    /// Move queued proposals from the connection tasks' channel into the
    /// `World`. Runs first; nothing in it may read a decision.
    Drain,
    /// Where a veto happens. Everything here observes proposals and may cancel
    /// them; nothing here may apply one.
    Adjudicate,
    /// Applies whatever survived [`TickSet::Adjudicate`] to authoritative
    /// state. Single-writer per piece of state, per clause 2.
    Apply,
    /// Advances simulation that needs no proposal at all — mob AI, block
    /// entities, random ticks, scheduled ticks. This is where the bulk of
    /// today's `run_tick_loop` body lands in Phase 1.
    Simulate,
    /// Publishes snapshots for the connection tasks to diff against
    /// (`LiveMobSource`, `BlockTickFeed`). Read-only with respect to
    /// simulation state; runs last so a connection never observes a
    /// half-applied tick.
    Publish,
}

/// [`NetIngest`]'s ordering anchors. Mirrors `lodestone_ecs::IngestSet`'s
/// shape (drain → apply → index) because the problem is the same one: bytes
/// have already been decoded elsewhere, and the `World` needs them folded in
/// before anything derived from them is rebuilt.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IngestSet {
    /// Takes decoded [`crate::ServerBound`] values off the inbound channel.
    Drain,
    /// Folds them into `World` state.
    Apply,
    /// Rebuilds anything derived from what `Apply` wrote.
    Index,
}
