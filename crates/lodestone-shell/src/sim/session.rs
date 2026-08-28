//! `Sim`'s **session lifecycle and the session scalars every HUD read pulls
//! off it** -- seam 10 of the sim.rs decomposition
//! sequence. Seam 1 was the test module, `sim/tests.rs`; 2 placement
//! prediction, `sim/placement.rs`; 3 the interaction/combat cluster,
//! `sim/actions.rs`; 4 the per-tick net-apply fold, `sim/net_apply.rs`; 5 the
//! audio cluster, `sim/audio.rs`; 6 the camera cluster, `sim/camera.rs`; 7
//! chunk/mesh streaming, `sim/meshing.rs`; 8 the `audio` *field* out of the
//! struct into the `AudioEngine` resource -- a field dissolution rather than a
//! file split, but `docs/sim-dissolution.md` numbers it in the same sequence,
//! so these five are 9-13. Seams 9-13 landed together.
//!
//! **`sim/meshing.rs`'s own module doc calls seam 7 "the last of the sim.rs
//! decomposition sequence".** That was true when it was written and is not now.
//! It is left exactly as it stands, because this split is a pure move and
//! editing a neighbour's prose is not part of one -- recorded here instead so a
//! reader who arrives through that file is not misled, and in
//! `docs/sim-dissolution.md`, which carries the authoritative seam list.
//!
//! Two halves that belong together because the second is only meaningful
//! while the first holds:
//!
//! * **Lifecycle**: [`Sim::connect`]/[`Sim::attach_net`] up, `end_session`
//!   down, the [`SessionPhase`] accessors between them, and the death /
//!   respawn / win latches that ride a session rather than a process.
//! * **Scalars**: health, food, saturation, air, experience, the tab list,
//!   scoreboard, boss bars, the title / action-bar / held-item overlays, the
//!   attack-strength cooldown, the folded menus, the hotbar selection and the
//!   reported difficulty. Every one is a read of a component the net thread
//!   folded; none of them own state.
//!
//! `end_session`'s long doc comment is the reason to keep the two together:
//! it is a hand-written list of *exactly* what a teardown resets, and it is
//! only auditable beside the accessors that would otherwise leak the previous
//! server's values into the next session. That comment already records one
//! stale-but-true-when-written claim it had to correct; splitting the list
//! away from the readers would make the next such drift invisible.
//!
//! # What widened, and why each had to
//!
//! Three methods go from private to `pub(crate)`, all for the same reason the
//! earlier seams' did -- a private item in a child module is invisible to its
//! *siblings*, and privacy only cascades downward:
//!
//! * `set_phase` -- `sim/net_apply.rs` sets the phase from five `NetUpdate`
//!   arms;
//! * `server_entity_id` -- `sim/net_apply.rs`'s two "is this packet about
//!   us?" checks, plus five reads in `sim/tests.rs`;
//! * `attack_strength_scale_at` -- `sim/actions.rs`'s
//!   `maybe_spawn_crit_particles` needs the `a = 0.5` form vanilla's
//!   `fullStrengthAttack` gate uses.
//!
//! `vitals`, `attack_strength_delay` and `send_selected_slot` stay **private**:
//! every caller of each is in this file. `attack_strength_delay` is named by a
//! doc link in `sim/actions.rs`, which resolves through the type and is not a
//! call, so it needs no visibility of its own.
//!
//! `use super::*;` for the same reason every earlier seam file uses it: this
//! module is a *descendant* of `sim`, so it already has the same visibility
//! into `Sim`'s private fields, into `sim.rs`'s remaining private helpers and
//! into everything `sim.rs` re-exports that `sim::tests` has always had, with
//! no need to enumerate any of it.

use super::*;

// Named rather than reached through the glob: `sim.rs` does not import it, and
// the title/action-bar accessors below return spans so a reader can see where the
// type comes from.
use lodestone_model::text::TextSpan;

impl Sim {
    /// Open a live connection to `host:port` and attach it, threading this `Sim`'s
    /// one `World` into the client so ingest folds where these systems read.
    ///
    /// This is the §4.1(c) wiring, and it is a `Sim` method rather than three lines
    /// at every call site because getting it wrong is silent: a `NetClient` built
    /// without the handle gets a `World` of its own, the session fold lands in it,
    /// and every HUD read here returns an empty default. Prefer this over
    /// [`Self::attach_net`], which exists for a client that has no connection to
    /// share (the loopback test double).
    ///
    /// Joins as the persisted "Play offline" identity — see
    /// [`NetClient::connect`]. A **live gate must use [`Self::connect_as`]**
    /// instead: a shared offline name is a shared player file, and a dead player
    /// is held on the death screen, which sends no chunks.
    pub fn connect(&mut self, host: String, port: u16, protocol: i32) {
        let net = NetClient::connect(
            host,
            port,
            protocol,
            Some((Arc::clone(&self.ecs), self.local)),
        );
        self.attach_net(net);
    }

    /// As [`Self::connect`], but joining under `username` rather than the
    /// persisted offline identity — [`NetClient::connect_as`] with this `Sim`'s
    /// `World` threaded in. For live gates, which need a fresh name per run.
    pub fn connect_as(&mut self, host: String, port: u16, protocol: i32, username: String) {
        let net = NetClient::connect_as(
            host,
            port,
            protocol,
            Some((Arc::clone(&self.ecs), self.local)),
            username,
        );
        self.attach_net(net);
    }

    /// Attach a live connection whose updates are polled each frame.
    // `mut` is used only by the `#[cfg(test)]` `bind_session` below.
    #[cfg_attr(not(test), allow(unused_mut))]
    pub fn attach_net(&mut self, mut net: NetClient) {
        // Stop any previous connection before clearing its state. Dropping
        // joins the network thread, so no late boss-bar packet can be queued
        // after this boundary and repopulate the freshly-cleared component.
        self.net = None;
        // A reconnect is a new server session even when the caller did not
        // first visit the title screen. Clear server-authored HUD state before
        // the new connection can publish its first packets.
        self.reset_for_server_transfer();
        // The `World`-sharing half of §4.1(c) for a test double, which has no
        // `ClientBuilder` to hand the handle to. Production goes through
        // [`Self::connect`], where the real client adopts it at build time.
        #[cfg(test)]
        net.bind_session(Arc::clone(&self.ecs), self.local);
        // Stage 5: the `Send + Sync` half of the connection goes into the `World`
        // so the `TickSet::Send` systems can read the client. Not a second copy —
        // it is the same `Arc<OnceLock<_>>` the net thread publishes into, and
        // `NetClient` itself can never be a resource because its `mpsc::Receiver`
        // is `!Sync`. See `crate::interact::NetHandle`.
        let handle = net.shared_handle();
        self.write(|w| w.insert_resource(NetHandle(Some(handle))));
        self.net = Some(net);
        self.status = "connecting…".into();
        self.set_phase(SessionPhase::Connecting);
        // The store itself is adopted later, in `poll_net`: `NetClient::connect`
        // publishes its `ClientHandle` from the net thread, so there is nothing to
        // adopt until login. The *policy* changes immediately, though — a session
        // with no vanilla atlas cannot mesh the server's ids and must start
        // counting that rather than silently rendering nothing.
        self.refresh_mesh_policy();
    }

    /// Tear down whatever live session is attached and reset every piece of
    /// per-session state, so a later [`Sim::attach_net`] behaves exactly like
    /// the very first connection rather than starting with leftovers from
    /// the one that just ended.
    ///
    /// Driven by the pause menu's Quit to Title
    /// (`crate::menu::nav::MenuAction::QuitToTitle`); `UiState` has already
    /// left for the main menu by the time this runs, independent of this
    /// teardown's own success.
    ///
    /// # What this resets
    ///
    /// - **The connection**: `net` is dropped — `NetClient`'s `Drop` signals
    ///   its background thread to stop and joins it (see `net.rs`), so this
    ///   cannot leak a thread — and [`Self::phase`] returns to
    ///   [`SessionPhase::LocalOnly`]. Left at a stale
    ///   [`SessionPhase::Ended`] this would otherwise immediately re-fail the
    ///   *new* main-menu screen the moment
    ///   `crate::app::WindowApp::drive_ui_from_session` next runs.
    /// - **Every read-model [`Sim::poll_net`] feeds**: the chat log and the
    ///   teleport-count diagnostic directly, and everything else via
    ///   `insert_hud_components` — the status-effect overlay, title/subtitle,
    ///   action bar, health, food, experience, respawn count, the session phase,
    ///   and the server-assigned entity id (stale, not merely wrong: left in
    ///   place it would misattribute the *next* session's
    ///   `EffectApplied`/`EffectRemoved` to whichever entity the new server
    ///   happens to assign that same id to first).
    /// - **The shared-fold set — the tab list, scoreboard, boss bars, menus, and
    ///   (since the vitals collapse) health/food/saturation, experience, the
    ///   server entity id, game mode, dimension and liveness** — via
    ///   `insert_session_components`, the same one-call reset
    ///   `insert_hud_components` is for the driver half.
    ///
    ///   This bullet used to say those needed no clearing at all, "and that is
    ///   Stage 3 working rather than an omission: they are components in the
    ///   *client's* `World`, so dropping `net` above drops the only route to
    ///   them". **That went stale the moment §4.1(c) merged the two `World`s** —
    ///   it is one `World` and one entity now, `Sim::sidebar`/`tab_list_view`/
    ///   `boss_bars` read `self.local` directly, and dropping `net` drops no route
    ///   to anything. Left as written, the previous server's sidebar and tab list
    ///   really did survive a quit-to-title. A stale-but-true-when-written note
    ///   about state that "cannot" leak is exactly the shape `CLAUDE.md`'s rule 2
    ///   warns about.
    /// - **In-flight prediction state**: `mining` and `placement` are
    ///   replaced wholesale rather than merely stopped — both track a
    ///   monotonic sequence counter with no public reset, and `Mining` also
    ///   tracks a post-break cooldown `stop()` alone does not clear (see the
    ///   report). `attacking` clears, and the last-sent player-input/sprint
    ///   edge trackers reset to their [`Sim::new`] values so the next
    ///   session's first packet is not suppressed as a redundant resend.
    /// - **Meshing**: mesh jobs still in flight for the old server's chunks
    ///   are flushed and discarded (not left to land silently in whatever
    ///   session comes next), `dirty_columns`/`mesh_drops` clear, and every
    ///   section this session ever uploaded is queued into
    ///   `pending_removals` — the app's existing per-frame drain — per
    ///   [`Self::uploaded_sections`]'s doc.
    /// - **The player**: returned to the same spawn the constructor used
    ///   ([`PRE_SESSION_FEET`] for a real client, the demo surface for the
    ///   [`Sim::with_demo_world`] fixture), and free-fly clears. A live
    ///   reconnect immediately overrides this with the new server's login
    ///   teleport; leaving the old server's coordinates in place would
    ///   otherwise show the title screen's frozen player at wherever they
    ///   happened to quit.
    /// - **`status`**: recomputed with the same rule [`Sim::new`] uses, so
    ///   the debug overlay reads "local world"/"live world (vanilla atlas)"
    ///   again instead of whatever the old session last wrote there (e.g.
    ///   "connecting…" or a disconnect reason).
    ///
    /// # What this deliberately leaves alone
    ///
    /// GPU pipelines/buffers and loaded assets (`vanilla_atlas`, `language`,
    /// `version_data`) are config- or asset-derived, not session state —
    /// `Sim::new` never reloads them on `attach_net` either, so a teardown
    /// should not either. `particles` is intentionally untouched: every
    /// particle already expires within a couple of seconds on its own, and
    /// nothing drives its `tick`/`extract` once the title screen stops
    /// calling into the render path, so a leftover burst is inert rather
    /// than a bug. See the report on this change for what is genuinely
    /// unverified rather than merely reasoned about.
    pub fn end_session(&mut self) {
        // Drop first: `NetClient::drop` signals its net thread and joins it,
        // so nothing below can race a still-running poll against state this
        // method is about to reset out from under it.
        self.net = None;

        self.teleport_count = 0;
        // A reconnect that hits the same id-space mismatch must warn again: see
        // `Sim::warned_id_space_mismatch`'s own doc for why this is not left set.
        self.warned_id_space_mismatch = false;
        // A death screen (issue #103) must not survive into the next session —
        // `reset_local_player` below clears the `Dead` marker itself, but this
        // field is plain `Sim` state, not an ECS component, so it needs its own
        // line (see its doc comment on why it lives here rather than in
        // `lodestone_ecs::session`).
        self.death_message = None;
        // The credits screen (issue #192) must not survive into the next
        // session either, for the same reason `death_message` does not: a
        // quit-to-title and reconnect must start un-won.
        self.won = false;
        // The pause menu must offer Open to LAN again on the next hosted
        // session — see `Self::lan_published`'s own field doc.
        self.lan_published = false;
        // The dimension edge detector and the portal-transition effect, in one
        // call so this line and the field list in `sim/dimension.rs` cannot drift
        // — the same reason `reset_local_player` is one call rather than a
        // field-by-field reset. Leaving `applied_dimension` set would make the
        // *next* session's login look like a dimension change and drop the
        // terrain it had just streamed; leaving the portal intensity set would
        // paint the overlay over the title screen.
        self.reset_dimension_state();

        // §4.1(c): the entity interpolator no longer owns a `World` to throw away,
        // so its tracks are cleared explicitly. Replacing the whole interpolator
        // used to *also* zero that `World`'s private `TickAccum` while leaving the
        // player's accumulator alone — a quit-to-title re-phased the two clocks
        // arbitrarily on top of the clamp divergence. There is one accumulator now
        // and it is reset on the next line, deliberately rather than incidentally.
        self.write(|w| {
            crate::entities::reset_entity_tracks(w);
            // The ingest-side twin of the line above: `reset_entity_tracks` only
            // clears the *render* fold, and until this call nothing ever cleared
            // `lodestone_ecs::entity::EntityIndex`, so a rejoin's fresh server ids
            // left every previous session's entity indexed, still enumerated by
            // `SharedState::entities`, and redrawn frozen alongside its live
            // duplicate. See `reset_ingest_entities`'s own docs for the full trace.
            lodestone_ecs::ingest::reset_ingest_entities(w);
            w.resource_mut::<FrameClock>().reset_accumulator();
        });

        // Stage 5: all four are resources now, and `chat_log` moved out of this
        // list entirely — it is a `SessionChat` component that
        // `insert_hud_components` below puts back with the rest of the set, which
        // is what stops it being the field a later addition forgets.
        self.write(|w| {
            w.insert_resource(MiningPredictor(Mining::new()));
            w.insert_resource(PlacementPredictor(Placement::new()));
            w.insert_resource(Attacking(false));
            w.insert_resource(UsingItem(false));
            w.insert_resource(NetHandle(None));
        });

        // Flush and discard mesh jobs still in flight for the old server's
        // chunks rather than letting them complete later and land silently
        // in whatever session comes next; clear the dirty set and the drop
        // counter; and queue every section this session ever uploaded for removal
        // through the app's ordinary drain path.
        self.terrain_mut(TerrainMesh::end_session);

        // Release the server's chunk store. A client session adopted the client's
        // `World` at login (`adopt_live_world`); handing it back an empty store is
        // both the teardown *and* what makes a later `attach_net` adopt again —
        // adoption is gated on our store being empty. A `with_demo_world` fixture
        // never adopted, so its terrain is not the live store and survives, which
        // is the behaviour `resident_after_connect`'s control asserts.
        if std::mem::take(&mut self.adopted_live_world) {
            // Issue #423: the two halves are always replaced *together* with one
            // fresh store — a write handle left pointing at the released server
            // store while the read resource names a new empty one would be the
            // two-worlds defect this resource design exists to delete.
            let write_handle = ChunkWorldWrite::default();
            let chunk_world = write_handle.read_handle();
            self.write(|w| {
                w.insert_resource(write_handle);
                w.insert_resource(chunk_world);
            });
        }

        // Back to whatever spawn this `Sim` was built around — the demo world's
        // surface for the fixture, the pre-session placeholder for a real client
        // (which has no offline world to return to).
        let feet = if self.chunk_world().is_empty() {
            PRE_SESSION_FEET
        } else {
            worldgen::spawn_feet()
        };
        let mut player = PlayerState::at(Vec3d::new(feet[0], feet[1], feet[2]), 180.0);
        player.pitch = 10.0;
        // One call rather than a field-by-field reset: `reset_local_player` puts
        // the whole component set back to what `spawn_local_player` produces —
        // pose, camera anchor, submersion, intent, free-fly, hotbar slot, the two
        // wire edge-trackers (to their `Sim::new` values, so the next session's
        // first packet is not suppressed as a redundant resend), and the `Dead`
        // marker. Keeping that list in one place is what stops a component added
        // later from being silently missed here.
        let local = self.local;
        self.write(|w| reset_local_player(w, local, player));
        // The Stage-3 half of the same reset, in two calls because the set is in
        // two halves. `insert_hud_components` writes the driver half back to its
        // just-spawned value (phase, the two overlays, the effect stack, the
        // respawn counter, the chat log); `insert_session_components` does the
        // shared half (scoreboard, tab list, boss bars, menus, vitals, xp, and the
        // server entity id — which is *stale*, not merely wrong: left in place it
        // would misattribute the next session's mob effects to whichever entity
        // the new server happens to assign that id to first). Two calls rather
        // than a field-by-field reset, for the same reason `reset_local_player` is
        // one: a component added to a spawn path and missed here leaks the old
        // session into the new one.
        // Through `lodestone_app` rather than the two calls inline: this list and
        // the spawn path's list must never diverge, and routing both through one
        // function is what makes "add it to the spawn path" sufficient. See
        // `lodestone_app::insert_session_component_sets`.
        self.write(|w| lodestone_app::insert_session_component_sets(w, local));
        self.set_target(None);
        self.input_mut(InputState::release_all);

        self.status = if self.vanilla_atlas.is_some() {
            "live world (vanilla atlas)".to_string()
        } else if let Some(banner) = &self.asset_banner {
            format!("demo palette — {banner}")
        } else {
            "local world".to_string()
        };
    }

