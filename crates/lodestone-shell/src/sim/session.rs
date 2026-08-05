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
    pub fn connect(&mut self, host: String, port: u16, protocol: i32) {
        let net = NetClient::connect(
            host,
            port,
            protocol,
            Some((Arc::clone(&self.ecs), self.local)),
        );
        self.attach_net(net);
    }

    /// Attach a live connection whose updates are polled each frame.
    // `mut` is used only by the `#[cfg(test)]` `bind_session` below.
    #[cfg_attr(not(test), allow(unused_mut))]
    pub fn attach_net(&mut self, mut net: NetClient) {
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
    ///   it is one `World` and one entity now, `Sim::sidebar`/`player_rows`/
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
            self.write(|w| w.insert_resource(ChunkWorld::default()));
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
        self.write(|w| {
            insert_hud_components(w, local);
            lodestone_ecs::insert_session_components(w, local);
        });
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
    /// row only while saturation is exhausted (`Hud.java:977-979`), so without
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

    /// The current tab-list, formatted as `NAME  <latency>ms` rows sorted by
    /// vanilla display order. Empty until the server sends player-list data.
    ///
    /// # Read straight off the component since §4.1(c)
    ///
    /// This and the three accessors below used to go out through `NetClient` into
    /// the *client's* `World`, because the net thread's fold lived there and a
    /// component in one `World` is unreachable from another. There is one `World`
    /// now and [`Self::local`] is the entity the fold writes, so the round trip is
    /// gone. Still exactly one fold — `lodestone_ecs::session`'s `NetIngest`
    /// systems — and still one copy of it; what changed is only who reads it.
    #[must_use]
    pub fn player_rows(&self) -> Vec<String> {
        let list = self.read(|w| {
            w.get::<lodestone_ecs::SessionTabList>(self.local)
                .map(|list| list.0.clone())
                .unwrap_or_default()
        });
        crate::tablist::player_rows(&list, self.translator().as_ref())
    }

    /// The same folded tab list [`Self::player_rows`] formats, unformatted —
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
    /// (`Player.java:1816-1818`, `.cache/mc/26.2/src`).
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

    /// The attack-cooldown fraction the crosshair indicator fills to,
    /// `0.0..=1.0` — vanilla's `getAttackStrengthScale(0.0F)`
    /// (`Player.java:1826-1828`), the exact call `Hud.extractCrosshair` makes
    /// for the crosshair-style indicator (`Hud.java:448`). The `a` (partial
    /// tick) argument is fixed at `0.0` here, same as that call site; nothing
    /// in this shell threads a render-time partial tick into `Sim`'s other
    /// accessors either (see [`Self::health`]/[`Self::xp`]).
    #[must_use]
    pub fn attack_strength_scale(&self) -> f32 {
        self.attack_strength_scale_at(0.0)
    }

    /// `getAttackStrengthScale(a)` (`Player.java:1826-1828`) with the partial
    /// tick argument exposed, because vanilla itself calls this with two
    /// different values for two different purposes: `0.0F` for the crosshair
    /// indicator ([`Self::attack_strength_scale`], `Hud.java:448`) and `0.5F`
    /// for `Player.attack`'s own `fullStrengthAttack` gate
    /// (`Player.java:956,962`), which [`Self::maybe_spawn_crit_particles`]
    /// needs. One private helper rather than two public accessors that would
    /// otherwise duplicate the ticker read and delay computation.
    #[must_use]
    pub(crate) fn attack_strength_scale_at(&self, a: f32) -> f32 {
        let delay = self.attack_strength_delay();
        let ticker = self.read(|w| w.get::<AttackStrengthTicker>(self.local).map_or(0, |t| t.0));
        ((ticker as f32 + a) / delay).clamp(0.0, 1.0)
    }

    /// The title/subtitle overlay as `(title, subtitle, alpha)`, `Some` while a
    /// server-sent title is visible. `Text` is flattened to a legacy `§` string
    /// at read time, matching the chat path, so colour survives once decoded.
    #[must_use]
    pub fn title_overlay(&self) -> Option<(String, Option<String>, f32)> {
        let state = self.read(|w| {
            w.get::<TitleOverlay>(self.local)
                .expect("the local player always carries TitleOverlay")
                .0
                .clone()
        });
        let title = state.title()?;
        Some((
            self.resolve_text(title).to_legacy_string(),
            state
                .subtitle()
                .map(|s| self.resolve_text(s).to_legacy_string()),
            state.alpha(),
        ))
    }

    /// The action-bar message as `(text, alpha)`, `Some` while a GameInfo
    /// message is visible (fades over its final ticks).
    #[must_use]
    pub fn action_bar_overlay(&self) -> Option<(String, f32)> {
        let state = self.read(|w| {
            w.get::<ActionBarOverlay>(self.local)
                .expect("the local player always carries ActionBarOverlay")
                .0
                .clone()
        });
        let text = state.text()?;
        Some((self.resolve_text(text).to_legacy_string(), state.alpha()))
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

    /// `Player.hasInfiniteMaterials()` — `Abilities.instabuild`
    /// (`Player.java`; `AnvilMenu.mayPickup` and
    /// `EnchantmentScreen.java:111` both gate on it). Used by
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
    /// [`Self::player_rows`] on why that is a direct read since §4.1(c). Note the
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
}
