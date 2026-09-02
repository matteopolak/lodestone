//! The connect/load phase names and the terrain progress count behind the
//! loading screen.
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
//! **Terrain is only half of it.** [`world_wait`] is the real dismissal
//! condition, and it ANDs [`is_level_ready`] with [`assets_ready`]: a
//! server-pushed resource pack downloads on its own thread and is applied to the
//! block atlas on a later frame, neither of which the terrain rule can see. The
//! owner-reported symptom was that gap — the screen cleared on the column
//! arriving, the world appeared wearing the *previous* pack's textures, and a
//! second later the atlas rebuild hitched and everything popped. Vanilla has no
//! such gap for a different reason: an application is an `Overlay`, not a
//! `Screen`, and `Gui.update` paints an overlay over everything until the reload
//! future completes, so nothing of the world is presented meanwhile. See
//! [`assets_ready`] for the bound, and `docs/join-readiness.md` for the whole
//! sequence.
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

/// One chunk's status in the loading grid — vanilla's
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

/// The grid's own radius, in chunks — **a constant, not the render distance**.
///
/// This is vanilla's number, and reading it out of the jar is the whole of this
/// constant's justification. `Minecraft.doWorldLoad` builds the view the
/// loading screen draws as
/// `createChunkLoadStatusView(Math.max(5, 3) + ChunkLevel.RADIUS_AROUND_FULL_CHUNK + 1)`,
/// and `ChunkLevel.RADIUS_AROUND_FULL_CHUNK` is
/// `ChunkPyramid.GENERATION_PYRAMID.getStepTo(ChunkStatus.FULL).accumulatedDependencies().getRadius()`,
/// which evaluates to **11** for 26.2's pyramid (the chain's widest accumulated
/// dependency is `LIGHT`'s, `STRUCTURE_STARTS` at radius 8 pushed out by the
/// `BIOMES`/`CARVERS`/`INITIALIZE_LIGHT` radius-1 steps above it). So vanilla's
/// grid is 17, i.e. `2 * 17 + 1 = 35` cells and `35 * 2 = 70` logical pixels
/// square, **for every render distance** — the status view is the *server's*
/// generation neighbourhood, which does not grow with what the client draws.
///
/// # Why this is a cap here rather than a fixed size
///
/// This client has no server-side status view to size from; the grid is built
/// from the client's own `NetClient::is_chunk_loaded` over the streamed square,
/// so its natural radius is the view radius. Taking the **minimum** of the two
/// keeps a small render distance showing its real, whole square (radius 8 draws
/// 17×17, all of it meaningful) while pinning the large end to vanilla's own
/// size instead of growing without bound.
///
/// Unbounded was the bug: at the owner's `render_distance = 32` the grid was
/// 65 cells and 130 px square, which does not fit above the phase label on the
/// 320×240 canvas `config::calculate_gui_scale` treats as the floor — it
/// overflowed the top of the screen, and kept getting worse with distance.
pub const MAX_GRID_RADIUS: u32 = 17;

impl TerrainChunkGrid {
    /// The radius to actually draw for a session streaming `view_radius`:
    /// [`MAX_GRID_RADIUS`], or the whole view when it is smaller.
    ///
    /// A function rather than a `min` at the one call site so the layout gate
    /// and the producer share one expression — the same reason
    /// `menu::render::screens::chunk_grid_dy` is a free function.
    #[must_use]
    pub const fn view_radius(view_radius: u32) -> u32 {
        if view_radius < MAX_GRID_RADIUS { view_radius } else { MAX_GRID_RADIUS }
    }

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

/// Everything [`world_wait`] reads about **asset** work that is still
/// outstanding, gathered so the decision is a pure function of observations
/// rather than of a `Sim` — the same shape [`TerrainWait`] has.
///
/// This exists because the terrain rule above is not the whole of "is the world
/// presentable". A server-pushed resource pack is downloaded on its own thread
/// and applied on a later frame, and neither step is anything
/// [`is_level_ready`] can see: the player's own column can arrive seconds before
/// the pack does. The owner-reported symptom is exactly that gap — the loading
/// screen clears, the world appears wearing the *previous* pack's textures, the
/// frame that applies the new one takes about a second, and everything pops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetWait {
    /// Server-pack downloads that have been accepted and not yet resolved —
    /// `crate::net::packs_in_flight`. Zero for singleplayer and for any server
    /// that pushes no pack, which is what makes this gate free there rather
    /// than a delay everyone pays.
    pub packs_in_flight: usize,
    /// Whether the pack stack has moved since the atlas the world is currently
    /// drawn with was built — `crate::resources::pack_generation` against the
    /// value `Sim::reload_resource_pack_atlas` last consumed.
    ///
    /// This is the *narrow* half and it is only ever true for a fraction of one
    /// frame, because the rebuild is synchronous inside `redraw`. It is here for
    /// the race the counter alone cannot cover: the download thread installs the
    /// bytes (bumping the generation) and only then resolves, so a reader that
    /// samples between those two points sees `packs_in_flight == 0` with a stale
    /// atlas. Without this term that reader dismisses the screen one frame early
    /// — which is the whole defect, just narrower.
    pub atlas_stale: bool,
    /// How long the terrain phase has been up. **The same clock and the same
    /// deadline as [`TerrainWait::elapsed`]**, deliberately: see
    /// [`assets_ready`].
    pub elapsed: core::time::Duration,
}

