//! `MobSim`'s lightning-bolt slice: the sidecar that turns a decided
//! [`crate::lightning::Strike`] into a real, ticking, network-visible entity,
//! and the per-tick effects it plays on whatever else is
//! standing under it.
//!
//! # What it is
//!
//! `crates/lodestone-server/src/lightning.rs` already owns every rule here —
//! strike-target selection, the bolt `life`/`flashes` state machine, and the
//! effect dispatch table — but that module has no entity type to spawn
//! into (`MobSim` holds mobs and item entities only) and no world to mutate.
//! This file is the missing "turn a decision into a mutation" half that
//! module's own doc names as blocked on `crate::mobs`: a
//! `lightning_bolts: HashMap<i32, LiveBolt>` sidecar beside the existing
//! `orbs`/`item_state` maps, following [`super::orbs`]'s own shape. A bolt has
//! no collision box or AI state.
//!
//! # How it works
//!
//! [`MobSim::spawn_lightning_bolts`] turns each [`Strike`] the driver's
//! per-chunk strike pass decided on into a [`LiveBolt`] —
//! [`BoltState::new`]'s own `life`/`flashes` roll, this crate's own entity-id
//! assignment. [`MobSim::tick_lightning`] is the per-tick driver: it runs
//! [`lightning::tick_bolt`] for every live bolt (including one spawned during
//! the same tick — chunk-level lightning runs before entity ticks, the same
//! ordering that `MobSim::tick_falling_blocks`'s caller in
//! `crate::tick` already documents for a block-tick-spawned falling block),
//! collects fire-ignition attempts for the driver, and — for a bolt whose
//! `hit_entities` fired — resolves [`lightning::resolve_effect`] against every
//! live mob within [`lightning::DAMAGE_RADIUS`].
//!
//! # What is out of reach from here, deliberately
//!
//! - **Fire ignition never writes through `self.world`.** [`super::MobSim`]'s
//!   `world: &'w ChunkWorld` is a frozen pathfinding snapshot, not the live
//!   `ChunkStore` — [`super::MobSim::take_grazes`]'s own doc comment
//!   establishes exactly this for block-eating goals, and the same reasoning
//!   applies here. Ignition attempts are recorded in
//!   `pending_lightning_fires` and drained by
//!   [`MobSim::take_lightning_fires`]; the driver
//!   (`crate::tick::run_tick_loop_with_weather`) is expected to test each
//!   candidate with `crate::fire::can_survive` against the *live* world and
//!   write `crate::fire::state_for_placement` only where the cell is air and
//!   survives, matching the fire-placement gate.
//! - **The lightning-rod power pulse, copper waxing/weathering reset, game-event
//!   notification and thunder-sound pair are not applied.**
//!   [`lightning::BoltTickEffects`] carries real flags for all four
//!   (`power_lightning_rod`, `clear_copper`, `game_event`,
//!   `play_thunder_sounds`); this file reads only `fire_attempts` and
//!   `hit_entities` from it. A disclosed gap, not a silent one — none of the
//!   four has a consumer here yet.
//! - **The creeper powered flag is not streamed, and no "powered" state is
//!   recorded anywhere.** [`super::CREEPER_EXPLOSION_RADIUS`]'s own doc
//!   already discloses this gap for the doubled-explosion half;
//!   [`LightningEffect::BecomeCharged`] effect applies the same damage as the
//!   default case for the same reason — the wire index this would need lives
//!   in the protocol layer, which does not expose that field. Adding a field
//!   nothing reads would be exactly the "island"
//!   shape this repo's own evidence section warns against.
//! - **A struck mob is not set alight.** `MobSim` has no burn state for a mob
//!   at all — `crate::burning`'s own module doc: *"`MobSim` has no burn state
//!   and streams no `on_fire` metadata flag"*, player-only, the same gap
//!   drowning has no equivalent mob-side state. The damage effect is supported
//!   here; mob ignition has no state to receive it.
//! - **The mooshroom variant never actually flips.** No species in this crate
//!   models a red/brown variant at all (`grep`ping `mobs/species.rs` for
//!   "mooshroom" finds only diet/breeding/baby-scale tables). So
//!   [`LightningEffect::ToggleMooshroomVariant`]'s handler records the guard
//!   (`SimMob::last_lightning_bolt`) and nothing else. The recorded guard prevents
//!   duplicate toggles if a variant field gains a consumer later.
//! - **Pig→zombified-piglin and villager→witch conversion is real but
//!   minimal.** [`convert_species`] replaces the source record with the target
//!   species and deliberately does **not**
//!   preserve health, equipment, age or leash state because this sim has no
//!   matching transfer state for those fields.
//!
//! # Dependencies
//!
//! [`crate::lightning`] for every rule; [`crate::mob_spawn::SpawnRng`] for the
//! bolt's own `life`/`flashes` roll, on a stream the driver keeps separate
//! from strike *target selection* (`tick_thunder_for_chunk`'s own `rng`
//! parameter) for [`super::orbs::ORB_BEHAVIOR_SEED`]'s reason: a strike
//! decision must not shift which roll a bolt's own state machine sees.

