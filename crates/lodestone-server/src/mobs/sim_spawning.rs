//! Despawn, natural spawning, patrols, and trader lifecycle for [`super::MobSim`].

use super::*;

impl<'w> MobSim<'w> {
    /// Runs one despawn check over every non-persistent mob, given the nearest
    /// player's position (vanilla `getNearestPlayer(-1.0)`), removing mobs the
    /// two distance gates discard and resetting the age timer of any within the
    /// immune radius.
    ///
    /// `nearest_player` is `None` when no player is loaded, in which case vanilla
    /// runs no despawn logic at all — the mobs are simply kept. The `1/800`
    /// gate-B roll is drawn per candidate mob from `rng`, with one success in
    /// every 800 outcomes.
    ///
    /// Returns the number of mobs discarded.
    pub fn despawn_pass(&mut self, nearest_player: Option<Vec3>, rng: &mut SpawnRng) -> usize {
        let Some(player) = nearest_player else {
            return 0;
        };
        let before = self.mobs.len();
        self.mobs.retain_mut(|m| {
            if m.persistent {
                return true;
            }
            let dist_sqr = dist_sqr(m.mob.position(), player);
            let rng_hit_800 = rng.next_int(800) == 0;
            let outcome = check_despawn(m.category, dist_sqr, m.no_action_time, rng_hit_800, true);
            match outcome {
                DespawnOutcome { discard: true, .. } => false,
                DespawnOutcome {
                    reset_timer: true, ..
                } => {
                    m.no_action_time = 0;
                    true
                }
                _ => true,
            }
        });
        before - self.mobs.len()
    }

    /// Runs one natural-spawn cycle over `chunks`, respecting the per-category
    /// global caps in `state`.
    ///
    /// For each chunk and each spawnable category still under its cap, the
    /// [`SpawnCandidateSource`] is asked for the group it would spawn there; each
    /// member becomes a real mob through [`spawn_species`](Self::spawn_species),
    /// so it arrives with the species' own body, attributes and vanilla goal set
    /// rather than a placeholder. Nothing here decides *which* mob or *where* —
    /// that is the source's registry/terrain-dependent job.
    ///
    /// The **category is the spawn list's**, not
    /// [`spawn_species`](Self::spawn_species)' hostile/friendly guess: vanilla's
    /// category is a property of the `EntityType` registration, and the biome
    /// list's key is exactly that. It is overridden after the spawn for the same
    /// reason the cap is keyed by it.
    ///
    /// A group is truncated the moment its category reaches the cap, so the cap
    /// can never be exceeded even though the source drew a whole cluster.
    ///
    /// Returns the number of mobs spawned.
    pub fn run_spawn_cycle(
        &mut self,
        state: &mut SpawnState,
        source: &mut dyn SpawnCandidateSource,
        chunks: &[(i32, i32)],
    ) -> usize {
        let planned = Self::plan_spawn_cycle(state, source, chunks);
        let spawned = planned.len();
        for (category, candidate) in planned {
            let mob = self.spawn_species(candidate.entity_type, candidate.pos);
            mob.set_category(category)
                .set_persistent(category.is_persistent());
        }
        spawned
    }

    /// Plans one natural-spawn cycle without mutating the simulation.
    ///
    /// The returned candidates retain their spawn-list category so a caller can
    /// submit them to an external adjudication pass before it takes the mob
    /// simulation lock to materialize accepted actions. [`run_spawn_cycle`]
    /// remains the direct compatibility path and applies this plan immediately.
    pub fn plan_spawn_cycle(
        state: &mut SpawnState,
        source: &mut dyn SpawnCandidateSource,
        chunks: &[(i32, i32)],
    ) -> Vec<(MobCategory, SpawnCandidate)> {
        let mut planned = Vec::new();
        for &(cx, cz) in chunks {
            for category in MobCategory::SPAWNING {
                if !state.can_spawn(category) {
                    continue;
                }
                for candidate in source.cluster(category, cx, cz) {
                    if !state.can_spawn(category) {
                        break;
                    }
                    state.record(category);
                    planned.push((category, candidate));
                }
            }
        }
        planned
    }

    /// Builds a [`SpawnState`] for `spawnable_chunks` from a census of the mobs
    /// currently alive, exactly as vanilla rebuilds `SpawnState` each cycle.
    #[must_use]
    pub fn census(&self, spawnable_chunks: i32) -> SpawnState {
        let mut state = SpawnState::new(spawnable_chunks);
        for m in &self.mobs {
            state.record(m.category);
        }
        state
    }

