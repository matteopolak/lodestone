//! `Sim`'s interaction/combat cluster, split out of `sim.rs` into its own
//! module: `break_block`, `begin_attack`/`begin_attack_demo`/
//! `begin_attack_live`, `entity_target`, `attack_entity`,
//! `maybe_spawn_crit_particles`, `interact_entity`, `end_attack`, `use_item`,
//! `end_use`/`end_use_live`, `use_item_live`, `use_item_generic`,
//! `predict_block` and `place_block` — seam 3 of the sim.rs decomposition
//! sequence (seam 1 was the test module, `sim/tests.rs`; seam 2 was placement
//! prediction, `sim/placement.rs`). This is `impl Sim` methods, not free
//! functions, so nothing needed re-exporting: a method call resolves through
//! the `Sim` type regardless of which file defines it, and `sim::actions` is
//! a *descendant* of `sim`, so it already has the same visibility into
//! `Sim`'s private fields and `sim.rs`'s other private items that
//! `sim::tests` has always had — the same reasoning that module's own move
//! relied on.
//!
//! `use super::*;` mirrors `sim/tests.rs`'s own top line for exactly that
//! reason: it pulls in `Sim`'s private fields' types, sim.rs's other private
//! helpers (`face_from_normal`, `hit_cursor`, …) and everything `sim.rs`
//! itself re-exports from `sim::placement`, with no need to enumerate them.
//!
//! **`placement_facts` and `block_intersects_player` left this `impl Sim`
//! block entirely**, later, for `PlaceIntent` (`docs/plugin-api.md`) — moved
//! to free functions in `sim/placement.rs`, parameterised over the two reads
//! that used to come from `self`, since `crate::interact::drive_placement` (a
//! `GameTick` system) needs the identical resolution with no `Sim` to call a
//! method on. `use_item_live`/`place_block` here call the free functions now.

use super::*;

impl Sim {
    /// Break the currently targeted block (set it to air) and remesh. Returns
    /// whether a block was broken.
    ///
    /// This is the **demo-world** direct edit: it mutates the shell's offline
    /// world in place. On a live server the shell must instead route the dig
    /// through the server (see [`begin_attack`](Self::begin_attack)), or the
    /// break would be local-only and the server would restore the block on the
    /// next chunk update.
    pub fn break_block(&mut self) -> bool {
        let Some(hit) = self.target() else {
            return false;
        };
        // Read the state *before* clearing the cell: the debris takes its
        // texture from the block that broke, and after `set_block_world` the
        // cell is air and that information is gone.
        let broken = self.block_at_world(hit.block);
        if self.set_block_world(hit.block, id::AIR) {
            // The demo world has no `ActionQueue` swing to piggy-back on (see
            // `drain_action_queue`), so the animation is started here. Without
            // this the offline demo — including every headless scene — could not
            // exercise the swing at all, which is the one world structurally
            // guaranteed not to.
            self.swing_hand();
            // Full-cube shape: vanilla derives the fragment grid from the
            // block's outline shape, which the shell does not carry, so debris
            // from a slab or fence fills the whole cell rather than hugging the
            // model.
            self.particles_mut(|p| p.destroy_block(hit.block, broken, [1.0; 3]));
            // Vanilla's own break is *predicted*, not received: the client's
            // `MultiPlayerGameMode.destroyBlock` runs `playerWillDestroy` →
            // `spawnDestroyParticles` → `level.levelEvent(player, 2001, …)`, and
            // `ClientLevel.levelEvent` ignores the exclusion and dispatches
            // straight into `LevelEventHandler`'s `case 2001` locally
            // (`ClientLevel.java:877-882`) — sound and debris together. This is
            // the offline mirror of that; the live predicted break is still
            // silent because its emit lives in `interact.rs`'s ECS system, which
            // has no audio handle (see `docs/sound-playback.md`).
            self.play_block_break_sound(hit.block, broken);
            self.remesh_around(hit.block);
            self.set_target(None);
            true
        } else {
            false
        }
    }

