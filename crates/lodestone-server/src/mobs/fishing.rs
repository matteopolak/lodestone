//! `MobSim`'s fishing-bobber slice: casting, the bob/bite/hook state machine,
//! fish/junk/treasure loot rolls, and reeling in a real item entity. The server
//! applies fishing-rod rules and the three bundled fishing loot tables, with
//! their measured weights, quality values, and item ids preserved.
//!
//! See `docs/fishing.md` for what reaches the screen, the disclosed gaps and
//! how to change it.

use lodestone_entity::item_entity::ItemLifecycle;
use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::mob_spawn::SpawnRng;

use super::{ChunkWorld, MobSim};

/// Seed for [`MobSim::fishing_rng`] — its own stream, on the same
/// "a fishing roll must not shift which roll a mob spawn/patrol/orb-merge
/// sees" reasoning [`super::orbs::ORB_BEHAVIOR_SEED`]'s own doc gives.
pub(super) const FISHING_ROLL_SEED: u64 = 0x4649_5348_5F52_4F44;

/// `FishingHook.MAX_OUT_OF_WATER_TIME`.
const MAX_OUT_OF_WATER_TIME: i32 = 10;

/// `FishingHook.life >= 1200` — twenty seconds resting on solid ground
/// (never in water) discards the bobber; the fallback despawn since this sim
/// has no per-connection "is the owner still holding a rod and within 1024
/// blocks" state to check every tick (see this module's own `docs/fishing.md`
/// §5 for why that half of `shouldStopFishing` is not ported here).
const HOOK_MAX_GROUND_LIFE: i32 = 1200;

/// `FishingHook.FishHookState` — vanilla's own three-state machine, ported
/// verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FishHookState {
    Flying,
    Bobbing,
    HookedInEntity,
}

/// One live fishing bobber — the fields `FishingHook` itself carries, minus
/// `hookedIn`'s general "any entity" case (see this file's own module doc
/// §5 in `docs/fishing.md`: this sim's bobber cannot snag a floating item or
/// a mob mid-flight, only fish once it is bobbing in open water).
#[derive(Debug, Clone)]
pub(super) struct FishingBobber {
    pub uuid: Uuid,
    /// The casting player's own entity id, supplied by the caller
    /// ([`MobSim::cast_fishing_bobber`]) because this sim tracks player
    /// *positions*, never their entity ids — the same limit
    /// [`super::projectiles`]'s own module doc discloses for melee.
    pub owner: i32,
    pub position: Vec3,
    pub velocity: Vec3,
    pub state: FishHookState,
    /// `FishingHook.life` — ticks spent `onGround()`; the ground-timeout
    /// clock.
    pub life: i32,
    pub out_of_water_time: i32,
    pub nibble: i32,
    pub time_until_lured: i32,
    pub time_until_hooked: i32,
    pub fish_angle: f32,
    pub open_water: bool,
    pub biting: bool,
    pub on_ground: bool,
    /// `FishingHook.luck` — `Math.max(0, luck)` of the rod's own
    /// Luck of the Sea level, clamped at cast time exactly as vanilla's
    /// constructor clamps it.
    pub luck: i32,
    /// `FishingHook.lureSpeed` — `Math.max(0, lureSpeed)`, `20 *`
    /// `getFishingTimeReduction` in whole ticks.
    pub lure_speed: i32,
}

/// The chunk containing a fishing bobber at the start of its tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FishingTickOwner {
    Chunk { cx: i32, cz: i32 },
}

impl FishingTickOwner {
    fn for_position(position: Vec3) -> Self {
        Self::Chunk {
            cx: (position.x.floor() as i32).div_euclid(16),
            cz: (position.z.floor() as i32).div_euclid(16),
        }
    }
}

/// One completed fishing-bobber owner batch.
#[derive(Debug, Clone)]
pub(crate) struct FishingTickOwnerBatch {
    owner: FishingTickOwner,
    expected_batch_count: usize,
    effects: Vec<FishingTickEffect>,
}

#[derive(Debug, Clone)]
struct FishingTickEffect {
    owner: FishingTickOwner,
    serial: usize,
    id: i32,
    bobber: FishingBobber,
    discard: bool,
}

/// One catch's proceeds: what [`MobSim::retrieve_fishing_bobber`] has
/// already turned into real entities, plus the rod-damage tier the
/// off-limits caller (durability lives on the connection's inventory) still
/// needs to apply — `FishingHook.retrieve`'s own return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FishingRetrieve {
    /// `0` (bobber still on ground, no bite), `1` (a loot item was reeled
    /// in), `2` (the bobber was resting on ground when retrieved) or `3`
    /// (a snagged entity was pulled — not reachable by this port, see this
    /// file's own doc).
    pub rod_damage: i32,
}

/// `LootPoolEntryContainer.getWeight(luck)` — `max(0, floor(weight + quality
/// * luck))`. Real per the record: the fixed integer arithmetic (not a
/// float lerp) is what makes a `luck` of exactly `-5` zero out a `quality:
/// -2, weight: 10` entry rather than merely shrinking it.
fn effective_weight(weight: i32, quality: i32, luck: i32) -> i32 {
    (weight + quality * luck).max(0)
}

/// One `minecraft:fishing` pool selection: `junk` (weight 10, quality -2),
/// `treasure` (weight 5, quality 2, **only when `open_water`** — the
/// `entity_properties`/`type_specific/fishing_hook` condition in
/// `gameplay/fishing.json`), `fish` (weight 85, quality -1). `rolls: 1.0`,
/// so exactly one of the three is chosen — no independent per-entry roll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LootCategory {
    Junk,
    Treasure,
    Fish,
}

