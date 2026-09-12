//! Mob lifecycle effects: drops, vibrations, gifts, and rider cleanup.

use super::*;

impl<'w> MobSim<'w> {
    pub fn set_mob_drops(&mut self, allowed: bool) {
        self.mob_drops = allowed;
    }

    /// Discards every mob Peaceful forbids — vanilla's own "check despawn" guard,
    /// peaceful difficulty and not allowed-in-peaceful for the type. Returns how
    /// many were removed.
    ///
    /// Rolls **no** loot: vanilla's peaceful sweep is `discard()`, not a death, so a
    /// player switching to Peaceful does not get a floor covered in rotten flesh.
    ///
    /// **The predicate is the per-type `notInPeaceful` flag
    /// ([`crate::mob_spawn::allowed_in_peaceful`]), not
    /// [`is_hostile_species`].** The two disagree in both directions and the
    /// disagreement is visible: `is_hostile_species` is a 22-name list serving the
    /// *category* question, so it kept a slime, magma cube, silverfish, phantom,
    /// vex, ravager, hoglin or warden alive on Peaceful — and slimes really do
    /// spawn here, because `crate::natural_spawn` models slime chunks. In the other
    /// direction the flag keeps a shulker and a piglin, which vanilla also keeps
    /// and which a monster-category test would delete.
    pub fn remove_monsters(&mut self) -> usize {
        let before = self.mobs.len();
        self.mobs
            .retain(|m| crate::mob_spawn::allowed_in_peaceful(m.entity_type.path()));
        before - self.mobs.len()
    }

    /// Removes every mob at or below zero health, rolling its death loot table
    /// on the way out (the mob tick's death-loot chain).
    ///
    /// This is the central mob-removal path. Each dead mob contributes the
    /// loot table selected by [`crate::block_drops::mob_loot_table_id`], and
    /// each resulting stack becomes
    /// an item entity at the mob's position with the configured drop velocity.
    ///
    /// Rolls in the **empty** loot context, so `killed_by_player` is `false` and
    /// `enchanted_count_increase` (looting) contributes nothing: rare drops gated
    /// on a player kill do not appear. That is honest rather than approximated —
    /// the context has no attacker field to fill (see [`crate::loot`]).
    pub(super) fn reap_dead(&mut self) {
        let now = self.tick_count;
        // `drops_experience` is vanilla's own drop-experience call's own guard, read here
        // while the mob still exists: a player's hit within the last
        // `PLAYER_HURT_EXPERIENCE_TIME` ticks, and not a baby
        // (its own "should drop experience" check is "not a baby").
        //
        // `drops_ominous_bottle` is vanilla's own "captain without raid"
        // raider predicate
        // (`hasRaid=false, isCaptain=true`) — see
        // [`drop_ominous_bottle`](Self::drop_ominous_bottle)'s own doc for
        // why it is resolved here rather than through
        // [`drop_death_loot`](Self::drop_death_loot)'s generic table roll.
        // `raid_containing_raider` reads `self.raids` only, so calling it
        // from inside this `self.mobs.iter()` closure borrows disjointly —
        // both borrows are shared, so nothing here needs deferring the way
        // the mutable passes below do.
        let dead: Vec<(i32, ResourceKey, Vec3, bool, bool)> = self
            .mobs
            .iter()
            .filter(|m| m.health <= 0.0)
            .map(|m| {
                let by_player = m.hurt_by_player_until.is_some_and(|until| now < until);
                let drops_ominous_bottle = m.entity_type.path() == "pillager"
                    && m.is_patrol_leader()
                    && self.raid_containing_raider(m.id).is_none();
                (
                    m.id,
                    m.entity_type.clone(),
                    m.position(),
                    by_player && !m.is_baby(),
                    drops_ominous_bottle,
                )
            })
            .collect();
        if dead.is_empty() {
            return;
        }
        self.mobs.retain(|m| m.health > 0.0);
        for (id, entity_type, position, drops_experience, drops_ominous_bottle) in dead {
            self.drop_death_loot(&entity_type, position);
            if drops_ominous_bottle {
                self.drop_ominous_bottle(position);
            }
            // Drop ordinary death loot before experience, so the two output
            // streams retain their stable ordering.
            if drops_experience {
                self.drop_death_experience(&entity_type, position);
            }
            // A death posts an entity-die event at the dying mob's position,
            // carrying that mob's id as the source. Other event producers are
            // posted by their owning systems.
            self.post_vibration(position, VibrationEvent::EntityDie, Some(id));
        }
    }
    /// Posts one vibration for a producer. The optional `source` identifies
    /// the entity responsible when the producer has one; see
    /// [`PostedVibration::source`]'s own doc.
    pub fn post_vibration(&mut self, position: Vec3, event: VibrationEvent, source: Option<i32>) {
        self.posted_vibrations.push(PostedVibration { position, event, source });
    }

