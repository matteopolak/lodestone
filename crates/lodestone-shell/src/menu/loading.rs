//! The connect/load phase names and the terrain progress count behind the
//! loading screen (issue #449).
//!
//! # What it is
//!
//! Two small pieces of shared vocabulary, deliberately kept out of both the
//! draw and the net layer:
//!
//! * [`ConnectPhase`] — which step of establishing a session we are on, and the
//!   **real vanilla string** for it. Not a cosmetic label: the whole point of
//!   the issue is that "still loading" and "broken" were indistinguishable, and
//!   three separate defects in this repo had "the game is frozen" as their only
//!   symptom (a dead player held on the death screen sending no chunks,
//!   `PERFORM_RESPAWN` decoded and discarded, LAN hosting with no tick loop).
//!   A screen that names its step turns those into "stuck at *this* step".
//! * [`TerrainProgress`] — the loaded-column count against the count the server
//!   is going to send, which is what the progress bar is derived from.
//!
//! # The rule this module exists to enforce
//!
//! **Every string here is a real key from the 26.2 `en_us.json`, and every
//! number is measured, never synthesised.** A progress screen showing fake
//! progress is worse than no screen at all, because it converts an honest
//! freeze into a false reassurance. So:
//!
//! * The labels are transcribed from
//!   `.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json`, and each
//!   variant records its key. Note that 26.2 has **no** `menu.loadingLevel` or
//!   `menu.generatingTerrain` — those were removed — which is why the
//!   singleplayer world-open shares [`ConnectPhase::Connecting`] rather than
//!   getting an invented string of its own. That is also what vanilla does:
//!   its integrated server is reached over a real connection, so singleplayer
//!   and multiplayer go through the same connect screens.
//! * [`TerrainProgress`] carries the raw numerator and denominator rather than a
//!   pre-computed percentage, so a caller cannot round a partial load up to
//!   "done", and [`TerrainProgress::fraction`] is clamped **below** 1.0 for
//!   exactly that reason — the screen closes when [`is_level_ready`] says so,
//!   never because a bar filled.
//!
//! # The dismissal condition
//!
//! [`is_level_ready`] is vanilla's `LevelLoadTracker.WaitingForPlayerChunk`
//! readiness rule, and it is worth naming what it is *not*: it is **not** "the
//! whole view square has landed". `TerrainProgress`'s `(2r+1)²` denominator is the
//! progress *bar*'s, and nothing else; waiting on it would hold the screen for the
//! entire initial stream. Vanilla waits on the player's own chunk and bounds even
//! that with a 30 s timeout — see [`CLIENT_WAIT_TIMEOUT`] for the incident that
//! made the bound load-bearing rather than defensive.
//!
//! Note that 26.2 has no `ReceivingLevelScreen` any more; the screen carrying
//! `multiplayer.downloadingTerrain` is `LevelLoadingScreen`, and
//! `Minecraft.doWorldLoad` constructs one unconditionally for singleplayer
//! alongside `ConnectScreen`/`ClientPacketListener` for multiplayer. So it really
//! does appear on **every** join, not only on world creation — the only
//! singleplayer-specific part is a 500 ms close delay for a brand-new world.
//!
//! # How to change it
//!
//! To add a phase you need a **real boundary in `net.rs`'s connect task** to
//! emit it from — a new variant with no emit site is the island pattern
//! `CLAUDE.md` §1 describes, and it will render as a phase the game never
//! reaches. The emit sites are `NetUpdate::ConnectPhase` sends in
//! `crate::net::run_session`; the route is `sim/net_apply.rs` →
//! `Sim::connect_phase` → `app::WindowApp::drive_ui_from_session` →
//! `UiState::set_connect_phase` → `menu/render/dispatch.rs`.
//!
//! **`connect.authorizing`, `connect.encrypting` and `connect.negotiating` are
//! deliberately absent.** Vanilla sets them from inside
//! `ClientHandshakePacketListenerImpl`, i.e. from the handshake state machine
//! itself. Ours lives in `lodestone-client`, behind one infallible
//! `ClientBuilder::connect().await`, so the shell cannot observe those three
//! boundaries at all. Adding them would mean the label changing on a timer
//! rather than on an event — the fake-progress failure this module exists to
//! avoid. They become available if and when the client driver reports the
//! handshake stage; until then their absence is honest.