    /// The live connection, when one is attached. Lets a harness read the
    /// client-owned world (`loaded_chunks`, `sections_and_light_at`,
    /// `world_dimensions`) to check the shell's live mesh against ground truth.
    #[must_use]
    pub fn net(&self) -> Option<&NetClient> {
        self.net.as_ref()
    }

    /// Whether the terrain under the player is still streaming in — the
    /// post-login half of the loading screen (issue #449). `false` with no live
    /// session: the demo/dev world has no net client and is never "loading
    /// terrain".
    ///
    /// The condition is vanilla's `LevelLoadTracker.WaitingForPlayerChunk`
    /// readiness rule — see [`crate::menu::loading::is_level_ready`], which holds
    /// the decision and the record it was ported from. This function's whole job is
    /// gathering the four observations it reads.
    ///
    /// The column math is the same as `live_collision` (`sim/collide.rs`), the
    /// other reader of this exact question, so the two cannot disagree about which
    /// chunk the player is standing on.
    ///
    /// # The bound, and why it is not belt-and-braces
    ///
    /// This used to be the bare column test with no timeout, which makes the
    /// screen's dismissal a liveness assumption about the server. The owner's
    /// report was that assumption failing: the server centred the join view on
    /// chunk `(0, 0)` instead of on the joining player, so for a player restored
    /// away from the origin the column this waits for was never sent — and, because
    /// the server's own view tracker had recorded it as sent, never would be. The
    /// screen had no way out. That server defect is fixed
    /// (`lodestone_server::server`'s join centre), and the timeout is what makes a
    /// *future* one present as a 30 s delay rather than as a game that never starts.
    #[must_use]
    pub fn terrain_loading(&self) -> bool {
        let Some(net) = self.net() else {
            return false;
        };
        let position = self.player().position;
        let pcx = (position.x.floor() as i32).div_euclid(16);
        let pcz = (position.z.floor() as i32).div_euclid(16);

        // `None` — no dimensions yet — is `false`, i.e. "not inside a build
        // height", which routes to `is_level_ready`'s bail-out. That is the honest
        // reading rather than a defensive one: with no world dimensions there is no
        // column under the player to be waiting for.
        let within_build_height = net.world_dimensions().is_some_and(|dims| {
            let top = dims.min_y + dims.section_count() as i32 * 16;
            let y = position.y.floor() as i32;
            y >= dims.min_y && y < top
        });

        !crate::menu::loading::is_level_ready(crate::menu::loading::TerrainWait {
            own_column_loaded: net.is_chunk_loaded(lodestone_client::ChunkPos { x: pcx, z: pcz }),
            // `Duration::ZERO` when the phase boundary was never seen, which can
            // only under-report the wait — it never dismisses early.
            elapsed: self
                .terrain_wait_started
                .map_or(core::time::Duration::ZERO, |started| started.elapsed()),
            player_alive: !self.is_dead(),
            within_build_height,
        })
    }

    /// The loading screen's current step (issue #449).
    ///
    /// Distinct from [`Self::session_phase`], which is the coarse *state
    /// machine* the menu switches on: this is the human-readable step the
    /// screen names, and it has boundaries (`Joining`, terrain streaming) that
    /// [`SessionPhase`] collapses into one `Connecting`/`Connected` pair.
    #[must_use]
    pub fn connect_phase(&self) -> crate::menu::loading::ConnectPhase {
        self.connect_phase
    }

    /// Record the loading screen's step. Only [`crate::net::NetUpdate`] handling
    /// calls this — see [`crate::menu::loading::ConnectPhase`].
    ///
    /// Also starts the terrain wait's clock, on the transition *into*
    /// `LoadingTerrain` and not on a re-set of the same phase, so the timeout in
    /// [`Self::terrain_loading`] measures the phase and cannot be pushed forward by
    /// a repeated `NetUpdate::ConnectPhase`. This is still the only place a real
    /// boundary is recorded; the clock reads off that boundary rather than
    /// replacing it.
    pub(crate) fn set_connect_phase(&mut self, phase: crate::menu::loading::ConnectPhase) {
        if phase == crate::menu::loading::ConnectPhase::LoadingTerrain
            && self.connect_phase != crate::menu::loading::ConnectPhase::LoadingTerrain
        {
            self.terrain_wait_started = Some(crate::platform::Instant::now());
        }
        self.connect_phase = phase;
    }

    /// Declare how many columns this session's initial view contains, from the
    /// view radius the launcher asked the server for. Establishes the progress
    /// bar's denominator (issue #449).
    pub fn set_view_radius(&mut self, view_radius: u32) {
        self.expected_view_columns =
            Some(crate::menu::loading::TerrainProgress::expected_for_radius(view_radius));
        self.expected_view_radius = Some(view_radius);
    }

    /// How much of the initial view has landed, or `None` when there is no
    /// session or no declared view radius to divide by.
    ///
    /// The numerator is the client's own loaded-column count and the denominator
    /// is the view square — both real, which is the whole constraint issue #449
    /// puts on this feature. A missing denominator yields `None` so the screen
    /// draws a phase name with no bar, rather than a synthesised one.
    #[must_use]
    pub fn terrain_progress(&self) -> Option<crate::menu::loading::TerrainProgress> {
        let net = self.net()?;
        let expected = self.expected_view_columns?;
        Some(crate::menu::loading::TerrainProgress {
            loaded: net.loaded_chunks().len(),
            expected,
        })
    }

    /// The loading screen's chunk-status grid (issue #568): real per-column
    /// state for every column in the current view, or `None` under the same
    /// conditions [`Self::terrain_progress`] is — no session, or no declared
    /// view radius to size the grid from.
    ///
    /// Centred on the chunk under the player, the same column math
    /// [`Self::terrain_loading`] uses — the two must agree about which chunk
    /// is "the player's own", or the grid's centre cell and the dismissal
    /// predicate would be pointing at different columns.
    ///
    /// Each cell reads [`crate::net::NetClient::is_chunk_loaded`] directly:
    /// real, per-position, client-observed state, never synthesised from the
    /// scalar count [`Self::terrain_progress`] reports. See
    /// [`crate::menu::loading::ChunkCellStatus`]'s doc for why this has two
    /// states rather than vanilla's twelve.
    ///
    /// **Bounded by [`crate::menu::loading::MAX_GRID_RADIUS`], not by the view
    /// radius**: vanilla's own status view is a constant 17 regardless of
    /// render distance, and an unbounded grid overflows the top of the screen
    /// (see that constant's doc). The cells are still whole, real columns; at a
    /// large render distance this is the innermost square of the view rather
    /// than all of it.
    #[must_use]
    pub fn terrain_chunk_grid(&self) -> Option<crate::menu::loading::TerrainChunkGrid> {
        let net = self.net()?;
        let radius =
            crate::menu::loading::TerrainChunkGrid::view_radius(self.expected_view_radius?);
        let position = self.player().position;
        let pcx = (position.x.floor() as i32).div_euclid(16);
        let pcz = (position.z.floor() as i32).div_euclid(16);
        let diameter = crate::menu::loading::TerrainChunkGrid::diameter(radius) as i32;
        let r = i32::try_from(radius).unwrap_or(i32::MAX);
        let mut cells = Vec::with_capacity((diameter * diameter).max(0) as usize);
        for z in 0..diameter {
            for x in 0..diameter {
                let pos = lodestone_client::ChunkPos {
                    x: pcx - r + x,
                    z: pcz - r + z,
                };
                cells.push(if net.is_chunk_loaded(pos) {
                    crate::menu::loading::ChunkCellStatus::Full
                } else {
                    crate::menu::loading::ChunkCellStatus::Empty
                });
            }
        }
        Some(crate::menu::loading::TerrainChunkGrid { radius, cells })
    }

