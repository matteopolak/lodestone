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
//!   [`rcon::RconOracle::advance_tick`] therefore lets the server's own loop
//!   advance in real time and reads back **its own `time query gametime`
//!   counter** to find out when a tick actually happened, rather than
//!   assuming one elapsed after a fixed sleep. That used to be a fixed
//!   `sleep(TICK_MILLIS)`, and the difference is measurable rather than
//!   theoretical: under CPU contention elsewhere on the machine running the
//!   harness, a fixed sleep undercounts real ticks (every `block_state`
//!   probe round trip happens between two sleeps, and contention stretches
//!   both), so the server's real tick count runs ahead of the harness's
//!   assumed one — repeatably, in one direction, and by an amount that grows
//!   with contention rather than with anything about the world being
//!   compared. Reading the counter back removes the assumption instead of
//!   tuning around it, at the cost of one extra RCON round trip per tick.
//!   Measured on the fluid-spread rig by hand, outside this harness
//!   entirely (a raw RCON probe with real timestamps, bypassing every
//!   tick-counting assumption below): cell 1 read as water at 247 ms after
//!   the source was placed, matching a 250 ms / 5-tick prediction, at a
//!   moment when the machine was busy enough to make the sleep-based
//!   harness itself report a spurious divergence on the same rig.
//!
//! ## What this module does not do yet
//!
//! - **Generated live scripts currently cover fluids only.** The test-support
//!   layer in `tests/support/differential_generation.rs` generates bounded
//!   `SetBlock`-only scripts and semantically shrinks them while preserving
//!   the complete first-divergence signature, including tick.
//!   `tests/differential_live_generated_fluid.rs` evaluates those candidates
//!   through this module's production fluid and RCON oracles. It clears and
//!   drains a dedicated live lane, verifies its baseline and re-anchors tick
//!   timing before every generated, shrink or replay candidate.
//! - **No validation against a reverted historical fix** — revert a committed
//!   fix in a scratch worktree and require the harness to rediscover it. That
//!   needs an action corpus rich enough to reach the reverted code path,
//!   which needs generation first.
//! - **The generic `WorldOracle` remains block-state-only.** The hermetic
//!   `tests/differential_client_state.rs` fixture uses this block-state half,
//!   adds direct entity and inventory comparisons through public client
//!   queries, and runs a real `IntegratedServer` tick loop against a direct
//!   `ChunkSource` read; scheduled-tick state is not exposed by that read
//!   model.

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

