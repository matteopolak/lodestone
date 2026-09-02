//! Mob spawner blocks — the decision half of vanilla's `BaseSpawner.serverTick`.
//!
//! # What it is
//!
//! Given a `minecraft:spawner` block entity that has counted its delay down to
//! zero, this answers **which entities to materialize and where** — or that
//! nothing should spawn this tick. It performs no world mutation and no
//! spawning; the caller hands each [`SpawnAttempt`] to
//! [`crate::MobSim::spawn_species`], the same composition
//! [`crate::spawn_egg::apply_spawn_egg`] already established for eggs.
//!
//! # Scope: this is the trigger→entity decision, not the tick cadence itself
//!
//! [`SpawnerState::tick`] *does* own the delay countdown and reroll — that part
//! of `BaseSpawner` is small, pure state and inseparable from "did this fire",
//! so splitting it out would just move the same arithmetic behind a second
//! seam. What it deliberately does **not** own:
//!
//! * **Per-species placement rules** (`SpawnPlacements.checkSpawnRules` —
//!   light level, biome, valid ground per species). `crate::natural_spawn`
//!   already carries that table for the natural-spawn cycle; wiring a spawner
//!   through it needs a light query this call site does not have plumbed yet.
//!   A spawner here checks only that the candidate cell has no block
//!   collision (vanilla's `level.noCollision`, approximated — see
//!   [`SpawnCtx::is_valid_position`]'s doc) and the plain Peaceful gate
//!   ([`crate::mob_spawn::allowed_in_peaceful`]), which is the one piece of
//!   `checkSpawnRules` every species shares. A zombie spawner will now spawn
//!   zombies in broad daylight, which vanilla's would not. Named here rather
//!   than silently approximated.
//! * **`SpawnData`'s `custom_spawn_rules` and `equipment` fields.** Not
//!   parsed, not applied — matching [`crate::spawn_egg`]'s own precedent of
//!   leaving `finalizeSpawn`/equipment unmodelled. A spawner with a data-pack
//!   `custom_spawn_rules` override behaves as if it had none.
//! * **The custom `Pos` override inside `SpawnData`.** Vanilla lets a spawner's
//!   NBT pin an exact spawn position; only the random-cell placement is
//!   implemented here.
//! * **Entity-vs-entity collision.** `level.noCollision` also checks other
//!   entities occupying the cell; this only checks blocks.
//! * **Passenger entities.** `EntityType.loadEntityRecursive` can spawn a
//!   ridden pair (e.g. a skeleton on a spider); only the primary entity spawns
//!   here.
//!
//! # NBT and the entity `id` derivation
//!
//! Vanilla's own spawn-data NBT shape is
//! `{entity: {id: "minecraft:...", ...}, custom_spawn_rules?: {...},
//! equipment?: {...}}`; `SpawnPotentials` is a `WeightedList<SpawnData>`, i.e. a
//! list of `{data: <SpawnData>, weight: <non-negative int>}` compounds
//! (`Weighted.codec`). [`crate::chunk_nbt`] is where those are read and
//! written; this module only ever sees the reduced `(weight, SpawnData)`
//! pairs, exactly as [`crate::spawn_egg`] only ever sees a validated
//! [`lodestone_model::ResourceKey`] rather than raw NBT.
//!
//! A `SpawnData` whose `entity` compound carries no `id` (vanilla's stripped
//! constructor removes the key entirely when absent) resolves to
//! [`SpawnData::NONE`] — [`SpawnerState::tick`] then reroots the delay and
//! spawns nothing, matching `BaseSpawner.serverTick`'s
//! `entityType.isEmpty()` early return.
//!
//! # Dependencies
//!
//! [`crate::mob_spawn::SpawnRng`] for the RNG stream (the same tiny
//! deterministic generator every other pure-decision module in this crate
//! uses) and [`crate::mob_spawn::allowed_in_peaceful`] for the peaceful gate.
//! No protocol, no world handle — the caller supplies closures for collision
//! and nearby-entity counting, the same shape [`crate::spawn_egg::use_spawn_egg`]
//! takes a `block_state` closure.

