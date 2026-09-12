//! Event drains, explosion handling, and wire-facing event queues.

use super::*;

impl<'w> MobSim<'w> {
    /// Drains and returns every [`Detonation`] [`tick`](Self::tick) has
    /// triggered since the last call — the handoff
    /// [`crate::tick::run_tick_loop`] uses to publish onto an
    /// [`crate::tick::ExplosionFeed`] every server tick, mirroring how
    /// [`items`](Self::item_count)' own despawn ids are drained rather than
    /// merely read. Draining (not just reading) is what keeps a detonation
    /// from being broadcast twice if a caller is slow to call this before
    /// the next [`tick`](Self::tick) runs.
    pub fn take_detonations(&mut self) -> Vec<Detonation> {
        std::mem::take(&mut self.pending_detonations)
    }

    /// Drains every hurt/death sound recorded since the last call.
    ///
    /// Drained rather than read for [`take_detonations`](Self::take_detonations)'
    /// reason — a slow consumer must not play the same hit twice.
    pub fn take_vocalisations(&mut self) -> Vec<crate::effects::WorldEffect> {
        std::mem::take(&mut self.pending_vocalisations)
    }

    /// Drains every idle ambient vocalisation [`tick`](Self::tick) has rolled
    /// since the last call — [`take_vocalisations`](Self::take_vocalisations)'
    /// periodic sibling. Drained for the same reason: a slow consumer must not
    /// replay the same moo twice.
    pub fn take_ambient_sounds(&mut self) -> Vec<crate::effects::WorldEffect> {
        let mut effects: Vec<_> = self
            .take_ambient_sound_effect_batches()
            .into_iter()
            .flat_map(|batch| batch.effects)
            .collect();
        effects.sort_unstable_by_key(|effect| effect.sequence);
        effects.into_iter().map(|effect| effect.effect).collect()
    }

    /// Drains this tick's ambient entity-effect phase as deterministic
    /// chunk-owner batches.
    ///
    /// The producer still simulates entities serially. The batching step is
    /// intentionally after that real phase: each owner hands effects to the
    /// central world publisher instead of publishing from an owner. Effects
    /// carry their former serial sequence so the publisher can preserve parity
    /// even when two owners' entities were interleaved in the simulation list.
    pub fn take_ambient_sound_effect_batches(&mut self) -> Vec<EntityTickEffectBatch> {
        batch_entity_tick_effects(std::mem::take(&mut self.pending_ambient_sounds))
    }

    /// Drains every per-entity animation cue recorded since the last call — the
    /// visible sibling of [`take_vocalisations`](Self::take_vocalisations), and
    /// drained rather than read for the same reason: a slow consumer must not
    /// flash the same hit twice.
    pub fn take_entity_animations(&mut self) -> Vec<MobAnimation> {
        std::mem::take(&mut self.pending_animations)
    }

    /// Records the hurt or death sound **and animation** for a hit that landed on
    /// mob `id` — vanilla's own generic hurt/die handlers playing
    /// their own hurt-sound/death-sound getters, plus the damage-event/entity-status-3
    /// broadcasts those two methods send alongside.
    ///
    /// Called from every funnel that applies damage rather than from
    /// [`SimMob::apply_damage`] itself, because the queue lives on the sim and
    /// `apply_damage` holds only the one mob. `applied <= 0.0` (a hit fully
    /// swallowed by i-frames or absorption) is silent *and* invisible, matching
    /// vanilla's own generic hurt handler returning before either broadcast — the guard is
    /// its own "took full damage" check there and the same `applied > 0.0` here.
    ///
    /// **Must be called before the end-of-tick `retain`**, or a killing blow
    /// finds no mob to read the species and position from and dies silently.
    ///
    /// # Why the sound and the animation share one entry point
    ///
    /// They share a *cause*. Vanilla emits both from inside its own generic
    /// hurt/die handlers
    /// under the same guard, so splitting them into two recorders here would give
    /// two chances for one damage funnel to be taught about one of them and not
    /// the other — which is exactly how the animation came to be missing while
    /// every funnel already had the sound.
    pub(super) fn note_vocalisation(&mut self, id: i32, applied: f32) {
        if applied <= 0.0 {
            return;
        }
        let Some(mob) = self.mobs.iter_mut().find(|m| m.id == id) else {
            return;
        };
        // Vanilla's own "play hurt sound" step calls its own ambient-sound-time
        // reset before
        // playing the hurt sound itself, so a mob that just yelped in pain
        // does not also roll an idle vocalisation on the very next tick.
        mob.ambient_sound_time = -AMBIENT_SOUND_INTERVAL;
        // Hurt *and* death on a killing blow, in that order, because vanilla sends
        // both: its own generic hurt handler broadcasts the damage event and only then calls die,
        // which broadcasts byte 3. The client needs the flash to have started for
        // the tip-over to look like a death rather than a teleport.
        self.pending_animations
            .push(MobAnimation::Hurt { entity_id: id });
        if mob.health <= 0.0 {
            self.pending_animations
                .push(MobAnimation::Died { entity_id: id });
        }
        // Vanilla draws pitch from the level RNG; this sim's only clock is
        // `tick_count`, and consuming from a shared generator here would shift
        // every other draw. Mixed with the id so two mobs hit in one tick differ.
        let phase = (self.tick_count.wrapping_mul(31).wrapping_add(id as u64)) % 21;
        let pitch = 0.9 + phase as f32 * 0.01;
        let effect = crate::effects::mob_vocalisation(
            mob.entity_type.to_string().as_str(),
            mob.position(),
            mob.health <= 0.0,
            mob.category == MobCategory::Monster,
            pitch,
            self.tick_count as i64,
        );
        if let Some(effect) = effect {
            self.pending_vocalisations.push(effect);
        }
    }

