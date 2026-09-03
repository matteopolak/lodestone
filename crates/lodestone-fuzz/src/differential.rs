//! The tick-aligned differential-fuzzing harness: run one action script
//! against two worlds and report the **first tick** at which they disagree.
//!
//! ## What is real and load-bearing here
//!
//! [`run_differential`] compares **every tick**, not just the end state, and
//! returns the **first** diverging tick/position/pair of values rather than
//! aggregating — comparing only the final state loses the signal that
//! localises the bug, and the two most instructive bug classes here (a fluid
//! spreading at the wrong rate, a piston committing on the wrong tick) are
//! *timing* bugs that agree on the final state.
//! `tests/differential_harness_self_check.rs` proves the loop against two
//! in-memory fake oracles (no server, no network, runs in milliseconds): an
//! identical script against two fresh fakes finds no divergence, and a script
//! that diverges on the fake's *n*th tick is caught at exactly tick *n*, not
//! merely "eventually".
//!
//! Two concrete oracles exist:
//!
//! * [`rcon::RconOracle`] (behind the `rcon-oracle` feature) drives any
//!   Source-RCON endpoint, so it works against a real vanilla server and
//!   against our own [`lodestone_server::IntegratedServer::start_rcon`] with
//!   the same code.
//! * [`fluid::FluidModelOracle`] drives this workspace's fluid model
//!   in-process through the same production entry point the world tick loop
//!   drains its scheduled block ticks into.
//!
//! Pairing those two is what `tests/differential_live_fluid_spread.rs` does,
//! and it is the comparison that actually measures us against vanilla.
//!
//! ## The two reading and timing constraints, measured
//!
//! - **Reading arbitrary block state over vanilla RCON has no general
//!   primitive.** `/data get block <pos>` only answers for a block **with** a
//!   block entity ("The target block has no tile data" otherwise), so there
//!   is no vanilla RCON command that returns "what block state is at this
//!   position" for an arbitrary block. [`rcon::RconOracle::block_state`]
//!   therefore reads by **probing a caller-supplied candidate list** with the
//!   terminal `execute if block <pos> <candidate>` form, whose feedback is
//!   `Test passed`/`Test failed`. So a comparison can only cover positions
//!   whose possible resulting states the caller enumerates up front — a real,
//!   narrowing limitation. [`state_matches`] gives the in-process side the
//!   same subset-matching semantics so both sides answer in one alphabet.
//! - **Neither side has a usable single-tick step primitive, and vanilla's
//!   `/tick step` is measurably not one.** Measured on a live 26.2 server
//!   with `pause-when-empty-seconds=0` (so the world was demonstrably not
//!   merely paused), with a control on the same rig in the same run: a water
//!   source in a closed channel advanced its front exactly one cell per 5
//!   ticks under real time, and advanced **zero** cells across 25 consecutive
//!   `/tick freeze` + `/tick step 1` pairs. Scheduled block ticks do not run
//!   under `tick step`, and the world being frozen rather than paused does
//!   not change that.
//!
//!   [`rcon::RconOracle::advance_tick`] therefore sleeps one real tick
//!   interval ([`TICK_MILLIS`], 50 ms) and lets the server's own loop
//!   advance. That bounds an RCON-side comparison to about ±1 tick of
//!   alignment rather than an exact tick count, which is why
//!   [`fluid::FluidModelOracle`] steps *exactly* — putting the whole error
//!   budget on one side instead of two. Measured on the same rig, real-time
//!   alignment was good to well under a tick: cell *N* first read as water at
//!   249·*N* ms across two independent trials, against a 250·*N* ms
//!   prediction.
//!
//! ## What this module does not do yet
//!
//! - **No generation or shrinking over the action alphabet.** [`Script`] is a
//!   fixed, hand-written sequence. `arbitrary`'s derive is the intended
//!   bridge; nothing here depends on it.
//! - **No validation against a reverted historical fix** — revert a committed
//!   fix in a scratch worktree and require the harness to rediscover it. That
//!   needs an action corpus rich enough to reach the reverted code path,
//!   which needs generation first.
//! - **No client-state comparison.** Only the two-`WorldOracle`, block-state
//!   half exists.

