//! Construction, configuration, and mob spawning for [`super::MobSim`].

use super::*;

impl<'w> MobSim<'w> {
    /// Creates an empty simulation over `world`.
    #[must_use]
    pub fn new(world: &'w ChunkWorld) -> Self {
        Self {
            world,
            mobs: Vec::new(),
            projectiles: ProjectileRegistry::new(),
            projectile_meta: HashMap::new(),
            items: ItemEntityRegistry::new(),
            item_state: HashMap::new(),
            orbs: HashMap::new(),
            orb_rng: SpawnRng::new(orbs::ORB_BEHAVIOR_SEED),
            equipment_rng: SpawnRng::new(EQUIPMENT_ROLL_SEED),
            goat_horn_rng: SpawnRng::new(GOAT_HORN_ROLL_SEED),
            door_rng: SpawnRng::new(DOOR_BREAK_ROLL_SEED),
            spawn_special_multiplier: 0.0,
            spawn_hard_difficulty: false,
            spawn_monsters_enabled: false,
            reinforcement_rng: SpawnRng::new(REINFORCEMENT_ROLL_SEED),
            pending_reinforcements: Vec::new(),
            falling_blocks: HashMap::new(),
            next_id: 1,
            tick_count: 0,
            item_owner_plan: 0,
            applied_item_owner_plan: 0,
            item_handoff: EntityOwnershipHandoff::default(),
            orb_owner_plan: 0,
            applied_orb_owner_plan: 0,
            orb_handoff: EntityOwnershipHandoff::default(),
            burn_owner_plan: 0,
            applied_burn_owner_plan: 0,
            leash_owner_plan: 0,
            applied_leash_owner_plan: 0,
            tnt_owner_plan: 0,
            applied_tnt_owner_plan: 0,
            vehicle_owner_plan: 0,
            applied_vehicle_owner_plan: 0,
            minecart_owner_plan: 0,
            applied_minecart_owner_plan: 0,
            fishing_owner_plan: 0,
            fishing_applied_owner_plan: 0,
            projectile_owner_plan: 0,
            applied_projectile_owner_plan: 0,
            item_probe_count: 0,
            pending_detonations: Vec::new(),
            pending_grazes: Vec::new(),
            pending_player_hits: Vec::new(),
            pending_mining_fatigue: Vec::new(),
            pending_vocalisations: Vec::new(),
            pending_ambient_sounds: Vec::new(),
            pending_animations: Vec::new(),
            players: Vec::new(),
            sleeping_players: Vec::new(),
            shoulder_riders: HashMap::new(),
            tame_rng: SpawnRng::new(TAME_ROLL_SEED),
            zombie_conversion_rng: SpawnRng::new(ZOMBIE_VILLAGER_CONVERSION_SEED),
            gossip_spread_rng: SpawnRng::new(GOSSIP_SPREAD_SEED),
            breed_rng: SpawnRng::new(BREED_XP_SEED),
            mob_drops: true,
            vehicles: HashMap::new(),
            tnt: HashMap::new(),
            minecarts: HashMap::new(),
            tnt_rng: SpawnRng::new(tnt::TNT_LAUNCH_SEED),
            // Vanilla's own field default (`private int nextTick;`, never
            // explicitly initialised, so Java's `0`) — the very first call
            // sees `nextTick <= 0` and may attempt a patrol on tick one,
            // subject to every other gate still applying.
            patrol_next_tick: 0,
            patrol_rng: SpawnRng::new(PATROL_SPAWN_SEED),
            // Vanilla's own field default (`private int tickDelay = 1200;`)
            // — the constructor sets it explicitly, unlike `nextTick`, so
            // the first call does not roll before tick 1200.
            trader_tick_delay: WANDERING_TRADER_TICK_DELAY,
            trader_spawn_delay: WANDERING_TRADER_SPAWN_DELAY,
            trader_spawn_chance: WANDERING_TRADER_MIN_SPAWN_CHANCE,
            trader_rng: SpawnRng::new(WANDERING_TRADER_SPAWN_SEED),
            lightning_bolts: HashMap::new(),
            pending_lightning_fires: Vec::new(),
            pending_projectile_block_hits: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            workstation_claims: villager::WorkstationClaims::new(),
            #[cfg(not(target_arch = "wasm32"))]
            bed_claims: villager::BedClaims::new(),
            #[cfg(not(target_arch = "wasm32"))]
            bell_claims: villager::BellClaims::new(),
            day_time: 0,
            posted_vibrations: Vec::new(),
            dragons: HashMap::new(),
            withers: HashMap::new(),
            wither_rng: SpawnRng::new(wither::WITHER_SKULL_SEED),
            crystals: HashMap::new(),
            dragon_rng: SpawnRng::new(dragon::DRAGON_PHASE_SEED),
            fishing_bobbers: HashMap::new(),
            fishing_rng: SpawnRng::new(fishing::FISHING_ROLL_SEED),
            raids: HashMap::new(),
            next_raid_id: 1,
            pending_hero_grants: Vec::new(),
            raid_rng: SpawnRng::new(raid::RAID_ROLL_SEED),
            dragon_fight: None,
            dragon_gateways: None,
            gateway_shuffle_rng: SpawnRng::new(GATEWAY_SHUFFLE_SEED),
            pending_dragon_deaths: Vec::new(),
        }
    }

