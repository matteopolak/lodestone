//! Per-tick orchestration and owner-batch application for [`super::MobSim`].

use super::*;

impl<'w> MobSim<'w> {
    /// Advances every mob one tick: run its goals (which drive A\* and path
    /// following through the [`MobController`] seam), then step the follower.
    /// Each mob's `no_action_time` ages by one tick and is first cleared for any
    /// persistent mob, or
    /// one within its category's immune radius of a player from
    /// [`set_players`](Self::set_players). See the body for why that reset lives
    /// here rather than only in [`despawn_pass`](MobSim::despawn_pass), which
    /// has no production caller and left the counter monotonic — permanently
    /// disabling every idle-throttled goal five seconds into a world.
    ///
    /// A melee attack that connected this tick is resolved into a real
    /// [`SimMob::apply_damage`] call against whichever mob its
    /// [`attack_target_id`](SimMob::attack_target_id) names — the goal
    /// scheduler only ever produces the *intent* to strike (a position, via
    /// [`NavigatingMob::take_new_attacks`]); this is where that intent becomes
    /// a real health change. Resolution runs in a second pass over collected
    /// events, after every mob's own AI has ticked, so an attacker damaging
    /// another mob never needs two simultaneous mutable borrows into the same
    /// `Vec`. A mob whose health reaches `0.0` is removed at the end of the
    /// tick that killed it.
    pub fn tick(&mut self) {
        let world = self.world;
        self.tick_with_terrain(&|x, y, z| world.block_state(x, y, z).to_owned());
    }