use std::time::Duration;

/// One action applied to a world. Deliberately small: the families where
/// divergence has actually been found here — waterloggable blocks, fluids,
/// redstone, containers, falling blocks, pistons — all reduce to a
/// `SetBlock` plus letting the server's own tick loop react, which is why
/// this alphabet starts here rather than modelling every vanilla command.
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
    /// non-command-based oracle ([`fluid::FluidModelOracle`]) can still
    /// implement the common case.
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
/// with a real read primitive ([`fluid::FluidModelOracle`]) still answers in
/// the candidate alphabet, via [`state_matches`], so both sides of a
/// comparison speak the same language — the trait does not require probing,
/// only permits it.
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
/// module's whole reason for existing: comparing only the final state loses
/// the signal that localises the bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The **0-based index of the tick that had just been run**, so `0` means
    /// "after exactly one elapsed tick": [`run_differential`] applies the
    /// steps scheduled for tick *T*, runs tick *T*, then compares. A
    /// divergence reported at `tick: 0` for a script whose only action is
    /// scheduled at tick 0 therefore says the two sides already disagreed on
    /// the very first tick either of them ran, not before any of them did.
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
/// of them — the tick-localisation property this module exists for.
///
/// Runs `script.last_tick() + settle_ticks` ticks total: actions scheduled
/// for a tick are applied to **both** sides, in script order, before either
/// side is ticked, then both are advanced one tick, then every `region`
/// entry is compared. `settle_ticks` exists because a real mechanic can
/// react on a delay (a redstone torch inverts two ticks after its input
/// changes) — comparing only through the last scheduled
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
    use std::time::Instant;

    #[derive(Debug)]
    pub struct RconOracle {
        client: RconClient,
        /// Prefixed to every position this oracle touches, so two oracles
        /// backed by the *same* running world can be driven side by side at
        /// disjoint coordinate ranges without one script's actions
        /// clobbering the other's — see `docs/fuzzing.md`'s self-consistency
        /// proof, which does exactly this against one live oracle.
        origin: (i32, i32, i32),
        /// When this oracle's *next* tick is due, on a fixed schedule
        /// anchored at the first [`WorldOracle::advance_tick`] call.
        ///
        /// A fixed `sleep(TICK_MILLIS)` per tick is not the same thing, and
        /// the difference is measurable rather than theoretical: every
        /// `block_state` probe is a round trip that happens *between* two
        /// sleeps, so a comparison probing k positions runs slower than the
        /// server and the server's tick count runs **ahead** of the
        /// harness's, cumulatively. Measured on a three-position,
        /// two-candidate region: a signal whose true arrival is game tick 10
        /// was reported at harness tick 8 — a wrong tick label, and one that
        /// reads exactly like a real timing divergence. Sleeping to a
        /// schedule instead absorbs the probe cost into the same 50 ms
        /// budget the server is using, so the drift does not accumulate.
        next_tick_at: Option<Instant>,
        /// How many ticks were already overdue when they came up — the
        /// instrument's own error count. Non-zero means the probes for one
        /// tick cost more than a tick, so nothing about *when* something
        /// happened can be concluded from that run.
        missed_deadlines: u32,
    }

    impl RconOracle {
        /// Connects and authenticates. `origin` is added to every position an
        /// [`Action`]/`block_state` query names, so the same script can run
        /// at two disjoint locations in one shared world.
        pub fn connect<A: ToSocketAddrs>(addr: A, password: &str, origin: (i32, i32, i32)) -> std::io::Result<Self> {
            Ok(Self {
                client: RconClient::connect(addr, password)?,
                origin,
                next_tick_at: None,
                missed_deadlines: 0,
            })
        }

        fn world_pos(&self, pos: (i32, i32, i32)) -> (i32, i32, i32) {
            (self.origin.0 + pos.0, self.origin.1 + pos.1, self.origin.2 + pos.2)
        }

        /// How many of this oracle's ticks were already overdue when they
        /// came up. **Assert this is zero before believing any tick label
        /// from a comparison this oracle took part in** — see
        /// [`next_tick_at`](Self::next_tick_at) for the measurement that
        /// motivates it.
        #[must_use]
        pub fn missed_deadlines(&self) -> u32 {
            self.missed_deadlines
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
            // advance a scheduled block tick. Sleeping to a schedule rather
            // than for a fixed interval, so probe round trips do not push the
            // harness behind the server — see `next_tick_at`.
            let now = Instant::now();
            let due = self.next_tick_at.unwrap_or(now) + super::TICK_MILLIS;
            if due > now {
                std::thread::sleep(due - now);
                self.next_tick_at = Some(due);
            } else {
                self.missed_deadlines += 1;
                self.next_tick_at = Some(now);
            }
            Ok(())
        }

        fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
            let (x, y, z) = self.world_pos(pos);
            for candidate in candidates {
                // The TERMINAL `execute if block` form, whose own feedback is
                // the literal string `Test passed` or `Test failed`.
                //
                // The `... run say <marker>` variant is measurably useless
                // over RCON and fails in the silent direction: `say`
                // broadcasts to chat and sends the command source no
                // feedback, so an RCON caller gets an EMPTY response body for
                // both the matching and the non-matching case. A probe built
                // that way answers "no candidate matched" for every position
                // in every world, which makes two oracles agree
                // unconditionally — an `Agreed` outcome that measures
                // nothing. Measured against a live 26.2 server, both arms:
                // `run say` returned `''` for a true and a false condition
                // alike, while the terminal form returned `Test passed` /
                // `Test failed` respectively.
                //
                // Anything else coming back (a parse error, a permission
                // refusal, a response from a server that does not implement
                // this command) is an oracle failure, NOT "did not match":
                // conflating the two is how a broken rig reports agreement.
                let response = self.client.command(&format!("execute if block {x} {y} {z} {candidate}"))?;
                let response = response.trim();
                if response.starts_with("Test passed") {
                    return Ok(Some(candidate.clone()));
                }
                if !response.starts_with("Test failed") {
                    return Err(std::io::Error::other(format!(
                        "`execute if block {x} {y} {z} {candidate}` answered {response:?}, \
                         which is neither `Test passed` nor `Test failed`"
                    )));
                }
            }
            Ok(None)
        }
    }
}