    /// Runs one patrol-spawn tick — vanilla's own patrol-spawner port
    /// (a 92-line generic custom-spawner). Meant to be called
    /// once per server tick, mirroring vanilla's own generic custom-spawner
    /// update: the
    /// internal countdown is decremented every call regardless of whether
    /// anything ends up spawning, so calling this less often than once a
    /// tick would make patrols rarer than vanilla rather than merely
    /// checked less often.
    ///
    /// `world` is the terrain a spawn candidate is checked against, and it
    /// must be a *live, player-following* snapshot — not this sim's own
    /// static `self.world` — because a patrol spawns near a **player**, and
    /// `self.world` is a fixed footprint around wherever `mob_area` was when
    /// the world opened. Feeding `self.world` here would reproduce the exact
    /// bug natural spawning already had and fixed: patrols would work near
    /// spawn and stop working entirely once a player walked away from it.
    /// The caller should hand in the same snapshot `crate::natural_spawn`
    /// already builds each cycle.
    ///
    /// `spawn_patrols` is the game rule of the same name; `is_bright_outside`
    /// is vanilla's own "is bright outside" check — day and not thundering
    /// — collapsed to a caller-supplied bool because no weather state crosses
    /// this seam yet.
    ///
    /// Returns the number of pillagers actually spawned this call (`0` on
    /// almost every call — vanilla's own interval is roughly once every
    /// 12000–13200 ticks per world, i.e. every 10–11 minutes).
    ///
    /// # Disclosed, not modelled
    ///
    /// `docs/pillager-patrols.md` has the full account; the summary:
    ///
    /// * **No spectator filter and no village-proximity check.** Neither a
    ///   spectator flag nor a POI/village census exists on this seam.
    /// * **No block-light check** (vanilla's own "check patrolling monster
    ///   spawn rules" step's own
    ///   block-brightness-above-8 test). [`ChunkWorld`] carries
    ///   block *identity*, not light — the same limit `natural_spawn`'s
    ///   caller-supplied light cache exists to work around for the mobs that
    ///   need it, which this method does not have access to.
    /// * **Vanilla's own "is valid empty spawn block" check is approximated** as "two blocks of open
    ///   air above the surface", with no fluid-state check.
    /// * [`patrol_group_size`] approximates vanilla's own
    ///   "current-difficulty-at-position, effective difficulty" formula, a continuous formula this crate has no
    ///   moon-phase or accumulated regional-difficulty state to compute, with
    ///   a per-[`Difficulty`]-enum constant.
    pub fn run_patrol_spawn_cycle(
        &mut self,
        world: &ChunkWorld,
        spawn_patrols: bool,
        is_bright_outside: bool,
        difficulty: Difficulty,
    ) -> usize {
        self.patrol_next_tick -= 1;
        if self.patrol_next_tick > 0 {
            return 0;
        }
        // Vanilla re-arms the countdown *before* any of the gates below can
        // reject the attempt (`this.nextTick = this.nextTick + 12000 +
        // random.nextInt(1200)`, unconditionally, immediately after the `<=
        // 0` check) — so a rejected attempt still waits a full interval
        // before the next one, rather than retrying every tick.
        self.patrol_next_tick += 12_000 + self.patrol_rng.next_int(1_200);
        if !spawn_patrols || !is_bright_outside {
            return 0;
        }
        if self.tick_count < PATROL_TIMELINE_GATE {
            return 0;
        }
        if self.patrol_rng.next_int(5) != 0 {
            return 0;
        }
        if self.players.is_empty() {
            return 0;
        }
        let player_pos = self.players
            [self.patrol_rng.next_int(self.players.len() as i32) as usize]
            .perception
            .position;

        let sign_x = if self.patrol_rng.next_int(2) == 0 {
            -1.0
        } else {
            1.0
        };
        let sign_z = if self.patrol_rng.next_int(2) == 0 {
            -1.0
        } else {
            1.0
        };
        let dx = f64::from(24 + self.patrol_rng.next_int(24)) * sign_x;
        let dz = f64::from(24 + self.patrol_rng.next_int(24)) * sign_z;
        let mut spawn_x = (player_pos.x + dx).floor() as i32;
        let mut spawn_z = (player_pos.z + dz).floor() as i32;

        let group_size = patrol_group_size(difficulty);
        let pillager: ResourceKey = "minecraft:pillager".parse().expect("valid key");
        let mut spawned = 0;
        for i in 0..group_size {
            // Vanilla's own natural-spawner "is valid empty spawn block"
            // check + this method's own
            // "not modelled" note: a surface exists and there are two open
            // cells above it. `None`/`false` both mean "no valid cell here".
            let spawn_ok = surface_y(world, spawn_x, spawn_z).filter(|&surface| {
                !world.is_solid(spawn_x, surface + 1, spawn_z)
                    && !world.is_solid(spawn_x, surface + 2, spawn_z)
            });
            if let Some(surface) = spawn_ok {
                let pos = Vec3::new(
                    f64::from(spawn_x) + 0.5,
                    f64::from(surface + 1),
                    f64::from(spawn_z) + 0.5,
                );
                let mob = self.spawn_species(pillager.clone(), pos);
                let id = mob.id;
                mob.set_category(MobCategory::Monster);
                self.get_mut(id)
                    .expect("just spawned")
                    .set_patrolling(true);
                if i == 0 {
                    // Vanilla's own "find patrol target" step: `-500 + nextInt(1000)` on both
                    // axes, offset from the mob's *own* spawn position.
                    let tx = f64::from(self.patrol_rng.next_int(1_000) - 500);
                    let tz = f64::from(self.patrol_rng.next_int(1_000) - 500);
                    let leader = self.get_mut(id).expect("just spawned");
                    leader.set_patrol_leader(true);
                    leader.set_patrol_target(Some(Vec3::new(pos.x + tx, pos.y, pos.z + tz)));
                }
                spawned += 1;
            } else if i == 0 {
                // The leader's own spawn attempt failed — vanilla abandons
                // the whole group rather than trying a different member
                // first (its own patrol-spawner's own early-break).
                break;
            }
            spawn_x += self.patrol_rng.next_int(5) - self.patrol_rng.next_int(5);
            spawn_z += self.patrol_rng.next_int(5) - self.patrol_rng.next_int(5);
        }
        // A follower spawned this same call has no group target until
        // `feed_perception` next runs its patrol census — a one-tick startup
        // lag, not a correctness gap: `LongDistancePatrolGoal::can_use`
        // requires `patrol_target().is_some()`, so it simply does not fire
        // until then.
        spawned
    }

