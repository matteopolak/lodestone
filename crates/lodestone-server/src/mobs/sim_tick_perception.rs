//! Host-side mob perception and social behavior passes.

use super::*;

impl<'w> MobSim<'w> {
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
    pub(super) fn tick_villager_professions(&mut self) {
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
    pub(super) fn tick_villager_beds(&mut self) {
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
    pub(super) fn tick_villager_bells(&mut self) {
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
    pub(super) fn tick_cat_block_search(&mut self) {
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
    pub(super) const GOSSIP_SPREAD_INTERVAL_TICKS: u64 = 100;
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
    pub(super) fn spread_villager_gossip(&mut self) {
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
    pub(super) fn tick_golem_summon(&mut self) {
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
    pub(super) fn feed_perception(&mut self) {
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