    /// Resolves this tick's nearest-vibration answer for every listener
    /// species, then drains the posted log back to empty. Runs at the *end*
    /// of the tick, deliberately not inside
    /// [`feed_perception`](Self::feed_perception) (which runs before
    /// [`reap_dead`](Self::reap_dead) posts anything): a death this same
    /// tick must be audible this same tick, not one tick late — the same
    /// reasoning [`tick_orbs`](Self::tick_orbs) already gives for reading
    /// `tick_count` before its own increment.
    fn resolve_vibrations(&mut self) {
        let posted = std::mem::take(&mut self.posted_vibrations);
        for mob in &mut self.mobs {
            mob.nearest_vibration = if is_vibration_listener(mob.entity_type.path()) {
                nearest_listenable(mob.position(), WARDEN_LISTENER_RADIUS, &posted)
            } else {
                None
            };
            // The allay note-block consumer: an allay within
            // `ALLAY_LISTENER_RADIUS` of a `NoteBlockPlay` this tick either
            // adopts it (when no liked note block exists) or refreshes its
            // cooldown for the same position heard again; a different position
            // while one is already liked is
            // ignored.
            if mob.entity_type.path() == "allay"
                && let Some(heard) =
                    nearest_note_block_play(mob.position(), ALLAY_LISTENER_RADIUS, &posted)
            {
                match mob.allay_liked_noteblock {
                    Some((pos, _)) if pos == heard.position => {
                        mob.allay_liked_noteblock = Some((pos, ALLAY_NOTEBLOCK_COOLDOWN_TICKS));
                    }
                    None => {
                        mob.allay_liked_noteblock =
                            Some((heard.position, ALLAY_NOTEBLOCK_COOLDOWN_TICKS));
                    }
                    Some(_) => {}
                }
            }
        }
    }

    /// Vanilla's own "pick up item" inventory-carrier helper /
    /// allay-specific "wants to pick up" check: a
    /// held-item allay with inventory room absorbs the nearest matching
    /// dropped item within [`ALLAY_ITEM_PICKUP_RADIUS`], the ground half of
    /// this crate's own [`ALLAY_ITEM_PICKUP_RADIUS`] doc-disclosed
    /// bounding-box substitution. Two passes for the same borrow-checker
    /// reason [`feed_perception`](Self::feed_perception)'s own doc gives:
    /// deciding what to pick up reads `self.item_state` while mutating
    /// `self.mobs` would need it held mutably too.
    ///
    /// **Disclosed narrowing**: this simulation has no access to the shared
    /// block-mutation rule at this seam, so every eligible allay picks up.
    fn allay_pick_up_items(&mut self) {
        struct Candidate {
            mob_index: usize,
            position: Vec3,
            held_item: String,
            room: u32,
        }
        let candidates: Vec<Candidate> = self
            .mobs
            .iter()
            .enumerate()
            .filter_map(|(mob_index, m)| {
                if m.entity_type.path() != "allay" || m.health <= 0.0 {
                    return None;
                }
                let held_item = m.mob.main_hand_item()?.to_owned();
                let room = ALLAY_INVENTORY_MAX.saturating_sub(m.allay_inventory_count);
                if room == 0 {
                    return None;
                }
                Some(Candidate { mob_index, position: m.position(), held_item, room })
            })
            .collect();

        let radius_sq = ALLAY_ITEM_PICKUP_RADIUS * ALLAY_ITEM_PICKUP_RADIUS;
        for candidate in candidates {
            let hit = self
                .item_state
                .iter()
                .filter(|(_, state)| {
                    state.item.path() == candidate.held_item
                        && dist_sqr(state.motion.position, candidate.position) <= radius_sq
                })
                .min_by(|a, b| {
                    dist_sqr(a.1.motion.position, candidate.position)
                        .total_cmp(&dist_sqr(b.1.motion.position, candidate.position))
                })
                .map(|(&id, _)| id);
            let Some(id) = hit else { continue };
            let stack = u32::from(self.items.get(id).map_or(0, |l| l.count));
            let take = stack.min(candidate.room);
            if take == 0 {
                continue;
            }
            let remaining = stack - take;
            if remaining == 0 {
                self.remove_item(id);
            } else {
                self.set_item_count(id, u8::try_from(remaining).unwrap_or(u8::MAX));
            }
            self.mobs[candidate.mob_index].allay_inventory_count += take;
        }
    }