    /// Replaces the RNG the taming mechanisms draw from — the injection point a
    /// tame-chance gate needs.
    ///
    /// A tame *chance* cannot be gated by observing that taming sometimes
    /// happens; that measures only that the code runs. Seed this with a stream
    /// whose first draw is known and the outcome becomes a prediction. The draw
    /// order and count are part of the specification, so a gate that reseeds
    /// between attempts is also asserting how many draws each mechanism makes.
    pub fn set_tame_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.tame_rng = rng;
        self
    }

    /// Sets the `DifficultyInstance` inputs every subsequent
    /// [`spawn_species`](Self::spawn_species) call feeds to
    /// [`lodestone_entity::spawn_equipment::populate_default_equipment_slots`]'s
    /// armour-upgrade roll: `special_multiplier` (`DifficultyInstance
    /// ::getSpecialMultiplier`, `0.0`..`1.0`) and whether the world's base
    /// difficulty is Hard.
    ///
    /// `crate::tick::run_tick_loop` is the real production caller — it
    /// resolves a `DifficultyInstance` (from world difficulty, game time and
    /// moon phase) once per tick and feeds
    /// [`DifficultyInstance::special_multiplier`](crate::regional_difficulty::DifficultyInstance::special_multiplier)
    /// and [`DifficultyInstance::is_hard`](crate::regional_difficulty::DifficultyInstance::is_hard)-shaped
    /// values here. Left at the `0.0`/`false` defaults, a spawn never rolls
    /// armour, which is vanilla's own behaviour for a fresh world's effective
    /// difficulty (below `2.0`).
    pub fn set_spawn_difficulty(&mut self, special_multiplier: f32, hard: bool) -> &mut Self {
        self.spawn_special_multiplier = special_multiplier;
        self.spawn_hard_difficulty = hard;
        self
    }

    /// Vanilla's own "is spawning monsters" check — the `spawn_mobs` game rule, gating
    /// [`attack`](Self::attack)'s zombie hurt-handler reinforcement roll
    /// alongside [`set_spawn_difficulty`](Self::set_spawn_difficulty)'s
    /// `hard` flag. `false` by default, matching every other spawn-difficulty
    /// input here: an unwired caller sees zero reinforcements rather than
    /// silently-always-on ones.
    pub fn set_spawn_monsters_enabled(&mut self, enabled: bool) -> &mut Self {
        self.spawn_monsters_enabled = enabled;
        self
    }

    /// Host injection point: the real world time-of-day, `0..24000` — see
    /// [`day_time`](Self::day_time)'s own field doc for what this feeds and
    /// why. `crate::tick::run_tick_loop` is the real production caller,
    /// reading `WorldState::time().day_time` (reduced mod 24000) once per
    /// tick, ahead of `tick_with_terrain`.
    pub fn set_day_time(&mut self, day_time: i32) -> &mut Self {
        self.day_time = day_time;
        self
    }

    /// Replaces the RNG [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)
    /// draws from — the injection point a patrol-spawn gate needs, for the same
    /// reason [`set_tame_rng`](Self::set_tame_rng) exists.
    pub fn set_patrol_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.patrol_rng = rng;
        self
    }

    /// Replaces the RNG
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// draws from — the injection point a trader-spawn gate needs, for the
    /// same reason [`set_patrol_rng`](Self::set_patrol_rng) exists.
    pub fn set_trader_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.trader_rng = rng;
        self
    }

    /// Overwrites [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)'s
    /// two nested countdowns directly — the injection point a gate needs to
    /// stage a sim past the 1200-tick poll and the 24000-tick delay without
    /// calling the cycle that many times. `0` for either drives the *next*
    /// call straight to the roll, matching vanilla's own `<= 0` checks.
    pub fn set_trader_timers(&mut self, tick_delay: i32, spawn_delay: i32) -> &mut Self {
        self.trader_tick_delay = tick_delay;
        self.trader_spawn_delay = spawn_delay;
        self
    }

    /// Overwrites [`tick_count`](Self::tick_count) directly — the injection
    /// point a gate needs to stage the sim past
    /// [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)'s timeline
    /// gate without actually ticking 120,000 times. Mirrors
    /// [`SimMob::set_temper`]'s reason: staging state a real playthrough
    /// would only reach by repetition.
    pub fn set_tick_count(&mut self, tick_count: u64) -> &mut Self {
        self.tick_count = tick_count;
        self
    }

    /// How many cells the **last** tick's item-settling pass asked the collision
    /// oracle about.
    ///
    /// This is the cost of routing items through swept collision, in the one unit
    /// that survives being read on a machine with four other builds running. It
    /// scales with item count and with how fast each item is moving (a faster item
    /// sweeps a longer box), so it is also the number that says whether a floor
    /// covered in drops can eat a tick — the question a per-item measurement
    /// structurally cannot answer.
    #[must_use]
    pub fn items_settled_probe_count(&self) -> u64 {
        self.item_probe_count
    }

    /// This villager's accumulated trading xp — `crate::server`'s own
    /// consumer for the `MERCHANT_OFFERS` packet's `villager_xp` field,
    /// alongside the `profession`/`level` an [`InteractOutcome::OpenTrade`]
    /// already carries. `0` for a non-villager or an unknown id — a
    /// harmless default rather than a panic, the same convention every
    /// other by-id accessor in this file uses.
    #[must_use]
    pub fn villager_xp(&self, mob_id: i32) -> i32 {
        self.mobs
            .iter()
            .find(|m| m.id == mob_id)
            .map_or(0, |m| m.villager_xp)
    }

    /// This villager's priced offer list for one moment in time, backed by
    /// its *persistent* [`crate::villager_trade::VillagerTrades`]. The
    /// persistent third trade-state field, demand, and uses persist between
    /// menu opens, while reputation and Hero of
    /// the Village are folded into a clone of each offer's price
    /// (`reset_special_price_diff` first, matching
    /// [`crate::mobs::villager::reputation::update_special_prices`]'s own
    /// contract): the *persisted* `special_price_diff` never accumulates
    /// across menu opens, only the persisted `uses`/`demand` do.
    ///
    /// Empty for a non-villager, an unknown id, or an unemployed villager —
    /// see [`SimMob::ensure_trades`].
    #[must_use]
    pub fn villager_offers(
        &mut self,
        mob_id: i32,
        reputation: i32,
        hero_of_the_village_amplifier: Option<u32>,
    ) -> Vec<crate::villager_trade::OfferState> {
        let Some(m) = self.get_mut(mob_id) else {
            return Vec::new();
        };
        let Some(trades) = m.ensure_trades() else {
            return Vec::new();
        };
        let mut offers = trades.offers.clone();
        for offer in &mut offers {
            offer.reset_special_price_diff();
        }
        villager::reputation::update_special_prices(&mut offers, reputation, hero_of_the_village_amplifier);
        offers
    }

    /// Executes a purchase against this villager's *persistent* offer at
    /// `index` — [`crate::villager_trade::VillagerTrades::try_trade`]'s
    /// first production caller. Pricing is computed the same way
    /// [`Self::villager_offers`] displays it (reset then
    /// re-discounted from `reputation`/`hero_of_the_village_amplifier`), so
    /// what a player sees is what they pay; [`OfferState::take`] then
    /// enforces the live, persisted cost *and* out-of-stock state for real,
    /// where every previous caller always saw a fresh `uses: 0` offer no
    /// matter how many times it had been bought.
    ///
    /// On success, feeds the trade's xp reward into
    /// [`SimMob::give_villager_xp`] — vanilla's own "notify trade" path,
    /// also previously unreached, which is why no villager could level up.
    /// Returns `None` — nothing mutated — for an unknown mob/villager, an
    /// out-of-range index, or a refused (out-of-stock/unsatisfied) offer.
    pub fn try_villager_trade(
        &mut self,
        mob_id: i32,
        index: usize,
        reputation: i32,
        hero_of_the_village_amplifier: Option<u32>,
    ) -> Option<crate::villager_trade::TradeTake> {
        let m = self.get_mut(mob_id)?;
        let trades = m.ensure_trades()?;
        let offer = trades.offers.get_mut(index)?;
        offer.reset_special_price_diff();
        let mut priced = [*offer];
        villager::reputation::update_special_prices(&mut priced, reputation, hero_of_the_village_amplifier);
        *offer = priced[0];
        let cost_a = offer.modified_cost_a_count();
        let cost_b = offer.record.wants_b.map_or(0, |(_, count)| count);
        let take = trades.try_trade(index, cost_a, cost_b)?;
        m.give_villager_xp(take.xp);
        Some(take)
    }

    /// Replaces the set of players mob perception can see, for
    /// [`tick`](Self::tick) to consume. The world tick calls this setter with
    /// player positions before goal evaluation, allowing
    /// [`MobController::nearest_player`] and [`MobController::temptation`] to
    /// read the shared perception input.
    ///
    /// # Why the parameter is generic
    ///
    /// It accepts anything that converts into a [`PerceivedPlayer`], which in
    /// practice means a `Vec<PerceivedPlayer>` **or** a bare
    /// `Vec<PlayerPerception>`. Both shapes are wanted at once and neither is
    /// transitional sugar for the other: taming needs the identity, so the real
    /// producer supplies views, while every gate that only cares where a mob
    /// looks is clearer without a uuid it does not use. A `PlayerPerception`
    /// converts to a view with **no identity**, which is the honest state for a
    /// producer that has none — see [`PerceivedPlayer`].
    pub fn set_players<I, P>(&mut self, players: I) -> &mut Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PerceivedPlayer>,
    {
        self.players = players.into_iter().map(Into::into).collect();
        self
    }

    /// The players mob perception currently sees, with their identities.
    #[must_use]
    pub fn players(&self) -> &[PerceivedPlayer] {
        &self.players
    }

    /// Refreshes the sleeping-player roster — see
    /// [`sleeping_players`](Self::sleeping_players)'s own field doc. The
    /// world tick loop calls this once per tick with
    /// `crate::sleep::SleepState`'s own `(entity id, lay-down tick)` pairs,
    /// the same shared-state join [`set_players`](Self::set_players) already
    /// performs for position.
    pub fn set_sleeping_players(&mut self, sleepers: Vec<(i32, u64)>) -> &mut Self {
        self.sleeping_players = sleepers;
        self
    }

    /// The position of the player with this identity's uuid, if they are in the
    /// current player list — the resolution vanilla's
    /// own entity-reference resolver performs for
    /// its own tamed-animal owner getter.
    ///
    /// Keyed on the **uuid**, never on the entity id, for the reason
    /// [`PlayerIdentity`] gives: the entity id is reassigned per session, so a
    /// pet whose owner reconnects would resolve to whoever inherited that id.
    #[must_use]
    pub(super) fn player_position(&self, uuid: Uuid) -> Option<Vec3> {
        self.players
            .iter()
            .find(|v| v.identity.is_some_and(|id| id.uuid == uuid))
            .map(|v| v.perception.position)
    }

    /// Resolves a [`LeashHolder`] to the wire entity id
    /// [`ServerProtocol::encode_set_entity_link`] needs as its target — the
    /// encoder takes a live entity id,
    /// and this is "which id" for each of the three holder shapes this sim
    /// tracks. Only `MobSim` can answer it — a bare `LeashHolder::Player` carries
    /// a uuid, not a session-scoped entity id, and resolving that needs
    /// `self.players` — which is why it is not a method on [`SimMob`] itself.
    ///
    /// [`LeashHolder::Player`] resolves through the same uuid-keyed lookup
    /// [`player_position`](Self::player_position) uses, for the identical reason
    /// given there: entity ids are reassigned per session, so keying on the uuid
    /// is what keeps a reconnecting owner's leash pointed at the right client.
    ///
    /// [`LeashHolder::Mob`] is already a wire id (`SimMob::id`), so this returns
    /// it verbatim — no lookup needed.
    ///
    /// [`LeashHolder::Fence`] returns `None`: this sim never spawns a
    /// `LeashFenceKnotEntity` (see that variant's own doc comment for why), so
    /// there is no entity id on the wire to link to yet. A mob leashed to a fence
    /// is tracked correctly server-side and draws no rope until a knot entity
    /// exists — a disclosed gap, not a silent one.
    #[must_use]
    pub(super) fn resolve_leash_target(&self, holder: LeashHolder) -> Option<i32> {
        match holder {
            LeashHolder::Player(uuid) => self
                .players
                .iter()
                .find(|v| v.identity.is_some_and(|id| id.uuid == uuid))
                .map(|v| v.identity.expect("just matched").entity_id),
            LeashHolder::Mob(id) => Some(id),
            LeashHolder::Fence(_) => None,
        }
    }

    /// Overrides the id the next [`spawn`](Self::spawn) call assigns (and
    /// every one after it, still incrementing by one each time).
    ///
    /// Exists for a caller that shares its mob ids' wire namespace with a
    /// real protocol's own reserved ids. `MobSim::new`'s default start (`1`)
    /// collided, in production, with `V770ServerProtocol`'s
    /// `LOCAL_PLAYER_ENTITY_ID` (also `1`, `crates/protocol/v770/src/server_protocol.rs`):
    /// a real client never spawns "itself" as a separate `ADD_ENTITY`, so the
    /// very first mob a fresh [`MobSim`] ever spawns silently failed to
    /// appear — found live by `crates/protocol/v770/tests/live_mob_sim.rs`,
    /// which consistently observed 2 of 3 seeded mobs, never 3, until
    /// `run_mob_tick_loop` started calling this. `MobSim::new`'s default is
    /// left unchanged (`1`) so every existing hermetic test keeps its
    /// already-asserted ids stable; only a caller wired to a real wire
    /// protocol needs to call this.
    pub fn set_next_id(&mut self, next_id: i32) -> &mut Self {
        self.next_id = next_id;
        self
    }

    /// The id the next [`spawn`](Self::spawn) call will assign.
    ///
    /// The read side of [`set_next_id`](Self::set_next_id), and it answers one
    /// question nothing else can: **has this sim been reseeded yet?**
    /// [`MobHandle::reseed`] replaces the whole sim and then calls
    /// `set_next_id(1000)`, while [`MobSim::new`] starts at `1` — so a caller that
    /// must not touch a sim about to be thrown away (a saved-entity restore, a
    /// `/summon` racing world open) can tell the difference. Without it, that
    /// caller has to guess, and guessing wrong is silent: the work lands in the
    /// sim that is discarded a moment later.
    #[must_use]
    pub fn next_id(&self) -> i32 {
        self.next_id
    }

    /// Spawns a mob at `pos` with body `shape`, moving `step_per_tick` blocks per
    /// tick (derived from its movement-speed attribute) and an A\* open-set
    /// budget of `visited_budget` (vanilla `floor(followRange * 16)`).
    ///
    /// Returns a mutable handle so the caller can attach goals and a target
    /// before the first tick.
    pub fn spawn(
        &mut self,
        pos: Vec3,
        shape: MobShape,
        step_per_tick: f64,
        visited_budget: i32,
    ) -> &mut SimMob<'w> {
        let entity_type = ResourceKey::from_str("minecraft:zombie").expect("static key is valid");
        self.spawn_with_type(pos, shape, step_per_tick, visited_budget, entity_type)
    }

    /// The shared body of [`spawn`](Self::spawn) and
    /// [`spawn_species`](Self::spawn_species): everything except *which*
    /// `entity_type` (and therefore which [`combat_defaults`]) the new mob
    /// gets.
    fn spawn_with_type(
        &mut self,
        pos: Vec3,
        shape: MobShape,
        step_per_tick: f64,
        visited_budget: i32,
        entity_type: ResourceKey,
    ) -> &mut SimMob<'w> {
        let id = self.next_id;
        self.next_id += 1;
        let (max_health, attack_damage, defenses, knockback_resistance) =
            combat_defaults(&entity_type);
        let is_warden = entity_type.path() == "warden";
        self.mobs.push(SimMob {
            id,
            mob: NavigatingMob::new(self.world, shape, pos, step_per_tick, visited_budget, id as u64),
            goals: GoalSelector::new(),
            category: MobCategory::Monster,
            no_action_time: 0,
            persistent: false,
            uuid: Uuid::new_v4(),
            entity_type,
            equipment: spawn_equipment::EquipmentSlots::default(),
            health: max_health,
            max_health,
            defenses,
            burn: crate::burning::BurnState::new(),
            anger: None,
            stung_at: None,
            piglin_alert_ticks: -1,
            armadillo_danger_ticks: 0,
            axolotl_play_dead_ticks: 0,
            camel_sitting: false,
            camel_pose_tick: 0,
            camel_dash_cooldown: 0,
            sniffer_state: sniffer::SnifferState::Idling,
            sniffer_state_ticks: 0,
            sniffer_sniff_cooldown: 0,
            sniffer_dig_target: None,
            sniffer_explored: Vec::new(),
            allay_liked_noteblock: None,
            allay_inventory_count: 0,
            allay_duplication_cooldown: 0,
            hurt_by_player_until: None,
            attack_damage,
            hurt_cooldown: HurtCooldown::default(),
            ambient_sound_time: 0,
            attack_target_id: None,
            owner: None,
            tame: false,
            ordered_to_sit: false,
            temper: 0,
            knockback_resistance,
            leash_holder: None,
            last_lightning_bolt: None,
            profession: villager::Profession::None,
            workstation: None,
            villager_level: 1,
            villager_xp: 0,
            trades: None,
            job_search_cooldown: 0,
            cat_search_cooldown: 0,
            shoulder_dismount_ticks: 0,
            bed: None,
            bed_search_cooldown: 0,
            meeting_point: None,
            bell_search_cooldown: 0,
            nearest_vibration: None,
            warden_anger: 0,
            warden_anger_target: None,
            warden_emerge_ticks: if is_warden { warden::EMERGE_DURATION_TICKS } else { 0 },
            warden_sonic_boom_cooldown: 0,
            warden_dig_cooldown: if is_warden { warden::DIGGING_COOLDOWN_TICKS } else { 0 },
            warden_digging_ticks: 0,
            has_left_horn: true,
            has_right_horn: true,
            reinforcement_chance: 0.0,
            gossip: villager::gossip::GossipContainer::new(),
            last_gossip_decay_tick: None,
            golem_detected_until: None,
            conversion: None,
            effects: crate::mob_effects::ActiveEffects::new(),
            rider: None,
        });
        self.mobs.last_mut().expect("just pushed")
    }

    /// Spawns a mob of a specific species at `pos`, resolving its body and
    /// behavior from the per-species data tables.
    ///
    /// * **Shape** comes from the 26.2 dimension census
    ///   ([`lodestone_data::entity_dimensions`], keyed by
    ///   [`lodestone_data::entity_type::EntityType::from_name`]) folded with the
    ///   type's `SCALE`/`STEP_HEIGHT` attributes — the same math
    ///   [`crate::resolve_mob_shape`] uses for a version-aware caller, read
    ///   directly here since `MobSim` already depends on `lodestone_data` for
    ///   its path/collision census above. Falls back to `MobShape::land(0.6,
    ///   1.95)` for a species the census does not know by name, matching that
    ///   function's own "explicit fallback, never a silent guess" contract.
    /// * **Combat stats** come from [`combat_defaults`], already species-aware.
    /// * **Speed**: the type's `movement_speed` attribute value feeds
    ///   [`SpeciesContext`](lodestone_entity::ai::roster::SpeciesContext) as-is
    ///   (every roster goal multiplies it by its own speed constants before it
    ///   reaches motion), but the actual kinematic-follower rate handed to
    ///   [`spawn_with_type`] is [`ai_ground_speed`] of that attribute. A bare
    ///   attribute value is not the mob's real blocks/tick rate; see
    ///   `docs/mob-species-spawning.md` for the conversion measurement.
    /// * **Goals** come from [`lodestone_entity::ai::roster`], which resolves the
    ///   species path to a prioritized set. This function does not know
    ///   individual species: a species with no roster entry gets `roster::FALLBACK`
    ///   (wander and look around).
    ///
    ///   The roster connects these behavior goals to production spawning, and
    ///   perception supplied by [`tick`](Self::tick) drives them during a tick.
    ///
    ///   Two consequences worth knowing when reading a mob's behaviour:
    ///   priorities use the roster's absolute values. For example, a creeper's
    ///   swell goal is at priority 2 and its melee goal at priority 4. Melee
    ///   speed is a multiplier on the mob's `movement_speed`; hostile roster
    ///   entries are above the `0.2` lower bound (the slowest entry is a zombie
    ///   at `0.23`).
    pub fn spawn_species(&mut self, entity_type: ResourceKey, pos: Vec3) -> &mut SimMob<'w> {
        let mut attrs = default_attributes(&entity_type).unwrap_or_else(AttributeMap::new);
        // Always spawns adult-shaped; a caller wanting a baby applies
        // `set_age(BABY_START_AGE)` afterward, which re-derives the shape
        // through the same function (see `SimMob::set_age`'s own doc).
        let mut shape = species_shape(&entity_type, &attrs, false);
        // The zombie family's door-breaking is a spawn-time coin flip scaled
        // by regional difficulty, not a species constant, so `species_shape`
        // cannot set it — rolled here, once, on its own RNG stream for the
        // same reason every other spawn-time roll on this sim gets one (see
        // `door_rng`'s own doc). See `docs/mob-species-spawning.md` for the
        // vanilla formula and the "leader zombie" bonus this does not model.
        if matches!(
            entity_type.path(),
            "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin"
        ) {
            shape.can_open_doors = self.door_rng.next_f32() < self.spawn_special_multiplier * 0.1;
        }
        let base_speed = attr(&attrs, "movement_speed");
        // `minecraft:follow_range`, read **once** and fed to both consumers, so
        // target acquisition and the A* budget cannot drift apart.
        //
        // `attr_present` rather than `attr`: for a species `default_attributes`
        // has no template for, `attrs` is empty and `attr` returns the *registry*
        // default of **32.0** — not 0.0, and not a harmless approximation. 32.0
        // is the single value this attribute never legitimately holds, because
        // The generic attribute fallback is 16.0 for every mob, so the registry
        // default is not the effective range. Falling back explicitly to
        // `DEFAULT_FOLLOW_RANGE` keeps an unlisted species usable.
        //
        // Species that raise it do so in their own attribute builder — the
        // zombie family 35.0, blaze 48.0,
        // enderman 64.0 — and `attribute.rs::type_spec` has arms for only
        // thirteen species. So `zombie` gets its real 35.0 here
        // while `zombie_villager` uses the generic 16.0 fallback here.
        // The explicit fallback keeps this behavior visible rather than assumed; the required
        // `type_spec` arms, not a fallback tuned to flatter the zombie family.
        let follow_range = attr_present(&attrs, "follow_range").unwrap_or(DEFAULT_FOLLOW_RANGE);
        let visited_budget = (follow_range * 16.0).floor() as i32;
        let hostile = species::is_hostile_species(&entity_type);
        let built_in_entity_type = EntityType::from_resource_key(&entity_type);

        // The roster still consumes the dynamic key's borrowed path. Closed
        // built-in behavior below uses the generated `EntityType` instead of
        // allocating a second species string and matching its characters.
        let species_path = entity_type.path();

        // Built *before* `entity_type` is moved into the spawn. `SpeciesContext`
        // wants the raw attribute — every roster goal supplies its own
        // speed multiplier on top — so it is *not* `ai_ground_speed`-converted
        // here; the conversion happens once, below, for the kinematic
        // follower's own rate.
        let goals = roster::goals_for(species_path, &SpeciesContext::new(base_speed));

        // Vanilla's own default-equipment-population step — what this mob spawns holding
        // and wearing (`lodestone_entity::spawn_equipment`'s module doc has
        // the full per-species table). Folded into `attrs` *before*
        // `spawn_with_type` reads combat stats from a fresh
        // `default_attributes` call of its own, so the two cannot disagree on
        // the base and only equipment is layered on top here.
        let equipped = match built_in_entity_type {
            Some(species) => spawn_equipment::populate_default_equipment_slots(
                species,
                &mut self.equipment_rng,
                self.spawn_special_multiplier,
                self.spawn_hard_difficulty,
            ),
            None => spawn_equipment::populate_default_equipment_slots_for_extension(
                &mut self.equipment_rng,
                self.spawn_special_multiplier,
                self.spawn_hard_difficulty,
            ),
        };
        equipment::apply_equipment(&mut attrs, equipped.iter());

        // Vanilla's own goat spawn-finalization's own pre-broken-horn roll — see
        // `goat_horn_spawn_roll`'s own doc. Rolled here, before `entity_type`
        // moves into `spawn_with_type` below, for the identical reason
        // `species_path` was captured above.
        let (has_left_horn, has_right_horn) =
            goat_horn_spawn_roll(species_path, &mut self.goat_horn_rng);

        // Vanilla's own reinforcements-chance randomizer — its attribute-handling
        // step calls
        // it for the whole zombie family (husk/drowned/zombie-villager/
        // zombified-piglin all extend the base zombie class and override neither method —
        // the same species list `can_open_doors` above already establishes).
        // Rolled here for the identical reason `has_left_horn`/
        // `has_right_horn` are: before `entity_type` moves into
        // `spawn_with_type` below.
        let reinforcement_chance = if matches!(
            species_path,
            "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin"
        ) {
            self.reinforcement_rng.next_f64() * 0.1
        } else {
            0.0
        };

        let mob = self.spawn_with_type(
            pos,
            shape,
            ai_ground_speed(base_speed),
            visited_budget,
            entity_type,
        );
        mob.has_left_horn = has_left_horn;
        mob.has_right_horn = has_right_horn;
        mob.reinforcement_chance = reinforcement_chance;
        mob.set_category(if hostile {
            MobCategory::Monster
        } else {
            MobCategory::Creature
        })
        .set_persistent(!hostile);
        for (priority, goal) in goals {
            mob.add_goal(priority, goal);
        }
        // The `FOLLOW_RANGE` attribute reaches the controller, which is what
        // bounds target acquisition. Without this every hosted mob used
        // the seam's `DEFAULT_FOLLOW_RANGE`, so the zombie family — the only
        // family `seed_demo_mobs` spawns — targeted at 16 blocks instead of its
        // real 35.0. A wrong *value* on a fully connected wire, which is the
        // failure mode `cargo xtask connectedness` structurally cannot see.
        //
        // Set here rather than in `feed_perception` on purpose: this is a species
        // attribute resolved once at spawn, not per-tick perception. Putting it in
        // the feed would mean re-reading `default_attributes` for every mob every
        // tick, and would invite a second source of truth for a number
        // `visited_budget` above already derives from this exact read.
        mob.mob.set_follow_range(follow_range);
        // What the mob's main hand holds (a drowned's trident roll is the one
        // production reader today, through `MobController::main_hand_item` and
        // `RangedAttackGoal`'s `requires_main_hand` gate), and `equip_attrs`
        // folded above overriding `spawn_with_type`'s bare-species combat
        // numbers with the equipped versions — armour, weapon damage,
        // netherite's knockback resistance.
        mob.mob.set_main_hand_item(equipped.main_hand.clone());
        mob.equipment = equipped;
        mob.defenses = defenses_from_attributes(&attrs);
        mob.attack_damage = attack_damage_from_attributes(&attrs);
        mob.knockback_resistance = knockback_resistance_from_attributes(&attrs);
        mob
    }

    /// Removes a mob by entity id, returning whether one was actually removed.
    ///
    /// The missing despawn half of a native plugin's spawn/despawn/modify
    /// surface: [`spawn_species`](Self::spawn_species) plus
    /// [`SimMob::id`] already give a caller "spawn and get an id back", and this
    /// is the same [`self.mobs`](MobSim) retain shape already used inline at
    /// the creeper self-detonation and [`reap_dead`](Self::reap_dead) call
    /// sites, named and made public rather than duplicated a third time.
    ///
    /// **Cannot remove a player.** Player entity ids are allocated from
    /// `PLAYER_ENTITY_ID_BASE` and live in `PlayerRegistry`, never in
    /// `self.mobs` — so a plugin calling this with a connected player's id is a
    /// harmless no-op, never an accidental disconnect. This is the server-side
    /// analogue of the client's `apply_entity_removal` skipping an id held by
    /// `LocalPlayer`.
    ///
    /// **Drops no loot and grants no experience** — unlike
    /// [`reap_dead`](Self::reap_dead)'s death sweep, this is vanilla's plain
    /// generic entity-remove call, not a kill. A plugin that wants a despawned mob to
    /// drop loot calls whatever already grants that on a real death, not this.
    pub fn remove_mob(&mut self, id: i32) -> bool {
        let before = self.mobs.len();
        self.mobs.retain(|m| m.id != id);
        self.mobs.len() != before
    }

    /// Given a just-placed carved pumpkin or jack o'lantern at `pumpkin_pos`,
    /// checks whether it completes a valid snow- or iron-golem block pattern
    /// and, if so, spawns the golem — vanilla's
    /// own carved-pumpkin "try spawn golem" step.
    ///
    /// Tries the snow golem pattern first and returns on a match, exactly as
    /// vanilla's early `return` does — a pumpkin that happens to complete
    /// both (impossible for these two shapes, but the order is part of the
    /// port) only ever produces the snow golem.
    ///
    /// **A pure detection query, not a world mutation.** `MobSim` holds only
    /// a read-only [`lodestone_entity::pathfinding::PathWorld`] and has no
    /// block-*write* authority, so
    /// `block_at` is the caller's own world oracle (the
    /// [`tick_with_terrain`](Self::tick_with_terrain) idiom) and
    /// [`GolemConstruction::consumed`] is a report, not an action — the
    /// caller (the block-placement owner) is the one that actually clears
    /// those cells, exactly as the documented scope says: "given this
    /// placement, does a valid pattern exist, and if so spawn the golem".
    pub fn try_construct_golem(
        &mut self,
        block_at: &dyn Fn(i32, i32, i32) -> String,
        pumpkin_pos: (i32, i32, i32),
    ) -> Option<GolemConstruction> {
        if let Some(found) = golem::find_golem_pattern(block_at, golem::SNOW_GOLEM_PATTERN, pumpkin_pos) {
            // `getBlock(0, 2, 0)` — the bottom snow block's cell.
            let feet = found.translate(0, 2, 0);
            let consumed = found.consumed(golem::SNOW_GOLEM_PATTERN);
            let id = self
                .spawn_species(
                    "minecraft:snow_golem".parse().expect("valid key"),
                    golem::golem_feet_to_spawn_pos(feet),
                )
                .id();
            return Some(GolemConstruction {
                species: GolemSpecies::Snow,
                id,
                consumed,
            });
        }
        if let Some(found) = golem::find_golem_pattern(block_at, golem::IRON_GOLEM_PATTERN, pumpkin_pos) {
            // `getBlock(1, 2, 0)` — the bottom-centre iron block's cell.
            let feet = found.translate(1, 2, 0);
            let consumed = found.consumed(golem::IRON_GOLEM_PATTERN);
            let id = self
                .spawn_species(
                    "minecraft:iron_golem".parse().expect("valid key"),
                    golem::golem_feet_to_spawn_pos(feet),
                )
                .id();
            // vanilla additionally calls its own "set player created" setter,
            // which suppresses this golem attacking
            // the player who angered it and is checked on NBT save/load.
            // This sim has no such per-golem flag and no player-directed
            // hostility model for a neutral mob to suppress — a disclosed
            // gap, not a silent omission: a player-built iron golem here
            // behaves identically to a village-spawned one.
            return Some(GolemConstruction {
                species: GolemSpecies::Iron,
                id,
                consumed,
            });
        }
        None
    }

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
    /// One tick, settling dropped items against this sim's own terrain snapshot.
    ///
    /// **Production should call [`tick_with_terrain`](Self::tick_with_terrain)
    /// instead**, and the difference is a real gameplay bug rather than a
    /// preference: the snapshot only covers the 7×7 `mob_area` columns taken when
    /// the world opened, so items dropped anywhere else fall straight through the
    /// ground. See [`settle_item`]. This entry point stays for hermetic callers,
    /// whose fixture world *is* the whole world they care about.