/// Does `state` match the state *pattern* `candidate`?
///
/// The two sides of a comparison have to answer in the same alphabet, and the
/// two read primitives are shaped differently: a real vanilla server answers
/// `execute if block <pos> <pattern>`, which matches on the base block name
/// plus **only the properties the pattern spells out**, while an in-process
/// oracle can read the full canonical state string. This function gives the
/// in-process side the vanilla side's matching semantics, so
/// `minecraft:water` matches `minecraft:water[level=3]` on both sides and a
/// pattern naming a property still discriminates on it.
///
/// Property order is irrelevant on both sides — a pattern's pairs are looked
/// up individually rather than compared as a substring.
#[must_use]
pub fn state_matches(state: &str, candidate: &str) -> bool {
    let (state_name, state_props) = split_state(state);
    let (candidate_name, candidate_props) = split_state(candidate);
    if state_name != candidate_name {
        return false;
    }
    candidate_props.iter().all(|(key, value)| {
        state_props
            .iter()
            .any(|(state_key, state_value)| state_key == key && state_value == value)
    })
}

fn split_state(state: &str) -> (&str, Vec<(&str, &str)>) {
    match state.split_once('[') {
        None => (state, Vec::new()),
        Some((name, rest)) => {
            let props = rest
                .strip_suffix(']')
                .unwrap_or(rest)
                .split(',')
                .filter(|pair| !pair.is_empty())
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (k.trim(), v.trim()))
                .collect();
            (name, props)
        }
    }
}

