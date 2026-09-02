//! `MobSim`'s projectile-tracking slice — arrow/throwable spawn, per-tick
//! impact resolution, and the projectile query API. Moved out of
//! `mobs/mod.rs` verbatim as part of the `mobs.rs` file split (see
//! `docs/plans/crate-and-file-splits.md`). Zero visibility churn: every
//! method below was already `pub`, and the one private helper
//! (`resolve_projectile_hit`) is called only from `resolve_projectile_impacts`
//! in this same file.

use lodestone_entity::DamageFlags;
use lodestone_entity::projectile::{Projectile, TrackedProjectile};
use lodestone_model::{BlockPos, ResourceKey, Vec3};
use uuid::Uuid;

use crate::mob_effects;
use crate::redstone_target::HitAxis;

use super::{ChunkWorld, MobSim, ProjectileMeta, ProjectileBlockHit};

/// Vanilla's own ghast `explosionPower` field default (`private int
/// explosionPower = 1`, `DEFAULT_EXPLOSION_POWER`). No producer in this sim
/// overrides it yet — no "Happy Ghast" variant, no `ExplosionPower` NBT
/// round-trip — so every fireball explodes at this one figure.
const GHAST_FIREBALL_EXPLOSION_POWER: f32 = 1.0;

/// One projectile impact [`MobSim::resolve_projectile_impacts`] found, staged
/// before resolution because the search borrows the mob list immutably and
/// applying the damage needs it mutably.
#[derive(Debug, Clone)]
struct ProjectileHit {
    /// The projectile's entity id, removed once resolved.
    projectile: i32,
    /// The mob it struck.
    target: i32,
    /// The projectile's bare registry path, e.g. `arrow`.
    entity_type: String,
    /// Its speed at impact, in blocks per tick — the arrow family's damage is
    /// proportional to it.
    speed: f64,
    /// Where the projectile was when it struck, standing in for the shooter as
    /// the retaliation direction.
    origin: Vec3,
    /// The projectile's own [`ProjectileMeta::owner`] — needed by the wither
    /// skull's `livingOwner.heal(5.0F)`-on-kill clause
    /// (`WitherSkull.onHitEntity`); every other impact ignores it (the
    /// retaliation direction above already covers the general "who did this"
    /// case).
    owner: Option<i32>,
}

/// One splash/lingering potion impact [`MobSim::resolve_projectile_impacts`]
/// found, staged for the same reason [`ProjectileHit`] is: applying the burst
/// mutates `self.mobs` while the search that found it still borrows it.
#[derive(Debug, Clone, Copy)]
struct PotionImpact {
    /// Where the burst is centred — `ThrownSplashPotion.onHitAsPotion`'s own
    /// `hitResult.getLocation()`, approximated here as the exact segment point
    /// the collision sweep found (entity hit) or the coarse block-entry point
    /// (block hit). The projectile's own bounding box (`potionAabb` in
    /// vanilla) is small enough relative to [`mob_effects::SPLASH_RANGE`] that
    /// treating the impact as a point rather than moving that box to it is a
    /// disclosed simplification, not a different rule.
    location: Vec3,
    /// The thrown stack's `minecraft:potion` registry id — see
    /// [`ProjectileMeta::potion`]. `None` means nothing to apply (no resolved
    /// potion contents), the same "component absent" contract that field uses.
    potion: Option<i32>,
    /// `ProjectileUtil.computeMargin(this)` at the moment of impact — the same
    /// value the entity-hit search above already computed for this tracked
    /// projectile, reused rather than recomputed from a `ticks_alive` this
    /// struct does not otherwise need to carry.
    margin: f64,
}

/// The `minecraft:damage_type` a projectile's impact deals, from each
/// projectile's own `DamageSources` call.
///
/// `AbstractArrow.onHitEntity` uses `damageSources().arrow(...)`,
/// `Snowball`/`ThrownEgg` use `thrown(...)`, and `SmallFireball` uses
/// `fireball(...)`. Named as a function rather than folded into
/// [`lodestone_entity::projectile::impact_effect`] because the damage *type* is
/// registry data this crate owns the table for, while that function is
/// version-free.
fn projectile_damage_type(path: &str) -> &'static str {
    match path {
        "arrow" | "spectral_arrow" => "arrow",
        "trident" => "trident",
        "small_fireball" | "fireball" => "fireball",
        // `WitherSkull.onHitEntity`'s own `damageSources().witherSkull(...)`
        // — a real, distinct `minecraft:wither_skull` damage type (confirmed
        // in the generated `lodestone_data::damage_types` table, not assumed).
        "wither_skull" => "wither_skull",
        // `thrown` covers snowball, egg and the potions.
        _ => "thrown",
    }
}

/// The segment parameter at which `from + t * delta` first enters a solid block,
/// or `None` if the whole segment is clear.
///
/// Sampled at quarter-block spacing, the same resolution
/// [`RayView::is_clear`]'s implementation on [`ChunkWorld`] uses and for the same
/// stated reason: a collision cell is a full block, so no cell can hide between
/// two samples. This is deliberately *not* how entity hits are found — see
/// [`MobSim::resolve_projectile_impacts`].
///
/// `t = 0.0` is excluded: a projectile that starts inside a solid block (spawned
/// at an archer's eye inside a low ceiling, say) would otherwise be destroyed on
/// its first tick before travelling at all, which vanilla's `inGround` handling
/// does not do either.
fn first_solid_along(world: &ChunkWorld, from: Vec3, delta: Vec3) -> Option<f64> {
    let dist = delta.length();
    if dist < 1e-9 {
        return None;
    }
    let steps = (dist / 0.25).ceil().max(1.0) as u32;
    for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        let p = from + delta.scale(t);
        if world.is_solid(
            p.x.floor() as i32,
            p.y.floor() as i32,
            p.z.floor() as i32,
        ) {
            return Some(t);
        }
    }
    None
}