use uuid::Uuid;

use lodestone_entity::DamageFlags;
use lodestone_model::{BlockPos, Difficulty, ResourceKey, Vec3};

use crate::lightning::{self, BoltState, LightningEffect, Strike};
use crate::mob_spawn::SpawnRng;

use super::MobSim;

/// One live lightning sidecar — the lightning twin of [`super::orbs::OrbState`]:
/// a lifecycle and a fixed position, with no motion or AI after the strike.
#[derive(Debug, Clone, Copy)]
pub(super) struct LiveBolt {
    pub(super) uuid: Uuid,
    pub(super) state: BoltState,
    /// The bolt's world position at the strike cell's bottom centre, fixed for
    /// the bolt's whole life.
    pub(super) pos: Vec3,
    /// [`lightning::strike_ground_pos`] of `pos`, computed once at spawn since
    /// it never changes across the bolt's life (`tick_bolt`'s own doc
    /// comment).
    pub(super) ground_pos: BlockPos,
}

/// The chunk that owns a bolt at the start of its tick.
///
/// This is a deterministic hand-off boundary, not a worker assignment. The
/// central application step remains the only writer to the live bolt map,
/// fire-attempt queue, and mob-effect path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LightningTickOwner {
    Chunk { cx: i32, cz: i32 },
}

impl LightningTickOwner {
    fn for_position(position: Vec3) -> Self {
        Self::Chunk {
            cx: (position.x.floor() as i32).div_euclid(16),
            cz: (position.z.floor() as i32).div_euclid(16),
        }
    }
}

/// One completed lightning-owner batch.
#[derive(Debug, Clone)]
pub(crate) struct LightningTickOwnerBatch {
    owner: LightningTickOwner,
    expected_batch_count: usize,
    effects: Vec<LightningTickEffect>,
}

impl LightningTickOwnerBatch {
    #[cfg(test)]
    fn owner(&self) -> LightningTickOwner {
        self.owner
    }
}

#[derive(Debug, Clone)]
struct LightningTickEffect {
    owner: LightningTickOwner,
    serial: usize,
    id: i32,
    bolt: LiveBolt,
    fire_attempts: Vec<BlockPos>,
    hit_entities: bool,
    discard: bool,
}

/// The wire entity type every lightning bolt streams as. Named rather than
/// numeric for [`super::item_entity_type`]'s documented reason.
pub(super) fn lightning_bolt_entity_type() -> ResourceKey {
    lightning::LIGHTNING_BOLT
        .parse()
        .expect("`minecraft:lightning_bolt` is a valid resource key")
}

/// Floors a world position to the [`BlockPos`] it falls in. This uses ordinary
/// floor semantics with no half-open epsilon subtraction, unlike
/// [`lightning::strike_ground_pos`]'s caller.
pub(super) fn floor_block_pos(v: Vec3) -> BlockPos {
    BlockPos::new(v.x.floor() as i32, v.y.floor() as i32, v.z.floor() as i32)
}