/// Whether no asset work is outstanding, bounded by [`CLIENT_WAIT_TIMEOUT`].
///
/// # The bound, and why it is the terrain wait's own
///
/// This shares [`CLIENT_WAIT_TIMEOUT`] *and the clock it is measured against*
/// with [`is_level_ready`] rather than getting a second deadline of its own, and
/// that is a port rather than a convenience. Vanilla's `LevelLoadTracker` stamps
/// `Util.getMillis() + CLIENT_WAIT_TIMEOUT_MS` **once**, in `startClientLoad`,
/// and `WaitingForServer.loadingPacketsReceived` carries that same `timeoutAfter`
/// into `WaitingForPlayerChunk` unchanged — one deadline for the whole client
/// load, not one per sub-wait. Two waits sharing one deadline is therefore the
/// shape the record already has.
///
/// It also settles the scope question by construction. The elapsed time is
/// measured from the entry into `ConnectPhase::LoadingTerrain`, so a pack pushed
/// **during a join** is inside the window and holds the screen, while a pack
/// pushed an hour into a session is far past it and this returns `true`
/// immediately. That is a named deviation from vanilla, which covers an in-play
/// reload with its `LoadingOverlay` too: reproducing that half needs a second
/// clock and a screen reachable from mid-play, and the cost of getting it wrong
/// is covering a live world, so it is deliberately left out rather than
/// approximated.
#[must_use]
pub fn assets_ready(wait: AssetWait) -> bool {
    if wait.elapsed >= CLIENT_WAIT_TIMEOUT {
        return true;
    }
    wait.packs_in_flight == 0 && !wait.atlas_stale
}

/// What the loading screen is waiting for, or `None` when the world is
/// presentable.
///
/// One expression with two consumers — the dismissal *and* the label the screen
/// draws — so the screen can never name a step it is not actually holding for.
/// The same reason `menu::render::screens::chunk_grid_dy` is a free function.
///
/// **Assets are checked first**, matching vanilla's own precedence: `Gui.update`
/// renders `overlay` in preference to `screen` (`if (overlay != null) … else if
/// (resourcesLoaded && screen != null) …`), and a resource reload is an
/// `Overlay` while the terrain wait is a `Screen`. So when both are outstanding
/// the player is told about the pack, which is also the honest ordering: the
/// terrain count is still moving underneath and would read as a stall.
#[must_use]
pub fn world_wait(terrain: TerrainWait, assets: AssetWait) -> Option<WorldWait> {
    if !assets_ready(assets) {
        return Some(WorldWait::ApplyingPack);
    }
    if !is_level_ready(terrain) {
        return Some(WorldWait::Terrain);
    }
    None
}

/// The step [`world_wait`] is holding the world back for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldWait {
    /// The player's own chunk column has not arrived — [`is_level_ready`].
    Terrain,
    /// A server-pushed resource pack is still downloading or has not been
    /// applied to the block atlas yet — [`assets_ready`].
    ApplyingPack,
}

