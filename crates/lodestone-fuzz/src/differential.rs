//! Track B of issue #549: the tick-aligned differential-fuzzing harness
//! skeleton.
//!
//! ## Scope, stated honestly
//!
//! This module is the harness — the tick-alignment mechanism and the
//! comparison loop — plus one concrete oracle ([`RconOracle`], gated behind
//! the `rcon-oracle` feature) that drives *any* Source-RCON endpoint,
//! vanilla or our own [`lodestone_server::IntegratedServer`] alike, since
//! both speak the same RCON wire format. What this module does **not** do:
//!
//! - **No generation or shrinking over the action alphabet.** [`Script`] is a
//!   fixed, hand-written sequence today. Issue #549's own suggested order
//!   puts this *after* "prove the harness agrees on a script with no known
//!   divergence" — step 2 of 5 — and that is as far as this session got.
//!   `arbitrary`'s derive is the bridge the issue names for step 3; nothing
//!   here depends on it yet.
//! - **No validation against a reverted historical fix.** The issue's own
//!   "Validation" section — revert a committed fix in a scratch worktree and
//!   require the fuzzer to rediscover it — needs a corpus of actions rich
//!   enough to reach the reverted code path, which needs step 3 first.
//! - **No client-state comparison.** Only the two-`WorldOracle`, block-state
//!   comparison half exists; the issue's "on the client half" section is
//!   entirely unaddressed.
//! - **Reading arbitrary block state over vanilla RCON has no general
//!   primitive.** `/data get block <pos>` only answers for a block **with**
//!   a block entity ("The target block has no tile data" otherwise), so
//!   there is no vanilla RCON command that returns "what block state is at
//!   this position" for an arbitrary block. [`RconOracle::block_state`]
//!   therefore reads by **probing a caller-supplied candidate list** with
//!   `execute if block <pos> <candidate>` — exactly the technique
//!   `crate::redstone_oracle_gate` (in `lodestone-server`, read-only
//!   reference here, not edited) already established as the correct way to
//!   get an exact state read rather than a sampled one. This means today's
//!   harness can only compare positions whose *possible* resulting states
//!   the caller enumerates up front — a real, narrowing limitation, not an
//!   implementation gap to paper over. A future version could add a custom
//!   data-pack function or a block-entity round-trip trick for full
//!   generality; neither is built here.
//! - **Tick alignment between two independently-running server processes has
//!   no shared "step" primitive either.** Neither side implements a
//!   `/tick step`/`/tick sprint`-equivalent pause/resume today (`lodestone
//!   -server` has no `/tick` command at all yet, per a grep of
//!   `crates/lodestone-server/src/commands/` done while writing this), and
//!   vanilla's own `/tick step` famously does not advance scheduled block
//!   ticks (`docs/fuzz-harness.md`'s sibling record, and
//!   `redstone_oracle_gate.rs`'s own module doc, both name this trap).
//!   [`RconOracle::advance_tick`] instead sleeps one real tick interval
//!   (`TICK_MILLIS`, matching vanilla's own 50 ms) and lets each side's
//!   already-running tick loop advance on its own — the same "real time,
//!   never `tick step`" discipline `redstone_oracle_gate.rs` already uses for
//!   its own timing measurements. This bounds tick alignment to "close
//!   enough that both sides have had at least one real tick", not "the exact
//!   same tick count provably elapsed on both processes" — a real
//!   imprecision, named rather than hidden.
//!
//! ## What is real and load-bearing here
//!
//! [`run_differential`] is the actual comparison loop the issue's design
//! insists on: it compares **every tick**, not just the end state, and
//! returns the **first** diverging tick/position/pair of values rather than
//! aggregating — "comparing only the final state loses the signal that
//! localises the bug" (issue #549). `tests/differential_harness_self_check.rs`
//! proves this against two in-memory fake oracles (no server, no
//! network, runs in milliseconds): an identical script against two fresh
//! fakes finds no divergence, and a script that diverges on the fake's
//! *n*th tick is caught at exactly tick *n*, not merely "eventually" — the
//! tick-localisation property this whole module exists for.

use std::time::Duration;

/// One action applied to a world. Deliberately small today — issue #549's
/// suggested biasing ("waterloggable blocks, fluids, redstone, containers,
/// falling blocks, pistons") all reduce to a `SetBlock` plus letting the
/// server's own tick loop react, which is why this alphabet starts here
/// rather than modelling every vanilla command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `/setblock x y z <state>` (or the equivalent direct world-sink call
    /// for a non-RCON oracle). `state` is a full blockstate string
    /// (`"minecraft:water[level=0]"`), matching the format
    /// `ChunkSource::block_state` already returns elsewhere in this
    /// workspace.
    SetBlock { pos: (i32, i32, i32), state: String },
    /// An escape hatch for anything not worth its own `Action` variant yet —
    /// a raw command string, sent verbatim to an oracle that supports one
    /// (RCON does; a hermetic fake may simply refuse it). Kept separate from
    /// `SetBlock` rather than folding everything into "run a command" so a
    /// non-command-based oracle (a future in-process `ChunkSource` oracle,
    /// say) can still implement the common case.
    RunCommand(String),
}

