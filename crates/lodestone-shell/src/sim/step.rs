//! `Sim`'s **per-frame driver**: [`Sim::step`] itself, everything it calls
//! that is not a resource accessor, and the per-frame work `app.rs` drives
//! around it -- seam 12 of the sim.rs decomposition
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
//! [`Sim::step`] is the fixed-timestep loop: one `Update` schedule, then N
//! catch-up `GameTick` schedules, then `poll_net`/`fold_entities`/`Extract`.
//! Around it sit the things that must happen once per *frame* rather than
//! once per tick and so cannot be systems -- `apply_mouse` (vanilla's
//! `MouseHandler.turnPlayer` is off the render loop too), `update_target` and
//! `update_entity_target` (the pick ray, cast from the already-interpolated
//! camera), the mesh drains `app.rs` uploads from, and `refresh_stats`.
//! `drain_action_queue` is here because it is the tail of each tick: the one
//! funnel where every queued [`ClientAction`] reaches the socket, in order,
//! and where a queued main-hand `SwingArm` starts the local animation.
//!
//! Reading `step` beside `apply_mouse` and `update_target` is the point. The
//! frame's ordering is load-bearing in three places its own comments record --
//! `Update` before the tick loop so `advance_interp_clocks` runs first, the
//! walk-bob inputs captured *before* the tick's movement, and the swing clock
//! ticking before the queue drains (a deliberate one-tick offset from
//! vanilla). None of that is checkable if the participants live in three
//! files.
//!
//! # What widened
//!
//! `drain_action_queue` and `update_entity_target` go private ->
//! `pub(crate)`, both called from `sim/tests.rs` -- a *sibling*, so its
//! `use super::*;` reaches `sim`'s items but not another child's private
//! ones. `refresh_stats` stays private: `step`, in this file, is its only
//! caller.
//!
//! `use super::*;` for the same reason every earlier seam file uses it: this
//! module is a *descendant* of `sim`, so it already has the same visibility
//! into `Sim`'s private fields, into `sim.rs`'s remaining private helpers and
//! into everything `sim.rs` re-exports that `sim::tests` has always had, with
//! no need to enumerate any of it.

use super::*;

impl Sim {
    /// Number of meshing jobs still outstanding.
    #[must_use]
    pub fn pending_meshes(&self) -> usize {
        self.terrain(|t| t.scheduler.pending())
    }

    /// Collect finished meshes for the caller to upload to the GPU.
    ///
    /// Also records each key into `TerrainMesh::uploaded_sections`, which is how
    /// [`Sim::end_session`] later knows every section the GPU is holding for
    /// this session and can queue every one of them for removal.
    pub fn drain_meshes(&mut self) -> Vec<Meshed> {
        self.terrain_mut(TerrainMesh::drain_meshes)
    }

    /// Block until every scheduled mesh is ready (used by headless runs/tests).
    pub fn drain_all_meshes(&mut self) -> Vec<Meshed> {
        self.terrain_mut(TerrainMesh::drain_all_meshes)
    }

    /// Sections that became empty (drained by the app to remove GPU meshes).
    pub fn drain_removals(&mut self) -> Vec<SectionKey> {
        self.terrain_mut(TerrainMesh::drain_removals)
    }

    /// Frames rendered per physics tick since start (fixed-timestep health).
    #[must_use]
    pub fn frames_per_tick(&self) -> f32 {
        self.clock().frames_per_tick()
    }

    /// Apply accumulated mouse motion to the view angles.
    ///
    /// Deliberately **not** a `GameTick` system: mouse-look is per-frame in
    /// vanilla too (`MouseHandler.turnPlayer` runs off the render loop, not the
    /// tick), so binding it to 20 Hz would make aiming feel stepped at high
    /// frame rates.
    pub fn apply_mouse(&mut self) {
        let (dx, dy) = self.input_mut(InputState::take_mouse);
        if dx != 0.0 || dy != 0.0 {
            // Issue #443: the *pushed* option, not `self.config.sensitivity`.
            // The latter is argv-derived and fixed for the process lifetime,
            // so reading it made the persisted slider apply only at the next
            // launch. See `Sim::sensitivity`'s own doc comment.
            let sensitivity = self.sensitivity;
            let player = self.player();
            let (yaw, pitch) = apply_look_inverted(
                player.yaw,
                player.pitch,
                dx,
                dy,
                sensitivity,
                self.invert_mouse_x,
                self.invert_mouse_y,
            );
            self.player_mut(|player| {
                player.yaw = yaw;
                player.pitch = pitch;
            });
        }
    }

    /// Push vanilla's `invertMouseX`/`invertMouseY` options down from the menu
    /// layer (issue #203), the same way [`Self::set_view_bobbing`] does for
    /// View Bobbing. Cheap and idempotent; `app.rs` calls it once per frame,
    /// before [`Self::step`] so the very tick the option changes already
    /// sees it.
    pub fn set_mouse_invert(&mut self, invert_x: bool, invert_y: bool) {
        self.invert_mouse_x = invert_x;
        self.invert_mouse_y = invert_y;
    }