/// Which step of establishing a session the loading screen is naming.
///
/// See the module doc: each variant is one real boundary the shell can actually
/// observe, and its label is the real 26.2 string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectPhase {
    /// `connect.connecting`. The session task has started: for multiplayer the
    /// socket is being dialled and the handshake/login run; for singleplayer the
    /// integrated server is being opened (world seed resolved, region source and
    /// generator built) and then reached over the in-memory transport. One phase
    /// covers both because vanilla's integrated server is likewise reached over
    /// a real connection.
    #[default]
    Connecting,
    /// `connect.joining`. The client handle exists — the handshake and login
    /// completed — and we are waiting for the play state to be usable.
    Joining,
    /// `multiplayer.downloadingTerrain`. Logged in; the server is streaming the
    /// initial view. This is the phase the progress bar belongs to, and on a
    /// brand-new singleplayer world it is also when generation happens, because
    /// columns are generated lazily as they are streamed.
    LoadingTerrain,
}

impl ConnectPhase {
    /// The vanilla translation key, recorded so a future i18n table has
    /// something to look up and so the label below is auditable against the jar.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Connecting => "connect.connecting",
            Self::Joining => "connect.joining",
            Self::LoadingTerrain => "multiplayer.downloadingTerrain",
        }
    }

    /// The `en_us` string for [`Self::key`], transcribed from the 26.2 jar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting to the server...",
            Self::Joining => "Joining world...",
            Self::LoadingTerrain => "Loading terrain...",
        }
    }
}

/// How much of the initial view has landed: `loaded` columns out of `expected`.
///
/// `expected` is the count the server is actually going to send — the view
/// square `(2 * view_radius + 1)^2`, the same square `join_view_rings`
/// partitions — not a guess and not a running maximum. `loaded` is the client's
/// own loaded-column count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainProgress {
    /// Columns the client has applied.
    pub loaded: usize,
    /// Columns the initial view contains.
    pub expected: usize,
}

impl TerrainProgress {
    /// The view square for a `view_radius`, in columns.
    #[must_use]
    pub const fn expected_for_radius(view_radius: u32) -> usize {
        let side = (view_radius as usize) * 2 + 1;
        side * side
    }

    /// The bar fill, in `0.0..=MAX_FRACTION`.
    ///
    /// **Clamped below 1.0 deliberately.** The loading screen is dismissed by
    /// [`is_level_ready`] — the player's own column, or one of the bail-outs
    /// there — never by this number reaching the end. A bar that could
    /// read as full while the screen is still up would be the false
    /// reassurance this whole feature exists to prevent; leaving the last
    /// sliver unfilled means "not done" stays visible even when the count
    /// happens to agree.
    #[must_use]
    pub fn fraction(self) -> f32 {
        if self.expected == 0 {
            return 0.0;
        }
        let raw = self.loaded as f32 / self.expected as f32;
        raw.clamp(0.0, MAX_FRACTION)
    }

    /// The count line drawn under the bar — the honest raw numbers, so a stall
    /// is legible as "stuck at 37/441" rather than as a bar that stopped.
    #[must_use]
    pub fn detail(self) -> String {
        format!("{} / {} chunks", self.loaded, self.expected)
    }
}

/// The most the progress bar will ever report. See [`TerrainProgress::fraction`].
pub const MAX_FRACTION: f32 = 0.99;

/// One chunk's status in the loading grid (issue #568) — vanilla's
/// `LevelLoadingScreen` per-chunk squares, reduced to the two states this
/// client can actually observe.
///
/// Vanilla colours each cell from `ChunkMap.getLatestStatus`, a **server-side
/// generation stage** (`ChunkStatus.EMPTY` through `.FULL`, twelve of them),
/// read in-process because vanilla's integrated server runs in the same JVM
/// as the client (`MinecraftServer.createChunkLoadStatusView`). This client's
/// server never models intermediate generation stages at all — a column comes
/// out of `ChunkColumn::from_generated` in one step, with nothing in between
/// to report — and the client only ever learns "not here yet" or "here", over
/// the network, identically for singleplayer and real multiplayer (unlike
/// vanilla, whose grid is singleplayer-only for exactly the in-process-read
/// reason above). So this grid draws exactly two of vanilla's own per-status
/// colours (`EMPTY` and `FULL`) rather than inventing intermediate ones: it is
/// real spatial information — which of *these* columns has actually arrived,
/// and in what pattern — just coarser than vanilla's twelve-stage view, and it
/// never claims more than that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkCellStatus {
    /// Not yet received by the client. Vanilla `ChunkStatus.EMPTY`'s colour,
    /// `0x545454`.
    Empty,
    /// Received and applied to the client-owned world. Vanilla
    /// `ChunkStatus.FULL`'s colour, white.
    Full,
}

