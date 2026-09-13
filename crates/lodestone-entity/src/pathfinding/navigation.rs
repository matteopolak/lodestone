//! Following a computed [`Path`] over time.
//!
//! A path is not consumed instantly: a mob walks toward the current waypoint,
//! advances when it gets close enough, and the navigation is periodically
//! recomputed and can fail. [`PathNavigator`] models that lifecycle, mirroring
//! vanilla's `PathNavigation.followThePath` / `doStuckDetection`: the
//! close-enough radius scales with the mob's width, and a mob that stops making
//! progress for 100 ticks is declared stuck and its path dropped.
//!
//! The corner-cutting shortcut in vanilla's `followThePath`
//! (`shouldTargetNextNodeInDirection`) needs a world raycast and is intentionally
//! left out here; its absence makes a mob hug waypoints slightly more tightly,
//! never less safely.

use super::search::Path;
use lodestone_model::Vec3;

/// Drives a mob along a [`Path`], advancing waypoints and detecting stalls.
#[derive(Debug, Clone)]
pub struct PathNavigator {
    path: Option<Path>,
    width: f32,
    speed: f32,
    max_distance_to_waypoint: f32,
    tick: i32,
    last_stuck_check: i32,
    last_stuck_pos: Vec3,
    is_stuck: bool,
}

impl PathNavigator {
    /// Creates a navigator for a mob of the given bounding-box width.
    #[must_use]
    pub fn new(width: f32) -> Self {
        Self {
            path: None,
            width,
            speed: 1.0,
            max_distance_to_waypoint: 0.5,
            tick: 0,
            last_stuck_check: 0,
            last_stuck_pos: Vec3::default(),
            is_stuck: false,
        }
    }

    /// Begins following `path` at the given movement speed.
    pub fn start(&mut self, path: Path, speed: f32) {
        self.path = Some(path);
        self.speed = speed;
        self.is_stuck = false;
        self.last_stuck_check = self.tick;
    }

    /// Stops navigation, discarding any path.
    pub fn stop(&mut self) {
        self.path = None;
    }

    /// Whether there is no active path (done or never started).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.path.as_ref().is_none_or(Path::is_done)
    }

    /// Whether the mob was declared stuck on the last tick.
    #[must_use]
    pub fn is_stuck(&self) -> bool {
        self.is_stuck
    }

    /// The active path, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_ref()
    }

    /// Advances the navigation by one tick given the mob's current position.
    ///
    /// Returns the world-space point (bottom-centre of the current waypoint) the
    /// mob should move toward this tick, or `None` if the path is finished or
    /// was dropped (done, or stuck).
    pub fn tick(&mut self, mob_pos: Vec3) -> Option<Vec3> {
        self.tick += 1;
        if self.is_done() {
            return None;
        }

        self.follow(mob_pos);
        self.detect_stuck(mob_pos);

        let path = self.path.as_ref()?;
        path.next_node()
            .map(|n| Vec3::new(f64::from(n.x) + 0.5, f64::from(n.y), f64::from(n.z) + 0.5))
    }

    fn follow(&mut self, mob_pos: Vec3) {
        // maxDistanceToWaypoint scales with width (vanilla followThePath).
        self.max_distance_to_waypoint = if self.width > 0.75 {
            self.width / 2.0
        } else {
            0.75 - self.width / 2.0
        };
        let Some(path) = self.path.as_mut() else {
            return;
        };
        let Some(node) = path.next_node() else {
            return;
        };
        let dx = (mob_pos.x - (f64::from(node.x) + 0.5)).abs();
        let dy = (mob_pos.y - f64::from(node.y)).abs();
        let dz = (mob_pos.z - (f64::from(node.z) + 0.5)).abs();
        let close = dx < f64::from(self.max_distance_to_waypoint)
            && dz < f64::from(self.max_distance_to_waypoint)
            && dy < 1.0;
        if close {
            path.advance();
        }
    }

    fn detect_stuck(&mut self, mob_pos: Vec3) {
        if self.tick - self.last_stuck_check > 100 {
            let effective = if self.speed >= 1.0 {
                self.speed
            } else {
                self.speed * self.speed
            };
            let threshold = effective * 100.0 * 0.25;
            let dsq = {
                let d = mob_pos - self.last_stuck_pos;
                d.x * d.x + d.y * d.y + d.z * d.z
            };
            if dsq < f64::from(threshold * threshold) {
                self.is_stuck = true;
                self.stop();
            } else {
                self.is_stuck = false;
            }
            self.last_stuck_check = self.tick;
            self.last_stuck_pos = mob_pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::search::{Path, PathNode};
    use super::*;
    use lodestone_model::BlockPos;

    fn straight_path() -> Path {
        let nodes = (0..=5)
            .map(|x| PathNode {
                x,
                y: 64,
                z: 0,
                kind: super::super::PathType::Walkable,
            })
            .collect();
        Path::new(nodes, BlockPos::new(5, 64, 0), true)
    }

    #[test]
    fn advances_waypoints_as_mob_approaches() {
        let mut nav = PathNavigator::new(0.6);
        nav.start(straight_path(), 1.0);
        // Standing on node 0.
        let target = nav.tick(Vec3::new(0.5, 64.0, 0.5)).unwrap();
        // Node 0 is close, so it should advance to node 1.
        assert_eq!(target, Vec3::new(1.5, 64.0, 0.5));
    }

    #[test]
    fn reaches_end() {
        let mut nav = PathNavigator::new(0.6);
        nav.start(straight_path(), 1.0);
        // Walk the mob along, snapping onto each node.
        let mut pos = Vec3::new(0.5, 64.0, 0.5);
        for _ in 0..10 {
            if let Some(t) = nav.tick(pos) {
                pos = t;
            }
        }
        assert!(nav.is_done());
    }

    #[test]
    fn stuck_when_not_progressing() {
        let mut nav = PathNavigator::new(0.6);
        nav.start(straight_path(), 1.0);
        // Never move; after >100 ticks stuck detection fires.
        let stuck_pos = Vec3::new(0.2, 64.0, 0.2);
        nav.last_stuck_pos = stuck_pos;
        for _ in 0..102 {
            nav.tick(stuck_pos);
        }
        assert!(nav.is_stuck());
        assert!(nav.is_done());
    }
}