impl<'w> MobSim<'w> {
    /// Turns every decided [`Strike`] into a live [`LiveBolt`] —
    /// [`BoltState::new`]'s `life = 2` and `flashes = random.nextInt(3) + 1`,
    /// followed by this crate's entity-id/UUID assignment, using the same
    /// pattern as [`MobSim::spawn_orb`].
    pub fn spawn_lightning_bolts(&mut self, strikes: Vec<Strike>, rng: &mut SpawnRng) {
        for strike in strikes {
            let id = self.next_id;
            self.next_id += 1;
            let pos = Vec3::new(
                f64::from(strike.pos.x) + 0.5,
                f64::from(strike.pos.y),
                f64::from(strike.pos.z) + 0.5,
            );
            self.lightning_bolts.insert(
                id,
                LiveBolt {
                    uuid: Uuid::new_v4(),
                    state: BoltState::new(rng, strike.visual_only),
                    pos,
                    ground_pos: lightning::strike_ground_pos(strike.pos),
                },
            );
        }
    }

    /// One tick of every live bolt: [`lightning::tick_bolt`]'s state machine,
    /// fire-attempt collection for the driver, and entity effects for
    /// whichever bolts hit this tick. Safe — and a cheap no-op — to call every
    /// tick even with nothing struck; a bolt spawned this same tick is ticked
    /// too (see the module doc for why that ordering is deliberate).
    pub fn tick_lightning(&mut self, difficulty: Difficulty, rng: &mut SpawnRng) {
        let batches = self.tick_lightning_owner_batches(difficulty, rng);
        self.apply_lightning_tick_owner_batches(batches, difficulty);
    }

