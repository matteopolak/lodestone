//! Mob-vs-mob and player combat resolution for [`super::MobSim`].

use super::*;

impl<'w> MobSim<'w> {
    /// Resolves a melee attack against a live mob: runs the damage pipeline
    /// ([`SimMob::apply_damage`]) and, whenever the hit lands, applies the
    /// knockback impulse ([`lodestone_physics::knockback::knockback_impulse`]).
    /// Both results are written to the target state before the next
    /// [`snapshots`](Self::snapshots) call emits an entity packet.
    ///
    /// # Two knockback contributions
    ///
    /// Each damaging hit applies a flat `0.4` contribution and then the
    /// caller-supplied `knockback_power` bonus. A non-sprinting hit passes
    /// `0.0` for the bonus; sprinting adds the configured extra contribution.
    ///
    /// The two calls are chained through the same `knockback_impulse` primitive:
    /// the second call receives the first call's output velocity. This preserves
    /// the two successive halving/subtraction operations; one call with the
    /// summed power would halve the pre-hit velocity only once.
    ///
    /// # Direction
    ///
    /// Both calls use the horizontal vector from the target to the attacker,
    /// `dx = attacker_pos.x - target_pos.x` and
    /// `dz = attacker_pos.z - target_pos.z`. The impulse subtracts that
    /// direction from velocity, moving the target away from the attacker.
    ///
    /// [`NavigatingMob`] stores no ground-contact flag, so the attack uses the
    /// grounded branch of `knockback_impulse` with its `0.4`-capped vertical
    /// hop.
    ///
    /// Returns `None` if `target_id` names no live mob. Returns `Some` for
    /// every resolved hit, including one ignored by invulnerability frames
    /// (see [`AttackOutcome::damage_dealt`]). A killing blow removes the mob
    /// immediately rather than deferring removal to the next [`tick`](Self::tick).
    pub fn attack(
        &mut self,
        target_id: i32,
        attacker_pos: Vec3,
        raw_damage: f32,
        flags: DamageFlags,
        knockback_power: f64,
    ) -> Option<AttackOutcome> {
        // Read before the mutable borrow below: the grudge deadline is
        // absolute, so it needs the clock as of this tick.
        let now = self.tick_count;
        let (health, velocity, damage_dealt, pack_alert) = {
            let mob = self.get_mut(target_id)?;
            let damage_dealt = mob.apply_damage(raw_damage, flags);
            // Record the attacker's position with the damage event so the mob's
            // retaliation logic can select a target and its knockback logic can
            // use the same direction.
            mob.mob.note_hurt(Some(attacker_pos));
            // Start the persistent grudge alongside the retaliation record, so
            // both state changes share this attack's simulation tick.
            //
            // Every mob records the grudge; only species with an anger-gated
            // target rule read `angry_target`, so the shared state does not
            // alter species that have no such rule.
            let was_already_angry = mob.anger.is_some();
            let end_time = now + grudge_ticks(&mut mob.mob);
            mob.anger = Some(Anger {
                end_time,
                target: attacker_pos,
            });
            // Group alerting is enabled for the species returned by
            // `alert_species`. It runs only when this hit creates a grudge;
            // repeated hits during one grudge do not re-alert the group.
            let pack_alert = if was_already_angry {
                None
            } else {
                alert_species(mob.entity_type.path()).map(|(box_xz, box_y, need_owner_match)| {
                    (
                        mob.entity_type.clone(),
                        mob.position(),
                        mob.owner_uuid(),
                        need_owner_match,
                        box_xz,
                        box_y,
                    )
                })
            };
            // Mark this as player-attributed damage for
            // `PLAYER_HURT_EXPERIENCE_TIME` ticks; the death-loot path uses this
            // deadline when deciding whether to award experience.
            mob.hurt_by_player_until = Some(now + PLAYER_HURT_EXPERIENCE_TIME);
            if damage_dealt > 0.0 && mob.health() > 0.0 {
                let target_pos = mob.position();
                // Vector from the target to the attacker; the impulse moves the
                // target away from that source position.
                let dx = attacker_pos.x - target_pos.x;
                let dz = attacker_pos.z - target_pos.z;
                let v = mob.velocity();
                let jitter = || (1.0, 0.0);
                // Coincident horizontal positions use a fixed non-degenerate
                // fallback because this call has no random source. That case
                // needs only one fallback draw to produce a valid direction.
                //
                // First call: the mandatory flat knockback contribution on
                // every damaging hit.
                let after_default = lodestone_physics::knockback::knockback_impulse(
                    lodestone_physics::geometry::Vec3d { x: v.x, y: v.y, z: v.z },
                    true, // always the grounded branch — see this method's own doc comment.
                    MELEE_DEFAULT_KNOCKBACK_POWER,
                    dx,
                    dz,
                    mob.knockback_resistance(),
                    jitter,
                );
                // Second call: the attacker-specific bonus, chained onto the
                // first call's result.
                let new_velocity = if knockback_power > 0.0 {
                    lodestone_physics::knockback::knockback_impulse(
                        after_default,
                        true,
                        knockback_power,
                        dx,
                        dz,
                        mob.knockback_resistance(),
                        jitter,
                    )
                } else {
                    after_default
                };
                mob.apply_knockback(Vec3::new(new_velocity.x, new_velocity.y, new_velocity.z));
            }
            (mob.health(), mob.velocity(), damage_dealt, pack_alert)
        };
        // Resolve group alerts after the mutable borrow of `target_id` ends.
        // Each matching same-species mob in the alert box that has no active
        // grudge receives the victim's deadline and target position.
        if let Some((species_key, victim_pos, victim_owner, need_owner_match, box_xz, box_y)) =
            pack_alert
        {
            for other in &mut self.mobs {
                if other.id == target_id || other.entity_type != species_key {
                    continue;
                }
                if need_owner_match && other.owner_uuid() != victim_owner {
                    continue;
                }
                if other.anger.is_some() {
                    continue;
                }
                let p = other.position();
                if (p.x - victim_pos.x).abs() > box_xz
                    || (p.z - victim_pos.z).abs() > box_xz
                    || (p.y - victim_pos.y).abs() > box_y
                {
                    continue;
                }
                other.anger = Some(Anger {
                    end_time: now + grudge_ticks(&mut other.mob),
                    target: attacker_pos,
                });
            }
        }
        // before the removal below, so a killing blow is read for
        // its death sound rather than finding no mob.
        self.note_vocalisation(target_id, damage_dealt);
        let killed = health <= 0.0;
        if killed {
            // Through `reap_dead`, not a bare retain: a melee kill must drop the
            // same loot an explosion kill does. Health is already `0.0` here, so
            // the shared reaper picks exactly this mob out.
            self.reap_dead();
        }
        Some(AttackOutcome {
            health,
            killed,
            damage_dealt,
            velocity,
        })
    }