    /// Push vanilla's `sensitivity` option down from the menu layer (issue
    /// #443), the same way [`Self::set_mouse_invert`] does. Cheap and
    /// idempotent; `app/redraw.rs` calls it once per frame **before**
    /// [`Self::step`] so the very tick the slider moves already turns at the
    /// new rate — pushing it after `step` would apply each change one frame
    /// late.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity;
    }

    /// Push vanilla's `key.sneak`/`key.sprint`/`key.attack`/`key.use`
    /// hold-vs-toggle options down from the menu layer (issues #202/#444).
    /// Stored rather than applied directly because the actual
    /// [`InputState::set_toggle_modes`] call has to happen inside
    /// [`Self::step`] (see that field's doc) — `Sim` has no `MenuNav` to read
    /// from at that point, only whatever was last pushed here.
    pub fn set_toggle_modes(
        &mut self,
        toggle_sneak: bool,
        toggle_sprint: bool,
        toggle_attack: bool,
        toggle_use: bool,
    ) {
        self.toggle_sneak = toggle_sneak;
        self.toggle_sprint = toggle_sprint;
        self.toggle_attack = toggle_attack;
        self.toggle_use = toggle_use;
    }

    /// Push vanilla's `options.autoJump` down from the menu layer (issue
    /// #444), the same shape as [`Self::set_toggle_modes`] — stored here,
    /// applied inside [`Self::step`] where the physics world is readable.
    pub fn set_auto_jump(&mut self, auto_jump: bool) {
        self.auto_jump = auto_jump;
    }

    /// Push vanilla's `options.sprintWindow` down from the menu layer (issue
    /// #444) — the double-tap-forward window in 20 Hz ticks. `0` disables
    /// double-tap sprint. Pushed once per `step` by the shell, so a mid-session
    /// change from the settings screen applies on the very next tick.
    pub fn set_sprint_window_ticks(&mut self, ticks: u8) {
        self.sprint_window_ticks = ticks;
    }

    /// Hand everything the `GameTick` systems queued to the socket, in order.
    ///
    /// The queue is drained (not read) even with no connection, so a
    /// disconnected session cannot accumulate a session's worth of stale
    /// actions to deliver on reconnect.
    ///
    /// # Also the animation half of every queued swing
    ///
    /// A [`ClientAction::SwingArm`] on this queue is the *same* event vanilla's
    /// `LivingEntity.swing` handles: it both sends `ClientboundAnimatePacket` to
    /// everyone else **and** starts the swinger's own animation clock. This is the
    /// single funnel every tick-driven swing passes through — notably
    /// `interact.rs`'s hold-to-mine loop via `lodestone_game::mining`, which is
    /// what makes the arm swing while breaking a block — so hooking it here means
    /// a new producer of swings animates for free rather than having to remember
    /// to. [`Self::use_item_live`] is the one swing that does *not* come through
    /// here (it writes to the socket directly, to control wire order) and calls
    /// [`Self::swing_hand`] itself.
    ///
    /// Deliberately **outside** the `if let Some(net)` below: the animation is
    /// client-side and must not depend on having a live socket, exactly as the
    /// demo world's [`Self::break_block`] swings with no connection at all.
    pub(crate) fn drain_action_queue(&mut self) {
        // The guard is released before `net.send_action`, per `EcsHandle`'s rule 1:
        // `send_action` is a channel push today, but the whole `NetClient` surface
        // otherwise reads this same `World` through `ClientHandle`, and holding a
        // write guard into it would deadlock the moment one of those was reached.
        let actions = self.write(|w| {
            let mut actions = std::mem::take(&mut w.resource_mut::<ActionQueue>().0);
            // Vanilla's tick tail. `Minecraft.tick` ends with
            // `connection.send(ServerboundClientTickEndPacket.INSTANCE)`
            // (`client-src/net/minecraft/client/Minecraft.java:1832-1835`) — every
            // tick, after everything else the tick queued, whenever a connection
            // exists and the game is not paused.
            //
            // **`ClientAction::EndClientTick` had no producer outside a test**:
            // v770 encodes it and nothing sent it, the `SetFlying` shape. It is
            // not cosmetic. `ServerGamePacketListenerImpl.handleClientTickEnd`
            // (`:2195-2202`) sets `knownMovement` to `Vec3.ZERO` when **no**
            // movement packet arrived that tick, so without this the server keeps
            // our last movement vector forever — `resetLastActionTime` (the AFK
            // clock) and every server-side `getKnownMovement()` reader see a
            // player still travelling after they stop.
            //
            // Appended here rather than by a `TickSet::Send` system for two
            // reasons: it must be **last**, which is a fragile thing to express as
            // system ordering among the interaction systems, and vanilla's own
            // send site is likewise outside the per-entity tick. It goes in
            // *before* the egress filter below on purpose — a plugin that
            // suppresses everything should be able to suppress this too.
            //
            // Gated on `Egress::in_world` — **the movement packet's gate, not the
            // player-input packet's.** `send_move_action` asks `in_world` alone
            // while `send_player_input` also asks `live`, and vanilla's send site
            // matches the former: it sits inside `if (this.level != null)` with a
            // `connection != null && !this.pause` guard, and has nothing to do
            // with whether a resource pack resolved (which is what `live` means
            // here). Ungated entirely, a merely-*Connecting* sim emits one per
            // tick before the adapter has a Play-state packet for it — the same
            // dropped-action noise `move_is_withheld_until_connected` forbids.
            if w.get_resource::<lodestone_ecs::Egress>()
                .is_some_and(|egress| egress.in_world)
            {
                actions.push(ClientAction::EndClientTick);
            }
            // Issue #157's outbound hook: a plugin's chance to inspect, replace
            // or suppress what another plugin queued, before any of it reaches
            // the socket. Inside the guard we already hold — the filters receive
            // only `&ClientAction`, never the `World`, so this cannot re-enter
            // the lock (see `lodestone_ecs::egress`'s module doc).
            //
            // `get_resource`, so a client with no plugin installed pays one
            // resource lookup and nothing else; `apply` itself returns after a
            // single `is_empty` check when no filter is registered.
            if let Some(filters) = w.get_resource::<lodestone_ecs::EgressFilters>() {
                filters.apply(&mut actions);
            }
            actions
        });
        // Only the *main* hand drives the first-person arm and the self-avatar's
        // right arm. An off-hand swing animates the left arm, which neither
        // consumer draws — treating it as a main-hand swing would swing the wrong
        // limb, so it is ignored rather than approximated.
        if actions
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main }))
        {
            self.swing_hand();
        }
        if let Some(net) = &self.net {
            for action in actions {
                // Best-effort — a closed session just drops it.
                net.send_action(action);
            }
        }
    }

    /// Start the local player's arm-swing animation, like `LivingEntity.swing`.
    ///
    /// Idempotent within the first half of a running swing — [`EntityPose::start_swing`]
    /// swallows a restart before its half-way point, which is what turns
    /// `interact.rs`'s once-per-tick swing during a held mine into a continuous
    /// arc instead of a stutter.
    ///
    /// # One-tick offset from vanilla, and why it is left alone
    ///
    /// Vanilla calls `swing()` from `Minecraft.handleKeybinds`, which runs
    /// *before* `updateSwingTime` in the same tick, so `swingTime` reaches `0` on
    /// the tick the click happened. Here [`Self::step`] ticks `body_pose` before
    /// draining the action queue, so the clock starts on the **next** tick — a
    /// 50 ms delay on the animation beginning, invisible at any frame rate, and
    /// worth less than reordering a tick loop whose wire ordering is load-bearing.
    ///
    /// The duration is [`lodestone_entity::pose::swing_duration`] with **no**
    /// effect inputs: neither Haste nor Mining Fatigue has a modelled source in
    /// this engine (`lodestone_game::mining::BreakInputs` has the identical hole —
    /// see `tool_inputs_stay_at_bare_hand_defaults`), so this is vanilla's
    /// component default of 6 ticks. Closing that hole is a change of arguments
    /// here, not a change of clock.
    pub(crate) fn swing_hand(&mut self) {
        self.body_pose
            .start_swing(lodestone_entity::pose::swing_duration(
                lodestone_entity::pose::DEFAULT_SWING_DURATION,
                None,
                None,
            ));
    }

    /// How far through an arm swing the local player is **this frame**, in
    /// `0.0..=1.0` — vanilla's `Player.getAttackAnim(partialTick)`.
    ///
    /// This is the value `RenderState::set_hand_swing_source`'s closure returns and
    /// the value `third_person_body_state` puts on [`AnimInput::attack_anim`]; both
    /// consumers read this one accessor so they can never disagree about where in
    /// the swing the player is.
    ///
    /// The swing clock advances in [`Self::step`]'s 20 Hz loop and is only
    /// *interpolated* here, so calling this more often does not make the arm swing
    /// faster. Reading it per frame is the correct and intended use.
    #[must_use]
    pub fn hand_swing_progress(&self) -> f32 {
        self.body_pose.attack_anim_lerp(self.clock().interp_alpha)
    }

    /// Advance the simulation by real elapsed time, running fixed 20 Hz `GameTick`
    /// schedules against the world's collision. Rendering interpolates between
    /// ticks via [`Sim::interp_alpha`].
    ///
    /// # What the tick loop is, since Stage 2
    ///
    /// Each iteration of the fixed-timestep loop resolves this tick's collision
    /// geometry, runs one `GameTick` schedule (`TickSet::Input` →
    /// `Physics` → `Send`), then hands whatever the systems queued to the
    /// socket. Everything the schedule needs is a component or resource, so a
    /// plugin can insert a system anywhere in that order.
    ///
    /// **Movement intent is now recomputed per tick, not per frame.** It used to
    /// be computed once before the loop, so a frame long enough to run several
    /// catch-up ticks reused one decision for all of them — see
    /// `lodestone_controller::ecs::compute_movement_intent` for exactly what
    /// that changes (nothing at all at 20 fps or better; the difference is
    /// confined to stalls).
    pub fn step(&mut self, dt: f64) {
        self.apply_mouse();
        // Issues #202/#444: apply the hold-vs-toggle and sprint-window options
        // to the live `InputState` before any `GameTick` schedule this call
        // runs reads it. One push per `step` call is enough — the option
        // cannot change mid-frame, and every catch-up tick inside this call
        // shares it.
        let (toggle_sneak, toggle_sprint, toggle_attack, toggle_use) = (
            self.toggle_sneak,
            self.toggle_sprint,
            self.toggle_attack,
            self.toggle_use,
        );
        let sprint_window_ticks = self.sprint_window_ticks;
        self.input_mut(|i| {
            i.set_toggle_modes(toggle_sneak, toggle_sprint, toggle_attack, toggle_use);
            i.set_sprint_window_ticks(sprint_window_ticks);
        });
        // The **one** accumulator, on the **one** catch-up policy
        // (`lodestone_ecs::MAX_CATCH_UP_SECS` — ten ticks, vanilla's own; see that
        // constant for why the shell's old inner `0.25 s` clamp lost).
        self.clock_mut(|clock| clock.begin_frame(dt));

        // The derived egress gate. Refreshed once per frame because both of its
        // inputs are frame-stable: `poll_net` is the only thing that changes the
        // phase and it runs after the loop.
        let egress = Egress {
            in_world: self.session_phase() == SessionPhase::Connected,
            live: self.is_live(),
        };
        self.write(|w| w.insert_resource(egress));

        // `Update` before the tick loop, not after it. `FrameSet::Interpolate`'s
        // `advance_interp_clocks` has to run first, because the tick systems
        // (`tick_item_physics`, `tick_walk_animation`) measure off the *drawn*
        // pose and would otherwise measure last frame's. That ordering was
        // internal to `EntityInterpolator::update_with_view` before §4.1(c) and is
        // now the frame's own.
        //
        // The one behaviour change this carries: `FrameSet::Terrain`'s
        // `heal_dirty_columns` now runs *before* `poll_net`, so a column that
        // arrives this frame has its neighbours healed on the next one. It is a
        // coalescing drain feeding an async worker pool on a per-frame budget, so a
        // single frame of latency is inside the noise it already tolerates —
        // but it is a change, not a no-op.
        let frame_dt = dt as f32;
        self.write(|w| {
            w.insert_resource(crate::entities::FrameDelta(frame_dt));
            w.run_schedule(Update);
        });

        loop {
            if !self.clock_mut(FrameClock::take_tick) {
                break;
            }
            let collision = self.tick_collision();
            let item_collision = self.item_collision();
            let nearby = self.tick_nearby_entities();
            // The walk bob's amplitude reads the state vanilla's `updateBob` sees,
            // which is the state **before** this tick's movement: `aiStep` calls
            // `updateBob()` and only then `super.aiStep()`, so `getDeltaMovement()`
            // is still last tick's post-friction velocity there. Captured here,
            // before the `GameTick` write guard, for that reason and not merely
            // for lock hygiene.
            let (pre_position, pre_speed, pre_on_ground, pre_swimming) = {
                let p = self.player();
                (
                    p.position,
                    (p.velocity.x * p.velocity.x + p.velocity.z * p.velocity.z).sqrt() as f32,
                    p.on_ground,
                    p.pose == lodestone_physics::Pose::Swimming,
                )
            };
            let pre_dead = self.is_dead();
            // Issue #201: vanilla's Auto-Jump option, pushed at the one place the
            // real detector can read it. Inside the tick loop rather than before
            // it for no reason other than symmetry with the three resources
            // below — the value is frame-stable either way.
            let auto_jump = lodestone_ecs::player::AutoJump(self.auto_jump);
            // Issue #206: the equipment half of `LivingEntity.canGlide`.
            let glider = lodestone_ecs::player::GliderEquipped(self.glider_equipped());
            self.write(|w| {
                w.insert_resource(collision);
                w.insert_resource(item_collision);
                w.insert_resource(nearby);
                w.insert_resource(auto_jump);
                w.insert_resource(glider);
                w.run_schedule(GameTick);
            });
            // Drive the local player's own walk/head-look clock off the
            // post-physics position, exactly like a tracked network entity's
            // `EntityPose::tick` — see `Self::body_pose`'s doc for why this
            // is unconditional rather than gated on `third_person`. Read
            // *after* the `GameTick` write guard above is dropped: `Self::player`
            // takes its own short read guard, and holding one across another
            // accessor is exactly what this crate's locking rules forbid.
            let p = self.player();
            self.body_pose
                .tick(p.position.x, p.position.z, p.yaw, p.yaw, p.pitch);
            // The camera's eye chases the entity's, half the gap per tick, so a
            // pose change eases instead of snapping. Same read guard as above.
            self.eye_height_smoother.tick(p.eye_height);
            // The bob's *phase* is the distance the feet actually travelled, which
            // is why this is a post-tick subtraction rather than a velocity:
            // `LocalPlayer.move` adds `length(getX() - prevX, getZ() - prevZ) * 0.6`
            // **after** `super.move` has already clipped the delta against
            // collision, so walking into a wall does not advance the stride.
            let moved = ((p.position.x - pre_position.x) as f32)
                .hypot((p.position.z - pre_position.z) as f32);
            self.view_bob.tick(
                moved,
                pre_speed,
                pre_on_ground,
                pre_dead,
                pre_swimming,
            );
            // The local player's own footstep, client-predicted. Here rather than
            // in a `GameTick` system for the same reason the bob's phase is: the
            // input is the movement *achieved* after collision, which only exists
            // as the difference across the schedule run above.
            self.tick_footstep(pre_position, &p);
            // **Auto-jump used to live here, and that was issue #201's defect.**
            // `lodestone_physics::update_auto_jump` is a complete port of
            // `LocalPlayer.updateAutoJump` — swept look-ahead probe, headroom
            // raycast, the `-0.15` facing-vs-moving dot product and all — and it
            // runs inside `tick_air` every tick. This file held a *second*,
            // deliberately simplified probe in front of it, gated on
            // `self.auto_jump`; the real one was gated on
            // `PlayerState::auto_jump_enabled`, which nothing outside tests ever
            // set. So the option correctly suppressed the simplification and the
            // real detector jumped anyway, and auto-jump could not be turned
            // off. The option now reaches the real detector through
            // `lodestone_ecs::player::AutoJump` (pushed above, before the
            // schedule) and the simplification is gone — one implementation, one
            // gate. Do not reintroduce a probe here.
            // Vanilla emits a movement packet every tick (20 Hz); mirror that so
            // the server sees our authoritative position/rotation and never has
            // to correct us. `TickSet::Send` produced it; this is where it and
            // everything else the tick queued reach the socket, in order.
            //
            // Since Stage 5 that includes the sprint edge and the hold-to-mine
            // loop, which used to be sent *after* this drain by a hand-written
            // `drive_interaction()` below. Wire order is unchanged: they are now
            // `TickSet::Send` systems ordered after `send_player_input`, so their
            // actions sit behind the movement packet in the same single queue.
            self.drain_action_queue();
            // The tick was counted and withdrawn by `FrameClock::take_tick` at the
            // top of this loop, so there is nothing to book-keep here any more.
            self.tick_particles();
            // Chest lids (issue #23), on the same fixed 20 Hz as everything else
            // here: `ChestLidController.tickLid()` ramps by ±0.1 per tick, so a
            // lid takes exactly 10 ticks to swing. Advancing it per *frame*
            // instead would open a chest in a third of a second at 60 fps and
            // make the animation speed a function of the frame rate.
            self.chest_lids.tick();
            // Bell shakes, on the same fixed 20 Hz and for the same reason: the
            // shake angle is a `sin` of vanilla's raw tick counter over a 50-tick
            // window, so advancing it per frame would make the swing's speed a
            // function of the frame rate.
            self.bell_shakes.tick();
            // Enchanting-table books, on the same fixed 20 Hz. Three of vanilla's
            // terms here are per-tick rates (`open` ±0.1, `tRot` +0.02, and the
            // 90% smoothing on `flipA`), so a per-frame advance would make the
            // book open three times faster at 60 fps — the `chest_lids` trap
            // exactly, but with three victims instead of one.
            //
            // Unlike the two above this needs the *world* and the player, because
            // nothing on the wire starts it: the trigger is the player standing
            // within three blocks of a table. The position gather is deliberately
            // radius-limited rather than `VIEW_DISTANCE`-limited, since a table
            // nobody is near can only ever be shut — see
            // `block_entities::enchanting_table_positions`.
            if let Some(net) = self.net.as_ref() {
                let handle = net.shared_handle();
                let player = {
                    let p = self.player();
                    glam::DVec3::new(p.position.x, p.position.y, p.position.z)
                };
                // A little slack over the 3.0 trigger radius so a table becomes
                // trackable a tick before it can wake, rather than on the same
                // tick the distance test first passes.
                let tables = crate::block_entities::enchanting_table_positions(
                    &handle, player, 8.0,
                );
                self.enchanting_table_books.tick(&tables, player);
                // Moving pistons, on the same fixed 20 Hz. This one *must* be a tick
                // and not a frame: vanilla's ramp is `progress += 0.5` per tick and
                // the whole push is two ticks, so a per-frame advance at 60 fps would
                // finish it in a single frame and the animation would not exist at
                // all. The gather is unbounded by view distance on purpose — see
                // `block_entities::moving_piston_seeds`.
                let pistons = crate::block_entities::moving_piston_seeds(&handle);
                self.moving_pistons.tick(&pistons);
            }
            // The HUD status effects and the title/action-bar overlays used to be
            // aged by three hand-written `tick(1)` calls right here. They are now
            // `lodestone_ecs::session::tick_hud_overlays` in `TickSet::Animate`,
            // which the `run_schedule(GameTick)` above already ran — same fixed
            // 20 Hz, but a plugin can now order against it and the components are
            // the only copy.
            // The live block interactions — the sprint edge and the held dig —
            // used to be driven from here by `drive_interaction()`. They are
            // `crate::interact`'s `send_sprint_command` / `drive_mining` systems in
            // `TickSet::Send` since Stage 5, which the `run_schedule(GameTick)`
            // above already ran; the `Egress` resource inserted before this loop
            // carries the `phase == Connected && is_live()` gate that used to be
            // written here. See `docs/sim-dissolution.md` for why the blocker
            // Stage 2 recorded (`Sim.target` / `version_data` / the live block
            // store) was not the real one.
        }
        // Publish the sub-tick residual. One number now: the camera's between-tick
        // ease and `extract_entity_draws`'s walk-cycle partial tick both read it,
        // where they used to read two accumulators' residuals.
        self.clock_mut(FrameClock::end_frame);

        self.poll_net();
        // Fold this frame's server report into the render-side tracks, then extract.
        // Still after the tick loop and after ingest, which is the order the ~25
        // interpolation tests are written against — see `fold_snapshots`' docs for
        // why it is not a `NetIngest` system even now that it could reach the
        // components directly.
        self.fold_entities();
        self.write(|w| w.run_schedule(Extract));
        self.refresh_stats();
    }

    /// Recompute the targeted block by casting the view ray from the (already
    /// interpolated) camera. Call once per frame before rendering the outline.
    ///
    /// The pick ray does **not** consult `is_solid`. `is_solid` is the *collision*
    /// predicate (also fed to the physics engine), and vanilla deliberately gives
    /// cross-plants (`short_grass`, ferns, flowers, kelp) an empty collision shape —
    /// you walk through grass — while picking them still works, because vanilla's
    /// `clip`/`clipWithInteractionOverride` walks a *separate* outline/interaction
    /// shape (`BlockBehaviour.getShape` / `getInteractionShape`), not the collision
    /// shape.
    ///
    /// The whole question therefore lives in one place,
    /// [`LiveCollision::pick_boxes`] / [`WorldCollision::pick_boxes`] — read its
    /// docs, which record why an earlier inlined `!is_water(...)` here made **kelp
    /// and every waterlogged block unbreakable**. Deliberately a single call and not
    /// an `||` chain: the geometry the collision tests exercise has to be the exact
    /// geometry the ray uses, or the gate proves nothing about the pick.
    ///
    /// # Issue #375: boxes, not a boolean
    ///
    /// This used to pass `is_pickable` — a per-*cell* occupancy predicate — so
    /// every pickable block was a unit cube to the hit test while the selection
    /// box was already drawn from the real outline census. Leaf litter therefore
    /// stayed targetable with the crosshair well above it. The closure now emits
    /// the cell's real outline boxes and [`raycast`] clips against them, which is
    /// vanilla's `ClipContext.Block.OUTLINE`.
    pub fn update_target(&mut self, aspect: f32) {
        let cam = self.camera(aspect);
        let origin = [
            f64::from(cam.position.x),
            f64::from(cam.position.y),
            f64::from(cam.position.z),
        ];
        let fwd = cam.forward();
        let dir = [f64::from(fwd.x), f64::from(fwd.y), f64::from(fwd.z)];
        // Live: raycast the server's terrain (client-owned world), not the demo
        // world, or dig/place would target phantom offline blocks. The 3×3
        // column snapshot spans ±16 blocks — far more than REACH (4.5) — so a
        // face at the edge of reach is always covered. A `None` snapshot means
        // the player's own column has not streamed in; nothing is targetable.
        let hit = if self.is_live() {
            self.live_collision().and_then(|view| {
                raycast(origin, dir, REACH, |x, y, z, out| {
                    view.pick_boxes(x, y, z, out);
                })
            })
        } else {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            raycast(origin, dir, REACH, |x, y, z, out| {
                view.pick_boxes(x, y, z, out);
            })
        };
        self.set_target(hit);
        // Shared with the demo world too (harmlessly a no-op there — the demo
        // ECS holds no networked entities), so `crack_target`/the outline and
        // `EntityRayTarget` are always derived from the exact same ray.
        self.update_entity_target(origin, dir, hit);
    }

    /// Recompute [`EntityRayTarget`] from the same ray [`Self::update_target`]
    /// just cast against blocks — vanilla's entity half of
    /// `GameRenderer.pick`, which [`Self::begin_attack`] reads to decide
    /// between `case ENTITY` and `case BLOCK`.
    ///
    /// The search radius is [`ENTITY_REACH`] (`3.0`, vanilla's
    /// `DEFAULT_ENTITY_INTERACTION_RANGE`, `Player.java:134`), shortened to
    /// `block_hit`'s own entry distance when a block sits closer than that —
    /// matching vanilla's `blockDistance` clamp, so a wall between the eye and
    /// an entity is never picked through.
    ///
    /// That distance is [`RayHit::distance`], the entry point of the **outline
    /// box** the ray actually struck. It used to be re-derived here by clipping
    /// a unit cube around `block_hit.block`, which was wrong in both directions
    /// on a partial block — too *short* whenever the real box sits deeper in the
    /// cell than its near face, which hid an entity standing in front of a
    /// fence. The ray now reports its own entry distance, so there is nothing
    /// left to approximate (issue #375).
    ///
    /// Candidates come from the same `(Position, EntityKind)` query
    /// [`Self::tick_nearby_entities`] uses for pushers, resolved to a hitbox
    /// through the identical [`VersionData::entity_facts`] seam — an unknown
    /// type is excluded, never approximated. The local player is never a
    /// candidate: `apply_entity_spawn`/`apply_local_player_login`
    /// (`lodestone_ecs::ingest`) never give the local player's own `Entity` a
    /// `Position`/`EntityKind` component, so the query structurally cannot
    /// return it — the same property vanilla's `clip()` gets from excluding
    /// `this` explicitly.
    pub(crate) fn update_entity_target(&mut self, origin: [f64; 3], dir: [f64; 3], block_hit: Option<RayHit>) {
        let search_limit = block_hit.map_or(ENTITY_REACH, |hit| hit.distance.min(ENTITY_REACH));

        let target = self.write(|w| {
            let mut state = w.query::<(&Position, &EntityKind, &MinecraftEntityId)>();
            let version = w.resource::<VersionData>();
            state
                .iter(w)
                .filter_map(|(pos, kind, id)| {
                    let feet = Vec3d::new(pos.0.x, pos.0.y, pos.0.z);
                    // Cheap pre-filter before the exact ray-vs-box test: an
                    // entity whose *feet* are already further than the search
                    // radius plus a generous per-axis margin for its own
                    // hitbox cannot possibly be hit. Same shape as
                    // `tick_nearby_entities`'s box, sized off `search_limit`
                    // instead of the fixed push radius.
                    let margin = search_limit + 4.0;
                    if (feet.x - origin[0]).abs() > margin
                        || (feet.y - origin[1]).abs() > margin
                        || (feet.z - origin[2]).abs() > margin
                    {
                        return None;
                    }
                    let facts = version.entity_facts(&kind.0)?;
                    let dims =
                        EntityDimensions::new(facts.dimensions.width, facts.dimensions.height, 0.6);
                    let aabb = dims.bounding_box(feet);
                    let t = ray_aabb(
                        origin,
                        dir,
                        search_limit,
                        [aabb.min_x, aabb.min_y, aabb.min_z],
                        [aabb.max_x, aabb.max_y, aabb.max_z],
                    )?;
                    Some((id.0, t))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(id, _)| id)
        });
        self.write(|w| w.resource_mut::<EntityRayTarget>().0 = target);
    }

    /// The number of fixed simulation ticks (20/s) elapsed. Drives animated
    /// block sprites, whose vanilla frame timing is measured in game ticks; the
    /// renderer samples each animation at this tick each frame.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.clock().ticks
    }

    fn refresh_stats(&mut self) {
        let player = self.player();
        self.stats.position = [player.position.x, player.position.y, player.position.z];
        self.stats.yaw = player.yaw;
        self.stats.pitch = player.pitch;
        let store = self.chunk_world();
        self.stats.chunk_count = store.len();
        self.stats.mesh_drops = self.terrain(|t| t.drops);
        self.stats.frames_per_tick = self.frames_per_tick();
        self.stats.target = self.target().map(|h| h.block);
        // The three fields whose cost is O(resident world) or a syscall, throttled
        // to one frame in [`WORLD_STATS_PERIOD`]. See that constant for the
        // measured numbers and why the whole overlay is not simply gated on `F3`.
        if refreshes_world_stats(self.clock().frames) {
            self.stats.live_columns = self.net.as_ref().map_or(0, |n| n.loaded_chunks().len());
            self.stats.world_bytes = store.read().heap_bytes();
            self.stats.rss_bytes = process_rss_bytes();
            // Issue #197's light readout. Inside the throttle deliberately: it
            // is a section fetch under the client world's own lock, which is the
            // same class of cost as the three above even though it touches one
            // section rather than all of them. The sky policy comes from
            // `shared_sky_default`, never from `sky_at` directly — see
            // `net::entity_light_at`'s doc for the two bugs that produced.
            self.stats.light = self.net.as_ref().and_then(|net| {
                let packed = crate::net::entity_light_at(
                    &net.shared_handle(),
                    player.position.x.floor() as i32,
                    player.position.y.floor() as i32,
                    player.position.z.floor() as i32,
                    net.shared_sky_default().get(),
                )?;
                Some((packed >> 4, packed & 0x0F))
            });
        }
        // `clone_from` reuses the existing `String`'s buffer, and the comparison
        // skips even that on the overwhelmingly common frame where the status line
        // has not changed. This used to be an unconditional `self.status.clone()`
        // — one heap allocation and free per frame, for a field that changes a
        // handful of times per session.
        if self.stats.status != self.status {
            self.stats.status.clone_from(&self.status);
        }
    }
}

