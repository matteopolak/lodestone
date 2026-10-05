//! Per-entity posture state for the wolf, the fox and the felines: the wire flags
//! that say what a mob is doing (sitting, sleeping, crouching, lying) and the
//! amounts the client ramps every tick from them.
//!
//! The server sends flags only. A fox's crouch depth, its interested head tilt, and
//! a cat's lie-down and relax amounts are client-side ramps that move a fixed step
//! per tick toward the flag's state. [`PostureRamps::step`] is one 20 Hz tick of
//! that, free of the ECS; [`tick_posture_ramps`] feeds it from the ingest entity and
//! [`PostureRamps::posture`] interpolates the render input at the frame's partial
//! tick.
//!
//! See `docs/entity-postures.md`.

use bevy_ecs::prelude::*;
use lodestone_model::{EntityPose, MobAppearance};
use lodestone_render::entity_posture::{
    Posture, WOLF_TAIL_ANGRY, WOLF_TAIL_WILD, wolf_tame_tail_angle,
};

/// Which posture a track keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostureSpecies {
    /// Sits on its tameable flag; the tail follows anger, tameness and health.
    Wolf,
    /// Sits, sleeps, crouches and pounces on its own flag byte.
    Fox,
    /// A cat or ocelot: sits on its tameable flag, crouches and runs on the shared
    /// pose and flags, and a cat lies down and relaxes on its own flags.
    Feline,
}

impl PostureSpecies {
    /// The species an entity type path names, if it keeps a posture.
    #[must_use]
    pub fn of(type_path: &str) -> Option<Self> {
        Some(match type_path {
            "wolf" => Self::Wolf,
            "fox" => Self::Fox,
            "cat" | "ocelot" => Self::Feline,
            _ => return None,
        })
    }
}

/// The fox flag byte's bits.
const FOX_SITTING: u8 = 0x01;
const FOX_CROUCHING: u8 = 0x04;
const FOX_INTERESTED: u8 = 0x08;
const FOX_POUNCING: u8 = 0x10;
const FOX_SLEEPING: u8 = 0x20;
const FOX_FACEPLANTED: u8 = 0x40;

/// The shared entity flags byte's sprinting bit.
const SPRINTING: u8 = 0x08;

/// A tamed wolf's maximum health: taming raises it from 8 to 40.
const TAME_WOLF_MAX_HEALTH: f32 = 40.0;

/// A fox's crouch deepens by this much per tick, up to [`FOX_CROUCH_MAX`].
const FOX_CROUCH_STEP: f32 = 0.2;
const FOX_CROUCH_MAX: f32 = 5.0;
/// A fox's interested tilt closes this fraction of the gap per tick.
const FOX_INTEREST_RATE: f32 = 0.4;
/// The tilt at full interest, in radians: `0.11 PI`.
const FOX_INTEREST_ROLL: f32 = 0.11 * std::f32::consts::PI;
/// A cat's lie-down ramp: up per tick while lying, down per tick otherwise, for the
/// body and (more slowly) the tail.
const CAT_LIE_UP: f32 = 0.15;
const CAT_LIE_DOWN: f32 = 0.22;
const CAT_LIE_TAIL_UP: f32 = 0.08;
const CAT_LIE_TAIL_DOWN: f32 = 0.13;
const CAT_RELAX_UP: f32 = 0.1;
const CAT_RELAX_DOWN: f32 = 0.13;

/// What one tick of [`PostureRamps::step`] reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostureFacts {
    /// The tameable sitting bit (wolf, cat).
    pub sitting: bool,
    /// Its species appearance fields.
    pub appearance: MobAppearance,
    /// Its pose, if reported.
    pub pose: Option<EntityPose>,
    /// The shared entity flags byte.
    pub entity_flags: u8,
    /// Whether it is tame.
    pub tamed: bool,
    /// Its health, if reported.
    pub health: Option<f32>,
    /// The client's game time in ticks, for a wolf's anger.
    pub game_time: i64,
}

/// A value and its previous tick's value, for interpolation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Ramp {
    old: f32,
    now: f32,
}