    /// The coarse session phase, for the menu state machine.
    ///
    /// Reads the [`Phase`] component; `Sim` holds no phase field.
    #[must_use]
    pub fn session_phase(&self) -> SessionPhase {
        self.read(|w| {
            w.get::<Phase>(self.local)
                .expect("the local player always carries Phase")
                .0
                .clone()
        })
    }

    /// Record a new session phase.
    pub(crate) fn set_phase(&mut self, phase: SessionPhase) {
        self.write_local(|w, local| {
            if let Some(mut current) = w.get_mut::<Phase>(local) {
                current.0 = phase;
            }
        });
    }

    /// Whether the local player is currently dead (awaiting the server-confirmed
    /// respawn). Movement is frozen while this holds.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.read(|w| w.get::<Dead>(self.local).is_some())
    }

    /// The current death's message, for the death screen (issue #103) to draw
    /// — `None` once the player is alive again, or before any death this
    /// session. See [`Self::death_message`]'s field doc.
    #[must_use]
    pub fn death_message(&self) -> Option<&str> {
        self.death_message.as_deref()
    }

    /// Whether `NetUpdate::WinGame` (issue #192) has arrived this session —
    /// the ground truth `app.rs`'s `drive_ui_from_session` reconciles into the
    /// credits screen, the same way [`Self::is_dead`] is reconciled into the
    /// death screen. See [`Self::won`]'s field doc.
    #[must_use]
    pub fn has_won(&self) -> bool {
        self.won
    }

    /// Takes the pending sign-edit request, if `NetUpdate::SignEditorOpened`
    /// arrived since the last call — the ground truth
    /// `app::session::drive_ui_from_session` polls once per frame to open
    /// [`crate::menu::Screen::SignEdit`]. See [`Self::pending_sign_edit`]'s
    /// own field doc on why this *takes* rather than reads a latched flag.
    pub fn take_pending_sign_edit(&mut self) -> Option<PendingSignEdit> {
        self.pending_sign_edit.take()
    }

    /// Takes the hand named by a pending server `OPEN_BOOK` request. This is a
    /// one-shot event, not a latched screen state: a later packet deliberately
    /// replaces an earlier unconsumed request, just like a sign-editor reopen.
    pub fn take_pending_book_open(&mut self) -> Option<bool> {
        self.pending_book_open.take()
    }

    /// Whether `NetUpdate::LanOpened` has arrived this session (issue #535's
    /// scope 2) — the ground truth `app::session::drive_ui_from_session`
    /// reconciles into `MenuNav::set_lan_published`, the same shape
    /// [`Self::has_won`] is reconciled into the credits screen. See
    /// [`Self::lan_published`]'s own field doc.
    #[must_use]
    pub fn is_lan_published(&self) -> bool {
        self.lan_published
    }

    /// Submit a manual respawn request (`ClientAction::Respawn`) — the death
    /// screen's Respawn button. A no-op unless the player is actually flagged
    /// dead, so a stray call (a double-click, a leftover queued action after
    /// the server already respawned us) cannot send an unsolicited respawn
    /// mid-game, and a no-op off a live session (nothing to send to).
    ///
    /// Manual because [`crate::net::run`] now builds the client with
    /// [`lodestone_client::RespawnPolicy::Manual`] (issue #103): the library
    /// used to answer every `Death` event with an automatic
    /// `ClientAction::Respawn`, which is what let the shell ride through death
    /// with no screen at all. See `docs/pause-menu.md`'s note on the death
    /// screen for the full picture.
    pub fn respawn(&mut self) {
        if self.is_dead()
            && let Some(net) = &self.net
        {
            net.send_action(ClientAction::Respawn);
        }
    }

    /// Number of respawns observed since the session started — a diagnostic the
    /// live death gate reads to confirm the client recovered from a death.
    #[must_use]
    pub fn respawn_count(&self) -> u64 {
        self.read(|w| {
            w.get::<RespawnCount>(self.local)
                .expect("the local player always carries RespawnCount")
                .0
        })
    }

    /// The most recent chat/system lines (oldest-first) for the HUD to draw,
    /// each paired with its **age in seconds** (now − arrival) so the HUD can
    /// apply the vanilla fade-out. Lines carry legacy `§` colour codes.
    #[must_use]
    pub fn recent_chat(&self, n: usize) -> Vec<(String, f32)> {
        let now = self.clock().secs;
        self.read(|w| {
            w.get::<SessionChat>(self.local)
                .expect("the local player always carries SessionChat")
                .0
                .recent_ages(n, now)
        })
    }

    /// The span-carrying sibling of `recent_chat`: same recent-lines-with-age
    /// projection, `recent_ages_spans` in place of `recent_ages`, so a hex
    /// colour survives past this accessor.
    #[must_use]
    pub fn recent_chat_spans(&self, n: usize) -> Vec<(Vec<lodestone_model::TextSpan>, f32)> {
        let now = self.clock().secs;
        self.read(|w| {
            w.get::<SessionChat>(self.local)
                .expect("the local player always carries SessionChat")
                .0
                .recent_ages_spans(n, now)
        })
    }

    /// The most recent `n` chat lines' trust level, **element-for-element
    /// aligned** with [`Self::recent_chat_spans`] — same `n`, same feed, same
    /// window — so a caller can zip the two without re-deriving the slice.
    ///
    /// `None` for a system line, which carries no signature and so has no
    /// verdict to badge; see `ChatLog::recent_trust`.
    #[must_use]
    pub fn recent_chat_trust(&self, n: usize) -> Vec<Option<lodestone_game::chat::MessageTrust>> {
        self.read(|w| {
            w.get::<SessionChat>(self.local)
                .expect("the local player always carries SessionChat")
                .0
                .recent_trust(n)
        })
    }

    /// The `click`/`hover`-carrying sibling of [`Self::recent_chat_spans`]:
    /// same recent-lines-with-age projection through
    /// `recent_ages_interactive`, so a chat hit-test
    /// ([`crate::hud::chat_interaction_at`]) has something to test against.
    /// `recent_chat_spans` cannot supply this — `TextSpan` has no field for
    /// either, which is why the tab-list-shaped question "the field exists,
    /// so is the feature done?" has to be asked of *this* accessor and not
    /// that one.
    #[must_use]
    pub fn recent_chat_interactive(
        &self,
        n: usize,
    ) -> Vec<(Vec<lodestone_game::text::InteractiveSpan>, f32)> {
        let now = self.clock().secs;
        let translate = self.translator();
        self.read(|w| {
            w.get::<SessionChat>(self.local)
                .expect("the local player always carries SessionChat")
                .0
                .recent_ages_interactive(n, now, translate.as_ref())
        })
    }

    /// Push a client-authored system line into the chat feed.
    ///
    /// The one writer that is not the wire. Vanilla has the same seam —
    /// `ChatComponent.addMessage` is called by local commands and by
    /// `MultiplayerOptionsScreen`'s publish result — and this exists for exactly
    /// that second caller (issue #535): the LAN port has to be readable while the
    /// host reads it out, which a toast is not.
    ///
    /// **Not for anything the server could say instead.** A client-authored line
    /// that looks like a server message is how a fabricated state reads as real;
    /// keep these to statements about the client's own doing.
    pub fn push_local_chat(&mut self, line: impl Into<String>) {
        let text = lodestone_model::text::Text::literal(line.into());
        let now = self.clock().secs;
        let local = self.local;
        self.write(|w| {
            if let Some(mut chat) = w.get_mut::<SessionChat>(local) {
                chat.0.push_system(text, now);
            } else {
                // Every other accessor of this component on this same
                // `self.local` entity (`Self::vitals`/`Self::hunger`/…, and
                // this file's own reads a few lines up) treats it as always
                // present and `.expect()`s it — `LocalPlayerPlugin` inserts
                // the whole session component set eagerly and nothing ever
                // removes it, so reaching here means the local player was
                // somehow despawned, a bug in the caller rather than a state
                // to route around. The owner's standing rule is that nothing
                // may be silently skipped: this call site is the one place
                // in the F3 debug-chord path (`app::lifecycle::apply_key_outcome`)
                // that could make a chord's toggle succeed while its own
                // "[Debug] X: shown/hidden" feedback silently never appears,
                // so a dropped line here must be loud, not a no-op `if let`.
                tracing::warn!(
                    target: "chat",
                    "push_local_chat dropped a line — SessionChat missing on the local entity: {}",
                    text.to_plain_string()
                );
            }
        });
    }

    /// Server-reported health in `0..=20`, or `None` off a live survival server.
    #[must_use]
    pub fn health(&self) -> Option<f32> {
        self.vitals().health
    }

    /// Server-reported food level in `0..=20`, or `None` off a live server.
    #[must_use]
    pub fn food(&self) -> Option<i32> {
        self.vitals().food
    }

    /// Server-reported food saturation — the hidden reserve that drains before
    /// `food` does — or `None` before the first `set_health`.
    ///
    /// Read by the HUD's hunger wobble (issue #30): vanilla shakes the hunger
    /// row only while saturation is exhausted (`Hud.java`), so without
    /// this the animation is computed correctly and never fires on a live
    /// server. `Vitals::saturation` was already populated; only the accessor
    /// and `app.rs`'s one assignment were missing.
    #[must_use]
    pub fn saturation(&self) -> Option<f32> {
        self.vitals().saturation
    }

    /// Server-reported air supply in ticks (`0..=300`), or `None` before the
    /// first entity-metadata update naming the local player arrives (see
    /// [`Vitals::air`]'s doc for why this rides a different event family than
    /// `health`/`food`).
    #[must_use]
    pub fn air(&self) -> Option<i32> {
        self.vitals().air
    }

    /// The [`Vitals`] component.
    ///
    /// # Read-only from this side
    ///
    /// There is no `set_vitals`, and there must not be one again. `Vitals`, [`Xp`]
    /// and [`ServerEntityId`] are folded by
    /// `lodestone_ecs::session::apply_local_player_state` on the **net thread**,
    /// into this same `World` and onto this same entity (§4.1(c) made
    /// `SharedState`'s session entity and `Sim.local` one entity). The shell used
    /// to fold `NetUpdate::{Health, Experience, LoggedIn}` into them itself, which
    /// after the `World` unification meant two writers of one component; those
    /// arms and the two `NetUpdate` variants are deleted.
    fn vitals(&self) -> Vitals {
        self.read(|w| {
            *w.get::<Vitals>(self.local)
                .expect("the local player always carries Vitals")
        })
    }

    /// The server-assigned entity id for the local player, `None` before login.
    ///
    /// Read by every entity-scoped update that has to decide "is this us" — mob
    /// effects, most obviously, whose packet applies to any entity. Written only
    /// by the net thread's fold; see [`Self::vitals`].
    #[must_use]
    pub(crate) fn server_entity_id(&self) -> Option<i32> {
        self.read(|w| {
            w.get::<ServerEntityId>(self.local)
                .expect("the local player always carries ServerEntityId")
                .0
        })
    }

    /// Server-reported experience as `(progress, level, total)`, or `None`
    /// before `set_experience` has arrived (e.g. the local dev world, or a
    /// live server before the first packet). `progress` is `0.0..1.0` toward
    /// the next level.
    #[must_use]
    pub fn experience(&self) -> Option<(f32, i32, i32)> {
        self.read(|w| {
            w.get::<Xp>(self.local)
                .expect("the local player always carries Xp")
                .0
        })
    }

    /// The tab overlay's whole frame — rows in vanilla display order, capped at
    /// `PlayerTabOverlay`'s 80, plus the server's header and footer. Empty until
    /// the server sends player-list data.
    ///
    /// One method rather than the `player_rows` + `tab_banner` pair it replaces:
    /// those returned pre-flattened `"NAME  30ms"` strings and a `(header,
    /// footer)` tuple, which cost the draw the game mode, the styled display name
    /// and the latency band — a fully-connected wire carrying a lossy value, so
    /// `cargo xtask connectedness` was green throughout. The whole projection now
    /// lives in [`crate::tablist::tab_list_view`], and this is its one reader
    /// into the world.
    ///
    /// # Read straight off the component since §4.1(c)
    ///
    /// This and the accessors below used to go out through `NetClient` into
    /// the *client's* `World`, because the net thread's fold lived there and a
    /// component in one `World` is unreachable from another. There is one `World`
    /// now and [`Self::local`] is the entity the fold writes, so the round trip is
    /// gone. Still exactly one fold — `lodestone_ecs::session`'s `NetIngest`
    /// systems — and still one copy of it; what changed is only who reads it.
    ///
    /// `SessionTabList.0.header`/`.footer` reach pixels through here. They were
    /// folded and unit-tested with **zero readers anywhere in the shell** for a
    /// while, and not *entirely* unread — `lodestone_game`'s own `HudSnapshot`
    /// reads them, which is why the fold's comment claiming they were "read
    /// downstream by `hud.rs`'s snapshot" survived review. But the shell builds
    /// its own `HudFrame` and never constructs a `HudSnapshot`.
    #[must_use]
    pub fn tab_list_view(&self) -> crate::tablist::TabListView {
        let list = self.tab_list();
        // The same `SessionScoreboard` read `Sim::sidebar` already does —
        // `PlayerTabOverlay.getNameForDisplay` runs a name with no explicit
        // display name through the player's team, so a tab list built without
        // this reads every team-coloured player in plain white.
        let board = self.read(|w| {
            w.get::<lodestone_ecs::SessionScoreboard>(self.local)
                .map(|board| board.0.clone())
                .unwrap_or_default()
        });
        crate::tablist::tab_list_view(&list, Some(&board), self.translator().as_ref())
    }

    /// The same folded tab list [`Self::tab_list_view`] projects, unprojected —
    /// issue #189's Social Interactions roster needs the raw entries
    /// (`crate::menu::social::entries_from_tablist`), not pre-rendered strings.
    #[must_use]
    pub fn tab_list(&self) -> lodestone_game::tablist::TabList {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionTabList>(self.local)
                .map(|list| list.0.clone())
                .unwrap_or_default()
        })
    }

    /// The connecting session's local player UUID, or `None` off a live
    /// session or before [`NetClient::local_uuid`] has published one. See
    /// that method's doc for why this identity has to travel through
    /// `NetClient` at all rather than living on a component.
    #[must_use]
    pub fn local_uuid(&self) -> Option<uuid::Uuid> {
        self.net.as_ref()?.local_uuid()
    }

    /// The locator bar's dots (issue #26) for the given camera pose —
    /// [`lodestone_ecs::session::SessionWaypoints`] is fully decoded and
    /// folded and, until this accessor, read by nothing at all. Empty off
    /// a live server or once the last tracked waypoint is untracked.
    ///
    /// `camera_pos`/`camera_yaw` are the caller's own render camera (see
    /// [`Self::camera`]) rather than something this method resolves itself
    /// — session scalars in this file read components, not the camera rig,
    /// which lives in a different seam of this same struct
    /// (`sim/camera.rs`).
    #[must_use]
    pub fn locator_dots(
        &self,
        camera_pos: glam::Vec3,
        camera_yaw: f32,
    ) -> Vec<crate::hud::locator::LocatorDot> {
        // The local player's own waypoint, when the server tracks one for
        // them — `LocatorBar.java`'s own exclusion. `local_uuid()` is the
        // connecting session's identity, not a per-entity component, which
        // is why this reads it through `NetClient` rather than the ECS
        // world the rest of this method touches.
        let local_id = self
            .local_uuid()
            .map(lodestone_model::event::WaypointId::Entity);
        self.read(|w| {
            w.get::<lodestone_ecs::SessionWaypoints>(self.local)
                .map_or_else(Vec::new, |store| {
                    crate::hud::locator::locator_dots(
                        store.0.iter(),
                        camera_pos,
                        camera_yaw,
                        local_id.as_ref(),
                    )
                })
        })
    }

    /// The scoreboard sidebar to draw, or `None` when none is displayed (or off
    /// a live server). Folded through [`lodestone_game::scoreboard::Scoreboard`].
    #[must_use]
    pub fn sidebar(&self) -> Option<Sidebar> {
        let board = self.read(|w| {
            w.get::<lodestone_ecs::SessionScoreboard>(self.local)
                .map(|board| board.0.clone())
                .unwrap_or_default()
        });
        crate::scoreboard::sidebar_from(&board, self.translator().as_ref())
    }

    /// The client's own folded team/objective state — the same
    /// `SessionScoreboard` read [`Self::sidebar`] does, exposed raw for a
    /// caller (the Spectator Menu's "Team Teleport" grouping, issue #613's
    /// `TeleportToEntity` remainder) that needs [`Scoreboard::team_of`]
    /// rather than a rendered sidebar.
    #[must_use]
    pub fn scoreboard(&self) -> lodestone_game::scoreboard::Scoreboard {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionScoreboard>(self.local)
                .map(|board| board.0.clone())
                .unwrap_or_default()
        })
    }

    /// Whether the local player's server-authoritative game mode is
    /// `Spectator` — `MultiPlayerGameMode.isSpectator()`. A public mirror of
    /// `sim::actions::Sim::is_spectator` (private to that module): the
    /// Spectator Menu's hotbar-key intercept (issue #613's
    /// `TeleportToEntity` remainder) is gated from `app/input.rs`, which has
    /// no access to that module-private helper.
    #[must_use]
    pub fn is_spectator(&self) -> bool {
        self.read(|w| w.get::<ServerGameMode>(self.local).and_then(|m| m.0))
            == Some(lodestone_client::GameMode::Spectator)
    }

    /// The active boss bars to draw, in render order. Empty off a live server.
    #[must_use]
    pub fn boss_bars(&self) -> Vec<BossBarView> {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionBossBars>(self.local)
                .map_or_else(Vec::new, |bars| {
                    crate::overlay::boss_bars_from(&bars.0, self.translator().as_ref())
                })
        })
    }

    /// The XP bar to draw as `(level, progress 0..=1)`, `Some` only once the
    /// server has sent an experience update. Reads the already-folded
    /// [`Sim::experience`]; off a live server it stays `None` and no bar draws.
    #[must_use]
    pub fn xp(&self) -> Option<(i32, f32)> {
        self.experience()
            .map(|(progress, level, _total)| (level, progress))
    }

    /// The ticks a fresh attack must wait before it is back at full strength —
    /// vanilla's `getCurrentItemAttackStrengthDelay`, `(1.0 /
    /// getAttributeValue(Attributes.ATTACK_SPEED)) * 20.0`
    /// (`Player.java`, `.cache/mc/26.2/src`).
    ///
    /// Reads `minecraft:attack_speed` off the local player's own
    /// [`Attributes`] snapshot — the same server-fed, per-item-aware value
    /// `lodestone_ecs::player::player_physics`'s `WATER_MOVEMENT_EFFICIENCY`
    /// injection already reads through `attribute_value`. This is *not* a hardcoded
    /// constant and does not need `lodestone-data`'s `item_prototypes` census
    /// (which was checked and does not carry attack speed at all — no
    /// `minecraft:attribute_modifiers` census exists in this repo yet): a
    /// weapon's `-2.4` (sword) / `-3.0` (axe) modifier arrives the same way
    /// any other equipment-driven attribute change does, as a server
    /// `update_attributes` packet the instant the held item changes
    /// (`AttributeMap`'s dirty-tracking on `LivingEntity.setItemSlot`), and
    /// [`Attributes`] already folds it. Before the first such packet (a fresh
    /// demo-world player, or a live session before login's fold lands)
    /// `attribute_value` reads the registry default (`4.0`, unarmed), giving a
    /// 5-tick delay — the correct unarmed value, not a guess.
    #[must_use]
    fn attack_strength_delay(&self) -> f32 {
        let key = lodestone_model::Identifier::new("minecraft", "attack_speed")
            .expect("valid built-in identifier");
        let speed = self.read(|w| {
            w.get::<Attributes>(self.local)
                .map_or(4.0, |attrs| attribute_value(&attrs.0, &key))
        });
        // `getAttributeValue` cannot legitimately reach 0 (the registry clamps
        // `attack_speed` to `>= 0.0`, and no vanilla modifier stack takes an
        // unarmed 4.0 base all the way there), but a hostile/future value of
        // exactly 0 must not become a divide-by-zero `inf` delay.
        20.0 / (speed.max(f64::from(f32::EPSILON)) as f32)
    }

    /// Armour points to draw, `None` before the local player has a server-fed
    /// attribute snapshot.
    ///
    /// Vanilla's `LivingEntity.getArmorValue` is
    /// `Mth.floor(getAttributeValue(Attributes.ARMOR))`, so this is the folded
    /// `minecraft:armor` attribute and **not** a per-item table — equipment
    /// contributes through the modifiers the server pushes on
    /// `LivingEntity.setItemSlot`, exactly as [`Self::attack_strength_delay`]
    /// above documents for `attack_speed`. `Some(0)` is a real state (a live
    /// player wearing nothing) and draws no row, matching vanilla's
    /// `if (armor > 0)`; `None` is "no snapshot yet" and also draws nothing, so
    /// the two agree on screen and differ only in what they claim to know.
    ///
    /// Public where [`Self::attack_strength_delay`] is private because this one
    /// has an out-of-file caller — the HUD frame assembler — which is the same
    /// reason [`Self::health`] and [`Self::food`] are public over a private
    /// `vitals`.
    #[must_use]
    pub fn armour_value(&self) -> Option<i32> {
        let key = lodestone_model::Identifier::new("minecraft", "armor")
            .expect("valid built-in identifier");
        self.read(|w| {
            w.get::<Attributes>(self.local)
                .map(|attrs| attribute_value(&attrs.0, &key).floor() as i32)
        })
    }

    /// The attack-cooldown fraction the crosshair indicator fills to,
    /// `0.0..=1.0` — vanilla's `getAttackStrengthScale(0.0F)`
    /// (`Player.java`), the exact call `Hud.extractCrosshair` makes
    /// for the crosshair-style indicator (`Hud.java`). The `a` (partial
    /// tick) argument is fixed at `0.0` here, same as that call site; nothing
    /// in this shell threads a render-time partial tick into `Sim`'s other
    /// accessors either (see [`Self::health`]/[`Self::xp`]).
    #[must_use]
    pub fn attack_strength_scale(&self) -> f32 {
        self.attack_strength_scale_at(0.0)
    }

    /// `getAttackStrengthScale(a)` (`Player.java`) with the partial
    /// tick argument exposed, because vanilla itself calls this with two
    /// different values for two different purposes: `0.0F` for the crosshair
    /// indicator ([`Self::attack_strength_scale`], `Hud.java`) and `0.5F`
    /// for `Player.attack`'s own `fullStrengthAttack` gate
    /// (`Player.java`), which [`Self::maybe_spawn_crit_particles`]
    /// needs. One private helper rather than two public accessors that would
    /// otherwise duplicate the ticker read and delay computation.
    #[must_use]
    pub(crate) fn attack_strength_scale_at(&self, a: f32) -> f32 {
        let delay = self.attack_strength_delay();
        let ticker = self.read(|w| w.get::<AttackStrengthTicker>(self.local).map_or(0, |t| t.0));
        ((ticker as f32 + a) / delay).clamp(0.0, 1.0)
    }

    /// The title/subtitle overlay as `(title, subtitle, alpha)`, `Some` while a
    /// server-sent title is visible. `Text` is flattened to **styled spans** at
    /// read time, so every colour a server sent survives to the HUD.
    ///
    /// This used `to_legacy_string()`, and that one call was where a hex title
    /// colour died. The sixteen named colours have `§` codes, so they survived a
    /// `String` — the font layer applies codes at draw time — but
    /// [`lodestone_model::text::TextColor::Rgb`] has none, so `to_legacy_string`
    /// dropped it silently, one layer above a HUD that could not have accepted it
    /// anyway. `to_spans` also applies `TextStyle::inherit` down the tree, so a
    /// nested run with no colour of its own arrives carrying its parent's.
    #[must_use]
    pub fn title_overlay(&self) -> Option<(Vec<TextSpan>, Option<Vec<TextSpan>>, f32)> {
        let state = self.read(|w| {
            w.get::<TitleOverlay>(self.local)
                .expect("the local player always carries TitleOverlay")
                .0
                .clone()
        });
        let title = state.title()?;
        Some((
            self.resolve_text(title).to_spans(),
            state.subtitle().map(|s| self.resolve_text(s).to_spans()),
            state.alpha(),
        ))
    }

    /// The action-bar message as `(text, alpha)`, `Some` while a GameInfo
    /// message is visible (fades over its final ticks). Styled spans rather than a
    /// legacy `§` string, for the reason [`Self::title_overlay`] gives.
    #[must_use]
    pub fn action_bar_overlay(&self) -> Option<(Vec<TextSpan>, f32)> {
        let state = self.read(|w| {
            w.get::<ActionBarOverlay>(self.local)
                .expect("the local player always carries ActionBarOverlay")
                .0
                .clone()
        });
        let text = state.text()?;
        Some((self.resolve_text(text).to_spans(), state.alpha()))
    }

    /// The held-item name highlight (issue #126) as `(styled name, alpha)`,
    /// `Some` while a selected item's name is showing. Ticked in
    /// [`lodestone_ecs::session::tick_hud_overlays`], keyed on the selected
    /// stack's *identity* rather than slot — see
    /// [`lodestone_ecs::session::HeldItemOverlay`]'s doc.
    #[must_use]
    pub fn held_item_overlay(&self) -> Option<(String, f32)> {
        self.read(|w| {
            let overlay = w
                .get::<lodestone_ecs::session::HeldItemOverlay>(self.local)
                .expect("the local player always carries HeldItemOverlay");
            let name = overlay.0.name()?;
            Some((name.to_owned(), overlay.0.alpha()))
        })
    }

    /// The [`Self::held_item_overlay`] sibling that keeps a hex colour:
    /// [`lodestone_game::player_state::HeldItemHighlight::name_spans`] instead
    /// of `name`, for the reason [`Self::action_bar_overlay`] gives.
    #[must_use]
    pub fn held_item_overlay_spans(&self) -> Option<(Vec<TextSpan>, f32)> {
        self.read(|w| {
            let overlay = w
                .get::<lodestone_ecs::session::HeldItemOverlay>(self.local)
                .expect("the local player always carries HeldItemOverlay");
            let spans = overlay.0.name_spans()?;
            Some((spans.to_vec(), overlay.0.alpha()))
        })
    }

    /// `Player.hasInfiniteMaterials()` — `Abilities.instabuild`
    /// (`Player.java`; `AnvilMenu.mayPickup` and
    /// `EnchantmentScreen.java` both gate on it). Used by
    /// `app.rs`'s `ContainerFrame::with_cost_context` for the anvil/enchanting
    /// affordability colours — see `docs/container-cost-screens.md`.
    #[must_use]
    pub fn has_infinite_materials(&self) -> bool {
        self.read(|w| {
            w.get::<lodestone_ecs::session::Abilities>(self.local)
                .is_some_and(|a| a.instabuild)
        })
    }

    /// The local player's active status effects, for the top-right HUD overlay.
    /// Empty until a server applies one; ticked down in [`Sim::step`].
    #[must_use]
    pub fn active_effects(&self) -> lodestone_game::effect::ActiveEffects {
        self.read(|w| {
            w.get::<HudEffects>(self.local)
                .expect("the local player always carries HudEffects")
                .0
                .clone()
        })
    }

    /// The folded player inventory menu. Off a live connection this returns an
    /// empty player menu so the local inventory screen can still render.
    ///
    /// Reads the [`lodestone_ecs::SessionMenus`] component — see
    /// [`Self::tab_list_view`] on why that is a direct read since §4.1(c). Note the
    /// *write* side is still `ClientHandle::menu_click`, which predicts against
    /// this same component under its own short guard: prediction has to mutate the
    /// one copy, and a clone has nowhere for the mutation to land.
    #[must_use]
    pub fn player_menu(&self) -> Menu {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionMenus>(self.local)
                .map_or_else(Menu::player, |menus| menus.0.player().clone())
        })
    }

    /// The currently open server menu, if any.
    #[must_use]
    pub fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        self.read(|w| {
            let menus = &w.get::<lodestone_ecs::SessionMenus>(self.local)?.0;
            Some(OpenMenuSnapshot {
                window_id: menus.opened_window_id()?,
                menu_type: menus.opened_menu_type()?.clone(),
                title: menus.opened_title()?.clone(),
                menu: menus.opened()?.clone(),
                data: menus.opened_data().to_vec(),
            })
        })
    }

    /// The book and selected page of the currently open lectern, if the
    /// server supplied a signed book in its sole menu slot. `LecternMenu`
    /// exposes that book without the ordinary 36 appended inventory slots;
    /// `lodestone_game::menus::Menus` preserves the slot before this reader
    /// projects it into the normal book-view state.
    #[must_use]
    pub fn lectern_book_view(
        &self,
    ) -> Option<(i32, crate::menu::book_view::BookViewOpen, i32)> {
        let open = self.open_menu()?;
        if open.menu_type.namespace() != "minecraft" || open.menu_type.path() != "lectern" {
            return None;
        }
        let stack = open.menu.slot_item(0)?;
        if stack.item().to_string() != "minecraft:written_book" {
            return None;
        }
        let content = stack.written_book_content()?;
        let page = open
            .data
            .iter()
            .find(|(property, _)| *property == 0)
            .map_or(0, |(_, value)| *value);
        Some((
            open.window_id,
            crate::menu::book_view::BookViewOpen::from_pages(
                content.title.clone(),
                content.author.clone(),
                content.generation,
                &content.pages,
            ),
            page,
        ))
    }

    /// The open merchant's trade list, if any server has sent one this
    /// session — [`lodestone_ecs::SessionTrades`], the same
    /// `MerchantOffersReceived -> TradeOffers` fold every other session
    /// scalar in this file reads (issue #245's UI half). Empty (never `None`
    /// — see [`lodestone_game::trades::TradeOffers::new`]) off a live
    /// connection or before any merchant screen has opened, which is what
    /// lets `app.rs` build a [`crate::container::ContainerFrame`] with
    /// [`crate::container::ContainerFrame::with_trades`] unconditionally
    /// rather than guarding on a connection first.
    #[must_use]
    pub fn trades(&self) -> lodestone_game::trades::TradeOffers {
        self.read(|w| {
            w.get::<lodestone_ecs::SessionTrades>(self.local)
                .map_or_else(lodestone_game::trades::TradeOffers::new, |trades| trades.0.clone())
        })
    }

    /// Close the open server menu: clear it locally **and** tell the server.
    ///
    /// # Both halves are required, and the local one is why this takes `&mut self`
    ///
    /// This used to only send `ContainerClose`, and the screen therefore never went
    /// away — you could open a crafting table and not get out of it. A vanilla
    /// server does **not** echo a close back; `ClientboundContainerClosePacket` is
    /// sent only when the *server* forces a close. So waiting for the wire to clear
    /// [`Self::open_menu`] waits forever, and every consumer that keys off it —
    /// `active_container_menu`, the key-dispatch gate, the container draw — stayed
    /// convinced a menu was open.
    ///
    /// Vanilla's `Player.closeContainer()` clears the client's own menu immediately
    /// and *then* notifies the server, which is what this now mirrors. The local
    /// clear reuses [`ClientEvent::ScreenClosed`] rather than poking the component,
    /// so the close travels the same fold as a server-driven one and cannot drift
    /// from it (`lodestone_game::menus::Menus::apply`).
    ///
    /// It needs `&mut self` for that write. The old `&self` signature was not a
    /// style choice — it made the local clear *unrepresentable*, which is why the
    /// bug survived a fix to the key dispatch that reached this function correctly.
    pub fn close_open_menu(&mut self) {
        let Some(open) = self.open_menu() else { return };
        // Issue #145: a plugin-opened local menu has no server-side container, so
        // a `ContainerClose` naming its window id would be addressed to something
        // the server has never heard of. Close it locally and send nothing.
        //
        // This branch is the reason `Menus::opened_is_local` exists rather than
        // callers comparing against `LOCAL_MENU_WINDOW_ID`: the wire send is
        // *unconditional* here otherwise, and that unconditional send is what made
        // the synthetic-event route to a local menu a correctness bug rather than
        // a cosmetic one.
        if self.close_local_menu() {
            return;
        }
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ContainerClose {
                window_id: open.window_id,
            });
        }
        let window_id = open.window_id;
        self.write_local(|w, local| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                menus
                    .0
                    .apply(&lodestone_model::ClientEvent::ScreenClosed { window_id });
            }
        });
    }

    // -----------------------------------------------------------------------
    // Plugin-opened local menus (issue #145)
    // -----------------------------------------------------------------------

    /// Open a menu a plugin built, with no server container behind it —
    /// `Bukkit.createInventory` + `Player.openInventory`.
    ///
    /// The plugin supplies the whole [`Menu`], so any of its constructors work,
    /// including the `SpecialLayout` ones. The screen draws through exactly the
    /// path a server-opened container draws through (`Sim::open_menu` →
    /// `ContainerFrame`), because it *is* the same `OpenMenu` slot — the only
    /// difference is that nothing about it reaches the wire.
    ///
    /// Off a live session this still works: a local menu needs no connection,
    /// which is the whole point (a client-side settings or waypoint screen must
    /// open at the title screen too).
    pub fn open_local_menu(
        &mut self,
        menu: Menu,
        menu_type: lodestone_model::ResourceKey,
        title: lodestone_model::Text,
    ) {
        self.write_local(|w, local| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                menus.0.open_local(menu, menu_type, title);
            }
        });
    }

    /// Close the open menu **only if it is a plugin-opened local one**, returning
    /// whether it closed.
    ///
    /// Narrower than [`Self::close_open_menu`] on purpose, and the narrowness is
    /// the safety property: this can never close a real server container, so it
    /// cannot desynchronise the server's own open container with no packet
    /// explaining why.
    pub fn close_local_menu(&mut self) -> bool {
        self.write_local(|w, local| {
            w.get_mut::<lodestone_ecs::SessionMenus>(local)
                .is_some_and(|mut menus| menus.0.close_local())
        })
    }

    /// Predict a click against a plugin-opened local menu, sending nothing.
    ///
    /// The local counterpart to `ClientHandle::menu_click`. That one predicts *and*
    /// returns a `ClientAction` the caller puts on the wire; for a local menu there
    /// is no wire, so the prediction is the whole operation and it is authoritative
    /// rather than provisional — no `container_set_slot` is ever coming to correct
    /// it.
    ///
    /// No-op (returns `false`) unless a local menu is open, so a caller that has
    /// not checked cannot accidentally mutate a server container through this.
    pub fn click_local_menu(&mut self, click: lodestone_game::click::Click) -> bool {
        self.write_local(|w, local| {
            let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) else {
                return false;
            };
            if !menus.0.opened_is_local() {
                return false;
            }
            let _ = menus
                .0
                .click(click, lodestone_game::click::PlayerCtx::survival());
            true
        })
    }

    /// Compose a typed chat line onto the outbound [`ClientAction`] seam and hand
    /// it to the live client (a leading `/` is a command, else a chat message).
    /// A blank line sends nothing. No-op without a live connection. Returns
    /// whether anything was sent, so the caller can echo command feedback.
    ///
    /// **Nothing server-bound is intercepted here.** A `/givedebug` wrapper
    /// used to run ahead of [`compose_chat_action`] and rewrite itself into
    /// the server's real `/give @s <item> <amount>`; issue #382 deleted it,
    /// because typing `/give` does the same thing with no bespoke parser to
    /// keep in step with the server's. Every `/` line still goes to the
    /// server verbatim, and every command response — including "you are not
    /// op" — arrives back over the ordinary inbound chat path.
    ///
    /// A leading `#`, unlike `/`, is a **client-local** command namespace and
    /// is intercepted before `compose_chat_action` ever sees it —
    /// deliberately the opposite policy from `/`. So **any** `#`-prefixed line
    /// is consumed here and refused, rather than falling through to
    /// `compose_chat_action` and leaking as literal chat text for every other
    /// player to read, which would be worse than silently dropping it.
    ///
    /// # Why the namespace is reserved but empty
    ///
    /// It used to hold exactly one command, `#goto x z` (issue #38, M1), which
    /// set `lodestone_autopilot::AutopilotGoal`. **Both the command and the
    /// dependency were removed on purpose**: the autopilot is a
    /// pre-implemented *external* plugin and the shipped client does not
    /// navigate itself (see `sim/build.rs`'s note where the plugin was
    /// registered, and `docs/autonomous-navigation.md`'s "Not wired into the
    /// shell"). A chat command in the shell that reaches into a plugin's
    /// resource is backwards for a plugin architecture — the plugin should
    /// register its own commands, which is
    /// [#118](https://github.com/matteopolak/lodestone/issues/118).
    ///
    /// **The reservation itself is kept, and is not autopilot-specific.**
    /// Deleting it would not restore any capability; it would only start
    /// leaking `#`-prefixed lines onto the wire as ordinary chat. So this arm
    /// stays as the shell's own guarantee about the namespace, and #118 is
    /// what will eventually give a plugin somewhere to hang a command off it.
    pub fn send_chat(&mut self, line: &str) -> bool {
        if let Some(rest) = line.trim().strip_prefix('#') {
            tracing::debug!(
                command = rest,
                "client-local # command refused: no plugin registers commands \
                 yet (issue #118). Consumed rather than leaked to chat."
            );
            return false;
        }
        let Some(action) = compose_chat_action(line) else {
            return false;
        };
        if let Some(net) = &self.net {
            net.send_action(action);
            true
        } else {
            false
        }
    }

    /// The currently selected hotbar slot, `0..9`.
    #[must_use]
    pub fn selected_slot(&self) -> usize {
        self.read(|w| {
            w.get::<SelectedSlot>(self.local)
                .expect("the local player always carries SelectedSlot")
                .0
        })
    }

    /// The currently held `minecraft:writable_book`'s edit-screen seed, or
    /// `None` if neither hand holds one — `WindowApp::try_use`'s fork for
    /// issue #613's `EditBook` producer, the same shape its command-block
    /// fork already has.
    ///
    /// The main hand is checked first, matching vanilla's own hand
    /// resolution (`Player.interactionResultAndUpdate` tries
    /// `InteractionHand.MAIN_HAND` before `OFF_HAND`). `slot` is already in
    /// `ServerboundEditBookPacket`'s own addressing — the hotbar's *inventory*
    /// index (`0..=8`) for the main hand, or
    /// [`lodestone_game::menu::OFFHAND_NATIVE`] (`40`) for the off hand — see
    /// `crate::menu::book_edit`'s module doc.
    #[must_use]
    pub fn writable_book_in_hand(&self) -> Option<crate::menu::book_edit::BookEditOpen> {
        self.writable_book_in_hand_at(true)
            .or_else(|| self.writable_book_in_hand_at(false))
    }

    /// The writable book in exactly the hand named by a server `OPEN_BOOK`
    /// packet. Unlike [`Self::writable_book_in_hand`], this must not fall back
    /// across hands: a server can intentionally open the off-hand book while
    /// the main hand holds another item.
    #[must_use]
    pub fn writable_book_in_hand_at(
        &self,
        main_hand: bool,
    ) -> Option<crate::menu::book_edit::BookEditOpen> {
        const WRITABLE_BOOK: &str = "minecraft:writable_book";
        let menu = self.player_menu();
        let native_slot = if main_hand {
            self.selected_slot()
        } else {
            lodestone_game::menu::OFFHAND_NATIVE
        };
        let stack = menu.player_native(native_slot)?;
        if stack.item().to_string() != WRITABLE_BOOK {
            return None;
        }
        let pages = stack
            .writable_book_content()
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        let author = self
            .local_uuid()
            .and_then(|id| self.tab_list().get(&id).map(|entry| entry.profile.name.clone()))
            .unwrap_or_default();
        Some(crate::menu::book_edit::BookEditOpen {
            slot: i32::try_from(native_slot).unwrap_or(0),
            pages,
            author,
        })
    }

    /// The currently held `minecraft:written_book`'s reading-screen seed,
    /// or `None` if neither hand holds a **signed** book —
    /// [`Self::writable_book_in_hand`]'s read-only sibling, and the other
    /// branch of `WindowApp::try_use`'s book fork.
    ///
    /// Main hand first, for the same reason that method gives. Two
    /// differences from it, both because a signed book is immutable:
    ///
    /// * **No slot.** Nothing this screen does reaches the wire, so there is
    ///   no `ServerboundEditBookPacket` addressing to compute.
    /// * **The `written_book_content` component is required, not
    ///   defaulted.** A book with no component is a freshly crafted draft
    ///   that `writable_book_in_hand` handles; a `minecraft:written_book`
    ///   *cannot* exist without one, since signing is what creates both. So
    ///   `None` here is genuinely "not holding a signed book", never "holding
    ///   an empty one".
    ///
    /// This is the first production reader `ItemStack::written_book_content`
    /// has outside its own round trip back to the model — before it, the
    /// component decoded off the wire, folded into the menu, and reached
    /// nothing.
    #[must_use]
    pub fn written_book_in_hand(&self) -> Option<crate::menu::book_view::BookViewOpen> {
        self.written_book_in_hand_at(true)
            .or_else(|| self.written_book_in_hand_at(false))
    }

    /// The signed book in exactly the hand named by a server `OPEN_BOOK`
    /// packet. This keeps the packet's hand selector authoritative when both
    /// hands contain books.
    #[must_use]
    pub fn written_book_in_hand_at(
        &self,
        main_hand: bool,
    ) -> Option<crate::menu::book_view::BookViewOpen> {
        const WRITTEN_BOOK: &str = "minecraft:written_book";
        let menu = self.player_menu();
        let native_slot = if main_hand {
            self.selected_slot()
        } else {
            lodestone_game::menu::OFFHAND_NATIVE
        };
        let stack = menu.player_native(native_slot)?;
        if stack.item().to_string() != WRITTEN_BOOK {
            return None;
        }
        let content = stack.written_book_content()?;
        Some(crate::menu::book_view::BookViewOpen::from_pages(
            content.title.clone(),
            content.author.clone(),
            content.generation,
            &content.pages,
        ))
    }

    /// The local player's in-progress eat or drink, or `None`.
    ///
    /// The same [`ConsumeState::resolve`](crate::consume::ConsumeState::resolve) join
    /// the particle system runs, read out of the world here so the render side and
    /// the tick side cannot disagree about whether a consume is happening — see that
    /// module's docs for why the composition is a named symbol.
    #[must_use]
    pub fn consume_state(&self) -> Option<crate::consume::ConsumeState> {
        let (using, ticks, held, food, game_mode) = self.read(|w| {
            let using = w.resource::<crate::interact::UsingItem>().0;
            let ticks = w.resource::<lodestone_ecs::player::ItemUseTicks>().0;
            let slot = w.get::<SelectedSlot>(self.local).map_or(0, |s| s.0);
            let held = w
                .get::<lodestone_ecs::SessionMenus>(self.local)
                .and_then(|menus| menus.0.player().player_native(slot).cloned())
                .map(|stack| stack.item().to_string());
            let food = w.get::<Vitals>(self.local).and_then(|v| v.food);
            let game_mode = w.get::<ServerGameMode>(self.local).and_then(|m| m.0);
            (using, ticks, held, food, game_mode)
        });
        // `!crate::hud::can_hurt_player`, not a second creative/spectator check —
        // see `consume.rs`'s `emit_consume_particles`, which reads the same pair.
        let invulnerable = !crate::hud::can_hurt_player(game_mode);
        crate::consume::ConsumeState::resolve(using, ticks, held.as_deref(), food, invulnerable)
    }

    /// `(currUsageTime, useDuration)` for `ItemInHandRenderer.applyEatTransform`
    /// **this frame** — what `RenderState::set_item_use_source`'s closure returns.
    ///
    /// Reads the frame's own `interp_alpha` rather than taking one, exactly as
    /// [`Sim::hand_swing_progress`] does and for the same reason: two accessors that
    /// each ask the caller for a partial tick can be handed different ones, and a
    /// bob a frame out of step with the swing is not visible as a bug. The clock
    /// itself is a **tick** counter — `Instant::now` traps on wasm32.
    #[must_use]
    pub fn consume_usage_time(&self) -> Option<(f32, u32)> {
        let consume = self.consume_state()?;
        Some((
            lodestone_render::entity::eat_usage_time(
                consume.remaining_ticks(),
                self.clock().interp_alpha,
            ),
            consume.consumable.consume_ticks,
        ))
    }

    /// Select hotbar slot `slot` (`0..9`); out-of-range values are ignored. When
    /// the selection actually changes, echoes it to the server via
    /// [`ClientAction::SetCarriedItem`] so the held item stays in sync. No-op
    /// off a live connection beyond updating the local selection the HUD draws.
    pub fn select_slot(&mut self, slot: usize) {
        if slot >= HOTBAR_SLOTS || slot == self.selected_slot() {
            return;
        }
        self.write_local(|w, local| {
            if let Some(mut selected) = w.get_mut::<SelectedSlot>(local) {
                selected.0 = slot;
            }
        });
        self.send_selected_slot();
    }

    /// Advance the hotbar selection by `delta` slots, wrapping at both ends
    /// (mouse-wheel behaviour). A positive `delta` moves right, matching vanilla
    /// scroll-down.
    pub fn cycle_slot(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let n = HOTBAR_SLOTS as i32;
        let next = (self.selected_slot() as i32 + delta).rem_euclid(n) as usize;
        self.select_slot(next);
    }

    /// Push the current selection to the server. Best-effort: no-op without a
    /// live connection, and a closed session just drops it.
    fn send_selected_slot(&self) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetCarriedItem {
                slot: self.selected_slot() as i32,
            });
        }
    }

    /// The world difficulty and lock state, as the server last reported it
    /// (issue #411). `None` until the first `ClientEvent::DifficultyChanged`
    /// arrives — off a server, and briefly after login before the packet lands.
    ///
    /// Mirrors [`Self::selected_slot`]'s shape: a plain read of the local
    /// player's own `ServerDifficulty` session component, folded by
    /// `lodestone_ecs::session::apply_local_player_state` through the ordinary
    /// `NetIngest` path. See `crates/lodestone-ecs/src/session.rs`'s doc on
    /// [`ServerDifficulty`] for why this was an island until now: the fold was
    /// real and tested, but nothing in the shell read it.
    #[must_use]
    pub fn difficulty(&self) -> Option<(lodestone_model::Difficulty, bool)> {
        self.read(|w| {
            w.get::<ServerDifficulty>(self.local)
                .expect("the local player always carries ServerDifficulty")
                .0
        })
    }

    /// The server's own recipe-book panel state, as `RECIPE_BOOK_SETTINGS` (76)
    /// last reported it (issue #436's `SessionRecipeBookSettings` island).
    ///
    /// Same shape as [`Self::difficulty`], and the same story: the fold landed
    /// in `fd53995`, was gated through the real `SharedState::apply` path, and
    /// nothing in the shell read it. `RecipeBookSettings::reported` is what
    /// separates "the server never sent it" from "the server sent all-false" —
    /// the caller must check it, because the all-false record is
    /// indistinguishable from the default otherwise.
    #[must_use]
    pub fn recipe_book_settings(&self) -> lodestone_game::recipe::RecipeBookSettings {
        self.read(|w| {
            // Full module path: unlike `SessionTabList`, this one is not
            // re-exported at `lodestone_ecs`'s crate root.
            w.get::<lodestone_ecs::session::SessionRecipeBookSettings>(self.local)
                .expect("the local player always carries SessionRecipeBookSettings")
                .0
        })
    }

    /// The server's recipe-book **unlock** sync — `RECIPE_BOOK_ADD`/`_REMOVE`,
    /// `PLACE_GHOST_RECIPE` and `UPDATE_RECIPES` as last folded into
    /// `lodestone_ecs::session::SessionRecipeBook` (issue #687's missing hop 3).
    ///
    /// Same shape as [`Self::recipe_book_settings`] and [`Self::difficulty`]: a
    /// plain read of a local-player session component through the ordinary
    /// `NetIngest` path. Cloned rather than borrowed so the caller (the
    /// recipe-toast dispatcher) can diff it against its own "already toasted"
    /// set without holding the ECS guard across the comparison.
    #[must_use]
    pub fn known_recipes(&self) -> lodestone_game::recipe_sync::RecipeBookSync {
        self.read(|w| {
            // Full module path: like `SessionRecipeBookSettings`, this is not
            // re-exported at `lodestone_ecs`'s crate root.
            w.get::<lodestone_ecs::session::SessionRecipeBook>(self.local)
                .expect("the local player always carries SessionRecipeBook")
                .0
                .clone()
        })
    }

    /// Report the recipe-book panel's open/filter state for one book type —
    /// vanilla's `ServerboundRecipeBookChangeSettingsPacket`.
    ///
    /// The first producer of [`ClientAction::SetRecipeBookSettings`] anywhere
    /// outside `crates/protocol/`: all four families encoded it and nothing
    /// ever constructed one. Best-effort like [`Self::send_selected_slot`] — a
    /// closed session drops it.
    pub fn send_recipe_book_settings(
        &self,
        book_type: lodestone_model::RecipeBookType,
        open: bool,
        filtering: bool,
    ) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            });
        }
    }

    /// Report a recipe as seen — vanilla's `ServerboundRecipeBookSeenRecipePacket`,
    /// sent from `LocalPlayer::removeRecipeHighlight`
    /// (`RecipeBookComponent::recipeShown`, itself called from
    /// `RecipeButton::init` for every highlighted recipe a page just placed a
    /// button for). Clears the recipe's "new" tab-highlight and squeeze
    /// animation server-side.
    ///
    /// `ClientAction::RecipeBookSeenRecipe` was already encoded by every
    /// protocol family with no shell caller anywhere — the same
    /// outbound-island shape [`Self::send_select_trade`]'s doc names.
    /// Best-effort like the sends above it: a closed session drops it.
    pub fn send_recipe_book_seen_recipe(&self, display_id: i32) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::RecipeBookSeenRecipe { recipe: display_id });
        }
    }

    /// Select a merchant trade row — vanilla's `ServerboundSelectTradePacket`
    /// (`MerchantScreen.postButtonClick`, `MerchantScreen.java`), sent
    /// when the player clicks a trade-list row (issue #245's UI half).
    ///
    /// [`ClientAction::SelectTrade`] was already encoded by every protocol
    /// family with no shell caller anywhere — the outbound-island shape
    /// `ClientAction::SetFlying` was caught in. Best-effort like
    /// [`Self::send_recipe_book_settings`] — a closed session drops it. Note
    /// this does **not** locally move items into the payment slots the way
    /// vanilla's `MerchantMenu.tryMoveItems` does — that needs the offer list
    /// cross-referenced against the player's own inventory contents, which is
    /// prediction work for a later unit, not this send.
    pub fn send_select_trade(&self, index: i32) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SelectTrade { index });
        }
    }

    /// Confirm a beacon's primary/secondary power selection — vanilla's
    /// `ServerboundSetBeaconPacket` (`BeaconConfirmButton.onPress`,
    /// `BeaconScreen.java`), sent when the player presses the beacon
    /// screen's confirm button (issue #613's `SetBeaconEffects` remainder).
    ///
    /// [`ClientAction::SetBeaconEffects`] was already encoded by every
    /// protocol family with no shell caller anywhere — the outbound-island
    /// shape `ClientAction::SetFlying` was caught in. Best-effort like
    /// [`Self::send_select_trade`] — a closed session drops it. The caller
    /// is expected to have already gated this on
    /// `crate::container::beacon::BeaconSelection::can_confirm`
    /// (`app::container_input::WindowApp::handle_beacon_click` does), the
    /// same "predict, don't validate here" shape every other producer in
    /// this file takes: the server is the authority and corrects a wrong
    /// send via its own `container_set_data` broadcast.
    /// Press one of the enchanting table's three enchant-offer buttons —
    /// vanilla's `ServerboundContainerButtonClickPacket`
    /// (`EnchantmentScreen.mouseClicked` → `Minecraft.gameMode.
    /// handleInventoryButtonClick`, `EnchantmentScreen.java`), sent when the
    /// player clicks an offer row the client-side gate already accepted
    /// (issue #613's `ContainerButtonClick` remainder).
    ///
    /// [`ClientAction::ContainerButtonClick`] was already encoded by every
    /// protocol family with no shell caller anywhere — the same
    /// outbound-island shape [`Self::send_set_beacon_effects`]'s own doc
    /// describes. Best-effort like that method — a closed session drops it.
    /// The caller is expected to have already gated this on
    /// `crate::container::enchant::offer_clickable`
    /// (`app::container_input::WindowApp::handle_enchant_click` does), the
    /// same "predict, don't validate here" shape every other producer in
    /// this file takes: the server is the authority and corrects a wrong
    /// send via its own `container_set_data` broadcast.
    pub fn send_container_button_click(&self, window_id: i32, button_id: i32) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ContainerButtonClick { window_id, button_id });
        }
    }

    /// Toggle a crafter slot's enabled/disabled state — vanilla's
    /// `ServerboundContainerSlotStateChangedPacket`, sent from
    /// `CrafterScreen.updateSlotState`/`CrafterMenu.setSlotState`. Issue
    /// #613's `SetContainerSlotState` remainder; see
    /// `app::container_input::WindowApp::maybe_toggle_crafter_slot` for the
    /// click gate this is called from. Same "predict, don't validate here"
    /// shape as [`Self::send_container_button_click`] — the server's own
    /// `container_set_data` broadcast is the authority and corrects a wrong
    /// send.
    pub fn send_set_container_slot_state(&self, slot_id: i32, container_id: i32, new_state: bool) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetContainerSlotState {
                slot_id,
                container_id,
                new_state,
            });
        }
    }

    /// Report which item inside a hovered bundle the scroll wheel just
    /// highlighted — vanilla's `ServerboundSelectBundleItemPacket`
    /// (`BundleMouseActions.toggleSelectedBundleItem`, issue #616's
    /// `BUNDLE_ITEM_SELECTED` / #613's `SelectBundleItem` remainder). Purely
    /// informational: the only server-side effect is which item a later
    /// right-click removal takes, and the component's own `selectedItem`
    /// never round-trips back to the client (see
    /// [`lodestone_model::ItemComponents::bundle_contents`]'s doc) — there is
    /// no reply to predict against.
    ///
    /// [`ClientAction::SelectBundleItem`] was already encoded by every
    /// protocol family with no shell caller anywhere — the same
    /// outbound-island shape `ClientAction::SetFlying` was caught in.
    /// Best-effort like [`Self::send_select_trade`] — a closed session drops
    /// it. `selected_item_index` of `-1` is vanilla's own "unselect"
    /// sentinel, sent on unhover; see
    /// `crate::container::bundle::bundle_slot_scrolled`, this send's caller.
    pub fn send_select_bundle_item(&self, slot_id: i32, selected_item_index: i32) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SelectBundleItem {
                slot_id,
                selected_item_index,
            });
        }
    }

    /// Apply the world-creation Game Rules editor's overrides — vanilla's
    /// `ServerboundSetGameRulePacket` (issue #592's More tab). Sent once, by
    /// `app/session.rs`'s `drive_ui_from_session`, the moment a freshly
    /// created singleplayer session reaches `SessionPhase::Connected` — there
    /// is no server to hold this state on any earlier, since the integrated
    /// server itself only starts inside `begin_singleplayer`.
    ///
    /// No-op for an empty `entries` (a world whose rules were never touched)
    /// so a plain "Create New World" send nothing rather than a vacuous
    /// packet. Best-effort like [`Self::send_set_beacon_effects`] — a closed
    /// session drops it.
    pub fn send_set_game_rules(&self, entries: Vec<(lodestone_model::ResourceKey, String)>) {
        if entries.is_empty() {
            return;
        }
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetGameRules { entries });
        }
    }

    /// Client-initiated round-trip latency probe — vanilla's
    /// `ServerboundPingRequestPacket` (`PingDebugMonitor.tick`, issue #613's
    /// `PingRequest` remainder). `time` is the caller's own clock reading in
    /// milliseconds, echoed back on the `Pong` reply so round-trip time can
    /// be computed; see `app.rs`'s `redraw` for the F3-gated cadence this is
    /// called at. Best-effort like [`Self::send_select_trade`] — a closed
    /// session drops it.
    pub fn send_ping_request(&self, time: i64) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::PingRequest { time });
        }
    }

    pub fn send_set_beacon_effects(
        &self,
        primary: Option<lodestone_model::ResourceKey>,
        secondary: Option<lodestone_model::ResourceKey>,
    ) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetBeaconEffects { primary, secondary });
        }
    }

    /// Report which Advancements tab is open, or that the screen was closed,
    /// so the server knows which of the player's unlocked advancements have
    /// actually been shown — sent whenever the selected tab changes
    /// (including the default tab on open) and once more when the screen
    /// closes. See `docs/serverbound-coverage.md` for the wider audit this
    /// closes one line of.
    ///
    /// [`ClientAction::SeenAdvancements`] was already encoded by every
    /// protocol family with no shell caller anywhere — the outbound-island
    /// shape `ClientAction::SetFlying` was caught in. Best-effort like
    /// [`Self::send_select_trade`] — a closed session drops it.
    pub fn send_seen_advancements(&self, tab: Option<lodestone_model::ResourceKey>) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SeenAdvancements { tab });
        }
    }

    /// Set a container slot's contents by creative fiat — vanilla's
    /// `ServerboundSetCreativeModeSlotPacket`, sent by the creative-inventory
    /// screen (issue #158).
    ///
    /// The **producer** half of a round trip whose encoder already existed:
    /// [`ClientAction::SetCreativeModeSlot`] is encoded by every protocol family
    /// and had no shell caller at all — the outbound-island shape
    /// `ClientAction::SetFlying` was caught in.
    ///
    /// `slot` is a window-0 container index (`36 + n` for hotbar slot `n`), and a
    /// negative value is vanilla's own "drop this stack" encoding. Sent directly
    /// rather than queued through `ActionQueue`, like
    /// [`Self::send_recipe_book_settings`]: this is a discrete click, not a
    /// per-tick state.
    pub fn send_creative_slot(&self, slot: i16, item: lodestone_model::Identifier, count: u32) {
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetCreativeModeSlot {
                slot: i32::from(slot),
                item: Some(lodestone_model::item::ItemStack::new(item, count)),
            });
        }
    }

    /// Write one window-0 menu slot **locally and on the wire** — vanilla's
    /// `MultiPlayerGameMode.handleCreativeModeItemAdd`, which is what the creative
    /// inventory screen uses in place of a `container_click` for every mutation it
    /// makes.
    ///
    /// # Why the local half is not optional
    ///
    /// `SET_CREATIVE_MODE_SLOT` is a **silent** write: the server applies it and sends
    /// nothing back (ours does exactly that, and so does vanilla's
    /// `handleSetCreativeModeSlot`). So a send with no prediction leaves the client's
    /// own inventory unchanged *forever* — the item is really in the server's
    /// inventory and the hotbar cell stays blank. That is the same shape as the
    /// unpredicted `DROP_ITEM`, and it is why [`Self::send_creative_slot`] alone was
    /// never enough to make the creative screen work.
    ///
    /// The local write goes in as a synthesized [`ClientEvent::ContainerSlot`] against
    /// window 0 rather than through a new mutator, so it lands through the **same**
    /// `Menus::apply` fold a real `container_set_slot` would take — including the
    /// re-addressing a window-0 player-section slot needs while a container owns the
    /// inventory. Marking it *confirmed* (which that fold does) is correct here rather
    /// than optimistic: this packet cannot be rejected or corrected, so there is no
    /// provisional state to keep.
    ///
    /// The item is carried as a canonical stack, so components (`max_stack_size`,
    /// `equippable`) survive into the local menu even though the wire form drops them.
    pub fn apply_creative_slot(
        &mut self,
        menu_index: usize,
        item: Option<lodestone_game::item::ItemStack>,
    ) {
        let model = item.as_ref().map(lodestone_model::ItemStack::from);
        if let Some(net) = &self.net {
            net.send_action(ClientAction::SetCreativeModeSlot {
                slot: i32::try_from(menu_index).unwrap_or(-1),
                item: model.clone(),
            });
        }
        let slot = i32::try_from(menu_index).unwrap_or(0);
        self.write_local(|w, local| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                // The current state id, unchanged: nothing about a creative write
                // advances the container's synchronisation counter.
                let state_id = menus.0.player().state_id() as i32;
                menus.0.apply(&lodestone_model::ClientEvent::ContainerSlot {
                    window_id: 0,
                    state_id,
                    slot,
                    item: model,
                });
            }
        });
    }

    /// Replace the shared cursor stack, locally only.
    ///
    /// There is no serverbound verb for the cursor, and there does not need to be:
    /// vanilla's `ItemPickerMenu.setCarried` delegates to `player.inventoryMenu`, so a
    /// creative cursor is purely client state until it is put into a slot — and *that*
    /// is what [`Self::apply_creative_slot`] reports. Routed through
    /// [`ClientEvent::CursorItemChanged`] for the same reason: one fold, not a second
    /// mutator that could disagree with it.
    pub fn set_local_carried(&mut self, item: Option<lodestone_game::item::ItemStack>) {
        let model = item.as_ref().map(lodestone_model::ItemStack::from);
        self.write_local(|w, local| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                menus
                    .0
                    .apply(&lodestone_model::ClientEvent::CursorItemChanged { item: model });
            }
        });
    }

    /// The server's game rules, as `GAME_EVENT`/`CHANGE_GAME_STATE` last
    /// reported them (issue #436's `SessionGameRules` island).
    ///
    /// Cloned rather than `Copy`ed: [`GameRuleValues`] wraps a `BTreeMap`. The
    /// map is small (a handful of rules a server actually reports), and the
    /// alternative — handing out a reference — would mean handing out a live
    /// read guard, which [`Self::player`]'s doc rules out.
    #[must_use]
    pub fn game_rules(&self) -> lodestone_game::levelstate::GameRuleValues {
        self.read(|w| {
            w.get::<lodestone_ecs::session::SessionGameRules>(self.local)
                .expect("the local player always carries SessionGameRules")
                .0
                .clone()
        })
    }

    /// The advancement tree and the local player's progress on it, as
    /// `UPDATE_ADVANCEMENTS` last reported them (issue #167).
    ///
    /// Cloned for [`Self::game_rules`]' reason — handing out a reference means
    /// handing out a live read guard. The clone is only taken while the
    /// Advancements screen is open or a toast is being polled, not per frame of
    /// ordinary play, and `crate::menu::advancements::AdvancementProgress`
    /// immediately reduces it to a lean per-id snapshot.
    ///
    /// The store carries **no tree positions** — 26.2's advancement JSON has none
    /// and the server computes them — so this is a source of *progress* keyed by
    /// id and never a source of tree shape. See the screen's module doc.
    #[must_use]
    pub fn advancements(&self) -> lodestone_game::advancement::AdvancementStore {
        self.read(|w| {
            w.get::<lodestone_ecs::session::SessionAdvancements>(self.local)
                .expect("the local player always carries SessionAdvancements")
                .0
                .clone()
        })
    }

    /// The local player's statistics counters, as `award_stats` last reported
    /// them.
    ///
    /// Cloned for [`Self::advancements`]' reason: handing out a reference hands
    /// out a live read guard. The map is sparse and holds only what the server has
    /// actually awarded, and the one caller
    /// (`app::session`'s reconciliation, which projects it onto
    /// `crate::menu::stats::StatsSnapshot`) is gated on being in a session — the
    /// same shape and the same cost as the `tab_list()` clone the Social
    /// Interactions roster takes every frame.
    ///
    /// This exists because the Statistics screen drew `StatsSnapshot::default()`
    /// unconditionally: the decode landed and nothing read it.
    #[must_use]
    pub fn statistics(&self) -> lodestone_game::progress::Statistics {
        self.read(|w| {
            w.get::<lodestone_ecs::session::SessionStatistics>(self.local)
                .expect("the local player always carries SessionStatistics")
                .0
                .clone()
        })
    }

    /// Every filled map the server has sent contents for, as `MAP_ITEM_DATA`
    /// last reported them (issue #184).
    ///
    /// Keyed on **map id**, not on an entity: several players and several item
    /// frames can show the same map, which is why the fold is session-scoped.
    /// Patch rectangles are already blitted into the full 128×128 grid by
    /// `MapStore::apply`, so a caller reads `MapState::color_at` and never has to
    /// think about sub-rectangles.
    #[must_use]
    pub fn maps(&self) -> lodestone_game::maps::MapStore {
        self.read(|w| {
            w.get::<lodestone_ecs::session::SessionMaps>(self.local)
                .expect("the local player always carries SessionMaps")
                .0
                .clone()
        })
    }

    /// `(how many maps the server has sent, the lowest-numbered map's explored
    /// fraction)` for the F3 overlay, or `None` when none have arrived.
    ///
    /// "Explored" is the fraction of the 128×128 grid whose colour byte is
    /// non-zero — `0` is vanilla's transparent/unexplored `MapColor.NONE`, so a
    /// freshly crafted map reads `0%` and one carried across a continent
    /// approaches `100%`. That makes this a real observation of the fold rather
    /// than a count of packets: a wire that arrived but blitted its patch
    /// rectangle into the wrong place still moves this number, but a patch
    /// decoded as 128×128 when it is really two columns wide would jump to a
    /// suspiciously round figure.
    #[must_use]
    pub fn map_debug(&self) -> Option<(usize, f32)> {
        let store = self.maps();
        let first = store.ids().next()?;
        let map = store.get(first)?;
        let total = lodestone_game::maps::MAP_SIZE * lodestone_game::maps::MAP_SIZE;
        let explored = map.colors.iter().filter(|c| **c != 0).count();
        Some((store.len(), explored as f32 / total as f32))
    }

    /// The player's spawn point, as `SET_DEFAULT_SPAWN_POSITION` last reported
    /// it (issue #436's `SessionSpawnPoint` island).
    ///
    /// `is_reported()` on the result separates "the server never sent one" from
    /// "the server sent the origin" — the distinction a compass needle needs in
    /// order to spin rather than point north.
    #[must_use]
    pub fn spawn_point(&self) -> lodestone_game::levelstate::SpawnPoint {
        self.read(|w| {
            w.get::<lodestone_ecs::session::SessionSpawnPoint>(self.local)
                .expect("the local player always carries SessionSpawnPoint")
                .0
                .clone()
        })
    }

    /// The world border's warning-overlay strength for this frame, in `0.0..=1.0`
    /// — issue #436's `SessionWorldBorder` island reaching pixels.
    ///
    /// A direct port of `Hud.extractVignette` (`Hud.java`,
    /// `.cache/mc/26.2/client-src`):
    ///
    /// ```text
    /// distToBorder         = worldBorder.getDistanceToBorder(camera)
    /// movingBlocksThreshold = min(getLerpSpeed() * getWarningTime(),
    ///                             abs(getLerpTarget() - getSize()))
    /// warningDistance      = max(getWarningBlocks(), movingBlocksThreshold)
    /// strength             = distToBorder < warningDistance
    ///                        ? 1 - distToBorder / warningDistance : 0
    /// ```
    ///
    /// # The clock is `FrameClock`, deliberately
    ///
    /// Not `WorldTime`: the server can freeze `WorldTime` with `advance_time`,
    /// and a frozen clock would freeze a resize mid-flight.
    /// `lodestone_ecs::session::apply_world_border` stamps the extent off
    /// `FrameClock` for the same reason, so reading it with anything
    /// else would compare two different time bases.
    ///
    /// # A unit hazard, recorded rather than silently resolved
    ///
    /// Vanilla's `getLerpSpeed()` is `abs(from - to) / (lerpEnd - lerpBegin)`
    /// (`WorldBorder.java`) where that denominator is
    /// `lerpSizeBetween`'s third parameter — named **`ticks`**
    /// (`WorldBorder.java`), not milliseconds. Our `BorderExtent::Moving`
    /// stores `duration_ms`, documented as milliseconds as the server sent it,
    /// so the conversion below is explicit at [`MILLIS_PER_TICK`].
    ///
    /// **If `duration_ms` is really ticks, the moving term is 20× too small.**
    /// It fails safe — the `max(warning_blocks, …)` floor still applies, so the
    /// overlay appears at the static distance and only the *early* warning for
    /// an incoming shrink is short. What would falsify it: a live server
    /// shrinking a border and a measurement of when the tint first appears. The
    /// static case, which is what the gates pin, is exact either way because
    /// `StaticBorderExtent.getLerpSpeed()` returns `0.0`
    /// (`WorldBorder.java`) and the floor wins outright.
    /// The command block the crosshair is on, resolved into the edit screen's
    /// opening state — issue #47's missing trigger, tracked on #436.
    ///
    /// `None` when the crosshair is on nothing, on a block that is not a
    /// command block, or when the chunk store has no data at that cell. Only
    /// the first of those is a "no interaction"; the others are "not this
    /// interaction", and both mean the ordinary use-item path should run.
    ///
    /// # Why this reads the raw NBT rather than waiting for a typed decode
    ///
    /// See `crate::command_block_source`'s module doc. In short: the ledger
    /// entry's `grep -rn "CommandBlock" crates/lodestone-model crates/protocol`
    /// is true and answers a neighbouring question — there is no *typed*
    /// decode, and none is needed, because `BlockEntity::nbt` already carries
    /// the payload and `block_states` already answers what block is there.
    ///
    /// # Deliberately no permission gate
    ///
    /// Vanilla guards on `player.canUseGameMasterBlocks()`
    /// (`CommandBlock.useWithoutItem`), which is op level 2 **and** creative.
    /// This client tracks neither: there is no op level anywhere in the
    /// workspace, and the server is the authority regardless — it rejects a
    /// `SetCommandBlock` from an unauthorised player, exactly as it rejects
    /// every other unauthorised action we optimistically send. Opening a local
    /// editor that the server then refuses is the honest failure; refusing to
    /// open it on a *guessed* permission would be a dead control.
    #[must_use]
    pub fn targeted_command_block(
        &self,
    ) -> Option<crate::menu::command_block::CommandBlockOpen> {
        let hit = self.target()?;
        let pos = lodestone_model::BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        let store = self.read(|w| w.resource::<ChunkWorld>().clone());
        let world = store.read();
        let chunk_pos = lodestone_world::ChunkPos {
            x: pos.x.div_euclid(16),
            z: pos.z.div_euclid(16),
        };
        let chunk = world.get(chunk_pos)?;
        let rel_x = pos.x.rem_euclid(16) as usize;
        let rel_z = pos.z.rem_euclid(16) as usize;
        let state_id = chunk.column.get_block(rel_x, pos.y, rel_z);
        // The block state is the truth about *whether* this is a command
        // block; the record is only where the payload lives. So this resolves
        // the state first and treats a missing record as an empty payload
        // rather than as "not a command block" — the same
        // state-wins-over-record rule `crate::block_entities` follows, and the
        // reason a freshly placed command block (state written, no record yet)
        // still opens.
        crate::command_block_source::mode_for_state(state_id)?;
        let nbt = chunk
            .block_entities
            .iter()
            .find(|be| {
                usize::from(be.rel_x) == rel_x
                    && usize::from(be.rel_z) == rel_z
                    && i32::from(be.y) == pos.y
            })
            .map_or(&lodestone_core::Nbt::End, |be| &be.nbt);
        crate::command_block_source::command_block_open(pos, state_id, nbt)
    }

    /// The sign at `pos`'s currently-synced text — `SignText::parse` on the
    /// block entity's NBT if a record exists there, `SignText::default()`
    /// (four blank lines, unwaxed) otherwise. Used to seed the sign-editing
    /// screen when [`NetUpdate::SignEditorOpened`](crate::net::NetUpdate::
    /// SignEditorOpened) names a position — see [`Self::poll_net`].
    ///
    /// Unlike [`Self::targeted_command_block`], this does **not** check the
    /// block state names a sign: the server decides whether a player may
    /// edit (the packet only arrives for a real sign it has already
    /// authorised), so trusting the position it named is the honest choice —
    /// there is no "not a sign" failure mode to guard here, only "no record
    /// yet", which degrades exactly as vanilla's own `new SignText()` does.
    #[must_use]
    pub fn sign_text_at(&self, pos: lodestone_model::BlockPos) -> lodestone_world::SignText {
        let store = self.read(|w| w.resource::<ChunkWorld>().clone());
        let world = store.read();
        let chunk_pos = lodestone_world::ChunkPos {
            x: pos.x.div_euclid(16),
            z: pos.z.div_euclid(16),
        };
        let Some(chunk) = world.get(chunk_pos) else {
            return lodestone_world::SignText::default();
        };
        let rel_x = pos.x.rem_euclid(16) as usize;
        let rel_z = pos.z.rem_euclid(16) as usize;
        let nbt = chunk
            .block_entities
            .iter()
            .find(|be| {
                usize::from(be.rel_x) == rel_x
                    && usize::from(be.rel_z) == rel_z
                    && i32::from(be.y) == pos.y
            })
            .map_or(&lodestone_core::Nbt::End, |be| &be.nbt);
        lodestone_world::SignText::parse(nbt)
    }

    /// `(distance_to_border, warning_distance, warning_strength)`, or `None`
    /// until the server has actually sent a border packet.
    ///
    /// `WorldBorder::initialized` is the gate rather than a size comparison:
    /// the default border is a real, legal `MAX_SIZE` border at the origin, so
    /// "looks like the default" cannot distinguish an unreported border from a
    /// server that genuinely set one that big.
    #[must_use]
    pub fn world_border_warning(&self) -> Option<(f64, f64, f32)> {
        // `clock()` and `player()` are themselves `read`s, so both must be
        // taken *before* the one below — the deadlock discipline
        // `sim/session.rs`'s chat-age accessor follows.
        let now = self.clock().secs;
        let pos = self.player().position;
        let border = self.read(|w| {
            w.get::<lodestone_ecs::session::SessionWorldBorder>(self.local)
                .expect("the local player always carries SessionWorldBorder")
                .0
        });
        if !border.initialized {
            return None;
        }
        let (dist, warn_at, strength) = border_warning(&border, pos.x, pos.z, now);
        Some((dist, warn_at, strength))
    }
}