    /// Produces chunk-owner completions from cloned tick-start bolt state.
    ///
    /// The shared bolt RNG remains serial in entity-id order. Each completion
    /// carries only its changed bolt and deferred visible effects; the central
    /// apply step restores serial order before it mutates shared state.
    pub(crate) fn tick_lightning_owner_batches(
        &self,
        difficulty: Difficulty,
        rng: &mut SpawnRng,
    ) -> Vec<LightningTickOwnerBatch> {
        let mut ids: Vec<i32> = self.lightning_bolts.keys().copied().collect();
        ids.sort_unstable();
        let mut batches = Vec::<LightningTickOwnerBatch>::new();
        for (serial, id) in ids.into_iter().enumerate() {
            let mut bolt = *self
                .lightning_bolts
                .get(&id)
                .expect("a tick-start lightning id must remain live while planning");
            let owner = LightningTickOwner::for_position(bolt.pos);
            let fx = lightning::tick_bolt(&mut bolt.state, bolt.ground_pos, difficulty, rng);
            let effect = LightningTickEffect {
                owner,
                serial,
                id,
                bolt,
                fire_attempts: fx.fire_attempts,
                hit_entities: fx.hit_entities,
                discard: fx.discard,
            };
            if let Some(batch) = batches.iter_mut().find(|batch| batch.owner == owner) {
                batch.effects.push(effect);
            } else {
                batches.push(LightningTickOwnerBatch {
                    owner,
                    expected_batch_count: 0,
                    effects: vec![effect],
                });
            }
        }
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.expected_batch_count = batch_count;
        }
        batches
    }

    /// Validates and centrally applies completed lightning-owner batches.
    ///
    /// The central writer restores entity-id serial slots before emitting fire
    /// attempts, applying mob effects, or discarding a bolt, so owner
    /// completion order cannot reorder visible lightning consequences.
    pub(crate) fn apply_lightning_tick_owner_batches(
        &mut self,
        batches: Vec<LightningTickOwnerBatch>,
        difficulty: Difficulty,
    ) {
        if batches.is_empty() {
            assert!(
                self.lightning_bolts.is_empty(),
                "lightning owner completion must retain every live tick-start bolt"
            );
            return;
        }
        let effects = merge_lightning_tick_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.lightning_bolts.len(),
            "lightning owner completion must retain every live tick-start bolt"
        );
        let mut ids = std::collections::HashSet::new();
        let mut hits = Vec::new();
        let mut discarded = Vec::new();
        for effect in effects {
            assert!(
                ids.insert(effect.id),
                "lightning owner completion may update one live bolt only once"
            );
            assert!(
                self.lightning_bolts.contains_key(&effect.id),
                "a lightning owner completion may update only a live tick-start bolt"
            );
            self.lightning_bolts.insert(effect.id, effect.bolt);
            self.pending_lightning_fires.extend(effect.fire_attempts);
            if effect.hit_entities {
                hits.push((effect.id, effect.bolt.pos));
            }
            if effect.discard {
                discarded.push(effect.id);
            }
        }
        for (bolt_id, bolt_pos) in hits {
            self.apply_lightning_hits(bolt_id, bolt_pos, difficulty);
        }
        for id in discarded {
            self.lightning_bolts.remove(&id);
        }
    }

    /// Every live mob within [`lightning::DAMAGE_RADIUS`] of `bolt_pos` is
    /// dispatched
    /// through [`lightning::resolve_effect`]. Players are not hit here: this
    /// sim owns no player health (`crate::server`/`PlayerVitals` does), so a
    /// struck player is outside this sim's responsibility: player health lives in
    /// `crate::server`/`PlayerVitals`, so this mob-side method drops that target
    /// explicitly rather than guessing at player state.
    fn apply_lightning_hits(&mut self, bolt_id: i32, bolt_pos: Vec3, difficulty: Difficulty) {
        let radius_sqr = lightning::DAMAGE_RADIUS * lightning::DAMAGE_RADIUS;
        let targets: Vec<(i32, LightningEffect)> = self
            .mobs
            .iter()
            .filter(|m| super::dist_sqr(m.position(), bolt_pos) <= radius_sqr)
            // `resolve_effect`'s own table is keyed on the **full** resource
            // key (`"minecraft:turtle"`, matching its own test fixtures), not
            // the bare path `ResourceKey::path()` returns (`"turtle"`) — a
            // mismatch that made every species-specific effect silently fall
            // through to the default and was caught by this file's own gates.
            .map(|m| (m.id(), lightning::resolve_effect(&m.entity_type().to_string(), difficulty)))
            .collect();
        let mut converted: Vec<(i32, &'static str)> = Vec::new();
        for (id, effect) in targets {
            match effect {
                LightningEffect::ConvertToZombifiedPiglin => {
                    converted.push((id, "minecraft:zombified_piglin"));
                }
                LightningEffect::ConvertToWitch => converted.push((id, "minecraft:witch")),
                LightningEffect::ToggleMooshroomVariant => {
                    if let Some(m) = self.get_mut(id)
                        && m.last_lightning_bolt != Some(bolt_id)
                    {
                        // See the module doc: the guard is real, the variant
                        // it would flip is not modelled yet.
                        m.last_lightning_bolt = Some(bolt_id);
                    }
                }
                LightningEffect::Lethal => {
                    if let Some(m) = self.get_mut(id) {
                        let applied = m.apply_damage(f32::MAX, DamageFlags::default());
                        self.note_vocalisation(id, applied);
                    }
                }
                LightningEffect::DamageAndIgnite | LightningEffect::BecomeCharged => {
                    if let Some(m) = self.get_mut(id) {
                        let applied = m.apply_damage(lightning::DEFAULT_DAMAGE, DamageFlags::default());
                        self.note_vocalisation(id, applied);
                    }
                }
            }
        }
        for (id, new_type) in converted {
            self.convert_species(id, new_type);
        }
        self.reap_dead();
    }

    /// `Entity.convertTo`, reduced to what this crate can express with no NBT
    /// carry-over — see the module doc's "pig/villager conversion is real but
    /// minimal" entry for exactly what is and is not preserved.
    fn convert_species(&mut self, id: i32, new_type: &str) {
        let Some(pos) = self.position(id) else {
            return;
        };
        self.mobs.retain(|m| m.id != id);
        if let Ok(key) = new_type.parse::<ResourceKey>() {
            self.spawn_species(key, pos);
        }
    }

    /// The number of live lightning-bolt entities — for a gate that needs to
    /// see the sidecar directly rather than filter [`MobSim::snapshots`].
    #[must_use]
    pub fn lightning_bolt_count(&self) -> usize {
        self.lightning_bolts.len()
    }
}