impl Ramp {
    fn set(&mut self, value: f32) {
        self.old = self.now;
        self.now = value;
    }

    fn at(self, partial_tick: f32) -> f32 {
        self.old + (self.now - self.old) * partial_tick
    }
}

/// A track's posture flags and ramps.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct PostureRamps {
    species: PostureSpecies,
    sitting: bool,
    crouching: bool,
    sprinting: bool,
    sleeping: bool,
    pouncing: bool,
    faceplanted: bool,
    angry: bool,
    tail_angle: f32,
    crouch: Ramp,
    interest: Ramp,
    lie: Ramp,
    lie_tail: Ramp,
    relax: Ramp,
}

impl PostureRamps {
    /// A standing, wild posture for `species`.
    #[must_use]
    pub fn new(species: PostureSpecies) -> Self {
        PostureRamps {
            species,
            sitting: false,
            crouching: false,
            sprinting: false,
            sleeping: false,
            pouncing: false,
            faceplanted: false,
            angry: false,
            tail_angle: WOLF_TAIL_WILD,
            crouch: Ramp::default(),
            interest: Ramp::default(),
            lie: Ramp::default(),
            lie_tail: Ramp::default(),
            relax: Ramp::default(),
        }
    }

    /// The species this keeps.
    #[must_use]
    pub fn species(&self) -> PostureSpecies {
        self.species
    }

    /// Advances one 20 Hz tick.
    pub fn step(&mut self, facts: &PostureFacts) {
        let a = &facts.appearance;
        match self.species {
            PostureSpecies::Wolf => {
                self.sitting = facts.sitting;
                self.angry = a.wolf_anger_end_time.is_some_and(|end| end > 0 && end - facts.game_time > 0);
                self.tail_angle = if self.angry {
                    WOLF_TAIL_ANGRY
                } else if facts.tamed {
                    wolf_tame_tail_angle(facts.health.unwrap_or(TAME_WOLF_MAX_HEALTH), TAME_WOLF_MAX_HEALTH)
                } else {
                    WOLF_TAIL_WILD
                };
            }
            PostureSpecies::Fox => {
                let flags = a.fox_flags.unwrap_or(0);
                self.sitting = flags & FOX_SITTING != 0;
                self.crouching = flags & FOX_CROUCHING != 0;
                self.pouncing = flags & FOX_POUNCING != 0;
                self.sleeping = flags & FOX_SLEEPING != 0;
                self.faceplanted = flags & FOX_FACEPLANTED != 0;
                let target = if flags & FOX_INTERESTED != 0 { 1.0 } else { 0.0 };
                let interest = self.interest.now;
                self.interest.set(interest + (target - interest) * FOX_INTEREST_RATE);
                let crouch = if self.crouching { (self.crouch.now + FOX_CROUCH_STEP).min(FOX_CROUCH_MAX) } else { 0.0 };
                self.crouch.set(crouch);
            }
            PostureSpecies::Feline => {
                self.sitting = facts.sitting;
                self.crouching = facts.pose == Some(EntityPose::Crouching);
                self.sprinting = facts.entity_flags & SPRINTING != 0;
                let lying = a.cat_lying.unwrap_or(false);
                let relaxed = a.cat_relaxed.unwrap_or(false);
                let ramp = |r: &mut Ramp, on: bool, up: f32, down: f32| {
                    let next = if on { (r.now + up).min(1.0) } else { (r.now - down).max(0.0) };
                    r.set(next);
                };
                ramp(&mut self.lie, lying, CAT_LIE_UP, CAT_LIE_DOWN);
                ramp(&mut self.lie_tail, lying, CAT_LIE_TAIL_UP, CAT_LIE_TAIL_DOWN);
                ramp(&mut self.relax, relaxed, CAT_RELAX_UP, CAT_RELAX_DOWN);
            }
        }
    }

