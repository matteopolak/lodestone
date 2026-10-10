//! Player interactions, leashes, breeding, and trading for [`super::MobSim`].

use super::*;

impl<'w> MobSim<'w> {
    /// Attaches or detaches a lead between `mob_id` and the player `holder` —
    /// vanilla's own generic entity-interact step's two leash-specific branches (excluding
    /// its sneak-multi-attach branch; see this method's own "not
    /// implemented" note).
    ///
    /// - If `mob_id` is already leashed to `holder`, detaches it (vanilla's
    ///   own "current holder is this player" arm) and reports whether a
    ///   `minecraft:lead` item should be spawned (`creative` mirrors
    ///   vanilla's own "has infinite materials" check, which this sim has no game-mode
    ///   state of its own to answer).
    /// - Else, if `holding_lead` and the mob is not already held by a
    ///   *player* (vanilla's own "current holder is not a player"
    ///   guard — one player cannot steal another's leashed mob just by
    ///   holding a lead), attaches it to `holder`, dropping any existing
    ///   non-player leash first exactly as vanilla's own drop-leash call does
    ///   before its own set-leashed-to call.
    /// - Otherwise refuses: not leashable, no lead in hand, or out of
    ///   [`LEASH_TOO_FAR_DIST`] (vanilla's own "can have a leash attached to"
    ///   check's own snap-distance check).
    ///
    /// **Not implemented**: vanilla's sneak-right-click branch, which
    /// re-parents *every* mob already leashed to `holder` onto whatever
    /// entity was clicked, in one interaction. This only ever moves the one
    /// `mob_id` named — a real gap for a player leashing several animals to
    /// one another, not merely an unlikely input.
    pub fn try_leash(
        &mut self,
        mob_id: i32,
        holder: Uuid,
        holding_lead: bool,
        creative: bool,
    ) -> LeashOutcome {
        let Some(mob) = self.get(mob_id) else {
            return LeashOutcome::Refused;
        };
        if mob.leash_holder() == Some(LeashHolder::Player(holder)) {
            let pos = mob.position();
            self.get_mut(mob_id)
                .expect("just found")
                .set_leash_holder(None);
            let dropped_lead = !creative;
            if dropped_lead {
                // Spawned here, not left to the caller, for the same reason
                // `tick_leashes`' snap branch spawns its own item: one place
                // decides "a lead item now exists in the world", so a future
                // second call site cannot forget it or double it.
                self.spawn_item(
                    "minecraft:lead".parse::<lodestone_model::ResourceKey>().expect("valid key"),
                    pos,
                    Vec3::new(0.0, 0.0, 0.0),
                    lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                        1,
                        lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                    ),
                );
            }
            return LeashOutcome::Detached { dropped_lead };
        }
        if !holding_lead || matches!(mob.leash_holder(), Some(LeashHolder::Player(_))) {
            return LeashOutcome::Refused;
        }
        if !species::is_leashable_species(mob.entity_type()) {
            return LeashOutcome::Refused;
        }
        let mob_pos = mob.position();
        let Some(holder_pos) = self
            .players
            .iter()
            .find(|p| p.identity.as_ref().map(|i| i.uuid) == Some(holder))
            .map(|p| p.perception.position)
        else {
            return LeashOutcome::Refused;
        };
        if dist_sqr(mob_pos, holder_pos).sqrt() > LEASH_TOO_FAR_DIST {
            return LeashOutcome::Refused;
        }
        self.get_mut(mob_id)
            .expect("just found")
            .set_leash_holder(Some(LeashHolder::Player(holder)));
        LeashOutcome::Attached
    }

    /// Right-clicking a fence while holding a lead: re-parents every mob
    /// currently leashed to `holder` (the player) onto a knot at `fence_pos`
    /// — vanilla's own lead-item "bind player mobs" call. Unlike vanilla this never spawns
    /// a fence-knot decoration entity; see [`LeashHolder::Fence`]'s own doc
    /// comment for why, and for what that costs a real client (no visible
    /// knot to render or right-click).
    ///
    /// **Simplified from vanilla's scan**: vanilla's "bind player
    /// mobs" call only
    /// re-parents mobs within a 32-block radius of `fence_pos`; this moves
    /// every mob leashed to `holder` regardless of distance from the fence.
    /// The two coincide in practice — a leashed mob is already capped at
    /// [`LEASH_TOO_FAR_DIST`] (12 blocks) from `holder`, and a player using
    /// this interaction is, by construction, standing at the fence — but a
    /// contrived setup (holder far from the fence, mob far from holder in
    /// the other direction) could observe the difference.
    ///
    /// Returns the ids re-leashed; empty means no mob was leashed to
    /// `holder` at all, matching vanilla's own pass-through result.
    #[cfg(test)]
    pub fn try_leash_to_fence(&mut self, holder: Uuid, fence_pos: BlockPos) -> Vec<i32> {
        let mut moved = Vec::new();
        for mob in &mut self.mobs {
            if mob.leash_holder == Some(LeashHolder::Player(holder)) {
                mob.leash_holder = Some(LeashHolder::Fence(fence_pos));
                moved.push(mob.id);
            }
        }
        moved
    }

    /// Spawns a wandering trader at `pos` with 1–2 leashed llama escorts —
    /// the entity-spawn half of vanilla's wandering-trader spawner's
    /// spawn call.
    /// Returns the trader's id and every llama actually spawned.
    ///
    /// **This is only the "given a spawn position, create the entity group"
    /// half.** Vanilla's own wandering-trader spawner itself is a generic
    /// custom-spawner driven by
    /// the world tick with its own 1200-tick poll, a 24000-tick base delay,
    /// a climbing 25→75% chance, a player-anchored 48-block search for a
    /// meeting-point point-of-interest (falling back to the player), and a
    /// "no wandering trader spawns" biome-tag exclusion — none of which
    /// exists in this crate. That whole cycle belongs beside
    /// [`crate::mob_spawn`]'s existing per-species natural-spawn cap/timer
    /// engine, a file outside this pass's ownership; see this session's
    /// broker note (wandering trader spawn cycle) for the exact shape a
    /// caller there needs.
    ///
    /// **Simplified escort placement.** Vanilla's own "try to spawn llama
    /// for" step
    /// searches up to 10 candidate positions within 4 blocks and can fail to
    /// find space, so "2 attempts" does not guarantee 2 llamas. This always
    /// places both at fixed offsets (`+2, 0, 0` and `-2, 0, 0` from the
    /// trader) with no space check — this sim has no per-cell obstruction
    /// query at the `MobSim` level the way vanilla's own block-getter seam does, and
    /// two llamas beside an already-chosen valid trader spawn are the common
    /// case in practice.
    ///
    /// **Wares are not generated.** Vanilla's own wandering-trader "update
    /// trades" step builds
    /// its offer list from its own buying/uncommon/common wandering-trader
    /// trade-set tables
    /// — this crate has no merchant-offer/trade-table model at all yet (see
    /// the villager-trading work this is deliberately distinct from). A
    /// spawned trader here has no wares and cannot be traded with.
    pub fn spawn_wandering_trader(&mut self, pos: Vec3) -> (i32, Vec<i32>) {
        let trader_id = self
            .spawn_species("minecraft:wandering_trader".parse().expect("valid key"), pos)
            .id();
        let mut llamas = Vec::new();
        for dx in [2.0, -2.0] {
            let llama_id = self
                .spawn_species(
                    "minecraft:trader_llama".parse().expect("valid key"),
                    Vec3::new(pos.x + dx, pos.y, pos.z),
                )
                .id();
            self.get_mut(llama_id)
                .expect("just spawned")
                .set_leash_holder(Some(LeashHolder::Mob(trader_id)));
            llamas.push(llama_id);
        }
        (trader_id, llamas)
    }

    /// A player right-clicked a mob with (or without) an item — vanilla's own
    /// generic mob-interact dispatch reaching each species' own interaction
    /// override, the single producer for taming, sitting,
    /// feeding and breeding.
    ///
    /// # The dispatch order is the specification
    ///
    /// Vanilla's per-species interaction overrides are nested `if` chains that end in
    /// their parent's own version, so *which arm wins* is as much a part of the port as
    /// the constants are. Two orderings that both "tame a wolf" differ
    /// observably: feeding a hurt tame wolf meat must heal it, **not** put it in
    /// love, and only once it is at full health does the same item breed it
    /// (the wolf's own interaction override's first arm, then its parent's
    /// generic animal interaction). This method's arms are in that order and each one
    /// names which vanilla interaction override it comes from, in prose.
    ///
    /// # What is deliberately not here
    ///
    /// Collar dyeing, wolf body armour and its repair, the parrot's poisonous
    /// cookie, and mounting a tame horse. Each needs an item model this crate
    /// does not have (dye components, equipment slots, damage values) or a
    /// passenger model that does not exist.
    ///
    /// Returns [`InteractOutcome::Pass`] when nothing responded, which is the
    /// caller's signal to fall through to whatever it does with an unconsumed
    /// right-click.
    pub fn interact(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        held_item: Option<&ResourceKey>,
    ) -> InteractOutcome {
        let Some(mob) = self.mobs.iter().find(|m| m.id == mob_id) else {
            return InteractOutcome::Pass;
        };
        let species = mob.entity_type().path().to_owned();
        let pos = mob.position();
        let item = held_item.map(|k| k.path().to_owned());
        let item = item.as_deref();

        // Vanilla's own villager interaction override is a full override, not an
        // animal-interaction
        // fall-through: a villager is never tameable, so this has to be a
        // short-circuit ahead of the `tame_mechanism` dispatch below rather
        // than another arm inside it.
        if species == "villager" {
            let profession = mob.profession;
            let level = mob.villager_level;
            let trade_level = lodestone_data::villager_trades::VillagerLevel::new(level)
                .expect("a simulated villager always keeps a level in 1..=5");
            let has_offers = !villager::trades::offers_up_to(profession, trade_level).is_empty();
            let outcome = if matches!(
                profession,
                villager::Profession::None | villager::Profession::Nitwit
            ) || !has_offers
            {
                // No job, or a job this crate has not ported real trades for
                // yet (`villager::trades`' own doc names which professions
                // those are) — an honest `Pass` rather than an empty screen.
                InteractOutcome::Pass
            } else {
                InteractOutcome::OpenTrade { profession, level }
            };
            return outcome;
        }

        // Zombie-villager interaction: only the golden-apple/weakness case
        // gets special handling. A golden apple used without Weakness (the
        // plain success result, which
        // does **not** reduce the stack) and every other item both fall
        // through to the generic dispatch below, which resolves to `Pass`
        // for a zombie villager exactly as its parent's generic interaction does for any
        // non-tameable monster — see `InteractOutcome::ZombieVillagerConversionStarted`'s
        // own doc for why that no-weakness arm is disclosed as `Pass` rather
        // than a distinct variant.
        if species == "zombie_villager" && item == Some("golden_apple") {
            let has_weakness = mob.effects().amplifier_of("minecraft:weakness").is_some();
            if !has_weakness {
                return InteractOutcome::Pass;
            }
            let state = villager::conversion::start_converting(Some(actor.uuid), |bound| {
                self.zombie_conversion_rng.next_int(bound)
            });
            let remaining_ticks = state.remaining_ticks;
            // Vanilla's own zombie-villager-cure sound's own play call:
            // `1.0F + random.nextFloat()` volume, `random.nextFloat() * 0.7F
            // + 0.3F` pitch (vanilla's own "start converting" call).
            let volume = 1.0 + self.zombie_conversion_rng.next_f32();
            let pitch = self.zombie_conversion_rng.next_f32() * 0.7 + 0.3;
            let seed = i64::from(self.zombie_conversion_rng.next_int(i32::MAX));
            if let Some(mob) = self.mobs.iter_mut().find(|m| m.id == mob_id) {
                mob.effects.remove("minecraft:weakness");
                // Vanilla's own difficulty-minus-one-clamped-to-zero formula: `0` on Easy/Normal/Hard
                // (ids 1-3), and this crate tracks no live difficulty integer
                // for a zombie villager's own amplifier calc — see this
                // module's `conversion` doc for the disclosed simplification.
                mob.effects.apply("minecraft:strength", remaining_ticks, 0);
                mob.conversion = Some(state);
            }
            if let Some(effect) = crate::effects::zombie_villager_cure_sound(pos, volume, pitch, seed) {
                self.pending_vocalisations.push(effect);
            }
            return InteractOutcome::ZombieVillagerConversionStarted;
        }

        // Allay interaction: duplication is checked first, then the
        // empty-handed carrying gift. An allay is never tameable, so — like
        // the villager and zombie-villager arms above — this is a
        // short-circuit ahead of the `tame_mechanism` dispatch rather than
        // another case inside it.
        //
        // See `InteractOutcome::AllayDuplicated`'s own doc for the disclosed
        // is dancing substitution the duplication arm makes.
        //
        // **Not modelled here**: taking the item back (an empty-hand
        // right-click on a carrying allay).
        if species == "allay" {
            if item == Some("amethyst_shard")
                && mob.allay_liked_noteblock.is_some_and(|(_, ticks)| ticks > 0)
                && mob.allay_duplication_cooldown <= 0
            {
                self.spawn_species(
                    ResourceKey::from_str("minecraft:allay").expect("static key is valid"),
                    pos,
                )
                .allay_duplication_cooldown = ALLAY_DUPLICATION_COOLDOWN_TICKS;
                if let Some(mob) = self.mobs.iter_mut().find(|m| m.id == mob_id) {
                    mob.allay_duplication_cooldown = ALLAY_DUPLICATION_COOLDOWN_TICKS;
                }
                // This short-circuit returns before the shared
                // `outcome.particle()` tail below runs (the same reason the
                // villager/zombie-villager arms above never reach it
                // either), so vanilla's own allay entity-event handler's status-18 heart burst
                // has to be pushed here directly rather than relying on that
                // generic path.
                self.pending_vocalisations
                    .push(taming_particles("minecraft:heart", pos));
                return InteractOutcome::AllayDuplicated;
            }
            let already_holding = mob.mob.main_hand_item().is_some();
            if already_holding || item.is_none() {
                return InteractOutcome::Pass;
            }
            let given = item.map(str::to_owned);
            if let Some(mob) = self.mobs.iter_mut().find(|m| m.id == mob_id) {
                mob.mob.set_main_hand_item(given);
            }
            return InteractOutcome::ItemGiven;
        }

        // Vanilla's own camel interaction override is a full override, the same "not an
        // animal-interaction fall-through" shape as the villager arm
        // above — a camel is tamed unconditionally
        // (its own "is tamed" check), so unlike the horse family there is no temper
        // roll to gate riding on at all. Only the empty-handed
        // mount-ride half is ported: vanilla's own "secondary use active" check's
        // inventory-GUI branch and its own "is food" check's heal/age-up/love branch both
        // need machinery this crate does not have for this species yet
        // (a horse-style inventory screen, and a `camel` row in
        // `species::breeding_food` — a real, disclosed, still-missing gap,
        // not silently dropped), so any held item is left as `Pass` rather
        // than guessed at.
        if species == "camel" {
            if item == Some("golden_dandelion") {
                return self.interact_age_lock(mob_id, &species);
            }
            if mob.is_baby() || item.is_some() {
                return InteractOutcome::Pass;
            }
            return if self.mount_mob(mob_id, actor.entity_id) {
                InteractOutcome::Mounted
            } else {
                // Already ridden by someone else — `mount_mob`'s own "one
                // map's worry" refusal.
                InteractOutcome::Pass
            };
        }

        if let Some(done) = self.interact_cosmetic(mob_id, actor, item, &species) {
            return done;
        }

        let outcome = match species::tame_mechanism(&species) {
            Some(species::TameMechanism::Temper { max_temper }) => {
                self.interact_horse(mob_id, actor, item, max_temper)
            }
            Some(mechanism) => self.interact_tamable(mob_id, actor, item, &species, mechanism),
            // Every other species goes straight to vanilla's own generic
            // animal interaction.
            None => self.interact_animal(mob_id, item, &species),
        };
        let outcome = if outcome == InteractOutcome::Pass && item == Some("golden_dandelion") {
            self.interact_age_lock(mob_id, &species)
        } else {
            outcome
        };

        // Vanilla's particles are an entity-status broadcast with status
        // `6`, `7`, or `18`,
        // which the *client* expands into a burst
        // (vanilla's own taming/love-mode particle spawners). This server has no `ENTITY_EVENT` encoder, so the burst is published
        // directly as a `LEVEL_PARTICLES` packet with the same particle type,
        // count and Gaussian spread the client would have produced. A disclosed
        // substitution, not an approximation of the visual: seven heart or smoke
        // particles at a randomized offset plus half a block of height either way.
        if let Some(particle) = outcome.particle() {
            self.pending_vocalisations
                .push(taming_particles(particle, pos));
        }
        outcome
    }

    /// Shearing a sheep, and dyeing a sheep's wool or a tamed wolf's or cat's
    /// collar. `None` when the held item does nothing to this mob, so the
    /// ordinary chain still runs.
    fn interact_cosmetic(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        species: &str,
    ) -> Option<InteractOutcome> {
        let item = item?;
        let tick = self.tick_count;
        let mob = self.mobs.iter().find(|m| m.id == mob_id)?;
        if item == "shears" && species == "sheep" {
            if mob.appearance.sheared || mob.is_baby() {
                return None;
            }
            let color = mob.appearance.wool;
            let pos = mob.position();
            let count = 1 + (tick.wrapping_add(u64::from(mob.uuid.as_u128() as u32)) % 3) as u8;
            let wool: ResourceKey = format!("minecraft:{}_wool", appearance::DYE_NAMES[usize::from(color & 0x0F)])
                .parse()
                .expect("a dye name makes a valid wool key");
            self.get_mut(mob_id)?.appearance.sheared = true;
            for _ in 0..count {
                self.spawn_item(
                    wool.clone(),
                    Vec3::new(pos.x, pos.y + 1.0, pos.z),
                    Vec3::new(0.0, 0.2, 0.0),
                    ItemLifecycle::newly_dropped(1, 64),
                );
            }
            return Some(InteractOutcome::Sheared);
        }
        let dye = appearance::dye_of_item(item)?;
        match species {
            "sheep" => {
                if mob.appearance.sheared || mob.appearance.wool == dye {
                    return None;
                }
                self.get_mut(mob_id)?.appearance.wool = dye;
                Some(InteractOutcome::Dyed { color: dye })
            }
            "wolf" | "cat" => {
                if !mob.is_tame()
                    || mob.owner_uuid() != Some(actor.uuid)
                    || mob.appearance.collar == dye
                {
                    return None;
                }
                self.get_mut(mob_id)?.appearance.collar = dye;
                Some(InteractOutcome::Dyed { color: dye })
            }
            _ => None,
        }
    }

    /// Applies a name tag carrying `name` to the mob: it keeps the name, and a
    /// named mob never despawns. `false` (item kept) for a mob that does not
    /// exist.
    pub fn apply_name_tag(&mut self, mob_id: i32, name: lodestone_core::Nbt) -> bool {
        let Some(mob) = self.get_mut(mob_id) else {
            return false;
        };
        mob.appearance.custom_name = Some(name);
        mob.set_persistence_required(true);
        true
    }

    /// Vanilla's own wolf/cat/parrot interaction overrides — the tameable-animal chain.
    fn interact_tamable(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        species: &str,
        mechanism: species::TameMechanism,
    ) -> InteractOutcome {
        let species::TameMechanism::FoodRoll {
            items,
            one_in,
            sit_on_success,
        } = mechanism
        else {
            return InteractOutcome::Pass;
        };
        let Some(mob) = self.mobs.iter().find(|m| m.id == mob_id) else {
            return InteractOutcome::Pass;
        };

        if mob.is_tame() {
            // Vanilla's own "is owned by" check — a tame animal ignores everyone but its owner.
            // Vanilla's cat wraps its whole body in this check and the wolf
            // repeats it per arm; the effect is the same.
            if mob.owner_uuid() != Some(actor.uuid) {
                return InteractOutcome::Pass;
            }
            // Vanilla's own wolf interaction override's first arm: is-food and
            // health-below-max → feed. **Before** the breeding arm, which is
            // reached only through its parent's generic interaction.
            let is_food = item.is_some_and(|i| species::breeding_food(species).contains(&i));
            if is_food && mob.health() < mob.max_health() {
                let heal = species::tame_feed_heal(species);
                let mob = self.get_mut(mob_id).expect("checked above");
                mob.heal(heal);
                return InteractOutcome::Fed;
            }
            // Its parent's generic animal interaction's love arm.
            if is_food && self.try_set_in_love(mob_id) {
                return InteractOutcome::InLove;
            }
            // Vanilla's own "did not consume the action, and is owned by the
            // player" check flips the sitting order. The *last* arm, so anything
            // above it suppresses the toggle — which is why an owner feeding a
            // hurt pet does not also sit it down.
            let mob = self.get_mut(mob_id).expect("checked above");
            let sitting = !mob.is_ordered_to_sit();
            mob.set_ordered_to_sit(sitting);
            return InteractOutcome::SitToggled { sitting };
        }

        // Untamed. The taming item is checked first and it is **not** the food
        // tag for the wolf: the bone item.
        if item.is_some_and(|i| items.contains(&i)) {
            // Vanilla's own wolf interaction override's "is not angry" guard. The cat and the
            // parrot have no such gate, and `anger` is `None` for them anyway,
            // so this is one condition rather than a per-species branch.
            if self.get(mob_id).is_some_and(|m| m.anger.is_some()) {
                return InteractOutcome::Pass;
            }
            // Vanilla's own "try to tame" step: one bounded-int draw, success on exactly `0`.
            let success = self.tame_rng.next_int(one_in) == 0;
            let mob = self.get_mut(mob_id).expect("checked above");
            if success {
                mob.tame(MobOwner::Player(actor.uuid));
                // Vanilla's own navigation-stop plus target-clear, then, for
                // the wolf and
                // the cat only, its own sit-order setter.
                mob.set_attack_target(None);
                mob.set_attack_target_id(None);
                if sit_on_success {
                    mob.set_ordered_to_sit(true);
                }
                return InteractOutcome::Tamed;
            }
            return InteractOutcome::TameFailed;
        }

        // Still its parent's generic animal interaction: an **untamed** wolf
        // fed meat really does fall in love in vanilla, because the bone check
        // above did not match and the chain continues.
        if item.is_some_and(|i| species::breeding_food(species).contains(&i))
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }
        InteractOutcome::Pass
    }

    /// Vanilla's own horse-family interaction override → its own "handle eating" step.
    ///
    /// The horse family's whole mechanism is a persisted counter, so this arm
    /// makes **no** tame roll: feeding raises `Temper` and nothing else. The roll
    /// lives in [`attempt_horse_tame`](Self::attempt_horse_tame), which
    /// the run-around-while-ridden rule drives while a player is riding.
    fn interact_horse(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        max_temper: i32,
    ) -> InteractOutcome {
        let Some(item) = item else {
            // An empty-handed right-click is vanilla's own mount-ride call — vanilla's only
            // route to the tame roll (on an untamed horse; see
            // `attempt_horse_tame`'s doc for the one disclosed deviation) and,
            // now that a passenger model exists, to actually boarding a tamed
            // one. A baby is excluded exactly as vanilla's own
            // "is a vehicle, or is a baby" guard at the top of its interaction
            // override
            // routes it to its parent's generic animal interaction instead, which has no
            // empty-handed arm at all.
            let Some(mob) = self.get(mob_id) else {
                return InteractOutcome::Pass;
            };
            if mob.is_baby() {
                return InteractOutcome::Pass;
            }
            return if self.mount_mob(mob_id, actor.entity_id) {
                InteractOutcome::Mounted
            } else {
                // Already ridden by someone else.
                InteractOutcome::Pass
            };
        };

        // Vanilla's own "handle eating" step's arms in order: heal, age-up,
        // love, temper. Love is
        // gated on tamed, age exactly zero, and not already in love, and only the two
        // gold items reach it.
        let mut used = false;
        if species::horse_breeding_items(item)
            && self.get(mob_id).is_some_and(SimMob::is_tame)
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }

        let gain = species::horse_temper_gain(item);
        let mob = match self.get_mut(mob_id) {
            Some(mob) => mob,
            None => return InteractOutcome::Pass,
        };
        // `if (temper > 0 && (itemUsed || !is_tamed()) && get_temper() <
        // get max temper)`. `hay_block` has `temper == 0` and so raises nothing,
        // however much of it you feed — the trap `horse_temper_gain` documents.
        if gain > 0 && mob.temper() < max_temper {
            let raised = (mob.temper() + gain).clamp(0, max_temper);
            mob.set_temper(raised);
            used = true;
        }
        if used {
            let temper = mob.temper();
            InteractOutcome::TemperRaised { temper }
        } else {
            InteractOutcome::Pass
        }
    }

    /// Vanilla's own generic animal interaction for a species with no taming at all — the cow, sheep,
    /// pig, chicken and rabbit route, and the only thing feeding them does.
    fn interact_animal(
        &mut self,
        mob_id: i32,
        item: Option<&str>,
        species: &str,
    ) -> InteractOutcome {
        if item.is_some_and(|i| species::breeding_food(species).contains(&i))
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }
        InteractOutcome::Pass
    }

    /// The golden dandelion on a baby: toggles its growth lock. Refused for the
    /// species the `cannot_be_age_locked` entity-type tag lists (the two
    /// undead horses and the villager) and while the previous use's cooldown
    /// is still running.
    fn interact_age_lock(&mut self, mob_id: i32, species: &str) -> InteractOutcome {
        if matches!(species, "zombie_horse" | "skeleton_horse" | "villager") {
            return InteractOutcome::Pass;
        }
        let Some(mob) = self.get_mut(mob_id) else {
            return InteractOutcome::Pass;
        };
        if !mob.mob.can_toggle_age_lock() {
            return InteractOutcome::Pass;
        }
        InteractOutcome::AgeLockToggled { locked: mob.mob.toggle_age_lock() }
    }

    /// Vanilla's own generic animal interaction's love arm as a single testable condition:
    /// age exactly zero and can-fall-in-love, then its own "set in love" call.
    ///
    /// **`age == 0` exactly**, not `!is_baby()`. The two differ on a parent
    /// inside its post-breeding cooldown, whose age is a positive countdown: it
    /// is not a baby and it still cannot fall in love. Reading `!is_baby()` here
    /// would let a pair breed every 60 ticks forever.
    fn try_set_in_love(&mut self, mob_id: i32) -> bool {
        let Some(mob) = self.get_mut(mob_id) else {
            return false;
        };
        if mob.age() != 0 || mob.is_in_love() || mob.mob.carries_egg() {
            return false;
        }
        mob.set_in_love();
        true
    }

    /// Resolves every untamed mount that tried to throw its rider this tick: a
    /// player rider tames it when a draw under the mount's maximum temper falls
    /// below its temper, and otherwise raises its temper by five; either way a
    /// failure throws the rider off, rears the mount, and queues the ejection
    /// for the rider's connection.
    pub(super) fn resolve_bucks(&mut self) {
        let bucking: Vec<i32> = self.mobs.iter_mut().filter_map(|m| m.mob.take_buck().then_some(m.id)).collect();
        for id in bucking {
            let Some(mount) = self.get(id) else { continue };
            let (Some(rider), false) = (mount.rider, mount.is_tame()) else { continue };
            let max_temper = match species::tame_mechanism(mount.entity_type().path()) {
                Some(species::TameMechanism::Temper { max_temper }) => max_temper,
                _ => 0,
            };
            let temper = mount.temper();
            let uuid = self
                .players
                .iter()
                .filter_map(|p| p.identity)
                .find(|identity| identity.entity_id == rider)
                .map(|identity| identity.uuid);
            if let Some(uuid) = uuid {
                if max_temper > 0 && self.tame_rng.next_int(max_temper) < temper {
                    if let Some(mount) = self.get_mut(id) {
                        mount.tame(MobOwner::Player(uuid));
                    }
                    continue;
                }
                if let Some(mount) = self.get_mut(id) {
                    mount.set_temper((temper + 5).clamp(0, max_temper.max(temper)));
                }
            }
            if let Some(mount) = self.get_mut(id) {
                mount.rider = None;
                lodestone_entity::ai::MobController::rear_up(&mut mount.mob);
            }
            self.pending_ejections.push((id, rider));
        }
    }

    /// Vanilla's own "run around like crazy" goal's per-tick tame roll for the horse family:
    /// a bounded-int-under-max-temper draw, tested against the current temper, and on failure
    /// a `+5` temper modification plus its own "make mad" call.
    ///
    /// # The one disclosed deviation, and why it is not silent
    ///
    /// Vanilla reaches this roll from a **goal** that runs while a player is a
    /// passenger, gated on its own `random.nextInt(adjusted_tick_delay(50)) == 0`
    /// — so a rider gets roughly one attempt every 25 ticks until the horse
    /// yields. This server has no passenger model at all, so there is nothing to
    /// stay mounted on and no goal to tick. The attempt is therefore made **once
    /// per mount attempt** (one empty-handed right-click), and the 1-in-50 outer
    /// gate is not drawn.
    ///
    /// What is *not* changed is the part that makes the horse a different
    /// mechanism from the wolf: the roll is still `nextInt(maxTemper) < temper`,
    /// still fails at temper `0` with certainty, and still adds 5 temper per
    /// failure — so a horse still has to be fed or ridden repeatedly, and the
    /// number of attempts it takes is vanilla's. Only the *pacing* differs.
    pub fn attempt_horse_tame(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        max_temper: i32,
    ) -> InteractOutcome {
        let Some(mob) = self.get(mob_id) else {
            return InteractOutcome::Pass;
        };
        if mob.is_tame() || max_temper <= 0 {
            return InteractOutcome::Pass;
        }
        let temper = mob.temper();
        let success = self.tame_rng.next_int(max_temper) < temper;
        let mob = self.get_mut(mob_id).expect("checked above");
        if success {
            // Vanilla's own "tame with name" call: sets owner plus the tame
            // flag. Note it does **not**
            // order the horse to sit — horses have no sitting pose at all.
            mob.tame(MobOwner::Player(actor.uuid));
            InteractOutcome::Tamed
        } else {
            let raised = (temper + 5).clamp(0, max_temper);
            mob.set_temper(raised);
            InteractOutcome::TameFailed
        }
    }

    /// Turns each drained [`NavigatingMob::take_bred`] event into a real child
    /// mob and applies vanilla's post-breeding cooldown to **both** parents.
    ///
    /// Vanilla's own post-breeding child-finalization step
    /// does three things: sets the post-breeding age cooldown on both parents,
    /// resets love on both, and spawns the child. `NavigatingMob::breed` can
    /// only do the love reset on the mob that ran the goal — it has no notion
    /// of the partner or of creating an entity — so the other two are here.
    ///
    /// Identifying the partner is the interesting part: by the time this runs,
    /// `breed()` has already cleared the breeder's love state, so "the other
    /// mob still in love" is not a usable key. It uses proximity instead —
    /// vanilla only breeds when the pair is within
    /// [`BREED_DISTANCE_SQR`](BREED_DISTANCE_SQR) (the breeding goal's own
    /// squared-distance check),
    /// so the nearest same-species adult inside that radius *is* the partner.
    pub(super) fn resolve_breeding(&mut self, bred: Vec<(i32, Vec3, ResourceKey)>) {
        if bred.is_empty() {
            return;
        }
        // A mob already consumed as someone else's partner must not breed
        // again this tick. Both animals of a pair can legitimately reach
        // `loveTime >= 60` on the same tick — each holds the other as its
        // partner candidate — and without this guard one mating produces two
        // children, doubling the population every time.
        let mut consumed: std::collections::HashSet<i32> = std::collections::HashSet::new();
        for (breeder_id, breeder_pos, species) in bred {
            if consumed.contains(&breeder_id) {
                continue;
            }
            let partner_id = self
                .mobs
                .iter()
                .filter(|m| {
                    m.id != breeder_id
                        && m.entity_type().path() == species.path()
                        && !m.is_baby()
                        && !consumed.contains(&m.id)
                        && dist_sqr(m.position(), breeder_pos) < BREED_DISTANCE_SQR
                })
                .min_by(|a, b| {
                    dist_sqr(a.position(), breeder_pos)
                        .total_cmp(&dist_sqr(b.position(), breeder_pos))
                })
                .map(SimMob::id);

            consumed.insert(breeder_id);
            for id in [Some(breeder_id), partner_id].into_iter().flatten() {
                consumed.insert(id);
                if let Some(m) = self.get_mut(id) {
                    m.set_age(PARENT_AGE_AFTER_BREEDING);
                    m.mob.reset_love();
                }
            }

            // A turtle pair produces an egg for the breeder to lay, not a child.
            if species.path() == "turtle" {
                if let Some(m) = self.get_mut(breeder_id) {
                    m.mob.restore_has_egg(true);
                }
                if self.mob_drops {
                    let value = self.breed_rng.next_int(7) + 1;
                    self.spawn_orb(value, breeder_pos, Vec3::new(0.0, 0.0, 0.0));
                }
                continue;
            }

            // The child spawns through `spawn_species`, not `spawn_with_type`,
            // so it inherits the same goal set and category any other mob of
            // its species gets — a child that could not act would be a fresh
            // island of exactly the kind this connectivity check exists to close.
            let parents = partner_id.and_then(|partner| {
                let breeder = self.get(breeder_id)?;
                let owner = breeder.is_tame().then(|| breeder.owner()).flatten();
                Some((breeder.appearance_copy(), self.get(partner)?.appearance_copy(), owner))
            });
            let child = self.spawn_species(species, breeder_pos);
            child.set_age(BABY_START_AGE);
            let child_id = child.id();
            // A bred baby takes its variant from its parents, not a wild roll;
            // a tamed wolf or cat's baby is born tame to the same owner.
            if let Some((a, b, owner)) = parents {
                let mut rng = self.breed_rng.clone();
                if let Some(child) = self.get_mut(child_id) {
                    child.inherit_appearance(&a, &b, &mut rng);
                    if let (Some(owner), "wolf" | "cat") = (owner, child.entity_type().path()) {
                        child.tame(owner);
                    }
                }
                self.breed_rng = rng;
            }

            // Vanilla's own post-breeding child-finalization step's last statement:
            // if the `mob_drops` gamerule is set, spawn an experience orb worth
            // a bounded-int-under-7-plus-1 draw.
            //
            // **Constructed, not awarded**, and the distinction is visible:
            // vanilla's own orb-award helper splits an amount into denominations and tries
            // its own merge-to-existing step first, whereas breeding builds one orb with
            // one value directly. Routing this through `award_experience` would
            // let a second mating in the same spot silently fold into the first
            // orb, so `spawn_orb` is the right call even though the values are
            // small enough that denomination splitting would be a no-op.
            //
            // The gate is the `mob_drops` rule, exactly as for a mob's death
            // reward — breeding on a `doMobLoot false` server pops nothing.
            if self.mob_drops {
                let value = self.breed_rng.next_int(7) + 1;
                self.spawn_orb(value, breeder_pos, Vec3::new(0.0, 0.0, 0.0));
            }
        }
    }

    /// Opens a turtle egg block at `pos`: one baby turtle per egg, each with the
    /// egg's cell as its nest.
    pub fn hatch_turtles(&mut self, pos: (i32, i32, i32), eggs: u32) {
        let key: ResourceKey = "minecraft:turtle".parse().expect("static key");
        for i in 0..eggs {
            let at = Vec3::new(f64::from(pos.0) + 0.3 + f64::from(i) * 0.2, f64::from(pos.1), f64::from(pos.2) + 0.3);
            let turtle = self.spawn_species(key.clone(), at);
            turtle.set_age(BABY_START_AGE);
            turtle.mob.set_nest(Some(Vec3::new(f64::from(pos.0) + 0.5, f64::from(pos.1), f64::from(pos.2) + 0.5)));
        }
    }

    /// Runs [`tick`](MobSim::tick) `n` times.
    pub fn tick_for(&mut self, n: u64) {
        for _ in 0..n {
            self.tick();
        }
    }
}