fn pick_category(open_water: bool, luck: i32, rng: &mut SpawnRng) -> LootCategory {
    let mut candidates = vec![(LootCategory::Junk, effective_weight(10, -2, luck))];
    if open_water {
        candidates.push((LootCategory::Treasure, effective_weight(5, 2, luck)));
    }
    candidates.push((LootCategory::Fish, effective_weight(85, -1, luck)));
    weighted_pick(&candidates, rng)
}

/// `gameplay/fishing/fish.json`, weights as written (no quality at this
/// level — the parent pool already applied it).
const FISH_POOL: &[(&str, i32)] = &[
    ("minecraft:cod", 60),
    ("minecraft:salmon", 25),
    ("minecraft:tropical_fish", 2),
    ("minecraft:pufferfish", 13),
];

/// `gameplay/fishing/junk.json`. Every entry with no explicit `"weight"` key
/// defaults to `1` (vanilla's own `LootPoolSingletonContainer` default) —
/// only `ink_sac` lacks one in the source table, so it is `1` here and not
/// the `10` its `set_count` function actually sets the *stack size* to
/// (those are two different numbers on the same entry; see
/// [`junk_stack_count`]).  `bamboo` carries a real
/// `minecraft:location_check` biome condition, applied in
/// [`pick_junk_item`] rather than folded into this table, because the
/// condition needs `ChunkWorld::biome_at` and this table does not have a
/// world to ask.
const JUNK_POOL: &[(&str, i32)] = &[
    ("minecraft:lily_pad", 17),
    ("minecraft:leather_boots", 10),
    ("minecraft:leather", 10),
    ("minecraft:bone", 10),
    ("minecraft:potion", 10),
    ("minecraft:string", 5),
    ("minecraft:fishing_rod", 2),
    ("minecraft:bowl", 10),
    ("minecraft:stick", 5),
    ("minecraft:ink_sac", 1),
    ("minecraft:tripwire_hook", 10),
    ("minecraft:rotten_flesh", 10),
    ("minecraft:bamboo", 10),
];

/// The one junk entry whose `minecraft:set_count` function overrides the
/// default stack size of `1` — `ink_sac`'s `10.0`. Every other junk/fish/
/// treasure entry drops a single item; `minecraft:set_damage` and
/// `minecraft:enchant_with_levels` (leather boots, fishing rod, bow, book)
/// are not applied — no item-durability-roll or enchantment model exists in
/// this crate (the same disclosed gap `mobs::projectiles`'s own module doc
/// names for Punch/Piercing), so those items are reeled in undamaged and
/// unenchanted rather than not at all.
fn junk_stack_count(item: &str) -> u8 {
    if item == "minecraft:ink_sac" { 10 } else { 1 }
}

/// `gameplay/fishing/treasure.json` — no entry carries a `"weight"` key, so
/// every one defaults to `1`; `enchant_with_levels`/`set_damage` are the
/// same disclosed no-op as [`junk_stack_count`]'s doc says.
const TREASURE_POOL: &[(&str, i32)] = &[
    ("minecraft:name_tag", 1),
    ("minecraft:saddle", 1),
    ("minecraft:bow", 1),
    ("minecraft:fishing_rod", 1),
    ("minecraft:book", 1),
    ("minecraft:nautilus_shell", 1),
];

/// The jungle family `bamboo`'s own `minecraft:location_check` names
/// (`data/minecraft/tags/item/enchantable/fishing.json` is a different
/// file; this is the loot condition's own three biomes, read directly off
/// `gameplay/fishing/junk.json`).
const BAMBOO_BIOMES: &[&str] = &[
    "minecraft:jungle",
    "minecraft:sparse_jungle",
    "minecraft:bamboo_jungle",
];

fn weighted_pick<T: Copy>(candidates: &[(T, i32)], rng: &mut SpawnRng) -> T {
    let total: i32 = candidates.iter().map(|&(_, w)| w).sum();
    if total <= 0 {
        // Every effective weight floored to zero (an extreme negative luck
        // against the treasure/fish entries is not reachable without an
        // enchantment model, but junk alone could in principle zero out) —
        // fall back to the first candidate rather than dividing by zero.
        return candidates[0].0;
    }
    let mut roll = rng.next_int(total);
    for &(value, w) in candidates {
        if roll < w {
            return value;
        }
        roll -= w;
    }
    candidates[candidates.len() - 1].0
}

/// One resolved loot item: its registry id and stack count.
fn roll_loot(open_water: bool, luck: i32, world: &ChunkWorld, pos: Vec3, rng: &mut SpawnRng) -> (ResourceKey, u8) {
    let category = pick_category(open_water, luck, rng);
    let item = match category {
        LootCategory::Fish => weighted_pick(FISH_POOL, rng),
        LootCategory::Treasure => weighted_pick(TREASURE_POOL, rng),
        LootCategory::Junk => {
            // The bamboo entry is excluded from the draw entirely outside a
            // jungle biome — `minecraft:location_check` is a *condition*,
            // not a weight of zero, so vanilla re-rolls among the remaining
            // entries rather than ever landing on "nothing". Filtering the
            // candidate list before the weighted pick reproduces that.
            let biome = world.biome_at(pos.x.floor() as i32, pos.y.floor() as i32, pos.z.floor() as i32);
            let in_jungle = biome.is_some_and(|b| BAMBOO_BIOMES.contains(&b.as_str()));
            let pool: Vec<(&str, i32)> = JUNK_POOL
                .iter()
                .copied()
                .filter(|&(name, _)| name != "minecraft:bamboo" || in_jungle)
                .collect();
            weighted_pick(&pool, rng)
        }
    };
    let count = if category == LootCategory::Junk { junk_stack_count(item) } else { 1 };
    (item.parse().expect("every table entry above is a valid resource key"), count)
}