pub mod fluid {
    //! [`FluidModelOracle`]: the *our-side* [`WorldOracle`], driving this
    //! workspace's fluid model through the same production entry point the
    //! world tick loop drains its scheduled block ticks into.
    //!
    //! This is the half that makes the harness compare **us** against vanilla
    //! rather than vanilla against itself. Two properties matter:
    //!
    //! * **Exact tick stepping.** Nothing here sleeps. `advance_tick`
    //!   increments a counter, drains every tick due at that number and runs
    //!   it — so our side's tick numbering is exact, and any imprecision in a
    //!   comparison comes from the other side's real-time alignment alone.
    //!   That asymmetry is deliberate: it puts the whole error budget in one
    //!   place instead of two.
    //! * **A real read primitive.** `block_state` reads the world directly
    //!   and reports which of the caller's candidate patterns matches, via
    //!   [`super::state_matches`] — the same subset-matching semantics a real
    //!   server's own `execute if block` uses, so both sides answer in one
    //!   alphabet.
    //!
    //! The world behind it is a flat rig: a solid floor at a caller-chosen
    //! `y`, air above, and whatever the script writes. It is not a worldgen
    //! world, and that is the point — a differential comparison needs both
    //! sides to start from a shape the caller can build on either, and a
    //! `/fill`ed stone channel is buildable on any vanilla world at any
    //! coordinates.
    use std::collections::HashMap;
    use std::sync::Mutex;

    use lodestone_model::BlockPos;
    use lodestone_server::fluid::{FluidEnv, run_scheduled_tick, ticks_after_edit};
    use lodestone_server::{ChunkColumn, ChunkSource, ScheduledTickQueue};

    use super::{Action, WorldOracle, state_matches};

    /// A sparse block store with a solid floor, sufficient for the fluid
    /// model's reads (`block_state`) and writes (`set_block`).
    #[derive(Debug)]
    struct FlatRig {
        blocks: Mutex<HashMap<(i32, i32, i32), String>>,
        floor_y: i32,
        floor_state: String,
    }