/// Where along the segment the ray enters `cell`'s own unit box, which face
/// axis it entered through, and the hit point's fractional position within
/// the cell — vanilla `BlockHitResult.getDirection().getAxis()` plus
/// `Mth.frac(hitLocation.{x,y,z})`, both needed by
/// `crate::redstone_target::redstone_strength` (issue #322) and neither of
/// which [`first_solid_along`]'s coarse quarter-block sampling can answer:
/// that function only asks "is there a solid cell by this point", not "which
/// face did the ray cross to get there".
///
/// The exact per-axis slab test, replicated from
/// [`lodestone_entity::projectile::clip_aabb`] rather than widening that
/// function's signature to report an axis no other caller needs — this is
/// the only call site that cares which face won, because it is the only one
/// asking a block (rather than an entity hitbox) which side was struck.
#[must_use]
fn block_entry(from: Vec3, delta: Vec3, cell: BlockPos) -> Option<(f64, HitAxis, Vec3)> {
    let min = Vec3::new(f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
    let max = Vec3::new(min.x + 1.0, min.y + 1.0, min.z + 1.0);
    let mut enter = 0.0_f64;
    let mut enter_axis = HitAxis::Y;
    let mut exit = 1.0_f64;
    for (axis, o, d, lo, hi) in [
        (HitAxis::X, from.x, delta.x, min.x, max.x),
        (HitAxis::Y, from.y, delta.y, min.y, max.y),
        (HitAxis::Z, from.z, delta.z, min.z, max.z),
    ] {
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let t1 = (lo - o) / d;
        let t2 = (hi - o) / d;
        let (near, far) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        if near > enter {
            enter = near;
            enter_axis = axis;
        }
        exit = exit.min(far);
        if enter > exit {
            return None;
        }
    }
    if enter > 1.0 || exit < 0.0 {
        return None;
    }
    let hit_point = from + delta.scale(enter);
    let frac = Vec3::new(hit_point.x - min.x, hit_point.y - min.y, hit_point.z - min.z);
    Some((enter, enter_axis, frac))
}

/// Squared distance from a point to an axis-aligned box — `AABB.distanceToSqr`
/// specialised to the potion-splash case, where one side of the comparison is
/// always a point (see [`PotionImpact::location`]'s own doc for why a point is
/// an acceptable stand-in for the projectile's own small bounding box). `0.0`
/// when the point is inside the box, matching a direct hit's `dist == 0.0`.
#[must_use]
fn point_to_box_distance_sq(point: Vec3, min: Vec3, max: Vec3) -> f64 {
    let dx = (min.x - point.x).max(0.0).max(point.x - max.x);
    let dy = (min.y - point.y).max(0.0).max(point.y - max.y);
    let dz = (min.z - point.z).max(0.0).max(point.z - max.z);
    dx * dx + dy * dy + dz * dz
}

impl<'w> MobSim<'w> {
    /// Registers a ballistic projectile (arrow, snowball, ender pearl, …) at
    /// its current [`Projectile::position`]/[`Projectile::velocity`] so
    /// [`tick`](Self::tick) advances it every server tick and
    /// [`snapshots`](Self::snapshots) puts it on the wire — the "spawned on
    /// launch" half of issue #211. `entity_type` is the wire identity (e.g.
    /// `minecraft:arrow`); the ballistic family/constants are whatever
    /// `Projectile::arrow`/`::throwable`/`::snowball`/… the caller already
    /// picked.
    ///
    /// Returns the assigned entity id. **Hit detection and impact resolution
    /// now happen** — [`tick`](Self::tick) runs
    /// [`resolve_projectile_impacts`](Self::resolve_projectile_impacts) every
    /// tick, so a projectile spawned here damages what it strikes and is removed
    /// on impact. Use
    /// [`spawn_projectile_from`](Self::spawn_projectile_from) whenever the
    /// launcher is known, or the projectile can hit its own shooter.
    pub fn spawn_projectile(&mut self, entity_type: ResourceKey, projectile: Projectile) -> i32 {
        self.spawn_projectile_from(entity_type, projectile, None)
    }

    /// [`spawn_projectile`](Self::spawn_projectile) with a known launcher, whose
    /// entity id the impact pass excludes from the candidate set.
    ///
    /// `owner` is an *entity id* rather than a position because the exclusion has
    /// to survive the shooter moving: a skeleton that launches an arrow and then
    /// steps forward into its own flight path must still not be hit by it, which a
    /// launch-time position could not express.
    pub fn spawn_projectile_from(
        &mut self,
        entity_type: ResourceKey,
        projectile: Projectile,
        owner: Option<i32>,
    ) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.projectiles.spawn(id, projectile);
        self.projectile_meta.insert(
            id,
            ProjectileMeta {
                uuid: Uuid::new_v4(),
                entity_type,
                owner,
                potion: None,
            },
        );
        id
    }

    /// [`spawn_projectile_from`](Self::spawn_projectile_from) plus the thrown
    /// stack's own `minecraft:potion` registry id, so
    /// [`resolve_projectile_impacts`](Self::resolve_projectile_impacts) knows
    /// which effects a splash/lingering potion applies on impact — see
    /// [`ProjectileMeta::potion`]'s own doc. `potion` is `None` for a water
    /// bottle (no `minecraft:potion_contents` resolved) or any non-potion
    /// throwable, exactly [`spawn_projectile_from`](Self::spawn_projectile_from)'s
    /// own behaviour, so this is purely additive.
    ///
    /// **Not yet called by production code.** The one call site that has the
    /// potion id in hand — `apply_use_item`'s launch arm in `crate::server`,
    /// via `spawn_player_projectile` — is outside this change's file ownership;
    /// see the issue this ships against for the exact hunk it still needs.
    pub fn spawn_potion_projectile_from(
        &mut self,
        entity_type: ResourceKey,
        projectile: Projectile,
        owner: Option<i32>,
        potion: Option<i32>,
    ) -> i32 {
        let id = self.spawn_projectile_from(entity_type, projectile, owner);
        if let Some(meta) = self.projectile_meta.get_mut(&id) {
            meta.potion = potion;
        }
        id
    }

    /// Resolves one tick's worth of projectile impacts, **before**
    /// [`ProjectileRegistry::tick`] moves anything.
    ///
    /// # Why before, and why the segment is the one about to be travelled
    ///
    /// Vanilla's `AbstractArrow.tick` clips `originalPosition ..
    /// originalPosition + movement` and only calls `setPos` if nothing was hit, so
    /// the test is against the step the projectile is *about* to take. Running
    /// this after the registry's `tick` would test the step already taken, which
    /// puts every impact one tick late and — worse — lets a projectile pass
    /// through a wall and resolve on the far side.
    ///
    /// # What it looks at
    ///
    /// For each tracked projectile, the segment from its position along its
    /// velocity, against two candidate sets:
    ///
    /// * **Blocks**, through [`ChunkWorld::is_solid`] sampled along the segment.
    ///   Sampling rather than vanilla's exact voxel traversal, at the same
    ///   quarter-block spacing [`RayView::is_clear`] already uses here and for the
    ///   same reason: a collision cell is a full block, so a quarter-block step
    ///   cannot skip one. Entity hits are *not* sampled — see
    ///   [`clip_aabb`](lodestone_entity::projectile::clip_aabb) for why a hitbox
    ///   narrower than the step needs the exact slab clip.
    /// * **Mobs**, each box inflated by
    ///   [`hitbox_margin`](lodestone_entity::projectile::hitbox_margin), excluding
    ///   the projectile's own [`owner`](ProjectileMeta::owner).
    ///
    /// The nearer of the two wins, by segment parameter — which is why the entity
    /// test has to be exact: an arrow that would strike a mob at `t = 0.4` and a
    /// wall at `t = 0.7` must hit the mob, and a quarter-block-sampled entity test
    /// can easily report the wrong order.
    ///
    /// # Disclosed gaps, each with a reason rather than a shrug
    ///
    /// * **A mob does not yet ignite from standing in a fire or lava block.**
    ///   [`SimMob`] now carries a real `crate::burning::BurnState`
    ///   ([`MobSim::tick_burning`] consumes it every tick, and a small
    ///   fireball's own `5.0`-second ignition raises it — `SimMob::ignite` is
    ///   a separate mechanic, the *creeper fuse*, that happens to share the
    ///   verb), but nothing yet reads what block a mob's feet are standing in
    ///   to raise the counter the way a player's does. So a mob can only
    ///   catch fire from an explicit ignition source today, not by walking
    ///   into flame.
    /// * **Players are not candidates.** This sim knows player *positions*
    ///   ([`PlayerPerception`]) and neither their entity ids nor their
    ///   `PlayerVitals`, which live per-connection. Mob-on-player damage has no
    ///   path anywhere in this workspace yet — melee included — so this is the
    ///   pre-existing seam rather than one introduced here.
    /// * **Piercing, critical arrows and Punch knockback are absent.** All three
    ///   are enchantment- or charge-derived and there is no enchantment model;
    ///   note that a plain arrow's knockback in vanilla is genuinely `0.0`
    ///   (`AbstractArrow.doKnockback` multiplies by an enchantment-derived value
    ///   that is zero without Punch), so an arrow hit *correctly* does not shove.
    ///
    /// Returns the number of projectiles removed by an impact.
    pub fn resolve_projectile_impacts(&mut self) -> usize {
        // Collected first, because resolving a hit needs `&mut self.mobs` while
        // the search reads both the projectile set and the mobs.
        let mut hits: Vec<ProjectileHit> = Vec::new();
        let mut spent: Vec<i32> = Vec::new();
        // Issue #322: every block impact this pass finds, precise face/frac
        // included — `resolve_projectile_impacts`'s own "before `spent` is
        // consumed" ordering doesn't change here, only what is recorded
        // alongside a block hit rather than only its projectile's removal.
        let mut block_hits: Vec<ProjectileBlockHit> = Vec::new();
        // A splash/lingering potion's blast, staged the same way as `hits`/
        // `spent`/`block_hits`: `resolve_potion_splash` mutates `self.mobs`
        // (health, status effects), so it cannot run while this loop still
        // holds an immutable borrow of it.
        let mut potion_impacts: Vec<PotionImpact> = Vec::new();
        // `WitherSkull.onHit`'s unconditional impact explosion — staged for
        // the same reason `potion_impacts` is: `MobSim::explode` needs
        // `&mut self.mobs` while this loop still holds an immutable borrow of
        // both the projectile set and the mob list.
        let mut wither_skull_blasts: Vec<Vec3> = Vec::new();
        // `LargeFireball.onHit`'s identical unconditional impact explosion —
        // a ghast's own fireball, staged the same way and for the same
        // reason. Kept as its own vec rather than merged with
        // `wither_skull_blasts` because the two explode at different powers
        // (`crate::wither::SKULL_EXPLOSION_POWER` vs
        // `GHAST_FIREBALL_EXPLOSION_POWER`).
        let mut fireball_blasts: Vec<Vec3> = Vec::new();
        for tracked in self.projectiles.iter() {
            let from = tracked.projectile.position;
            let delta = tracked.projectile.velocity;
            if delta.length() < 1e-9 {
                continue;
            }
            let meta = self.projectile_meta.get(&tracked.id);
            let owner = meta.and_then(|m| m.owner);
            let margin = lodestone_entity::projectile::hitbox_margin(tracked.ticks_alive);
            // `AbstractThrownPotion`'s family — `ThrownSplashPotion` and
            // `ThrownLingeringPotion` both override `onHit` to run the splash
            // burst; nothing else in this sim's throwable set does.
            let potion_id = meta.and_then(|m| m.potion);
            let is_potion_family = meta
                .map(|m| matches!(m.entity_type.path(), "splash_potion" | "lingering_potion"))
                .unwrap_or(false);

            // Nearest mob along the segment.
            let mut nearest: Option<(f64, i32)> = None;
            for m in &self.mobs {
                if Some(m.id) == owner || m.health <= 0.0 {
                    continue;
                }
                let shape = m.shape();
                let pos = m.position();
                let hw = f64::from(shape.width) / 2.0 + margin;
                let min = Vec3::new(pos.x - hw, pos.y - margin, pos.z - hw);
                let max = Vec3::new(
                    pos.x + hw,
                    pos.y + f64::from(shape.height) + margin,
                    pos.z + hw,
                );
                if let Some(t) = lodestone_entity::projectile::clip_aabb(from, delta, min, max)
                    && nearest.is_none_or(|(best, _)| t < best)
                {
                    nearest = Some((t, m.id));
                }
            }
            // A live wither is a valid target too — `self.withers`, not
            // `self.mobs` (see `TrackedWither`'s own doc), so without this
            // second loop no arrow could ever strike one: the search above
            // simply never considers it. Same id space as `self.mobs` (both
            // draw from `self.next_id`), so a hit resolving to a wither's id
            // cannot collide with a real mob's.
            for (&id, w) in &self.withers {
                if Some(id) == owner {
                    continue;
                }
                let (width, height) = super::wither::HITBOX;
                let hw = f64::from(width) / 2.0 + margin;
                let pos = w.position;
                let min = Vec3::new(pos.x - hw, pos.y - margin, pos.z - hw);
                let max = Vec3::new(pos.x + hw, pos.y + f64::from(height) + margin, pos.z + hw);
                if let Some(t) = lodestone_entity::projectile::clip_aabb(from, delta, min, max)
                    && nearest.is_none_or(|(best, _)| t < best)
                {
                    nearest = Some((t, id));
                }
            }

            let block_t = first_solid_along(self.world, from, delta);
            match (nearest, block_t) {
                (Some((entity_t, target)), block) if block.is_none_or(|b| entity_t <= b) => {
                    let entity_type = meta.map(|m| m.entity_type.path().to_owned());
                    let is_wither_skull = entity_type.as_deref() == Some("wither_skull");
                    let is_ghast_fireball = entity_type.as_deref() == Some("fireball");
                    if is_potion_family {
                        // Vanilla's own search is over the blast AABB, not
                        // just whichever entity the collision sweep names as
                        // the nearest — so the burst is staged here exactly
                        // as it is in the block-hit arm below, from the same
                        // impact point.
                        potion_impacts.push(PotionImpact {
                            location: from + delta.scale(entity_t),
                            potion: potion_id,
                            margin,
                        });
                    }
                    if is_wither_skull {
                        wither_skull_blasts.push(from + delta.scale(entity_t));
                    }
                    if is_ghast_fireball {
                        fireball_blasts.push(from + delta.scale(entity_t));
                    }
                    hits.push(ProjectileHit {
                        projectile: tracked.id,
                        target,
                        entity_type: entity_type.unwrap_or_default(),
                        speed: delta.length(),
                        origin: from,
                        owner,
                    });
                }
                (_, Some(block_t)) => {
                    // Issue #322: record the exact face/frac this block was
                    // struck at — `first_solid_along`'s own `block_t` is only
                    // precise to a quarter block, so the cell it lands in is
                    // trusted (that quarter-block cannot straddle two cells)
                    // but the *face* is recomputed exactly via `block_entry`
                    // against that one cell, the same slab test
                    // `clip_aabb` runs for an entity hitbox.
                    let coarse_hit = from + delta.scale(block_t);
                    if is_potion_family {
                        potion_impacts.push(PotionImpact {
                            location: coarse_hit,
                            potion: potion_id,
                            margin,
                        });
                    }
                    if meta.map(|m| m.entity_type.path()) == Some("wither_skull") {
                        // `WitherSkull.onHit` explodes on **any** surface,
                        // block included — not just an entity hit.
                        wither_skull_blasts.push(coarse_hit);
                    }
                    if meta.map(|m| m.entity_type.path()) == Some("fireball") {
                        // `LargeFireball.onHit` — the identical "any surface"
                        // rule for a ghast's own fireball.
                        fireball_blasts.push(coarse_hit);
                    }
                    let cell = super::lightning::floor_block_pos(coarse_hit);
                    if let Some((_, axis, frac)) = block_entry(from, delta, cell) {
                        let path = meta.map(|m| m.entity_type.path().to_owned()).unwrap_or_default();
                        block_hits.push(ProjectileBlockHit {
                            pos: cell,
                            axis,
                            frac,
                            // `AbstractArrow`'s family — `Trident` extends it
                            // too (`ThrownTrident extends AbstractArrow`),
                            // unlike every other throwable this crate spawns.
                            is_arrow: matches!(path.as_str(), "arrow" | "spectral_arrow" | "trident"),
                        });
                    }
                    spent.push(tracked.id);
                }
                // Nothing on this segment, or a mob further along it than the
                // block that stopped the projectile first.
                _ => {}
            }
        }
        self.pending_projectile_block_hits.extend(block_hits);

        let removed = hits.len() + spent.len();
        for hit in hits {
            self.resolve_projectile_hit(&hit);
            self.remove_projectile(hit.projectile);
        }
        for id in spent {
            self.remove_projectile(id);
        }
        // After the projectiles that struck them are gone (so a splash that
        // also happened to be the nearest-mob hit does not double-resolve
        // against a dangling id) and before the reaper, so a lethal instant
        // splash of harming drops loot on the same tick a melee kill would.
        for impact in &potion_impacts {
            self.resolve_potion_splash(impact);
        }
        // `WitherSkull.onHit`'s unconditional impact blast — after the
        // direct hit's own damage/wither-effect/owner-heal above (matching
        // vanilla's own ordering: `onHitEntity` runs inside `onHit`, before
        // `onHit`'s own `explode` call), and before the reaper so a blast
        // that finishes off an already-hit mob still drops loot through the
        // shared reap below. No source exemption — see `mobs::wither`'s own
        // module doc for why.
        for &centre in &wither_skull_blasts {
            self.explode(centre, crate::wither::SKULL_EXPLOSION_POWER, DamageFlags::default());
        }
        // `LargeFireball.onHit`'s own unconditional blast, same ordering
        // reasoning as the wither skull's just above.
        for &centre in &fireball_blasts {
            self.explode(centre, GHAST_FIREBALL_EXPLOSION_POWER, DamageFlags::default());
        }
        // Through the shared reaper, so an arrow kill drops the same loot a melee
        // kill does — the same argument `attack`'s own killing blow makes.
        self.reap_dead();
        removed
    }

    /// Applies one resolved projectile hit: the damage through the same
    /// [`SimMob::apply_damage`] funnel every other source uses, plus the
    /// retaliation record and the hurt sound.
    fn resolve_projectile_hit(&mut self, hit: &ProjectileHit) {
        let mut effect =
            lodestone_entity::projectile::impact_effect(&hit.entity_type, hit.speed);
        // `Snowball.onHitEntity`'s `entity instanceof Blaze ? 3 : 0` — the one
        // impact rule that depends on the *target's* type, which the version-free
        // table cannot see. Applied here rather than by widening that function's
        // signature, so the general case stays a pure function of the projectile.
        if hit.entity_type == "snowball"
            && self
                .get(hit.target)
                .is_some_and(|m| m.entity_type().path() == "blaze")
        {
            effect.damage = lodestone_entity::projectile::SNOWBALL_BLAZE_DAMAGE;
        }
        if effect.damage <= 0.0 {
            return;
        }
        // A wither lives in `self.withers`, not `self.mobs` (see
        // `TrackedWither`'s own doc), so `self.get_mut` below would find
        // nothing and every arrow, trident and thrown potion would pass
        // straight through one — the identical island `attack_from_player`
        // had for melee before its own wither branch landed. Routed to a
        // dedicated helper rather than folded into the code below: none of
        // the armour/retaliation/wither-effect/owner-heal machinery past
        // this point applies to a `TrackedWither`.
        if self.withers.contains_key(&hit.target) {
            self.resolve_projectile_hit_on_wither(hit, effect.damage);
            return;
        }
        // `minecraft:arrow` is the damage type for an arrow, `minecraft:thrown`
        // for a throwable, `minecraft:fireball` for a small fireball — all three
        // are ordinary reducible types (none carries `bypasses_armor`), so armour
        // reduces a projectile hit exactly as it reduces a melee one.
        let flags = DamageFlags::for_damage_type_name(projectile_damage_type(&hit.entity_type))
            .unwrap_or_default();
        let (applied, target_died) = {
            let Some(target) = self.get_mut(hit.target) else {
                return;
            };
            let applied = target.apply_damage(effect.damage, flags);
            // The retaliation half: a mob shot by an arrow turns on where the
            // shot came from, exactly as `attack` does for a melee hit. The
            // arrow's own last position stands in for the shooter, which is the
            // best identity available here and points the right way along the
            // flight path.
            target.mob.note_hurt(Some(hit.origin));
            // A small fireball's five seconds of fire
            // (`lodestone_entity::projectile::impact_effect`'s `ignite_seconds`,
            // nonzero only for `small_fireball` — a ghast's large fireball and
            // a wither skull deal their own flat damage and never call
            // vanilla's `igniteForSeconds`) — previously computed and read
            // nowhere in the workspace. Applied only on a landed, surviving
            // hit (matching the damage above, which already returned early
            // at `effect.damage <= 0.0`); a dead target has nothing left to
            // burn.
            if effect.ignite_seconds > 0.0 && applied > 0.0 {
                target.ignite_for_seconds(effect.ignite_seconds);
            }
            (applied, target.health() <= 0.0)
        };
        // `WitherSkull.onHitEntity`: a landed hit applies `MobEffects.WITHER`
        // (Normal-difficulty duration — see `mobs::wither`'s own module doc
        // for why this does not thread a real `Difficulty` through yet), and
        // a killing hit heals the shooter. Both gated on `applied > 0.0`,
        // matching vanilla's own `wasHurt` guard.
        if hit.entity_type == "wither_skull" && applied > 0.0 {
            let ticks = crate::wither::wither_effect_ticks(lodestone_model::Difficulty::Normal);
            if ticks > 0 && !target_died {
                if let Some(target) = self.get_mut(hit.target) {
                    target.apply_effect("minecraft:wither", ticks, crate::wither::WITHER_EFFECT_AMPLIFIER);
                }
            }
            if target_died {
                if let Some(owner_id) = hit.owner {
                    // The shooter is a wither, tracked in `self.withers`, not
                    // `self.mobs` — see `mobs::wither`'s own module doc for
                    // why the wither is a plain tracked entity rather than a
                    // `SimMob`. `self.get_mut` cannot reach it.
                    if let Some(owner) = self.withers.get_mut(&owner_id) {
                        owner.health = (owner.health + crate::wither::OWNER_HEAL_ON_KILL).min(owner.max_health);
                    }
                }
            }
        }
        self.note_vocalisation(hit.target, applied);
    }

    /// [`resolve_projectile_hit`](Self::resolve_projectile_hit)'s wither
    /// branch — a `TrackedWither` is not a [`super::SimMob`], so
    /// `self.get_mut` cannot reach it and none of the armour reduction,
    /// retaliation record or `minecraft:wither`-effect-on-hit logic above
    /// applies (a wither has no armour attribute of its own, and is immune
    /// to its own status effect). Routes through
    /// [`MobSim::damage_wither`](super::MobSim::damage_wither) instead,
    /// which already applies the emergence-invulnerability and
    /// powered-armour-while-below-half-health gates.
    ///
    /// `is_arrow_or_wind_charge` mirrors vanilla's own hurt-source
    /// classification for that gate: an arrow (a spectral arrow and a
    /// trident are both part of the same projectile family) or a wind
    /// charge is blocked outright while the wither is powered; every other
    /// projectile — a thrown potion, a snowball, a ghast fireball, another
    /// wither's own skull — lands normally regardless of powered state.
    ///
    /// **Disclosed gap:** a skull that strikes a *different* live wither
    /// (two withers, one arena) does not apply the `minecraft:wither`
    /// status effect or the owner-heal-on-kill bonus the way a hit on an
    /// ordinary mob does — narrow enough (it needs two live withers) that
    /// duplicating that machinery for it was not worth the risk of drifting
    /// from the ordinary-mob path above.
    fn resolve_projectile_hit_on_wither(&mut self, hit: &ProjectileHit, damage: f32) {
        let is_arrow_or_wind_charge = matches!(
            hit.entity_type.as_str(),
            "arrow" | "spectral_arrow" | "trident" | "wind_charge"
        );
        self.damage_wither(hit.target, damage, is_arrow_or_wind_charge, false);
    }

    /// Applies one splash-family potion's blast at its impact point —
    /// `ThrownSplashPotion.onHitAsPotion`'s whole entity loop, run for
    /// **every** splash/lingering impact regardless of whether the collision
    /// sweep named a mob as the target: vanilla's own search is over the blast
    /// AABB, not over whatever stopped the projectile.
    ///
    /// # What this does not do
    ///
    /// A **lingering** potion's real vanilla behaviour is
    /// `ThrownLingeringPotion.onHitAsPotion`: it does not run this loop at all
    /// — it spawns an `AreaEffectCloud` that reapplies these effects every 5
    /// ticks for up to 30 seconds. That entity does not exist in this sim, so a
    /// lingering potion applies its burst exactly **once**, at impact, same as
    /// a splash — closer to the real gameplay point (something happens) than
    /// doing nothing, but not the real mechanic. Tracked as a follow-up.
    fn resolve_potion_splash(&mut self, impact: &PotionImpact) {
        let Some(potion) = impact.potion else {
            // No `minecraft:potion_contents` resolved for the thrown stack —
            // matches vanilla's own no-effects case (`!potion.hasEffects()`),
            // which is silent too. This is also the water-bottle path: a
            // water bottle's `potion` is `Some(water)`, but
            // `potion_splash_effects` itself returns nothing for it (empty
            // built-in list) — checked here as `None` only so an unresolved
            // stack short-circuits before the AABB search runs at all.
            return;
        };

        // First pass: read-only over `self.mobs`, exactly the `hits`/`spent`
        // staging pattern `resolve_projectile_impacts` already uses — applying
        // an effect can remove a mob (a lethal instant splash of harming), so
        // nothing here may mutate `self.mobs` until this loop is done reading it.
        let mut applications: Vec<(i32, Vec<mob_effects::SplashEffect>)> = Vec::new();
        for m in &self.mobs {
            // `LivingEntity.isAffectedByPotions() == !isDeadOrDying()`; this
            // sim has no distinct dying state, so health above zero is the
            // whole guard.
            if m.health() <= 0.0 {
                continue;
            }
            let shape = m.shape();
            let pos = m.position();
            let hw = f64::from(shape.width) / 2.0 + impact.margin;
            let min = Vec3::new(pos.x - hw, pos.y - impact.margin, pos.z - hw);
            let max = Vec3::new(
                pos.x + hw,
                pos.y + f64::from(shape.height) + impact.margin,
                pos.z + hw,
            );
            let dist_sq = point_to_box_distance_sq(impact.location, min, max);
            if dist_sq >= mob_effects::SPLASH_RANGE_SQ {
                continue;
            }
            let scale = mob_effects::splash_scale(dist_sq);
            // `1.0`: this build's `ItemComponents` does not model
            // `minecraft:potion_duration_scale` — see `mob_effects`'s module
            // doc for the disclosed gap.
            let effects = mob_effects::potion_splash_effects(potion, scale, 1.0);
            if !effects.is_empty() {
                applications.push((m.id(), effects));
            }
        }

        for (id, effects) in applications {
            for effect in effects {
                match effect {
                    mob_effects::SplashEffect::Instant { effect_id, amount } => {
                        self.apply_instant_splash_effect(id, &effect_id, amount, impact.location);
                    }
                    mob_effects::SplashEffect::Timed {
                        effect_id,
                        duration,
                        amplifier,
                    } => {
                        if let Some(mob) = self.get_mut(id) {
                            mob.apply_effect(&effect_id, duration, amplifier);
                        }
                    }
                }
            }
        }
    }

    /// `HealOrHarmMobEffect.applyInstantaneousEffect`'s two branches: heal for
    /// `instant_health`, damage through `indirect_magic` (vanilla's
    /// `source != null` branch — the projectile that hit is always known
    /// here) for `instant_damage`. Any other id [`mob_effects`] resolved as
    /// instantaneous would be a bug in that module's own table, so it is
    /// silently skipped here rather than guessed at.
    fn apply_instant_splash_effect(&mut self, target: i32, effect_id: &str, amount: f32, impact_location: Vec3) {
        let path = effect_id.strip_prefix("minecraft:").unwrap_or(effect_id);
        match path {
            "instant_health" => {
                if let Some(mob) = self.get_mut(target) {
                    mob.heal(amount);
                }
            }
            "instant_damage" => {
                if amount <= 0.0 {
                    return;
                }
                // `indirect_magic` bypasses armour, wolf armour and the
                // shield — a splash of harming hurts exactly as hard behind a
                // raised shield as without one.
                let flags = DamageFlags::for_damage_type_name("indirect_magic").unwrap_or_default();
                let applied = {
                    let Some(mob) = self.get_mut(target) else {
                        return;
                    };
                    let applied = mob.apply_damage(amount, flags);
                    // Retaliation, matching `resolve_projectile_hit`'s own
                    // pattern: the impact point stands in for the shooter,
                    // the best identity this sim has for a splash.
                    mob.mob.note_hurt(Some(impact_location));
                    applied
                };
                self.note_vocalisation(target, applied);
            }
            _ => {}
        }
    }

    /// Removes a tracked projectile (impact or manual despawn), returning its
    /// last ballistic state if it was tracked.
    pub fn remove_projectile(&mut self, id: i32) -> Option<TrackedProjectile> {
        self.projectile_meta.remove(&id);
        self.projectiles.remove(id)
    }

    /// The number of tracked projectiles.
    #[must_use]
    pub fn projectile_count(&self) -> usize {
        self.projectiles.len()
    }

    /// The current position of a tracked projectile, if any.
    #[must_use]
    pub fn projectile_position(&self, id: i32) -> Option<Vec3> {
        self.projectiles.get(id).map(|p| p.position)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone_target;

    /// A cell whose unit box is `x: 5..6, y: 1..2, z: 5..6` — every test
    /// below fires at this one cell from a different direction/offset.
    fn target_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-4, 24);
        world.set_solid(5, 1, 5, true);
        world
    }

    /// **The discriminating gate for issue #322.** Two arrows struck at
    /// different points on the *same* face of the *same* block must produce
    /// different [`ProjectileBlockHit::frac`]s and therefore different
    /// [`redstone_target::redstone_strength`] readings — not merely "a hit
    /// registered", which a gate checking only `hits.len() == 1` cannot tell
    /// apart from a wrongly-centred one. Both levels are derived from the
    /// formula, not guessed: a dead-centre top-face hit is `15`
    /// (`a_dead_centre_hit_reads_fifteen_on_every_axis`'s own value), and a
    /// quarter-offset one is exactly `8`
    /// (`a_quarter_offset_hit_derives_to_eight_not_a_round_number`'s own
    /// value) — cross-validated against `redstone_target`'s pre-existing
    /// pinned test rather than invented here.
    #[test]
    fn two_distinct_impact_points_on_a_target_yield_two_distinct_power_levels() {
        let world = target_world();

        // Dead centre of the top face (`frac_x = frac_z = 0.5`), fired
        // straight down from above.
        let mut centred = MobSim::new(&world);
        centred.spawn_projectile(
            "minecraft:arrow".parse().expect("valid key"),
            Projectile::arrow(Vec3::new(5.5, 3.0, 5.5), Vec3::new(0.0, -2.0, 0.0)),
        );
        let removed = centred.resolve_projectile_impacts();
        assert_eq!(removed, 1, "the arrow must be consumed by the block hit");
        let centred_hits = centred.take_projectile_block_hits();
        assert_eq!(centred_hits.len(), 1, "exactly one block hit must be recorded");
        let centred_hit = centred_hits[0];
        assert_eq!(centred_hit.pos, BlockPos::new(5, 1, 5));
        assert_eq!(centred_hit.axis, redstone_target::HitAxis::Y, "struck the top face");
        let centred_strength = redstone_target::redstone_strength(
            centred_hit.axis,
            centred_hit.frac.x,
            centred_hit.frac.y,
            centred_hit.frac.z,
        );
        assert_eq!(centred_strength, 15, "a dead-centre hit must read the formula's own maximum");

        // A quarter of the way off-centre along X (`frac_x = 0.75`), same
        // face, same cell, same axis — only the lateral offset differs.
        let mut offset = MobSim::new(&world);
        offset.spawn_projectile(
            "minecraft:arrow".parse().expect("valid key"),
            Projectile::arrow(Vec3::new(5.75, 3.0, 5.5), Vec3::new(0.0, -2.0, 0.0)),
        );
        let removed = offset.resolve_projectile_impacts();
        assert_eq!(removed, 1);
        let offset_hits = offset.take_projectile_block_hits();
        assert_eq!(offset_hits.len(), 1);
        let offset_hit = offset_hits[0];
        assert_eq!(offset_hit.pos, BlockPos::new(5, 1, 5));
        assert_eq!(offset_hit.axis, redstone_target::HitAxis::Y);
        let offset_strength = redstone_target::redstone_strength(
            offset_hit.axis,
            offset_hit.frac.x,
            offset_hit.frac.y,
            offset_hit.frac.z,
        );
        assert_eq!(offset_strength, 8, "a quarter-offset hit must derive to 8, not 15 and not 0");

        assert_ne!(
            centred_strength, offset_strength,
            "two distinct impact points on the same block must yield two distinct power levels"
        );
    }

    /// The struck face's own axis is excluded from the fraction that decides
    /// the reading — a hit on the **west/east** face (`X` axis) must read the
    /// `Y`/`Z` fractions, not `X`'s. Distinguishes a producer that always
    /// reports the *travel* axis from one that reports the *face* axis; here
    /// they coincide (the arrow travels along X and strikes the X face), so
    /// this is the companion `Z`-face gate: an arrow travelling along `+Z`
    /// must report [`redstone_target::HitAxis::Z`], not `Y` or `X`.
    #[test]
    fn a_side_face_hit_reports_its_own_axis() {
        let world = target_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_projectile(
            "minecraft:arrow".parse().expect("valid key"),
            Projectile::arrow(Vec3::new(5.5, 1.5, 4.0), Vec3::new(0.0, 0.0, 2.0)),
        );
        sim.resolve_projectile_impacts();
        let hits = sim.take_projectile_block_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].axis, redstone_target::HitAxis::Z, "travelled and struck along Z");
        assert!(hits[0].is_arrow, "minecraft:arrow is AbstractArrow's own family");
    }

    /// A thrown (non-arrow) projectile that stops against a block is still
    /// recorded, but [`ProjectileBlockHit::is_arrow`] must be `false` — the
    /// bit `redstone_target::activation_duration` uses to pick 8 vs 20 ticks.
    #[test]
    fn a_non_arrow_block_hit_is_not_flagged_as_an_arrow() {
        let world = target_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_projectile(
            "minecraft:snowball".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(5.5, 1.5, 4.0), Vec3::new(0.0, 0.0, 2.0)),
        );
        sim.resolve_projectile_impacts();
        let hits = sim.take_projectile_block_hits();
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].is_arrow, "a snowball is not AbstractArrow's family");
    }

    /// A thrown splash potion, lingering potion, ender pearl and experience
    /// bottle each stop at a wall — the report this guards is "a thrown
    /// potion phases through every block".
    ///
    /// Each case fires diagonally, fast enough (4-7.5 blocks/tick — well
    /// past `POTION_SHOOT_POWER`'s real `0.5`, chosen specifically to
    /// discriminate the sweep from a point check, not to model a real
    /// throw) that the segment's own *endpoint* has already passed clean
    /// through the far side of the target block. A collision test that only
    /// asks "is the destination point solid" would report **no hit** on
    /// every one of these four cases — asserted explicitly below as the
    /// wrong hypothesis this gate must fail against — while the real
    /// [`first_solid_along`]/[`block_entry`] sweep finds the exact entry
    /// point along the segment. Positions, entry axis and entry fraction are
    /// all derived analytically from the segment/box slab test (the same
    /// maths [`block_entry`] implements), not guessed.
    #[test]
    fn thrown_potions_and_their_throwable_siblings_stop_at_a_diagonal_wall_not_just_their_endpoint() {
        struct Case {
            entity_type: &'static str,
            from: Vec3,
            delta: Vec3,
            cell: BlockPos,
            axis: redstone_target::HitAxis,
            frac: Vec3,
        }
        let cases = [
            Case {
                entity_type: "minecraft:splash_potion",
                from: Vec3::new(0.0, 1.5, 0.0),
                delta: Vec3::new(4.0, 0.0, 3.0),
                cell: BlockPos::new(2, 1, 1),
                axis: redstone_target::HitAxis::X,
                frac: Vec3::new(0.0, 0.5, 0.5),
            },
            Case {
                entity_type: "minecraft:lingering_potion",
                from: Vec3::new(0.0, 1.5, 0.0),
                delta: Vec3::new(3.0, 0.0, 5.0),
                cell: BlockPos::new(1, 1, 2),
                axis: redstone_target::HitAxis::Z,
                frac: Vec3::new(0.2, 0.5, 0.0),
            },
            Case {
                entity_type: "minecraft:ender_pearl",
                from: Vec3::new(0.0, 1.5, 0.0),
                delta: Vec3::new(6.0, 0.0, 4.0),
                cell: BlockPos::new(4, 1, 3),
                axis: redstone_target::HitAxis::Z,
                frac: Vec3::new(0.5, 0.5, 0.0),
            },
            Case {
                entity_type: "minecraft:experience_bottle",
                from: Vec3::new(0.0, 1.5, 0.0),
                delta: Vec3::new(5.0, 0.0, 4.5),
                cell: BlockPos::new(3, 1, 3),
                axis: redstone_target::HitAxis::Z,
                frac: Vec3::new(1.0 / 3.0, 0.5, 0.0),
            },
        ];

        let mut mismatches: Vec<String> = Vec::new();
        for case in &cases {
            // The wrong hypothesis, checked first: a point test at the
            // segment's endpoint must NOT already see the wall — otherwise
            // this case fails to discriminate sweep-vs-point at all.
            let end = case.from + case.delta;
            let end_cell = BlockPos::new(
                end.x.floor() as i32,
                end.y.floor() as i32,
                end.z.floor() as i32,
            );
            if end_cell == case.cell {
                mismatches.push(format!(
                    "{}: fixture bug — endpoint {:?} already lands in the target cell, \
                     so this case cannot discriminate a swept test from a naive one",
                    case.entity_type, end
                ));
                continue;
            }

            let mut world = ChunkWorld::new(-4, 24);
            world.set_solid(case.cell.x, case.cell.y, case.cell.z, true);
            let mut sim = MobSim::new(&world);
            sim.spawn_projectile(
                case.entity_type.parse().expect("valid key"),
                Projectile::throwable(case.from, case.delta),
            );
            let removed = sim.resolve_projectile_impacts();
            if removed != 1 {
                mismatches.push(format!(
                    "{}: expected exactly 1 removal (the block impact), got {removed}",
                    case.entity_type
                ));
                continue;
            }
            let hits = sim.take_projectile_block_hits();
            if hits.len() != 1 {
                mismatches.push(format!(
                    "{}: expected exactly 1 recorded block hit, got {}",
                    case.entity_type,
                    hits.len()
                ));
                continue;
            }
            let hit = hits[0];
            if hit.pos != case.cell {
                mismatches.push(format!(
                    "{}: struck cell {:?}, predicted {:?}",
                    case.entity_type, hit.pos, case.cell
                ));
            }
            if hit.axis != case.axis {
                mismatches.push(format!(
                    "{}: entry axis {:?}, predicted {:?}",
                    case.entity_type, hit.axis, case.axis
                ));
            }
            let frac_err = (hit.frac.x - case.frac.x).abs()
                + (hit.frac.y - case.frac.y).abs()
                + (hit.frac.z - case.frac.z).abs();
            if frac_err > 1e-6 {
                mismatches.push(format!(
                    "{}: entry frac {:?}, predicted {:?} (err {frac_err})",
                    case.entity_type, hit.frac, case.frac
                ));
            }
            if hit.is_arrow {
                mismatches.push(format!(
                    "{}: flagged as an arrow-family hit, which none of these are",
                    case.entity_type
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "thrown-projectile block collision mismatches:\n{}",
            mismatches.join("\n")
        );
    }

    /// The negative control for the test above: the identical diagonal
    /// splash-potion throw down a corridor with **no** solid block anywhere
    /// on the segment must not stop early and must not be removed. Without
    /// this, a detector that always reports a hit (or always fails open)
    /// would pass the wall test above vacuously.
    #[test]
    fn a_splash_potion_thrown_down_an_open_corridor_is_not_stopped() {
        let world = ChunkWorld::new(-4, 24); // no solid cells set anywhere
        let mut sim = MobSim::new(&world);
        sim.spawn_projectile(
            "minecraft:splash_potion".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.0, 1.5, 0.0), Vec3::new(4.0, 0.0, 3.0)),
        );
        let removed = sim.resolve_projectile_impacts();
        assert_eq!(removed, 0, "an open corridor must not stop the potion");
        assert!(
            sim.take_projectile_block_hits().is_empty(),
            "an open corridor must record no block hit"
        );
        assert_eq!(
            sim.projectile_count(),
            1,
            "the potion must still be tracked — the control that proves the detector \
             would have fired had a wall actually been there"
        );
    }

    // ---- splash-potion impact effects (the report's real gap: collision is
    // fine, nothing applies on impact) ----

    /// A generic mob at `pos`, health/armour from `combat_defaults`'s zombie
    /// default (the generic [`MobSim::spawn`]'s own placeholder species) — the
    /// exact numbers are read back from the mob rather than assumed, so these
    /// tests never predict a "plausible round number" for health.
    fn spawn_target<'w>(sim: &mut MobSim<'w>, pos: Vec3) -> i32 {
        sim.spawn(pos, lodestone_entity::pathfinding::MobShape::land(0.6, 1.95), 0.2, 32)
            .id()
    }

    /// **The discriminating gate for the real bug this issue reports.** A
    /// splash potion of swiftness lands on two mobs at two different
    /// distances from its impact — a **direct** entity hit (`scale == 1.0`)
    /// and a block hit whose blast reaches a second mob at the edge of its
    /// range (`distance_sq == 10.24`, `scale == 0.2`) — and the applied
    /// duration must differ between them. The wrong hypothesis ("no falloff",
    /// i.e. applying the base 3600-tick duration unconditionally) is checked
    /// explicitly and must NOT match either case.
    ///
    /// Expected durations come from
    /// `lodestone_data::potion::potion_effect_entries` (swiftness's own base
    /// duration, an independently-tested source) and the falloff arithmetic
    /// transcribed directly here — not by calling this crate's own
    /// `mob_effects::splash_timed_duration`.
    #[test]
    fn a_splash_potion_of_swiftness_scales_the_applied_duration_by_distance() {
        let swiftness = lodestone_data::potion::potion_id("minecraft:swiftness").expect("swiftness exists");
        let entries = lodestone_data::potion::potion_effect_entries(swiftness);
        assert_eq!(entries.len(), 1, "swiftness carries exactly one built-in effect");
        let base_duration = f64::from(entries[0].duration_ticks);
        assert_eq!(base_duration, 3600.0, "swiftness's own base duration");

        let mut mismatches: Vec<String> = Vec::new();

        // Case 1: a direct entity hit. No wall in the way; the throw sweeps
        // straight through the mob's own hitbox, so the collision sweep
        // resolves this as an entity impact and the burst's own distance is
        // zero (the impact point is inside the target's box).
        {
            let world = ChunkWorld::new(-4, 24);
            let mut sim = MobSim::new(&world);
            let target = spawn_target(&mut sim, Vec3::new(5.0, 1.0, 1.5));
            sim.spawn_potion_projectile_from(
                "minecraft:splash_potion".parse().expect("valid key"),
                Projectile::throwable(Vec3::new(0.5, 1.5, 1.5), Vec3::new(8.0, 0.0, 0.0)),
                None,
                Some(swiftness),
            );
            sim.resolve_projectile_impacts();
            let duration = sim.get(target).and_then(|m| m.effects().get("minecraft:speed")).map(|e| e.duration());
            // scale(0.0) = 1.0 -> floor(1.0 * 3600 + 0.5) = 3600.
            if duration != Some(3600) {
                mismatches.push(format!("direct hit: expected duration Some(3600), got {duration:?}"));
            }
        }

        // Case 2: a block hit, with a second mob placed off the throw's own
        // line (so the sweep cannot clip it directly) at the edge of the
        // blast — `distance_sq = 3.2^2 = 10.24`, inside `SPLASH_RANGE_SQ`
        // (16.0) but far enough that "no falloff" and the real formula give
        // different integers, not just different floats that round the same.
        {
            let mut world = ChunkWorld::new(-4, 24);
            world.set_solid(5, 1, 1, true);
            let mut sim = MobSim::new(&world);
            let target = spawn_target(&mut sim, Vec3::new(5.0, 1.0, 5.0));
            sim.spawn_potion_projectile_from(
                "minecraft:splash_potion".parse().expect("valid key"),
                Projectile::throwable(Vec3::new(0.5, 1.5, 1.5), Vec3::new(8.0, 0.0, 0.0)),
                None,
                Some(swiftness),
            );
            let removed = sim.resolve_projectile_impacts();
            if removed != 1 {
                mismatches.push(format!("block hit: expected the potion to be consumed by the wall, removed={removed}"));
            }
            let duration = sim.get(target).and_then(|m| m.effects().get("minecraft:speed")).map(|e| e.duration());
            // scale(10.24) = 1.0 - 3.2/4.0 = 0.2 -> floor(0.2 * 3600 + 0.5) = 720.
            if duration != Some(720) {
                mismatches.push(format!("blast edge: expected duration Some(720), got {duration:?}"));
            }
            if duration == Some(3600) {
                mismatches.push("blast edge: got the DIRECT-HIT duration — falloff was not applied at all".to_owned());
            }
        }

        assert!(mismatches.is_empty(), "splash duration falloff mismatches:\n{}", mismatches.join("\n"));
    }

    /// The instant-vs-timed split: a splash potion of harming (`instant_damage`)
    /// must scale the *damage*, not a duration, and must scale it by the same
    /// falloff — the discriminating case the timed-effect gate above cannot
    /// cover on its own.
    #[test]
    fn a_splash_potion_of_harming_scales_instant_damage_by_distance() {
        let harming = lodestone_data::potion::potion_id("minecraft:harming").expect("harming exists");
        let mut mismatches: Vec<String> = Vec::new();

        // Direct hit: scale 1.0 -> floor(1.0 * 6 + 0.5) = 6.
        {
            let world = ChunkWorld::new(-4, 24);
            let mut sim = MobSim::new(&world);
            let target = spawn_target(&mut sim, Vec3::new(5.0, 1.0, 1.5));
            let before = sim.get(target).expect("just spawned").health();
            sim.spawn_potion_projectile_from(
                "minecraft:splash_potion".parse().expect("valid key"),
                Projectile::throwable(Vec3::new(0.5, 1.5, 1.5), Vec3::new(8.0, 0.0, 0.0)),
                None,
                Some(harming),
            );
            sim.resolve_projectile_impacts();
            let after = sim.get(target).expect("still alive at 6 damage").health();
            let dealt = before - after;
            if (dealt - 6.0).abs() > 1e-4 {
                mismatches.push(format!("direct hit: expected 6.0 damage, dealt {dealt}"));
            }
        }

        // Blast edge: scale 0.2 -> floor(0.2 * 6 + 0.5) = 1.
        {
            let mut world = ChunkWorld::new(-4, 24);
            world.set_solid(5, 1, 1, true);
            let mut sim = MobSim::new(&world);
            let target = spawn_target(&mut sim, Vec3::new(5.0, 1.0, 5.0));
            let before = sim.get(target).expect("just spawned").health();
            sim.spawn_potion_projectile_from(
                "minecraft:splash_potion".parse().expect("valid key"),
                Projectile::throwable(Vec3::new(0.5, 1.5, 1.5), Vec3::new(8.0, 0.0, 0.0)),
                None,
                Some(harming),
            );
            sim.resolve_projectile_impacts();
            let after = sim.get(target).expect("still alive at 1 damage").health();
            let dealt = before - after;
            if (dealt - 1.0).abs() > 1e-4 {
                mismatches.push(format!("blast edge: expected 1.0 damage, dealt {dealt}"));
            }
            if (dealt - 6.0).abs() < 1e-4 {
                mismatches.push("blast edge: dealt the DIRECT-HIT amount — falloff was not applied at all".to_owned());
            }
        }

        assert!(mismatches.is_empty(), "splash instant-damage falloff mismatches:\n{}", mismatches.join("\n"));
    }

    /// **Control**: a thrown water bottle (no `minecraft:potion_contents`
    /// effects — `potion_id` resolves, but its built-in effect list is empty)
    /// must apply nothing at all, even on a direct hit well inside the blast.
    /// Without this, a gate that only checks "some effect landed" cannot tell
    /// a real splash from one that always applies something.
    #[test]
    fn a_thrown_water_bottle_applies_no_effects_the_control() {
        let water = lodestone_data::potion::potion_id("minecraft:water").expect("water exists");
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let target = spawn_target(&mut sim, Vec3::new(5.0, 1.0, 1.5));
        let before = sim.get(target).expect("just spawned").health();
        sim.spawn_potion_projectile_from(
            "minecraft:splash_potion".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.5, 1.5, 1.5), Vec3::new(8.0, 0.0, 0.0)),
            None,
            Some(water),
        );
        sim.resolve_projectile_impacts();
        let mob = sim.get(target).expect("water does not kill anything");
        assert_eq!(mob.health(), before, "a water bottle must not change health");
        assert!(mob.effects().is_empty(), "a water bottle must not apply any status effect");
    }

    /// **Control**: the pre-existing `spawn_projectile`/`spawn_projectile_from`
    /// API (used throughout this file's own collision tests, and by every
    /// production call site until the server-side launch seam is wired to
    /// [`MobSim::spawn_potion_projectile_from`]) leaves `ProjectileMeta::potion`
    /// as `None` — a splash potion launched through it must apply no effects,
    /// exactly as an unresolved `minecraft:potion_contents` does. Without this,
    /// a regression that ignored `ProjectileMeta::potion` entirely (applying
    /// some hardcoded effect to every splash regardless of identity) would
    /// still pass every gate above.
    #[test]
    fn a_splash_potion_with_no_resolved_potion_id_applies_nothing() {
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let target = spawn_target(&mut sim, Vec3::new(5.0, 1.0, 1.5));
        let before = sim.get(target).expect("just spawned").health();
        sim.spawn_projectile(
            "minecraft:splash_potion".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.5, 1.5, 1.5), Vec3::new(8.0, 0.0, 0.0)),
        );
        sim.resolve_projectile_impacts();
        let mob = sim.get(target).expect("still alive");
        assert_eq!(mob.health(), before);
        assert!(mob.effects().is_empty());
    }

    /// A ghast's fireball (`minecraft:fireball`, `LargeFireball`) deals its
    /// own flat `6.0` on a direct entity hit — `LargeFireball.onHitEntity`'s
    /// `hurtServer(..., 6.0F)`, not speed-scaled the way an arrow's damage is.
    /// Same structure as `mobs::wither`'s equivalent skull test: two speeds
    /// that both cross the gap within one tick, checked for equal damage
    /// rather than a proportional one.
    #[test]
    fn a_ghast_fireball_deals_flat_not_speed_scaled_damage_on_a_direct_hit() {
        let world = ChunkWorld::new(-4, 24);
        let mut mismatches: Vec<String> = Vec::new();
        let mut dealt_by_speed: Vec<f32> = Vec::new();
        for speed in [5.0, 20.0] {
            let mut sim = MobSim::new(&world);
            let target = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 0.0));
            let before = sim.get(target).expect("just spawned").health();
            sim.spawn_projectile(
                "minecraft:fireball".parse().expect("valid key"),
                Projectile::throwable(Vec3::new(0.0, 1.0, 0.0), Vec3::new(speed, 0.0, 0.0)),
            );
            sim.resolve_projectile_impacts();
            let Some(after_mob) = sim.get(target) else {
                mismatches.push(format!("speed {speed}: target did not survive a single fireball hit"));
                continue;
            };
            let dealt = before - after_mob.health();
            if dealt <= 0.0 {
                mismatches.push(format!("speed {speed}: expected nonzero damage, dealt {dealt}"));
            }
            dealt_by_speed.push(dealt);
        }
        if dealt_by_speed.len() == 2 && (dealt_by_speed[0] - dealt_by_speed[1]).abs() > 1e-4 {
            mismatches.push(format!(
                "damage must not scale with speed (flat 6.0 base plus the unconditional blast): got {dealt_by_speed:?} at 5 vs 20 blocks/tick"
            ));
        }
        assert!(mismatches.is_empty(), "ghast fireball impact mismatches:\n{}", mismatches.join("\n"));
    }

    /// **Production-path proof that a fireball's `ignite_seconds` reaches the
    /// target.** Before this, `impact_effect` computed `5.0` for a *small*
    /// fireball (a blaze's shot — `SmallFireball.onHitEntity`'s own
    /// `igniteForSeconds(5.0F)`) and nothing in the workspace read it. Drives
    /// the real [`MobSim::resolve_projectile_impacts`] entry point — the
    /// same one every other test in this module uses as its production seam
    /// — rather than calling `resolve_projectile_hit` in isolation.
    ///
    /// **Not** `minecraft:fireball` (a ghast's *large* fireball): reading
    /// `LargeFireball.onHitEntity` shows it deals its flat `6.0` and calls no
    /// `igniteForSeconds` at all — only the small variant ignites. The task
    /// this fix came from named "small_fireball/fireball/wither_skull" as all
    /// three igniting; the jar disagrees with that premise, and the fix
    /// (`impact_effect`'s existing `ignite_seconds: 0.0` for both) was
    /// already correct. See [`a_large_fireball_does_not_ignite`] for the
    /// control proving that distinction.
    #[test]
    fn a_small_fireball_impact_ignites_its_target() {
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let target = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 0.0));
        assert!(!sim.get(target).expect("just spawned").is_on_fire());
        sim.spawn_projectile(
            "minecraft:small_fireball".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.0, 1.0, 0.0), Vec3::new(20.0, 0.0, 0.0)),
        );
        sim.resolve_projectile_impacts();
        assert!(
            sim.get(target).expect("survives a single small fireball hit").is_on_fire(),
            "a small fireball hit must ignite its target through the real production entry point"
        );
    }

    /// **Control 1**: an arrow's `impact_effect` carries no `ignite_seconds`
    /// at all, so a mob it strikes must take damage without igniting.
    #[test]
    fn an_arrow_hit_does_not_ignite_its_target() {
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let target = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 0.0));
        let before = sim.get(target).expect("just spawned").health();
        sim.spawn_projectile(
            // A modest, realistic speed — `arrow_impact_damage` scales with
            // it (unlike a fireball's flat damage), and the fireball tests'
            // own `20.0` would one-shot this 20-health target here.
            "minecraft:arrow".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.0, 1.0, 0.0), Vec3::new(4.0, 0.0, 0.0)),
        );
        sim.resolve_projectile_impacts();
        let mob = sim.get(target).expect("survives a single arrow hit");
        assert!(mob.health() < before, "control: the arrow must still deal damage");
        assert!(!mob.is_on_fire(), "an arrow must not ignite its target");
    }

    /// **Control 2**, the discriminating one: a *large* fireball (a ghast's
    /// shot, `minecraft:fireball`) deals real damage but — per
    /// `LargeFireball.onHitEntity`, which never calls `igniteForSeconds` —
    /// must not ignite. Without this control, `a_small_fireball_impact_ignites_its_target`
    /// could pass by igniting on every projectile, fireball-shaped or not.
    #[test]
    fn a_large_fireball_does_not_ignite() {
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let target = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 0.0));
        let before = sim.get(target).expect("just spawned").health();
        sim.spawn_projectile(
            "minecraft:fireball".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.0, 1.0, 0.0), Vec3::new(20.0, 0.0, 0.0)),
        );
        sim.resolve_projectile_impacts();
        let mob = sim.get(target).expect("survives a single large fireball hit");
        assert!(mob.health() < before, "control: the large fireball must still deal damage");
        assert!(!mob.is_on_fire(), "a large (ghast) fireball must not ignite its target");
    }

    /// **The burn actually runs its course**, driven through the same
    /// production tick loop: an ignition of `N` seconds is `20*N` ticks
    /// (`crate::burning::ignite_ticks_for_seconds`), each `tick_burning` call
    /// consuming exactly one — so the mob must still be on fire after `20*N -
    /// 1` ticks and burnt out at exactly `20*N`, the same edge
    /// `crate::burning::BurnState`'s own unit tests pin. Health is asserted
    /// only qualitatively (strictly lower once burnt out) rather than to an
    /// exact figure: the burn's first tick lands the same game tick as
    /// whatever ignited it, which can interact with the hurt-cooldown i-frame
    /// window that `MobSim::apply_damage` already applies to every damage
    /// source — a separately covered mechanic this test does not re-derive.
    /// [`SimMob::ignite_for_seconds`] is the same call
    /// `resolve_projectile_hit` makes, exercised here directly so the tick
    /// count is exact rather than dependent on projectile travel time.
    #[test]
    fn an_ignited_mob_burns_for_exactly_its_duration_and_loses_health() {
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let target = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 0.0));
        let before = sim.get_mut(target).expect("just spawned").health();
        sim.get_mut(target).expect("just spawned").ignite_for_seconds(5.0);

        let total_ticks = crate::burning::ignite_ticks_for_seconds(5.0);
        assert_eq!(total_ticks, 100);
        for _ in 0..total_ticks - 1 {
            sim.tick();
        }
        assert!(
            sim.get(target).expect("a 20-health mob survives five seconds of burning").is_on_fire(),
            "must still be burning one tick before the duration elapses"
        );

        sim.tick();
        let mob = sim.get(target).expect("must survive the full burn");
        assert!(!mob.is_on_fire(), "must be burnt out at exactly the duration");
        assert!(
            mob.health() < before,
            "the burn must have dealt real damage: before {before}, after {}",
            mob.health()
        );
    }

    /// **The discriminating gate for the fireball's blast half.** A ghast's
    /// fireball explodes unconditionally on impact — `LargeFireball.onHit`'s
    /// own blast, the identical rule [`resolve_projectile_impacts`]'s
    /// wither-skull arm already carries — so a *second* mob standing near
    /// the directly-hit one, never itself on the fireball's own flight path,
    /// must still take damage from the same explosion. The control mob far
    /// past the blast radius must take none, proving this is a real falloff
    /// rather than "every mob in the world takes some damage".
    ///
    /// Fired through open air rather than at a block: a block-hit's impact
    /// point is only known to quarter-block precision
    /// ([`first_solid_along`]'s own doc), which can land the explosion
    /// centre a few hundredths of a block inside the solid cell it just hit
    /// and zero its own exposure — a real property of this sim's coarse
    /// terrain-hit precision, not something this gate exists to probe. A
    /// direct entity hit's impact point has no such rounding.
    #[test]
    fn a_ghast_fireball_exploding_on_direct_impact_damages_a_nearby_bystander_but_not_a_distant_control() {
        let world = ChunkWorld::new(-4, 24);
        let mut sim = MobSim::new(&world);
        let struck = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 0.0));
        // 1.5 blocks off the flight path (which runs along `z = 0`), so the
        // collision sweep can only ever resolve the fireball against
        // `struck`, never against this mob directly.
        let bystander = spawn_target(&mut sim, Vec3::new(3.0, 1.0, 1.5));
        let control = spawn_target(&mut sim, Vec3::new(200.0, 1.0, 200.0));
        sim.spawn_projectile(
            "minecraft:fireball".parse().expect("valid key"),
            Projectile::throwable(Vec3::new(0.0, 1.0, 0.0), Vec3::new(5.0, 0.0, 0.0)),
        );
        let bystander_before = sim.get(bystander).expect("just spawned").health();
        let control_before = sim.get(control).expect("just spawned").health();

        sim.resolve_projectile_impacts();

        let bystander_after = sim.get(bystander).expect("blast alone must not be lethal here").health();
        let control_after = sim.get(control).expect("far outside blast radius").health();
        assert!(
            bystander_after < bystander_before,
            "a fireball's direct hit must still blast a mob standing 1.5 blocks away: before {bystander_before}, after {bystander_after}"
        );
        assert_eq!(
            control_after, control_before,
            "a mob 200+ blocks from the impact is outside the blast radius and must take no damage"
        );
        // `struck` itself must still be alive to read, or the two damage
        // sources (the direct 6.0 plus the blast) combined would have been
        // lethal and the control above would be measuring nothing.
        assert!(sim.get(struck).is_some(), "fixture sanity: the directly-hit mob must survive both hits");
    }
}