use lodestone_data::entity_types;
use lodestone_model::{BlockPos, Difficulty, ResourceKey, Vec3};

use crate::mob_spawn::{allowed_in_peaceful, SpawnRng};

/// Validates a raw `entity.id` string (from a `SpawnData`/`SpawnPotentials`
/// compound) against the entity-type registry, the same "the name must
/// resolve to something real" check [`crate::spawn_egg::entity_type_for_egg`]
/// applies to a derived egg name. [`crate::chunk_nbt`]'s load path is the one
/// caller — kept here rather than there so the validation lives next to the
/// type it is validating for.
#[must_use]
pub fn entity_type_from_id_field(id: &str) -> Option<ResourceKey> {
    let key: ResourceKey = id.parse().ok()?;
    entity_types::entity_type_id(&key.to_string())?;
    Some(key)
}

/// The RNG stream seed for spawner-block decisions
/// (`crate::tick::run_tick_loop`'s own `spawner_rng`), picked distinct from
/// every other feature's seed the same way [`crate::redstone_dispenser
/// ::DISPENSER_BEHAVIOR_SEED`] is.
pub const SPAWNER_BEHAVIOR_SEED: u64 = 0x5BA0_7A0F_5EED_5E17;

/// `SpawnData` reduced to what this crate can act on: which entity to create.
/// See the module doc's NBT section for what is dropped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpawnData {
    /// `entityToSpawn.getString("id")`, parsed and validated the same way
    /// [`crate::spawn_egg::entity_type_for_egg`] validates an egg's derived
    /// name — `None` when the entity compound carries no `id` at all, or the
    /// `id` names nothing in the registry.
    pub entity_type: Option<ResourceKey>,
}

impl SpawnData {
    /// An entry naming no entity — vanilla's default-constructed `SpawnData`,
    /// and what a spawner with empty `SpawnPotentials` and no `SpawnData` tag
    /// falls back to (`getOrCreateNextSpawnData`'s `orElseGet(SpawnData::new)`).
    pub const NONE: Self = Self { entity_type: None };
}

/// One `SpawnPotentials` entry: `Weighted<SpawnData>`, reduced to the two
/// fields [`SpawnerState`] needs.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightedSpawnData {
    /// `Weighted.weight` — non-negative per vanilla's own constructor check.
    pub weight: u32,
    /// `Weighted.value`.
    pub data: SpawnData,
}

/// A materialized decision from [`SpawnerState::tick`]: spawn `entity_type` at
/// `position`. The caller hands this straight to
/// [`crate::MobSim::spawn_species`].
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnAttempt {
    /// Which species.
    pub entity_type: ResourceKey,
    /// Feet position, already the random cell vanilla's own `spawnPos`
    /// arithmetic picked.
    pub position: Vec3,
}

/// Per-tick facts [`SpawnerState::tick`] cannot compute itself, mirroring the
/// closures [`crate::spawn_egg::use_spawn_egg`] takes.
pub struct SpawnCtx<'a> {
    /// `BaseSpawner.isNearPlayer` — whether an alive player is within
    /// `required_player_range` blocks of the spawner. Computed by the caller
    /// because only it holds the player list.
    pub near_player: bool,
    /// The `spawner_blocks_work` game rule (`ServerLevel.isSpawnerBlockEnabled`).
    pub spawner_blocks_work: bool,
    /// World difficulty, for the Peaceful gate.
    pub difficulty: Difficulty,
    /// The spawner's own block position.
    pub pos: BlockPos,
    /// `level.noCollision(entityType.getSpawnAABB(...))`, approximated as "the
    /// block at this candidate's floor cell has an empty collision shape" —
    /// see the module doc's scope note for what this does not check.
    pub is_valid_position: &'a dyn Fn(Vec3) -> bool,
    /// `level.getEntities(EntityTypeTest.forExactClass(...), aabb,
    /// NO_SPECTATORS).size()` — how many entities of exactly `entity_type`
    /// already occupy the box centred on `pos` and inflated by `spawn_range`.
    /// The caller answers this from its own live entity list (`MobSim
    /// ::snapshots`), the same "count the exact type, not the category"
    /// distinction vanilla's `EntityTypeTest.forExactClass` makes.
    pub nearby_count: &'a dyn Fn(&ResourceKey, i32) -> i32,
}