    /// Begin an attack (left-click / attack button pressed).
    ///
    /// Vanilla's `Minecraft.startAttack` (`Minecraft.java:1603-1672`) switches
    /// on `hitResult.getType()` and swings the arm **unconditionally after the
    /// switch**, on every arm of it, miss included:
    ///
    /// * `ENTITY` — `this.gameMode.attack(player, entity)`, i.e. send the
    ///   attack.
    /// * `BLOCK`, and the block is *not* air — `startDestroyBlock`, i.e. begin
    ///   mining. (Vanilla deliberately **falls through** to `MISS` when the
    ///   block at `hitResult`'s position is air; this shell's `target()`
    ///   never reports a hit on an air cell in the first place — the ray only
    ///   stops at a *solid* cell — so that fallthrough has no case to cover
    ///   here.)
    /// * `MISS` (or no target at all) — nothing happens server-side, but the
    ///   arm still swings.
    ///
    /// Before this fix, only the `BLOCK`-with-a-dig-that-actually-starts arm
    /// ever reached [`Self::swing_hand`] (through `drive_mining`'s own queued
    /// `SwingArm`, see `drain_action_queue`'s docs) — so punching air, an
    /// entity, or empty space produced no animation at all (issue #72). This
    /// method is the one place all three branches now funnel through.
    ///
    /// `case ENTITY` takes priority over `case BLOCK`: [`EntityRayTarget`] is
    /// already the nearer of an entity-or-block pick (see
    /// [`Self::update_entity_target`]'s docs), so a `Some` there means mining
    /// must not start on this click even when [`RayTarget`] also holds a
    /// block.
    ///
    /// # What is deliberately not modelled here
    ///
    /// Vanilla's `attackStrengthTicker`/`getAttackStrengthScale` cooldown, the
    /// crit condition and the sweep-attack condition are real per-hit vanilla
    /// mechanics, but every one of them exists only to scale **local** sound/
    /// particle feedback and the crosshair cooldown indicator — the damage
    /// number itself is server-authoritative (the wire `Attack` packet
    /// carries only the target id, no damage or strength scalar; see
    /// `EntityInteraction::Attack`'s encoding in
    /// `crates/protocol/v770/src/adapter.rs`). None of those consumers exist
    /// in this shell yet: the crosshair indicator is `hud.rs`'s (held by
    /// another agent), and sweep/crit sound-and-particle feedback is
    /// `entities.rs`/asset work, also out of this file's scope. Building a
    /// ticker nothing reads would be exactly the unconsumed-island class
    /// `CLAUDE.md`'s core rule warns about, so it stays unbuilt rather than
    /// built and orphaned — whoever adds the crosshair pip or the sweep sound
    /// is the right owner for it, alongside the half it feeds.
    pub fn begin_attack(&mut self) {
        if self.is_live() {
            self.begin_attack_live();
        } else {
            self.begin_attack_demo();
        }
    }

    /// The demo-world half of [`Self::begin_attack`]: break the targeted
    /// block if there is one ([`Self::break_block`] already swings on
    /// success), or swing on a miss — the offline mirror of vanilla's
    /// unconditional swing. The demo ECS holds no networked entities (see
    /// [`Self::update_entity_target`]'s docs), so there is no `case ENTITY` to
    /// take here; only `BLOCK` vs `MISS`.
    fn begin_attack_demo(&mut self) {
        if !self.break_block() {
            self.swing_hand();
        }
    }

    /// The live half of [`Self::begin_attack`]. See that method's docs for the
    /// three-way switch this implements.
    pub(crate) fn begin_attack_live(&mut self) {
        if self.is_dead() {
            return;
        }
        if let Some(entity_id) = self.entity_target() {
            self.attack_entity(entity_id);
            self.swing_hand();
            return;
        }
        if self.target().is_some() {
            // Unchanged from before this fix: arms the hold-to-mine loop.
            // `drive_mining` itself queues the `SwingArm` the instant a dig
            // actually starts, through the same `ActionQueue`/
            // `drain_action_queue` funnel every other tick-driven swing uses.
            self.write(|w| w.resource_mut::<Attacking>().0 = true);
            return;
        }
        // MISS: no block, no entity. Vanilla still swings.
        self.swing_hand();
    }

    /// The entity [`EntityRayTarget`] currently names, if any — the live
    /// left-click's attack target.
    #[must_use]
    pub fn entity_target(&self) -> Option<i32> {
        self.read(|w| w.resource::<EntityRayTarget>().0)
    }

    /// `key.pickItem` — vanilla's `Minecraft.pickBlockOrEntity`
    /// (`Minecraft.java:2342-2354`), middle-click by default
    /// (`Options.java:669`). `include_data` is vanilla's `hasControlDown()`.
    ///
    /// Entity wins over block, for the same reason [`Self::begin_attack_live`]
    /// already gives: [`EntityRayTarget`] is resolved as the *nearer* pick, so
    /// preferring it here matches what the crosshair is actually on rather than
    /// re-deciding the priority.
    ///
    /// Two distinct actions rather than one with an enum, because 26.2 splits
    /// them on the wire — `PickItemFromBlock` carries a packed `BlockPos`,
    /// `PickItemFromEntity` a VarInt entity id (see the v770 adapter's own
    /// arms). Both encoders existed with **zero producers** before this method,
    /// the same outbound-island shape `ClientAction::SetFlying` was caught in.
    ///
    /// Sent directly rather than through [`ActionQueue`], like the attack and
    /// use paths: that queue drains inside the tick loop, and this is a discrete
    /// click, not a per-tick one. No game-mode gate — vanilla's pick works in
    /// every mode, spectator included.
    pub fn pick_block_or_entity(&mut self, include_data: bool) {
        if let Some(entity_id) = self.entity_target() {
            if let Some(net) = &self.net {
                net.send_action(ClientAction::PickItemFromEntity {
                    entity_id,
                    include_data,
                });
            }
            return;
        }
        let Some(hit) = self.target() else { return };
        let pos = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        if let Some(net) = &self.net {
            net.send_action(ClientAction::PickItemFromBlock { pos, include_data });
        }
    }

