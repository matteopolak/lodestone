//! A mob walking between the points of interest of a village.

use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal};
use super::mob::MobController;

/// A village counts as close when a claimed point of interest is within this
/// many blocks (six chunk sections).
pub const CLOSE_TO_VILLAGE: f64 = 96.0;
/// A stroll candidate is inside the village when a point of interest is this near.
const CANDIDATE_VILLAGE_RADIUS: f64 = 16.0;
/// How far from a candidate the point of interest to visit may be.
const POI_SEARCH_RADIUS: f64 = 10.0;
/// Stroll candidates sampled per search.
const ATTEMPTS: usize = 10;
/// Most recently visited points of interest remembered.
const VISITED_MEMORY: usize = 15;

fn flat_distance_sqr(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).powi(2) + (a.z - b.z).powi(2)
}

fn distance_sqr(a: Vec3, b: Vec3) -> f64 {
    flat_distance_sqr(a, b) + (a.y - b.y).powi(2)
}

/// Whether any claimed village point of interest lies within `radius` of `at`.
#[must_use]
pub fn near_village(mob: &dyn MobController, at: Vec3, radius: f64) -> bool {
    mob.village_pois().iter().any(|p| flat_distance_sqr(*p, at) <= radius * radius)
}

/// Walks to a point of interest it has not visited lately, optionally only
/// after dark, and remembers the last fifteen it reached.
#[derive(Debug)]
pub struct MoveThroughVillageGoal {
    speed: f64,
    only_at_night: bool,
    reach: f64,
    target: Option<Vec3>,
    visited: Vec<Vec3>,
}

impl MoveThroughVillageGoal {
    /// A village walk at `speed` blocks per tick that ends within `reach`
    /// blocks of the point of interest.
    #[must_use]
    pub fn new(speed: f64, only_at_night: bool, reach: f64) -> Self {
        Self { speed, only_at_night, reach, target: None, visited: Vec::new() }
    }

    fn unvisited_near(&self, mob: &dyn MobController, at: Vec3) -> Option<Vec3> {
        mob.village_pois()
            .iter()
            .copied()
            .filter(|p| !self.visited.contains(p) && distance_sqr(*p, at) <= POI_SEARCH_RADIUS * POI_SEARCH_RADIUS)
            .min_by(|a, b| distance_sqr(*a, at).total_cmp(&distance_sqr(*b, at)))
    }
}

impl Goal for MoveThroughVillageGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.visited.len() > VISITED_MEMORY {
            self.visited.remove(0);
        }
        if self.only_at_night && mob.bright_outside() {
            return false;
        }
        let here = mob.position();
        if !near_village(mob, here, CLOSE_TO_VILLAGE) {
            return false;
        }
        // The candidate whose nearest unvisited point of interest is closest
        // to where the mob stands wins.
        let mut best: Option<(f64, Vec3)> = None;
        for _ in 0..ATTEMPTS {
            let Some(candidate) = mob.random_stroll_target() else { continue };
            if !near_village(mob, candidate, CANDIDATE_VILLAGE_RADIUS) {
                continue;
            }
            let Some(poi) = self.unvisited_near(mob, candidate) else { continue };
            let score = -distance_sqr(poi, here);
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, candidate));
            }
        }
        let Some((_, landing)) = best else { return false };
        let Some(poi) = self.unvisited_near(mob, landing) else { return false };
        self.target = Some(poi);
        true
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(target) = self.target else { return false };
        !mob.navigation_done() && flat_distance_sqr(target, mob.position()) > (self.reach + 0.6).powi(2)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        let Some(target) = self.target else { return };
        if !mob.move_to(target, self.speed)
            && let Some(step) = mob.random_target_towards(target)
        {
            mob.move_to(step, self.speed);
        }
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        if let Some(target) = self.target.take()
            && (mob.navigation_done() || flat_distance_sqr(target, mob.position()) <= self.reach * self.reach)
        {
            self.visited.push(target);
        }
    }
}
