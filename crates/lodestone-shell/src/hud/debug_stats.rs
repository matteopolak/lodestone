/// Everything the debug overlay shows for one frame.
#[derive(Debug, Clone, Default)]
pub struct DebugStats {
    /// Player feet position (world blocks).
    pub position: [f64; 3],
    /// Yaw / pitch in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Frames presented in the last completed one-second window — a **count**,
    /// not a reciprocal, matching vanilla's own `Minecraft.runTick` counter.
    /// Deliberately not smoothed: an EMA over a per-second count would lag a
    /// real rate change without making the figure any more stable.
    pub fps: f32,
    /// Time spent *producing* the last frame, in milliseconds — from frame
    /// start to the end of our own submission, which **excludes** the frame
    /// limiter's wait.
    ///
    /// So this is deliberately **not** `1000.0 / fps`, and the two diverge by
    /// exactly the wait whenever a cap is active: at a 10 fps cap a frame that
    /// takes 2 ms of work still leaves ~98 ms of waiting. Reading one as the
    /// other's reciprocal is what hid a counter that reported 20,000 fps under
    /// a 10 fps cap, so the overlay labels them as separate quantities.
    pub frame_ms: f32,
    /// Round-trip time of the latest F3-gated ping reply, in milliseconds.
    /// `None` means no probe has completed yet.
    pub ping_rtt_ms: Option<u64>,
    /// Loaded chunk columns.
    pub chunk_count: usize,
    /// Columns resident in the *live client-owned* world, read through
    /// [`crate::net::NetClient::loaded_chunks`]. Distinct from `chunk_count`
    /// (the locally rendered world): while connected, `0` here is the
    /// chunk-blackout signal, and a non-zero count is the section-read seam
    /// proving live world data is reaching the shell.
    pub live_columns: usize,
    /// Live columns that failed a meshing guard (id spaces disagreed, or block
    /// storage reported non-air data but no section snapshot was eligible).
    /// Deliberately all-air columns are valid and are not counted. Mirrors
    /// [`crate::sim::Sim`]'s `mesh_drops` counter; shown next to `LIVE COLS` so a
    /// recurrence of the silent-drop defect class is visible at a glance.
    /// Healthy sessions read `0`.
    pub mesh_drops: u64,
    /// Mesh sections **drawn this frame** (`RenderStats::sections_drawn`), i.e.
    /// post-cull — not the resident count, which is `RenderState::section_count`.
    /// The doc said "uploaded" and the value never was; the two differ by every
    /// section behind you, so this moves when you turn on the spot and that is
    /// correct for a drawn counter.
    pub section_count: usize,
    /// Quads **drawn this frame** (`RenderStats::total_quads`), post-cull, for
    /// the reason [`Self::section_count`] gives. Residency is
    /// `RenderState::total_quads`.
    pub quads: usize,
    /// Draw calls issued this frame (`RenderStats::draw_calls`), terrain plus a
    /// small per-frame constant (the first-person arm draws even with no pack).
    /// Read it against [`Self::section_count`]: this repo's own evidence
    /// standard is that "3 ms across 60 draws" and "3 ms across 6000" are
    /// different problems, and no timing on this overlay means anything without
    /// the count beside it.
    pub draw_calls: usize,
    /// Sections the distance cull rejected this frame
    /// (`RenderStats::sections_culled_distance`).
    pub sections_culled_distance: usize,
    /// Sections the frustum cull rejected this frame
    /// (`RenderStats::sections_culled_frustum`). The first of the three cull
    /// buckets to move when you turn on the spot, and the one whose *sum* with
    /// the other two plus [`Self::section_count`] should account for every
    /// resident section.
    pub sections_culled_frustum: usize,
    /// Distinct terrain camera bind-group objects bound across the whole frame
    /// (`RenderStats::terrain_camera_bind_group_switches`).
    ///
    /// Expected to be **1**, and flat regardless of how much terrain is
    /// resident — the packed and model paths each share one bind group with a
    /// dynamic offset per section. A number that starts tracking the section
    /// count is the per-section-bind-group regression returning, which is a
    /// shape change no frame-time figure would identify on its own.
    pub terrain_camera_bind_group_switches: usize,
    /// Exact bytes of GPU mesh storage occupied by resident sections
    /// (`RenderStats::vram_bytes`). A pure function of residency: unlike the two
    /// counters above it must **not** move when the camera merely rotates.
    pub vram_bytes: usize,
    /// Bytes of GPU mesh storage the driver is holding, arena blocks whole
    /// (`RenderStats::vram_reserved_bytes`). Always `>= vram_bytes`; shown beside
    /// it because only the pair separates healthy span reuse from fragmentation.
    pub vram_reserved_bytes: usize,
    /// Resident process memory in bytes (0 if unavailable).
    pub rss_bytes: usize,
    /// Heap bytes owned by loaded world chunks (`World::heap_bytes`). Per the
    /// §12.24 ruling this is the single honest world-memory number — it reads
    /// the same whether the world is locally generated or client-owned.
    pub world_bytes: usize,
    /// Physics ticks per rendered frame since start (fixed-timestep health;
    /// vanilla runs 20 ticks/s, so at 50 FPS this settles near 0.4).
    pub frames_per_tick: f32,
    /// The block currently targeted by the view ray, if any.
    pub target: Option<[i32; 3]>,
    /// Entity instances drawn this frame (post-frustum-cull). `0` while
    /// disconnected or when no mobs are in view.
    pub entities_drawn: usize,
    /// Sections in the occlusion graph — [`crate::gpu::RenderStats::
    /// occlusion_graph_sections`], which is strictly **more** than
    /// [`Self::section_count`] because it includes sections with no geometry. A
    /// value that tracks `SECTIONS` instead means the fully-solid sections are
    /// missing and the walk has no floor to see.
    pub occlusion_graph_sections: usize,
    /// Sections the occlusion graph rejected this frame
    /// ([`crate::gpu::RenderStats::sections_culled_occlusion`]).
    ///
    /// **Zero is often correct.** At a near-horizontal camera the frustum has
    /// already removed the subsurface and the graph has nothing left to take;
    /// it only shows up looking steeply down or underground (measured 191 → 59
    /// sections at pitch 75). Read it next to [`Self::occlusion_active`], never
    /// alone.
    pub sections_culled_occlusion: usize,
    /// Sections the walk **would** have culled but drew anyway, because the graph
    /// is in shadow mode ([`crate::gpu::RenderStats::sections_occlusion_shadow`]).
    /// The soak counter: what flipping the cull on would remove, on the world you
    /// are standing in, while nothing can disappear yet.
    pub sections_occlusion_shadow: usize,
    /// Whether the graph is actually culling this frame
    /// ([`crate::gpu::RenderStats::occlusion_active`]).
    ///
    /// **The load-bearing one on this line.** Every failure mode of this cull
    /// draws *more*, so a zero cull count cannot by itself tell an open surface
    /// from a graph that refused to walk — without this flag on screen, a
    /// silently-dead graph looks identical to a correct one on a clear day.
    pub occlusion_active: bool,
    /// Camera walks this **session** (cumulative, not per frame —
    /// [`crate::gpu::RenderStats::occlusion_walks`]).
    ///
    /// Cumulative on purpose: the claim the invalidation cadence makes is that
    /// this does *not* increment while you turn on the spot (8-block cell
    /// crossings, frustum decoupled from reachability), and only a counter read
    /// across two frames can express that. A number rising while you stand still
    /// is a bug, not activity.
    pub occlusion_walks: u64,
    /// Live particles in the simulation this frame.
    pub particles_alive: usize,
    /// Particle billboards actually submitted to the GPU.
    pub particles_drawn: usize,
    /// Live particles whose sprite could not be resolved, so they were not
    /// drawn. Reported rather than dropped silently: a zero draw count against
    /// a non-zero alive count is exactly the "renders nothing, reports fine"
    /// state this counter exists to make visible.
    pub particles_unresolved: usize,
    /// Columns the weather pass uploaded this frame
    /// (`RenderState::weather_columns`), i.e. what the next frame will submit.
    /// `0` in clear weather or with no pass installed.
    pub weather_columns: usize,
    /// Of [`Self::weather_columns`], how many are rain rather than snow
    /// (`RenderState::weather_rain_columns`). Shown beside it for the same
    /// reason `SECTIONS`/`occlusion_graph_sections` are shown together: "rain
    /// is not falling" and "no columns were extracted" look identical without
    /// both numbers on screen.
    pub weather_rain_columns: usize,
    /// A short connection/status line ("local world", "connecting…", …).
    pub status: String,
    /// The world difficulty and lock state, as the server last reported it
    /// (`Sim::difficulty`) — `None` until the first report arrives.
    /// `ServerDifficulty` reached a real, tested ECS fold in `44485e4` but
    /// nothing in the shell read it; this is that last hop.
    pub difficulty: Option<(lodestone_model::Difficulty, bool)>,
    /// The server-reported simulation distance, in chunks. Unlike render
    /// distance, this is an authoritative server decision; `None` means the
    /// current session has not received that packet.
    pub simulation_distance: Option<i32>,
    /// The public message of the day the connected server announced during
    /// play, flattened to one line for F3. `None` means no packet has arrived.
    pub server_motd: Option<String>,
    /// The latest server-announced local combat state. `None` means neither
    /// combat packet arrived, rather than an inferred idle state.
    pub combat_session: Option<lodestone_ecs::CombatSession>,
    /// Sky and block light at the player's feet, as the client's own world
    /// reports them — `None` before login or for an unloaded section, which is
    /// the honest "no data" state and is drawn as such.
    ///
    /// There is no "light-level pie chart" to draw: **26.2 does not have one.**
    /// `DebugScreenEntries` registers a `minecraft:light_levels` *text* entry
    /// (`DebugEntryLight`) that prints `Client Light: <raw> (<sky> sky, <block>
    /// block)`, and the pie was removed. So this reproduces the entry that
    /// actually exists rather than a chart that no longer does — see
    /// `docs/debug-overlay.md`.
    ///
    /// `(sky, block)`, each `0..=15`.
    pub light: Option<(u8, u8)>,
    /// Fixed lines describing the graphics adapter and backend, resolved **once**
    /// from `wgpu::Adapter::get_info()` when the GPU comes up.
    ///
    /// The owner's steer on this overlay was "not 1:1 — show information that is
    /// useful for *this* implementation", and this is the concrete half of it:
    /// vanilla's right column is full of JVM-shaped fields (GC, Java version,
    /// allocation rate) that have no analogue here, and printing a number we do
    /// not have is the same fabrication `menu::options` already refuses for an
    /// option we do not honour. The adapter, its backend and its reported limits
    /// *are* true of this client, and one of them has already caused a crash
    /// class: `max_bind_groups` reads 4 in a browser and 8 on this Mac, which is
    /// why the model shader is pinned at four groups.
    ///
    /// Empty off a GPU-less run (every headless gate), which draws no lines
    /// rather than placeholders.
    pub adapter: Vec<String>,
    /// The dimension the local player is in, as the server named it —
    /// `minecraft:overworld`, `minecraft:the_nether`, or a data pack's own id.
    ///
    /// The last line of vanilla's own position debug group is the dimension
    /// identifier followed by the loaded-chunk count, and
    /// this is its identifier half. Read from the local player's
    /// `lodestone_ecs::session::ServerDimension` component in `sim/step.rs`, so
    /// it follows a portal trip: that fold updates on `Respawned` as well as
    /// `Login`, which is the whole reason the too-bright-Nether bug is fixed.
    ///
    /// **`None` before login draws no line at all**, matching vanilla — whose
    /// entire `position` group is absent when there is no camera entity. It is
    /// not `-`, because unlike `Difficulty:` or `Client Light:` vanilla's line
    /// has no prefix to hang a placeholder off.
    pub dimension: Option<String>,
    /// Whether the F3+B entity-hitbox overlay is on.
    ///
    /// Mirrors `WindowApp::debug_hitboxes`, the `Arc<AtomicBool>` the world-line
    /// source closure actually reads. **Copied per frame rather than derived from
    /// a local guess** — the `Debug overlays:` line exists to report the state
    /// that decides whether boxes draw, and a second source of truth for it is
    /// how a hint that lies gets shipped.
    pub hitboxes_shown: bool,
    /// Whether the F3+G chunk-border overlay is on. See
    /// [`Self::hitboxes_shown`].
    pub chunk_borders_shown: bool,
    /// Per-phase CPU frame-timing lines (`app::frame_profile::PhaseSummary::line`)
    /// plus the GPU-timing line, both formatted by `app::redraw` — see
    /// `docs/frame-profiling.md`. Empty until the first frame has run once
    /// (nothing to report yet), which draws no extra F3 lines rather than
    /// placeholders.
    pub frame_profile: Vec<String>,
    /// The F3+Shift profiler pie chart — `None` when the chart is toggled off
    /// (`app::WindowApp::show_profiler_chart`) or the debug overlay itself is
    /// closed. Built alongside [`Self::frame_profile`] in `app::redraw` from
    /// the same [`crate::app::frame_profile::FrameProfiler::summary`]/GPU
    /// report, so the text lines and the chart can never disagree about a
    /// frame's numbers. See `docs/frame-profiling.md`'s "Pie chart" section.
    pub profiler_chart: Option<ProfilerChart>,
}