/// Milliseconds per tick — the conversion `world_border_warning` needs to read
/// `BorderExtent::Moving::duration_ms` as vanilla's tick-denominated lerp
/// duration. See that method's "unit hazard" section.
const MILLIS_PER_TICK: f64 = 50.0;

/// The pure half of [`Sim::world_border_warning`], so the formula is testable
/// against hand-computed vanilla values with no ECS, no clock and no player.
///
/// Split out for exactly the reason `recipe_toast_view` is a free function: a
/// gate that had to stand up a `Sim` to check an arithmetic port would be
/// measuring the harness as much as the formula.
/// Returns `(distance_to_border, warning_distance, strength)` — all three,
/// because the first two are what make a failing gate diagnosable. A gate that
/// only saw `strength` could not tell "the distance is wrong" from "the
/// threshold is wrong".
#[must_use]
pub(crate) fn border_warning(
    border: &lodestone_game::worldborder::WorldBorder,
    x: f64,
    z: f64,
    now_secs: f64,
) -> (f64, f64, f32) {
    use lodestone_game::worldborder::BorderExtent;

    // `StaticBorderExtent.getLerpSpeed()` is `0.0`; only a moving border has a
    // speed at all.
    let lerp_speed = match border.extent {
        BorderExtent::Static { .. } => 0.0,
        BorderExtent::Moving {
            from,
            to,
            duration_ms,
            ..
        } => {
            #[allow(clippy::cast_precision_loss)]
            let duration_ticks = duration_ms as f64 / MILLIS_PER_TICK;
            if duration_ticks > 0.0 {
                (from - to).abs() / duration_ticks
            } else {
                0.0
            }
        }
    };
    let size = border.extent.size_at(now_secs);
    let moving_blocks = (lerp_speed * f64::from(border.warning_time))
        .min((border.target_size() - size).abs());
    let warning_distance = f64::from(border.warning_blocks).max(moving_blocks);
    let dist = border.distance_to_border(x, z, now_secs);
    if warning_distance <= 0.0 || dist >= warning_distance {
        return (dist, warning_distance, 0.0);
    }
    // Vanilla does not clamp the low end here, but `distance_to_border` goes
    // negative outside the border, which would push this above 1.0. Vanilla
    // clamps immediately afterwards (`Mth.clamp(borderWarningStrength, 0, 1)`,
    // `Hud.java`), so clamping here is the same answer one step earlier.
    #[allow(clippy::cast_possible_truncation)]
    let strength = (1.0 - dist / warning_distance).clamp(0.0, 1.0) as f32;
    (dist, warning_distance, strength)
}