impl<'w> MobSim<'w> {
    /// Casts a fishing bobber — `FishingHook`'s own player constructor,
    /// `FishingRodItem.use`'s "no active bobber" arm.
    ///
    /// `eye_pos` is the player's `getEyeY()` position (feet + eye height,
    /// resolved by the caller — this sim has no player eye-height constant
    /// of its own, and `PLAYER_EYE_HEIGHT` already lives in `orbs.rs` for a
    /// different reason); `yaw`/`pitch` are degrees, vanilla's own
    /// convention. `luck`/`lure_speed` are the rod's *Luck of the Sea*/
    /// *Lure* enchantment levels, clamped to `>= 0` here exactly as the
    /// vanilla constructor clamps them — `0`/`0` for an unenchanted rod,
    /// which is the only value any call site can supply today (no
    /// enchantment model; see `docs/fishing.md`).
    ///
    /// Returns the assigned entity id.
    pub fn cast_fishing_bobber(
        &mut self,
        owner: i32,
        player_pos: Vec3,
        eye_y: f64,
        yaw: f32,
        pitch: f32,
        luck: i32,
        lure_speed: i32,
    ) -> i32 {
        // `Mth.cos`/`Mth.sin` are a 65,536-entry lookup table, not
        // `f32::cos`/`f32::sin` — see `lodestone_physics::mth`'s own doc. The
        // difference is not cosmetic here: at `pitch == 90.0` (straight down)
        // `x_cos` sits exactly on the table's quantized zero, and vanilla's
        // table gives it a *consistent* sign (`-0.0`, driving `base.y`
        // negative i.e. downward) where the standard library's transcendental
        // `cos` lands on the wrong side of zero by float noise, flipping
        // `base.y`'s clamp to `+5.0` and launching every straight-down cast
        // skyward instead of into the water.
        let y_rot = yaw.to_radians();
        let x_rot = pitch.to_radians();
        let y_cos = lodestone_physics::mth::cos(f64::from(-y_rot - std::f32::consts::PI));
        let y_sin = lodestone_physics::mth::sin(f64::from(-y_rot - std::f32::consts::PI));
        let x_cos = -lodestone_physics::mth::cos(f64::from(-x_rot));
        let x_sin = lodestone_physics::mth::sin(f64::from(-x_rot));
        let spawn = Vec3::new(
            player_pos.x - f64::from(y_sin) * 0.3,
            eye_y,
            player_pos.z - f64::from(y_cos) * 0.3,
        );
        let base = Vec3::new(f64::from(-y_sin), f64::from((-(x_sin / x_cos)).clamp(-5.0, 5.0)), f64::from(-y_cos));
        let dist = (base.x * base.x + base.y * base.y + base.z * base.z).sqrt().max(1e-9);
        // `random.triangle(0.5, 0.0103365)`: mean 0.5, half-width 0.0103365,
        // `mean + width * (nextDouble() - nextDouble())`. Not vanilla's exact
        // stream (this sim's `SpawnRng` never claims to be — see
        // `crate::mob_spawn::SpawnRng`'s own doc), but the same real
        // distribution shape and constants.
        let triangle = |rng: &mut SpawnRng| 0.5 + 0.010_3365 * (rng.next_f64() - rng.next_f64());
        let scale = Vec3::new(
            0.6 / dist + triangle(&mut self.fishing_rng),
            0.6 / dist + triangle(&mut self.fishing_rng),
            0.6 / dist + triangle(&mut self.fishing_rng),
        );
        let velocity = Vec3::new(base.x * scale.x, base.y * scale.y, base.z * scale.z);

        let id = self.next_id;
        self.next_id += 1;
        self.fishing_bobbers.insert(
            id,
            FishingBobber {
                uuid: Uuid::new_v4(),
                owner,
                position: spawn,
                velocity,
                state: FishHookState::Flying,
                life: 0,
                out_of_water_time: 0,
                nibble: 0,
                time_until_lured: 0,
                time_until_hooked: 0,
                fish_angle: 0.0,
                open_water: true,
                biting: false,
                on_ground: false,
                luck: luck.max(0),
                lure_speed: lure_speed.max(0),
            },
        );
        id
    }