/// Render-ready data for the F3+Shift profiler pie chart — see
/// [`DebugStats::profiler_chart`]. Vanilla's shape
/// (`Minecraft.renderFpsMeter`): a filled pie, one wedge per section, with a
/// legend and a darker lower half; number keys drill into a child section and
/// `0` returns to the root. Re-derived rather than transliterated — see
/// `docs/frame-profiling.md` for what is the same shape and what goes beyond
/// it (GPU segments and skip counts alongside the CPU wedges, which vanilla's
/// single-threaded profiler has no equivalent of).
#[derive(Debug, Clone)]
pub struct ProfilerChart {
    /// One wedge per CPU frame phase, in
    /// [`crate::app::frame_profile::FramePhase::ALL`] order — the pie's root
    /// level. `mean_ms` (not `p99_ms`) sizes the wedges: vanilla's own pie
    /// sizes by the profiler's accumulated *total* time per section, and mean
    /// is this instrument's steady-state analogue: see [`ProfilerChartSlice`].
    pub slices: Vec<ProfilerChartSlice>,
    /// The drilled-in wedge index (`0..slices.len()`), or `None` at the root.
    /// Vanilla's number-key/`0` navigation, ported as an F3 chord
    /// (`app::input::KeyOutcome::ProfilerChartSelect`) rather than a bare
    /// number press — the number row is already the (rebindable) hotbar
    /// selector, so vanilla's own un-chorded literal key would collide with
    /// it constantly instead of only while F3 is held.
    pub selected: Option<usize>,
    /// GPU segment name → milliseconds, in
    /// `gpu::gpu_timing::GpuQueryTimer::results_ms` order. `None` means "no
    /// reading yet", never a fabricated `0.0` — the same contract that
    /// method's own doc requires of every caller. Empty when the device lacks
    /// `Features::TIMESTAMP_QUERY`; [`Self::gpu_unavailable`] tells the two
    /// cases apart so the chart can say "unavailable" instead of drawing an
    /// empty legend.
    pub gpu: Vec<(&'static str, Option<f32>)>,
    /// Whether GPU timing itself is unavailable on this device (distinct from
    /// "no reading yet" — see [`Self::gpu`]'s doc).
    pub gpu_unavailable: bool,
    /// Frames where a GPU readback slot was still outstanding when due for
    /// reuse (`gpu::gpu_timing::GpuQueryTimer::stalled_frames`) — real
    /// GPU/driver backpressure, surfaced next to the chart rather than folded
    /// into it.
    pub gpu_stalled_frames: u64,
}

/// One wedge's worth of data — the pie-chart sibling of
/// `app::frame_profile::PhaseSummary`, carrying the same fields (this module
/// cannot depend on `app`'s private `PhaseSummary` type directly, so
/// `app::redraw` copies the fields across) plus nothing else: no colour here,
/// because colour is assigned by wedge *position* at draw time
/// (`hud::PROFILER_CHART_COLORS`), matching vanilla's own preferred-colour lookup
/// hashing a section's identity rather than storing a colour on the model.
#[derive(Debug, Clone, Copy)]
pub struct ProfilerChartSlice {
    pub name: &'static str,
    pub mean_ms: f32,
    pub p95_ms: f32,
    pub p99_ms: f32,
    pub samples: usize,
    pub window: usize,
    pub skipped: u64,
}

impl ProfilerChart {
    /// Sum of every wedge's [`ProfilerChartSlice::mean_ms`] — the pie's whole
    /// circle. A frame profiler's phases are sequential checkpoints inside one
    /// `redraw`, not a sampled subset, so this total *is* the frame's own mean
    /// wall-clock cost (modulo the pacer's wait, exactly as
    /// `DebugStats::frame_ms`'s own doc already distinguishes from `1000.0 /
    /// fps`) — never re-derived from `fps`, which would silently disagree the
    /// moment a frame cap is active.
    #[must_use]
    pub fn total_mean_ms(&self) -> f32 {
        self.slices.iter().map(|s| s.mean_ms).sum()
    }
}

/// Display name for a [`lodestone_model::Difficulty`] — vanilla's own
/// serialized keys (`Difficulty`'s `PEACEFUL(0, "peaceful")` … `HARD(3,
/// "hard")`), lowercase.
///
/// **Not the translated `options.difficulty.*` component**, which this overlay
/// has no translation table to draw from (see the module doc's "jar-less"
/// path). Lowercase rather than shouted because that is the F3 overlay's own
/// convention for an enum: `DebugEntryPosition` prints its own direction's
/// to-string,
/// which is the lowercase `name`, and the dimension as `minecraft:overworld`.
pub(super) fn difficulty_name(d: lodestone_model::Difficulty) -> &'static str {
    match d {
        lodestone_model::Difficulty::Peaceful => "peaceful",
        lodestone_model::Difficulty::Easy => "easy",
        lodestone_model::Difficulty::Normal => "normal",
        lodestone_model::Difficulty::Hard => "hard",
    }
}

