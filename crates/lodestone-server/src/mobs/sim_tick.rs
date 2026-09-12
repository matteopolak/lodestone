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

    /// Ticks between one unemployed villager's job searches — throttles
    /// [`villager::find_and_claim_workstation`]'s bounded terrain scan (see
    /// that function's own doc for the cost it is bounding). 100 ticks is a
    /// scope choice, not a transcribed vanilla constant: nothing in this
    /// codebase ports `AssignProfessionFromJobSite`'s own interval.
    #[cfg(not(target_arch = "wasm32"))]
    const JOB_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-profession pass: throttled job search for
    /// unemployed villagers, and re-verification for employed ones.
    ///
    /// Re-verification, not an event hook, is how "losing the block loses
    /// the job" is implemented — see [`villager`]'s own module doc for why,
    /// and for the one-tick lag that trade-off buys. A villager whose
    /// workstation position no longer resolves to the profession it was
    /// claimed under (destroyed, or replaced with a different workstation
    /// type) releases its ticket and goes back to unemployed on the very
    /// next call.
    ///
    /// Native-only (the wasm32 scope note) — see
    /// [`villager::WorkstationClaims`]'s own doc. A villager spawned in a
    /// `wasm32` (browser singleplayer) world keeps whatever profession it
    /// already had and simply never claims a new one.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_professions(&mut self) {
        let world = self.world;
        let claims = &mut self.workstation_claims;
        // Vanilla's own villager restock step's own cadence check (its own
        // per-AI-tick brain activity
        // call, not built here — see `villager_trade`'s module doc), run
        // once per profession pass for every employed villager instead.
        // `tick_count` is this sim's only clock (see its own field doc);
        // `restock_day` divides it into vanilla's 24000-tick day the same
        // way `day_time` is derived elsewhere in this file.
        let restock_time = self.tick_count as i64;
        let restock_day = restock_time / 24_000;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.workstation {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::poi_type_for_block(villager::bare_block_id(state))
                    .and_then(villager::profession_for_poi_type)
                    == Some(mob.profession);
                if !still_valid {
                    claims.remove(pos);
                    mob.set_profession(villager::Profession::None, None);
                } else if let Some(trades) = mob.ensure_trades() {
                    trades.maybe_restock(restock_time, restock_day);
                }
                continue;
            }
            // A profession with no job site (`Nitwit`) has nothing to search
            // for; only `None` (truly unemployed) runs the search below.
            if mob.profession != villager::Profession::None {
                continue;
            }
            if mob.job_search_cooldown > 0 {
                mob.job_search_cooldown -= 1;
                continue;
            }
            mob.job_search_cooldown = Self::JOB_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some((pos, profession)) =
                villager::find_and_claim_workstation(origin, world, claims)
            {
                mob.set_profession(profession, Some(pos));
            }
        }
    }

    /// Bed search interval — [`JOB_SEARCH_INTERVAL_TICKS`](Self::JOB_SEARCH_INTERVAL_TICKS)'s
    /// own scope choice, reused for the identical reason: nothing in this
    /// codebase ports `AcquirePoi`'s own per-behavior scheduling.
    #[cfg(not(target_arch = "wasm32"))]
    const BED_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-bed pass (the raid trigger): throttled bed
    /// search for an unclaimed villager, re-verification for a claimed one.
    ///
    /// Independent of [`tick_villager_professions`](Self::tick_villager_professions):
    /// a bed (vanilla's own "home" memory) and a job site
    /// (vanilla's own "job site" memory) are two separate memories in vanilla,
    /// and a villager can hold either, both, or neither at once. Same
    /// re-verification shape as professions — see that method's own doc for
    /// why a poll, not an event hook, is how "losing the bed loses the
    /// claim" is implemented, and the one-tick lag that trade-off buys.
    ///
    /// Native-only, for [`tick_villager_professions`](Self::tick_villager_professions)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_beds(&mut self) {
        let world = self.world;
        let claims = &mut self.bed_claims;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.bed {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::is_bed_block(villager::bare_block_id(state));
                if !still_valid {
                    claims.remove(pos);
                    mob.bed = None;
                }
                continue;
            }
            if mob.bed_search_cooldown > 0 {
                mob.bed_search_cooldown -= 1;
                continue;
            }
            mob.bed_search_cooldown = Self::BED_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some(pos) = villager::find_and_claim_bed(origin, world, claims) {
                mob.bed = Some(pos);
            }
        }
    }

    /// The live equivalent of
    /// [`crate::poi_storage::PoiStorage::occupied_in_range`] restricted to
    /// `home` POIs: every bed claimed through [`tick_villager_beds`](Self::tick_villager_beds)
    /// within `radius` real blocks of `center`. The raid trigger
    /// (vanilla's own raid-creation-or-extension step's own point-of-interest
    /// range query over the `#village` tag, occupied only) is this method's reason to exist: a bed
    /// claimed through [`villager::BedClaims`] is never written to the
    /// on-disk `poi/` region set (see that type's own doc), so a caller
    /// wiring the real trigger against *live* villagers reads this rather
    /// than (or in addition to) [`crate::poi_storage::PoiStorage::occupied_in_range`],
    /// which can only ever see a bed claim that has been persisted to disk.
    ///
    /// Native-only, for [`tick_villager_beds`](Self::tick_villager_beds)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn occupied_homes_in_range(&self, center: BlockPos, radius: i32) -> Vec<BlockPos> {
        self.bed_claims.occupied_in_range(center, radius)
    }

    /// The full point-of-interest range query, filtered to the `#village`
    /// point-of-interest tag and occupied only, that the raid trigger actually
    /// needs — every claimed bed, workstation *or* bell within `radius` real
    /// blocks of `center`, unioning [`occupied_homes_in_range`](Self::occupied_homes_in_range)
    /// with [`villager::WorkstationClaims::occupied_in_range`] and
    /// [`villager::BellClaims::occupied_in_range`].
    ///
    /// [`occupied_homes_in_range`](Self::occupied_homes_in_range) alone is
    /// narrower than vanilla's `#village` tag (`home` + `meeting` +
    /// `#acquirable_job_site`, per `point_of_interest_type/village.json`) —
    /// a village whose villagers have claimed jobs and a bell but no bed yet
    /// would never trigger a raid through the beds-only query. This is the
    /// one [`super::raid`]'s `create_or_extend_raid` and `crate::server`'s
    /// Bad-Omen-to-Raid-Omen conversion check both use instead.
    ///
    /// Native-only, for [`occupied_homes_in_range`](Self::occupied_homes_in_range)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn occupied_village_pois_in_range(&self, center: BlockPos, radius: i32) -> Vec<BlockPos> {
        let mut found = self.bed_claims.occupied_in_range(center, radius);
        found.extend(self.workstation_claims.occupied_in_range(center, radius));
        found.extend(self.bell_claims.occupied_in_range(center, radius));
        found
    }

    /// Bell search interval — [`JOB_SEARCH_INTERVAL_TICKS`](Self::JOB_SEARCH_INTERVAL_TICKS)'s
    /// own scope choice, reused for the identical reason.
    #[cfg(not(target_arch = "wasm32"))]
    const BELL_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-bell pass (the `MEET` schedule activity):
    /// throttled bell search for an unclaimed villager, re-verification for
    /// a claimed one — [`tick_villager_beds`](Self::tick_villager_beds)'s own
    /// shape, restricted to [`villager::BellClaims`]/[`villager::find_and_claim_bell`]
    /// and with **no occupancy exclusion** (a bell hands out 32 tickets, so
    /// nothing here needs to check whether another villager already claimed
    /// this exact bell — [`villager::find_and_claim_bell`]'s own search
    /// already tries the next ticket via `try_claim` regardless).
    ///
    /// Independent of [`tick_villager_beds`](Self::tick_villager_beds)/
    /// [`tick_villager_professions`](Self::tick_villager_professions): a
    /// bell (vanilla's own "meeting point" memory), a bed
    /// (vanilla's own "home" memory) and a job site
    /// (vanilla's own "job site" memory) are three separate memories in vanilla,
    /// and a villager can hold any combination of the three at once.
    ///
    /// Native-only, for [`tick_villager_professions`](Self::tick_villager_professions)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_bells(&mut self) {
        let world = self.world;
        let claims = &mut self.bell_claims;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.meeting_point {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::is_bell_block(villager::bare_block_id(state));
                if !still_valid {
                    claims.remove(pos);
                    mob.meeting_point = None;
                }
                continue;
            }
            if mob.bell_search_cooldown > 0 {
                mob.bell_search_cooldown -= 1;
                continue;
            }
            mob.bell_search_cooldown = Self::BELL_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some(pos) = villager::find_and_claim_bell(origin, world, claims) {
                mob.meeting_point = Some(pos);
            }
        }
    }

    /// Throttles [`tick_cat_block_search`](Self::tick_cat_block_search)'s
    /// bounded terrain scan — the same shape
    /// [`JOB_SEARCH_INTERVAL_TICKS`](Self::JOB_SEARCH_INTERVAL_TICKS) is, and
    /// for the identical reason: a scope choice, not a copied constant. The
    /// scan rechecks every 100 ticks independently of whether a movement
    /// behavior is eligible to start; the interval bounds terrain work without
    /// coupling it to the movement scheduler.
    const CAT_BLOCK_SEARCH_INTERVAL_TICKS: i32 = 100;
    /// The sitting search bounds: horizontal range 8 and vertical range 1,
    /// centered on the mob's block position.
    const CAT_SIT_HORIZONTAL_RANGE: i32 = 8;
    const CAT_SIT_VERTICAL_RANGE: i32 = 1;
    /// The bed search bounds: horizontal range 8, vertical start -2, and
    /// vertical range 6.
    const CAT_BED_HORIZONTAL_RANGE: i32 = 8;
    const CAT_BED_VERTICAL_MIN: i32 = -2;
    const CAT_BED_VERTICAL_MAX: i32 = 6;

    /// The cat block-spiral search, run here rather than inside either goal —
    /// `docs/mob-block-perception.md`'s own guidance for a goal that needs to
    /// search a neighbourhood ("must not be built on [block cues]… that is a
    /// host-computed candidate position instead"), the same shape
    /// [`tick_villager_professions`](Self::tick_villager_professions) already
    /// is for the identical reason. See
    /// [`lodestone_entity::ai::MobController::cat_sit_target`]'s own doc for
    /// the seam this feeds.
    ///
    /// The scan checks the whole box and keeps the closest valid cell by real
    /// squared distance. It therefore does not depend on ring traversal order
    /// when several valid cells are present.
    ///
    /// Throttled per mob by [`SimMob::cat_search_cooldown`], the same shape
    /// [`job_search_cooldown`](SimMob::job_search_cooldown) already uses.
    ///
    /// No `wasm32` gate — unlike [`tick_villager_professions`](Self::tick_villager_professions),
    /// this touches no `std::fs`-backed type.
    fn tick_cat_block_search(&mut self) {
        let world = self.world;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "cat" {
                continue;
            }
            if mob.cat_search_cooldown > 0 {
                mob.cat_search_cooldown -= 1;
                continue;
            }
            mob.cat_search_cooldown = Self::CAT_BLOCK_SEARCH_INTERVAL_TICKS;
            let pos = mob.position();
            let origin = BlockPos::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            );

            // A sitting target is a chest, a lit furnace, or a bed's non-head
            // part.
            let sit = Self::find_nearest_cat_block(
                world,
                origin,
                Self::CAT_SIT_HORIZONTAL_RANGE,
                -Self::CAT_SIT_VERTICAL_RANGE,
                Self::CAT_SIT_VERTICAL_RANGE,
                |state| {
                    let bare = villager::bare_block_id(state);
                    bare == "chest"
                        || (bare == "furnace" && state.contains("lit=true"))
                        || (bare.ends_with("_bed") && !state.contains("part=head"))
                },
            );
            mob.mob.set_cat_sit_target(sit);

            // A bed target accepts either bed part; unlike sitting, no
            // head/foot distinction is needed here.
            let bed = Self::find_nearest_cat_block(
                world,
                origin,
                Self::CAT_BED_HORIZONTAL_RANGE,
                Self::CAT_BED_VERTICAL_MIN,
                Self::CAT_BED_VERTICAL_MAX,
                |state| villager::bare_block_id(state).ends_with("_bed"),
            );
            mob.mob.set_cat_bed_target(bed);
        }
    }

    /// The bounded box scan [`tick_cat_block_search`](Self::tick_cat_block_search)
    /// runs for both cat goals: every cell in `[-horiz, horiz]` horizontally
    /// and `[y_min, y_max]` vertically around `origin`, gated by the same
    /// headroom check vanilla's own valid-target check makes
    /// (an "is empty block" test on the cell above, approximated here as the `#air`
    /// tag's three members — `air`/`cave_air`/`void_air` — rather than a real
    /// per-block-state emptiness census). Returns the nearest match's
    /// stand-on point: one block above the matched cell, block-centred,
    /// matching vanilla's own generic "move to block" goal's own
    /// move-to-target getter (one block above).
    fn find_nearest_cat_block(
        world: &ChunkWorld,
        origin: BlockPos,
        horiz: i32,
        y_min: i32,
        y_max: i32,
        is_valid: impl Fn(&str) -> bool,
    ) -> Option<Vec3> {
        let mut best: Option<(i32, Vec3)> = None;
        for dy in y_min..=y_max {
            for dx in -horiz..=horiz {
                for dz in -horiz..=horiz {
                    let x = origin.x + dx;
                    let y = origin.y + dy;
                    let z = origin.z + dz;
                    let above = world.block_state(x, y + 1, z);
                    if !matches!(villager::bare_block_id(above), "air" | "cave_air" | "void_air") {
                        continue;
                    }
                    let state = world.block_state(x, y, z);
                    if !is_valid(state) {
                        continue;
                    }
                    let dist = dx * dx + dy * dy + dz * dz;
                    let better = match best {
                        Some((best_dist, _)) => dist < best_dist,
                        None => true,
                    };
                    if better {
                        best = Some((
                            dist,
                            Vec3::new(f64::from(x) + 0.5, f64::from(y) + 1.0, f64::from(z) + 0.5),
                        ));
                    }
                }
            }
        }
        best.map(|(_, pos)| pos)
    }

    /// Ticks between gossip-spread passes. The whole-pass throttle is
    /// intentionally separate from the per-pair gossip values; it keeps the
    /// radius-bounded scan deterministic and bounded.
    const GOSSIP_SPREAD_INTERVAL_TICKS: u64 = 100;
    /// How close two villagers must be to gossip this pass. This crate uses an
    /// explicit squared radius so the pair scan remains bounded.
    const GOSSIP_SPREAD_RADIUS_SQR: f64 = 64.0; // 8 blocks

    /// Nearby villagers exchange gossip during a periodic radius-bounded scan
    /// over every villager pair. The pass is an approximation of the
    /// sensor-driven meeting behavior; the `villager` module supplies the
    /// workstation-claiming boundary separately.
    ///
    /// Both directions of a meeting pair exchange from a **pre-transfer
    /// snapshot** of each side (`source_a`/`source_b`, cloned before either
    /// mutates), so the second transfer never reads the first transfer's
    /// updated state. The result is independent of pair-transfer order.
    fn spread_villager_gossip(&mut self) {
        if self.tick_count % Self::GOSSIP_SPREAD_INTERVAL_TICKS != 0 {
            return;
        }
        let mut rng = self.gossip_spread_rng.clone();
        let villagers: Vec<(usize, Vec3)> = self
            .mobs
            .iter()
            .enumerate()
            .filter(|(_, m)| m.entity_type.path() == "villager")
            .map(|(i, m)| (i, m.position()))
            .collect();
        for a in 0..villagers.len() {
            for b in (a + 1)..villagers.len() {
                let (ia, pa) = villagers[a];
                let (ib, pb) = villagers[b];
                let dist_sqr =
                    (pa.x - pb.x).powi(2) + (pa.y - pb.y).powi(2) + (pa.z - pb.z).powi(2);
                if dist_sqr > Self::GOSSIP_SPREAD_RADIUS_SQR {
                    continue;
                }
                let (lo, hi) = if ia < ib { (ia, ib) } else { (ib, ia) };
                let (left, right) = self.mobs.split_at_mut(hi);
                let source_lo = left[lo].gossip.clone();
                let source_hi = right[0].gossip.clone();
                left[lo]
                    .gossip
                    .transfer_from(&source_hi, |bound| rng.next_int(bound), 10);
                right[0]
                    .gossip
                    .transfer_from(&source_lo, |bound| rng.next_int(bound), 10);
            }
        }
        self.gossip_spread_rng = rng;
    }

    /// Ticks between golem-summon checks. The periodic check runs every 100
    /// ticks.
    const GOLEM_SUMMON_INTERVAL_TICKS: u64 = 100;
    /// Host-side radius for the hostile-nearby check: 8 blocks squared. The
    /// test is recomputed from the live mob list rather than a villager memory.
    const GOLEM_SUMMON_HOSTILE_RANGE_SQR: f64 = 64.0;
    /// Axis-aligned agreement box for golem spawning, inflated `10.0` on every
    /// axis.
    const GOLEM_AGREEMENT_RADIUS: f64 = 10.0;
    /// Number of villagers required by the hurt/hostile agreement path. The
    /// gossip-transfer path uses a separate threshold and is not part of this
    /// check.
    const GOLEM_VILLAGERS_NEEDED: usize = 3;
    /// Memory lifetime after a successful golem spawn: `599` ticks before the
    /// next summon attempt is eligible.
    const GOLEM_DETECTED_TTL: u64 = 599;

    /// Golem-summon-on-hurt: the 100-tick cadence checks each villager that is
    /// hurt or has a hostile nearby.
    ///
    /// # Why this lives on `MobSim` rather than in a single-mob behavior
    ///
    /// It needs other villagers' state (the agreement count) and the ability to
    /// create a new entity. A single-mob behavior cannot provide either, so
    /// "is this villager hurt or does it see a hostile" is recomputed here from
    /// [`SimMob::last_hurt_by`] and [`species::is_hostile_species`]
    /// over `self.mobs`, matching the same hurt/nearby-hostile inputs
    /// sensors
    /// would answer rather than reading their output.
    ///
    /// # Three explicit behavior boundaries
    ///
    /// * **Sleep state is not an eligibility input.** Villager records do not
    ///   carry bed state, so the hurt/hostile agreement check uses the
    ///   available mob state without an additional rest requirement.
    /// * **Placement uses a fixed adjacent cell.** The terrain interface is a
    ///   pathfinding snapshot rather than a live column scan, so the agreement
    ///   result places the golem one block beside the triggering villager.
    /// * **One spawn candidate is evaluated per pass.** Candidates are sorted
    ///   by id and the first qualifying candidate keeps the result deterministic.
    fn tick_golem_summon(&mut self) {
        if self.tick_count % Self::GOLEM_SUMMON_INTERVAL_TICKS != 0 {
            return;
        }
        let tick_count = self.tick_count;
        for m in &mut self.mobs {
            if m.entity_type.path() == "villager"
                && m.golem_detected_until.is_some_and(|until| tick_count >= until)
            {
                m.golem_detected_until = None;
            }
        }

        let hostile_positions: Vec<Vec3> = self
            .mobs
            .iter()
            .filter(|m| species::is_hostile_species(&m.entity_type))
            .map(|m| m.position())
            .collect();

        // `wantsToSpawnGolem`: not on cooldown, and hurt or a hostile nearby.
        let candidates: Vec<(i32, Vec3)> = self
            .mobs
            .iter()
            .filter(|m| m.entity_type.path() == "villager" && m.golem_detected_until.is_none())
            .filter(|m| {
                let pos = m.position();
                m.last_hurt_by().is_some()
                    || hostile_positions.iter().any(|hp| {
                        let d = *hp - pos;
                        d.dot(d) <= Self::GOLEM_SUMMON_HOSTILE_RANGE_SQR
                    })
            })
            .map(|m| (m.id, m.position()))
            .collect();

        if candidates.is_empty() {
            return;
        }

        let (_, origin_pos) = candidates[0];
        let within_box = |pos: Vec3| {
            (pos.x - origin_pos.x).abs() <= Self::GOLEM_AGREEMENT_RADIUS
                && (pos.y - origin_pos.y).abs() <= Self::GOLEM_AGREEMENT_RADIUS
                && (pos.z - origin_pos.z).abs() <= Self::GOLEM_AGREEMENT_RADIUS
        };
        let agreeing = candidates.iter().filter(|&&(_, pos)| within_box(pos)).take(5).count();
        if agreeing < Self::GOLEM_VILLAGERS_NEEDED {
            return;
        }

        let golem_pos = Vec3::new(origin_pos.x + 1.0, origin_pos.y, origin_pos.z);
        self.spawn_species(
            "minecraft:iron_golem".parse().expect("static key"),
            golem_pos,
        );

        // `nearbyVillagers.forEach(GolemSensor::golemDetected)`: every
        // villager in the search box, not only the ones that individually
        // wanted a golem — vanilla marks the whole nearby set.
        let until = tick_count + Self::GOLEM_DETECTED_TTL;
        for m in &mut self.mobs {
            if m.entity_type.path() == "villager" && within_box(m.position()) {
                m.golem_detected_until = Some(until);
            }
        }
    }

    /// This villager's summed reputation toward `player` —
    /// vanilla's own "get player reputation" getter. `0` for a non-villager mob or an
    /// untracked player, matching
    /// [`villager::gossip::GossipContainer::reputation`]'s own default.
    #[must_use]
    pub fn villager_reputation(&self, villager_id: i32, player: uuid::Uuid) -> i32 {
        self.get(villager_id)
            .map(|m| m.gossip.reputation(player))
            .unwrap_or(0)
    }

    /// Applies a reputation event directly to `villager_id`'s
    /// own gossip ledger — the entry point [`attack_from_player`](Self::attack_from_player)
    /// uses internally, and what any future caller with a villager id and a
    /// source uuid in hand (a wired `SELECT_TRADE` handler for `Trade`, a
    /// golem-death hook for `GolemKilled`) should call once it exists. A
    /// no-op if `villager_id` names no live mob.
    pub fn record_reputation_event(
        &mut self,
        villager_id: i32,
        event: villager::reputation::ReputationEventType,
        source: uuid::Uuid,
    ) {
        if let Some(mob) = self.get_mut(villager_id) {
            villager::reputation::apply_reputation_event(&mut mob.gossip, event, source);
        }
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

    fn tick_item_owner_batches_with_workers(
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

    fn tick_entity_push_owner_batches_with_workers(
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

    fn tick_burning_owner_batches_with_workers(
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
    fn tick_leashes(&mut self) {
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

    fn tick_leash_owner_batches_with_workers(
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

    /// Populates every mob's [`MobController`] perception inputs from this
    /// sim's own census plus [`set_players`](Self::set_players)' player list.
    ///
    /// Two passes, and the split is a borrow-checker necessity rather than a
    /// style choice: deciding mob `i`'s threat/partner/parent means reading
    /// every *other* mob, so the decisions are computed under shared borrows
    /// first and applied under a mutable one second. It is the same shape
    /// [`tick`](Self::tick) already uses for melee resolution.
    ///
    /// Nothing here is species-*goal* knowledge — that is the roster's job.
    /// The only species table it consults is [`avoided_species`], which answers
    /// "is that a threat to me", a perception question.
    fn feed_perception(&mut self) {
        let n = self.mobs.len();
        let mut nearest_player = vec![None; n];
        let mut temptation = vec![None; n];
        let mut threat = vec![None; n];
        let mut partner = vec![None; n];
        let mut parent = vec![None; n];
        let mut owner = vec![None; n];
        let mut patrol_group = vec![None; n];
        let mut stared_at = vec![false; n];
        let mut nearby_entities: Vec<Vec<NearbyBrainEntity>> = vec![Vec::new(); n];
        // How long each mob's owner has been asleep, used by shoulder and
        // morning-gift behavior; see the per-mob computation below for the
        // uuid/entity-id join.
        let mut owner_sleep_ticks: Vec<Option<u32>> = vec![None; n];
        // the nearest visible zombified piglin, fed to a
        // piglin's `AVOID` brain activity.
        let mut nearest_visible_zombified = vec![None; n];
        // the nearest eligible tongue-attack prey, fed to a
        // frog's `TONGUE` brain activity.
        let mut nearest_attackable_food = vec![None; n];
        // an allay's own delivery target, fed to its `DELIVER`
        // brain activity.
        let mut delivery_target = vec![None; n];

        // --- persistent anger (the anger deadline) -------------------------------
        //
        // Resolved here, in the feed, for the same reason every other
        // pre-computed answer is: `MobController::angry_target` hands the goal
        // an `Option<Vec3>`, never a query, because the seam has no shared game
        // clock to compare an absolute deadline against. So the host does the
        // comparison and only the answer crosses.
        //
        // `now >= end_time` clears the grudge outright rather than merely
        // reporting `None`; an expired grudge must not come back if the clock
        // is read again.
        let now = self.tick_count;

        // Warden pursuit: a warden tracks its own suspect
        // (`SimMob::warden_anger`/`warden_anger_target`, entirely separate
        // from the `SimMob::anger` primitive the loop below reads) and never
        // populates `me.anger`, so without this it would always feed `None`
        // here and its `Brain`'s `FIGHT` activity (`warden_brain`) would
        // never become eligible. Resolved in its own pre-pass, over an
        // immutable borrow of `self.mobs`, because it needs to look up a
        // *different* mob's current position by id — the same reason
        // `partner`/`parent`/`owner` above are resolved before the mutating
        // loop rather than inside it. Gated on `AngerLevel::Angry` (not
        // merely "has a tracked suspect") so an `Agitated` warden — anger
        // above zero but below the chase threshold — does not already start
        // walking, matching `resolve_warden_anger`'s own gate on the strike
        // itself.
        let warden_pursuit_target: Vec<Option<Vec3>> = self
            .mobs
            .iter()
            .map(|me| {
                if me.entity_type().path() != "warden"
                    || me.warden_emerge_ticks > 0
                    // Digging outranks fighting in the activity priority list,
                    // the same reason `warden_emerge_ticks`
                    // above already gates this off — a digging warden must
                    // not also start walking toward whatever it is angry at.
                    || me.warden_digging_ticks > 0
                    || !warden::AngerLevel::from_anger(me.warden_anger).is_angry()
                {
                    return None;
                }
                let target_id = me.warden_anger_target?;
                self.mobs.iter().find(|m| m.id == target_id).map(SimMob::position)
            })
            .collect();

        for (i, me) in self.mobs.iter_mut().enumerate() {
            if me.anger.is_some_and(|a| now >= a.end_time) {
                me.anger = None;
            }
            // A warden never sets `me.anger`, and no non-warden mob ever
            // gets a `warden_pursuit_target` entry (the closure above
            // returns `None` for every other species) — the two halves of
            // this `or` can never both be `Some` for the same mob, so this
            // is a merge of disjoint producers, not a priority order between
            // two that could disagree.
            let target = me.anger.map(|a| a.target).or(warden_pursuit_target[i]);
            me.mob.set_angry_target(target);
        }

        for i in 0..n {
            let me = &self.mobs[i];
            let pos = me.position();
            let species = me.entity_type().path().to_owned();

            // --- nearest player -------------------------------------------
            // Fed with **no range cut**, deliberately: vanilla's range for this
            // lives in the *goal*'s targeting conditions (`LookAtPlayerGoal`
            // takes a look-distance, 6.0F or 8.0F per species,
            // set in its own constructor), not on the mob, and our
            // `LookAtPlayerGoal::can_use` applies exactly that cut itself
            // (`goals.rs`). Cutting here as well would silently take the
            // minimum of two ranges and make the goal's own parameter a lie.
            nearest_player[i] =
                nearest_by(&self.players, pos, |p| p.perception.position, |_| true, None);

            // --- temptation -----------------------------------------------
            // The range *is* on the mob here (vanilla's own tempt-range attribute), so it
            // belongs in the feed. See `TEMPT_RANGE`.
            //
            // The item test is per-species (`tempt_food`), which is why
            // `PlayerPerception` carries the held item rather than a boolean:
            // the same wheat that tempts a cow does nothing to a chicken.
            let foods = species::tempt_food(&species);
            if !foods.is_empty() {
                temptation[i] = nearest_by(
                    &self.players,
                    pos,
                    |p| p.perception.position,
                    |p| {
                        p.perception
                            .held_item
                            .as_ref()
                            .is_some_and(|item| foods.contains(&item.path()))
                    },
                    Some((TEMPT_RANGE, TEMPT_RANGE)),
                );
            }

            // --- avoid threat ---------------------------------------------
            let avoided = species::avoided_species(&species);
            if !avoided.is_empty() {
                threat[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| other.id != me.id && avoided.contains(&other.entity_type().path()),
                    Some((AVOID_RANGE, AVOID_RANGE_Y)),
                );
            }

            // --- breeding partner -----------------------------------------
            // Vanilla's own generic "can mate" check: the
            // partner must be the *same class* and both must be in love. A
            // baby cannot breed (vanilla's own "can fall in love" check gates on age), and
            // The continuing-breed check additionally requires the partner
            // not be panicking — enforced here
            // too, since feeding a panicking partner would start the goal only
            // for it to abort on the next tick.
            if me.is_in_love() && !me.is_baby() {
                partner[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.entity_type() == me.entity_type()
                            && other.is_in_love()
                            && !other.is_baby()
                            && !other.is_panicking()
                    },
                    Some((BREED_RANGE, BREED_RANGE)),
                );
            }

            // --- parent ---------------------------------------------------
            // Vanilla's own follow-parent goal: no goal while this mob's own
            // age is non-negative (adult),
            // and the candidate must itself have a non-negative age, i.e. be an
            // adult, searched over an `8.0, 4.0, 8.0` inflation.
            if me.is_baby() {
                parent[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.entity_type() == me.entity_type()
                            && !other.is_baby()
                    },
                    Some((FOLLOW_PARENT_RANGE, FOLLOW_PARENT_RANGE_Y)),
                );
            }

            // --- owner ----------------------------------------------------
            // The owner *identity* is a census fact (`SimMob::owner`); only the
            // resolved position can cross the seam
            // (`MobController::owner_position`), so this is resolved here
            // exactly like partner/parent.
            //
            // Both flavours resolve, and the player one is what taming produces:
            // vanilla's owner is a uuid (its own owner-uuid metadata field) and
            // its own owner getter resolves it against the level every time it is asked,
            // which is what `player_position` does here. A tamed pet whose owner
            // is not in the list resolves to `None` — offline, or in another
            // dimension, which are the same two cases vanilla's
            // own "owner's level differs from this level" check covers — and `None` is the correct
            // answer rather than a stale last-known position: a pet must not
            // path toward where you were an hour ago.
            //
            // `is_tame` is fed *unconditionally* below rather than derived from
            // this, because a mob is tame whether or not its owner is resolvable.
            match me.owner {
                Some(MobOwner::Mob(oid)) => {
                    owner[i] = nearest_by(
                        &self.mobs,
                        pos,
                        SimMob::position,
                        |other| other.id == oid,
                        None,
                    );
                }
                Some(MobOwner::Player(uuid)) => {
                    owner[i] = self.player_position(uuid);
                    // how long that same player has been asleep,
                    // joined through `self.players`' own uuid<->entity_id
                    // pairing (`PlayerIdentity`) against
                    // `self.sleeping_players`' entity-id-keyed roster — see
                    // `sleeping_players`'s own field doc for why the join
                    // happens here rather than the sleep roster carrying
                    // uuids itself.
                    if let Some(entity_id) = self
                        .players
                        .iter()
                        .find_map(|p| p.identity.filter(|id| id.uuid == uuid).map(|id| id.entity_id))
                    {
                        owner_sleep_ticks[i] = self
                            .sleeping_players
                            .iter()
                            .find(|&&(id, _)| id == entity_id)
                            .map(|&(_, since)| self.tick_count.saturating_sub(since) as u32);
                    }
                }
                None => {}
            }

            // --- nearest visible zombified piglin  -------------
            // A piglin's own "avoid" brain activity. No range cut lives on the
            // mob in the jar (vanilla's own piglin-specific sensor reads whatever
            // its own "nearest visible living entities" sensor already gathered), so this
            // reuses the same generous scan box `nearby_entities` above uses
            // for brain species, restricted to `zombified_piglin` and gated
            // on species the same way `threat[i]`/`temptation[i]` already
            // gate on a non-empty predicate table.
            if species == "piglin" {
                nearest_visible_zombified[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| other.id != me.id && other.entity_type().path() == "zombified_piglin",
                    Some((NEARBY_HOSTILE_SCAN_RANGE, NEARBY_HOSTILE_SCAN_RANGE_Y)),
                );
            }

            // --- nearest eligible tongue-attack prey  ----------
            // A frog's own "tongue" brain activity. Vanilla's own frog-attackables
            // sensor's own
            // range is its own target-detection-distance constant (10.0F); `FROG_FOOD_SPECIES`
            // is the host-side stand-in for vanilla's own "can eat" check's
            // own frog-food entity-type tag (see that constant's own doc
            // for the disclosed size-1 narrowing this does not model).
            if species == "frog" {
                nearest_attackable_food[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.health > 0.0
                            && FROG_FOOD_SPECIES.contains(&other.entity_type().path())
                    },
                    Some((10.0, 10.0)),
                );
            }

            // --- allay delivery target  -------------------------
            // Vanilla's own "get item deposit position" helper's note-block half
            // (its own "should deposit items at liked noteblock" check): only offered once
            // there is something to deliver, a recently-heard note block is
            // still remembered, and the block there is still really a note
            // block (a player could have mined it since). One tick behind
            // `resolve_vibrations`'s own write, the same lag every other
            // activity-swap species' own tests already document — `hearing`
            // runs at the end of the *previous* tick's `MobSim::tick`.
            if species == "allay"
                && me.allay_inventory_count > 0
                && let Some((liked_pos, ticks)) = me.allay_liked_noteblock
                && ticks > 0
                && crate::redstone::base_name(self.world.block_state(
                    liked_pos.x as i32,
                    liked_pos.y as i32,
                    liked_pos.z as i32,
                )) == crate::redstone_note_block::NOTE_BLOCK
            {
                delivery_target[i] = Some(Vec3::new(liked_pos.x, liked_pos.y + 1.0, liked_pos.z));
            }

            // --- patrol group target ---------------------------------------
            // A leader never reads this — it computes its own
            // fresh target from `LongDistancePatrolGoal` itself; only a
            // non-leading, still-patrolling member needs the host's census.
            // See `nearest_patrol_leader_target`'s own doc comment for why
            // this cannot reuse `nearest_by`.
            if me.is_patrolling() && !me.is_patrol_leader() {
                patrol_group[i] = nearest_patrol_leader_target(&self.mobs, pos, me.id);
            }

            // --- gaze (the view-direction feed) -----------------------------------
            // `MobController::is_being_stared_at` is host-fed: the geometry is
            // `lodestone_entity::ai::mob::is_in_view_cone`, vanilla's exact
            // `dot > 1.0 - coneSize / dist`. Line of sight is the same
            // disclosed gap `find_nearest_target` already carries — no world
            // raycast at this seam, erring permissive. The carved-pumpkin
            // disguise check (vanilla's own "player not wearing disguise item"
            // condition) is not
            // modelled either: `PlayerPerception` has no armour-slot data yet.
            //
            // `0.025` is the enderman's own view-cone-size constant; this feed is per-mob, not per-species, so
            // every mob gets the same tolerance today — the only consumer is
            // `EndermanFreezeWhenLookedAt`, so this is not yet observably
            // wrong, but a second gaze-gated species with a different
            // `coneSize` would need this to become species-aware.
            let mob_eye = Vec3::new(pos.x, pos.y + f64::from(me.shape().height) * 0.85, pos.z);
            stared_at[i] = self.players.iter().any(|p| {
                let player_eye = Vec3::new(
                    p.perception.position.x,
                    p.perception.position.y + PLAYER_EYE_HEIGHT,
                    p.perception.position.z,
                );
                lodestone_entity::ai::mob::is_in_view_cone(
                    player_eye,
                    p.perception.view_direction,
                    mob_eye,
                    0.025,
                    true,
                )
            });

            // --- nearby entities (brain target-acquisition primitive) ------
            // Only built for brain-driven species: every other species'
            // `BrainMob::nearby_entities` default (empty) is never read, so
            // scanning the whole mob list for a goal-driven zombie would be
            // pure waste — the same cost-avoidance `avoided_species`'s
            // `is_empty()` check above already applies to a different feed.
            if is_brain_species(&species) {
                nearby_entities[i] = self
                    .mobs
                    .iter()
                    .filter(|other| {
                        other.id != me.id
                            && (other.position().x - pos.x).abs() <= NEARBY_HOSTILE_SCAN_RANGE
                            && (other.position().z - pos.z).abs() <= NEARBY_HOSTILE_SCAN_RANGE
                            && (other.position().y - pos.y).abs() <= NEARBY_HOSTILE_SCAN_RANGE_Y
                    })
                    .map(|other| NearbyBrainEntity {
                        id: other.id,
                        position: other.position(),
                        hostile: species::is_hostile_species(other.entity_type()),
                    })
                    .collect();
            }
        }

        // a plain field read, not a per-mob computation, so it
        // lives outside the loop below like every other constant the loop
        // reuses (`tick_count`) — `self.mobs.iter_mut()` only borrows the
        // `mobs` field, so this and that are disjoint borrows regardless.
        let day_time = self.day_time;
        let block_center = |p: BlockPos| {
            Vec3::new(f64::from(p.x) + 0.5, f64::from(p.y) + 0.5, f64::from(p.z) + 0.5)
        };

        for (i, m) in self.mobs.iter_mut().enumerate() {
            // Not folded into the chain below: `set_tame`/`set_ordered_to_sit`
            // read `m`'s own record while the chain holds `m.mob` mutably.
            let (tame, ordered_to_sit) = (m.tame, m.ordered_to_sit);
            // same reason as `tame`/`ordered_to_sit` above — read
            // before `m.mob` is borrowed mutably by the chain below.
            let shoulder_dismount_ticks = m.shoulder_dismount_ticks;
            m.mob.set_tame(tame).set_ordered_to_sit(ordered_to_sit);
            // the villager POI-claim feed
            // (`crate::brain::VillagerPoiSensor`'s own source), read before
            // `m.mob` is borrowed mutably below — `m.workstation`/`m.bed`/
            // `m.meeting_point` are `None` for every non-villager species,
            // so this is safe to feed unconditionally, the same "harmless
            // default" shape `set_nearby_entities` already is for a
            // goal-driven mob.
            let job_site = m.workstation.map(block_center);
            let home = m.bed.map(block_center);
            let meeting_point = m.meeting_point.map(block_center);
            m.mob
                .set_nearest_player(nearest_player[i])
                .set_temptation(temptation[i])
                .set_avoid_threat(threat[i])
                // The sim has incremented this every tick since long before
                // this mob's record, but it never crossed the
                // `MobController` seam, so idle
                // suppression read the trait default `0` and never fired.
                .set_no_action_time(m.no_action_time)
                .set_love_partner_candidate(partner[i])
                .set_parent_candidate(parent[i])
                .set_owner(owner[i])
                .set_patrol_group_target(patrol_group[i])
                .set_stared_at(stared_at[i])
                .set_nearby_entities(std::mem::take(&mut nearby_entities[i]))
                .set_job_site(job_site)
                .set_home(home)
                .set_meeting_point(meeting_point)
                .set_owner_sleep_ticks(owner_sleep_ticks[i])
                .set_nearest_visible_zombified(nearest_visible_zombified[i])
                .set_nearest_attackable_food(nearest_attackable_food[i])
                .set_delivery_target(delivery_target[i])
                // a sniffer's own host-found dig-search target,
                // fed to its `Brain`'s `WalkToPoi` — see
                // `sniffer::MobSim::tick_sniffers`'s own doc for the state
                // machine that produces this. `None` for every non-sniffer
                // species, the same harmless-default shape every other
                // host-computed-candidate field here already is.
                .set_sniffer_dig_target(m.sniffer_dig_target)
                .set_ticks_since_shoulder_dismount(shoulder_dismount_ticks)
                .set_day_time(day_time);
        }
    }

}