    /// Reels in the bobber `id` — `FishingHook.retrieve`. Rolls the loot
    /// table if the fish was hooked (`nibble > 0`), spawns the caught
    /// item(s) as real, flying-toward-the-owner item entities via
    /// [`MobSim::spawn_item`] and an experience orb via
    /// [`MobSim::award_experience`] (both this sim's own producers — the
    /// item-entity lifecycle handoff), and
    /// discards the bobber either way.
    ///
    /// `owner_pos`/`owner_luck` are the reeling player's current position (the
    /// item's pull target, read at retrieve time rather than cast time) and
    /// luck value, added to the rod's own luck; `0` for a caller with no luck
    /// effect modelled yet.
    ///
    /// Returns `None` if `id` is not a tracked bobber.
    pub fn retrieve_fishing_bobber(
        &mut self,
        id: i32,
        owner_pos: Vec3,
        owner_luck: i32,
    ) -> Option<FishingRetrieve> {
        let bobber = self.fishing_bobbers.remove(&id)?;
        // Not reachable by this port (see the struct's own doc): a bobber
        // never enters `HookedInEntity` here, so vanilla's `dmg = 5` (a
        // player) / `3` (an item) branch never fires. Only the loot-roll and
        // on-ground branches are live.
        let rod_damage = if bobber.nibble > 0 {
            let total_luck = bobber.luck + owner_luck;
            let (item, count) = roll_loot(bobber.open_water, total_luck, self.world, bobber.position, &mut self.fishing_rng);
            let delta = Vec3::new(owner_pos.x - bobber.position.x, owner_pos.y - bobber.position.y, owner_pos.z - bobber.position.z);
            let horiz_sq = delta.x * delta.x + delta.y * delta.y + delta.z * delta.z;
            let velocity = Vec3::new(
                delta.x * 0.1,
                delta.y * 0.1 + horiz_sq.sqrt().sqrt() * 0.08,
                delta.z * 0.1,
            );
            self.spawn_item(
                item,
                bobber.position,
                velocity,
                ItemLifecycle::newly_dropped(count, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
            );
            let xp = self.fishing_rng.next_int(6) + 1;
            self.award_experience(owner_pos, Vec3::new(0.0, 0.0, 0.0), xp);
            1
        } else {
            0
        };
        Some(FishingRetrieve {
            rod_damage: if bobber.on_ground { 2 } else { rod_damage },
        })
    }

    /// The bobber id owned by `owner`, if any — what a caller uses to decide
    /// "cast" vs "reel in" on a rod right-click (`player.fishing != null`).
    /// `O(n)` over live bobbers, which is fine: a server has at most one per
    /// connected player.
    #[must_use]
    pub fn player_active_bobber(&self, owner: i32) -> Option<i32> {
        self.fishing_bobbers.iter().find(|(_, b)| b.owner == owner).map(|(&id, _)| id)
    }

    /// Every live bobber's [`crate::protocol::EntitySnapshot`], appended by
    /// [`MobSim::snapshots`]. No metadata: `DATA_HOOKED_ENTITY`/`DATA_BITING`
    /// need new `MetadataField` variants and an encoder arm in
    /// `crates/protocol/v770`, neither of which this file can add — see
    /// `docs/fishing.md` §5. The bite/nibble motion still reaches the wire
    /// through `position`/`velocity` alone, because the dip in
    /// [`tick_fishing_bobbers`](Self::tick_fishing_bobbers) is real physics,
    /// not an animation driven by the missing flag.
    pub(super) fn fishing_bobber_snapshots(&self, out: &mut Vec<crate::protocol::EntitySnapshot>) {
        let mut ids: Vec<i32> = self.fishing_bobbers.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(b) = self.fishing_bobbers.get(&id) else { continue };
            out.push(crate::protocol::EntitySnapshot {
                id,
                uuid: b.uuid,
                entity_type: "minecraft:fishing_bobber".parse().expect("valid key"),
                position: b.position,
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: b.velocity,
                metadata: Vec::new(),
                // `FishingHook.getAddEntityPacket` sends the owner's own
                // entity id as object data (`owner == null ? this.getId() :
                // owner.getId()`) — the field a real client's
                // `FishingHookRenderer` reads to draw the line back to the
                // rod tip.
                object_data: b.owner,
                leash_link: None,
            });
        }
    }

    /// One tick of every live fishing bobber — `FishingHook.tick`, in its
    /// own order, minus the two branches this file's own doc discloses
    /// (`shouldStopFishing`'s distance/held-item check, and hooking a
    /// world entity rather than only bobbing for fish).
    pub(super) fn tick_fishing_bobbers(&mut self) {
        let batches = self.tick_fishing_owner_batches();
        self.apply_fishing_tick_owner_batches(batches);
    }