/// An ordered action sequence. Fixed live comparisons construct this by hand;
/// hermetic differential tests can generate it through their test-support
/// layer without adding a property-test runtime to this library.
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

    /// Classifies an operation error for callers that need to separate an
    /// unavailable or stalled instrument from a gameplay disagreement.
    /// Implementations whose error type carries no timeout signal can keep
    /// the default.
    fn classify_error(_error: &Self::Error) -> OracleFailureKind {
        OracleFailureKind::Failure
    }

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
    /// Separates a stalled/timed-out instrument from other transport or
    /// protocol failures. Neither kind is a gameplay divergence.
    pub kind: OracleFailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleFailureKind {
    Failure,
    Timeout,
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

fn oracle_failure<O: WorldOracle>(tick: u64, side: Side, error: O::Error) -> DifferentialOutcome {
    let kind = O::classify_error(&error);
    DifferentialOutcome::OracleFailed(OracleFailure {
        tick,
        side,
        kind,
        message: error.to_string(),
    })
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
                return oracle_failure::<L>(tick, Side::Left, e);
            }
            if let Err(e) = right.apply(action) {
                return oracle_failure::<R>(tick, Side::Right, e);
            }
        }

        if let Err(e) = left.advance_tick() {
            return oracle_failure::<L>(tick, Side::Left, e);
        }
        if let Err(e) = right.advance_tick() {
            return oracle_failure::<R>(tick, Side::Right, e);
        }

        for (pos, candidates) in region {
            let left_state = match left.block_state(*pos, candidates) {
                Ok(s) => s,
                Err(e) => {
                    return oracle_failure::<L>(tick, Side::Left, e);
                }
            };
            let right_state = match right.block_state(*pos, candidates) {
                Ok(s) => s,
                Err(e) => {
                    return oracle_failure::<R>(tick, Side::Right, e);
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

/// One vanilla tick, in wall-clock terms — used only as the poll cadence
/// while [`rcon::RconOracle`] waits for the server's own tick counter to
/// advance (see that type's `advance_tick`), and by callers that need to
/// sleep roughly one tick for reasons of their own. Not the thing that
/// decides when a tick has happened; see this module's top-level doc for why
/// there is no `/tick step`-based mechanism today.
pub const TICK_MILLIS: Duration = Duration::from_millis(50);

/// How long [`rcon::RconOracle::advance_tick`] waits for the server's own
/// `time query gametime` counter to advance by one before giving up and
/// reporting an oracle failure. Generous relative to [`TICK_MILLIS`]
/// specifically so that CPU contention elsewhere on the machine — which
/// slows the wait down without changing what it is waiting *for* — cannot
/// turn into a false divergence; a world that is genuinely not ticking
/// (frozen, paused, or `pause-when-empty-seconds` having fired) is the only
/// thing this timeout is meant to catch.
pub const MAX_TICK_WAIT: Duration = Duration::from_secs(20);

/// Maximum wall-clock time for one RCON connection attempt or complete frame
/// read or write. Every command in a live differential candidate is therefore
/// interruptible even when the remote endpoint accepts a socket and then
/// stops responding.
pub const RCON_IO_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(feature = "rcon-oracle")]
pub mod rcon {
    //! [`RconOracle`]: a [`super::WorldOracle`] over any Source-RCON
    //! endpoint. Works against real vanilla (`scripts/live-oracles/*.sh`) and
    //! against our own [`lodestone_server::IntegratedServer::start_rcon`]
    //! identically, since both speak the same wire protocol — this is what
    //! lets one oracle type serve both sides of the comparison.
    use super::{Action, OracleFailureKind, WorldOracle};
    use lodestone_testsupport::rcon_frame;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};

    const TYPE_COMMAND: i32 = 2;
    const TYPE_AUTH: i32 = 3;
    const MAX_RESPONSE_BYTES: i32 = 4 * 1024 * 1024;

    #[derive(Debug)]
    struct TimedRconClient {
        stream: TcpStream,
        next_id: i32,
        io_timeout: Duration,
    }

    impl TimedRconClient {
        fn connect(
            addr: SocketAddr,
            password: &str,
            timeout: Duration,
        ) -> std::io::Result<Self> {
            if timeout.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "RCON I/O timeout must be positive",
                ));
            }
            let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "RCON I/O timeout is too large for a monotonic deadline",
                )
            })?;
            let remaining = Self::remaining(deadline, "connection")?;
            let stream = TcpStream::connect_timeout(&addr, remaining)
                .map_err(Self::normalize_timeout)?;
            let mut client = Self {
                stream,
                next_id: 1,
                io_timeout: timeout,
            };
            let id = client.send(TYPE_AUTH, password)?;
            let (response_id, _) = client.read_response()?;
            if response_id != id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "RCON authentication failed",
                ));
            }
            Ok(client)
        }

        fn normalize_timeout(error: std::io::Error) -> std::io::Error {
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("RCON I/O exceeded the configured socket timeout: {error}"),
                )
            } else {
                error
            }
        }

        fn operation_deadline(&self) -> std::io::Result<Instant> {
            Instant::now().checked_add(self.io_timeout).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "RCON I/O timeout is too large for a monotonic deadline",
                )
            })
        }

        fn remaining(deadline: Instant, operation: &str) -> std::io::Result<Duration> {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("RCON {operation} exceeded its full-operation deadline"),
                ))
            } else {
                Ok(remaining)
            }
        }

        fn write_all_until(
            stream: &mut TcpStream,
            mut bytes: &[u8],
            deadline: Instant,
        ) -> std::io::Result<()> {
            while !bytes.is_empty() {
                let remaining = Self::remaining(deadline, "frame write")?;
                stream.set_write_timeout(Some(remaining))?;
                match stream.write(bytes) {
                    Ok(0) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "RCON frame write returned zero bytes",
                        ));
                    }
                    Ok(written) => bytes = &bytes[written..],
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(Self::normalize_timeout(error)),
                }
            }
            Self::remaining(deadline, "frame write").map(|_| ())
        }

        fn read_exact_until(
            stream: &mut TcpStream,
            bytes: &mut [u8],
            deadline: Instant,
        ) -> std::io::Result<()> {
            let mut filled = 0;
            while filled < bytes.len() {
                let remaining = Self::remaining(deadline, "frame read")?;
                stream.set_read_timeout(Some(remaining))?;
                match stream.read(&mut bytes[filled..]) {
                    Ok(0) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "RCON response ended before the complete frame arrived",
                        ));
                    }
                    Ok(read) => filled += read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(Self::normalize_timeout(error)),
                }
            }
            Self::remaining(deadline, "frame read").map(|_| ())
        }

        fn send(&mut self, packet_type: i32, payload: &str) -> std::io::Result<i32> {
            let id = self.next_id;
            self.next_id += 1;
            let deadline = self.operation_deadline()?;
            Self::write_all_until(
                &mut self.stream,
                &rcon_frame(id, packet_type, payload),
                deadline,
            )?;
            Ok(id)
        }

        fn read_response(&mut self) -> std::io::Result<(i32, String)> {
            let deadline = self.operation_deadline()?;
            let mut len_buf = [0; 4];
            Self::read_exact_until(&mut self.stream, &mut len_buf, deadline)?;
            let len = i32::from_le_bytes(len_buf);
            if !(10..=MAX_RESPONSE_BYTES).contains(&len) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid RCON frame length {len}"),
                ));
            }
            let mut body = vec![0; len as usize];
            Self::read_exact_until(&mut self.stream, &mut body, deadline)?;
            if body[len as usize - 2..] != [0, 0] {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "RCON response is missing its two terminator bytes",
                ));
            }
            let response_id = i32::from_le_bytes(body[0..4].try_into().expect("four-byte response id"));
            let payload = String::from_utf8(body[8..len as usize - 2].to_vec()).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("RCON response is not UTF-8: {error}"),
                )
            })?;
            Ok((response_id, payload))
        }

        fn command(&mut self, command: &str) -> std::io::Result<String> {
            let id = self.send(TYPE_COMMAND, command)?;
            let (response_id, body) = self.read_response()?;
            if response_id != id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "RCON response id mismatch",
                ));
            }
            Ok(body)
        }
    }

    #[derive(Debug)]
    pub struct RconOracle {
        client: TimedRconClient,
        /// Prefixed to every position this oracle touches, so two oracles
        /// backed by the *same* running world can be driven side by side at
        /// disjoint coordinate ranges without one script's actions
        /// clobbering the other's — see `docs/fuzzing.md`'s self-consistency
        /// proof, which does exactly this against one live oracle.
        origin: (i32, i32, i32),
        /// The **nominal** tick count this oracle has reported reaching so
        /// far via [`WorldOracle::advance_tick`] (or `None` before the first
        /// call) — read from the server's own `time query gametime` counter,
        /// but advanced by exactly one per call rather than jumped to
        /// whatever the counter shows.
        ///
        /// That distinction is the whole fix. A fixed `sleep(TICK_MILLIS)`
        /// assumes one sleep equals one tick, and under CPU contention
        /// elsewhere on the machine that assumption is measurably wrong: a
        /// `block_state` probe is a round trip that happens *between* two
        /// sleeps, so a slow probe (or a slow machine) lets the server's
        /// real tick count run ahead of the harness's assumed one. Reading
        /// the real counter fixes *that* — `advance_tick` never returns
        /// before the tick it is waiting for has genuinely happened — but a
        /// second failure mode remains if the counter's value is adopted
        /// wholesale: a real tick that arrives late gets read alongside a
        /// second real tick that has *also* already happened by the time
        /// the read lands, and jumping straight to the observed value would
        /// silently skip this oracle's own nominal tick forward by two,
        /// desynchronising it from a peer oracle (like
        /// [`fluid::FluidModelOracle`]) that steps exactly one nominal tick
        /// per call and has no way to know a real tick was skipped. Banking
        /// only `+1` here, and letting the surplus satisfy the *next* call's
        /// wait immediately instead, keeps both oracles' nominal tick counts
        /// in one-to-one lockstep regardless of how many real ticks a single
        /// call happened to straddle.
        baseline_gametime: Option<i64>,
        /// How many *extra* real ticks had already elapsed by the time
        /// `advance_tick` observed the counter it was waiting for — the
        /// instrument's own error count, kept purely for diagnosis. It does
        /// **not** indicate a wrong tick label: [`Self::advance_tick`] never
        /// adopts an overshoot into its own nominal count (see
        /// [`Self::baseline_gametime`]), so a divergence's tick number is
        /// exact regardless of this value. A consistently non-zero count
        /// here still means every step ran with less real-time headroom
        /// than a quiet machine gives, and is worth checking before reading
        /// too much into a *fine-grained timing* comparison from the same
        /// run — a rerun once the machine quiets down has more of that
        /// headroom to spend.
        missed_deadlines: u32,
    }

    impl RconOracle {
        /// Connects and authenticates. `origin` is added to every position an
        /// [`Action`]/`block_state` query names, so the same script can run
        /// at two disjoint locations in one shared world.
        pub fn connect(
            addr: impl AsRef<str>,
            password: &str,
            origin: (i32, i32, i32),
        ) -> std::io::Result<Self> {
            Self::connect_with_io_timeout(addr, password, origin, super::RCON_IO_TIMEOUT)
        }

        /// Connects with an explicit hard bound for each connection attempt
        /// and complete frame operation.
        ///
        /// `addr` must be a numeric IP address and port. Rejecting hostnames
        /// before any connection work keeps name resolution from sitting
        /// outside the enforceable connection deadline.
        /// The ordinary live path uses [`super::RCON_IO_TIMEOUT`]; the
        /// parameterized form keeps the stalled-peer control fast and proves
        /// that a blocking frame read cannot bypass timeout classification.
        pub fn connect_with_io_timeout(
            addr: impl AsRef<str>,
            password: &str,
            origin: (i32, i32, i32),
            io_timeout: Duration,
        ) -> std::io::Result<Self> {
            let addr = addr.as_ref().parse::<SocketAddr>().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("RCON endpoint must be a numeric IP address and port: {error}"),
                )
            })?;
            let mut client = TimedRconClient::connect(addr, password, io_timeout)?;
            // Read the counter now, before the caller applies a single
            // action, rather than lazily on the first `advance_tick` call.
            // Capturing it late used to hide exactly the gap this oracle
            // most needs to catch: the round trip that applies the script's
            // first action is real wall-clock time too, and under
            // contention it is long enough on its own to let several real
            // ticks pass before anything reads the counter at all. Reading
            // it here means that gap lands inside the very first
            // `advance_tick`'s own wait, where `missed_deadlines` can see it,
            // instead of being silently folded into what gets called "tick
            // 0".
            let baseline_gametime = Some(Self::query_gametime_over(&mut client)?);
            Ok(Self {
                client,
                origin,
                baseline_gametime,
                missed_deadlines: 0,
            })
        }

        fn world_pos(&self, pos: (i32, i32, i32)) -> (i32, i32, i32) {
            (self.origin.0 + pos.0, self.origin.1 + pos.1, self.origin.2 + pos.2)
        }

        /// Re-anchors the nominal tick ladder to the counter's *current*
        /// value. For a caller that sends rig-building commands (a `/fill`
        /// channel, a `forceload`) through this same oracle's connection
        /// before starting the actual comparison: those commands are real
        /// round trips too, and [`Self::connect`]'s own baseline was read
        /// before any of them, not after. Left uncorrected, whatever real
        /// time rig-building costs becomes a head start folded silently into
        /// "tick 0" — invisible to [`Self::missed_deadlines`], which only
        /// flags a real tick *overshooting* the nominal ladder, not the
        /// ladder having started early. Call this once, right after
        /// rig-building and right before the script's first action, so the
        /// ladder starts at the same moment the comparison does. This also
        /// clears [`Self::missed_deadlines`], because setup and reset time is
        /// outside the candidate whose timing that counter diagnoses.
        pub fn reset_baseline(&mut self) -> std::io::Result<()> {
            self.baseline_gametime = Some(self.query_gametime()?);
            self.missed_deadlines = 0;
            Ok(())
        }

        /// Reads the server's own tick counter via `time query gametime`.
        /// Works identically against real vanilla (`"The game time is <N>
        /// tick(s)"`) and against our own [`lodestone_server`] RCON handler
        /// (`"The time is <N>"`, `crate::commands::time`'s wording) — both
        /// answers carry exactly one integer token, which is all this reads.
        fn query_gametime(&mut self) -> std::io::Result<i64> {
            Self::query_gametime_over(&mut self.client)
        }

        /// Free-function form of [`Self::query_gametime`], for
        /// [`Self::connect`] to call before `Self` exists to be borrowed.
        fn query_gametime_over(client: &mut TimedRconClient) -> std::io::Result<i64> {
            let response = client.command("time query gametime")?;
            response
                .split_whitespace()
                .filter_map(|token| token.parse::<i64>().ok())
                .next_back()
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "`time query gametime` answered {response:?}, which has no parseable tick count"
                    ))
                })
        }

        /// How many extra real ticks had already elapsed when this oracle's
        /// waited-for tick showed up. **A non-zero count here does not make a
        /// tick label wrong** (see [`Self::baseline_gametime`]) but a large or
        /// growing one is worth checking before trusting a *timing*
        /// comparison's fine-grained shape, since it means this run had
        /// little headroom.
        #[must_use]
        pub fn missed_deadlines(&self) -> u32 {
            self.missed_deadlines
        }
    }

    impl WorldOracle for RconOracle {
        type Error = std::io::Error;

        fn classify_error(error: &Self::Error) -> OracleFailureKind {
            if error.kind() == std::io::ErrorKind::TimedOut {
                OracleFailureKind::Timeout
            } else {
                OracleFailureKind::Failure
            }
        }

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
            // Real ticks, read back from the server's own counter rather
            // than assumed from wall-clock sleep — deliberately never
            // `/tick step`, per this module's top-level doc. `time query
            // gametime` is the same read-back primitive used to confirm
            // each manual step landed in `crate::redstone_diode_oracle_gate`
            // over in `lodestone-server`.
            let baseline = match self.baseline_gametime {
                Some(value) => value,
                None => self.query_gametime()?,
            };
            let target = baseline + 1;
            let deadline = Instant::now() + super::MAX_TICK_WAIT;
            loop {
                let observed = self.query_gametime()?;
                if observed >= target {
                    // Advance the nominal counter by exactly one tick, not to
                    // `observed` — see this field's own doc. If `observed`
                    // overshot `target`, that real tick has already happened;
                    // banking `target` (rather than `observed`) as the new
                    // baseline means the *next* call's wait is satisfied
                    // immediately by the same real tick, so the two sides'
                    // nominal tick counts stay in one-to-one lockstep no
                    // matter how much real time a single call took.
                    if observed > target {
                        self.missed_deadlines += u32::try_from(observed - target).unwrap_or(u32::MAX);
                    }
                    self.baseline_gametime = Some(target);
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!(
                        "`time query gametime` did not advance past {baseline} within \
                         {:?} — the world is not ticking (check \
                         pause-when-empty-seconds=0) rather than merely slow",
                        super::MAX_TICK_WAIT
                    )));
                }
                std::thread::sleep(super::TICK_MILLIS / 4);
            }
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
                    for pending in ticks_after_edit(&self.rig, self.env, BlockPos::new(x, y, z)) {
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
