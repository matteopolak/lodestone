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

use crate::redstone_target::HitAxis;

use super::{ChunkWorld, MobSim, ProjectileMeta, ProjectileBlockHit};

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
        // Issue #322: every block impact this pass finds, precise face/frac
        // included — `resolve_projectile_impacts`'s own "before `spent` is
        // consumed" ordering doesn't change here, only what is recorded
        // alongside a block hit rather than only its projectile's removal.
        let mut block_hits: Vec<ProjectileBlockHit> = Vec::new();
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
                (_, Some(block_t)) => {
                    // Issue #322: record the exact face/frac this block was
                    // struck at — `first_solid_along`'s own `block_t` is only
                    // precise to a quarter block, so the cell it lands in is
                    // trusted (that quarter-block cannot straddle two cells)
                    // but the *face* is recomputed exactly via `block_entry`
                    // against that one cell, the same slab test
                    // `clip_aabb` runs for an entity hitbox.
                    let coarse_hit = from + delta.scale(block_t);
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
}