    /// Produces owner completions from cloned tick-start bobber state.
    ///
    /// The fishing RNG remains serial in entity-id order because bobbing and
    /// bite decisions share one stream. Owners return only changed copies;
    /// the live map is written by the central apply step.
    pub(crate) fn tick_fishing_owner_batches(&mut self) -> Vec<FishingTickOwnerBatch> {
        let world = self.world;
        let mut ids: Vec<i32> = self.fishing_bobbers.keys().copied().collect();
        ids.sort_unstable();
        let mut batches = Vec::<FishingTickOwnerBatch>::new();
        for (serial, id) in ids.into_iter().enumerate() {
            let mut bobber = self
                .fishing_bobbers
                .get(&id)
                .cloned()
                .expect("a tick-start fishing id must remain live while planning");
            let owner = FishingTickOwner::for_position(bobber.position);
            let discard = Self::tick_fishing_bobber(world, &mut self.fishing_rng, &mut bobber);
            let effect = FishingTickEffect {
                owner,
                serial,
                id,
                bobber,
                discard,
            };
            if let Some(batch) = batches.iter_mut().find(|batch| batch.owner == owner) {
                batch.effects.push(effect);
            } else {
                batches.push(FishingTickOwnerBatch {
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

    /// Validates and centrally applies completed fishing-owner batches.
    pub(crate) fn apply_fishing_tick_owner_batches(
        &mut self,
        batches: Vec<FishingTickOwnerBatch>,
    ) {
        if batches.is_empty() {
            assert!(
                self.fishing_bobbers.is_empty(),
                "fishing owner completion must retain every live tick-start bobber"
            );
            return;
        }
        let effects = merge_fishing_tick_owner_batches(batches);
        assert_eq!(
            effects.len(),
            self.fishing_bobbers.len(),
            "fishing owner completion must retain every live tick-start bobber"
        );
        let mut ids = std::collections::HashSet::new();
        for effect in &effects {
            assert!(
                ids.insert(effect.id),
                "fishing owner completion may update one live bobber only once"
            );
            assert!(
                self.fishing_bobbers.contains_key(&effect.id),
                "fishing owner completion may update only a live tick-start bobber"
            );
        }
        for effect in effects {
            if effect.discard {
                self.fishing_bobbers.remove(&effect.id);
            } else {
                self.fishing_bobbers.insert(effect.id, effect.bobber);
            }
        }
    }

    fn tick_fishing_bobber(
        world: &ChunkWorld,
        rng: &mut SpawnRng,
        b: &mut FishingBobber,
    ) -> bool {
            if b.on_ground {
                b.life += 1;
                if b.life >= HOOK_MAX_GROUND_LIFE {
                    return true;
                }
            } else {
                b.life = 0;
            }

            let bx = b.position.x.floor() as i32;
            let by = b.position.y.floor() as i32;
            let bz = b.position.z.floor() as i32;
            let fluid = crate::fluid::fluid_state_of(world.block_state(bx, by, bz));
            let is_water = fluid.is_some_and(|f| f.kind == crate::fluid::FluidKind::Water);
            let liquid_height = if is_water { f64::from(fluid.expect("checked above").own_height()) } else { 0.0 };
            let in_water = liquid_height > 0.0;

            match b.state {
                FishHookState::Flying => {
                    if in_water {
                        b.velocity = Vec3::new(b.velocity.x * 0.3, b.velocity.y * 0.2, b.velocity.z * 0.3);
                        b.state = FishHookState::Bobbing;
                    } else {
                        // No entity-hit search here (see this file's doc) —
                        // only the block/on-ground transition below applies.
                    }
                }
                FishHookState::HookedInEntity => {
                    // Unreachable in this port (never set), kept only so the
                    // match is exhaustive against vanilla's own state machine.
                }
                FishHookState::Bobbing => {
                    let movement = b.velocity;
                    let mut force = b.position.y + movement.y - f64::from(by) - liquid_height;
                    if force.abs() < 0.01 {
                        force += force.signum() * 0.1;
                    }
                    let damp_roll = rng.next_f32();
                    b.velocity = Vec3::new(
                        movement.x * 0.9,
                        movement.y - force * f64::from(damp_roll) * 0.2,
                        movement.z * 0.9,
                    );
                    if b.nibble <= 0 && b.time_until_hooked <= 0 {
                        b.open_water = true;
                    } else {
                        b.open_water = b.open_water
                            && b.out_of_water_time < MAX_OUT_OF_WATER_TIME
                            && calculate_open_water(world, bx, by, bz);
                    }
                    if in_water {
                        b.out_of_water_time = (b.out_of_water_time - 1).max(0);
                        if b.biting {
                            let r1 = f64::from(rng.next_f32());
                            let r2 = f64::from(rng.next_f32());
                            b.velocity = Vec3::new(b.velocity.x, b.velocity.y - 0.1 * r1 * r2, b.velocity.z);
                        }
                        catching_fish(b, rng);
                    } else {
                        b.out_of_water_time = (b.out_of_water_time + 1).min(MAX_OUT_OF_WATER_TIME);
                    }
                }
            }

            if !is_water && !b.on_ground {
                b.velocity = Vec3::new(b.velocity.x, b.velocity.y - 0.03, b.velocity.z);
            }
            let start_y = b.position.y;
            b.position = Vec3::new(b.position.x + b.velocity.x, b.position.y + b.velocity.y, b.position.z + b.velocity.z);
            // Settling: a coarse "am I resting on solid ground" read off the
            // cell(s) crossed between the old and new position, standing in
            // for vanilla's full collision sweep — this sim's item-settling
            // code (`items.rs`) does the real swept version for dropped
            // items; duplicating it here for a bobber that spends its whole
            // interesting life in water was not worth the borrow-splitting
            // cost.
            //
            // The one piece this coarse version cannot skip: a bobber whose
            // per-tick fall exceeds a shallow pond's (or a single-block
            // platform's) depth — a straight-down cast is the case that hits
            // it — used to tunnel straight through the floor into the void
            // below, because a version of this code that only ever *read*
            // the single cell below the already-moved position can miss a
            // thin floor entirely when the fall crosses it in one tick.
            // Scan every integer Y cell the fall passed through and stop at
            // the topmost solid one, exactly as vanilla's real `move()`
            // would have — a one-block floor can no longer be skipped over.
            let below_x = b.position.x.floor() as i32;
            let below_z = b.position.z.floor() as i32;
            let mut landed = false;
            if b.velocity.y <= 0.0 {
                let top = (start_y - 0.01).floor() as i32;
                let bottom = (b.position.y - 0.01).floor() as i32;
                let mut y = top;
                while y >= bottom {
                    if world.is_solid(below_x, y, below_z) {
                        let surface = f64::from(y + 1);
                        if b.position.y < surface {
                            b.position = Vec3::new(b.position.x, surface, b.position.z);
                        }
                        b.velocity = Vec3::new(b.velocity.x, 0.0, b.velocity.z);
                        landed = true;
                        break;
                    }
                    y -= 1;
                }
            }
            b.on_ground = landed;
            if b.state == FishHookState::Flying && b.on_ground {
                b.velocity = Vec3::new(0.0, 0.0, 0.0);
            }
            b.velocity = Vec3::new(b.velocity.x * 0.92, b.velocity.y * 0.92, b.velocity.z * 0.92);
            false
    }
}

fn merge_fishing_tick_owner_batches(
    mut batches: Vec<FishingTickOwnerBatch>,
) -> Vec<FishingTickEffect> {
    let expected_batch_count = batches
        .first()
        .map(|batch| batch.expected_batch_count)
        .expect("fishing owner completion must contain every tick-start owner batch");
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            batch.expected_batch_count, expected_batch_count,
            "fishing owner completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "fishing owner completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "a fishing owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "fishing owner completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "fishing owner completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

/// `FishingHook.calculateOpenWater` — a 5×5 area at four Y layers
/// (`blockPos.offset(-2, y, -2) .. offset(2, y, 2)`, `y` in `-1..=2`), each
/// layer classified `ABOVE_WATER` (air or a lily pad throughout),
/// `INSIDE_WATER` (a water *source* with an empty collision shape
/// throughout) or `INVALID` (anything else, or a mixed layer), and the whole
/// area is "open" only if the layers read as a clean above-water-then-
/// underwater stack with no `INVALID` layer anywhere.
fn calculate_open_water(world: &ChunkWorld, x: i32, y: i32, z: i32) -> bool {
    #[derive(PartialEq, Eq, Clone, Copy)]
    enum Layer {
        Above,
        Inside,
        Invalid,
    }
    let mut previous = Layer::Invalid;
    for dy in -1..=2 {
        let mut layer: Option<Layer> = None;
        'cell: for dx in -2..=2 {
            for dz in -2..=2 {
                let state = world.block_state(x + dx, y + dy, z + dz);
                let cell = if state == "minecraft:air" || state == "minecraft:lily_pad" {
                    Layer::Above
                } else {
                    match crate::fluid::fluid_state_of(state) {
                        Some(f) if f.kind == crate::fluid::FluidKind::Water && f.is_source() => Layer::Inside,
                        _ => Layer::Invalid,
                    }
                };
                match layer {
                    None => layer = Some(cell),
                    Some(prev) if prev == cell => {}
                    Some(_) => {
                        layer = Some(Layer::Invalid);
                        break 'cell;
                    }
                }
            }
        }
        let layer = layer.unwrap_or(Layer::Invalid);
        match layer {
            // `previous == Invalid` is only ever true on the very first
            // (`dy == -1`) iteration — the sentinel start value, never
            // reassigned to `Invalid` by a prior loop turn (an `Invalid`
            // layer returns `false` immediately, below). So this is exactly
            // vanilla's `previousLayer == INVALID` check on the bottom layer.
            Layer::Above if previous == Layer::Invalid => return false,
            Layer::Inside if previous == Layer::Above => return false,
            Layer::Invalid => return false,
            _ => {}
        }
        previous = layer;
    }
    true
}

/// `FishingHook.catchingFish`, minus the rain/sky-visibility `fishingSpeed`
/// modifier (this sim has no weather state or heightmap crossing its own
/// seam — see `docs/fishing.md` §5) and minus the particle bursts (no
/// world-effect channel from inside the per-bobber loop; see
/// [`MobSim::tick_fishing_bobbers`]'s own doc). `fishingSpeed` is therefore
/// always vanilla's own unmodified `1`. Every RNG draw and every
/// duration/threshold below is transcribed as written.
fn catching_fish(b: &mut FishingBobber, rng: &mut SpawnRng) {
    let fishing_speed = 1;
    if b.nibble > 0 {
        b.nibble -= 1;
        if b.nibble <= 0 {
            b.time_until_lured = 0;
            b.time_until_hooked = 0;
            b.biting = false;
        }
    } else if b.time_until_hooked > 0 {
        b.time_until_hooked -= fishing_speed;
        if b.time_until_hooked <= 0 {
            // The bite: `nibble = nextInt(20, 40)` and `biting = true`,
            // which is also where vanilla's synced-data listener applies the
            // downward yank (`-0.4F * nextFloat(0.6, 1.0)`) — folded in here
            // rather than through a metadata round-trip, since this sim has
            // no client to notify and applies its own state directly.
            b.nibble = 20 + rng.next_int(21);
            b.biting = true;
            let dip = 0.4 * f64::from(0.6 + rng.next_f32() * 0.4);
            b.velocity = Vec3::new(b.velocity.x, -dip, b.velocity.z);
        }
    } else if b.time_until_lured > 0 {
        b.time_until_lured -= fishing_speed;
        if b.time_until_lured <= 0 {
            b.fish_angle = rng.next_f32() * 360.0;
            b.time_until_hooked = 20 + rng.next_int(61);
        }
    } else {
        b.time_until_lured = 100 + rng.next_int(501);
        b.time_until_lured -= b.lure_speed;
    }
}

#[cfg(test)]
mod fishing_tests {
    use super::*;
    use crate::mobs::PlayerPerception;

    /// A 5×5 pond, three deep, walled by stone — real open water by
    /// vanilla's own rule (a clean water layer under a clean air layer).
    fn pond_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -4..=4 {
            for z in -4..=4 {
                world.set_block(x, 0, z, "minecraft:stone");
                for y in 1..=3 {
                    world.set_block(x, y, z, "minecraft:water");
                }
            }
        }
        world
    }

    fn owner_fixture() -> MobSim<'static> {
        let world = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        let mut sim = MobSim::new(world);
        for (id, position) in [
            (10, Vec3::new(-0.5, 20.0, 0.5)),
            (11, Vec3::new(16.5, 20.0, 0.5)),
            (12, Vec3::new(-0.25, 21.0, 0.5)),
        ] {
            sim.fishing_bobbers.insert(
                id,
                FishingBobber {
                    uuid: Uuid::from_u128(id as u128),
                    owner: id + 100,
                    position,
                    velocity: Vec3::new(0.0, -0.1, 0.0),
                    state: FishHookState::Flying,
                    life: 0,
                    out_of_water_time: 0,
                    nibble: 0,
                    time_until_lured: 0,
                    time_until_hooked: 0,
                    fish_angle: 0.0,
                    open_water: true,
                    biting: false,
                    on_ground: false,
                    luck: 0,
                    lure_speed: 0,
                },
            );
        }
        sim
    }

    fn bobber_states(sim: &MobSim<'_>) -> Vec<(i32, Vec3, Vec3)> {
        let mut states: Vec<_> = sim
            .fishing_bobbers
            .iter()
            .map(|(&id, bobber)| (id, bobber.position, bobber.velocity))
            .collect();
        states.sort_unstable_by_key(|(id, _, _)| *id);
        states
    }

    #[test]
    fn fishing_owner_batches_restore_entity_order_after_reversed_completion() {
        let mut serial = owner_fixture();
        let serial_batches = serial.tick_fishing_owner_batches();
        assert_eq!(
            serial_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            [
                FishingTickOwner::Chunk { cx: -1, cz: 0 },
                FishingTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "negative fractional positions use Euclidean chunk ownership"
        );
        serial.apply_fishing_tick_owner_batches(serial_batches);
        let expected = bobber_states(&serial);

        let mut completed = owner_fixture();
        let mut batches = completed.tick_fishing_owner_batches();
        batches.reverse();
        let raw_slots = batches
            .iter()
            .flat_map(|batch| batch.effects.iter())
            .map(|effect| effect.serial)
            .collect::<Vec<_>>();
        assert_ne!(raw_slots, vec![0, 1, 2], "control must actually reorder completion slots");
        completed.apply_fishing_tick_owner_batches(batches);
        assert_eq!(bobber_states(&completed), expected);
    }

    #[test]
    #[should_panic(expected = "every tick-start owner batch exactly once")]
    fn fishing_owner_batch_merge_rejects_a_missing_owner() {
        let mut sim = owner_fixture();
        let mut batches = sim.tick_fishing_owner_batches();
        batches.pop();
        let _ = merge_fishing_tick_owner_batches(batches);
    }

    #[test]
    #[should_panic(expected = "may not contain one owner twice")]
    fn fishing_owner_batch_merge_rejects_a_duplicate_owner() {
        let mut sim = owner_fixture();
        let mut batches = sim.tick_fishing_owner_batches();
        batches[1] = batches[0].clone();
        let _ = merge_fishing_tick_owner_batches(batches);
    }

    /// **The three declared weights sum to 100 and split exactly as
    /// `gameplay/fishing.json` writes them, at zero luck.**
    ///
    /// Predicts the value rather than asserting a direction: junk 10%,
    /// treasure 5%, fish 85%, over a large sample, each within a wide but
    /// real tolerance. A category swap or a mistyped weight moves one share
    /// by double digits, which this catches; a ±3-point sampling wobble does
    /// not.
    #[test]
    fn category_split_matches_the_declared_weights_at_zero_luck() {
        let mut rng = SpawnRng::new(1);
        let mut junk = 0;
        let mut treasure = 0;
        let mut fish = 0;
        const N: i32 = 20_000;
        for _ in 0..N {
            match pick_category(true, 0, &mut rng) {
                LootCategory::Junk => junk += 1,
                LootCategory::Treasure => treasure += 1,
                LootCategory::Fish => fish += 1,
            }
        }
        let pct = |n: i32| f64::from(n) / f64::from(N) * 100.0;
        assert!((pct(junk) - 10.0).abs() < 2.0, "junk share {}%, want ~10%", pct(junk));
        assert!((pct(treasure) - 5.0).abs() < 2.0, "treasure share {}%, want ~5%", pct(treasure));
        assert!((pct(fish) - 85.0).abs() < 2.0, "fish share {}%, want ~85%", pct(fish));
    }

    /// **The discriminating input: Luck of the Sea shifts weight toward
    /// treasure, by the real quality-weighted formula, not merely "more
    /// treasure than at luck 0".**
    ///
    /// At `luck = 15`: junk `10 + (-2)*15 = -20` floors to `0` (excluded from
    /// the draw entirely — the formula's own zero-floor, not a special
    /// case), treasure `5 + 2*15 = 35`, fish `85 + (-1)*15 = 70`; total 105,
    /// so treasure's predicted share is `35/105 ≈ 33.3%` — up from 5%, and
    /// junk's predicted share is exactly `0%`. Both are asserted, because a
    /// direction-only check ("more treasure at higher luck") is satisfied by
    /// any monotonic function, not only the real one.
    #[test]
    fn an_enchanted_rods_luck_shifts_weight_toward_treasure_by_the_derived_amount() {
        let mut rng = SpawnRng::new(7);
        let mut treasure = 0;
        let mut junk = 0;
        const N: i32 = 20_000;
        for _ in 0..N {
            match pick_category(true, 15, &mut rng) {
                LootCategory::Treasure => treasure += 1,
                LootCategory::Junk => junk += 1,
                LootCategory::Fish => {}
            }
        }
        let treasure_pct = f64::from(treasure) / f64::from(N) * 100.0;
        assert!(
            (treasure_pct - 33.3).abs() < 2.5,
            "treasure share at luck 15 was {treasure_pct}%, the derived value is ~33.3% (35/105)"
        );
        assert_eq!(junk, 0, "junk's effective weight floors to 0 at luck 15 — it must never be drawn");
    }

    /// **The control: without open water, treasure is not a candidate at
    /// all**, whatever the luck — the `in_open_water` condition gates the
    /// whole entry, not just its weight.
    #[test]
    fn control_treasure_is_unreachable_without_open_water() {
        let mut rng = SpawnRng::new(3);
        for _ in 0..2_000 {
            assert_ne!(pick_category(false, 30, &mut rng), LootCategory::Treasure);
        }
    }

    /// A cast bobber lands in the water and the tick loop carries it through
    /// lure → hook → bite, ending with `nibble > 0` and `biting` true, and
    /// crucially a **visible downward dip** the moment the bite lands — the
    /// signal that reaches the wire with no metadata field at all (see this
    /// file's own `fishing_bobber_snapshots` doc).
    #[test]
    fn a_cast_bobber_eventually_bites_and_dips() {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        let id = sim.cast_fishing_bobber(1, Vec3::new(0.0, 4.0, 0.0), 4.6, 0.0, 90.0, 0, 0);
        // Straight down at pitch 90; give it a few ticks to settle into the
        // pond, then run long enough to cross lure (100..600) + hook
        // (20..80) worst case.
        let mut biting_seen = false;
        let mut dipped = false;
        for _ in 0..900 {
            sim.tick_fishing_bobbers();
            if let Some(b) = sim.fishing_bobbers.get(&id) {
                if b.biting {
                    biting_seen = true;
                    if b.velocity.y < -0.05 {
                        dipped = true;
                    }
                }
            } else {
                break;
            }
        }
        assert!(biting_seen, "a bobber left ticking for 900 ticks in open water must eventually bite");
        assert!(dipped, "the bite must apply a real downward velocity dip, not just flip a flag");
    }

    /// **Reeling in a bite spawns a real item entity flying toward the
    /// owner** — the item-entity lifecycle handoff, asserted against the actual producer rather than
    /// the loot roll alone.
    #[test]
    fn retrieving_a_bite_spawns_a_real_item_entity_and_xp() {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        let id = sim.cast_fishing_bobber(1, Vec3::new(0.0, 4.0, 0.0), 4.6, 0.0, 90.0, 0, 0);
        for _ in 0..900 {
            sim.tick_fishing_bobbers();
            if sim.fishing_bobbers.get(&id).is_some_and(|b| b.nibble > 0) {
                break;
            }
        }
        assert!(sim.fishing_bobbers.get(&id).is_some_and(|b| b.nibble > 0), "must have bitten within 900 ticks");
        let before_items = sim.item_count();
        let owner_pos = Vec3::new(5.0, 4.0, 0.0);
        let retrieve = sim.retrieve_fishing_bobber(id, owner_pos, 0).expect("bobber was tracked");
        assert_eq!(retrieve.rod_damage, 1, "a landed bite reels in exactly one loot item, dmg 1");
        assert_eq!(sim.item_count(), before_items + 1, "one real item entity must be spawned");
        assert!(sim.orb_points_outstanding() > 0, "a catch must award experience");
        assert!(sim.player_active_bobber(1).is_none(), "the bobber is discarded on retrieve");
    }

    /// The control for the item-entity claim: retrieving with **no** bite
    /// (a fresh cast, immediately reeled in) spawns nothing.
    #[test]
    fn control_an_immediate_retrieve_with_no_bite_spawns_nothing() {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        let id = sim.cast_fishing_bobber(1, Vec3::new(0.0, 4.0, 0.0), 4.6, 0.0, 90.0, 0, 0);
        let before_items = sim.item_count();
        let before_xp = sim.orb_points_outstanding();
        let retrieve = sim.retrieve_fishing_bobber(id, Vec3::new(5.0, 4.0, 0.0), 0).expect("tracked");
        assert_eq!(retrieve.rod_damage, 0, "no bite means dmg 0, not a phantom catch");
        assert_eq!(sim.item_count(), before_items, "no item may spawn without a bite");
        assert_eq!(sim.orb_points_outstanding(), before_xp, "no xp may spawn without a bite");
    }

    /// A bobber that lands on dry ground rather than water despawns after
    /// [`HOOK_MAX_GROUND_LIFE`] ticks — `FishingHook.life >= 1200`.
    #[test]
    fn a_grounded_bobber_despawns_after_1200_ticks() {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -2..=2 {
            for z in -2..=2 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        let mut sim = MobSim::new(&world);
        let id = sim.cast_fishing_bobber(1, Vec3::new(0.0, 1.0, 0.0), 1.6, 0.0, 90.0, 0, 0);
        for _ in 0..HOOK_MAX_GROUND_LIFE + 5 {
            sim.tick_fishing_bobbers();
        }
        assert!(sim.fishing_bobbers.get(&id).is_none(), "a bobber resting on ground for 1200+ ticks must discard itself");
    }

    /// `player_active_bobber` finds the caster's own bobber and nobody
    /// else's — the query a rod right-click needs to decide cast vs. reel.
    #[test]
    fn player_active_bobber_is_keyed_by_owner() {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        let mine = sim.cast_fishing_bobber(1, Vec3::new(0.0, 4.0, 0.0), 4.6, 0.0, 90.0, 0, 0);
        let _theirs = sim.cast_fishing_bobber(2, Vec3::new(0.0, 4.0, 0.0), 4.6, 0.0, 90.0, 0, 0);
        assert_eq!(sim.player_active_bobber(1), Some(mine));
        assert_eq!(sim.player_active_bobber(3), None, "a player with no cast bobber finds nothing");
    }

    /// A cast bobber appears in `snapshots()` as `minecraft:fishing_bobber`
    /// carrying the owner's entity id as object data — the field a real
    /// client's line-to-rod-tip renderer reads.
    #[test]
    fn a_cast_bobber_streams_with_the_owners_id_as_object_data() {
        let world = pond_world();
        let mut sim = MobSim::new(&world);
        sim.set_players(vec![PlayerPerception {
            position: Vec3::new(0.0, 4.0, 0.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);
        let owner_entity_id = 42;
        let id = sim.cast_fishing_bobber(owner_entity_id, Vec3::new(0.0, 4.0, 0.0), 4.6, 0.0, 90.0, 0, 0);
        let snap = sim.snapshots().into_iter().find(|s| s.id == id).expect("a live bobber must be streamed");
        assert_eq!(snap.entity_type.to_string(), "minecraft:fishing_bobber");
        assert_eq!(snap.object_data, owner_entity_id);
    }
}