    /// Allay item delivery: a carrying allay within
    /// [`ALLAY_DELIVER_ARRIVAL_DISTANCE`] of its liked note-block's `.above()`
    /// cell throws one item from its inventory there per tick — a real dropped
    /// [`ItemEntity`](lodestone_entity::item_entity), not a state flag, so a
    /// player can actually walk over and collect it. Throws use a 20-tick
    /// cadence with a small random velocity; this model drains one item per
    /// tick and does not model velocity spread.
    ///
    /// **Not delivered to a liked player as a fallback** because no player
    /// delivery target is available in this simulation seam.
    fn allay_deliver_items(&mut self) {
        struct Delivery {
            mob_index: usize,
            drop_position: Vec3,
        }
        let deliveries: Vec<Delivery> = self
            .mobs
            .iter()
            .enumerate()
            .filter_map(|(mob_index, m)| {
                if m.entity_type.path() != "allay" || m.health <= 0.0 || m.allay_inventory_count == 0
                {
                    return None;
                }
                let (liked_pos, ticks) = m.allay_liked_noteblock?;
                if ticks <= 0 {
                    return None;
                }
                let above = Vec3::new(liked_pos.x, liked_pos.y + 1.0, liked_pos.z);
                if dist_sqr(m.position(), above) > ALLAY_DELIVER_ARRIVAL_DISTANCE * ALLAY_DELIVER_ARRIVAL_DISTANCE {
                    return None;
                }
                Some(Delivery { mob_index, drop_position: above })
            })
            .collect();

        for delivery in deliveries {
            let Some(item) = self.mobs[delivery.mob_index].mob.main_hand_item() else {
                continue;
            };
            let Ok(item) = ResourceKey::from_str(&format!("minecraft:{item}")) else {
                continue;
            };
            self.spawn_item(
                item,
                delivery.drop_position,
                Vec3::new(0.0, 0.0, 0.0),
                ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
            );
            self.mobs[delivery.mob_index].allay_inventory_count -= 1;
        }
    }

    /// Vanilla's own generic drop-experience call: pops this species' reward as orbs at `position`.
    ///
    /// The caller has already applied vanilla's two eligibility tests (see
    /// [`reap_dead`](Self::reap_dead)); this applies the third,
    /// `level.getGameRules().get(GameRules.MOB_DROPS)`, which is the same rule
    /// [`drop_death_loot`](Self::drop_death_loot) honours — so `/gamerule mobDrops
    /// false` suppresses XP as well as items, exactly as vanilla does.
    ///
    /// The reward roll rides [`orb_rng`](Self::orb_rng) rather than a position-seeded
    /// stream: unlike a loot roll, an animal's `1 + nextInt(3)` has no reason to be
    /// reproducible from the death site, and putting it on the orb stream keeps every
    /// orb-related draw in one sequence.
    fn drop_death_experience(&mut self, entity_type: &ResourceKey, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let reward = species::mob_experience_reward(entity_type, &mut self.orb_rng);
        if reward <= 0 {
            return;
        }
        self.award_experience(position, Vec3::new(0.0, 0.0, 0.0), reward);
    }