    /// Produces chunk-owner completions from dropped-item tick-start state.
    ///
    /// Lifecycle counters and collision motion are computed on copies. No
    /// owner writes either live item registry; the central apply step below
    /// validates the complete plan before publishing any result.
    pub(crate) fn tick_item_owner_batches(
        &mut self,
        block_state: &(dyn Fn(i32, i32, i32) -> String + Sync),
    ) -> (Vec<ItemTickOwnerBatch>, u64) {
        self.item_owner_plan = self
            .item_owner_plan
            .checked_add(1)
            .expect("item owner plan generation must not overflow");
        #[cfg(not(target_arch = "wasm32"))]
        let workers = if self.items.len() >= 128 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
                .min(4)
        } else {
            1
        };
        #[cfg(target_arch = "wasm32")]
        let workers = 1;
        self.tick_item_owner_batches_with_workers(block_state, workers)
    }

    pub(super) fn tick_item_owner_batches_with_workers(
        &self,
        block_state: &(dyn Fn(i32, i32, i32) -> String + Sync),
        worker_count: usize,
    ) -> (Vec<ItemTickOwnerBatch>, u64) {
        let mut jobs = Vec::<(ItemTickOwner, Vec<ItemTickInput>)>::new();
        for (serial, tracked) in self.items.iter().enumerate() {
            let state = self
                .item_state
                .get(&tracked.id)
                .cloned()
                .expect("a tracked item lifecycle must have matching motion state");
            let owner = state.owner;
            let input = ItemTickInput {
                owner,
                serial,
                id: tracked.id,
                state,
                lifecycle: tracked.lifecycle,
            };
            if let Some((_, inputs)) = jobs.iter_mut().find(|(candidate, _)| *candidate == owner) {
                inputs.push(input);
            } else {
                jobs.push((owner, vec![input]));
            }
        }
        let min_y = f64::from(self.world.min_y);
        let plan = self.item_owner_plan;
        let completed = crate::tick_region::run_bounded_owner_jobs(jobs, worker_count, &|(owner, inputs)| {
            let view = LiveBlockCollision {
                block_state,
                probe_count: std::cell::Cell::new(0),
            };
            let effects = inputs
                .into_iter()
                .map(|input| {
                    let mut lifecycle = input.lifecycle;
                    let mut state = input.state;
                    lifecycle.tick();
                    let mut discard = lifecycle.should_despawn();
                    if !discard {
                        let before = state.motion.position;
                        state.motion.tick();
                        settle_item(&view, &mut state.motion, before);
                        discard = state.motion.position.y < min_y - VOID_DESPAWN_DEPTH;
                    }
                    let destination = ItemTickOwner::for_position(state.motion.position);
                    ItemTickEffect {
                        owner: input.owner,
                        destination,
                        serial: input.serial,
                        id: input.id,
                        lifecycle,
                        state,
                        discard,
                    }
                })
                .collect();
            (
                ItemTickOwnerBatch {
                    owner,
                    plan,
                    expected_batch_count: 0,
                    effects,
                },
                view.probe_count.get(),
            )
        });
        let (mut batches, probe_count): (Vec<_>, Vec<_>) = completed.into_iter().unzip();
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
        }
        (batches, probe_count.into_iter().sum())
    }

    /// Validates and centrally applies completed dropped-item owner batches.
    pub(crate) fn apply_item_tick_owner_batches(&mut self, batches: Vec<ItemTickOwnerBatch>) {
        if batches.is_empty() {
            assert!(
                self.items.is_empty() && self.item_state.is_empty(),
                "item owner completion must retain every live tick-start item"
            );
            return;
        }
        let plan = batches[0].plan;
        assert_eq!(
            plan, self.item_owner_plan,
            "item completion must belong to the latest tick-start plan"
        );
        assert!(
            plan > self.applied_item_owner_plan,
            "item completion must not replay an already applied tick-start plan"
        );
        let effects = merge_item_tick_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.items.len(),
            "item owner completion must retain every live tick-start lifecycle"
        );
        assert_eq!(
            effects.len(),
            self.item_state.len(),
            "item owner completion must retain every live tick-start motion state"
        );
        let mut ids = std::collections::HashSet::new();
        for effect in &effects {
            assert!(
                ids.insert(effect.id),
                "item owner completion may update one live item only once"
            );
            assert!(
                self.items.get(effect.id).is_some() && self.item_state.contains_key(&effect.id),
                "item owner completion may update only a live tick-start item"
            );
            assert_eq!(
                self.item_state
                    .get(&effect.id)
                    .expect("checked above")
                    .owner,
                effect.owner,
                "item owner completion must stop the item's tick-start owner"
            );
        }
        let transfers: Vec<_> = effects
            .iter()
            .filter_map(|effect| {
                (!effect.discard && effect.owner != effect.destination).then(|| {
                    EntityHandoffToken::new(
                        effect.id,
                        effect.serial,
                        plan,
                        effect.owner.tick_owner(),
                        effect.destination.tick_owner(),
                    )
                    .expect("a nonzero item plan names a real cross-owner transfer")
                })
            })
            .collect();
        // All sources stop before any destination is admitted. This is the
        // central barrier: workers never receive an item whose source owner is
        // still allowed to publish a later completion.
        for &token in &transfers {
            self.item_handoff
                .stop_source(token)
                .expect("item source stop must be unique and newer than the last transfer");
        }
        for &token in &transfers {
            self.item_handoff
                .acknowledge_durable_save(token, |_| true)
                .expect("item durable-save acknowledgement must precede state replacement");
        }
        for effect in effects {
            self.items.remove(effect.id);
            if effect.discard {
                self.item_state.remove(&effect.id);
                self.item_handoff.forget_entity(effect.id);
            } else {
                let mut state = effect.state;
                state.owner = effect.destination;
                self.items.spawn(effect.id, effect.lifecycle);
                self.item_state.insert(effect.id, state);
            }
        }
        // Destination admission follows the source-stop phase and the live
        // state replacement. A subsequent tick can therefore submit this
        // entity only to its newly admitted owner.
        for token in transfers {
            self.item_handoff
                .start_destination(token)
                .expect("item destination start must follow its source stop");
        }
        self.applied_item_owner_plan = plan;
    }

    /// One tick, settling dropped items against a caller-supplied solidity
    /// oracle — the live world, when the caller has one.
    ///
    /// Only the item-settling pass consults `block_state`; everything else still
    /// reads the snapshot, because mob pathfinding genuinely wants a view that
    /// does not change underneath a search in progress. Items are the opposite
    /// case: an item has to land on the block that is there *this* tick.
    ///
    /// **The oracle is a block-state *name*, not a solid/air boolean.** A name
    /// distinguishes shapes such as a bottom slab, soul sand, and a grass patch
    /// when [`LiveBlockCollision`] computes the resting surface.
    pub fn tick_with_terrain(
        &mut self,
        block_state: &(dyn Fn(i32, i32, i32) -> String + Sync),
    ) {
        let live_collision = LiveBlockCollision {
            block_state,
            probe_count: std::cell::Cell::new(0),
        };
        // Feed every mob's perception inputs before its goals run. The pass
        // supplies `nearest_player`, `temptation`, `avoid_threat`,
        // `no_action_time`, `partner_candidate`, and `parent_candidate`; the
        // subsequent goal tick evaluates `can_use` against those values.
        //
        // `no_action_time` increments before the perception pass, so each goal
        // sees the value for the current simulation tick. The seam test checks
        // that the same value reaches both the controller and the simulated mob.
        //
        // The reset check runs immediately before the increment. Reusing
        // [`check_despawn`] keeps the player-distance reset rule in one place;
        // this call reads only `reset_timer`, passes `rng_hit_800: false`, and
        // ignores `discard` because removal belongs to [`despawn_pass`].
        for m in &mut self.mobs {
            let pos = m.position();
            let nearest = self
                .players
                .iter()
                .map(|p| dist_sqr(p.perception.position, pos))
                .min_by(f64::total_cmp);
            // Player proximity is the **only** reset condition here, and
            // deliberately *not* vanilla's other one.
            //
            // Vanilla's own "check despawn" step's `else` branch does clear the timer every tick
            // for a mob that requires persistence, keyed on
            // its own "is persistence required or requires custom persistence"
            // check. Keying
            // this off `SimMob::persistent` would look like a faithful port and
            // would not be one, because that flag carries a **wider** meaning
            // here than vanilla's: `spawn_species` sets it from `!hostile`, so
            // every passive animal is `persistent` in this crate. Vanilla animals
            // are not persistence-required — they opt out of distance
            // despawning through their own "removes when far away" override returning false,
            // which the despawn check consults for
            // *discarding* and never for the timer. Only a name-tagged or
            // summoned mob takes vanilla's `else` branch.
            //
            // Including it therefore would not have been "more vanilla": it would
            // have given every cow, pig and sheep in the world a permanently open
            // idle throttle regardless of whether any player was near. Measured,
            // not reasoned — the first draft did include it, and
            // `tests/mob_sim.rs`'s
            // `no_action_time_crosses_the_seam_instead_of_staying_on_the_sim_record`
            // failed its own precondition, because its cow's counter could no
            // longer climb past 100 at all. `despawn_pass` treats `persistent` the
            // same way (an early `return true`, with no reset), so the two agree.
            //
            // Modelling vanilla's real persistence branch needs a flag that means
            // `isPersistenceRequired` and nothing else; that is a separate change
            // to what `spawn_species` records, not something to smuggle in here.
            let reset = nearest.is_some_and(|dist_sqr| {
                crate::mob_spawn::check_despawn(m.category, dist_sqr, m.no_action_time, false, true)
                    .reset_timer
            });
            if reset {
                m.no_action_time = 0;
            }
            m.no_action_time = m.no_action_time.saturating_add(1);
            // Vanilla's own shoulder-riding per-tick update's own unconditional
            // ride-cooldown-counter increment, mirrored the same way `no_action_time`
            // is above.
            m.shoulder_dismount_ticks = m.shoulder_dismount_ticks.saturating_add(1);
        }
        self.feed_perception();

        // Retain the attacked position from each record so the resolution pass
        // can identify which player, if any, receives the hit.
        let mut hits: Vec<(Option<i32>, Vec3, f32, Vec3)> = Vec::new();
        let mut detonations: Vec<(i32, Vec3)> = Vec::new();
        let mut bred: Vec<(i32, Vec3, ResourceKey)> = Vec::new();
        // Accumulated into a local and moved into
        // `self.pending_grazes` after the loop, not pushed directly — `self` is
        // mutably borrowed by `&mut self.mobs` for the whole loop, exactly as it
        // is for `hits`/`detonations`/`bred`.
        let mut grazes: Vec<(BlockPos, EatenBlock)> = Vec::new();
        let mut launches: Vec<(i32, ProjectileLaunch)> = Vec::new();
        // Self-inflicted damage requests are drained per mob and resolved
        // below, after `hits`.
        let mut self_damage: Vec<(i32, f32)> = Vec::new();
        // Idle ambient vocalisations rolled this tick — accumulated into a
        // local for the same reason `grazes`/`bred` are: `self.mobs` is
        // mutably borrowed for the whole loop.
        let mut ambient_sounds: Vec<(Vec3, crate::effects::WorldEffect)> = Vec::new();
        // Elder guardian mining-fatigue pulses rolled this tick —
        // accumulated into a local for the same reason `grazes`/`bred` are:
        // `self.mobs` is mutably borrowed for the whole loop, and reading
        // `self.players` from inside it (a *different* field) is fine, but
        // this sim still cannot push straight onto `self.pending_mining_fatigue`
        // without an extra borrow of `self` the loop already avoids for the
        // others.
        let mut mining_fatigue: Vec<MiningFatigueAura> = Vec::new();
        // Vanilla's own zombified-piglin alert-others call, resolved the same way
        // `hits`/`bred` are: accumulated into a local while `self.mobs` is
        // mutably borrowed for the per-mob loop below, then applied to the
        // rest of `self.mobs` in a second pass afterwards. Each entry is
        // (alerting piglin's own position, its live target's position) — see
        // `piglin_alert_ticks`'s own doc comment for the mechanism and the
        // disclosed target-position approximation.
        let mut piglin_alerts: Vec<(Vec3, Vec3)> = Vec::new();
        // The morning-gift request and per-tick shoulder-mount request — both
        // drained per mob the same way `bred`/`grazes` are (own mob id, since
        // resolving either needs a second look at `self.mobs`/`self.players`
        // after the per-mob loop releases its borrow).
        let mut gift_requests: Vec<i32> = Vec::new();
        let mut shoulder_requests: Vec<i32> = Vec::new();
        let tick_count = self.tick_count;
        // The disconnect self-heal for a mounted mob — the mob twin of
        // `tick_vehicles`' identical guard for a boat (see that comment for
        // why it is gated on a *non-empty* roster: `set_players` starts empty
        // before anyone has moved, and treating that as "nobody is connected"
        // would evict a rider the instant they mounted). Without this a mount
        // whose rider crashed or quit stays `Some(id)` forever and never ticks
        // its own goal AI again below.
        if !self.players.is_empty() {
            let connected: Vec<i32> = self
                .players
                .iter()
                .filter_map(|p| p.identity.map(|identity| identity.entity_id))
                .collect();
            if !connected.is_empty() {
                for mob in &mut self.mobs {
                    if mob.rider.is_some_and(|rider| !connected.contains(&rider)) {
                        mob.rider = None;
                    }
                }
            }
        }
        for m in &mut self.mobs {
            let before_live_collision = m.mob.live_collision_origin();
            // Vanilla ages `invulnerableTime`/`hurtTime` every tick regardless
            // of whether the mob was hit this tick.
            m.hurt_cooldown.tick();
            // A ridden mob's movement is client-authoritative
            // (`MobSim::apply_mob_move`), the same handover `tick_vehicles`
            // documents for an unridden boat: running goal AI here too would
            // fight the rider's own reports and produce jitter, so a mount's
            // goal selector simply does not tick while ridden.
            if m.rider.is_none() {
                m.mob.tick(&mut m.goals);
            }
            // The navigation world is intentionally a stable, bounded snapshot;
            // collision cannot be. A command-spawned mob may be far outside the
            // initial snapshot, and a player can edit its support after it was
            // made, so resolve every SimMob through the live shape oracle before
            // any subsequent per-tick consumer reads its position.
            settle_mob(&live_collision, &mut m.mob, before_live_collision, true);
            // Vanilla's own generic per-tick base update's ambient-sound roll runs every tick a
            // mob is alive, independent of any goal — see
            // `roll_ambient_sound`'s own doc.
            if m.health > 0.0 {
                if let Some(effect) = roll_ambient_sound(m, tick_count) {
                    ambient_sounds.push((m.position(), effect));
                }
            }
            // Vanilla's own zombified-piglin AI step's private alert-others call
            // — see `piglin_alert_ticks`'s own doc comment for the mechanism.
            // `attack_target()` doubles as "current live target position" per
            // that same disclosed approximation (this seam has no entity
            // reference to resolve a byte-exact live-target getter from).
            if m.health > 0.0 && m.entity_type.path() == "zombified_piglin" {
                match m.mob.attack_target() {
                    Some(_) if m.piglin_alert_ticks < 0 => {
                        m.piglin_alert_ticks = piglin_alert_interval(&mut m.mob);
                    }
                    Some(target_pos) if m.piglin_alert_ticks == 0 => {
                        let world = self.world;
                        if world.is_clear(m.position(), target_pos) {
                            piglin_alerts.push((m.position(), target_pos));
                        }
                        m.piglin_alert_ticks = piglin_alert_interval(&mut m.mob);
                    }
                    Some(_) => {
                        m.piglin_alert_ticks -= 1;
                    }
                    None => {
                        m.piglin_alert_ticks = -1;
                    }
                }
            }
            // `armadillo_danger_ticks`'s countdown — see its own doc comment.
            // A dead armadillo does not un-scare (matching every other
            // per-mob timer in this loop, which is likewise gated on
            // `health > 0.0`; a corpse's fields are frozen, not ticked).
            if m.health > 0.0 && m.armadillo_danger_ticks > 0 {
                m.armadillo_danger_ticks -= 1;
            }
            // `axolotl_play_dead_ticks`'s countdown — same "a corpse's
            // fields are frozen, not ticked" gate as the armadillo one
            // above.
            if m.health > 0.0 && m.axolotl_play_dead_ticks > 0 {
                m.axolotl_play_dead_ticks -= 1;
            }
            // `allay_liked_noteblock`'s cooldown countdown, and
            // `allay_duplication_cooldown`'s — see both fields' own docs.
            // Cleared outright at zero rather than left as `Some((pos, 0))`,
            // the disclosed simplification `allay_liked_noteblock`'s own doc
            // names.
            if m.health > 0.0 && m.entity_type.path() == "allay" {
                if let Some((pos, ticks)) = m.allay_liked_noteblock {
                    m.allay_liked_noteblock = if ticks > 1 { Some((pos, ticks - 1)) } else { None };
                }
                if m.allay_duplication_cooldown > 0 {
                    m.allay_duplication_cooldown -= 1;
                }
            }
            // A camel entering water clears its sitting pose before the random
            // sitting toggle runs. Only one of the two branches fires in a tick.
            if m.health > 0.0 && m.entity_type.path() == "camel" {
                if m.camel_sitting && m.in_water() {
                    m.camel_sitting = false;
                    m.camel_pose_tick = tick_count as i64;
                } else {
                    camel_random_sitting(m, tick_count);
                }
                // A living camel decrements its dash cooldown; corpse fields
                // remain frozen like every other per-mob timer in this loop.
                if m.camel_dash_cooldown > 0 {
                    m.camel_dash_cooldown -= 1;
                }
            }
            let new_attacks = m.mob.take_new_attacks();
            // A bee's sting connects when its attack event is emitted. Only the
            // first sting matters; clearing `anger` here prevents reacquisition.
            if !new_attacks.is_empty() && m.stung_at.is_none() && m.entity_type.path() == "bee" {
                m.stung_at = Some(tick_count);
                m.anger = None;
            }
            for target_pos in new_attacks {
                // Carry the attacker's position so the victim can retaliate and
                // identify the source of the hit.
                hits.push((m.attack_target_id, target_pos, m.attack_damage, m.position()));
            }
            // A stung bee's self-destruct roll — see
            // `bee_sting_death_roll` for the exact formula
            // and its two derived bounds (certainly alive at sting+1,
            // certainly dead by sting+1200).
            if let Some(stung_at) = m.stung_at {
                let elapsed = tick_count.saturating_sub(stung_at);
                if elapsed > 0 && elapsed % 5 == 0 && bee_sting_death_roll(tick_count, m.id, elapsed)
                {
                    // A fixed, large amount rather than `m.health`: this is a
                    // lethal roll, not a graded hit, and `apply_damage`'s
                    // reductions (armour, absorption) must not be able to
                    // leave a "certainly dead" tick non-lethal.
                    m.mob.damage_self(10_000.0);
                }
            }
            if m.mob.take_detonated() {
                detonations.push((m.id, m.position()));
            }
            // Drain the breeding flag. The mob controller records the event;
            // this driver resolves it into a child because it owns the entity
            // registry and the partner-independent spawn decision.
            if m.mob.take_bred() {
                bred.push((m.id, m.position(), m.entity_type().clone()));
            }
            // Same one-shot-flag drain shape as `take_bred` above.
            if m.mob.take_gift_requested() {
                gift_requests.push(m.id);
            }
            if m.mob.take_shoulder_ride_requested() {
                shoulder_requests.push(m.id);
            }
            // The goal records *that* a block was eaten and which of
            // the two positions it was; it cannot mutate the world, because this
            // sim borrows `world: &'w ChunkWorld` immutably. So this takes the
            // same route `pending_detonations` does — accumulate here, and let
            // `crate::tick::run_tick_loop` (which owns mutable chunk access)
            // apply it. The tick loop owns mutable chunk access, so the
            // simulation records the event and the loop performs the write.
            for what in m.mob.take_new_eaten() {
                grazes.push((m.mob.block_position(), what));
            }
            // Paired with the launching mob's id so the impact pass can exclude
            // it: a projectile is created inside its shooter's own bounding box,
            // so without an owner a skeleton's arrow hits the skeleton.
            launches.extend(m.mob.take_new_launches().into_iter().map(|l| (m.id, l)));
            for amount in m.mob.take_self_damage() {
                self_damage.push((m.id, amount));
            }
            // Villager gossip decays on a 24000-tick cadence. `None` records
            // the first timestamp without applying decay.
            if m.entity_type.path() == "villager" {
                match m.last_gossip_decay_tick {
                    None => m.last_gossip_decay_tick = Some(tick_count),
                    Some(last) if tick_count >= last + 24000 => {
                        m.gossip.decay();
                        m.last_gossip_decay_tick = Some(tick_count);
                    }
                    Some(_) => {}
                }
            }
            // Advance a zombie-villager conversion countdown.
            if m.entity_type.path() == "zombie_villager"
                && let Some(mut state) = m.conversion
            {
                let pos = m.position();
                let world = self.world;
                let progress = villager::conversion::conversion_progress(
                    || self.zombie_conversion_rng.next_f32(),
                    || villager::conversion::count_nearby_special_blocks(world, pos),
                );
                state.remaining_ticks -= progress;
                if state.remaining_ticks <= 0 {
                    // Conversion changes the species-derived combat stats,
                    // category, and gossip seed; profession, level, and XP are
                    // already fields on `SimMob`.
                    m.set_entity_type(
                        ResourceKey::from_str("minecraft:villager").expect("static key"),
                    );
                    m.category = MobCategory::Creature;
                    let (max_health, attack_damage, defenses, knockback_resistance) =
                        combat_defaults(&m.entity_type);
                    m.max_health = max_health;
                    m.health = m.health.min(max_health);
                    m.attack_damage = attack_damage;
                    m.defenses = defenses;
                    m.knockback_resistance = knockback_resistance;
                    if let Some(starter) = state.starter {
                        villager::reputation::apply_reputation_event(
                            &mut m.gossip,
                            villager::reputation::ReputationEventType::ZombieVillagerCured,
                            starter,
                        );
                    }
                    // Conversion applies nausea for 200 ticks at amplifier 0.
                    // It is a timed effect visible through `SimMob::effects()`.
                    m.effects.apply("minecraft:nausea", 200, 0);
                    m.conversion = None;
                    let block_pos = BlockPos::new(
                        pos.x.floor() as i32,
                        pos.y.floor() as i32,
                        pos.z.floor() as i32,
                    );
                    ambient_sounds.push((pos, crate::effects::WorldEffect::LevelEvent {
                        event: crate::effects::SOUND_ZOMBIE_CONVERTED,
                        pos: block_pos,
                        data: 0,
                        global: false,
                    }));
                } else {
                    m.conversion = Some(state);
                }
            }
            // Emit the elder-guardian mining-fatigue pulse on its periodic
            // interval. `tick_count` is the simulation clock for this schedule.
            if m.entity_type.path() == "elder_guardian"
                && tick_count.wrapping_add(m.id as u64) % ELDER_GUARDIAN_EFFECT_INTERVAL == 0
            {
                let source_pos = m.position();
                for player in &self.players {
                    let Some(identity) = player.identity else {
                        continue;
                    };
                    let delta = source_pos - player.perception.position;
                    if delta.dot(delta)
                        < ELDER_GUARDIAN_EFFECT_RADIUS * ELDER_GUARDIAN_EFFECT_RADIUS
                    {
                        mining_fatigue.push(MiningFatigueAura { target: identity });
                    }
                }
            }
        }
        self.push_entities();
        self.pending_grazes.extend(grazes);
        self.pending_ambient_sounds.extend(
            ambient_sounds.into_iter().map(|(source, effect)| PendingEntityTickEffect {
                owner: entity_tick_owner(source),
                source,
                effect,
            }),
        );
        self.pending_mining_fatigue.extend(mining_fatigue);
        // Propagate zombified-piglin alerts after the per-mob loop releases
        // each `SimMob` borrow. The shared box from
        // `alert_species("zombified_piglin")` bounds the one-shot pack alert,
        // and mobs with an existing grudge keep their current target.
        if let Some((box_xz, box_y, _)) = alert_species("zombified_piglin") {
            for (source_pos, target_pos) in piglin_alerts {
                for other in &mut self.mobs {
                    if other.entity_type.path() != "zombified_piglin" || other.anger.is_some() {
                        continue;
                    }
                    let p = other.position();
                    if (p.x - source_pos.x).abs() > box_xz
                        || (p.z - source_pos.z).abs() > box_xz
                        || (p.y - source_pos.y).abs() > box_y
                    {
                        continue;
                    }
                    other.anger = Some(Anger {
                        end_time: tick_count + grudge_ticks(&mut other.mob),
                        target: target_pos,
                    });
                }
            }
        }
        for (shooter, launch) in launches {
            use lodestone_entity::ai::roster::ranged::{integrates_as_arrow, projectile_entity_type};
            let projectile = if integrates_as_arrow(launch.kind) {
                Projectile::arrow(launch.origin, launch.velocity)
            } else {
                Projectile::throwable(launch.origin, launch.velocity)
            };
            let key = ResourceKey::from_str(&format!("minecraft:{}", projectile_entity_type(launch.kind)))
                .expect("static projectile key");
            self.spawn_projectile_from(key, projectile, Some(shooter));
        }
        for (target_id, target_pos, raw_damage, attacker_pos) in hits {
            if let Some(target_id) = target_id
                && let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id)
            {
                let applied = target.apply_damage(raw_damage, DamageFlags::default());
                target.mob.note_hurt(Some(attacker_pos));
                self.note_vocalisation(target_id, applied);
                continue;
            }
            // `attack_target_id` identifies another `SimMob`; player targets
            // arrive as a position with `target_id == None`. Match that
            // position against the player registry because this event carries
            // no player identity. The exact comparison is documented by
            // `PlayerHit`, including the possible stale-target miss.
            if let Some(identity) = self
                .players
                .iter()
                .find(|p| dist_sqr(p.perception.position, target_pos) < 1e-6)
                .and_then(|p| p.identity)
            {
                self.pending_player_hits.push(PlayerHit {
                    identity,
                    raw_damage,
                    attacker_pos,
                });
                // A tamed pet retaliates against the source that hurt its
                // owner. The event carries the attacker's position rather than
                // an entity identity, so each owned pet records that position
                // for its next target selection.
                for pet in &mut self.mobs {
                    if pet.owner_uuid() == Some(identity.uuid)
                        && pet.is_tame()
                        && pet.health() > 0.0
                    {
                        pet.mob.set_owner_hurt_by(Some(attacker_pos));
                    }
                }
            }
        }
        // Self-inflicted damage from a bee's sting self-destruct. `damage_self` only
        // records the intent; health lives here, so it is applied through the
        // same pipeline as a melee hit (invulnerability and armour reductions
        // included). Resolve it before retaining live mobs so a fatal event
        // removes its mob in the same tick.
        for (id, amount) in self_damage {
            if let Some(m) = self.get_mut(id) {
                let applied = m.apply_damage(amount, DamageFlags::default());
                self.note_vocalisation(id, applied);
            }
        }
        self.reap_dead();
        self.resolve_breeding(bred);
        // Drain the cat morning-gift roll/spawn and parrot shoulder-mount
        // request collected alongside `bred`.
        self.resolve_cat_gifts(gift_requests);
        self.resolve_shoulder_mounts(shoulder_requests);
        self.tick_shoulder_dismounts();

        // A detonation removes the initiating mob explicitly after applying
        // blast damage. This keeps self-removal independent of whether terrain
        // shields the mob from its own blast.
        for (id, pos) in detonations {
            self.explode(pos, CREEPER_EXPLOSION_RADIUS, DamageFlags::default());
            self.mobs.retain(|m| m.id != id);
            // Record the detonation separately from damage so a connected client
            // can receive the explosion event (particle and sound handling) even
            // when the blast does not damage an entity. See `take_detonations`'s
            // doc comment for the drain side.
            self.pending_detonations.push(Detonation {
                centre: pos,
                radius: CREEPER_EXPLOSION_RADIUS,
            });
        }

        // Advance both registries from this shared tick. Resolve projectile
        // impacts before motion so the swept segment is clipped at the first
        // collision; moving first would place impacts one tick late and could
        // carry an arrow through a wall.
        self.resolve_projectile_impacts();
        let projectile_batches = self.tick_projectile_owner_batches();
        self.apply_projectile_tick_owner_batches(projectile_batches);
        // **items land.** `ItemMotion::tick` is the entity's own
        // motion — gravity, translate, drag — and its doc comment has always said
        // "block collision that would zero a component is the world crate's job
        // and is expressed here through `on_ground`". Nothing ever did that job:
        // `on_ground` was set `false` by `ItemMotion::new` and never written
        // again, so every dropped item accelerated downward forever, fell through
        // the terrain, and streamed to the client until its 6000-tick despawn.
        //
        // That is also why merging never happened. `merge_neighbouring_items`
        // requires `|dy| < ITEM_MERGE_REACH_Y` (0.25), and two stacks dropped even
        // one tick apart fall at permanently different speeds — so the vertical
        // test could never pass for anything but two items spawned on the same
        // tick. Settling them onto a surface is what makes the merge reachable,
        // which is why the item lifecycle and inventory handoff are one operation.
        let (item_batches, item_probe_count) = self.tick_item_owner_batches(block_state);
        self.apply_item_tick_owner_batches(item_batches);
        // **The cost of the sweep, as a counter rather than a duration.** Swept
        // collision against real shapes is strictly more work per item than one
        // boolean lookup was, and the number of items in one tick is unbounded — so
        // the thing worth measuring is not per-item cost but how much of one tick a
        // floor covered in drops can consume. A counter is what a gate can assert
        // and what survives being read on a loaded machine; see
        // `items_settled_probe_count`.
        self.item_probe_count = item_probe_count;
        self.merge_neighbouring_items();
        // Experience orbs, on the same live-terrain oracle the items above use and for
        // the same reason: an orb settled against the sim's static `ChunkWorld`
        // snapshot would fall through any block the player has placed and rest on any
        // block they have mined. `tick_orbs` reads `tick_count` for its merge phase, so
        // it runs before the increment below.
        self.tick_orbs(block_state);
        // A fireball's `ignite_seconds` used to reach nothing: computed by
        // `lodestone_entity::projectile::impact_effect` and read by no
        // production caller. `resolve_projectile_impacts` above is what can
        // raise a mob's burn counter (through `resolve_projectile_hit`); this
        // is the consumption half, run every tick regardless of whether a
        // fireball landed this one.
        self.tick_burning();
        self.tick_leashes();
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_professions();
        // villager bed claiming — see `tick_villager_beds`'s own
        // doc for why this is a separate memory from the job site above.
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_beds();
        // villager bell claiming — see `tick_villager_bells`'s
        // own doc for why this is a third, independent memory from the job
        // site and bed above.
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_bells();
        // fishing bobbers. Reads `self.world` (the static
        // per-tick terrain snapshot, not the live `view` oracle the item/orb
        // passes just above use) — see `fishing::MobSim::tick_fishing_bobbers`'s
        // own doc for why a bobber's whole interesting life is spent sitting
        // in open water, where the two oracles agree.
        self.tick_fishing_bobbers();
        // Raids. Wave spawning and victory/defeat need no live
        // terrain oracle either — see `raid::MobSim::tick_raids`'s own doc.
        self.tick_raids();
        // Nearby-villager gossip spread. No `wasm32` gate — unlike
        // `tick_villager_professions`, this touches no `std::fs`-backed type.
        self.spread_villager_gossip();
        // Golem-summon-on-hurt. No `wasm32` gate, for
        // `spread_villager_gossip`'s own reason.
        self.tick_golem_summon();
        // The cat's chest/lit-furnace/bed candidate search.
        self.tick_cat_block_search();
        // Allay item-carry-and-deliver — pick up matching ground
        // items, then throw one at a live delivery target. Order matters:
        // an item picked up this very tick could in principle also be
        // delivered this tick if the allay is already standing at its
        // target, matching vanilla's own same-tick pickup-then-throw
        // possibility rather than an arbitrary one-tick lag.
        self.allay_pick_up_items();
        self.allay_deliver_items();
        // The vibration substrate. Last, so every producer
        // earlier in this tick (currently just `reap_dead`'s `entity_die`)
        // has already posted before a listener resolves its nearest answer.
        self.resolve_vibrations();
        // Turns this tick's `nearest_vibration` answer
        // into real warden anger and, once angry and in range, a real melee
        // hit — see `warden::MobSim::resolve_warden_anger`'s own doc.
        self.resolve_warden_anger();
        // The sniffer's own seek/dig/
        // rise/egg-drop state machine — see `sniffer::MobSim::tick_sniffers`'s
        // own doc. No particular ordering dependency on the calls above;
        // placed last alongside the warden consumer as the other
        // per-species host-side driver this tick runs.
        self.tick_sniffers();

        // Combat, leashes, crowd push, and warden effects can apply an impulse
        // after the AI loop's first sweep. Resolve that final movement before
        // snapshots are published so no producer gets a one-tick collision
        // bypass merely because it ran later in the tick order.
        for mob in &mut self.mobs {
            let before_live_collision = mob.mob.live_collision_origin();
            // This pass only resolves motion added after the main AI sweep. Do
            // not integrate gravity twice when a mined floor left a mob
            // unsupported: the first pass already did that for this tick.
            settle_mob(&live_collision, &mut mob.mob, before_live_collision, false);
        }

        self.tick_count += 1;
    }

    /// Shoves apart entities whose bodies overlap — vanilla's own generic
    /// entity-push step,
    /// invoked once per tick for every pushable neighbour by
    /// vanilla's own generic living-entity "push entities" step (queries
    /// pushable neighbours,
    /// then pushes each in turn), called near the end of
    /// vanilla's own generic living-entity per-tick base update, after that tick's own movement has already
    /// been applied — the same ordering `tick_with_terrain` gives this call,
    /// right after the per-mob loop that runs `m.mob.tick(...)`.
    ///
    /// # The formula lives in `lodestone-physics`, not here
    ///
    /// [`push_impulse`] delegates to [`lodestone_physics::pair_push_vector`],
    /// which already carries the full citation of vanilla's own generic entity-push step —
    /// see `docs/entity-push.md` for the derivation, including the
    /// genuinely-non-obvious `sqrt(max(|dx|,|dz|))` Chebyshev normaliser
    /// (not `sqrt(dx²+dz²)`) and the widened `0.01f`/`0.05f` literals. That
    /// module is otherwise **unwired** — `docs/entity-push.md`'s own "Wiring"
    /// section says nothing in `lodestone-shell`, `lodestone-ecs` or
    /// `lodestone-client` calls it yet, because its documented use case is
    /// the *client-authoritative local player* feeling a push from nearby
    /// entities, which is a different half of vanilla's symmetric rule from
    /// this one: a *server-authoritative mob* being shoved by a player or
    /// another mob. This call site is the first production consumer.
    ///
    /// # What this port narrows, disclosed
    ///
    /// * **Overlap is a horizontal-distance-under-combined-half-width test**,
    ///   not vanilla's real AABB intersection
    ///   (its own entity-query/pushable-neighbour helpers), which also accounts for
    ///   height overlap. Two mobs stacked exactly on top of one another with
    ///   no horizontal offset therefore push in this port and would not
    ///   collide in vanilla (their Y ranges might not overlap) — an edge case
    ///   this seam's `lodestone_entity::pathfinding::PathWorld`/`SimMob` do not
    ///   carry enough geometry to
    ///   resolve exactly.
    /// * **Applied once per pair per tick**, not vanilla's twice (each side's
    ///   own "push entities" step invokes a push against the other, so a
    ///   living pair receives the impulse from *both* directions every tick).
    ///   The formula itself is unchanged; this halves the net closing-speed
    ///   reduction relative to vanilla's double application, a scope cut
    ///   rather than a transcription error.
    /// * **Player recoil is not applied.** A player's own position/velocity
    ///   in this codebase is client-authoritative (the client sends
    ///   `move_player_pos`; the server does not own a player's velocity the
    ///   way it owns a mob's), so shoving a player back needs a clientbound
    ///   self-velocity packet the client applies to its own physics —
    ///   `crates/protocol/**` and `crates/lodestone-shell/**`, both outside
    ///   this crate. This pass pushes the **mob** away from an intersecting
    ///   player (the reported "I can't push pigs" symptom: the pig now moves
    ///   out of the way), but the player itself is not nudged.
    /// * **"is pushable"/vehicle/passenger exclusions are not modelled** —
    ///   every [`SimMob`] is treated as pushable, matching vanilla's default
    ///   for a plain living entity with nothing riding it.
    /// * **Mount cramming damage is not modelled** — vanilla's own
    ///   `maxEntityCramming` gamerule check in the same method.
    fn push_entities(&mut self) {
        let batches = self.tick_entity_push_owner_batches();
        self.apply_entity_push_owner_batches(batches);
    }

    /// Computes one deferred impulse for every tick-start mob and groups the
    /// results by source chunk. Cross-owner pairs are read-only here; no mob
    /// receives an impulse until the central apply step validates the plan.
    pub(crate) fn tick_entity_push_owner_batches(&self) -> Vec<EntityPushOwnerBatch> {
        #[cfg(not(target_arch = "wasm32"))]
        let workers = if self.mobs.len() >= 128 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
                .min(4)
        } else {
            1
        };
        #[cfg(target_arch = "wasm32")]
        let workers = 1;
        self.tick_entity_push_owner_batches_with_workers(workers)
    }

    pub(super) fn tick_entity_push_owner_batches_with_workers(
        &self,
        worker_count: usize,
    ) -> Vec<EntityPushOwnerBatch> {
        let n = self.mobs.len();
        if n == 0 {
            return Vec::new();
        }
        let positions: Vec<Vec3> = self.mobs.iter().map(SimMob::position).collect();
        let widths: Vec<f64> = self
            .mobs
            .iter()
            .map(|m| f64::from(m.shape().width))
            .collect();
        let ids: Vec<i32> = self.mobs.iter().map(|mob| mob.id).collect();
        let mut jobs = Vec::<(EntityTickOwner, Vec<usize>)>::new();
        for (serial, position) in positions.iter().copied().enumerate() {
            let owner = entity_tick_owner(position);
            if let Some((_, serials)) = jobs.iter_mut().find(|(candidate, _)| *candidate == owner) {
                serials.push(serial);
            } else {
                jobs.push((owner, vec![serial]));
            }
        }
        let players = &self.players;
        let mut batches = crate::tick_region::run_bounded_owner_jobs(jobs, worker_count, &|(owner, serials)| {
            let effects = serials
                .into_iter()
                .map(|serial| {
                    let mut impulse = Vec3::default();
                    for other in 0..n {
                        if other == serial {
                            continue;
                        }
                        let touch = (widths[serial] + widths[other]) / 2.0;
                        let contribution = if serial < other {
                            push_impulse(positions[serial], positions[other], touch)
                                .map(|pair| pair.0)
                        } else {
                            push_impulse(positions[other], positions[serial], touch)
                                .map(|pair| pair.1)
                        };
                        if let Some(contribution) = contribution {
                            impulse.x += contribution.x;
                            impulse.z += contribution.z;
                        }
                    }
                    // Player recoil remains client-authoritative; only the mob
                    // half is accumulated into this owner's completion.
                    const PLAYER_WIDTH: f64 = 0.6;
                    for player in players {
                        let touch = (widths[serial] + PLAYER_WIDTH) / 2.0;
                        if let Some((mob_impulse, _)) =
                            push_impulse(positions[serial], player.perception.position, touch)
                        {
                            impulse.x += mob_impulse.x;
                            impulse.z += mob_impulse.z;
                        }
                    }
                    EntityPushEffect {
                        owner,
                        serial,
                        id: ids[serial],
                        impulse,
                    }
                })
                .collect();
            EntityPushOwnerBatch {
                owner,
                expected_batch_count: 0,
                effects,
            }
        });
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
        }
        batches
    }

    /// Validates and centrally applies completed entity-push owner batches.
    pub(crate) fn apply_entity_push_owner_batches(&mut self, batches: Vec<EntityPushOwnerBatch>) {
        if batches.is_empty() {
            assert!(
                self.mobs.is_empty(),
                "entity-push completion must retain every live tick-start mob"
            );
            return;
        }
        let effects = merge_entity_push_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.mobs.len(),
            "entity-push completion must retain every live tick-start mob"
        );
        for (mob, effect) in self.mobs.iter().zip(&effects) {
            assert_eq!(
                mob.id, effect.id,
                "entity-push completion must retain the tick-start entity order"
            );
        }
        for (mob, effect) in self.mobs.iter_mut().zip(effects) {
            if effect.impulse.x != 0.0 || effect.impulse.z != 0.0 {
                mob.apply_knockback(effect.impulse);
            }
        }
    }

    /// Advances every mob's burn counter one tick and applies the damage it
    /// reports — the consumption half of vanilla's own generic per-tick base update's fire section
    /// (see `crate::burning`'s own module doc for the full mechanic), scoped
    /// to what actually reaches a mob today.
    ///
    /// **What ignites a mob**: only a fireball/wither-skull impact
    /// ([`MobSim::resolve_projectile_hit`]) currently raises the counter —
    /// this pass never itself ignites anything. Standing in a fire or lava
    /// block does **not** ignite a mob here; that half of `baseTick` is a
    /// disclosed gap (`crate::burning`'s own "What is not here" section
    /// already named "mob burning" as unwired at all — this closes the
    /// consumption half, not the block-contact ignition half).
    ///
    /// **What puts it out**: water contact only, read through
    /// [`SimMob::in_water`] (vanilla's own per-tick base update's water-block fire-clear call).
    /// Fire immunity ([`species::is_fire_immune`]) clears the counter outright
    /// rather than merely refusing damage, matching
    /// [`crate::burning::BurnState::tick`]'s own `fire_immune` handling.
    /// `standing_in` is always `None` (no fire/lava contact modelled for
    /// mobs), so the lava-guard and per-block contact-damage halves of
    /// [`crate::burning::BurnState::tick`] never fire from this call site —
    /// only the every-20-ticks burn tick itself does.
    fn tick_burning(&mut self) {
        let batches = self.tick_burning_owner_batches();
        self.apply_burning_owner_batches(batches);
    }

    /// Plans burn-counter changes under each mob's tick-start chunk owner.
    pub(crate) fn tick_burning_owner_batches(&mut self) -> Vec<BurnTickOwnerBatch> {
        self.burn_owner_plan = self
            .burn_owner_plan
            .checked_add(1)
            .expect("burn owner plan generation must not overflow");
        #[cfg(not(target_arch = "wasm32"))]
        let workers = if self.mobs.len() >= 128 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
                .min(4)
        } else {
            1
        };
        #[cfg(target_arch = "wasm32")]
        let workers = 1;
        self.tick_burning_owner_batches_with_workers(workers)
    }

    pub(super) fn tick_burning_owner_batches_with_workers(
        &self,
        worker_count: usize,
    ) -> Vec<BurnTickOwnerBatch> {
        let mut jobs = Vec::<(EntityTickOwner, Vec<BurnTickInput>)>::new();
        for (serial, m) in self.mobs.iter().enumerate() {
            let owner = entity_tick_owner(m.position());
            let input = BurnTickInput {
                owner,
                serial,
                id: m.id,
                burn: m.burn,
                in_water: m.in_water(),
                fire_immune: species::is_fire_immune(&m.entity_type),
                fire_resistance: m.effects.get("minecraft:fire_resistance").is_some(),
            };
            if let Some((_, inputs)) = jobs.iter_mut().find(|(candidate, _)| *candidate == owner) {
                inputs.push(input);
            } else {
                jobs.push((owner, vec![input]));
            }
        }
        let plan = self.burn_owner_plan;
        let mut batches = crate::tick_region::run_bounded_owner_jobs(
            jobs,
            worker_count,
            &|(owner, inputs)| {
                let effects = inputs
                    .into_iter()
                    .map(|input| {
                        let mut burn = input.burn;
                        let damage = if input.in_water {
                            burn.clear();
                            0.0
                        } else {
                            burn.tick(None, input.fire_immune, input.fire_resistance).damage
                        };
                        BurnTickEffect {
                            owner: input.owner,
                            serial: input.serial,
                            id: input.id,
                            burn,
                            damage,
                        }
                    })
                    .collect();
                BurnTickOwnerBatch {
                    owner,
                    plan,
                    expected_batch_count: 0,
                    effects,
                }
            },
        );
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
        }
        batches
    }

    /// Validates all burn-owner completions before mutating live mob state.
    pub(crate) fn apply_burning_owner_batches(&mut self, batches: Vec<BurnTickOwnerBatch>) {
        if batches.is_empty() {
            assert!(
                self.mobs.is_empty(),
                "burn completion must retain every live tick-start mob"
            );
            return;
        }
        let plan = batches[0].plan;
        assert_eq!(
            plan, self.burn_owner_plan,
            "burn completion must belong to the latest tick-start plan"
        );
        assert!(
            plan > self.applied_burn_owner_plan,
            "burn completion must not replay an already applied tick-start plan"
        );
        let effects = merge_burn_tick_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.mobs.len(),
            "burn completion must retain every live tick-start mob"
        );
        for (m, effect) in self.mobs.iter().zip(&effects) {
            assert_eq!(
                m.id, effect.id,
                "burn completion must retain the tick-start entity order"
            );
        }
        let mut hits: Vec<(i32, f32)> = Vec::new();
        for (m, effect) in self.mobs.iter_mut().zip(effects) {
            m.burn = effect.burn;
            if effect.damage > 0.0 {
                hits.push((effect.id, effect.damage));
            }
        }
        for (id, damage) in hits {
            if let Some(m) = self.get_mut(id) {
                let applied = m.apply_damage(
                    damage,
                    DamageFlags::for_damage_type_name("minecraft:on_fire").unwrap_or_default(),
                );
                self.note_vocalisation(id, applied);
            }
        }
        self.reap_dead();
        self.applied_burn_owner_plan = plan;
    }

    /// Per-tick leash physics: pull leashed mobs toward their holder, and
    /// snap (dropping a lead item) past [`LEASH_TOO_FAR_DIST`] — vanilla's
    /// own leash per-tick update.
    ///
    /// **Simplified, and disclosed rather than silent.** Real vanilla
    /// computes a spring/torque interaction across up to four
    /// attachment-point pairs and applies angular momentum to yaw
    /// (its own elastic-interaction check/computation).
    /// This applies one straight-line impulse toward the holder's position
    /// instead, through [`SimMob::apply_knockback`] — the same "hand
    /// velocity application to the physics owner rather than growing a
    /// second model here" seam `explosion.rs`/`damage.rs` already use for
    /// combat knockback. Three things this does not carry:
    ///
    /// - No yaw torque (vanilla's own angular-momentum field and yaw setter).
    /// - **No per-entity bounding-box subtraction from the elastic
    ///   threshold** — vanilla's actual pull distance is
    ///   the elastic distance minus both entities' own bounding-box widths;
    ///   this uses the flat [`LEASH_ELASTIC_DIST`] constant, so a very wide
    ///   mob starts pulling slightly later than vanilla would.
    /// - **A holder that cannot be resolved this tick silently drops the
    ///   leash with no item spawned** — vanilla's own "cannot interact with
    ///   level" check's
    ///   branch, narrowed to its entity-drops-off arm (its own remove-leash path)
    ///   only. A disconnected player or a removed leash-holder mob loses the
    ///   leashed mob's attachment rather than the mob dropping a lead for a
    ///   holder that is not really gone (a reconnecting player, in
    ///   particular) — the safer of the two wrong answers, but still a
    ///   simplification worth naming.
    pub(super) fn tick_leashes(&mut self) {
        let batches = self.tick_leash_owner_batches();
        self.apply_leash_owner_batches(batches);
    }

    /// Resolves holder positions from a shared tick-start census and groups
    /// the resulting leash decisions by the leashed mob's source chunk.
    pub(crate) fn tick_leash_owner_batches(&mut self) -> Vec<LeashTickOwnerBatch> {
        self.leash_owner_plan = self
            .leash_owner_plan
            .checked_add(1)
            .expect("leash owner plan generation must not overflow");
        #[cfg(not(target_arch = "wasm32"))]
        let workers = if self.mobs.iter().filter(|mob| mob.leash_holder.is_some()).count() >= 256 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
                .min(4)
        } else {
            1
        };
        #[cfg(target_arch = "wasm32")]
        let workers = 1;
        self.tick_leash_owner_batches_with_workers(workers)
    }

    pub(super) fn tick_leash_owner_batches_with_workers(
        &self,
        worker_count: usize,
    ) -> Vec<LeashTickOwnerBatch> {
        let mob_positions: Vec<_> = self
            .mobs
            .iter()
            .map(|mob| (mob.id, mob.position()))
            .collect();
        let player_positions: Vec<_> = self
            .players
            .iter()
            .filter_map(|player| player.identity.map(|identity| (identity.uuid, player.perception.position)))
            .collect();
        let mut jobs = Vec::<(EntityTickOwner, Vec<LeashTickInput>)>::new();
        for (serial, mob) in self.mobs.iter().enumerate() {
            let Some(holder) = mob.leash_holder else {
                continue;
            };
            let position = mob.position();
            let owner = entity_tick_owner(position);
            let input = LeashTickInput {
                owner,
                serial,
                id: mob.id,
                position,
                holder,
            };
            if let Some((_, inputs)) = jobs.iter_mut().find(|(candidate, _)| *candidate == owner) {
                inputs.push(input);
            } else {
                jobs.push((owner, vec![input]));
            }
        }
        let plan = self.leash_owner_plan;
        let mut batches = crate::tick_region::run_bounded_owner_jobs(jobs, worker_count, &|(owner, inputs)| {
            let effects = inputs
                .into_iter()
                .map(|input| {
                    let holder_pos = match input.holder {
                        LeashHolder::Player(uuid) => player_positions
                            .iter()
                            .find(|(candidate, _)| *candidate == uuid)
                            .map(|(_, position)| *position),
                        LeashHolder::Mob(id) => mob_positions
                            .iter()
                            .find(|(candidate, _)| *candidate == id)
                            .map(|(_, position)| *position),
                        LeashHolder::Fence(pos) => Some(Vec3::new(
                            f64::from(pos.x) + 0.5,
                            f64::from(pos.y) + 0.5,
                            f64::from(pos.z) + 0.5,
                        )),
                    };
                    let action = match holder_pos {
                        None => LeashTickAction::Orphan,
                        Some(holder_pos) => {
                            let distance = dist_sqr(input.position, holder_pos).sqrt();
                            if distance > LEASH_TOO_FAR_DIST {
                                LeashTickAction::Snap(input.position)
                            } else if distance > LEASH_ELASTIC_DIST {
                                let excess = distance - LEASH_ELASTIC_DIST;
                                let dir = Vec3::new(
                                    (holder_pos.x - input.position.x) / distance,
                                    (holder_pos.y - input.position.y) / distance,
                                    (holder_pos.z - input.position.z) / distance,
                                );
                                let pull = excess.min(1.0) * 0.3;
                                LeashTickAction::Pull(Vec3::new(
                                    dir.x * pull,
                                    dir.y * pull,
                                    dir.z * pull,
                                ))
                            } else {
                                LeashTickAction::Keep
                            }
                        }
                    };
                    LeashTickEffect {
                        owner: input.owner,
                        serial: input.serial,
                        id: input.id,
                        action,
                    }
                })
                .collect();
            LeashTickOwnerBatch {
                owner,
                plan,
                expected_batch_count: 0,
                expected_effect_count: 0,
                effects,
            }
        });
        let batch_count = batches.len();
        let effect_count = batches.iter().map(|batch| batch.effects.len()).sum();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
            batch.expected_effect_count = effect_count;
        }
        batches
    }

    /// Validates every leash-owner completion before applying decisions in the
    /// original mob-vector order.
    pub(crate) fn apply_leash_owner_batches(&mut self, batches: Vec<LeashTickOwnerBatch>) {
        if batches.is_empty() {
            assert!(
                self.mobs.iter().all(|mob| mob.leash_holder.is_none()),
                "leash completion must retain every tick-start leash"
            );
            return;
        }
        let plan = batches[0].plan;
        assert_eq!(
            plan, self.leash_owner_plan,
            "leash completion must belong to the latest tick-start plan"
        );
        assert!(
            plan > self.applied_leash_owner_plan,
            "leash completion must not replay an already applied tick-start plan"
        );
        let effects = merge_leash_tick_owner_batches(batches);
        for effect in &effects {
            assert_eq!(
                self.mobs.get(effect.serial).map(|mob| mob.id),
                Some(effect.id),
                "leash completion must retain the tick-start entity order"
            );
        }
        for effect in effects {
            match effect.action {
                LeashTickAction::Keep => {}
                LeashTickAction::Orphan => {
                    self.mobs[effect.serial].set_leash_holder(None);
                }
                LeashTickAction::Pull(impulse) => {
                    self.mobs[effect.serial].apply_knockback(impulse);
                }
                LeashTickAction::Snap(pos) => {
                    self.mobs[effect.serial].set_leash_holder(None);
                    self.spawn_item(
                        "minecraft:lead".parse().expect("valid key"),
                        pos,
                        Vec3::new(0.0, 0.0, 0.0),
                        lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                            1,
                            lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                        ),
                    );
                }
            }
        }
        self.applied_leash_owner_plan = plan;
    }

}
