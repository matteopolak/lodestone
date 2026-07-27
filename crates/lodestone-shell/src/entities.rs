//! Client-side entity interpolation: turning the 20 Hz stream of
//! [`EntityView`](lodestone_client::EntityView) snapshots into smooth per-frame
//! render transforms.
//!
//! The server reports entity positions at tick rate (20 Hz); the shell draws at
//! 50–120 fps. Snapping each mob to its latest reported position would stutter
//! visibly, so — exactly as vanilla does — we render one tick *behind* the
//! latest snapshot and ease from the previous position to the current one over a
//! tick. When a fresh position arrives we start the new interpolation from where
//! the mob is *currently being drawn*, not from the stale target, so motion is
//! C0-continuous and never jumps.
//!
//! This module is deliberately GPU-free and depends only on `glam`, so the
//! interpolation is unit-testable without a device or a server: the sim converts
//! each [`EntityView`] into an [`EntitySnapshot`] (version-free, glam-only) and
//! feeds those in. The output is a flat list of [`EntityDraw`]s — type path,
//! feet position, body yaw and scale — that the renderer resolves into instanced
//! draws.

use std::collections::HashMap;

use glam::Vec3;

/// One physics tick, in seconds. Interpolation eases over exactly this window,
/// so a mob reaches its newest reported position one tick after it arrives.
const TICK: f32 = 0.05;

/// Position change (blocks) below which a snapshot is treated as "no movement",
/// so idle mobs don't restart their interpolation clock every frame.
const POS_EPS: f32 = 1.0e-4;

/// Yaw change (degrees) below which a snapshot is treated as "no turn".
const YAW_EPS: f32 = 1.0e-2;

/// A version-free entity snapshot as reported by the client for one tick. Built
/// by the sim from an [`EntityView`](lodestone_client::EntityView); carries only
/// what the renderer needs, in glam types, so this module needs no client or
/// model dependency.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySnapshot {
    /// The server-assigned entity id (interpolation key).
    pub id: i32,
    /// The entity type's canonical path (e.g. `"pig"`), for model resolution.
    pub type_path: String,
    /// Feet position in world space.
    pub feet: Vec3,
    /// Body yaw in degrees.
    pub yaw: f32,
    /// Uniform render scale (baby mobs are drawn smaller).
    pub scale: f32,
}

/// A single entity ready to draw this frame: its model type and interpolated
/// transform inputs. The renderer turns this into an
/// [`EntityInstance`](lodestone_render::EntityInstance).
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDraw {
    /// The entity type's canonical path (e.g. `"pig"`).
    pub type_path: String,
    /// Interpolated feet position in world space.
    pub feet: Vec3,
    /// Interpolated body yaw in degrees.
    pub yaw: f32,
    /// Uniform render scale.
    pub scale: f32,
}

/// Per-entity interpolation track: the position/yaw we are easing *from*, the
/// latest reported target we are easing *to*, and how far through the tick we
/// are.
#[derive(Debug, Clone)]
struct Track {
    type_path: String,
    scale: f32,
    prev: Vec3,
    curr: Vec3,
    prev_yaw: f32,
    curr_yaw: f32,
    /// Seconds since `curr` was set, capped at [`TICK`].
    t: f32,
}

impl Track {
    /// The fraction `[0, 1]` through the current tick's interpolation.
    fn alpha(&self) -> f32 {
        (self.t / TICK).clamp(0.0, 1.0)
    }

    /// The currently-drawn position: `prev` eased toward `curr` by `alpha`.
    fn render_pos(&self) -> Vec3 {
        self.prev.lerp(self.curr, self.alpha())
    }

    /// The currently-drawn body yaw, taking the shortest arc so a wrap across
    /// 360° (e.g. 350°→10°) turns +20° rather than −340°.
    fn render_yaw(&self) -> f32 {
        lerp_angle(self.prev_yaw, self.curr_yaw, self.alpha())
    }
}

/// Tracks and interpolates every visible entity between server ticks.
#[derive(Debug, Default)]
pub struct EntityInterpolator {
    tracks: HashMap<i32, Track>,
}