    impl ChunkSource for FlatRig {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            // Never read by the fluid model, which goes through
            // `block_state`/`set_block`. A column is still required by the
            // trait, so answer with an empty one of the right vertical
            // extent rather than a panic that would be reached only if that
            // ever changed.
            ChunkColumn::new(FLUID_MIN_Y, FLUID_HEIGHT / 16)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            if let Some(state) = self.blocks.lock().expect("rig lock").get(&(x, y, z)) {
                return state.clone();
            }
            if y == self.floor_y {
                return self.floor_state.clone();
            }
            "minecraft:air".to_owned()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:plains".to_owned()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.blocks
                .lock()
                .expect("rig lock")
                .insert((x, y, z), name.to_owned());
        }
    }

    /// Vertical extent of the rig, matching an overworld dimension's own
    /// build limits so the fluid model's below-the-world guard behaves as it
    /// does in production rather than being exercised at an unusual `y`.
    const FLUID_MIN_Y: i32 = -64;
    const FLUID_HEIGHT: i32 = 384;

    /// Our side of a differential comparison, over the fluid model.
    #[derive(Debug)]
    pub struct FluidModelOracle {
        rig: FlatRig,
        queue: ScheduledTickQueue<String>,
        tick: u64,
        origin: (i32, i32, i32),
        env: FluidEnv,
    }

    impl FluidModelOracle {
        /// `origin` is added to every position an [`Action`] or a
        /// `block_state` query names, mirroring the RCON oracle's own offset
        /// so one script can be written in relative coordinates and run on
        /// both sides.
        ///
        /// `floor_y` is relative to `origin`: the floor sits at
        /// `origin.1 + floor_y`, so a script writing a fluid at relative
        /// `(0, 0, 0)` rests on a floor built with `floor_y = -1`.
        #[must_use]
        pub fn new(origin: (i32, i32, i32), floor_y: i32, floor_state: &str) -> Self {
            Self {
                rig: FlatRig {
                    blocks: Mutex::new(HashMap::new()),
                    floor_y: origin.1 + floor_y,
                    floor_state: floor_state.to_owned(),
                },
                queue: ScheduledTickQueue::new(),
                tick: 0,
                origin,
                env: FluidEnv::OVERWORLD,
            }
        }

        /// Writes a block WITHOUT scheduling the edit's follow-up ticks — for
        /// building the rig before a script runs, where a `/fill`ed wall must
        /// not be a fluid trigger of its own.
        pub fn place_static(&mut self, pos: (i32, i32, i32), state: &str) {
            let (x, y, z) = self.world_pos(pos);
            self.rig.set_block(x, y, z, state);
        }

        fn world_pos(&self, pos: (i32, i32, i32)) -> (i32, i32, i32) {
            (self.origin.0 + pos.0, self.origin.1 + pos.1, self.origin.2 + pos.2)
        }

        /// The tick number this oracle has advanced to. Exact by
        /// construction, unlike a real-time-aligned oracle's.
        #[must_use]
        pub fn tick(&self) -> u64 {
            self.tick
        }
    }

    impl WorldOracle for FluidModelOracle {
        type Error = std::convert::Infallible;

        fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
            match action {
                Action::SetBlock { pos, state } => {
                    let (x, y, z) = self.world_pos(*pos);
                    self.rig.set_block(x, y, z, state);
                    // The same follow-up ticks a production block edit
                    // schedules, so a script's `SetBlock` behaves like the
                    // set-block path it stands for rather than like a silent
                    // poke at the store.
                    for pending in ticks_after_edit(BlockPos::new(x, y, z)) {
                        self.queue.schedule(
                            pending.pos,
                            pending.kind,
                            self.tick + pending.trigger_tick,
                            pending.priority,
                        );
                    }
                }
                Action::RunCommand(_) => {
                    // No command grammar on this side by design: a command
                    // string is a vanilla-shaped instruction, and reproducing
                    // its parse here would make this oracle a second command
                    // implementation to keep in step. Rig construction goes
                    // through `place_static` instead.
                }
            }
            Ok(())
        }

        fn advance_tick(&mut self) -> Result<(), Self::Error> {
            self.tick += 1;
            let mut changes = Vec::new();
            for entry in self.queue.drain_due(self.tick, usize::MAX) {
                let pos = BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2);
                changes.clear();
                run_scheduled_tick(&self.rig, self.env, pos, &mut self.queue, self.tick, &mut changes);
            }
            Ok(())
        }

        fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
            let (x, y, z) = self.world_pos(pos);
            let state = self.rig.block_state(x, y, z);
            Ok(candidates
                .iter()
                .find(|candidate| state_matches(&state, candidate))
                .cloned())
        }
    }
}

pub mod redstone {
    //! [`RedstoneModelOracle`]: the *our-side* [`WorldOracle`] over this
    //! workspace's redstone model, driven through the same two production
    //! entry points a real session uses — the placement reaction a world edit
    //! runs, and the scheduled block-tick drain the world tick loop runs.
    //!
    //! ## Why this exists next to [`super::fluid`]
    //!
    //! Fluid spread is a single-cell rule iterated by one scheduled kind.
    //! Redstone is not: a signal crosses dust synchronously, waits `2d` game
    //! ticks at a repeater, and re-enters the cascade from a *drained* tick
    //! rather than from the edit. Those are two different production paths,
    //! and a comparison that drives only one of them cannot see an ordering
    //! or delay bug at all.
    //!
    //! ## The multi-column rig, and why it is not one column
    //!
    //! The store behind this oracle is a map of **whole chunk columns**, all
    //! resident, created on demand with a solid floor. That is load-bearing
    //! rather than convenient: our reaction dispatch reads and writes through
    //! a cascade-scoped multi-column view whose reach is decided by the
    //! [`ChunkSource`] it is handed, so a single-column rig would answer air
    //! one cell past a seam and every cross-seam question would come back
    //! "no signal" with nothing to distinguish that from a real model bug.
    //! A contraption laid out across two seams is the whole point of the
    //! comparison this oracle serves.
    //!
    //! ## Exact stepping
    //!
    //! Nothing here sleeps. `advance_tick` increments a counter, drains every
    //! entry due at that number and runs it, so this side's tick numbering is
    //! exact and the whole alignment error budget of a comparison sits on the
    //! real-time side — the same asymmetry [`super::fluid`] documents, for
    //! the same reason.
    use std::collections::HashMap;
    use std::sync::Mutex;

