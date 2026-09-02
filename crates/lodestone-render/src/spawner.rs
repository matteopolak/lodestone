//! Client-side geometry for the spinning miniature mob inside a mob spawner
//! or trial spawner cage.
//!
//! Vanilla's own regular and trial spawner renderers share this exactly —
//! the trial spawner renderer's own spawner-data-extraction function calls
//! the plain spawner renderer's own entity-submit function directly, so one module covers
//! both registrations. Both block types have **real block-model geometry for
//! the cage itself**: `models/block/spawner.json` is a plain
//! `cube_all_inner_faces`, and `trial_spawner.json` is
//! `cube_bottom_top_inner_faces` whose per-state texture (inactive / active /
//! ejecting-reward, each with an `_ominous` sibling) is already selected by
//! the ordinary block mesher from the real `trial_spawner_state`/`ominous`
//! block-state properties — unlike chest/skull, neither cage is a hole in
//! the world. This module is only the mob spinning inside it.
//!
//! # Placement is a *nested* composition, not one matrix
//!
//! Vanilla's own spawner renderer's entity-submit function builds its pose
//! stack with five ordered operations before dispatching to the display
//! entity's own renderer: translate by `(0.5, 0.4, 0.5)`; rotate about world
//! `+Y` by `spin` degrees; translate by `(0, -0.2, 0)`; rotate about world
//! `+X` by `-30` degrees; and scale uniformly by `scale`. It then
//! hands that pose stack to vanilla's own entity-render-dispatcher's submit
//! function, which then
//! dispatches to the display entity's *own* renderer — the ordinary mob
//! render path, with its own flip/lift/yaw — at zero further translation.
//! So [`spawner_display_outer_matrix`] is the **outer** chain only; the
//! caller composes it with [`crate::entity_model_matrix`] at `feet =
//! Vec3::ZERO` for the entity's own placement, exactly the nesting vanilla's
//! two render calls produce, and hands the product to
//! [`crate::EntityModelSet::resolve_at`] — the seam this module was built
//! for, since every other `resolve*` there derives its transform from a
//! `(feet, yaw, scale)` triple under the ordinary convention, which cannot
//! express "placed inside another transform chain".
//!
//! `det(outer) == +1` for any `spin_deg`/positive `scale` (translation and
//! rotation preserve sign; nothing here flips an axis), the same invariant
//! `block_entity_placement_matrix` carries — flipping happens once, inside
//! the entity's own model matrix, not twice.
//!
//! # No packet, and no cuboid rig
//!
//! Like the beacon beam, nothing here is on the wire: the current and
//! previous-tick spin angles are a
//! client-side accumulator (vanilla's own base-spawner client-tick function,
//! ported as [`crate::block_entities::SpawnerSpins`][spins] in the shell
//! crate) and the display entity's *type* comes from the block entity's own
//! spawn-data/spawn-potentials NBT, already resolved to a plain type-path
//! string before it reaches this module. Unlike the beacon, the geometry
//! itself is not procedural — it is the ordinary mob corpus
//! [`crate::EntityModelSet`] already bakes, at a shrunk, tilted, spinning
//! placement.
//!
//! [spins]: ../../lodestone_shell/block_entities/struct.SpawnerSpins.html

use glam::{Mat4, Vec3};

/// Vanilla's own spawner renderer's entity-submit function's outer pose-stack chain,
/// block-relative — the caller composes this with the block's own world
/// translation and with the display entity's ordinary placement matrix (see
/// the module doc).
#[must_use]
pub fn spawner_display_outer_matrix(spin_deg: f32, scale: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(0.5, 0.4, 0.5))
        * Mat4::from_rotation_y(spin_deg.to_radians())
        * Mat4::from_translation(Vec3::new(0.0, -0.2, 0.0))
        * Mat4::from_rotation_x((-30.0_f32).to_radians())
        * Mat4::from_scale(Vec3::splat(scale))
}

/// Vanilla's own spawner-data-extraction function's scale term:
/// vanilla's fixed `0.53125`, halved further (divided by the larger side)
/// once the display entity's own hitbox exceeds one block in width or
/// height — the clause that keeps a large mob's miniature from overflowing
/// the cage. Vanilla starts the scale at `0.53125`, takes the larger of the
/// display entity's own bounding-box width and height, and — only when that
/// larger side exceeds one block — divides the scale by it.
#[must_use]
pub fn spawner_display_scale(bb_width: f32, bb_height: f32) -> f32 {
    const BASE: f32 = 0.531_25;
    let max_len = bb_width.max(bb_height);
    if max_len > 1.0 { BASE / max_len } else { BASE }
}

/// Vanilla's own spawner render-state's spin field: a plain lerp between the
/// previous and current spin, times `10.0`.
///
/// Plain lerp, not the shortest-arc lerp a wrapping angle usually
/// wants — vanilla's own choice, and correct here because
/// vanilla's own base-spawner client-tick function only ever moves `spin` by a handful of degrees a
/// tick (`1000 / (spawnDelay + 200)`, at most `5`), never enough to cross the
/// `0`/`360` wrap within a single tick's interpolation.
#[must_use]
pub fn spawner_spin_degrees(o_spin: f32, spin: f32, partial_tick: f32) -> f32 {
    let t = partial_tick.clamp(0.0, 1.0);
    (o_spin + (spin - o_spin) * t) * 10.0
}