/// How many frames apart [`Sim::refresh_stats`] recomputes the debug fields whose
/// cost scales with the resident world.
///
/// Three fields were recomputed **every frame** for an overlay that is usually
/// not on screen:
///
/// * `world_bytes` — `World::heap_bytes` walks every resident chunk, every
///   section and every paletted container, under the world **read lock**;
/// * `live_columns` — `NetClient::loaded_chunks` allocates a `Vec<ChunkPos>` of
///   every loaded column and the caller immediately takes `.len()`;
/// * `rss_bytes` — a `task_info` syscall.
///
/// Measured in instructions retired at render distance 8 (361 resident columns),
/// by `crates/lodestone-shell/tests/client_chunk_cycles.rs`: `heap_bytes`
/// **494,570 instructions per frame** and the position `Vec` **116,242**. Both
/// scale linearly in resident columns, so both get worse at render distance 16
/// and 32 — the direction the render plan is trying to move.
///
/// **30 frames is ~0.5 s at 60 fps.** That is below the rate a human reads a
/// changing debug figure, and it divides both terms by 30.
///
/// Why a throttle and not a `show_debug` gate: `DebugStats` has a second consumer,
/// `DebugStats::one_line`, which `app/redraw.rs` and `app/runners.rs` print in
/// headless and logged runs where no overlay is visible. Gating on overlay
/// visibility would silently zero those logs — the shape of defect this repo
/// calls a signal that looks like evidence and isn't. A throttle keeps every
/// consumer correct and merely slightly stale. See `docs/client-chunk-cycles.md`.
const WORLD_STATS_PERIOD: u64 = 30;