    use lodestone_model::BlockPos;
    use lodestone_server::block_tick_reaction::run_due_block_tick;
    use lodestone_server::{ChunkColumn, ChunkSource, ScheduledTickQueue, react_at_placement_with_entities};

    use super::{Action, WorldOracle, state_matches};

    /// Vertical extent of the rig, matching an overworld dimension's own
    /// build limits so any below-the-world guard in the model behaves as it
    /// does in production.
    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;

    /// A resident-everywhere flat world of real [`ChunkColumn`]s.
    #[derive(Debug)]
    struct ColumnRig {
        columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
        floor_y: i32,
        floor_state: String,
        /// When set, this source claims **no** column is resident, which is
        /// what a cascade-scoped multi-column view needs to hear in order to
        /// behave like the single-column reach it replaced: it never fetches
        /// a neighbour, so a read one cell past a seam answers air and a
        /// write there is dropped. Used only by
        /// [`RedstoneModelOracle::without_neighbours`], as the control that
        /// proves a cross-seam assertion has a detector behind it.
        deny_neighbours: bool,
    }

    impl ColumnRig {
        fn with_column<R>(&self, cx: i32, cz: i32, f: impl FnOnce(&mut ChunkColumn) -> R) -> R {
            let mut guard = self.columns.lock().expect("rig lock");
            let column = guard.entry((cx, cz)).or_insert_with(|| {
                let mut fresh = ChunkColumn::new(MIN_Y, HEIGHT);
                for lx in 0..16 {
                    for lz in 0..16 {
                        fresh.set_block(lx, self.floor_y, lz, &self.floor_state);
                    }
                }
                fresh
            });
            f(column)
        }
    }