/// A `minecraft:spawner` block entity's live state — `BaseSpawner`'s fields,
/// NBT-shaped so [`crate::chunk_nbt`] can load/save them verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnerState {
    spawn_delay: i32,
    min_spawn_delay: i32,
    max_spawn_delay: i32,
    spawn_count: i32,
    max_nearby_entities: i32,
    required_player_range: i32,
    spawn_range: i32,
    spawn_potentials: Vec<WeightedSpawnData>,
    next_spawn_data: Option<SpawnData>,
}

impl Default for SpawnerState {
    /// `BaseSpawner`'s field initializers: `DEFAULT_SPAWN_DELAY` (20),
    /// `DEFAULT_MIN_SPAWN_DELAY` (200), `DEFAULT_MAX_SPAWN_DELAY` (800),
    /// `DEFAULT_SPAWN_COUNT` (4), `DEFAULT_MAX_NEARBY_ENTITIES` (6),
    /// `DEFAULT_REQUIRED_PLAYER_RANGE` (16), `DEFAULT_SPAWN_RANGE` (4), an
    /// empty `spawnPotentials` and no `nextSpawnData` — a freshly placed
    /// spawner with nothing to spawn, exactly vanilla's `/setblock`d one.
    fn default() -> Self {
        Self {
            spawn_delay: 20,
            min_spawn_delay: 200,
            max_spawn_delay: 800,
            spawn_count: 4,
            max_nearby_entities: 6,
            required_player_range: 16,
            spawn_range: 4,
            spawn_potentials: Vec::new(),
            next_spawn_data: None,
        }
    }
}