/// One [`Action`], scheduled to apply at a specific tick — the unit
/// [`Script`] is built from, and the unit [`run_differential`] applies in
/// lockstep to both sides before ticking either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptStep {
    pub tick: u64,
    pub action: Action,
}

/// A fixed, ordered action sequence. Not yet `arbitrary`-generated — see this
/// module's doc for why.
#[derive(Debug, Clone, Default)]
pub struct Script {
    pub steps: Vec<ScriptStep>,
}

impl Script {
    #[must_use]
    pub fn new(steps: Vec<ScriptStep>) -> Self {
        Self { steps }
    }

    /// The last tick any step is scheduled at, or 0 for an empty script.
    #[must_use]
    pub fn last_tick(&self) -> u64 {
        self.steps.iter().map(|s| s.tick).max().unwrap_or(0)
    }

    /// Every step scheduled at exactly `tick`, in script order.
    fn steps_at(&self, tick: u64) -> impl Iterator<Item = &Action> {
        self.steps.iter().filter(move |s| s.tick == tick).map(|s| &s.action)
    }
}

/// One side of a differential comparison: something that can have an
/// [`Action`] applied to it, be ticked forward by one, and answer "what is
/// the block state at this position" for a position the caller names.
///
/// `block_state`'s `candidates` parameter is the reading limitation this
/// module's doc explains: an oracle with no general block-state-read
/// primitive (real vanilla RCON) can only tell you which of a supplied list
/// the position currently matches, not enumerate the state itself. An oracle
/// with a real read primitive (a future in-process `ChunkSource` oracle) is
/// free to ignore `candidates` and return the true state directly — the
/// trait does not require probing, only permits it.
pub trait WorldOracle {
    type Error: std::fmt::Display;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error>;
    fn advance_tick(&mut self) -> Result<(), Self::Error>;
    /// Returns the state string at `pos` if it matches any of `candidates`
    /// (checked in order, first match wins), or `Ok(None)` if it matches
    /// none of them. `Err` means the oracle itself failed (a dropped
    /// connection, a malformed response) — a genuinely different case from
    /// "queried cleanly and the state was not in `candidates`".
    fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error>;
}

/// Where two oracles disagreed: the first tick, position and pair of
/// observed values `run_differential` found — never an aggregate, per this
/// module's whole reason for existing (CLAUDE.md: "comparing only the final
/// state loses the signal that localises the bug").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub tick: u64,
    pub pos: (i32, i32, i32),
    /// `None` means "did not match any candidate", distinct from a specific
    /// wrong state — both are worth reporting differently.
    pub left: Option<String>,
    pub right: Option<String>,
}