    impl ChunkSource for ColumnRig {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.with_column(cx, cz, |column| column.clone())
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.with_column(cx, cz, |column| {
                if y < column.min_y || y >= column.min_y + column.height {
                    return "minecraft:air".to_owned();
                }
                column.block_state(x - cx * 16, y, z - cz * 16).to_owned()
            })
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:plains".to_owned()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.with_column(cx, cz, |column| {
                if y >= column.min_y && y < column.min_y + column.height {
                    column.set_block(x - cx * 16, y, z - cz * 16, name);
                }
            });
        }

        fn is_column_resident(&self, _cx: i32, _cz: i32) -> bool {
            !self.deny_neighbours
        }
    }

    /// Our side of a differential comparison, over the redstone model.
    #[derive(Debug)]
    pub struct RedstoneModelOracle {
        rig: ColumnRig,
        queue: ScheduledTickQueue<String>,
        tick: u64,
        origin: (i32, i32, i32),
    }

    impl RedstoneModelOracle {
        /// `origin` is added to every position an [`Action`] or a
        /// `block_state` query names, mirroring the RCON oracle's own offset
        /// so one script can be written in relative coordinates and run on
        /// both sides. `floor_y` is relative to `origin`.
        #[must_use]
        pub fn new(origin: (i32, i32, i32), floor_y: i32, floor_state: &str) -> Self {
            Self::with_reach(origin, floor_y, floor_state, false)
        }

        /// The same oracle with its cross-column reach taken away: the world
        /// behind it reports no column resident, so the reaction dispatch
        /// falls back to the one column it was handed and a signal cannot
        /// leave it.
        ///
        /// This is a **control**, not a mode anything should use. A
        /// cross-seam assertion is an assertion about an absence — no signal
        /// is lost at a boundary — and an absence needs a case that would be
        /// missed, watched failing. Building that case out of the same rig
        /// and the same script as the real comparison is what makes it
        /// evidence about the detector rather than about a second rig.
        #[must_use]
        pub fn without_neighbours(origin: (i32, i32, i32), floor_y: i32, floor_state: &str) -> Self {
            Self::with_reach(origin, floor_y, floor_state, true)
        }

        fn with_reach(origin: (i32, i32, i32), floor_y: i32, floor_state: &str, deny_neighbours: bool) -> Self {
            Self {
                rig: ColumnRig {
                    columns: Mutex::new(HashMap::new()),
                    floor_y: origin.1 + floor_y,
                    floor_state: floor_state.to_owned(),
                    deny_neighbours,
                },
                queue: ScheduledTickQueue::new(),
                tick: 0,
                origin,
            }
        }

        /// Writes a block WITHOUT running the placement reaction — for laying
        /// out a contraption before a script runs, where each component must
        /// not fire the circuit as it appears.
        pub fn place_static(&mut self, pos: (i32, i32, i32), state: &str) {
            let (x, y, z) = self.world_pos(pos);
            self.rig.set_block(x, y, z, state);
        }

        fn world_pos(&self, pos: (i32, i32, i32)) -> (i32, i32, i32) {
            (self.origin.0 + pos.0, self.origin.1 + pos.1, self.origin.2 + pos.2)
        }

        /// The tick number this oracle has advanced to. Exact by
        /// construction, unlike a real-time-aligned oracle's.
        #[must_use]
        pub fn tick(&self) -> u64 {
            self.tick
        }
    }

    impl WorldOracle for RedstoneModelOracle {
        type Error = std::convert::Infallible;

        fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
            match action {
                Action::SetBlock { pos, state } => {
                    let (x, y, z) = self.world_pos(*pos);
                    self.rig.set_block(x, y, z, state);
                    let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
                    let mut column = self.rig.column(cx, cz);
                    // The same reaction a world edit runs in production: the
                    // placed block's own on-place half plus the neighbour
                    // fan-out, both across chunk seams because `rig` reaches
                    // every column.
                    let events = react_at_placement_with_entities(
                        &mut column,
                        cx * 16,
                        cz * 16,
                        &self.rig,
                        x,
                        y,
                        z,
                        &mut self.queue,
                        self.tick,
                        None,
                    );
                    for event in events {
                        let (ex, ey, ez) = event.pos;
                        self.rig.set_block(ex, ey, ez, &event.to);
                    }
                }
                Action::RunCommand(_) => {
                    // No command grammar on this side by design — see
                    // `super::fluid`'s own arm for the reasoning. Layout goes
                    // through `place_static` instead.
                }
            }
            Ok(())
        }

        fn advance_tick(&mut self) -> Result<(), Self::Error> {
            self.tick += 1;
            for due in self.queue.drain_due(self.tick, usize::MAX) {
                let (x, y, z) = due.pos;
                let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
                let mut column = self.rig.column(cx, cz);
                if y < column.min_y || y >= column.min_y + column.height {
                    continue;
                }
                let state = column.block_state(x - cx * 16, y, z - cz * 16).to_owned();
                let reaction = run_due_block_tick(
                    &mut column,
                    cx * 16,
                    cz * 16,
                    &self.rig,
                    &due.kind,
                    BlockPos::new(x, y, z),
                    &state,
                    &mut self.queue,
                    self.tick,
                    None,
                );
                for event in reaction.events {
                    let (ex, ey, ez) = event.pos;
                    self.rig.set_block(ex, ey, ez, &event.to);
                }
            }
            Ok(())
        }

        fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
            let (x, y, z) = self.world_pos(pos);
            let state = self.rig.block_state(x, y, z);
            Ok(candidates
                .iter()
                .find(|candidate| state_matches(&state, candidate))
                .cloned())
        }
    }
}