impl EntityInterpolator {
    /// A fresh interpolator with no tracked entities.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance every track by `dt` seconds, then fold in this frame's snapshots.
    ///
    /// Entities absent from `snapshots` are dropped (despawned/out of range).
    /// A snapshot whose position or yaw differs from the current target starts a
    /// new interpolation *from the current render pose*, so the mob never jumps.
    /// A snapshot that matches the current target only lets the existing ease
    /// run to completion.
    pub fn update(&mut self, snapshots: &[EntitySnapshot], dt: f32) {
        // Advance existing clocks first, so a snapshot that resets `t` to 0 this
        // frame starts the new tick from exactly its previous render pose.
        for track in self.tracks.values_mut() {
            track.t = (track.t + dt).min(TICK);
        }

        for snap in snapshots {
            match self.tracks.get_mut(&snap.id) {
                None => {
                    // A newly seen entity is drawn at rest at its reported pose.
                    self.tracks.insert(
                        snap.id,
                        Track {
                            type_path: snap.type_path.clone(),
                            scale: snap.scale,
                            prev: snap.feet,
                            curr: snap.feet,
                            prev_yaw: snap.yaw,
                            curr_yaw: snap.yaw,
                            t: TICK,
                        },
                    );
                }
                Some(track) => {
                    track.type_path.clone_from(&snap.type_path);
                    track.scale = snap.scale;
                    let moved = (snap.feet - track.curr).length() > POS_EPS;
                    let turned = angle_diff(snap.yaw, track.curr_yaw).abs() > YAW_EPS;
                    if moved || turned {
                        // Re-anchor the ease at where the mob is drawn right now.
                        track.prev = track.render_pos();
                        track.prev_yaw = track.render_yaw();
                        track.curr = snap.feet;
                        track.curr_yaw = snap.yaw;
                        track.t = 0.0;
                    }
                }
            }
        }

        // Drop tracks for entities no longer reported.
        let seen: std::collections::HashSet<i32> = snapshots.iter().map(|s| s.id).collect();
        self.tracks.retain(|id, _| seen.contains(id));
    }

    /// The interpolated draw list for this frame. Order is unspecified (grouped
    /// by model downstream), so no ordering guarantees are made here.
    #[must_use]
    pub fn draws(&self) -> Vec<EntityDraw> {
        self.tracks
            .values()
            .map(|t| EntityDraw {
                type_path: t.type_path.clone(),
                feet: t.render_pos(),
                yaw: t.render_yaw(),
                scale: t.scale,
            })
            .collect()
    }

    /// Number of entities currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

/// The signed shortest difference `a − b` mapped into `(−180, 180]` degrees.
fn angle_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// Interpolate between two angles along the shortest arc.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    a + angle_diff(b, a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: i32, feet: Vec3, yaw: f32) -> EntitySnapshot {
        EntitySnapshot {
            id,
            type_path: "pig".into(),
            feet,
            yaw,
            scale: 1.0,
        }
    }

    #[test]
    fn a_new_entity_is_drawn_at_its_reported_pose() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::new(3.0, 64.0, -2.0), 90.0)], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].feet, Vec3::new(3.0, 64.0, -2.0));
        assert_eq!(draws[0].yaw, 90.0);
    }

    #[test]
    fn movement_interpolates_rather_than_snapping() {
        let mut interp = EntityInterpolator::new();
        // Establish the entity at the origin, its ease already complete.
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], TICK);
        // A new position arrives 4 blocks along +X.
        let target = Vec3::new(4.0, 0.0, 0.0);
        interp.update(&[snap(1, target, 0.0)], 0.0);

        // At t≈0 the mob must still be at (near) the old pose — NOT snapped to
        // the target. This is the anti-vacuity guard: a renderer that ignored
        // interpolation and drew the latest snapshot would already be at x=4.
        let x0 = interp.draws()[0].feet.x;
        assert!(
            x0 < 0.5,
            "on a fresh snapshot the mob must start from its old pose, was x={x0}"
        );

        // Half a tick later it must be strictly between old and new — a snap
        // (jump straight to 4) or a freeze (stuck at 0) both fail this.
        interp.update(&[snap(1, target, 0.0)], TICK / 2.0);
        let xm = interp.draws()[0].feet.x;
        assert!(
            xm > 0.5 && xm < 3.5,
            "half a tick in, the mob should be mid-way, was x={xm}"
        );

        // A full tick after the snapshot it reaches the target.
        interp.update(&[snap(1, target, 0.0)], TICK);
        let xf = interp.draws()[0].feet.x;
        assert!(
            (xf - 4.0).abs() < 1.0e-3,
            "after a tick it arrives, was x={xf}"
        );
    }

    #[test]
    fn a_despawned_entity_stops_being_drawn() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 0.0), snap(2, Vec3::X, 0.0)], 0.016);
        assert_eq!(interp.len(), 2);
        // Entity 2 vanishes from the report.
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1, "the despawned entity must be gone");
    }

    #[test]
    fn yaw_interpolates_along_the_shortest_arc_across_the_wrap() {
        let mut interp = EntityInterpolator::new();
        interp.update(&[snap(1, Vec3::ZERO, 350.0)], TICK);
        // Turn to 10°: the short way is +20° through 360/0, not −340° through 180.
        interp.update(&[snap(1, Vec3::ZERO, 10.0)], 0.0);
        interp.update(&[snap(1, Vec3::ZERO, 10.0)], TICK / 2.0);
        let y = interp.draws()[0].yaw;
        // Halfway along the +20° arc from 350° is 360° ≡ 0°. Reject the long-way
        // answer (~180°), which is what naive linear lerp would give.
        let near_zero = y.rem_euclid(360.0);
        let dist = near_zero.min(360.0 - near_zero);
        assert!(dist < 5.0, "yaw should pass through ~0°, was {y}");
    }
}