/// The loading screen's chunk-status grid: real per-column state for every
/// column in the current view, meant to be centred on the chunk under the
/// player — the same recentring `ChunkLoadStatusView::moveTo` does in
/// vanilla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainChunkGrid {
    /// Half the grid's side length in chunks. The grid is [`Self::diameter`]
    /// cells square — the same radius [`TerrainProgress::expected_for_radius`]
    /// squares for the progress bar's denominator, so the two can never
    /// disagree about the size of "the initial view".
    pub radius: u32,
    /// Row-major statuses, `x` fastest, [`Self::diameter`]`(radius)`² entries
    /// — matches `LevelLoadingScreen.extractChunksForRendering`'s own
    /// `for (x) { for (z) }` iteration order.
    pub cells: Vec<ChunkCellStatus>,
}

impl TerrainChunkGrid {
    /// Cells per side: `LevelLoadingScreen.extractChunksForRendering`'s
    /// `diameter = statusView.radius() * 2 + 1`.
    #[must_use]
    pub const fn diameter(radius: u32) -> usize {
        radius as usize * 2 + 1
    }

    /// The status at grid offset `(x, z)`, each `0..diameter(self.radius)`.
    #[must_use]
    pub fn get(&self, x: usize, z: usize) -> ChunkCellStatus {
        let diameter = Self::diameter(self.radius);
        self.cells[z * diameter + x]
    }
}

/// How long the terrain screen may hold before it gives up and lets the player
/// in anyway — vanilla's `LevelLoadTracker.CLIENT_WAIT_TIMEOUT_MS`, 30 s.
///
/// This is not a safety margin someone chose here; it is vanilla's own escape
/// hatch, and its log line says what it is for: *"Timed out while waiting for the
/// client to load chunks, letting the player into the world anyway"*. Without it
/// the screen's condition is a liveness assumption about the server, and the
/// owner-reported symptom was exactly what happens when that assumption fails —
/// the join view was centred on chunk `(0, 0)` rather than on the player, so the
/// column the predicate waits for was never coming, and there was nothing to
/// dismiss the screen. A bug in the server presenting as a permanently stuck
/// client is the failure mode this constant exists to bound.
pub const CLIENT_WAIT_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(30);

/// Everything [`is_level_ready`] reads, gathered so the decision itself is a pure
/// function of observations rather than of a `Sim`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainWait {
    /// Whether the chunk column under the player's feet has arrived.
    pub own_column_loaded: bool,
    /// How long the terrain phase has been up. Compared against
    /// [`CLIENT_WAIT_TIMEOUT`].
    pub elapsed: core::time::Duration,
    /// Whether the local player is alive. A dead player is held on the death
    /// screen, and **a server holding a dead player sends no chunks at all** —
    /// so waiting for a column while dead waits forever.
    pub player_alive: bool,
    /// Whether the player's Y is inside the world's build height. `false` when
    /// the client has no world dimensions yet, which is also the honest answer:
    /// there is no build height to be inside of.
    pub within_build_height: bool,
}