impl SpawnerState {
    /// Reconstructs a spawner's state from its saved NBT fields —
    /// [`crate::chunk_nbt`]'s load path. Mirrors `BaseSpawner.load` field for
    /// field; a caller building a fresh (not-loaded-from-disk) spawner should
    /// use [`SpawnerState::default`] instead.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        spawn_delay: i32,
        min_spawn_delay: i32,
        max_spawn_delay: i32,
        spawn_count: i32,
        max_nearby_entities: i32,
        required_player_range: i32,
        spawn_range: i32,
        spawn_potentials: Vec<WeightedSpawnData>,
        next_spawn_data: Option<SpawnData>,
    ) -> Self {
        Self {
            spawn_delay,
            min_spawn_delay,
            max_spawn_delay,
            spawn_count,
            max_nearby_entities,
            required_player_range,
            spawn_range,
            spawn_potentials,
            next_spawn_data,
        }
    }

    /// `required_player_range` — [`crate::tick::run_tick_loop`] reads this to
    /// compute [`SpawnCtx::near_player`] itself, since only it holds the
    /// player list.
    #[must_use]
    pub fn required_player_range(&self) -> i32 {
        self.required_player_range
    }

    /// The saved-NBT view of every field, for [`crate::chunk_nbt`]'s save path.
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn saved_fields(
        &self,
    ) -> (
        i32,
        i32,
        i32,
        i32,
        i32,
        i32,
        i32,
        &[WeightedSpawnData],
        Option<&SpawnData>,
    ) {
        (
            self.spawn_delay,
            self.min_spawn_delay,
            self.max_spawn_delay,
            self.spawn_count,
            self.max_nearby_entities,
            self.required_player_range,
            self.spawn_range,
            &self.spawn_potentials,
            self.next_spawn_data.as_ref(),
        )
    }

    /// `WeightedList.getRandom` over `spawn_potentials`: a weighted draw, or
    /// `None` when the list is empty or every weight is `0` (vanilla's
    /// `totalWeight == 0` → `selector = null` → `getRandom` always empty).
    fn weighted_pick(&self, rng: &mut SpawnRng) -> Option<SpawnData> {
        let total: u32 = self.spawn_potentials.iter().map(|w| w.weight).sum();
        if total == 0 {
            return None;
        }
        let mut roll = rng.next_int(total as i32);
        for entry in &self.spawn_potentials {
            if roll < entry.weight as i32 {
                return Some(entry.data.clone());
            }
            roll -= entry.weight as i32;
        }
        // Unreachable while `total` and the walk agree, kept as a safe
        // fallback rather than a panic — an empty spawn is honest, a crash on
        // a malformed weight table is not.
        None
    }

    /// `BaseSpawner.delay`: rerolls `spawn_delay` uniformly over
    /// `[min_spawn_delay, max_spawn_delay)` (or pins to `min_spawn_delay` when
    /// the range is empty or inverted), and rerolls `next_spawn_data` **only**
    /// when the weighted draw succeeds — an empty `spawn_potentials` leaves
    /// whatever `next_spawn_data` already held untouched, exactly
    /// `Optional::ifPresent`'s no-op-on-empty semantics.
    fn reroll(&mut self, rng: &mut SpawnRng) {
        self.spawn_delay = if self.max_spawn_delay <= self.min_spawn_delay {
            self.min_spawn_delay
        } else {
            self.min_spawn_delay + rng.next_int(self.max_spawn_delay - self.min_spawn_delay)
        };
        if let Some(picked) = self.weighted_pick(rng) {
            self.next_spawn_data = Some(picked);
        }
    }

    /// `BaseSpawner.getOrCreateNextSpawnData`: returns the pinned
    /// `next_spawn_data`, or draws one and pins it (falling back to
    /// [`SpawnData::NONE`] when `spawn_potentials` is empty).
    fn next_spawn_data(&mut self, rng: &mut SpawnRng) -> SpawnData {
        if let Some(next) = &self.next_spawn_data {
            return next.clone();
        }
        let picked = self.weighted_pick(rng).unwrap_or_default();
        self.next_spawn_data = Some(picked.clone());
        picked
    }

    /// `BaseSpawner.serverTick`, reduced to the decision this crate can act
    /// on. See the module doc for the full clause table and what is
    /// deliberately not modelled (per-species placement rules, custom spawn
    /// rules, equipment, entity-vs-entity collision, passengers).
    ///
    /// # Clauses, in vanilla's own order
    ///
    /// 1. `isNearPlayer && isSpawnerBlockEnabled` — gate the whole method
    ///    ([`SpawnCtx::near_player`] / [`SpawnCtx::spawner_blocks_work`]).
    /// 2. `spawnDelay == -1` primes a fresh delay (dead in this port: nothing
    ///    here ever sets `spawn_delay` to `-1`, since [`SpawnerState::default`]
    ///    and NBT loading both produce a real delay — kept for parity with
    ///    the Java source rather than removed as unreachable).
    /// 3. `spawnDelay > 0` counts down and returns.
    /// 4. Otherwise, up to `spawn_count` attempts: resolve the entity type
    ///    (empty → [`Self::reroll`] and abandon the remaining attempts, matching
    ///    vanilla's early `return`), pick a random cell, check collision, check
    ///    the peaceful gate, check the nearby-count cap (exceeded →
    ///    [`Self::reroll`] and abandon), else record a [`SpawnAttempt`].
    /// 5. If **any** attempt in the loop succeeded, [`Self::reroll`] once at the
    ///    end. If none did (every attempt merely `continue`d — collision or
    ///    peaceful refused every candidate), `spawn_delay` stays at `0` and the
    ///    very next tick retries the same `next_spawn_data` immediately —
    ///    vanilla's own behaviour, not a bug in this port.
    #[must_use]
    pub fn tick(&mut self, ctx: &SpawnCtx<'_>, rng: &mut SpawnRng) -> Vec<SpawnAttempt> {
        let mut out = Vec::new();
        if !(ctx.near_player && ctx.spawner_blocks_work) {
            return out;
        }
        if self.spawn_delay == -1 {
            self.reroll(rng);
        }
        if self.spawn_delay > 0 {
            self.spawn_delay -= 1;
            return out;
        }

        let next = self.next_spawn_data(rng);
        let mut any_spawned = false;
        for _ in 0..self.spawn_count.max(0) {
            let Some(entity_type) = next.entity_type.clone() else {
                self.reroll(rng);
                return out;
            };

            let dx = (rng.next_f64() - rng.next_f64()) * f64::from(self.spawn_range);
            let dz = (rng.next_f64() - rng.next_f64()) * f64::from(self.spawn_range);
            let dy = rng.next_int(3) - 1;
            let candidate = Vec3::new(
                f64::from(ctx.pos.x) + dx + 0.5,
                f64::from(ctx.pos.y + dy),
                f64::from(ctx.pos.z) + dz + 0.5,
            );

            if !(ctx.is_valid_position)(candidate) {
                continue;
            }
            // `SpawnPlacements.checkSpawnRules`'s own first statement — the one
            // clause of the per-species predicate every species shares. See
            // the module doc for why the rest of that predicate (light,
            // ground, biome) is not evaluated here.
            if ctx.difficulty == Difficulty::Peaceful && !allowed_in_peaceful(entity_type.path()) {
                continue;
            }

            let nearby = (ctx.nearby_count)(&entity_type, self.spawn_range);
            if nearby >= self.max_nearby_entities {
                self.reroll(rng);
                return out;
            }

            out.push(SpawnAttempt {
                entity_type,
                position: candidate,
            });
            any_spawned = true;
        }

        if any_spawned {
            self.reroll(rng);
        }
        out
    }
}