    /// How far a witnessing villager can be from a killed one and still record
    /// the death. The fixed radius uses the same squared-distance shape as
    /// [`GOSSIP_SPREAD_RADIUS_SQR`](Self::GOSSIP_SPREAD_RADIUS_SQR).
    const VILLAGER_KILLED_WITNESS_RADIUS_SQR: f64 = 100.0; // 10 blocks

    /// [`attack`](Self::attack), plus villager-reputation updates: a
    /// player-identified attacker hurting or killing a villager writes
    /// `VillagerHurt`/`VillagerKilled` gossip through
    /// [`villager::reputation::apply_reputation_event`].
    ///
    /// A separate method keeps `attack`'s signature focused on damage. The
    /// server attack path calls this method for player-attributed hits, while
    /// `attacker` is `None` when the swinging actor is unavailable; the gossip
    /// write is skipped and the damage behavior remains [`attack`](Self::attack).
    ///
    /// A hit's gossip is written to the **victim's own** ledger. A kill's gossip
    /// is written to **every nearby witnessing villager's own** ledger instead,
    /// because a killed victim is removed before the witness update.
    pub fn attack_from_player(
        &mut self,
        target_id: i32,
        attacker: Option<PlayerIdentity>,
        attacker_pos: Vec3,
        raw_damage: f32,
        flags: DamageFlags,
        knockback_power: f64,
    ) -> Option<AttackOutcome> {
        // Withers live in `self.withers`, separate from the ordinary mob map,
        // and use their own armor and emergence gates without mob anger,
        // gossip, or knockback state.
        if self.withers.contains_key(&target_id) {
            return self.attack_wither(target_id, raw_damage);
        }
        // Dragons likewise live in `self.dragons` and use the dedicated dragon
        // damage path.
        if self.dragons.contains_key(&target_id) {
            return self.attack_dragon(target_id, raw_damage);
        }
        // End crystals live in `self.crystals` and use a one-hit destruction
        // branch, so they do not enter the mob damage pipeline. Boats, rafts,
        // and minecarts live in `self.vehicles` and use their own damage
        // response without health, armor, knockback, or mob reputation state.
        if self.vehicles.contains_key(&target_id) {
            return self.attack_vehicle(target_id, raw_damage);
        }
        if self.crystals.contains_key(&target_id) {
            self.destroy_end_crystal(target_id)?;
            return Some(AttackOutcome {
                health: 0.0,
                killed: true,
                damage_dealt: raw_damage,
                velocity: Vec3::new(0.0, 0.0, 0.0),
            });
        }
        let target_was_villager = self
            .get(target_id)
            .is_some_and(|m| m.entity_type.path() == "villager");
        // Capture raid membership before `self.attack` mutates the target;
        // `raid_containing_raider` reads the live raider list, which is pruned
        // during the next raid tick.
        let target_raid_id = self.raid_containing_raider(target_id);
        let target_pos_before = self.get(target_id).map(SimMob::position);
        let outcome = self.attack(target_id, attacker_pos, raw_damage, flags, knockback_power)?;
        if let Some(actor) = attacker
            && outcome.killed
            && let Some(raid_id) = target_raid_id
        {
            self.add_raid_hero(raid_id, actor.uuid);
        }
        // Zombie-family reinforcement performs only its probability roll here;
        // the terrain search belongs to the spawn driver. It requires a
        // successful hit, hard difficulty, and the `spawn_mobs` rule. Reborrow
        // the target after the random draw because the RNG is a sibling field.
        if !outcome.killed && outcome.damage_dealt > 0.0 && self.spawn_hard_difficulty && self.spawn_monsters_enabled {
            let reinforcement_info = self.get(target_id).and_then(|mob| {
                matches!(
                    mob.entity_type.path(),
                    "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin"
                )
                .then(|| {
                    (
                        mob.entity_type.clone(),
                        mob.position(),
                        mob.reinforcement_chance,
                        mob.attack_target_id,
                    )
                })
            });
            if let Some((entity_type, position, chance, own_target)) = reinforcement_info
                && self.reinforcement_rng.next_f32() < chance as f32
                // Vanilla's own reinforcement-target resolution: the
                // mob's own current attack target, falling back to whoever
                // just hit it (only if that attacker is a living entity).
                && let Some(reinforcement_target) = own_target.or_else(|| attacker.map(|a| a.entity_id))
            {
                if let Some(mob) = self.get_mut(target_id) {
                    mob.reinforcement_chance -= ZOMBIE_REINFORCEMENT_CALLER_CHARGE;
                }
                self.pending_reinforcements.push(ReinforcementCall {
                    position,
                    entity_type,
                    target_id: reinforcement_target,
                });
            }
        }
        // Owner-directed retaliation: a wolf (or any
        // tamed pet) joins whatever
        // fight its owner just started, reading the owner's own "last hurt
        // mob" field on
        // the *owner's* own living-entity state. This is the same field the
        // mob retaliation behavior reads, just recorded on the player instead
        // — see `NavigatingMob::set_owner_hurt_target`'s own
        // doc comment for the decay rule. Every tame pet owned by the
        // attacking player gets the target's pre-attack position (matching
        // the villager-witness resolution just below, which uses the same
        // `target_pos_before` for the identical reason: `target_id` may no
        // longer resolve to a live `SimMob` once `self.attack` has killed it).
        if let Some(actor) = attacker
            && let Some(pos) = target_pos_before
        {
            for pet in &mut self.mobs {
                if pet.owner_uuid() == Some(actor.uuid) && pet.is_tame() && pet.health() > 0.0 {
                    pet.mob.set_owner_hurt_target(Some(pos));
                }
            }
        }
        if let Some(actor) = attacker
            && target_was_villager
        {
            if outcome.killed {
                if let Some(pos) = target_pos_before {
                    for witness in &mut self.mobs {
                        if witness.entity_type.path() != "villager" {
                            continue;
                        }
                        let p = witness.position();
                        let dist_sqr =
                            (p.x - pos.x).powi(2) + (p.y - pos.y).powi(2) + (p.z - pos.z).powi(2);
                        if dist_sqr > Self::VILLAGER_KILLED_WITNESS_RADIUS_SQR {
                            continue;
                        }
                        villager::reputation::apply_reputation_event(
                            &mut witness.gossip,
                            villager::reputation::ReputationEventType::VillagerKilled,
                            actor.uuid,
                        );
                    }
                }
            } else if let Some(mob) = self.get_mut(target_id) {
                villager::reputation::apply_reputation_event(
                    &mut mob.gossip,
                    villager::reputation::ReputationEventType::VillagerHurt,
                    actor.uuid,
                );
            }
        }
        Some(outcome)
    }

    /// [`attack_from_player`](Self::attack_from_player)'s wither branch — a
    /// melee hit is never an arrow or a wind charge and never bypasses the
    /// emergence-invulnerability gate, so both of
    /// [`damage_wither`](Self::damage_wither)'s bool parameters are fixed
    /// `false`; `damage_wither` itself already applies the powered-armour
    /// and invulnerable-emergence refusals and removes the wither on a
    /// killing blow. A wither never moves (see `mobs::wither`'s own module
    /// doc), so the outcome's `velocity` is always zero rather than a
    /// knockback impulse.
    fn attack_wither(&mut self, target_id: i32, raw_damage: f32) -> Option<AttackOutcome> {
        let health = self.damage_wither(target_id, raw_damage, false, false)?;
        Some(AttackOutcome {
            health,
            killed: health <= 0.0,
            damage_dealt: raw_damage,
            velocity: Vec3::new(0.0, 0.0, 0.0),
        })
    }
}