fn merge_lightning_tick_owner_batches(
    mut batches: Vec<LightningTickOwnerBatch>,
) -> Vec<LightningTickEffect> {
    let expected_batch_count = batches
        .first()
        .map(|batch| batch.expected_batch_count)
        .expect("lightning owner completion must contain every tick-start owner batch");
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            batch.expected_batch_count, expected_batch_count,
            "lightning owner completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "lightning owner completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "a lightning owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "lightning owner completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "lightning owner completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::lightning::Strike;
    use crate::mobs::ChunkWorld;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn rng() -> SpawnRng {
        SpawnRng::new(0x11DE_7116_5EED_0003)
    }

    fn owner_batch_fixture() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(flat_world()));
        let mut sim = MobSim::new(world);
        sim.spawn_lightning_bolts(
            vec![
                Strike { pos: BlockPos::new(-1, 1, -1), visual_only: true },
                Strike { pos: BlockPos::new(16, 1, 0), visual_only: true },
                Strike { pos: BlockPos::new(-1, 1, -1), visual_only: true },
            ],
            &mut rng(),
        );
        sim
    }

    fn bolt_states(sim: &MobSim<'_>) -> Vec<(i32, BoltState)> {
        let mut states: Vec<_> = sim
            .lightning_bolts
            .iter()
            .map(|(&id, bolt)| (id, bolt.state))
            .collect();
        states.sort_unstable_by_key(|(id, _)| *id);
        states
    }

    #[test]
    fn lightning_owner_batches_restore_serial_state_after_reversed_completion() {
        let mut serial = owner_batch_fixture();
        let mut serial_rng = rng();
        serial.tick_lightning(Difficulty::Hard, &mut serial_rng);
        let serial_states = bolt_states(&serial);

        let mut completed = owner_batch_fixture();
        let mut completed_rng = rng();
        let mut batches = completed.tick_lightning_owner_batches(Difficulty::Hard, &mut completed_rng);
        assert_eq!(
            batches.iter().map(LightningTickOwnerBatch::owner).collect::<Vec<_>>(),
            [
                LightningTickOwner::Chunk { cx: -1, cz: -1 },
                LightningTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "owners enter the tick-start plan at their first serial bolt"
        );
        batches.reverse();
        let raw_completion = batches
            .iter()
            .flat_map(|batch| batch.effects.iter())
            .map(|effect| effect.serial)
            .collect::<Vec<_>>();
        assert_ne!(
            raw_completion,
            vec![0, 1, 2],
            "control requires reversed owner completion to move raw serial slots"
        );
        let restored = merge_lightning_tick_owner_batches(batches.clone())
            .into_iter()
            .map(|effect| effect.serial)
            .collect::<Vec<_>>();
        assert_eq!(restored, vec![0, 1, 2], "the central merge must restore serial slots");

        completed.apply_lightning_tick_owner_batches(batches, Difficulty::Hard);
        assert_eq!(
            bolt_states(&completed),
            serial_states,
            "the live central consumer must preserve the serial bolt state"
        );
    }

    #[test]
    #[should_panic(expected = "every tick-start owner batch exactly once")]
    fn lightning_owner_batch_merge_rejects_a_missing_owner() {
        let mut random = rng();
        let mut batches = owner_batch_fixture().tick_lightning_owner_batches(Difficulty::Hard, &mut random);
        batches.pop();
        let _ = merge_lightning_tick_owner_batches(batches);
    }

    #[test]
    #[should_panic(expected = "may not contain one owner twice")]
    fn lightning_owner_batch_merge_rejects_a_duplicate_owner() {
        let mut random = rng();
        let mut batches = owner_batch_fixture().tick_lightning_owner_batches(Difficulty::Hard, &mut random);
        batches[1] = batches[0].clone();
        let _ = merge_lightning_tick_owner_batches(batches);
    }

    #[test]
    #[should_panic(expected = "retain every live tick-start bolt")]
    fn lightning_owner_apply_rejects_no_completions_for_live_bolts() {
        owner_batch_fixture().apply_lightning_tick_owner_batches(Vec::new(), Difficulty::Hard);
    }

    /// **The production wiring gate.** A strike published
    /// during a thunderstorm reaches [`MobSim::snapshots`] as a real
    /// `minecraft:lightning_bolt` entity with empty metadata; a sim that never
    /// receives a strike (the "clear weather" arm — modelled here by simply
    /// not calling [`MobSim::spawn_lightning_bolts`], the only producer) never
    /// streams one. A gate that only checked the bolt state machine in
    /// isolation would pass even with nothing wired to `MobSim` at all — see
    /// this module's own doc for the complete producer-to-snapshot chain.
    #[test]
    fn a_strike_reaches_the_snapshot_stream_and_a_sim_with_no_strike_streams_none() {
        let world = flat_world();

        let mut struck = MobSim::new(&world);
        assert!(
            struck.snapshots().iter().all(|s| s.entity_type.to_string() != lightning::LIGHTNING_BOLT),
            "no bolt before any strike is published"
        );
        struck.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(4, 5, 9), visual_only: false }],
            &mut rng(),
        );
        let bolts: Vec<_> = struck
            .snapshots()
            .into_iter()
            .filter(|s| s.entity_type.to_string() == lightning::LIGHTNING_BOLT)
            .collect();
        assert_eq!(bolts.len(), 1, "exactly one bolt for exactly one strike");
        assert_eq!(bolts[0].position, Vec3::new(4.5, 5.0, 9.5), "bottom-centre of the strike cell");
        assert!(bolts[0].metadata.is_empty(), "LightningBolt.defineSynchedData registers nothing");

        // The control: an otherwise-identical sim that never receives a
        // strike (clear weather, or a thunderstorm whose per-chunk roll never
        // hits) must stream zero bolts — the arm a bolt-state-machine-only
        // gate cannot see at all.
        let clear = MobSim::new(&world);
        assert!(
            clear.snapshots().iter().all(|s| s.entity_type.to_string() != lightning::LIGHTNING_BOLT),
            "a sim nothing ever struck must stream no bolt"
        );
    }

    /// A bolt's `life`/`flashes` countdown really runs inside `MobSim`, not
    /// just in `crate::lightning`'s own unit tests — enough ticks discards a
    /// single-flash bolt.
    #[test]
    fn a_bolt_discards_after_its_life_and_flashes_run_out() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(0, 1, 0), visual_only: false }],
            &mut rng(),
        );
        assert_eq!(sim.lightning_bolt_count(), 1);
        let mut r = rng();
        // `flashes` is at most 3 and `life` starts at 2; a generous 40 ticks
        // clears every possible restrike chain (`life < -next_int(10)` can
        // stall at most 9 extra ticks per flash).
        for _ in 0..40 {
            sim.tick_lightning(Difficulty::Normal, &mut r);
        }
        assert_eq!(sim.lightning_bolt_count(), 0, "a bolt must eventually discard itself");
    }

    /// A live mob within `DAMAGE_RADIUS` of a hit-ticking bolt takes real
    /// damage; one well outside the radius does not — the entity-effect half
    /// of `tick_lightning`, not just the state machine.
    #[test]
    fn a_hit_tick_damages_a_nearby_mob_and_not_a_far_one() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let near = sim
            .spawn_species(ResourceKey::from_str("minecraft:cow").expect("valid key"), Vec3::new(4.0, 1.0, 4.0))
            .id();
        let far = sim
            .spawn_species(ResourceKey::from_str("minecraft:cow").expect("valid key"), Vec3::new(40.0, 1.0, 40.0))
            .id();
        let near_health_before = sim.get(near).expect("spawned").health();
        let far_health_before = sim.get(far).expect("spawned").health();

        sim.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(4, 1, 4), visual_only: false }],
            &mut rng(),
        );
        let mut r = rng();
        // `life == 2` -> after the first tick, `life == 1 >= 0`: `hit_entities`
        // is set on that very first call.
        sim.tick_lightning(Difficulty::Peaceful, &mut r);

        let near_health_after = sim.get(near).expect("still alive").health();
        assert!(
            near_health_after < near_health_before,
            "a mob within DAMAGE_RADIUS of the strike must take damage: before={near_health_before}, after={near_health_after}"
        );
        let far_health_after = sim.get(far).expect("still alive").health();
        assert_eq!(far_health_after, far_health_before, "a mob far outside DAMAGE_RADIUS must be untouched");
    }

    /// The turtle species overrides the default with a lethal hit — a struck
    /// turtle dies outright rather than taking `DEFAULT_DAMAGE`.
    #[test]
    fn a_struck_turtle_dies_outright() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species(ResourceKey::from_str("minecraft:turtle").expect("valid key"), Vec3::new(2.0, 1.0, 2.0));
        sim.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(2, 1, 2), visual_only: false }],
            &mut rng(),
        );
        let mut r = rng();
        sim.tick_lightning(Difficulty::Normal, &mut r);
        assert_eq!(sim.len(), 0, "Turtle.thunderHit's Float.MAX_VALUE hit must kill outright");
    }

    /// On any non-Peaceful difficulty, lightning converts a pig to a
    /// zombified piglin and a villager to a witch. The test exercises the
    /// replacement through the complete tick path.
    #[test]
    fn a_struck_pig_converts_to_a_zombified_piglin() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(ResourceKey::from_str("minecraft:pig").expect("valid key"), Vec3::new(3.0, 1.0, 3.0))
            .id();
        sim.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(3, 1, 3), visual_only: false }],
            &mut rng(),
        );
        let mut r = rng();
        sim.tick_lightning(Difficulty::Normal, &mut r);
        assert!(sim.get(id).is_none(), "the original pig entity id must be gone");
        assert_eq!(sim.len(), 1, "exactly one mob must remain: the converted piglin");
        let converted = sim.iter().next().expect("one mob remains");
        assert_eq!(converted.entity_type().to_string(), "minecraft:zombified_piglin");
    }

    /// The same conversion is gated on difficulty — a Peaceful strike falls
    /// back to the default effect and the pig survives as a pig.
    #[test]
    fn control_a_struck_pig_on_peaceful_does_not_convert() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(ResourceKey::from_str("minecraft:pig").expect("valid key"), Vec3::new(3.0, 1.0, 3.0))
            .id();
        sim.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(3, 1, 3), visual_only: false }],
            &mut rng(),
        );
        let mut r = rng();
        sim.tick_lightning(Difficulty::Peaceful, &mut r);
        let survivor = sim.get(id).expect("must still be the same pig entity");
        assert_eq!(survivor.entity_type().to_string(), "minecraft:pig", "Peaceful must not convert");
    }

    /// A `visual_only` (skeleton-horse-trap) bolt must never hit an entity —
    /// `tick_bolt`'s own `!visual_only` guard, exercised through the sim
    /// rather than only against `BoltTickEffects` directly.
    #[test]
    fn control_a_visual_only_bolt_never_damages_a_nearby_mob() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(ResourceKey::from_str("minecraft:cow").expect("valid key"), Vec3::new(1.0, 1.0, 1.0))
            .id();
        let health_before = sim.get(id).expect("spawned").health();
        sim.spawn_lightning_bolts(
            vec![Strike { pos: BlockPos::new(1, 1, 1), visual_only: true }],
            &mut rng(),
        );
        let mut r = rng();
        for _ in 0..5 {
            sim.tick_lightning(Difficulty::Hard, &mut r);
        }
        let health_after = sim.get(id).expect("still alive").health();
        assert_eq!(health_after, health_before, "a visual-only trap bolt must never hit entities");
    }
}