/// Whether the frame numbered `frames` (from [`FrameClock::frames`]) recomputes
/// the O(resident-world) debug fields.
///
/// `FrameClock::begin_frame` increments before the step body, so `frames` is `1`
/// on the first frame; subtracting one makes that first frame a refresh frame, so
/// the overlay is populated immediately instead of reading zeros for half a
/// second. Exactly one frame in every [`WORLD_STATS_PERIOD`] returns `true` —
/// pinned by `world_stats_refresh_is_exactly_one_frame_per_period`, which computes
/// both the correct count (1) and the unthrottled one (`WORLD_STATS_PERIOD`).
const fn refreshes_world_stats(frames: u64) -> bool {
    frames.saturating_sub(1) % WORLD_STATS_PERIOD == 0
}

#[cfg(test)]
mod stats_throttle_tests {
    use super::{WORLD_STATS_PERIOD, refreshes_world_stats};

    /// The throttle's whole claim, as a count rather than a sign: over any window
    /// of [`WORLD_STATS_PERIOD`] consecutive frames, exactly **one** recomputes.
    ///
    /// Both hypotheses are computed from the constant rather than restated: the
    /// correct one is `1` per window, and the pre-fix (unthrottled) one is
    /// `WORLD_STATS_PERIOD` per window. Asserting only "fewer than before" would
    /// be the *magnitude* species of vacuous test — a throttle that fired on 29
    /// frames in 30 would pass it.
    #[test]
    fn world_stats_refresh_is_exactly_one_frame_per_period() {
        const WINDOWS: u64 = 7;
        let unthrottled_hypothesis = WORLD_STATS_PERIOD;
        assert_ne!(
            1, unthrottled_hypothesis,
            "with a period of 1 the throttle is a no-op and this test cannot distinguish the \
             two hypotheses"
        );
        for window in 0..WINDOWS {
            let first = window * WORLD_STATS_PERIOD + 1;
            let hits = (first..first + WORLD_STATS_PERIOD)
                .filter(|&f| refreshes_world_stats(f))
                .count() as u64;
            assert_eq!(
                hits, 1,
                "frames {first}..{} recomputed the world stats {hits} times; the correct \
                 hypothesis is 1 and the unthrottled hypothesis is {unthrottled_hypothesis}",
                first + WORLD_STATS_PERIOD
            );
        }
    }

    /// The first frame must refresh, or the overlay and the `one_line` log read
    /// zeros for the first half-second of every session — which is exactly the
    /// "flat zero that looks like evidence" failure the RSS field already had once.
    #[test]
    fn the_first_frame_refreshes() {
        assert!(
            refreshes_world_stats(1),
            "FrameClock::frames is 1 on the first frame and it must be a refresh frame"
        );
        // And frame 0, in case a caller reaches `refresh_stats` before any
        // `begin_frame` (the hermetic-test path).
        assert!(refreshes_world_stats(0), "frame 0 must also refresh");
    }
}