    /// Send the serverbound attack for `entity_id` — vanilla's
    /// `MultiPlayerGameMode.attack`'s outbound half. Lowers to
    /// `ClientAction::InteractEntity { interaction: EntityInteraction::Attack,
    /// .. }`, which the v770 adapter already encodes as the dedicated `Attack`
    /// packet (26.2 split entity-attack out of the old combined interact
    /// packet; see `crates/protocol/v770/src/adapter.rs`'s `InteractEntity`
    /// arm) — this method is the first caller that ever constructs the
    /// variant; the encoder was previously dead, unused code.
    ///
    /// Sent directly, like [`Self::use_item_live`]'s two sends, rather than
    /// queued through [`ActionQueue`]: that queue only drains inside the tick
    /// loop (see `crate::interact`'s "how to change it"), and an attack is a
    /// discrete click event, not a per-tick one.
    ///
    /// Also resets [`AttackStrengthTicker`] to `0` — vanilla's
    /// `MultiPlayerGameMode.attack` calling `player.resetAttackStrengthTicker()`
    /// right after the client-side `player.attack(entity)`
    /// (`MultiPlayerGameMode.java:425-430`, `.cache/mc/26.2/client-src`).
    /// Unconditional on every entity target, exactly like vanilla's call site:
    /// there is no client-side `cannotAttack` gate here (damage is fully
    /// server-authoritative per `docs/combat.md`), so every left-click on an
    /// entity restarts the cooldown regardless of whether the server ends up
    /// applying any damage.
    fn attack_entity(&mut self, entity_id: i32) {
        // The same tick-driven intent `use_item_live` reads for its own
        // sneaking bit, so a sneak-attack cannot disagree with what the wire
        // already told the server this tick's crouch state is.
        let sneaking = self.movement_intent().sneak;
        let local = self.local;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Attack,
                sneaking,
            });
        }
        // Vanilla's own order (`MultiPlayerGameMode.attack`,
        // `MultiPlayerGameMode.java:427-429`): the packet, then the
        // client-side `player.attack(entity)` prediction — whose crit
        // condition reads `attackStrengthTicker` **before** it is reset — and
        // only then `resetAttackStrengthTicker()`. Reading the ticker after
        // zeroing it here would make `fullStrengthAttack` false on every
        // attack, including the one that just landed at full charge, so this
        // call must stay above the reset below.
        self.maybe_spawn_crit_particles(entity_id);
        self.write(|w| {
            if let Some(mut ticker) = w.get_mut::<AttackStrengthTicker>(local) {
                ticker.0 = 0;
            }
        });
    }

    /// Vanilla's local-only crit-particle prediction — `Player.attack`'s
    /// `criticalAttack = fullStrengthAttack && canCriticalAttack(entity)`
    /// (`Player.java:970-971,1032-1041`), whose visual half is
    /// `attackVisualEffects`' `this.crit(entity)` call (`Player.java:1063-1066`,
    /// `LocalPlayer.crit` → `ParticleEngine.createTrackingEmitter`,
    /// `LocalPlayer.java:664-665`).
    ///
    /// # This is real vanilla dual simulation, not an approximation invented
    /// for this port
    ///
    /// `MultiPlayerGameMode.attack` runs the **client's own copy** of
    /// `player.attack(entity)` (`MultiPlayerGameMode.java:428`) independently
    /// of, and before, the server's authoritative copy of the same method —
    /// the server computes the real damage, the client predicts only the
    /// cosmetic trigger (sound + particle) so it does not wait a round trip to
    /// see feedback on its own swing. The wire `Attack` packet itself carries
    /// no damage or crit flag (`docs/combat.md`); nothing here affects what
    /// the server decides.
    ///
    /// # Condition, checked against the jar rather than assumed
    ///
    /// `canCriticalAttack` (`Player.java:1032-1041`): `fallDistance > 0.0 &&
    /// !onGround && !onClimbable && !isInWater && !isMobilityRestricted &&
    /// !isPassenger && entity is LivingEntity && !isSprinting`.
    /// `fullStrengthAttack = getAttackStrengthScale(0.5F) > 0.9F`
    /// (`Player.java:956,962`) is the caller's own gate, not part of
    /// `canCriticalAttack` — hence [`Self::attack_strength_scale_at`] rather
    /// than reusing [`Self::attack_strength_scale`]'s `a = 0.0`, which is a
    /// different call site's (the crosshair's) partial-tick argument.
    ///
    /// Two vanilla clauses are not modelled, and the divergence is small and
    /// explained rather than silent:
    /// - **`!onClimbable` is not read separately.** This engine resets
    ///   `fall_distance` to `0.0` the instant `tick_air` finds a climbable —
    ///   `LivingEntity.handleOnClimbable`, folded into `tick_air` per
    ///   [`lodestone_physics::player::PlayerState::fall_distance`]'s own
    ///   "Climbable reset" bullet — so `fall_distance > 0.0` already implies
    ///   not-on-climbable in this port's physics model. Checked against that
    ///   source rather than guessed.
    /// - **`!isMobilityRestricted`/`!isPassenger`, and the outer `baseDamage >
    ///   0.0F || magicBoost > 0.0F` gate, are not modelled.** This shell has
    ///   no riding state (`docs/combat.md`'s knockback section notes the same
    ///   absence for a different mechanic) and no local weapon-damage/
    ///   enchantment computation to derive `baseDamage`/`magicBoost` from —
    ///   the identical gap [`Self::attack_strength_delay`]'s own doc names for
    ///   `lodestone-data` carrying no per-item attack-speed census. The only
    ///   case this can diverge on is an attack that deals zero base damage
    ///   (an already-broken or damage-less item), which vanilla itself treats
    ///   as "nothing happens" at the outer `if` — the crit particle is cosmetic
    ///   and no damage number depends on it either way.
    ///
    /// # The particle burst: one tick of `TrackingEmitter`, not three
    ///
    /// `TrackingEmitter` (`TrackingEmitter.java:29-41`) runs for **3 ticks**,
    /// spawning up to 16 candidates per tick (filtered to a unit sphere,
    /// ~52% pass) that track the entity's *current* position each tick. This
    /// shell's particle system has no per-attack persistent emitter — every
    /// existing local spawn ([`crate::particles::Particles::destroy_block`]/
    /// `breaking_block`) is a one-shot burst — so this spawns **one** tick's
    /// worth (16 candidates, same unit-sphere filter) at the target's
    /// position at the moment of the attack, rather than adding new
    /// multi-tick emitter machinery for a purely cosmetic burst. The
    /// per-candidate position/velocity formula (`Entity.getX(double)` etc.,
    /// `Entity.java:3770-3811`) and the emitted particle's own physics
    /// (`lodestone_particle::emit::crit`) are both exact; only the tick count
    /// is a disclosed simplification.
    fn maybe_spawn_crit_particles(&mut self, entity_id: i32) {
        if self.attack_strength_scale_at(0.5) <= 0.9 {
            return;
        }
        let Some((feet, width, height)) = self.read(|w| {
            let target = w.resource::<EntityIndex>().get(entity_id)?;
            let pos = w.get::<Position>(target)?;
            let kind = w.get::<EntityKind>(target)?;
            let facts = w.resource::<VersionData>().entity_facts(&kind.0)?;
            let type_id = lodestone_data::entity_types::entity_type_id_parts(
                kind.0.namespace(),
                kind.0.path(),
            )?;
            lodestone_data::entity_census::is_living(type_id)
                .unwrap_or(false)
                .then_some((pos.0, facts.dimensions.width, facts.dimensions.height))
        }) else {
            return;
        };
        let local = self.local;
        let (fall_distance, on_ground) = self.read(|w| {
            w.get::<PhysicsState>(local)
                .map_or((0.0, true), |s| (s.0.fall_distance, s.0.on_ground))
        });
        if fall_distance <= 0.0 || on_ground {
            return;
        }
        if self.fluid_state().in_water() || self.movement_intent().sprint {
            return;
        }
        self.particles_mut(|p| {
            let engine = p.engine_mut();
            for _ in 0..16 {
                let xa = f64::from(engine.rng().next_float()) * 2.0 - 1.0;
                let ya = f64::from(engine.rng().next_float()) * 2.0 - 1.0;
                let za = f64::from(engine.rng().next_float()) * 2.0 - 1.0;
                if xa * xa + ya * ya + za * za > 1.0 {
                    continue;
                }
                let x = f64::from(feet.x) + f64::from(width) * (xa / 4.0);
                let y = f64::from(feet.y) + f64::from(height) * (0.5 + ya / 4.0);
                let z = f64::from(feet.z) + f64::from(width) * (za / 4.0);
                particle_emit::crit(engine, x, y, z, xa, ya + 0.2, za);
            }
        });
    }

    /// Send the serverbound **use-on-entity** for `entity_id` — vanilla's
    /// `MultiPlayerGameMode.interact`, the outbound half of mounting a boat,
    /// minecart or saddled animal.
    ///
    /// This is the mirror image of [`Self::attack_entity`]: same packet family,
    /// same direct-send reasoning (a click is a discrete event, not a per-tick
    /// one, and [`ActionQueue`] only drains inside the tick loop), same
    /// tick-derived `sneaking` bit so the local decision cannot disagree with the
    /// crouch state the wire already reported this tick. The differences are the
    /// interaction kind and that there is no attack cooldown to reset.
    ///
    /// **`Interact`, never `InteractAt`** — see [`Self::use_item_live`]'s entity
    /// branch for why the entity-local hit position is not fabricated here.
    ///
    /// The swing is vanilla's too: `MultiPlayerGameMode.interact` is followed by
    /// `player.swing(hand)` at the `Minecraft.startUseItem` call site whenever the
    /// result `consumesAction()`. We swing unconditionally, matching what
    /// [`Self::use_item_live`]'s block path already does with its own
    /// `SwingArm` — the result is server-side and one round trip away, and a
    /// suppressed swing on a refused interaction is a smaller error than a
    /// missing swing on an accepted one.
    fn interact_entity(&mut self, entity_id: i32) {
        let sneaking = self.movement_intent().sneak;
        if let Some(net) = &self.net {
            net.send_action(ClientAction::InteractEntity {
                entity_id,
                interaction: EntityInteraction::Interact { hand: Hand::Main },
                sneaking,
            });
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // Client-side animation, so it runs with or without a socket — the same
        // split `use_item_live` makes for its own unconditional `swing_hand`.
        self.swing_hand();
    }

    /// End an attack (attack button released). Aborts a live dig in progress so
    /// the server stops mining; a no-op on the demo world.
    pub fn end_attack(&mut self) {
        if !self.is_live() {
            return;
        }
        let actions = self.write(|w| {
            w.resource_mut::<Attacking>().0 = false;
            w.resource_mut::<MiningPredictor>().0.stop()
        });
        // Sent directly rather than queued: `ActionQueue` is only drained inside
        // the tick loop, so a release on a frame that runs no tick would sit for
        // up to 50 ms before the `ABORT` reached the server. See
        // `crate::interact`'s "how to change it".
        if let Some(net) = &self.net {
            for action in actions {
                net.send_action(action);
            }
        }
    }

    /// Use the held item on the targeted block (use button pressed). On a live
    /// server this lowers the click into the server's `use_item_on` action
    /// through the placement predictor; on the demo world it places directly.
    pub fn use_item(&mut self) {
        if self.is_live() {
            self.use_item_live();
        } else {
            self.place_block();
        }
    }

    /// Release the use button — vanilla's `Minecraft.java:1914-1917`:
    ///
    /// ```text
    /// if (this.player.isUsingItem()) {
    ///    if (!this.options.keyUse.isDown()) {
    ///       this.gameMode.releaseUsingItem(this.player);
    ///    }
    ///    ...
    /// }
    /// ```
    ///
    /// which itself lowers to `MultiPlayerGameMode.releaseUsingItem`
    /// (`:513-517`) sending a bare `ServerboundPlayerActionPacket`
    /// (`RELEASE_USE_ITEM`) — [`ClientAction::ReleaseUseItem`] here, encoded
    /// by all four protocol adapters already
    /// (`crates/protocol/{v47,v340,v735,v770}/src/adapter.rs`) but with no
    /// producer anywhere in this shell before this method. Bow, crossbow and
    /// shield are all `useOnRelease() == true`
    /// (`LivingEntity.java:3471-3475,3602-3616`) and structurally cannot
    /// complete a use without this packet — food and potions are
    /// `useOnRelease() == false` and auto-complete on the server's own tick
    /// count, which is exactly why this gap went unnoticed: eating and
    /// drinking still worked.
    ///
    /// A no-op on the demo world (nothing there tracks an in-progress use).
    pub fn end_use(&mut self) {
        if self.is_live() {
            self.end_use_live();
        }
    }

    /// The live half of [`Self::end_use`], split out the same way
    /// [`Self::begin_attack_live`] is — reachable directly from a test with no
    /// `vanilla_atlas`, since the swing/send logic itself needs no GPU asset.
    ///
    /// A no-op if [`UsingItem`] is already `false`: no button was ever pressed
    /// down (via [`Self::use_item_live`]) for this to be the release edge of.
    /// Sending `RELEASE_USE_ITEM` in that case would still be harmless —
    /// `LivingEntity.releaseUsingItem`
    /// (`.cache/mc/26.2/src/…/LivingEntity.java:3602-3613`) no-ops whenever
    /// the server has no `useItem` in progress — but there is nothing to
    /// justify sending it for.
    pub(crate) fn end_use_live(&mut self) {
        let was_using = self.write(|w| {
            let mut using = w.resource_mut::<UsingItem>();
            std::mem::replace(&mut using.0, false)
        });
        if !was_using {
            return;
        }
        if let Some(net) = &self.net {
            net.send_action(ClientAction::ReleaseUseItem);
        }
    }

    /// Lower a live right-click into the server's `use_item_on` action **and
    /// predict the placement locally** (issue #381).
    ///
    /// The server stays authoritative: [`Placement::use_on`] returns the action to
    /// send in *every* branch, so the shell sends it unconditionally (with a
    /// proper prediction sequence) and lets the server decide, exactly as vanilla
    /// does. Because the server owns the sneak state derived from the wire, the
    /// crouch input must have been sent (see
    /// [`send_player_input`](Self::send_player_input)) for a sneak-placement
    /// against a chest/door to suppress the interaction.
    ///
    /// # Why the local write exists
    ///
    /// This method used to send and wait, so a placed block did not exist
    /// client-side until the server's `BLOCK_UPDATE` came back — one round trip of
    /// hole. For a chest that is #374 reached through a different door: the state
    /// write is what creates the block entity, and with no local state write there
    /// was no local record and nothing to draw. The prediction now writes through
    /// [`write_predicted_block`], the same `set_block` + `sync_block_entity` pair
    /// the adapter's `BLOCK_UPDATE` arm calls.
    ///
    /// # What happens when the server refuses
    ///
    /// Nothing here has to detect it, because vanilla's server corrects **both**
    /// candidate positions after *every* `use_item_on`, unconditionally — accepted,
    /// refused, or actually an interaction
    /// (`ServerGamePacketListenerImpl.java:1397-1398`):
    ///
    /// ```text
    /// this.send(new ClientboundBlockUpdatePacket(level, pos));
    /// this.send(new ClientboundBlockUpdatePacket(level, pos.relative(direction)));
    /// ```
    ///
    /// `pos` is `clicked` and `pos.relative(direction)` is the adjacent cell, and a
    /// prediction can only ever land on one of those two. So a refused placement is
    /// overwritten by the authoritative state within one round trip — and since
    /// #374 that path calls `sync_block_entity`, which **removes** the block-entity
    /// record the prediction created (`BlockEntitySync::Removed`). The removal half
    /// is not a second mechanism to build; it is the same one, pointing the other
    /// way. `crates/lodestone-shell/tests/placed_chest_block_entity_pixels.rs`
    /// gates it rather than assuming it.
    ///
    /// A mispredicted placement therefore costs exactly the round trip the hole
    /// used to cost, which is why every classification below is allowed to err
    /// toward *not* predicting but never toward predicting something wrong.
    pub(crate) fn use_item_live(&mut self) {
        if self.is_dead() {
            return;
        }
        // Marks [`UsingItem`] so a later [`Self::end_use`] knows the button
        // was actually pressed — see that resource's own docs for why this is
        // an input-state mirror rather than vanilla's real `isUsingItem()`.
        // Set unconditionally here rather than in every branch below: vanilla
        // arms `player.isUsingItem()` from the held item's own `use()` call,
        // which can happen inside any of this method's block/entity/generic
        // branches, and this client has no equivalent per-item hook to mark
        // it from.
        self.write(|w| w.resource_mut::<UsingItem>().0 = true);
        // **Entity before block, and this branch is the whole of "get in a boat".**
        // Vanilla's `Minecraft.startUseItem` switches on `hitResult.getType()` and
        // `case ENTITY` comes first (`Minecraft.java`'s `useItem`), the identical
        // priority [`Self::begin_attack_live`] already implements for the left
        // button off the same [`EntityRayTarget`]. Before this, `use_item_live`
        // returned early on `self.target()` being `None` and never looked at the
        // entity ray at all, so a right-click on a boat, minecart or saddled horse
        // sent nothing — the mount packet had no producer, which is the outbound
        // half of the island `EntityPassengersChanged` was the inbound half of.
        //
        // `InteractAt` is deliberately **not** used, even though vanilla sends both
        // it and `Interact` for a `case ENTITY` click: `InteractAt` carries the
        // entity-local hit position, and [`Self::update_entity_target`] keeps only
        // the winning entity's id, not the ray's hit point on its box. A fabricated
        // local offset would be a wrong number where the server accepts a missing
        // one — `ServerGamePacketListenerImpl` dispatches mounting off the plain
        // `Interact` (it is `Entity.interact` that returns `InteractionResult` and
        // calls `player.startRiding`), and `InteractAt` only matters for the
        // per-part hit an armour stand or a horse's saddle slot resolves. So the
        // honest subset is sent, and refining it needs the ray to start reporting
        // its hit position, not a guess here.
        //
        // **`case ENTITY` only returns here on a *successful* interact.**
        // Vanilla's own switch (`Minecraft.java:1693-1708`) returns
        // immediately only when `gameMode.interact(...) instanceof
        // InteractionResult.Success`; anything else hits an explicit `break;`
        // at `:1708` and falls through to the unconditional generic-use call
        // at `:1730` (`gameMode.useItem`) — which is what actually raises a
        // shield or starts drawing a bow when the crosshair happens to be
        // over a mob with no special right-click behaviour (hostile mobs,
        // overwhelmingly, which is exactly the combat case). Before this fix
        // `use_item_live` always returned here, so `entity_target()` being
        // `Some` for *any* living entity in `ENTITY_REACH` — hostile or not —
        // permanently short-circuited the fallback.
        //
        // This client has no local classification of an interact's result to
        // match vanilla's `instanceof Success` test against: there is no
        // `player.interactOn` equivalent here, only the wire send (the same
        // gap `Self::interact_entity`'s own docs cover for why `InteractAt`
        // is not fabricated). So every entity interact is treated as
        // non-consuming for this decision and always falls through to
        // [`Self::use_item_generic`]. The one place this can diverge from
        // vanilla is a genuinely successful mount (an empty boat, a saddled
        // and rideable horse): vanilla's own local prediction would skip the
        // fallback there, and this does not, so an item held while boarding a
        // vehicle can also start its use. That is judged the smaller error
        // next to a shield/bow that could never fire at all.
        if let Some(entity_id) = self.entity_target() {
            self.interact_entity(entity_id);
            self.use_item_generic();
            return;
        }
        let Some(hit) = self.target() else {
            // Vanilla's own MISS/no-target path: a `null` `hitResult` skips
            // the whole `if (this.hitResult != null)` switch in
            // `startUseItem` (`Minecraft.java:1681,1691`) and still reaches
            // the unconditional fallback at `:1730`. This used to `return`
            // here with **nothing sent at all** — aiming at open air, or at a
            // mob standing just past block reach with nothing behind it,
            // silently dropped the click.
            self.use_item_generic();
            return;
        };
        let clicked = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
        let face = face_from_normal(hit.normal);
        let cursor = hit_cursor(hit);
        // The intent this tick's physics ran on — the same one
        // `lodestone_controller::ecs::send_player_input` derived the wire's shift
        // bit from, so the local decision and the server's cannot disagree. This
        // used to re-read the keyboard, which was frame-granular; vanilla is
        // tick-granular here too (`Minecraft.handleKeybinds` runs in the tick).
        let sneaking = self.movement_intent().sneak;

        let menu = self.player_menu();
        let main = menu
            .player_native(self.selected_slot())
            .filter(|stack| !stack.is_empty())
            .map(|stack| stack.item().clone());
        // Vanilla's `haveSomethingInOurHands` — *either* hand, and it is what
        // makes a sneak-click suppress the block's own use.
        let has_item_in_hand = main.is_some()
            || menu
                .player_native(crate::sim::OFFHAND_NATIVE_INDEX)
                .is_some_and(|stack| !stack.is_empty());
        // Placeable only when the census can name the block *and* classify how it
        // orients. Leaving `placing` at `None` otherwise is what makes an
        // unclassifiable item fall back to send-and-wait rather than write a state
        // we are not confident in.
        let placeable = main.as_ref().and_then(|item| {
            let name = item.to_string();
            let states = block_states_of(&name)?;
            let orientation = orientation_for_placement(&name, &states)?;
            Some((name, states, orientation))
        });
        let ctx = UseOnContext {
            hand: Hand::Main,
            clicked,
            face,
            cursor,
            inside_block: false,
            rotation: Rotation::new(self.player().yaw, self.player().pitch),
            sneaking,
            has_item_in_hand,
            placing: placeable.as_ref().and_then(|_| main.clone()),
            orientation: placeable
                .as_ref()
                .map_or(OrientationKind::Fixed, |&(_, _, kind)| kind),
        };
        // Read the world facts before taking the ECS guard `use_on` needs — see
        // `PlacementFacts` on why the two guards must not nest. Free function
        // since `PlaceIntent` (`crate::interact::drive_placement`, a `GameTick`
        // system, needs the identical resolution with no `Sim` to call it on)
        // — see `sim/placement.rs`'s `placement_facts` doc.
        let bb = self.player().bounding_box(&self.profile());
        let facts = placement_facts(
            clicked,
            face,
            |pos| self.net.as_ref().and_then(|net| net.block_at(pos)),
            |pos| block_intersects_player(&bb, [pos.x, pos.y, pos.z]),
        );
        let decision = self.write(|w| {
            w.resource_mut::<PlacementPredictor>()
                .0
                .use_on(&ctx, &facts)
        });
        let (UseOnDecision::Interact { action }
        | UseOnDecision::Place { action, .. }
        | UseOnDecision::Nothing { action }) = &decision;
        if let Some(net) = &self.net {
            net.send_action(action.clone());
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // This swing bypasses `ActionQueue` (the two sends above go straight to
        // the socket so their wire order is fixed), so it also bypasses
        // `drain_action_queue`'s hook and has to start the animation itself.
        // Unconditional, not inside the `if let` above: the animation is
        // client-side and does not need a socket.
        self.swing_hand();

        // The prediction. `placeable` is `Some` whenever `use_on` could have
        // returned `Place` at all (it is what filled `ctx.placing`), so the only
        // way this declines is `state_for_placement` failing on a property it
        // cannot resolve.
        if let (UseOnDecision::Place { prediction, .. }, Some((name, states, orientation))) =
            (&decision, &placeable)
        {
            if let Some(state) = state_for_placement(name, states, *orientation, &prediction.state) {
                let pos = prediction.pos;
                self.predict_block([pos.x, pos.y, pos.z], state);
                // Vanilla's placement sound is the tail of `BlockItem.place`
                // (`BlockItem.java:87`), which passes the placing player as
                // `playSound`'s **excluded** entity — so the server broadcasts it
                // to everyone but us, and our own copy is predicted locally by
                // `ClientLevel.playSound`, whose exclusion test is inverted
                // (`if (except == this.minecraft.player)`, `ClientLevel.java:705`).
                // It therefore hangs off the prediction, exactly as vanilla's
                // does: no prediction, no sound, and no double-play either.
                //
                // Tied to the *predicted state* rather than to the item, because
                // the sound is `placedState.getSoundType()` — a waterlogged or
                // half-slab placement can be a different `SoundType` from the
                // block's default state.
                self.play_block_place_sound([pos.x, pos.y, pos.z], state);
            }
        }
    }

    /// Vanilla's unconditional generic-use fallback at the bottom of
    /// `Minecraft.startUseItem`'s per-hand loop (`Minecraft.java:1730`,
    /// `gameMode.useItem`) — the send that actually raises a shield, draws a
    /// bow, or starts eating/drinking, independent of any block or entity
    /// under the crosshair. Called from [`Self::use_item_live`]'s entity and
    /// no-target branches; see that method's docs for exactly which vanilla
    /// cases reach it.
    ///
    /// Lowers to [`ClientAction::UseItem`] — a **second** serverbound island
    /// this investigation found alongside `ReleaseUseItem`: encoded by all
    /// four protocol adapters
    /// (`crates/protocol/{v47,v340,v735,v770}/src/adapter.rs`) with zero
    /// producers anywhere in this shell before this method.
    ///
    /// Guarded on the main hand actually holding something, matching
    /// vanilla's own `!heldItem.isEmpty()` check at the same call site —
    /// there is nothing to use and no packet to justify for an empty hand.
    /// Only `Hand::Main` is considered, matching every other send in this
    /// method; vanilla's per-hand loop also tries the off hand, which this
    /// shell does not model here.
    ///
    /// The prediction sequence is borrowed from [`PlacementPredictor`]'s own
    /// counter rather than a second, independent one — see
    /// [`Placement::take_use_sequence`]'s docs for why that matches vanilla's
    /// own single shared counter.
    fn use_item_generic(&mut self) {
        let has_item = self
            .player_menu()
            .player_native(self.selected_slot())
            .is_some_and(|stack| !stack.is_empty());
        if !has_item {
            return;
        }
        let rotation = Rotation::new(self.player().yaw, self.player().pitch);
        let sequence =
            self.write(|w| w.resource_mut::<PlacementPredictor>().0.take_use_sequence());
        if let Some(net) = &self.net {
            net.send_action(ClientAction::UseItem {
                hand: Hand::Main,
                rotation,
                sequence,
            });
            net.send_action(ClientAction::SwingArm { hand: Hand::Main });
        }
        // Client-side animation, so it runs with or without a socket — the
        // same split every other swing site in this method makes.
        self.swing_hand();
    }

    /// Apply a locally predicted block state to the one chunk store and re-mesh.
    ///
    /// The write itself is [`write_predicted_block`] — state *and* block entity,
    /// the adapter's `BLOCK_UPDATE` pair — so a predicted chest exists as a
    /// block-entity record the moment it is placed instead of one round trip
    /// later.
    fn predict_block(&mut self, block: [i32; 3], state: u32) -> BlockEntitySync {
        let store = self.chunk_world();
        // The chunk guard is taken and dropped before `remesh_around` reaches for
        // the ECS resource again, so the two are never held together.
        let outcome = {
            let mut world = store.write();
            write_predicted_block(&mut *world, block, state)
        };
        self.remesh_around(block);
        outcome
    }

    /// Place [`PLACE_BLOCK`] against the targeted face on the **demo world**, if
    /// the cell is empty and doesn't intersect the player. Returns whether a
    /// block was placed. The live path uses [`use_item`](Self::use_item) instead
    /// so the server actually hears the placement.
    pub fn place_block(&mut self) -> bool {
        let Some(hit) = self.target() else {
            return false;
        };
        let pos = hit.place_position();
        let cell_empty = {
            let store = self.chunk_world();
            let world = store.read();
            let view = WorldCollision::new(&world);
            view.block_at(pos[0], pos[1], pos[2]) == id::AIR
        };
        let bb = self.player().bounding_box(&self.profile());
        if !cell_empty || block_intersects_player(&bb, pos) {
            return false;
        }
        if self.set_block_world(pos, PLACE_BLOCK) {
            self.remesh_around(pos);
            // Demo-world placement, same reasoning as `break_block`.
            self.swing_hand();
            true
        } else {
            false
        }
    }
}