    /// The render input at `partial_tick` into the current tick.
    #[must_use]
    pub fn posture(&self, partial_tick: f32) -> Posture {
        Posture {
            sitting: self.sitting,
            crouching: self.crouching,
            sprinting: self.sprinting,
            sleeping: self.sleeping,
            pouncing: self.pouncing,
            faceplanted: self.faceplanted,
            crouch_amount: self.crouch.at(partial_tick),
            head_roll: self.interest.at(partial_tick) * FOX_INTEREST_ROLL,
            lie_down: self.lie.at(partial_tick),
            lie_down_tail: self.lie_tail.at(partial_tick),
            relax: self.relax.at(partial_tick),
            tail_angle: self.tail_angle,
            angry: self.angry,
            axolotl: lodestone_render::entity_posture::AxolotlFactors::NONE,
        }
    }

    /// Whether a fox is asleep, which also swaps its sheet for the closed-eye one.
    #[must_use]
    pub fn sleeping(&self) -> bool {
        self.sleeping
    }
}

/// `GameTick` / `TickSet::Animate`: steps every posture track one tick from the
/// ingest entity's facts, bridged through [`EntityIndex`].
pub fn tick_posture_ramps(
    index: Res<EntityIndex>,
    world_time: Option<Res<lodestone_ecs::WorldTime>>,
    facts: Query<(
        Option<&lodestone_ecs::entity::Sitting>,
        Option<&lodestone_ecs::entity::Appearance>,
        Option<&lodestone_ecs::entity::Pose>,
        Option<&lodestone_ecs::entity::EntityFlags>,
        Option<&lodestone_ecs::entity::Tamed>,
        Option<&lodestone_ecs::entity::Health>,
    )>,
    mut tracks: Query<(&lodestone_ecs::entity::MinecraftEntityId, &mut PostureRamps)>,
) {
    let game_time = world_time.as_deref().map_or(0, |t| t.age);
    for (id, mut ramps) in &mut tracks {
        let mut input = PostureFacts { game_time, ..PostureFacts::default() };
        if let Some(entity) = index.get(id.0)
            && let Ok((sitting, appearance, pose, flags, tamed, health)) = facts.get(entity)
        {
            input.sitting = sitting.is_some_and(|s| s.0);
            input.appearance = appearance.map(|a| a.0).unwrap_or_default();
            input.pose = pose.map(|p| p.0);
            input.entity_flags = flags.map_or(0, |f| f.0);
            input.tamed = tamed.is_some_and(|t| t.0);
            input.health = health.map(|h| h.0);
        }
        ramps.step(&input);
    }
}

use lodestone_ecs::entity::EntityIndex;

#[cfg(test)]
mod tests {
    use super::*;

    fn fox(flags: u8) -> PostureFacts {
        PostureFacts {
            appearance: MobAppearance { fox_flags: Some(flags), ..MobAppearance::default() },
            ..PostureFacts::default()
        }
    }

    /// A crouching fox deepens 0.2 a tick and stops at 5: after 3 ticks 0.6 (drawn
    /// from 0.4 at the start of the frame's tick to 0.6 at its end), after 30 still 5.
    /// Clearing the flag drops it to 0 at once.
    #[test]
    fn a_fox_crouch_ramps_by_a_fifth_and_caps_at_five() {
        let mut r = PostureRamps::new(PostureSpecies::Fox);
        for _ in 0..3 {
            r.step(&fox(FOX_CROUCHING));
        }
        assert!((r.posture(1.0).crouch_amount - 0.6).abs() < 1.0e-5);
        assert!((r.posture(0.0).crouch_amount - 0.4).abs() < 1.0e-5);
        // Halfway into the next tick: 0.4 + (0.6 - 0.4) / 2 = 0.5 between the last two ticks.
        assert!((r.posture(0.5).crouch_amount - 0.5).abs() < 1.0e-5);
        for _ in 0..30 {
            r.step(&fox(FOX_CROUCHING));
        }
        assert!((r.posture(0.0).crouch_amount - 5.0).abs() < 1.0e-5);
        r.step(&fox(0));
        assert_eq!(r.posture(1.0).crouch_amount, 0.0);
        assert!(!r.posture(0.0).crouching);
    }