/// A single oracle-level failure (as opposed to a state disagreement),
/// tagged with which side and at which tick it happened, so a caller can
/// tell "the two servers disagree" from "one server's connection broke".
#[derive(Debug, Clone)]
pub struct OracleFailure {
    pub tick: u64,
    pub side: Side,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub enum DifferentialOutcome {
    /// Every tick up to and including `script.last_tick()` (plus
    /// `settle_ticks` trailing ticks with no further actions, letting a
    /// delayed reaction like a torch's 2-tick inversion settle) compared
    /// equal at every position in `region`.
    Agreed,
    Diverged(Divergence),
    OracleFailed(OracleFailure),
}

/// Runs `script` against `left` and `right` in lockstep, comparing every
/// position in `region` (each entry a position plus the candidate states
/// worth probing there, per [`WorldOracle::block_state`]'s doc) after every
/// tick, and returning the *first* disagreement rather than collecting all
/// of them — the tick-localisation property issue #549 asks for by name.
///
/// Runs `script.last_tick() + settle_ticks` ticks total: actions scheduled
/// for a tick are applied to **both** sides, in script order, before either
/// side is ticked, then both are advanced one tick, then every `region`
/// entry is compared. `settle_ticks` exists because a real mechanic can
/// react on a delay (a redstone torch's `TOGGLE_DELAY = 2`, per
/// `redstone_oracle_gate.rs`) — comparing only through the last scheduled
/// action would miss a divergence that only manifests after it.
pub fn run_differential<L: WorldOracle, R: WorldOracle>(
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    left: &mut L,
    right: &mut R,
    settle_ticks: u64,
) -> DifferentialOutcome {
    let total_ticks = script.last_tick() + settle_ticks;

    for tick in 0..=total_ticks {
        for action in script.steps_at(tick) {
            if let Err(e) = left.apply(action) {
                return DifferentialOutcome::OracleFailed(OracleFailure {
                    tick,
                    side: Side::Left,
                    message: e.to_string(),
                });
            }
            if let Err(e) = right.apply(action) {
                return DifferentialOutcome::OracleFailed(OracleFailure {
                    tick,
                    side: Side::Right,
                    message: e.to_string(),
                });
            }
        }

        if let Err(e) = left.advance_tick() {
            return DifferentialOutcome::OracleFailed(OracleFailure {
                tick,
                side: Side::Left,
                message: e.to_string(),
            });
        }
        if let Err(e) = right.advance_tick() {
            return DifferentialOutcome::OracleFailed(OracleFailure {
                tick,
                side: Side::Right,
                message: e.to_string(),
            });
        }

        for (pos, candidates) in region {
            let left_state = match left.block_state(*pos, candidates) {
                Ok(s) => s,
                Err(e) => {
                    return DifferentialOutcome::OracleFailed(OracleFailure {
                        tick,
                        side: Side::Left,
                        message: e.to_string(),
                    });
                }
            };
            let right_state = match right.block_state(*pos, candidates) {
                Ok(s) => s,
                Err(e) => {
                    return DifferentialOutcome::OracleFailed(OracleFailure {
                        tick,
                        side: Side::Right,
                        message: e.to_string(),
                    });
                }
            };
            if left_state != right_state {
                return DifferentialOutcome::Diverged(Divergence {
                    tick,
                    pos: *pos,
                    left: left_state,
                    right: right_state,
                });
            }
        }
    }

    DifferentialOutcome::Agreed
}

/// One vanilla tick, for an oracle that aligns by sleeping rather than by a
/// shared step command — see this module's doc for why there is no better
/// mechanism today.
pub const TICK_MILLIS: Duration = Duration::from_millis(50);

#[cfg(feature = "rcon-oracle")]
pub mod rcon {
    //! [`RconOracle`]: a [`super::WorldOracle`] over any Source-RCON
    //! endpoint. Works against real vanilla (`scripts/live-oracles/*.sh`) and
    //! against our own [`lodestone_server::IntegratedServer::start_rcon`]
    //! identically, since both speak the same wire protocol — this is what
    //! lets one oracle type serve both sides of the comparison.
    use super::{Action, WorldOracle};
    use lodestone_testsupport::RconClient;
    use std::net::ToSocketAddrs;

    pub struct RconOracle {
        client: RconClient,
        /// Prefixed to every position this oracle touches, so two oracles
        /// backed by the *same* running world can be driven side by side at
        /// disjoint coordinate ranges without one script's actions
        /// clobbering the other's — see `docs/fuzzing.md`'s self-consistency
        /// proof, which does exactly this against one live oracle.
        origin: (i32, i32, i32),
    }

    impl RconOracle {
        /// Connects and authenticates. `origin` is added to every position an
        /// [`Action`]/`block_state` query names, so the same script can run
        /// at two disjoint locations in one shared world.
        pub fn connect<A: ToSocketAddrs>(addr: A, password: &str, origin: (i32, i32, i32)) -> std::io::Result<Self> {
            Ok(Self {
                client: RconClient::connect(addr, password)?,
                origin,
            })
        }

        fn world_pos(&self, pos: (i32, i32, i32)) -> (i32, i32, i32) {
            (self.origin.0 + pos.0, self.origin.1 + pos.1, self.origin.2 + pos.2)
        }
    }

    impl WorldOracle for RconOracle {
        type Error = std::io::Error;

        fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
            match action {
                Action::SetBlock { pos, state } => {
                    let (x, y, z) = self.world_pos(*pos);
                    self.client.command(&format!("setblock {x} {y} {z} {state}"))?;
                    Ok(())
                }
                Action::RunCommand(cmd) => {
                    self.client.command(cmd)?;
                    Ok(())
                }
            }
        }

        fn advance_tick(&mut self) -> Result<(), Self::Error> {
            // Real time, deliberately never `/tick step` — see this module's
            // top-level doc for why that command cannot be trusted to
            // advance a scheduled block tick.
            std::thread::sleep(super::TICK_MILLIS);
            Ok(())
        }

        fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
            let (x, y, z) = self.world_pos(pos);
            for candidate in candidates {
                let response = self
                    .client
                    .command(&format!("execute if block {x} {y} {z} {candidate} run say match"))?;
                // Vanilla's `execute if` prints nothing (empty success
                // response with no feedback) when the condition is false and
                // runs the attached `say` (echoing back "match") when it is
                // true — probing one candidate at a time, first hit wins,
                // exactly `redstone_oracle_gate.rs`'s own technique.
                if response.contains("match") {
                    return Ok(Some(candidate.clone()));
                }
            }
            Ok(None)
        }
    }
}
