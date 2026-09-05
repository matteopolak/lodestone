//! `Sim`'s per-tick network-apply cluster: `poll_net` (the ~66 `NetUpdate::`
//! arms that turn a decoded server event into shell state) and `fold_entities`
//! (the entity-snapshot fold that runs immediately after it in
//! [`Sim::step`](super::Sim::step)) — seam 4 of the sim.rs decomposition
//! sequence (seam 1 was the test module, `sim/tests.rs`; seam 2 was placement
//! prediction, `sim/placement.rs`; seam 3 was the interaction/combat cluster,
//! `sim/actions.rs`). This is the file with the most future contention:
//! adding a `NetUpdate` variant means an arm here, same as adding a
//! `ClientEvent` means a `net.rs`/`ingest`/`session` arm elsewhere.
//!
//! `use super::*;` for the same reason `sim/actions.rs` uses it: it pulls in
//! `Sim`'s private fields' types and `sim.rs`'s other private helpers with no
//! need to enumerate them, since `sim::net_apply` is a descendant of `sim` and
//! already has the same visibility into `Sim`'s private fields that
//! `sim::actions` and `sim::tests` have.
//!
//! Both methods are `pub(crate)`, not private: `Sim::step` calls
//! `self.poll_net()`/`self.fold_entities()` from `sim.rs` itself, which is
//! this module's *parent* — a private item in a child module is not visible
//! to the parent (privacy only cascades downward, the same rule
//! `sim/actions.rs`'s doc explains for `begin_attack_live` and friends) — and
//! `sim/tests.rs`'s many `sim.poll_net()` calls cross the same sibling
//! boundary `sim/actions.rs` hit for its three `pub(crate)` methods.

use super::*;
// Not reachable through `super::*`: the disconnect and failure arms build a
// `SessionEnd` whose reason is a styled `Text` rather than a formatted string.

impl Sim {
    /// Fold this frame's entity state into the render-side component set, so
    /// [`entity_draws`](Self::entity_draws) yields smooth per-frame transforms.
    /// No live connection means no entities.
    ///
    /// # What §4.1(c) changed here
    ///
    /// This used to be `update_entities`, which drove
    /// `EntityInterpolator::update_with_view` — a whole second `World` running its
    /// own `Update`, its own `GameTick` loop off its own accumulator, and its own
    /// `Extract`. Those three schedule runs are now the frame's own, so all this
    /// does is the fold. The item collision it used to pass by argument is the
    /// [`crate::entities::ItemCollision`] resource the tick loop inserts.
    ///
    /// # `EntitySnapshot` deletion
    ///
    /// [`crate::entities::fold_entities`] reads the ingest components directly
    /// inside its own write guard — there is no separate snapshot read to
    /// resolve ahead of it any more, so this is a single `self.write` call.
    pub(crate) fn fold_entities(&mut self) {
        let local_name = self.local_uuid().and_then(|uuid| {
            self.tab_list()
                .get(&uuid)
                .map(|entry| entry.profile.name.clone())
        });
        // The language table is cloned out of the `Sim` first: `write` takes
        // `&mut self`, so a borrowed translator cannot survive into the
        // closure. This is the last point that holds a table at all, and a
        // nametag built past it can no longer be resolved.
        let language = self.language.clone();
        self.write(|world| {
            let translate: Box<dyn Fn(&str) -> Option<String>> = match &language {
                Some(lang) => Box::new(lang.translator()),
                None => Box::new(|_: &str| None),
            };
            crate::entities::fold_entities_for_local(
                world,
                local_name.as_deref(),
                translate.as_ref(),
            );
        });
    }