/// One spawner (or trial spawner) cage's miniature display mob for this
/// frame — the render crate's spawn type for
/// `lodestone_shell::block_entities::spawner_mob_spawns`, the same shape
/// [`crate::BeaconSpawn`] is for the beacon: a position plus everything the
/// GPU pass needs to resolve and pose one instance, with no client-owned
/// tracker state travelling through it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnerMobSpawn {
    /// The block's world position (the spawner or trial spawner itself).
    pub pos: [i32; 3],
    /// The entity type to draw, e.g. `"minecraft:zombie"` —
    /// [`crate::EntityModelSet::resolve_at`]'s `type_path`, already resolved
    /// from the block entity's spawn-data/spawn-potentials NBT. This
    /// module has no NBT dependency of its own.
    pub entity_type: String,
    /// Interpolated spin, in degrees — [`spawner_spin_degrees`]'s output.
    pub spin_deg: f32,
    /// [`spawner_display_scale`]'s output for this entity type.
    pub scale: f32,
    /// Packed sky/block light, the same convention every other block-entity
    /// spawn in this crate uses.
    pub light: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outer matrix is a pure rigid transform plus a uniform positive
    /// scale — no axis flip lives here, so its sign must match
    /// `block_entity_placement_matrix`'s own `+1` invariant. The entity's own
    /// `entity_model_matrix` supplies the one flip in the whole nested chain.
    #[test]
    fn outer_matrix_determinant_is_positive_for_any_spin() {
        for spin in [0.0, 37.0, 90.0, 180.0, 271.5, 359.0] {
            let m = spawner_display_outer_matrix(spin, 0.53125);
            assert!(
                m.determinant() > 0.0,
                "spin {spin}: determinant was {}",
                m.determinant()
            );
        }
    }

    /// At `spin = 0`, `scale = 1`: the chain is
    /// `T(0.5,0.4,0.5) · T(0,-0.2,0) · RotX(-30°)`, so the nested entity's own
    /// origin (its feet, since the caller composes this with
    /// `entity_model_matrix(Vec3::ZERO, ..)`) lands at `(0.5, 0.2, 0.5)` —
    /// hand-derived from the two translates alone, since `RotX`/scale act on
    /// the origin as identities.
    #[test]
    fn zero_spin_places_the_origin_at_the_hand_derived_point() {
        let m = spawner_display_outer_matrix(0.0, 1.0);
        let p = m.transform_point3(Vec3::ZERO);
        assert!((p - Vec3::new(0.5, 0.2, 0.5)).length() < 1e-5, "{p:?}");
    }

    /// A `90°` spin must move a probe point off the `X`/`Z` axes it started
    /// on — the sanity check that `spin_deg` actually reaches the rotation
    /// term and is not silently dropped.
    #[test]
    fn spin_rotates_a_probe_point() {
        let at_rest = spawner_display_outer_matrix(0.0, 1.0).transform_point3(Vec3::new(0.0, 0.0, 1.0));
        let spun = spawner_display_outer_matrix(90.0, 1.0).transform_point3(Vec3::new(0.0, 0.0, 1.0));
        assert!((at_rest - spun).length() > 0.1, "{at_rest:?} vs {spun:?}");
    }

    /// Below the 1-block threshold: the base constant, untouched — a pig's
    /// real base hitbox (`0.9 × 0.9`, from `lodestone_data::entity_dimensions`).
    #[test]
    fn small_mob_uses_the_base_scale_unmodified() {
        assert!((spawner_display_scale(0.9, 0.9) - 0.531_25).abs() < 1e-6);
    }

    /// Above the threshold: divided by the larger side — a zombie's real base
    /// hitbox (`0.6 × 1.95`), predicted from the formula's own constants
    /// rather than a remembered literal.
    #[test]
    fn tall_mob_is_shrunk_by_its_own_height() {
        let expected = 0.531_25 / 1.95_f32;
        assert!((spawner_display_scale(0.6, 1.95) - expected).abs() < 1e-6);
    }

    /// Exactly `1.0` does not divide — vanilla's guard is a strict `>`.
    #[test]
    fn exactly_one_block_does_not_trigger_the_shrink() {
        assert!((spawner_display_scale(1.0, 1.0) - 0.531_25).abs() < 1e-6);
    }

    /// Predicted from the formula's own constants, not a remembered number:
    /// `(10 + (20-10) * 0.5) * 10 = 150`.
    #[test]
    fn spin_degrees_lerps_then_scales_by_ten() {
        assert!((spawner_spin_degrees(10.0, 20.0, 0.5) - 150.0).abs() < 1e-5);
    }

    /// At `partial_tick = 0` the result is `o_spin * 10`; at `1` it is
    /// `spin * 10` — the two endpoints a lerp must hit exactly.
    #[test]
    fn spin_degrees_endpoints_are_the_raw_values_times_ten() {
        assert!((spawner_spin_degrees(4.0, 9.0, 0.0) - 40.0).abs() < 1e-5);
        assert!((spawner_spin_degrees(4.0, 9.0, 1.0) - 90.0).abs() < 1e-5);
    }
}