/// `DebugScreenOverlay.formatChart` — `formatKeybind(…) + " " + name + " " +
/// (status ? "visible" : "hidden")`, with `formatKeybind` bracketing the chord as
/// `"[" + modifier + "+" + key + "]"`.
///
/// The chord arrives as a literal (`"F3+B"`) rather than as a lookup, because
/// unlike vanilla's these two are not `KeyMapping`s — they are hardcoded in
/// `app/input.rs` behind the `KeyGate::debug_held` flag, so there is no
/// `getTranslatedKeyMessage` to ask and no unbound case to handle. **If they ever
/// become rebindable this must read the binding**, or the hint will name the old
/// key with total confidence.
pub(super) fn format_toggle(chord: &str, name: &str, shown: bool) -> String {
    format!(
        "[{chord}] {name} {}",
        if shown { "visible" } else { "hidden" }
    )
}

/// `Mth.wrapDegrees(float)` — `angle % 360`, pulled into `[-180, 180)`.
///
/// `DebugEntryPosition` wraps both angles before printing them, so a player who
/// has spun twice reads `-12.3` rather than `708.0`. Rust's `%` and Java's `%`
/// agree on sign for floats, so this is the same two branches.
pub(super) fn wrap_degrees(angle: f32) -> f32 {
    let mut wrapped = angle % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

impl DebugStats {
    /// Compass facing derived from yaw (Minecraft convention: yaw 0 = south, +Z).
    #[must_use]
    pub fn facing(&self) -> &'static str {
        // Normalise to [0,360).
        let y = self.yaw.rem_euclid(360.0);
        // south=0/360, west=90, north=180, east=270 (yaw increases clockwise).
        match y {
            v if !(45.0..315.0).contains(&v) => "south (+Z)",
            v if v < 135.0 => "west (-X)",
            v if v < 225.0 => "north (-Z)",
            _ => "east (+X)",
        }
    }

    /// The two halves of vanilla's `Facing:` line — its own direction to-string (the
    /// lowercase enum `name`) and `DebugEntryPosition`'s own `faceString`.
    ///
    /// The thresholds are [`Self::facing`]'s, which are already vanilla's:
    /// `Direction.fromYRot` is `from2DDataValue(floor(yRot / 90 + 0.5) & 3)`
    /// with `0 = SOUTH, 1 = WEST, 2 = NORTH, 3 = EAST`, and that flips exactly
    /// at yaw 45/135/225/315. Kept separate from `facing` because that method's
    /// `south (+Z)` shorthand is [`Self::one_line`]'s stdout format and is not
    /// what the overlay draws.
    #[must_use]
    pub fn facing_parts(&self) -> (&'static str, &'static str) {
        let y = self.yaw.rem_euclid(360.0);
        match y {
            v if !(45.0..315.0).contains(&v) => ("south", "Towards positive Z"),
            v if v < 135.0 => ("west", "Towards negative X"),
            v if v < 225.0 => ("north", "Towards negative Z"),
            _ => ("east", "Towards positive X"),
        }
    }

    /// The player's block position — vanilla's own block-position accessor, i.e. `Mth.floor`
    /// of each coordinate.
    ///
    /// **Not `as i64`.** A cast truncates toward zero, so it maps `-0.5` to `0`
    /// and puts a player just west of the origin in chunk `0` instead of chunk
    /// `-1`; every line below that divides or masks a coordinate inherits the
    /// error, and it is invisible at the origin.
    #[must_use]
    pub fn block_position(&self) -> [i64; 3] {
        [
            self.position[0].floor() as i64,
            self.position[1].floor() as i64,
            self.position[2].floor() as i64,
        ]
    }

    /// The overlay's text lines, in one flat list.
    ///
    /// Kept as the concatenation of [`Self::left_lines`], [`Self::right_lines`]
    /// and [`Self::profile_lines`] so nothing that wanted "every line" has to
    /// know about the block split, and so the draw cannot silently drop a line:
    /// adding one to any block changes this too. The profile block joined this
    /// concatenation on the same day it stopped being part of the right column
    /// — a block that is drawn but not listed here is exactly the island this
    /// function exists to prevent.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut all = self.left_lines();
        all.extend(self.right_lines());
        all.extend(self.profile_lines());
        all
    }

    /// The **left** column: the player and the world around them.
    ///
    /// # How the column split was decided
    ///
    /// An earlier note here recorded a *deliberate* refusal to follow vanilla,
    /// on the grounds that "vanilla's split is mechanical, and a mechanical
    /// halve would reshuffle both columns every time a line is added".
    /// **That is superseded.** The premise was half right and the conclusion
    /// does not follow from it. `DebugScreenOverlay.extractRenderState` does not
    /// halve *lines*; it halves within three **categories**, and the categories
    /// are semantic:
    ///
    /// | category | how a line gets there | how it is placed |
    /// |---|---|---|
    /// | priority | `addPriorityLine` | into whichever column is currently shorter |
    /// | regular | `addLine` | the flat list halved at `mid = (n + 1) / 2` |
    /// | group | `addToGroup(id, …)` | whole named groups, halved by *group count* |
    ///
    /// So the thing that decides a line's column is which category its entry
    /// used, and each category block is separated from the next by a `""`
    /// spacer. Reproducing *that* is what makes the overlay look like vanilla's,
    /// and it is stable in exactly the way the old note wanted: adding a line to
    /// a group cannot move any other line across columns.
    ///
    /// What is **not** reproduced is running vanilla's halve over *our* entry
    /// set, because our set differs (no JVM entries, extra engine ones) and the
    /// arithmetic would then put `XYZ:` on the right — further from vanilla's
    /// screen, not closer. The category→column assignment below is therefore
    /// still by hand, but it is now *derived from vanilla's own default-profile
    /// output* rather than chosen freely: with `DebugScreenProfile.DEFAULT` the
    /// enabled entries are `3d_crosshair`, `fps`, `game_version`, `memory`,
    /// `player_position`, `player_section_position`,
    /// `simple_performance_impactors`, `system_specs` and `tps` (sorted by
    /// `Identifier.compareTo`, which compares *path* first), and vanilla's
    /// algorithm puts the fps line, the perf-impactor lines, the memory group
    /// and the position group on the **left**, and the version line, the tps
    /// line and the system group on the **right**. Ours match that placement.
    ///
    /// Order *within* a column is vanilla's, and so are the format strings —
    /// see `docs/debug-overlay.md` for the per-line ported/replaced/dropped
    /// table.
    #[must_use]
    pub fn left_lines(&self) -> Vec<String> {
        let [bx, by, bz] = self.block_position();
        // `ChunkPos.containing` / `SectionPos.blockToSectionCoord`, both `>> 4`.
        let (cx, cy, cz) = (bx >> 4, by >> 4, bz >> 4);
        let (facing, face_hint) = self.facing_parts();
        let mut out = vec![
            // `DebugEntryFps`: `"%d fps T: %s%s"`, a *priority* line, and the
            // first one added — so it lands left, because `addPriorityLine`
            // fills the shorter column and both start empty.
            //
            // `T:` is the framerate-limit target and the parenthetical after it
            // is the swapchain present mode. This shell now honours both (see
            // `app::pacing::effective_target_fps` and `Options::enable_vsync`),
            // so porting the `T:` half is no longer blocked on fabricating a
            // limit we do not enforce — it is simply unported, and wants the
            // target threaded onto `DebugStats` alongside the two fields here.
            //
            // The slot meanwhile carries the frame time we measure, labelled
            // `work` because it is **not** `1000 / fps`: it excludes the
            // limiter's wait, so under a cap the two legitimately disagree.
            // Leaving it unlabelled invited exactly that misreading — see both
            // fields' own docs.
            format!("{:.0} fps ({:.2} ms work)", self.fps, self.frame_ms),
            self.ping_rtt_ms
                .map_or_else(|| "Ping: -".to_string(), |ms| format!("Ping: {ms} ms")),
            String::new(),
            // `DebugEntryLight`'s group, verbatim: `"Client Light: " +
            // rawBrightness + " (" + sky + " sky, " + block + " block)"`.
            // `getRawBrightness` is the max of the two, which is what the
            // renderer actually samples.
            match self.light {
                Some((sky, block)) => {
                    format!("Client Light: {} ({sky} sky, {block} block)", sky.max(block))
                }
                None => "Client Light: -".to_string(),
            },
            // Vanilla's neighbouring entry is `DebugEntryLocalDifficulty`,
            // `"Local Difficulty: %.2f // %.2f"` — a *server*-side scalar folded
            // from inhabited time and moon brightness, which we do not compute.
            // This is the world difficulty the server reported instead, so the
            // prefix deliberately omits `Local`.
            match self.difficulty {
                Some((d, locked)) => format!(
                    "Difficulty: {}{}",
                    difficulty_name(d),
                    if locked { " (locked)" } else { "" }
                ),
                None => "Difficulty: -".to_string(),
            },
            String::new(),
            // `DebugEntryPosition`'s group. The four format strings are
            // vanilla's, including the asymmetric `%.3f / %.5f / %.3f` (Y gets
            // five places because a step height or a fluid offset lives in the
            // fourth), the `r.X.Z.mca` region hint, and the `%02d` pad on the
            // section-relative triple.
            format!(
                "XYZ: {:.3} / {:.5} / {:.3}",
                self.position[0], self.position[1], self.position[2]
            ),
            format!("Block: {bx} {by} {bz}"),
            format!(
                "Chunk: {cx} {cy} {cz} [{} {} in r.{}.{}.mca]",
                cx & 31,
                cz & 31,
                cx >> 5,
                cz >> 5
            ),
            format!(
                "Facing: {facing} ({face_hint}) ({:.1} / {:.1})",
                wrap_degrees(self.yaw),
                wrap_degrees(self.pitch)
            ),
        ];
        // The fifth and last line `DebugEntryPosition` adds to its group is
        // `level.dimension().identifier() + " FC: " + chunks.size()`. The
        // identifier is real here; `FC` is `ServerLevel.getForceLoadedChunks`,
        // which the client has no view of, so the suffix is dropped rather than
        // printed as a `0` we did not measure. Absent rather than `-` before
        // login: vanilla omits its whole position group when there is no camera
        // entity, and this line has no prefix to hang a placeholder off.
        if let Some(dimension) = &self.dimension {
            out.push(dimension.clone());
        }
        out.extend([
            // `DebugEntrySectionPosition`, which joins the *position* group and
            // is therefore drawn after everything `DebugEntryPosition` added.
            format!("Section-relative: {:02} {:02} {:02}", bx & 15, by & 15, bz & 15),
            // `DebugEntryLookingAt.BlockStateInfo`'s first line, whose prefix is
            // the literal `"Targeted Block"` and whose separators are commas.
            // The block state and its properties are the rest of that group and
            // are not plumbed here — see the doc's table.
            match self.target {
                Some([x, y, z]) => format!("Targeted Block: {x}, {y}, {z}"),
                None => "Targeted Block: -".to_string(),
            },
            String::new(),
            // Vanilla closes the left column with its chart-keybind block, gated
            // on `isOverlayVisible()`:
            //
            //   Debug charts: [F3+2] Profiler hidden; [F3+1] FPS + TPS hidden;
            //   [F3+3] Ping hidden; [F3+4] Lightmap hidden
            //   To edit: press [F3+I]
            //
            // built from `formatChart` = `[mod+key] Name visible|hidden`. None of
            // those four charts exists here, but the two world overlays that do
            // are toggled by exactly this kind of chord and had no on-screen
            // state at all — so this is vanilla's shape carrying our real
            // toggles. `To edit:` is dropped: there is no entry-enable screen to
            // point at, and a hint naming a chord that does nothing is worse than
            // no hint.
            //
            // The booleans are copied per frame from the `Arc<AtomicBool>`s the
            // draw itself reads (`WindowApp::debug_hitboxes` /
            // `debug_chunk_borders`, flipped in `app/lifecycle.rs` and consumed
            // by `install_debug_lines_source`'s closure), never re-derived — the
            // line's whole job is to report the state that decides whether the
            // boxes draw.
            format!(
                "Debug overlays: {}; {}",
                format_toggle("F3+B", "Hitboxes", self.hitboxes_shown),
                format_toggle("F3+G", "Chunk borders", self.chunk_borders_shown)
            ),
        ]);
        out
    }

    /// The **right** column: the client's identity, then the render engine —
    /// where vanilla puts its version line, its server/tps line and its
    /// `system` group.
    ///
    /// See [`Self::left_lines`] for why each block sits in this column.
    #[must_use]
    pub fn right_lines(&self) -> Vec<String> {
        let mut out = vec![
            // `DebugEntryVersion`'s priority line, `"Minecraft " + version +
            // " (" + launched + "/" + brand + ")"`. It is the *second* priority
            // line added, so vanilla's shorter-column rule sends it right.
            format!("Lodestone {}", env!("CARGO_PKG_VERSION")),
            String::new(),
        ];
        // `DebugEntryTps`'s slot — `"\"%s\" server%s, %.0f tx, %.0f rx"` remote,
        // `"Integrated server @ %.1f/%.1f ms…"` in singleplayer. We have neither
        // a smoothed server tick time nor packet-rate counters, so this carries
        // the session status ("local world", "connecting…").
        //
        // Skipped when empty because vanilla's entry adds nothing at all with no
        // connection — pushing the blank instead would put two spacers in a row,
        // which draws as a double gap rather than as an absent line.
        if !self.status.is_empty() {
            out.push(self.status.clone());
        }
        if let Some(motd) = &self.server_motd {
            out.push(format!("MOTD: {motd}"));
        }
        if let Some(combat) = self.combat_session {
            let line = match combat {
                lodestone_ecs::CombatSession::Active => "Combat: active".to_owned(),
                lodestone_ecs::CombatSession::Ended { duration_ticks } => {
                    format!("Combat: ended ({duration_ticks} ticks)")
                }
            };
            out.push(line);
        }
        out.extend([
            // `LevelExtractor.sectionStatistics`, `"C: %d/%d %sD: %d, %s"` —
            // rendered sections over total, then the view distance and the
            // dispatcher's queue. Ours is drawn-over-graph-nodes (the occlusion
            // graph is the closest thing here to vanilla's own view-area size accessor),
            // plus the two counters vanilla has no field for.
            format!(
                "C: {}/{} sections, {} columns, {} quads",
                self.section_count, self.occlusion_graph_sections, self.chunk_count, self.quads
            ),
            // The entity statistic includes the server's simulation-distance
            // scalar. We track only drawn entities, but preserve `SD` after the
            // packet actually arrives rather than guessing it from render/view
            // distance.
            self.simulation_distance.map_or_else(
                || format!("E: {}", self.entities_drawn),
                |distance| format!("E: {}, SD: {distance}", self.entities_drawn),
            ),
            // `DebugEntryParticleRenderStats`, `"P: " + countParticles()`. The
            // unresolved count is ours and stays on the line: a zero draw
            // against a non-zero alive count is the "renders nothing, reports
            // fine" state that counter exists to expose.
            format!(
                "P: {}/{}, {} unresolved",
                self.particles_drawn, self.particles_alive, self.particles_unresolved
            ),
            // No vanilla counterpart: the weather pass's own instrument, added
            // for the same reason `Occl`'s `active` flag is — "rain is not
            // falling" and "no columns reached the pass" are indistinguishable
            // on screen without both numbers. `0/0` in clear weather is correct.
            format!(
                "Weather cols: {}, rain: {}",
                self.weather_columns, self.weather_rain_columns
            ),
            String::new(),
            // No vanilla counterpart from here to the memory block: these are
            // this engine's own instruments, in a group of their own the way
            // `addToGroup` would give them one.
            //
            // `F/T` is fixed-timestep health (vanilla runs 20 ticks/s, so at
            // 50 fps this settles near 0.4). `Live cols`/`drops` is the
            // silent-mesh-drop detector. `Occl`'s `active`/`off` is the
            // load-bearing token — see `DebugStats::occlusion_active`: the other
            // four numbers cannot tell an open surface from a dead graph,
            // because every failure mode of that cull draws *more*.
            format!("F/T: {:.2}", self.frames_per_tick),
            format!(
                "Live cols: {}, drops: {}",
                self.live_columns, self.mesh_drops
            ),
            format!(
                "Occl: {}, nodes: {}, cull: {}, shadow: {}, walks: {}",
                if self.occlusion_active { "active" } else { "off" },
                self.occlusion_graph_sections,
                self.sections_culled_occlusion,
                self.sections_occlusion_shadow,
                self.occlusion_walks
            ),
            String::new(),
            // Vanilla's own memory group is three JVM heap
            // lines (`Mem:`, `Allocation rate:`, `Allocated:`) plus
            // a heap/non-heap pair from a more detailed variant. All five are JVM
            // facts and none has an analogue here, so the group is rebuilt from
            // the three real numbers this process can measure. `Mem:` keeps
            // vanilla's prefix but drops its trailing percentage — that percentage is
            // used memory over the JVM's configured heap ceiling and there is no such ceiling to divide by.
            format!("Mem: {} MiB (RSS)", self.rss_bytes / (1024 * 1024)),
            format!("World: {} KiB", self.world_bytes / 1024),
            // `Mesh VRAM live/reserved`: the first is the spans handed out to
            // resident sections, the second the arena blocks the driver is
            // holding. Both are residency figures measured off the real
            // `wgpu::Buffer` sizes — a camera rotation must leave this line
            // unchanged, which is what distinguishes real load/unload churn from
            // the cull-derived estimate this used to print. KiB rather than
            // vanilla's MiB on purpose: the live figure's sawtooth is the signal
            // and MiB granularity flattens it.
            format!(
                "Mesh VRAM: {}/{} KiB",
                self.vram_bytes / 1024,
                self.vram_reserved_bytes / 1024
            ),
        ]);
        if !self.adapter.is_empty() {
            // Vanilla's own system group is `Java:`,
            // `CPU:`, `Display:`, the device name and the backend/driver pair.
            // The first is dropped as a JVM fact; the rest of this block is the
            // adapter, its backend and its reported limits, which are true of
            // this client and are what the group is *for*. Empty lines are
            // skipped by the draw, which is what makes the spacer a gap rather
            // than an empty plate.
            out.push(String::new());
            out.extend(self.adapter.iter().cloned());
        }
        out
    }

    /// The **frame-profile block**: where the frame went, as its own group
    /// below the left column rather than tacked onto the right one.
    ///
    /// It moved for a measured reason. These lines are the widest thing on the
    /// screen by a long way — `world_encode_submit` carries a bracketed
    /// breakdown of four `world.*` sub-phases with a mean/p95/p99 each plus the
    /// section counts — and the right column is *right*-aligned, so a long line
    /// there grows leftwards across the left column and its continuation rows
    /// come out ragged-left, which is the hardest way to read a table of
    /// numbers. Left-aligned they grow rightwards into empty canvas, and
    /// [`debug_overlay::fit_line`] breaks them at the profiler's own `", "`
    /// separators, so each `world.*` sub-phase lands on its own indented row
    /// under the phase it belongs to.
    ///
    /// The strings themselves are **not** built here: they are
    /// [`Self::frame_profile`], formatted by `app::redraw` from
    /// `app::frame_profile`'s summary and the GPU timer. This function only
    /// decides the group's heading and that it is a group — re-deriving a
    /// number the profiler already formatted would be a second source of truth
    /// for it. Empty (no heading, no spacer) until the first frame has run, so
    /// the block is absent rather than a placeholder.
    ///
    /// The heading deliberately names no units: the CPU phase lines and the
    /// `gpu …` lines below them do not share a format, and a heading that
    /// described only the first would be wrong about the rest.
    #[must_use]
    pub fn profile_lines(&self) -> Vec<String> {
        if self.frame_profile.is_empty() {
            return Vec::new();
        }
        let mut out = vec![String::new(), "Frame profile:".to_string()];
        out.extend(self.frame_profile.iter().cloned());
        out
    }

    /// One-line stdout summary (primary evidence in headless / logged runs).
    #[must_use]
    pub fn one_line(&self) -> String {
        format!(
            "pos=({:.1},{:.1},{:.1}) facing={} f/t={:.2} target={} fps={:.0} frame={:.2}ms chunks={} live_cols={} drops={} entities={} particles={}/{}+{}unres sections={} quads={} vram={}/{}KB world={}KB rss={}MB {}",
            self.position[0],
            self.position[1],
            self.position[2],
            self.facing(),
            self.frames_per_tick,
            match self.target {
                Some([x, y, z]) => format!("{x},{y},{z}"),
                None => "-".to_string(),
            },
            self.fps,
            self.frame_ms,
            self.chunk_count,
            self.live_columns,
            self.mesh_drops,
            self.entities_drawn,
            self.particles_drawn,
            self.particles_alive,
            self.particles_unresolved,
            self.section_count,
            self.quads,
            self.vram_bytes / 1024,
            self.vram_reserved_bytes / 1024,
            self.world_bytes / 1024,
            self.rss_bytes / (1024 * 1024),
            self.status,
        )
    }
}