impl WorldWait {
    /// The vanilla translation key, recorded for the same reason
    /// [`ConnectPhase::key`] is.
    ///
    /// `multiplayer.applyingPack` is a real 26.2 key with exactly this meaning.
    /// Vanilla's *server-pack* path shows a `LoadingOverlay` rather than a
    /// worded screen and so does not use this string itself — Realms' own
    /// pack-application wait does — but it is the jar's own phrasing for the
    /// state, not one invented here, which is the rule this module holds itself
    /// to.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Terrain => ConnectPhase::LoadingTerrain.key(),
            Self::ApplyingPack => "multiplayer.applyingPack",
        }
    }

    /// The `en_us` string for [`Self::key`], transcribed from the 26.2 jar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            // Delegated rather than restated: the terrain wait already owns
            // this string, and two copies could drift.
            Self::Terrain => ConnectPhase::LoadingTerrain.label(),
            Self::ApplyingPack => "Applying resource pack",
        }
    }

    /// Whether this step has a real progress bar and chunk grid to draw.
    ///
    /// Only the terrain wait does. Nothing in this client observes a pack
    /// download's byte count, so an `ApplyingPack` bar would be the synthesised
    /// progress this module's own doc forbids — the bare label is the honest
    /// presentation.
    #[must_use]
    pub const fn has_terrain_progress(self) -> bool {
        matches!(self, Self::Terrain)
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::{
        AssetWait, CLIENT_WAIT_TIMEOUT, ChunkCellStatus, ConnectPhase, MAX_FRACTION,
        TerrainChunkGrid, TerrainProgress, TerrainWait, WorldWait, assets_ready, is_level_ready,
        world_wait,
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

    /// The terrain state a healthy join is in **after** its own column has
    /// landed — i.e. the exact moment the world used to be presented. Every
    /// asset case below pairs with this, so what they measure is unambiguously
    /// the asset half and never a terrain condition leaking in.
    const TERRAIN_DONE: TerrainWait = TerrainWait {
        own_column_loaded: true,
        elapsed: Duration::ZERO,
        player_alive: true,
        within_build_height: true,
    };

    /// No asset work outstanding, at the start of the wait.
    const ASSETS_DONE: AssetWait = AssetWait {
        packs_in_flight: 0,
        atlas_stale: false,
        elapsed: Duration::ZERO,
    };

    /// **The defect, stated as a test.** With the terrain half satisfied — the
    /// world would have been presented — each of the two asset observations
    /// must independently hold it back.
    ///
    /// The control comes first and is load-bearing: it establishes that
    /// `TERRAIN_DONE`/`ASSETS_DONE` really is the "would have been shown" state,
    /// so the two holds below are attributable to the field each one moves and
    /// not to the fixture already being unready.
    #[test]
    fn an_outstanding_pack_holds_the_world_back_after_the_terrain_is_ready() {
        assert_eq!(
            world_wait(TERRAIN_DONE, ASSETS_DONE),
            None,
            "control: with the column landed and no pack outstanding the world \
             is presentable — this is the state the world used to appear in"
        );

        assert_eq!(
            world_wait(
                TERRAIN_DONE,
                AssetWait {
                    packs_in_flight: 1,
                    ..ASSETS_DONE
                }
            ),
            Some(WorldWait::ApplyingPack),
            "a download that has been accepted and not yet resolved must hold \
             the screen: this is the several-second half of the wait"
        );

        assert_eq!(
            world_wait(
                TERRAIN_DONE,
                AssetWait {
                    atlas_stale: true,
                    ..ASSETS_DONE
                }
            ),
            Some(WorldWait::ApplyingPack),
            "installed bytes whose atlas rebuild has not run yet must hold it \
             too — the one-frame race the counter alone cannot see"
        );
    }

    /// Assets take precedence over terrain when both are outstanding, matching
    /// vanilla's `Gui.update` preferring `overlay` to `screen`.
    ///
    /// The discriminating input is deliberately one where the two hypotheses
    /// differ: *both* halves unready. With only one unready either ordering
    /// gives the same answer, so a fixture like that would measure nothing.
    #[test]
    fn the_pack_wait_is_named_ahead_of_the_terrain_wait_when_both_are_outstanding() {
        let terrain_waiting = TerrainWait {
            own_column_loaded: false,
            ..TERRAIN_DONE
        };
        let assets_waiting = AssetWait {
            packs_in_flight: 1,
            ..ASSETS_DONE
        };

        // Each half alone, to prove both are genuinely unready at these inputs.
        assert_eq!(
            world_wait(terrain_waiting, ASSETS_DONE),
            Some(WorldWait::Terrain)
        );
        assert_eq!(
            world_wait(TERRAIN_DONE, assets_waiting),
            Some(WorldWait::ApplyingPack)
        );

        assert_eq!(
            world_wait(terrain_waiting, assets_waiting),
            Some(WorldWait::ApplyingPack),
            "with both outstanding the player is told about the pack: the \
             terrain count is still moving underneath and would read as a stall"
        );
    }

    /// The asset wait is bounded by the terrain wait's own deadline, checked
    /// one millisecond either side.
    ///
    /// A "does it eventually give up" test would pass against any finite bound.
    /// The prediction is the exact figure the terrain half already uses, and the
    /// two inputs below are the only ones at which a 30 s bound and any other
    /// bound disagree.
    ///
    /// This is also what scopes the feature to the join window rather than to
    /// the whole session: `elapsed` is measured from the entry into the terrain
    /// phase, so the hour-into-a-session case sits far past this bound and is
    /// never held.
    #[test]
    fn the_asset_wait_gives_up_at_the_same_thirty_seconds_the_terrain_wait_does() {
        let just_short = AssetWait {
            packs_in_flight: 1,
            atlas_stale: true,
            elapsed: Duration::from_millis(29_999),
        };
        assert!(
            !assets_ready(just_short),
            "at 29.999 s with a pack still outstanding the screen must be held"
        );
        assert_eq!(
            world_wait(TERRAIN_DONE, just_short),
            Some(WorldWait::ApplyingPack)
        );

        let at_the_bound = AssetWait {
            elapsed: CLIENT_WAIT_TIMEOUT,
            ..just_short
        };
        assert!(
            assets_ready(at_the_bound),
            "at exactly 30.000 s the player is let in anyway, on the same \
             deadline `is_level_ready` uses and for the same reason: a pack \
             that never arrives must not be a game that never starts"
        );
        assert_eq!(world_wait(TERRAIN_DONE, at_the_bound), None);

        // An hour in — an in-play push — is past the bound and never held.
        assert!(assets_ready(AssetWait {
            elapsed: Duration::from_secs(3600),
            ..just_short
        }));
    }

    /// Singleplayer, and any server that pushes no pack, must pay nothing for
    /// this: the readiness condition has to be satisfied by the *absence* of
    /// work rather than by a delay elapsing.
    ///
    /// Asserted at `Duration::ZERO` precisely so a fixed wait of any length
    /// would fail it.
    #[test]
    fn a_session_with_no_pack_is_ready_at_zero_elapsed() {
        assert!(assets_ready(ASSETS_DONE));
        assert_eq!(world_wait(TERRAIN_DONE, ASSETS_DONE), None);
    }

    /// Both waits carry a real key and a distinct, non-empty label, and the
    /// terrain one is the *same string* the phase already owns rather than a
    /// second copy that could drift.
    #[test]
    fn each_world_wait_names_a_real_vanilla_string() {
        for wait in [WorldWait::Terrain, WorldWait::ApplyingPack] {
            assert!(wait.key().contains('.'), "{wait:?} key must be a real key");
            assert!(!wait.label().is_empty());
        }
        assert_ne!(WorldWait::Terrain.key(), WorldWait::ApplyingPack.key());
        assert_ne!(WorldWait::Terrain.label(), WorldWait::ApplyingPack.label());

        assert_eq!(WorldWait::Terrain.key(), ConnectPhase::LoadingTerrain.key());
        assert_eq!(
            WorldWait::Terrain.label(),
            ConnectPhase::LoadingTerrain.label()
        );
        assert_eq!(WorldWait::ApplyingPack.key(), "multiplayer.applyingPack");
        assert_eq!(WorldWait::ApplyingPack.label(), "Applying resource pack");

        // Only the terrain wait has a real denominator to draw a bar from.
        assert!(WorldWait::Terrain.has_terrain_progress());
        assert!(!WorldWait::ApplyingPack.has_terrain_progress());
    }
}