    /// Drains every graze [`tick`](Self::tick) has recorded since the last call,
    /// as `(mob block position, which block)`.
    ///
    /// Drained rather than read for [`take_detonations`](Self::take_detonations)'
    /// reason — a slow consumer must not apply the same eat twice — and it exists
    /// at all because this sim cannot apply it itself: `world: &'w ChunkWorld` is
    /// an immutable borrow.
    ///
    /// # Consumer behavior
    ///
    /// With block mutation enabled:
    ///
    /// * [`EatenBlock::AtFeet`] → destroy the block at that cell, **no drops**
    ///   (its own destroy-block call with drops disabled).
    /// * [`EatenBlock::Below`] → set the cell one down to `minecraft:dirt`, plus
    ///   level event `2001` for the break particles.
    ///
    /// The "ate" notification is emitted **even when block mutation suppresses
    /// the block change**, so wool regrowth and world mutation are separable —
    /// the gamerule check belongs on the consumer, never in the goal.
    ///
    /// The consumer drains this queue to apply wool regrowth (unshearing and
    /// aging the coat by 60 ticks), which is entity metadata on the wire.
    pub fn take_grazes(&mut self) -> Vec<(BlockPos, EatenBlock)> {
        std::mem::take(&mut self.pending_grazes)
    }

    /// Drains every player hit by a hostile mob's melee attack since the last
    /// call — the player-facing twin of
    /// [`take_detonations`](Self::take_detonations)'s handoff shape, for the
    /// identical reason: this sim owns no connection, so the driver
    /// (`crate::server::serve_play`'s `vitals_tick` arm) is what turns each
    /// entry into a real `PlayerVitals::apply_damage` call and a `SET_HEALTH`/
    /// hurt-animation packet. See [`PlayerHit`]'s own doc comment for how a
    /// target position resolves to an identity, and its disclosed gap for a
    /// grudge target.
    pub fn take_player_hits(&mut self) -> Vec<PlayerHit> {
        std::mem::take(&mut self.pending_player_hits)
    }

    /// Drains every player caught in an elder guardian's mining-fatigue pulse
    /// since the last call — the same handoff shape as
    /// [`take_player_hits`](Self::take_player_hits) above and for the
    /// identical reason: this sim owns no connection, so the driver is what
    /// turns each entry into a real `ActiveEffects::apply` call and a
    /// `GUARDIAN_ELDER_EFFECT` game event. See [`MiningFatigueAura`]'s own
    /// doc comment for exactly what the driver must apply.
    pub fn take_mining_fatigue_auras(&mut self) -> Vec<MiningFatigueAura> {
        std::mem::take(&mut self.pending_mining_fatigue)
    }

    /// Drains every zombie reinforcement roll that passed since the last call
    /// — see [`ReinforcementCall`]'s own doc for the 50-candidate terrain
    /// search the driver performs before spawning one.
    pub fn take_reinforcement_calls(&mut self) -> Vec<ReinforcementCall> {
        std::mem::take(&mut self.pending_reinforcements)
    }