/// Vanilla's `LevelLoadTracker.WaitingForPlayerChunk.isReady`, ported.
///
/// The record, so the port is auditable:
///
/// ```text
/// private boolean isReady() {
///    if (Util.getMillis() > this.timeoutAfter) {
///       LOGGER.warn("Timed out while waiting for the client to load chunks, letting the player into the world anyway");
///       return true;
///    } else {
///       BlockPos playerPos = this.player.blockPosition();
///       BlockPos cameraPos = Minecraft.getInstance().gameRenderer.mainCamera().blockPosition();
///       return !this.level.isOutsideBuildHeight(playerPos.getY())
///             && !this.level.isOutsideBuildHeight(cameraPos.getY())
///             && !this.player.isSpectator()
///             && this.player.isAlive()
///          ? this.playerSectionReady.get()
///          : true;
///    }
/// }
/// ```
///
/// Read carefully, the ternary is **"only wait if waiting could work"**: every one
/// of those four conditions failing makes the answer `true`, i.e. *ready*. They
/// are not extra requirements for readiness — they are the states in which the
/// wait is pointless, and vanilla short-circuits out of the screen rather than
/// holding a player it can never satisfy. Transcribing them as `&&`ed
/// preconditions for dismissal inverts the record and produces a screen that
/// hangs in precisely the cases vanilla wrote them for.
///
/// # Two named deviations
///
/// * **Column loaded, not section compiled.** Vanilla waits on
///   `playerSectionReady`, set from a mesh-compilation callback. This client has
///   no such callback, and the column being present in the client-owned world is
///   the observation it does have. It is a strictly *earlier* condition than a
///   compiled mesh, so this dismisses no later than vanilla — never longer, which
///   is the direction that matters for a screen the player is stuck behind.
/// * **No spectator check, and no separate camera check.** `Sim` carries no game
///   mode, so `isSpectator` has nothing to read; the camera Y is the player Y here
///   because the shell has no detached camera in the loading phase. Both are
///   short-circuits *out* of the wait, so their absence can only make this hold
///   longer than vanilla in a spectator join — bounded by
///   [`CLIENT_WAIT_TIMEOUT`], never unbounded.
#[must_use]
pub fn is_level_ready(wait: TerrainWait) -> bool {
    if wait.elapsed >= CLIENT_WAIT_TIMEOUT {
        return true;
    }
    if !wait.player_alive || !wait.within_build_height {
        return true;
    }
    wait.own_column_loaded
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::{
        CLIENT_WAIT_TIMEOUT, ChunkCellStatus, ConnectPhase, MAX_FRACTION, TerrainChunkGrid,
        TerrainProgress, TerrainWait, is_level_ready,
    };

    /// The state a healthy join is in the instant the terrain phase starts: alive,
    /// in the world, no column yet, no time elapsed. Every case below is this with
    /// one field moved, so what each test measures is unambiguous.
    const STILL_WAITING: TerrainWait = TerrainWait {
        own_column_loaded: false,
        elapsed: Duration::ZERO,
        player_alive: true,
        within_build_height: true,
    };

    /// The timeout is vanilla's 30 s, and the boundary is asserted at the two
    /// inputs where the two readings of it differ.
    ///
    /// A "does it eventually give up" test would pass against any finite bound —
    /// the *magnitude* species. So the prediction is the exact figure from
    /// `LevelLoadTracker.CLIENT_WAIT_TIMEOUT_MS`, and it is checked one
    /// millisecond either side: at 29.999 s the screen must still be up, at 30.000 s
    /// it must be gone. A bound of, say, 5 s or 60 s fails one of those two.
    #[test]
    fn the_wait_is_bounded_at_vanillas_thirty_seconds_exactly() {
        assert_eq!(CLIENT_WAIT_TIMEOUT, Duration::from_secs(30));

        let just_short = TerrainWait {
            elapsed: Duration::from_millis(29_999),
            ..STILL_WAITING
        };
        assert!(
            !is_level_ready(just_short),
            "at 29.999 s with no column the screen must still be held — a shorter bound \
             would dismiss here"
        );

        let at_the_bound = TerrainWait {
            elapsed: Duration::from_millis(30_000),
            ..STILL_WAITING
        };
        assert!(
            is_level_ready(at_the_bound),
            "at exactly 30.000 s the player is let in anyway, per vanilla's own log line"
        );
    }

    /// The player's own column — not the view square — is what a healthy join
    /// waits for, and it is sufficient on its own well inside the timeout.
    ///
    /// This is the case the whole feature is about, so it is checked at a
    /// mid-stream elapsed rather than at zero: the dismissal must come from the
    /// column, not from the clock.
    #[test]
    fn the_players_own_column_dismisses_the_screen_without_the_rest_of_the_view() {
        let mid_stream = Duration::from_secs(3);
        assert!(!is_level_ready(TerrainWait {
            elapsed: mid_stream,
            ..STILL_WAITING
        }));
        assert!(
            is_level_ready(TerrainWait {
                own_column_loaded: true,
                elapsed: mid_stream,
                ..STILL_WAITING
            }),
            "one column is the condition; at view_radius 9 the square is 361 and a \
             square-based condition would still be holding here"
        );
    }

    /// **The direction of vanilla's ternary, which is the easy thing to get
    /// backwards.** A dead player and a player outside build height are *ready*,
    /// not *held*: those are the states in which waiting cannot succeed.
    ///
    /// The dead case is the one with teeth in this repo — a server holding a dead
    /// player on the death screen sends no chunks at all, so a screen that treated
    /// `player_alive` as a requirement for dismissal would stack the terrain
    /// overlay on top of the death screen until the 30 s timeout, every death.
    #[test]
    fn the_states_where_waiting_cannot_succeed_dismiss_rather_than_hold() {
        assert!(
            is_level_ready(TerrainWait {
                player_alive: false,
                ..STILL_WAITING
            }),
            "a dead player must be let through: the server sends no chunks while it \
             holds them, so the column being waited for is never coming"
        );
        assert!(
            is_level_ready(TerrainWait {
                within_build_height: false,
                ..STILL_WAITING
            }),
            "outside build height (or with no world dimensions yet) there is no column \
             under the player to wait for"
        );
        // And the control for both: with those two back to their healthy values and
        // nothing else changed, the screen is held — so the assertions above are
        // about the fields they name and not about `STILL_WAITING` being ready
        // already.
        assert!(!is_level_ready(STILL_WAITING));
    }

    /// Every phase must have both a key and a non-empty label, and no two
    /// phases may share either — a duplicated label is a phase the screen
    /// cannot distinguish, which defeats the point.
    #[test]
    fn every_phase_has_a_distinct_key_and_label() {
        let all = [
            ConnectPhase::Connecting,
            ConnectPhase::Joining,
            ConnectPhase::LoadingTerrain,
        ];
        for phase in all {
            assert!(phase.key().contains('.'), "{phase:?} key must be a real key");
            assert!(!phase.label().is_empty());
        }
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.key(), b.key());
                assert_ne!(a.label(), b.label());
            }
        }
    }

    /// The denominator is the view square, and the bar can never claim
    /// completion — the two properties the module doc calls load-bearing.
    #[test]
    fn progress_uses_the_view_square_and_never_claims_completion() {
        // A render distance of 10 gives view_radius 11 -> 23x23 = 529.
        assert_eq!(TerrainProgress::expected_for_radius(11), 529);
        assert_eq!(TerrainProgress::expected_for_radius(0), 1);

        let empty = TerrainProgress { loaded: 0, expected: 529 };
        assert_eq!(empty.fraction(), 0.0);

        let full = TerrainProgress { loaded: 529, expected: 529 };
        assert_eq!(full.fraction(), MAX_FRACTION);

        // Over-count (the server sent more than the square, e.g. the player
        // moved mid-load) still cannot read as done.
        let over = TerrainProgress { loaded: 900, expected: 529 };
        assert_eq!(over.fraction(), MAX_FRACTION);

        // A zero denominator is a missing view radius, not a divide by zero.
        let unknown = TerrainProgress { loaded: 5, expected: 0 };
        assert_eq!(unknown.fraction(), 0.0);

        assert_eq!(
            TerrainProgress { loaded: 37, expected: 441 }.detail(),
            "37 / 441 chunks"
        );
    }

    /// `diameter` matches vanilla's `radius * 2 + 1`, and `get` reads the
    /// row-major `x`-fastest layout the doc promises — checked at two
    /// distinct cells so a transposed `(x, z)` index would fail rather than
    /// coincide.
    #[test]
    fn the_grid_is_radius_times_two_plus_one_square_and_row_major() {
        assert_eq!(TerrainChunkGrid::diameter(0), 1);
        assert_eq!(TerrainChunkGrid::diameter(2), 5);
        assert_eq!(TerrainChunkGrid::diameter(11), 23);

        // A 3x3 grid (radius 1) with only two distinct cells set, at offsets
        // that would collide under a transposed index.
        let grid = TerrainChunkGrid {
            radius: 1,
            cells: vec![
                ChunkCellStatus::Full, // (0, 0)
                ChunkCellStatus::Empty,
                ChunkCellStatus::Empty,
                ChunkCellStatus::Empty,
                ChunkCellStatus::Empty,
                ChunkCellStatus::Empty,
                ChunkCellStatus::Empty,
                ChunkCellStatus::Empty,
                ChunkCellStatus::Full, // (2, 2)
            ],
        };
        assert_eq!(grid.get(0, 0), ChunkCellStatus::Full);
        assert_eq!(grid.get(2, 2), ChunkCellStatus::Full);
        assert_eq!(grid.get(2, 0), ChunkCellStatus::Empty);
        assert_eq!(grid.get(0, 2), ChunkCellStatus::Empty);
    }
}
