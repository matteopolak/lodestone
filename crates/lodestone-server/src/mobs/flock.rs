//! Schooling fish: leaders and followers.
//!
//! # What it is
//!
//! Cod, salmon and tropical fish gather into schools. A follower swims after
//! its leader and no longer wanders on its own.
//!
//! # How it works
//!
//! Each fish keeps a [`FlockLink`]: the id of the leader it follows and a
//! `size` counting itself plus its followers (more than 1 means it leads).
//! Every second tick a fish that neither follows nor leads counts down a
//! delay of 100 plus a random 0 to 19 goal ticks; at zero it gathers every fish of
//! its species within 8 blocks that either can still take followers or follows
//! no one. The first one that leads and has room becomes the leader, else the
//! fish itself. Up to `max_school_size - size` of the gathered fish that follow
//! no one (in world order, the leader among them) are limited first and then
//! made followers. A follower farther than 11 blocks from its leader, or whose
//! leader died, stops following. A leader drops back to size 1 with chance
//! 1/200 per tick when no other fish of its species is within 8 blocks.
//! The goal side is `lodestone_entity::ai::roster::aquatic`; this module owns
//! the links because they span mobs.
//!
//! # How to change it
//!
//! `max_school_size` is the per-species cap. Spawn-time grouping (a spawn
//! cluster starting out as one school) is not modelled; schools form through
//! the election above.
//!
//! # Dependencies
//!
//! `MobController::flock_leader`, `has_flock_followers` and `leave_flock`.

use lodestone_entity::ai::MobController;
use lodestone_entity::ai::goal::reduced_tick_delay;

use super::MobSim;

/// A fish's place in a school.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlockLink {
    /// The entity id of the leader being followed.
    pub leader: Option<i32>,
    /// This fish plus its followers.
    pub size: i32,
    /// Goal ticks until the next election attempt, once drawn.
    pub next_start: Option<i32>,
}

impl Default for FlockLink {
    fn default() -> Self {
        Self { leader: None, size: 1, next_start: None }
    }
}

/// The most fish in one school.
#[must_use]
pub fn max_school_size(species: &str) -> Option<i32> {
    match species {
        "cod" | "tropical_fish" => Some(8),
        "salmon" => Some(5),
        _ => None,
    }
}

impl MobSim<'_> {
    /// Advances every school one tick and feeds each fish its leader.
    pub(super) fn update_flocks(&mut self) {
        let n = self.mobs.len();
        if !self.mobs.iter().any(|m| max_school_size(m.entity_type().path()).is_some()) {
            return;
        }
        let alive: Vec<bool> = self.mobs.iter().map(|m| m.health > 0.0).collect();
        let index_of = |mobs: &[super::SimMob<'_>], id: i32| mobs.iter().position(|m| m.id == id);
        let near = |mobs: &[super::SimMob<'_>], i: usize, j: usize| {
            let (a, b) = (mobs[i].position(), mobs[j].position());
            (a.x - b.x).abs() <= 8.0 && (a.y - b.y).abs() <= 8.0 && (a.z - b.z).abs() <= 8.0
        };
        let even_tick = self.tick_count % 2 == 0;
        for i in 0..n {
            let Some(max) = max_school_size(self.mobs[i].entity_type().path()) else { continue };
            if !alive[i] {
                continue;
            }
            // Gave up following (out of range).
            if self.mobs[i].mob.take_flock_left() {
                if let Some(leader) = self.mobs[i].flock.leader.take()
                    && let Some(l) = index_of(&self.mobs, leader)
                {
                    self.mobs[l].flock.size = (self.mobs[l].flock.size - 1).max(1);
                }
            }
            // A dead or missing leader ends the link.
            if let Some(leader) = self.mobs[i].flock.leader
                && !index_of(&self.mobs, leader).is_some_and(|l| alive[l])
            {
                self.mobs[i].flock.leader = None;
            }
            let species = self.mobs[i].entity_type().path().to_owned();
            let same = |mobs: &[super::SimMob<'_>], j: usize| mobs[j].entity_type().path() == species && alive[j];
            // A leader with nobody about disbands.
            if self.mobs[i].flock.size > 1 && self.mobs[i].mob.next_i32(200) == 1 {
                let neighbours = (0..n).filter(|&j| same(&self.mobs, j) && near(&self.mobs, i, j)).count();
                if neighbours <= 1 {
                    self.mobs[i].flock.size = 1;
                }
            }
            if self.mobs[i].flock.leader.is_some() || self.mobs[i].flock.size > 1 || !even_tick {
                continue;
            }
            let delay = |mob: &mut super::SimMob<'_>| reduced_tick_delay(200 + mob.mob.next_i32(200) % 20);
            let Some(wait) = self.mobs[i].flock.next_start else {
                let d = delay(&mut self.mobs[i]);
                self.mobs[i].flock.next_start = Some(d);
                continue;
            };
            if wait > 0 {
                self.mobs[i].flock.next_start = Some(wait - 1);
                continue;
            }
            let d = delay(&mut self.mobs[i]);
            self.mobs[i].flock.next_start = Some(d);
            let follows = |mobs: &[super::SimMob<'_>], j: usize| {
                mobs[j].flock.leader.is_some_and(|l| index_of(mobs, l).is_some_and(|k| alive[k]))
            };
            let can_be_followed = |mobs: &[super::SimMob<'_>], j: usize| mobs[j].flock.size > 1 && mobs[j].flock.size < max;
            let gathered: Vec<usize> = (0..n)
                .filter(|&j| same(&self.mobs, j) && near(&self.mobs, i, j))
                .filter(|&j| can_be_followed(&self.mobs, j) || !follows(&self.mobs, j))
                .collect();
            let leader = gathered.iter().copied().find(|&j| can_be_followed(&self.mobs, j)).unwrap_or(i);
            let room = usize::try_from(max - self.mobs[leader].flock.size).unwrap_or(0);
            let leader_id = self.mobs[leader].id;
            let joiners: Vec<usize> = gathered
                .iter()
                .copied()
                .filter(|&j| !follows(&self.mobs, j))
                .take(room)
                .filter(|&j| j != leader)
                .collect();
            for j in joiners {
                self.mobs[j].flock.leader = Some(leader_id);
                self.mobs[leader].flock.size += 1;
            }
        }
        for i in 0..n {
            if max_school_size(self.mobs[i].entity_type().path()).is_none() {
                continue;
            }
            let leader_at = self.mobs[i]
                .flock
                .leader
                .and_then(|id| index_of(&self.mobs, id))
                .map(|l| self.mobs[l].position());
            let leads = self.mobs[i].flock.size > 1;
            self.mobs[i].mob.set_flock(leader_at, leads);
        }
    }
}