    /// Drains every fire-ignition attempt a live lightning bolt made this
    /// tick — [`pending_lightning_fires`](Self::pending_lightning_fires)'s own
    /// doc explains why this sim cannot place the fire itself. The driver
    /// (`crate::tick::run_tick_loop_with_weather`) is expected to test each
    /// position with `crate::fire::can_survive` against the *live* world and
    /// write `crate::fire::state_for_placement` only where the cell is air and
    /// survives — this drain hands over candidates, not verified placements;
    /// the "air and can-survive" gate stays at the call site.
    pub fn take_lightning_fires(&mut self) -> Vec<BlockPos> {
        std::mem::take(&mut self.pending_lightning_fires)
    }

    /// Drains every projectile-vs-block impact recorded since the last call —
    /// see [`ProjectileBlockHit`]'s own doc for why this sim hands the write
    /// to a driver rather than resolving it here. The driver is expected to
    /// read the *live* block at each `pos`, check it is really still
    /// `redstone_target::TARGET`, consult its own `ScheduledTickQueue` for
    /// `has_pending_decay`, and call `redstone_target::apply_hit`.
    pub fn take_projectile_block_hits(&mut self) -> Vec<ProjectileBlockHit> {
        std::mem::take(&mut self.pending_projectile_block_hits)
    }

    /// Every live mob or connected player's position, floored to a
    /// [`BlockPos`] — the living-entity-in-box census for a lightning target.
    /// Pre-culling is deferred to the caller; `lightning::find_lightning_target_around`
    /// filters to its own search box internally.
    #[must_use]
    pub fn living_entity_positions(&self) -> Vec<BlockPos> {
        self.mobs
            .iter()
            .map(|m| lightning::floor_block_pos(m.position()))
            .chain(
                self.players
                    .iter()
                    .map(|p| lightning::floor_block_pos(p.perception.position)),
            )
            .collect()
    }

    /// The number of ticks advanced so far.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Applies an explosion centred at `centre` with blast `radius` (TNT is
    /// `4.0`) to every live mob, through the real ray-sampled exposure model
    /// (`explosion::seen_percent`, sampled against the sim's own
    /// [`ChunkWorld`] via its [`RayView`] impl) and damage formula
    /// (`explosion::entity_damage`), landing through the same
    /// [`SimMob::apply_damage`] pipeline a melee hit uses. Before this,
    /// `explosion.rs` had no consumer anywhere in the tree — its exposure grid
    /// and damage formula were exercised only by their own hermetic unit
    /// tests, with no path from "an explosion happened" to a health value
    /// anywhere changing.
    ///
    /// `flags` lets the caller pick which reduction stages the blast bypasses;
    /// a plain `DamageFlags::default()` runs armour/absorption normally.
    ///
    /// Returns `(id, damage_dealt)` for every mob that took nonzero damage,
    /// and removes any mob the blast killed. A mob whose exposure is fully
    /// blocked (every sampled ray hits terrain before the centre) takes no
    /// damage and is absent from the result — a wall genuinely shields it,
    /// this is not a distance cutoff.
    pub fn explode(&mut self, centre: Vec3, radius: f32, flags: DamageFlags) -> Vec<(i32, f32)> {
        let mut dealt = Vec::new();
        for m in &mut self.mobs {
            let shape = m.shape();
            let box_ = ExplosionAabb::from_size(
                m.position(),
                f64::from(shape.width),
                f64::from(shape.height),
            );
            let box_center = Vec3::new(
                (box_.min.x + box_.max.x) / 2.0,
                (box_.min.y + box_.max.y) / 2.0,
                (box_.min.z + box_.max.z) / 2.0,
            );
            let exposure = seen_percent(centre, box_, self.world);
            if exposure <= 0.0 {
                continue;
            }
            let distance = (box_center - centre).length();
            let raw = entity_damage(radius, distance, exposure);
            if raw <= 0.0 {
                continue;
            }
            let applied = m.apply_damage(raw, flags);
            if applied > 0.0 {
                dealt.push((m.id, applied));
            }
        }
        // After the loop rather than inside it: `note_vocalisation`
        // needs `&mut self` while the loop holds `&mut self.mobs`, and it must
        // still precede the retain below so a mob the blast killed is read for
        // its death sound before it leaves.
        for &(id, applied) in &dealt {
            self.note_vocalisation(id, applied);
        }
        self.reap_dead();
        dealt
    }
}