/// Resident set size (physical memory) of this process in bytes, or 0 only if
/// the platform genuinely cannot report it.
///
/// Reads the real per-process figure via [`memory_stats`], which uses
/// `task_info` on macOS and `/proc/self/statm` on Linux. The shell stays within
/// the workspace's `deny(unsafe_code)` because the syscall FFI lives inside that
/// crate. Previously this returned 0 on every non-Linux host, so the HUD's
/// memory gauge read a flat zero on macOS — a signal that looks like evidence
/// and isn't (§12). The [`rss_is_observable`](tests) test guards against a
/// regression back to that.
/// On wasm32 there is no process and no `task_info`, but there **is** a real
/// figure with the same meaning: the module's linear memory, which is the whole of
/// its heap and the only thing it can grow. `memory_size(0)` returns it in 64 KiB
/// pages. That is a genuine measurement rather than a stub — which matters here
/// specifically, because this function's whole history is that returning a flat 0
/// made the gauge look like evidence when it was not (§12), and a browser stub
/// would have reintroduced exactly that.
///
/// It is not identical to native RSS: linear memory is *reserved* address space
/// that the engine has committed, so it never shrinks after a
/// `memory.grow`, whereas RSS can fall. Read it as a high-water mark.
#[must_use]
pub fn process_rss_bytes() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        memory_stats::memory_stats().map_or(0, |m| m.physical_mem)
    }
    #[cfg(target_arch = "wasm32")]
    {
        /// wasm's page size, fixed by the spec at 64 KiB.
        const WASM_PAGE_BYTES: usize = 65536;
        core::arch::wasm32::memory_size(0) * WASM_PAGE_BYTES
    }
}