    /// Runs the wandering-trader spawn cycle against the live, player-following
    /// `world` snapshot. The caller supplies the `spawn_wandering_traders` rule,
    /// while this simulation owns the cycle counters and needs one call per tick.
    ///
    /// Cycle counters are session state; this simulation does not persist them.
    /// Spawn selection uses a random player's position and omits point-of-interest
    /// search, biome exclusion, collision checks, and post-spawn home/despawn
    /// fields because those inputs are not part of this simulation's state.
    ///
    /// Returns the trader's entity id on a successful spawn.
    pub fn run_wandering_trader_spawn_cycle(
        &mut self,
        world: &ChunkWorld,
        spawn_wandering_traders: bool,
    ) -> Option<i32> {
        self.trader_tick_delay -= 1;
        if self.trader_tick_delay > 0 {
            return None;
        }
        self.trader_tick_delay = WANDERING_TRADER_TICK_DELAY;
        if !spawn_wandering_traders {
            return None;
        }
        self.trader_spawn_delay -= WANDERING_TRADER_TICK_DELAY;
        if self.trader_spawn_delay > 0 {
            return None;
        }
        self.trader_spawn_delay = WANDERING_TRADER_SPAWN_DELAY;
        let chance = self.trader_spawn_chance;
        self.trader_spawn_chance = (self.trader_spawn_chance
            + WANDERING_TRADER_SPAWN_CHANCE_INCREASE)
            .min(WANDERING_TRADER_MAX_SPAWN_CHANCE);
        // `random.nextInt(100) <= chanceToSpawn` is the entry condition;
        // missing it draws nothing further and leaves the climbed chance
        // in place for next time.
        if self.trader_rng.next_int(100) > chance {
            return None;
        }
        if self.players.is_empty() {
            // Vanilla's own spawn call's "no random player found" arm returns
            // `true` — a "success" for chance-reset purposes — without
            // drawing further or spawning anything.
            self.trader_spawn_chance = WANDERING_TRADER_MIN_SPAWN_CHANCE;
            return None;
        }
        // Vanilla's own spawn call's own extra one-in-ten gate, drawn only
        // once a player
        // exists — vanilla's short-circuit on "no player found" above skips
        // this draw entirely, which is why the empty-players check has to
        // come first rather than being folded into a single `if`.
        if self.trader_rng.next_int(10) != 0 {
            return None;
        }
        let player_pos = self.players
            [self.trader_rng.next_int(self.players.len() as i32) as usize]
            .perception
            .position;
        let reference = (player_pos.x.floor() as i32, player_pos.z.floor() as i32);
        // Vanilla's own "find spawn position near" step: up to 10 candidates
        // within a 48-block
        // radius, first one with a real surface wins. No "is spawn position
        // ok" check beyond "a column exists here" — see the gaps disclosed
        // above.
        let mut spawn_pos = None;
        for _ in 0..10 {
            let x = reference.0 + self.trader_rng.next_int(96) - 48;
            let z = reference.1 + self.trader_rng.next_int(96) - 48;
            if let Some(surface) = surface_y(world, x, z) {
                spawn_pos = Some(Vec3::new(
                    f64::from(x) + 0.5,
                    f64::from(surface + 1),
                    f64::from(z) + 0.5,
                ));
                break;
            }
        }
        let pos = spawn_pos?;
        let (trader_id, _llamas) = self.spawn_wandering_trader(pos);
        self.trader_spawn_chance = WANDERING_TRADER_MIN_SPAWN_CHANCE;
        Some(trader_id)
    }
}