    pub(crate) fn poll_net(&mut self) {
        // Collect owned updates first so the immutable borrow of `self.net`
        // ends before the loop — the sound arms need a `self.audio_mut` guard
        // and (for entity sounds) a fresh read of `self.net` for positions,
        // neither of which can coexist with a borrow held across the loop.
        // Adopt the client's chunk store the first frame a handle exists — this
        // is where the process comes to have exactly one `lodestone_world::World`
        // (`docs/chunk-world-resource.md`). Idempotent and a pointer compare
        // thereafter.
        self.adopt_live_world();
        // The connected dimension (and therefore the absent-sky policy) can change
        // mid-session on a portal trip, so the mesh policy is refreshed every poll
        // rather than only at attach.
        self.refresh_mesh_policy();
        let updates = match &self.net {
            Some(net) => net.poll(),
            None => return,
        };
        for update in updates {
            match update {
                NetUpdate::Connecting => {
                    self.status = "connecting…".into();
                    self.set_phase(SessionPhase::Connecting);
                    self.set_connect_phase(crate::menu::loading::ConnectPhase::Connecting);
                }
                NetUpdate::ConnectPhase(phase) => {
                    // Purely the loading screen's label. Kept out of
                    // `SessionPhase` on purpose — that enum drives the menu
                    // state machine, and adding display-only steps to it would
                    // make every `match` on it care about them.
                    self.set_connect_phase(phase);
                }
                NetUpdate::LoggedIn { entity_id } => {
                    // A `LoggedIn` while already connected is a **second login
                    // on one connection** — what a Velocity/BungeeCord proxy
                    // does when it swaps the backend behind an unbroken socket
                    // (`START_CONFIGURATION`, a configuration round, then a
                    // second `LOGIN`), with no reconnect and no `Respawned`.
                    // Vanilla's `handleLogin` assigns a whole new `ClientLevel`
                    // for exactly this, dropping every entity and every chunk of
                    // what came before; the client's decoded store is cleared on
                    // the net thread (`lodestone_client`'s own `Login` arm, the
                    // only place the ordering is safe) and this is the
                    // render-side half of the same drop. Without it the previous
                    // backend's meshed columns stay uploaded over terrain the
                    // store no longer holds — geometry with nothing behind it,
                    // which is the "standing on invisible blocks" shape.
                    //
                    // The phase is the test because it is the only local fact
                    // that separates the two cases: a fresh join arrives here
                    // while still `Connecting`, and nothing but a second login
                    // can arrive while `Connected`.
                    if self.session_phase() == SessionPhase::Connected {
                        tracing::info!(
                            target: "transfer",
                            entity_id,
                            "xfer: second LOGIN while connected -- dropping the \
                             previous backend's meshes and entity tracks"
                        );
                        self.reset_for_server_transfer();
                        self.reset_for_dimension_change();
                    }
                    // The id is *not* recorded here. `ClientEvent::Login` folds it
                    // into the `ServerEntityId` component (and into
                    // `EntityIndex`) on the net thread, in the same `World` this
                    // `Sim` reads — a second write here would be the duplicate the
                    // vitals collapse deleted. It stays in the status line because
                    // that is a human-readable string, not state.
                    self.status = format!("connected (entity {entity_id})");
                    self.set_phase(SessionPhase::Connected);
                    // Login done, so the screen is now naming the
                    // terrain stream rather than the connect handshake. On a
                    // brand-new singleplayer world this is also when generation
                    // happens — columns are generated lazily as they stream.
                    self.set_connect_phase(crate::menu::loading::ConnectPhase::LoadingTerrain);
                    // The baseline for `apply_respawn`'s comparison. Safe to read
                    // the shared handle here — unlike in the `Respawned` arm —
                    // because the fold runs on the net thread *before* the event is
                    // queued for us (`Driver::emit` does `read_model.apply(&event)`
                    // and only then sends), so `Login`'s dimension has landed by
                    // now. Without this the first portal trip of a session would
                    // compare against `None` and skip its reset entirely.
                    self.record_login_dimension();
                }
                NetUpdate::Chunk { x, z } => {
                    // §12.24 dirty-region signal: no block data travels on the
                    // event — the client applies decoded chunks to its own
                    // `World`, which we read via `NetClient::sections_and_light_at`
                    // (+ `world_dimensions` for geometry). `mark_column_dirty`
                    // meshes live columns through the vanilla classifier.
                    self.on_column_arrived(x, z);
                }
                NetUpdate::ChunkUnloaded { x, z } => {
                    // That fix's missing half. The column is already out of the
                    // store (the adapter unloads before it emits), so this drops
                    // only what the *renderer* still holds for it.
                    self.on_column_unloaded(x, z);
                }
                NetUpdate::SectionBlocks { x, y, z, blocks } => {
                    // A server-authoritative edit inside one loaded section.
                    // Re-mesh at *section* granularity, not the whole column:
                    // the same signal carries every redstone tick, and a column
                    // re-mesh is ~24 sections × a 27-section snapshot each.
                    // `remesh_around` also handles the boundary case, so a break
                    // at x=15 dirties the neighbouring column's face too.
                    self.reconcile_predictions(x, y, z, &blocks);
                    self.remesh_changed_blocks(x, y, z, &blocks);
                }
                NetUpdate::BlockChangedAck { sequence } => {
                    // The adapter has already installed the authoritative block
                    // writes before this acknowledgement reaches the shell. The
                    // sequence is therefore a ledger-retirement signal, not a
                    // second world write: `Placement` clears every prediction the
                    // server has processed through this value.
                    self.settle_placement_predictions(sequence);
                }
                NetUpdate::PlayerRotationSet {
                    y_rot,
                    relative_y,
                    x_rot,
                    relative_x,
                } => {
                    // `PhysicsState` is the one pose that drives the rendered
                    // camera, interaction ray, audio listener, and the next
                    // outbound move. Keep the correction here rather than in a
                    // passive session component: all four consumers must observe
                    // the same absolute-or-relative result this frame.
                    self.player_mut(|player| {
                        player.yaw = if relative_y {
                            player.yaw + y_rot
                        } else {
                            y_rot
                        };
                        player.pitch = if relative_x {
                            player.pitch + x_rot
                        } else {
                            x_rot
                        };
                    });
                }
                NetUpdate::BlockEvent { pos, b0, b1 } => {
                    // Chest lids. `ChestBlockEntity.triggerEvent`
                    // takes `b0 == 1` and `b1 > 0` as "somebody is looking in
                    // this chest"; `ChestLids` owns both that rule and the
                    // per-tick ramp, so this arm forwards the raw bytes rather
                    // than interpreting them here. Every other `b0` belongs to
                    // some other block type (a note block's pitch, a piston's
                    // direction) and is dropped by `apply_block_event`.
                    self.chest_lids.apply_block_event(pos, b0, b1);
                    // Bells share the same `b0 == 1` — see
                    // `BellShakes::apply_block_event`. Both trackers are offered
                    // the event because the packet cannot tell them apart; the
                    // per-type gather is what reads only its own positions back
                    // out, so a rung bell never opens a chest lid and vice versa.
                    self.bell_shakes.apply_block_event(pos, b0, b1);
                    // Spawners/trial spawners share the same `b0 == 1` for
                    // `onEventTriggered`'s spawn-delay reset — see
                    // `SpawnerSpins::apply_block_event`. A third tracker
                    // offered the same event, same reason as the two above:
                    // the packet cannot tell a spawner from a chest, only
                    // the gather at the position can.
                    self.spawner_spins.apply_block_event(pos, b0, b1);
                    // End gateway teleport cooldowns share the same `b0 ==
                    // 1` — see `GatewayCooldowns::apply_block_event`. A
                    // fourth tracker offered the same event, same reason as
                    // the three above.
                    self.gateway_cooldowns.apply_block_event(pos, b0, b1);
                }
                NetUpdate::Explosion {
                    pos: _,
                    radius: _,
                    affected_blocks: _,
                    knockback,
                } => {
                    // Only the local-player knockback lands here today. The
                    // block removals (pre-26.2 families only —
                    // `ClientEvent::Explosion::affected_blocks`'s own doc
                    // explains why 26.2 never populates it) and the cosmetic
                    // particle/sound burst are deliberately not wired: this
                    // fold's job is routing the event to the right place
                    // (`docs/plugin-api.md`'s three-router convention this
                    // event follows — world/block state, not per-entity or
                    // session), not the gameplay effect itself, which is a
                    // separate piece of work.
                    //
                    // An **additive** velocity delta, matching vanilla's own
                    // `Entity::addDeltaMovement` (`ClientPacketListener`'s
                    // `handleExplosion` calls exactly that, not an assignment)
                    // — a second explosion's push stacks onto whatever this
                    // one already imparted rather than overwriting it.
                    if let Some(kb) = knockback {
                        self.player_mut(|player| {
                            player.velocity =
                                player.velocity.add(Vec3d::new(kb.x, kb.y, kb.z));
                        });
                    }
                }
                NetUpdate::SignEditorOpened { pos, is_front_text } => {
                    // Read the sign's already-synced text now, while `pos` is
                    // known. `PendingSignEdit` is deliberately menu-agnostic
                    // (see its own doc) — `app::session::drive_ui_from_session`
                    // is what converts this into
                    // `crate::menu::sign_edit::SignEditOpen` and opens the
                    // screen, once per frame.
                    let text = self.sign_text_at(pos);
                    let side = if is_front_text { text.front } else { text.back };
                    // Plain text, not styled spans: opening the editor always
                    // shows (and re-committing always overwrites with) a
                    // plain literal per line, the same as vanilla's
                    // `SignEditScreen` reading `getMessage(idx, false).getString()`
                    // — editing discards formatting rather than round-tripping it.
                    let lines = std::array::from_fn(|i| {
                        side.lines[i].iter().map(|span| span.text.as_str()).collect::<String>()
                    });
                    self.pending_sign_edit = Some(PendingSignEdit {
                        pos,
                        is_front_text,
                        lines,
                    });
                }
                NetUpdate::BookOpened { main_hand } => {
                    // The inventory is already folded by the client's shared
                    // state. Keep only the requested hand here; `app::session`
                    // projects the current stack into its book UI on the next
                    // frame, so an updated book component is never duplicated
                    // in this transient signal.
                    self.pending_book_open = Some(main_hand);
                }
                NetUpdate::ItemPickup(event) => {
                    // That fix. Accumulated, not acted on here: the drain at the
                    // end of this function needs a `&mut World` guard and there is
                    // no reason to take one per collected item.
                    self.pickups.apply(&event);
                }
                NetUpdate::Teleport {
                    pos,
                    rotation,
                    flags,
                } => {
                    // Adopt the server's authoritative placement. The shell runs
                    // its own physics and streams an optimistic position every
                    // tick from the demo spawn; on a server whose spawn is far
                    // from the origin the server ignores that bogus claim and
                    // keeps us at the real spawn, streaming chunks there. Snap the
                    // camera onto it (resolving any relative components against the
                    // current pose) so it sits where the world actually is instead
                    // of stranded over the unmeshed demo platform. `prev_position`
                    // is moved with it so the frame interpolator does not smear the
                    // camera across the teleport.
                    let placed = self.player_mut(|player| {
                        let base = player.position;
                        player.position = Vec3d::new(
                            if flags.relative_x {
                                base.x + pos.x
                            } else {
                                pos.x
                            },
                            if flags.relative_y {
                                base.y + pos.y
                            } else {
                                pos.y
                            },
                            if flags.relative_z {
                                base.z + pos.z
                            } else {
                                pos.z
                            },
                        );
                        player.yaw = if flags.relative_yaw {
                            player.yaw + rotation.yaw
                        } else {
                            rotation.yaw
                        };
                        player.pitch = if flags.relative_pitch {
                            player.pitch + rotation.pitch
                        } else {
                            rotation.pitch
                        };
                        player.velocity = Vec3d::ZERO;
                        // A teleport is not a fall. Vanilla resets fall distance on
                        // every position snap, and this one handler covers server
                        // corrections, respawn and every teleport packet — so
                        // without it, a corrective teleport mid-fall leaves the
                        // accumulated distance behind to feed `maybeBackOffFromEdge`
                        // (and, later, fall damage) as though the fall continued.
                        player.reset_fall_distance();
                        (base, player.position)
                    });
                    let (was, placed) = placed;
                    self.set_prev_position(placed);
                    self.teleport_count += 1;
                    // The simulation is now level with the server, so the net
                    // thread can stop overriding the pose our outbound `Move`s
                    // claim. Published here — after the pose is actually
                    // adopted, never before — because that is precisely the
                    // edge the override exists to bridge. See
                    // `crate::net::with_authorised_pose`.
                    crate::net::note_teleport_applied();
                    // The `transfer` target's simulation-side hop, and the one
                    // that dates the window: the driver wrote
                    // `ACCEPT_TELEPORTATION` when the packet decoded, but the
                    // pose only becomes ours *here*, a channel hop and up to a
                    // frame later. Every `Move` the tick loop queued in between
                    // claims `was`, not `placed`. `moved` is what makes the two
                    // ends of that window comparable in the log — the v770
                    // adapter's own `xfer: move packet` line reports the same
                    // distance from the other side of the channel. See the
                    // `xfer` module in that crate for the whole chain.
                    tracing::debug!(
                        target: "transfer",
                        teleport_count = self.teleport_count,
                        from_x = was.x,
                        from_y = was.y,
                        from_z = was.z,
                        x = placed.x,
                        y = placed.y,
                        z = placed.z,
                        moved = {
                            let (dx, dy, dz) =
                                (placed.x - was.x, placed.y - was.y, placed.z - was.z);
                            (dx * dx + dy * dy + dz * dz).sqrt()
                        },
                        "xfer: teleport applied to the simulation"
                    );
                }
                NetUpdate::Chat {
                    text,
                    player,
                    sender,
                    verified,
                } => {
                    // A player hidden on the Social Interactions
                    // screen must not reach the feed. Only a signed v770 player
                    // message carries a sender, so the set is re-read from the
                    // file the toggle wrote (the same eager-persistence rule the
                    // toggle itself uses) rather than held in a stale copy — and
                    // `None` (system/disguised chat, and every legacy family's
                    // player chat, which has no sender on the wire) always shows,
                    // vanilla's Hide in Chat being signed-chat-only.
                    if player
                        && let Some(id) = sender
                        && !crate::menu::social::should_show_message(
                            &crate::config::HiddenPlayers::load(),
                            Some(id),
                        )
                    {
                        continue;
                    }
                    // The scrollback stores the server's own component and
                    // resolves at read (`ChatLog::recent_spans` takes the table),
                    // so a language pack that arrives later — a pushed resource
                    // pack — re-reads lines already in the log. Only the log line
                    // is resolved here, and only because it is written once.
                    //
                    // `to_plain_string`, not `to_legacy_string`: a terminal does not
                    // render `§` codes, so logging the legacy-flattened string prints
                    // mojibake for any coloured line — the code points survive, just
                    // uninterpreted, into the log file.
                    tracing::debug!(target: "chat", "{}", self.resolve_text(&text).to_plain_string());
                    // Stamped with the driver's own clock, which is why the log and
                    // the clock had to move to the ECS together (Stage 3 deferred
                    // both for exactly this reason). `local` is the session entity,
                    // so a `SessionChat` that somehow went missing drops the line
                    // rather than panicking mid-poll.
                    let now = self.clock().secs;
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut chat) = w.get_mut::<SessionChat>(local) {
                            if player {
                                // The one place a trust level is decided. It
                                // used to be the literal `NotSecure` for every
                                // player message, which is the shape this repo
                                // calls a correct consumer fed a constant by
                                // its producer: `MessageTrust` had three
                                // variants, a real signature check ran in the
                                // client driver, and the value it produced was
                                // discarded one layer up in `net.rs`'s router.
                                //
                                // Two variants, not three. `Secure` and
                                // `NotSecure` are the two states the wire and
                                // the driver between them can establish;
                                // `Modified` needs the signed content compared
                                // against the displayed content, which nothing
                                // here computes, so it is never produced rather
                                // than approximated. See `docs/secure-chat.md`.
                                let trust = if verified {
                                    lodestone_game::chat::MessageTrust::Secure
                                } else {
                                    lodestone_game::chat::MessageTrust::NotSecure
                                };
                                chat.0.push_player(text, trust, now);
                            } else {
                                chat.0.push_system(text, now);
                            }
                        }
                    });
                }
                NetUpdate::BlockDestroyed { pos, state } => {
                    // The live counterpart of the offline `break_block` emit.
                    // It is driven by the server rather than by our own click
                    // because the server is authoritative about *whether* the
                    // block broke and *what* it was — a predicted break that the
                    // server rejects would otherwise throw debris off a block
                    // still standing there.
                    //
                    // Shape is a full cube for the same reason as the offline
                    // path: vanilla derives the fragment grid from the block's
                    // outline shape, which the shell does not carry. Debris from
                    // a slab or a fence therefore fills the whole cell rather
                    // than hugging the model.
                    self.particles_mut(|p| {
                        p.destroy_block([pos.x, pos.y, pos.z], state, [1.0, 1.0, 1.0]);
                    });
                    // The *other* half of vanilla's `case 2001`, which this arm
                    // used to drop: `playLocalSound(pos, getBreakSound(), …)`.
                    // `Level.destroyBlock` fires the event with **no** excluded
                    // entity, so this is a genuinely
                    // server-sent sound, not a prediction — every client in range
                    // hears it, the breaker included. Note which breaks reach here:
                    // `Level.destroyBlock`'s callers (a torch losing support, fire
                    // spread, an explosion), *not* a player's own dig, which
                    // `ServerPlayerGameMode.destroyBlock` routes through
                    // `removeBlock` with no `levelEvent` at all — see the long note
                    // in `interact.rs` on the same asymmetry for the particles.
                    self.play_block_break_sound([pos.x, pos.y, pos.z], state);
                }
                NetUpdate::Particles {
                    kind,
                    long_distance,
                    always_show,
                    pos,
                    offset,
                    max_speed,
                    count,
                    options,
                } => {
                    // `ClientLevel.doAddParticle`'s render cutoff: a particle
                    // farther than 32 blocks (`1024.0` == `32.0` squared) from
                    // the viewer is dropped unless the packet set the
                    // override-limiter flag (`long_distance` here). Vanilla
                    // measures from the render camera; the player's feet
                    // position is close enough for a cutoff whose only
                    // visible effect is "does this puff bother rendering,"
                    // and it is what the rest of the shell's render-adjacent
                    // logic already keys off.
                    let feet = self.player().position;
                    let dx = pos.x - feet.x;
                    let dy = pos.y - feet.y;
                    let dz = pos.z - feet.z;
                    let within_cutoff = dx.mul_add(dx, dy.mul_add(dy, dz * dz)) <= 1024.0;
                    // Vanilla's structure, and the nesting is the point:
                    // `overrideLimiter` bypasses **both** the distance cutoff
                    // and the particle-level filter, in that one branch. Only
                    // the non-override path consults `options.particles`.
                    // Writing this as one `&&` would let a MINIMAL setting
                    // suppress the particles the server explicitly marked
                    // un-suppressible.
                    let level = self.particle_level;
                    self.particles_mut(|p| {
                        // Both refusals below are silent by nature — the packet
                        // arrives, nothing spawns, and no counter moves — so
                        // each says why. A particle the server sent and this
                        // client chose not to draw must be distinguishable from
                        // one that was never sent, and from one that spawned and
                        // failed to resolve a sprite (`Particles::extract` logs
                        // that third case on the same target).
                        let spawn = if long_distance {
                            true
                        } else if !within_cutoff {
                            tracing::debug!(
                                target: "particles",
                                kind = %kind,
                                distance = dx.mul_add(dx, dy.mul_add(dy, dz * dz)).sqrt(),
                                "dropped: past the 32-block cutoff and the packet did not \
                                 set override-limiter"
                            );
                            false
                        } else if p.particle_level_permits(level, always_show) {
                            true
                        } else {
                            tracing::debug!(
                                target: "particles",
                                kind = %kind,
                                ?level,
                                always_show,
                                "dropped: the Particles video option suppressed it"
                            );
                            false
                        };
                        if spawn {
                            p.spawn_particles(
                                &kind,
                                [pos.x, pos.y, pos.z],
                                [offset.x, offset.y, offset.z],
                                max_speed,
                                count,
                                options,
                            );
                        }
                    });
                }
                // No `Health`/`Experience` arms, and no `NetUpdate` variants for
                // them either: the net thread folds `ClientEvent::HealthChanged`
                // and `ExperienceChanged` straight into the `Vitals`/`Xp`
                // components on `self.local`, so [`Self::health`], [`Self::food`]
                // and [`Self::experience`] read what they always read and this
                // side has nothing left to do. Death is still a separate event
                // ([`NetUpdate::Death`]); health reaching zero is not itself a
                // session event and does not unload chunks.
                NetUpdate::Death { message } => {
                    // Death is a state the shell rides through, not the end of the
                    // session. `net::run` now builds the client with
                    // `RespawnPolicy::Manual`, so nothing respawns
                    // automatically here: this arm marks the player dead (which
                    // freezes movement in `step`) and records the message for the
                    // death screen (`app.rs`'s `drive_ui_from_session` notices
                    // `is_dead()` and shows it); the screen's Respawn button is
                    // what eventually calls `Self::respawn`. The new position
                    // rides in on the placement teleport that follows
                    // `NetUpdate::Respawned`, whose arm snaps `prev_position` too.
                    if self.recover_from_death {
                        self.set_dead(true);
                        // Resolved here, not left for the draw side: this is
                        // the first point downstream of `net::forward` that
                        // holds a language table at all (see
                        // `NetUpdate::Death::message`'s own doc on why the
                        // message now arrives unflattened). `to_interactive_spans`
                        // keeps whatever `click`/`hover` a killer's own
                        // decorated name carries, the same seam chat and the
                        // tab list already resolve through.
                        self.death_message =
                            Some(self.resolve_text(&message).to_interactive_spans());
                        self.status = "you died".into();
                    } else {
                        // Retained only as the live death gate's negative control:
                        // the pre-fix behaviour that declared the session over and
                        // stranded the client on the death screen forever.
                        self.status = "server: died".into();
                        self.set_phase(SessionPhase::Ended(Box::new(SessionEnd::died(
                            lodestone_model::ResolvedText::literal("player died"),
                        ))));
                    }
                }
                NetUpdate::Respawned { dimension } => {
                    // The server confirmed the respawn: the player is alive again.
                    // The fresh spawn position arrives in the placement teleport
                    // that immediately follows this event; the `NetUpdate::Teleport`
                    // arm snaps `position` and `prev_position` together, so the
                    // frame interpolator never smears the camera from the death
                    // site across the world to the new spawn (the same class of
                    // bug as the original far-spawn camera gap).
                    self.set_dead(false);
                    self.death_message = None;
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut count) = w.get_mut::<RespawnCount>(local) {
                            count.0 += 1;
                        }
                    });
                    self.status = "respawned".into();
                    // **The same packet is also how a portal trip is reported**, so
                    // this is the one place the two are told apart:
                    // `apply_respawn` compares the destination against the
                    // dimension whose reset already ran and drops the other
                    // entities and every meshed column only when they differ. A
                    // death-respawn in the same dimension changes nothing here, and
                    // nothing player-scoped (inventory, XP, health) is touched on
                    // either path — see `sim/dimension.rs`'s survives/does-not
                    // table. It sets its own status line when it fires, which is
                    // why the "respawned" line above is not the last word.
                    self.apply_respawn(dimension);
                }
                NetUpdate::WinGame => {
                    // A pure latch. `app.rs`'s `drive_ui_from_session`
                    // notices `Sim::has_won()` the same way it notices
                    // `Sim::is_dead()` for the death screen, and opens the
                    // credits screen exactly once (guarded there on the
                    // screen not already being `Credits`).
                    self.won = true;
                }
                NetUpdate::LanOpened { port } => {
                    // Vanilla's `menu.multiplayerOptions.publish.started.lan`,
                    // which is a chat line rather than a toast — the port has to
                    // stay readable while the host reads it out, and a toast
                    // expires in five seconds.
                    tracing::info!(target: "chat", "Local game hosted on port {port}");
                    self.push_local_chat(format!("Local game hosted on port {port}"));
                    self.status = format!("open to LAN on {port}");
                    // `NetUpdate::LanOpened` is the source for the local-host
                    // notification. `app::session::drive_ui_from_session`
                    // reconciles `Self::lan_published` into
                    // `MenuNav::set_lan_published`, keeping the pause-menu
                    // action aligned with the session state.
                    self.lan_published = true;
                }
                NetUpdate::Sound {
                    name,
                    category,
                    pos,
                    volume,
                    pitch,
                    seed,
                } => {
                    let pos = glam::Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                    // Drop a sound we already predicted locally. Nothing reachable
                    // today double-plays — see `lodestone_sound::predict` — so this
                    // is defence in depth against a server that forgets vanilla's
                    // acting-player exclusion, not a fix for a live bug.
                    if self.suppresses_echo(&name, pos) {
                        continue;
                    }
                    self.audio_mut(|audio| {
                        if let Some(audio) = audio {
                            audio.play_sound(&name, category, pos, volume, pitch, seed);
                        }
                    });
                }
                NetUpdate::EntitySound {
                    name,
                    category,
                    entity_id,
                    volume,
                    pitch,
                    seed,
                } => {
                    // Resolve the entity's live position *before* borrowing the
                    // audio engine mutably (disjoint, sequential borrows).
                    let pos = self.entity_sound_position(entity_id);
                    self.audio_mut(|audio| {
                        if let Some(audio) = audio {
                            audio.play_entity_sound(&name, category, pos, volume, pitch, seed);
                        }
                    });
                }
                // Only the local player's effects are folded: they feed both the
                // physics view ([`PlayerState::effects`]) and the display view
                // ([`Sim::hud_effects`]). Entity-scoped effects are filtered here
                // rather than in `net::forward`, keeping the wire event
                // entity-agnostic.
                NetUpdate::EffectApplied {
                    entity_id,
                    effect,
                    amplifier,
                    duration_ticks,
                    ambient,
                    show_icon,
                } => {
                    if self.server_entity_id() == Some(entity_id) {
                        let local = self.local;
                        self.write(|w| {
                            if let Some(mut state) = w.get_mut::<PhysicsState>(local) {
                                state.0.effects.apply(&effect, amplifier);
                            }
                            if let Ok(id) =
                                lodestone_model::Identifier::new("minecraft", effect.as_str())
                                && let Some(mut effects) = w.get_mut::<HudEffects>(local)
                            {
                                effects.0.apply(lodestone_game::effect::StatusEffect {
                                    id,
                                    amplifier: u8::try_from(amplifier).unwrap_or(u8::MAX),
                                    duration_ticks,
                                    ambient,
                                    show_particles: true,
                                    show_icon,
                                });
                            }
                        });
                    }
                }
                // The camera's damage tilt (`GameRenderer.bobHurt`). Filtered to
                // the local player here rather than in `net.rs`'s router, matching
                // the effect arms below: the router forwards every entity's hurt
                // animation and this is where "is that me" is decided.
                //
                // The *other* consumer of the same wire event is
                // `lodestone_ecs::ingest`'s `HurtTime` component, which reddens the
                // mob that was hit. Both are live and neither subsumes the other —
                // ingest drops the `yaw`, which is the whole direction half of the
                // tilt, and a `HurtTime` on a remote mob must not tilt our camera.
                NetUpdate::HurtAnimation { entity_id, yaw } => {
                    if self.server_entity_id() == Some(entity_id) {
                        self.on_local_player_hurt(yaw);
                    }
                }
                NetUpdate::EffectRemoved { entity_id, effect } => {
                    if self.server_entity_id() == Some(entity_id) {
                        let local = self.local;
                        self.write(|w| {
                            if let Some(mut state) = w.get_mut::<PhysicsState>(local) {
                                state.0.effects.remove(&effect);
                            }
                            if let Ok(id) =
                                lodestone_model::Identifier::new("minecraft", effect.as_str())
                                && let Some(mut effects) = w.get_mut::<HudEffects>(local)
                            {
                                effects.0.remove(&id);
                            }
                        });
                    }
                }
                // The tab-list and scoreboard arms are *deleted*, not moved:
                // `lodestone_ecs::session`'s systems fold them inside the
                // client, and `Sim::sidebar`/`tab_list_view` read that one copy
                // through `NetClient`. Keeping a fold here as well is precisely
                // the two-sources-of-truth Stage 3 exists to remove.
                NetUpdate::TitleEvent(event) => {
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut title) = w.get_mut::<TitleOverlay>(local) {
                            let _ = title.0.apply(&event);
                        }
                    });
                }
                NetUpdate::ActionBar(text) => {
                    let local = self.local;
                    self.write(|w| {
                        if let Some(mut bar) = w.get_mut::<ActionBarOverlay>(local) {
                            bar.0.set(text);
                        }
                    });
                }
                NetUpdate::Disconnected(reason) => {
                    // `reason` is an unresolved `Text`: a kicked player's
                    // disconnect reason is a `translate` component like
                    // `multiplayer.disconnect.kicked`, so it goes through the
                    // same read-boundary translator that
                    // `title_overlay`/`action_bar_overlay` already use.
                    //
                    // **Resolved, not flattened.** This used to continue
                    // `.to_legacy_string()` into `format!("disconnected: {…}")`,
                    // and those two calls were where every kick message lost its
                    // colour: the styled tree became a `String` here, so no
                    // downstream renderer *could* draw a span — and the
                    // `"disconnected: "` prefix was ours, not vanilla's, which
                    // puts its screen title in a separate widget above the
                    // reason rather than gluing it on.
                    let reason = self.resolve_text(&reason);
                    self.reset_for_server_transfer();
                    self.status = format!("disconnected: {}", reason.to_plain_string());
                    self.set_phase(SessionPhase::Ended(Box::new(SessionEnd::disconnected(
                        reason,
                    ))));
                }
                NetUpdate::Error(e) => {
                    // A client-side failure, and a *different thing* from the arm
                    // above: there is no server text, only our own error. It gets
                    // logged here as well as shown, because until now the real
                    // cause was logged inside `lodestone-client` and the shell
                    // then reported the generic end-of-stream reason instead —
                    // the failure mode where a join error reached no log at all.
                    tracing::error!(error = %e, "session failed");
                    self.reset_for_server_transfer();
                    self.status = format!("net error: {e}");
                    self.set_phase(SessionPhase::Ended(Box::new(SessionEnd::failed(
                        lodestone_model::ResolvedText::literal(e),
                    ))));
                }
                NetUpdate::LanPublishError(e) => {
                    // The non-fatal counterpart to the arm above: a publish
                    // attempt (typically a second press of the pause menu's
                    // Open to LAN, since the world is already published) that
                    // failed server-side without the net thread's own loop
                    // ever leaving — see `NetUpdate::LanPublishError`'s own
                    // doc for why this must never touch `SessionPhase`. A
                    // real `NetUpdate::Error` here used to run this exact
                    // message through the arm above and disconnect a
                    // perfectly healthy session.
                    tracing::warn!(error = %e, "lan publish failed");
                    self.push_local_chat(e);
                }
            }
        }

        // Start this frame's pickup animations — **inside `poll_net`,
        // ahead of `fold_entities`, and that ordering is the whole trick.**
        // `handleTakeItemEntity` removes the item entity in the same breath as it
        // spawns the animation, so by the time `Sim::step` reaches `fold_entities`
        // the server has stopped reporting the item and `fold_snapshots` prunes its
        // render track and its `ItemStacks` entry. `begin_item_pickup` reads both.
        // Deferring this by even one call site draws nothing, silently.
        let pickups = self.pickups.drain();
        if !pickups.is_empty() {
            self.write(|w| {
                for pickup in pickups {
                    // `false` is "the item was not tracked on the render side" —
                    // no stack ever reported, or the track already pruned. Nothing
                    // to animate, and that is the pre-fix behaviour rather than a
                    // failure worth logging every time somebody walks over an
                    // unreported drop.
                    let _ = crate::entities::begin_item_pickup(
                        w,
                        pickup.item_entity_id,
                        pickup.collector_id,
                    );
                }
            });
        }
    }
}