    /// Rolls `entity_type`'s death loot table and spawns the result at
    /// `position`. See [`reap_dead`](Self::reap_dead) for the vanilla chain.
    ///
    /// Seeded from the tick count and the position, so a death is deterministic
    /// for a given world state without threading a connection's RNG into the sim.
    fn drop_death_loot(&mut self, entity_type: &ResourceKey, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let Some(table) = crate::block_drops::mob_loot_table_id(entity_type) else {
            return;
        };
        let tables = crate::block_drops::bundled_tables();
        if tables.get(&table).is_none() {
            return;
        }
        let mut rng = SpawnRng::new(
            (self.tick_count as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (position.x.to_bits() ^ position.z.to_bits().rotate_left(31)),
        );
        let rolled = tables.roll(&table, &crate::loot::LootContext::default(), &mut rng);
        for stack in rolled {
            if stack.count == 0 {
                continue;
            }
            let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
            let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
            self.spawn_item(
                stack.item.clone(),
                position,
                velocity,
                ItemLifecycle::newly_dropped(
                    count,
                    lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                ),
            );
        }
    }

    /// The ominous bottle item off a pillager patrol captain's death —
    /// vanilla's `entities/pillager.json` loot pool, gated on
    /// vanilla's own "captain without raid" raider predicate (`hasRaid=false,
    /// isCaptain=true`): a patrol leader ([`SimMob::is_patrol_leader`]) not
    /// currently a member of any active raid
    /// ([`raid::MobSim::raid_containing_raider`]). [`reap_dead`](Self::reap_dead)
    /// resolves the gate (it needs both a live patrol-leader flag and a raid
    /// census, neither available inside a loot roll) and calls this only
    /// when it holds.
    ///
    /// **Not routed through [`drop_death_loot`](Self::drop_death_loot)'s
    /// generic bundled-loot-table engine.** `crate::loot`'s own
    /// `entity_properties` condition is context-blind — `LootContext` carries
    /// no entity data at all (see that module's own doc) — so a bundled
    /// `entities/pillager.json` would silently roll `false` on exactly the
    /// gate this drop needs: the identical hole `block_state_property` was
    /// before `LootContext::block_state` existed. A dedicated call site,
    /// [`drop_death_experience`](Self::drop_death_experience)'s own shape,
    /// until entity-conditioned loot context lands generically.
    ///
    /// **Disclosed narrowing**: vanilla rolls a uniform `0..=4` amplifier
    /// onto the bottle's own `minecraft:ominous_bottle_amplifier` component
    /// (its own "set ominous bottle amplifier" loot function); every bottle dropped here is
    /// amplifier `0` instead of the real roll, because persisting a
    /// per-stack amplifier needs a new field on
    /// `lodestone_model::ItemComponents`, which this session's ownership
    /// does not reach (`crates/lodestone-model/**` — see
    /// `docs/raids-and-patrols.md` §5 for the exact hunk).
    /// `crate::server::finish_drinking_ominous_bottle` is the consumer this
    /// feeds; amplifier `0` is still a real, working value there —
    /// `raid::absorb_raid_omen(0, 0) == 1` starts a genuine raid — so this is
    /// "always the weakest roll", not "does nothing".
    fn drop_ominous_bottle(&mut self, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let bottle: ResourceKey = "minecraft:ominous_bottle".parse().expect("a literal item id is always valid");
        let mut rng = SpawnRng::new(
            (self.tick_count as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (position.x.to_bits() ^ position.z.to_bits().rotate_left(31)),
        );
        let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
        self.spawn_item(
            bottle,
            position,
            velocity,
            ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
        );
    }

    /// Vanilla's own environment-attribute "cat waking-up gift chance" value
    /// at `day_time` —
    /// hand-transcribed from its one modifier track
    /// (its own timelines table's cat-waking-up-gift-chance row: a maximum
    /// float modifier,
    /// constant easing, keyframes `0.0F` at tick 362 and `0.7F` at tick
    /// 23667 within the 24000-tick day cycle) rather than read from a general
    /// timeline engine — this crate has no environment-attribute/timeline
    /// reader at all, the same disclosed gap
    /// [`natural_spawn::surface_slime_spawn_chance`](crate::natural_spawn)'s own
    /// doc names for the moon-phase slime chance, and building one for a
    /// single step function would be out of proportion to what it buys.
    ///
    /// A `CONSTANT` easing is a step function: the attribute holds `0.0` from
    /// tick 362 up to (not including) 23667, and `0.7` from 23667 wrapping
    /// through midnight back to 362 — so a cat's gift only has a real chance
    /// to land in the pre-dawn stretch of the night, which is when a player
    /// who slept through to morning actually wakes.
    fn cat_gift_chance(day_time: i32) -> f32 {
        let t = day_time.rem_euclid(24_000);
        if !(362..23_667).contains(&t) { 0.7 } else { 0.0 }
    }

    /// Resolves every cat morning-gift request
    /// recorded this tick: rolls [`Self::cat_gift_chance`] at
    /// the current [`MobSim::day_time`], and on success rolls
    /// `gameplay/cat_morning_gift` and spawns the result at the cat's own
    /// position — the same loot-table-then-`spawn_item` shape
    /// [`drop_death_loot`](Self::drop_death_loot) already uses.
    ///
    /// **Disclosed simplification**: no random relocation occurs before the
    /// drop. The item spawns at the cat's current position.
    fn resolve_cat_gifts(&mut self, gift_requests: Vec<i32>) {
        if gift_requests.is_empty() {
            return;
        }
        let chance = Self::cat_gift_chance(self.day_time);
        let table = ResourceKey::new("minecraft", "gameplay/cat_morning_gift")
            .expect("a static loot-table key parses");
        let tables = crate::block_drops::bundled_tables();
        for id in gift_requests {
            let Some(pos) = self.get(id).map(SimMob::position) else {
                continue;
            };
            let mut rng = SpawnRng::new(
                (self.tick_count as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ (pos.x.to_bits() ^ pos.z.to_bits().rotate_left(31))
                    ^ (id as u64),
            );
            if rng.next_f32() >= chance {
                continue;
            }
            let rolled = tables.roll(&table, &crate::loot::LootContext::default(), &mut rng);
            for stack in rolled {
                if stack.count == 0 {
                    continue;
                }
                let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
                let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
                self.spawn_item(
                    stack.item.clone(),
                    pos,
                    velocity,
                    ItemLifecycle::newly_dropped(
                        count,
                        lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                    ),
                );
            }
        }
    }

    /// Resolves every shoulder-mount request recorded this tick. This crate has
    /// no per-player NBT inventory, so [`SimMob::owner`] resolves to a UUID
    /// plus [`self.shoulder_riders`](Self::shoulder_riders) — one slot per
    /// owner — is the stand-in: the
    /// parrot mob is removed the same way [`Self::despawn_pass`] removes any
    /// other mob, and [`Self::tick_shoulder_dismounts`] is what brings it
    /// back.
    ///
    fn resolve_shoulder_mounts(&mut self, shoulder_requests: Vec<i32>) {
        for id in shoulder_requests {
            let Some(m) = self.get(id) else { continue };
            let Some(MobOwner::Player(uuid)) = m.owner else {
                continue;
            };
            if self.shoulder_riders.contains_key(&uuid) {
                // A slot is already taken — vanilla tries the second
                // shoulder here; this crate models one slot per owner (see
                // this method's own doc), so a second parrot simply fails to
                // mount and stays in the world, exactly as a vanilla parrot
                // does once both shoulders are full.
                continue;
            }
            self.shoulder_riders.insert(
                uuid,
                ShoulderRider {
                    entity_type: m.entity_type().clone(),
                    mounted_tick: self.tick_count,
                },
            );
            self.mobs.retain(|m| m.id != id);
        }
    }

    /// Dismounts every shoulder rider whose owner meets a dismount condition,
    /// respawning the mob at the owner's position — vanilla's own
    /// player-side "remove entities on shoulder"/"respawn entity on shoulder"
    /// calls,
    /// gated the same way on `mounted_tick + 20 <
    /// gameTime` so a parrot cannot fall off the instant it lands.
    ///
    /// **Disclosed simplification**: vanilla's own "handle shoulder entities" step fires
    /// on five conditions (`fallDistance > 0.5`, in water, flying, sleeping,
    /// in powder snow) — this models only **sleeping**, because it is the
    /// only one of the five this sim already tracks for an owner
    /// ([`Self::sleeping_players`], built for [`Self::resolve_cat_gifts`]'s
    /// own owner-sleep feed). The other four need per-player physical state
    /// (fall distance, ability flags, block-at-feet) this crate's player
    /// census does not carry.
    fn tick_shoulder_dismounts(&mut self) {
        if self.shoulder_riders.is_empty() {
            return;
        }
        let sleeping: std::collections::HashSet<i32> =
            self.sleeping_players.iter().map(|&(id, _)| id).collect();
        let sleeping_owners: Vec<Uuid> = self
            .players
            .iter()
            .filter_map(|p| {
                let identity = p.identity?;
                sleeping.contains(&identity.entity_id).then_some(identity.uuid)
            })
            .collect();
        let tick_count = self.tick_count;
        let mut to_dismount = Vec::new();
        for (&uuid, rider) in &self.shoulder_riders {
            if tick_count < rider.mounted_tick + 20 {
                continue;
            }
            if sleeping_owners.contains(&uuid) {
                to_dismount.push(uuid);
            }
        }
        for uuid in to_dismount {
            let Some(rider) = self.shoulder_riders.remove(&uuid) else {
                continue;
            };
            let Some(owner_pos) = self.player_position(uuid) else {
                // Owner disconnected while carrying the parrot — drop the
                // rider entirely rather than respawn it at a stale position;
                // there is no live position to respawn at.
                continue;
            };
            self.spawn_species(rider.entity_type, owner_pos)
                .tame(MobOwner::Player(uuid))
                .set_shoulder_dismount_ticks(0);
        }
    }
}