    /// The interested tilt closes 40% of the gap a tick: 0.4, then 0.64, of `0.11 PI`.
    #[test]
    fn a_fox_interest_tilt_eases_toward_full() {
        let mut r = PostureRamps::new(PostureSpecies::Fox);
        r.step(&fox(FOX_INTERESTED));
        r.step(&fox(FOX_INTERESTED));
        let roll = r.posture(1.0).head_roll;
        assert!((roll - 0.64 * 0.11 * std::f32::consts::PI).abs() < 1.0e-5, "{roll}");
    }

    #[test]
    fn the_fox_flag_bits_land_on_their_own_states() {
        let mut r = PostureRamps::new(PostureSpecies::Fox);
        r.step(&fox(FOX_SLEEPING | FOX_SITTING));
        let p = r.posture(0.0);
        assert!(p.sleeping && p.sitting && !p.crouching && !p.pouncing && !p.faceplanted);
        r.step(&fox(FOX_POUNCING | FOX_FACEPLANTED));
        let p = r.posture(0.0);
        assert!(!p.sleeping && p.pouncing && p.faceplanted);
    }

    /// A lying cat: body +0.15 and tail +0.08 a tick, so after 4 ticks 0.6 and 0.32;
    /// standing up takes them down 0.22 and 0.13: 0.38 and 0.19.
    #[test]
    fn a_cat_lies_down_and_gets_up_at_its_own_rates() {
        let mut r = PostureRamps::new(PostureSpecies::Feline);
        let cat = |lying| PostureFacts {
            appearance: MobAppearance { cat_lying: Some(lying), ..MobAppearance::default() },
            ..PostureFacts::default()
        };
        for _ in 0..4 {
            r.step(&cat(true));
        }
        let p = r.posture(1.0);
        assert!((p.lie_down - 0.6).abs() < 1.0e-5 && (p.lie_down_tail - 0.32).abs() < 1.0e-5, "{p:?}");
        r.step(&cat(false));
        let p = r.posture(1.0);
        assert!((p.lie_down - 0.38).abs() < 1.0e-5 && (p.lie_down_tail - 0.19).abs() < 1.0e-5, "{p:?}");
    }

    #[test]
    fn a_feline_crouches_on_its_pose_and_runs_on_its_flag() {
        let mut r = PostureRamps::new(PostureSpecies::Feline);
        r.step(&PostureFacts { pose: Some(EntityPose::Crouching), entity_flags: SPRINTING, sitting: true, ..PostureFacts::default() });
        let p = r.posture(0.0);
        assert!(p.crouching && p.sprinting && p.sitting);
        r.step(&PostureFacts::default());
        let p = r.posture(0.0);
        assert!(!p.crouching && !p.sprinting && !p.sitting);
    }

    /// The wolf's tail: wild at `PI / 5`, angry while its anger ends after the clock,
    /// and a tame wolf at half of 40 health droops to `0.35 PI`.
    #[test]
    fn a_wolf_tail_follows_anger_tameness_and_health() {
        let mut r = PostureRamps::new(PostureSpecies::Wolf);
        r.step(&PostureFacts::default());
        assert_eq!(r.posture(0.0).tail_angle, WOLF_TAIL_WILD);
        let angry = PostureFacts {
            appearance: MobAppearance { wolf_anger_end_time: Some(500), ..MobAppearance::default() },
            game_time: 100,
            ..PostureFacts::default()
        };
        r.step(&angry);
        assert!(r.posture(0.0).angry && r.posture(0.0).tail_angle == WOLF_TAIL_ANGRY);
        r.step(&PostureFacts { game_time: 600, ..angry });
        assert!(!r.posture(0.0).angry, "control: anger that has run out");
        r.step(&PostureFacts { tamed: true, health: Some(20.0), sitting: true, ..PostureFacts::default() });
        let p = r.posture(0.0);
        assert!((p.tail_angle - 0.35 * std::f32::consts::PI).abs() < 1.0e-5 && p.sitting);
    }
}