/// [`SpawnerState::tick`] **plus the spawn** — the composition, named for the
/// same reason [`crate::spawn_egg::apply_spawn_egg`] is: a decision function
/// and a spawn function can each be correct while the seam between them is
/// unguarded, and a seam with no name has nothing to point a test at.
///
/// Returns the network ids of everything spawned this call, so a caller (or a
/// test) can look them up in [`crate::MobSim::snapshots`].
pub fn apply_spawner_tick(
    state: &mut SpawnerState,
    ctx: &SpawnCtx<'_>,
    rng: &mut SpawnRng,
    mobs: &crate::MobHandle,
) -> Vec<i32> {
    state
        .tick(ctx, rng)
        .into_iter()
        .map(|attempt| {
            mobs.with(|sim| sim.spawn_species(attempt.entity_type, attempt.position).id())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> ResourceKey {
        s.parse().unwrap()
    }

    fn zombie_potentials() -> Vec<WeightedSpawnData> {
        vec![WeightedSpawnData {
            weight: 1,
            data: SpawnData {
                entity_type: Some(key("minecraft:zombie")),
            },
        }]
    }

    /// Not near a player: no countdown happens and nothing spawns, whatever
    /// the delay currently is — `isNearPlayer` gates the whole method.
    #[test]
    fn far_from_every_player_does_nothing() {
        let mut state = SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, zombie_potentials(), None);
        let mut rng = SpawnRng::new(1);
        let valid = |_: Vec3| true;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: false,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &valid,
            nearby_count: &nearby,
        };
        let out = state.tick(&ctx, &mut rng);
        assert!(out.is_empty());
        assert_eq!(
            state.saved_fields().0,
            0,
            "the delay counter must not move while no player is near"
        );
    }

    /// The `spawner_blocks_work` game rule gates the method exactly like
    /// `near_player` does — the discriminating pair, since a gate that only
    /// checked `near_player` would pass every other row here too.
    #[test]
    fn the_game_rule_gates_it_even_with_a_nearby_player() {
        let mut state = SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, zombie_potentials(), None);
        let mut rng = SpawnRng::new(1);
        let valid = |_: Vec3| true;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: false,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &valid,
            nearby_count: &nearby,
        };
        assert!(state.tick(&ctx, &mut rng).is_empty());
    }

    /// A positive delay just counts down by one and produces nothing.
    #[test]
    fn a_positive_delay_counts_down_and_spawns_nothing() {
        let mut state = SpawnerState::restore(5, 200, 800, 4, 6, 16, 4, zombie_potentials(), None);
        let mut rng = SpawnRng::new(1);
        let valid = |_: Vec3| true;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &valid,
            nearby_count: &nearby,
        };
        let out = state.tick(&ctx, &mut rng);
        assert!(out.is_empty());
        assert_eq!(state.saved_fields().0, 4);
    }

    /// **The composition reaches `MobSim::snapshots`** — the assertion that
    /// matters per this crate's evidence standards: not that `tick` returned
    /// something, but that the entity is on the wire. When the delay hits
    /// zero with every candidate valid, at least one zombie is spawned and
    /// its id is present in the snapshot set.
    #[test]
    fn a_fired_spawner_puts_real_entities_into_the_snapshot_set() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let before = mobs.with(|sim| sim.snapshots().len());

        let mut state = SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, zombie_potentials(), None);
        let mut rng = SpawnRng::new(7);
        let valid = |_: Vec3| true;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &valid,
            nearby_count: &nearby,
        };
        let ids = apply_spawner_tick(&mut state, &ctx, &mut rng, &mobs);
        assert_eq!(ids.len(), 4, "spawn_count is 4 and every candidate is valid");

        let snapshots = mobs.with(|sim| sim.snapshots());
        assert_eq!(snapshots.len(), before + 4);
        for id in &ids {
            let spawned = snapshots
                .iter()
                .find(|s| s.id == *id)
                .expect("every spawned id must be in the snapshot set that becomes ADD_ENTITY");
            assert_eq!(spawned.entity_type.to_string(), "minecraft:zombie");
        }
        // Firing rerolled the delay away from 0 into the configured window.
        let (delay, ..) = state.saved_fields();
        assert!((200..800).contains(&delay), "got {delay}");
    }

    /// Collision refusal: every candidate invalid means nothing spawns **and**
    /// the delay is left at exactly `0` rather than rerolled — the "retry next
    /// tick with the same data" behaviour the module doc calls out.
    #[test]
    fn every_candidate_colliding_refuses_and_does_not_reroll() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let mut state = SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, zombie_potentials(), None);
        let mut rng = SpawnRng::new(3);
        let never_valid = |_: Vec3| false;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &never_valid,
            nearby_count: &nearby,
        };
        let ids = apply_spawner_tick(&mut state, &ctx, &mut rng, &mobs);
        assert!(ids.is_empty());
        assert_eq!(mobs.with(|sim| sim.snapshots().len()), 0);
        assert_eq!(
            state.saved_fields().0,
            0,
            "no attempt succeeded, so the delay must stay exactly 0 (vanilla retries next tick)"
        );
    }

    /// **Peaceful refuses a monster and accepts an animal** — the
    /// discriminating pair per this repo's evidence standards, since a gate
    /// on the monster alone passes an implementation that refuses everything.
    #[test]
    fn peaceful_refuses_a_monster_only() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let valid = |_: Vec3| true;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let peaceful_ctx = |pos| SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Peaceful,
            pos,
            is_valid_position: &valid,
            nearby_count: &nearby,
        };

        let mut zombie_spawner =
            SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, zombie_potentials(), None);
        let mut rng = SpawnRng::new(11);
        let ids = apply_spawner_tick(
            &mut zombie_spawner,
            &peaceful_ctx(BlockPos::new(0, 64, 0)),
            &mut rng,
            &mobs,
        );
        assert!(ids.is_empty(), "a zombie must never spawn on Peaceful");

        let pig_potentials = vec![WeightedSpawnData {
            weight: 1,
            data: SpawnData {
                entity_type: Some(key("minecraft:pig")),
            },
        }];
        let mut pig_spawner =
            SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, pig_potentials, None);
        let ids = apply_spawner_tick(
            &mut pig_spawner,
            &peaceful_ctx(BlockPos::new(0, 64, 0)),
            &mut rng,
            &mobs,
        );
        assert!(
            !ids.is_empty(),
            "a pig is allowed in peaceful, so refusing it would mean the gate refuses \
             everything rather than only monsters"
        );
    }

    /// The nearby-entity cap: once the count callback reports the cap has
    /// been reached, the first attempt reroll-and-abandons rather than
    /// spawning anything.
    #[test]
    fn the_nearby_cap_refuses_before_any_spawn() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let mut state = SpawnerState::restore(
            0,
            200,
            800,
            4,
            /* max_nearby_entities */ 1,
            16,
            4,
            zombie_potentials(),
            None,
        );
        let mut rng = SpawnRng::new(5);
        let valid = |_: Vec3| true;
        let at_cap = |_: &ResourceKey, _: i32| 1;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &valid,
            nearby_count: &at_cap,
        };
        let ids = apply_spawner_tick(&mut state, &ctx, &mut rng, &mobs);
        assert!(ids.is_empty());
        let (delay, ..) = state.saved_fields();
        assert!(
            (200..800).contains(&delay),
            "the cap failure still rerolls the delay (unlike a plain collision refusal): got {delay}"
        );
    }

    /// A `SpawnData` naming no entity (no `id` at all — vanilla's empty
    /// default) spawns nothing and still rerolls, matching
    /// `entityType.isEmpty()`'s early return through `delay()`.
    #[test]
    fn no_entity_id_spawns_nothing_and_rerolls() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        // Empty spawn_potentials AND no next_spawn_data: `next_spawn_data`
        // falls back to `SpawnData::NONE`.
        let mut state = SpawnerState::restore(0, 200, 800, 4, 6, 16, 4, Vec::new(), None);
        let mut rng = SpawnRng::new(2);
        let valid = |_: Vec3| true;
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos: BlockPos::new(0, 64, 0),
            is_valid_position: &valid,
            nearby_count: &nearby,
        };
        let ids = apply_spawner_tick(&mut state, &ctx, &mut rng, &mobs);
        assert!(ids.is_empty());
        assert_eq!(mobs.with(|sim| sim.snapshots().len()), 0);
    }

    /// The candidate position lands within `spawn_range` of the spawner block
    /// on every axis, and the y term is one of exactly `{-1, 0, 1}` —
    /// `BaseSpawner`'s own arithmetic, checked over many draws so a
    /// mis-transcribed bound (e.g. `spawn_range` used as a diameter instead of
    /// a radius) would show up as an out-of-range sample.
    #[test]
    fn candidate_positions_stay_within_spawn_range() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let collected = std::cell::RefCell::new(Vec::<Vec3>::new());
        let mut state = SpawnerState::restore(
            0, 200, 800, /* spawn_count */ 50, /* max_nearby */ 10_000, 16,
            /* spawn_range */ 4, zombie_potentials(), None,
        );
        let pos = BlockPos::new(100, 64, -50);
        let mut rng = SpawnRng::new(99);
        // Record every candidate offered to `is_valid_position`, refusing
        // none, so the loop runs the full `spawn_count` attempts. `RefCell`
        // rather than a plain `Vec` capture: `SpawnCtx::is_valid_position` is
        // `dyn Fn`, not `dyn FnMut`, matching production's read-only closures.
        let recording_valid = |v: Vec3| {
            collected.borrow_mut().push(v);
            false // refuse everything: this test only cares about the sample
        };
        let nearby = |_: &ResourceKey, _: i32| 0;
        let ctx = SpawnCtx {
            near_player: true,
            spawner_blocks_work: true,
            difficulty: Difficulty::Normal,
            pos,
            is_valid_position: &recording_valid,
            nearby_count: &nearby,
        };
        let ids = apply_spawner_tick(&mut state, &ctx, &mut rng, &mobs);
        assert!(ids.is_empty());
        let collected = collected.into_inner();
        assert_eq!(collected.len(), 50);
        let mut out_of_range: Vec<Vec3> = Vec::new();
        for v in &collected {
            let dx = v.x - (f64::from(pos.x) + 0.5);
            let dz = v.z - (f64::from(pos.z) + 0.5);
            let dy = v.y - f64::from(pos.y);
            if dx.abs() > 4.0 || dz.abs() > 4.0 || !(-1.0..=1.0).contains(&dy) {
                out_of_range.push(*v);
            }
        }
        assert!(
            out_of_range.is_empty(),
            "{} of 50 candidates fell outside the spawn_range=4 box: {out_of_range:?}",
            out_of_range.len()
        );
    }
}
