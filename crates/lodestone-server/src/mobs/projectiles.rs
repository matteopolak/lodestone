//! `MobSim`'s projectile-tracking slice — arrow/throwable spawn, per-tick
//! impact resolution, and the projectile query API. Moved out of
//! `mobs/mod.rs` verbatim as part of the `mobs.rs` file split (see
//! `docs/plans/crate-and-file-splits.md`). Zero visibility churn: every
//! method below was already `pub`, and the one private helper
//! (`resolve_projectile_hit`) is called only from `resolve_projectile_impacts`
//! in this same file.

use lodestone_entity::DamageFlags;
use lodestone_entity::projectile::{Projectile, TrackedProjectile};
use lodestone_model::{ResourceKey, Vec3};
use uuid::Uuid;

use super::{ChunkWorld, MobSim, ProjectileMeta};

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
            },
        );
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
    /// * **A fireball's five seconds of fire are not applied.** [`SimMob`] has no
    ///   burning state at all (`SimMob::ignite` is the *creeper fuse*, a different
    ///   mechanic that happens to share the verb), so there is nothing to write
    ///   the fire ticks into. The fireball's `5.0` damage does land.
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
        for tracked in self.projectiles.iter() {
            let from = tracked.projectile.position;
            let delta = tracked.projectile.velocity;
            if delta.length() < 1e-9 {
                continue;
            }
            let meta = self.projectile_meta.get(&tracked.id);
            let owner = meta.and_then(|m| m.owner);
            let margin = lodestone_entity::projectile::hitbox_margin(tracked.ticks_alive);

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

            let block_t = first_solid_along(self.world, from, delta);
            match (nearest, block_t) {
                (Some((entity_t, target)), block) if block.is_none_or(|b| entity_t <= b) => {
                    let entity_type = meta.map(|m| m.entity_type.path().to_owned());
                    hits.push(ProjectileHit {
                        projectile: tracked.id,
                        target,
                        entity_type: entity_type.unwrap_or_default(),
                        speed: delta.length(),
                        origin: from,
                    });
                }
                (_, Some(_)) => spent.push(tracked.id),
                // Nothing on this segment, or a mob further along it than the
                // block that stopped the projectile first.
                _ => {}
            }
        }

        let removed = hits.len() + spent.len();
        for hit in hits {
            self.resolve_projectile_hit(&hit);
            self.remove_projectile(hit.projectile);
        }
        for id in spent {
            self.remove_projectile(id);
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
        // `minecraft:arrow` is the damage type for an arrow, `minecraft:thrown`
        // for a throwable, `minecraft:fireball` for a small fireball — all three
        // are ordinary reducible types (none carries `bypasses_armor`), so armour
        // reduces a projectile hit exactly as it reduces a melee one.
        let flags = DamageFlags::for_damage_type_name(projectile_damage_type(&hit.entity_type))
            .unwrap_or_default();
        let applied = {
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
            applied
        };
        self.note_vocalisation(hit.target, applied);
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
