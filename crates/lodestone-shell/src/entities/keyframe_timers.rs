//! Per-entity keyframe animation timers: the client-side state machines that decide
//! which keyframe animation of a mob is running and since when.
//!
//! The server never sends an animation clock. It sends a few facts (an entity-event
//! byte for a rabbit's hop, a pose for a frog, a state ordinal for a sniffer, a
//! pose-change stamp for a camel) and the client keeps its own per-tick timers from
//! them. [`KeyframeTimers::step`] is that logic, one 20 Hz tick at a time and free of
//! the ECS so it can be tested directly; [`tick_keyframe_timers`] feeds it from the
//! ingest entity and [`KeyframeTimers::keyframes`] turns the result into the render
//! input at the frame's partial tick.
//!
//! See `docs/keyframe-animation.md`.

use bevy_ecs::prelude::*;
use lodestone_model::{EntityPose, MobAppearance};
use lodestone_render::entity_keyframe::{ALL_SLOTS, Flag, Keyframes, SLOT_COUNT, Slot};

/// The value of a stopped slot.
const STOPPED: i32 = i32::MIN;

/// Which species' state machine a track runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Species {
    /// Hops on an entity event and tilts its head at random.
    Rabbit,
    /// Flies or roosts on a metadata flag.
    Bat,
    /// Jumps, croaks and strikes on pose changes; swims in water.
    Frog,
    /// Sits, stands and dashes from a pose-change stamp.
    Camel,
    /// Rolls up, peeks and unrolls on a state ordinal.
    Armadillo,
    /// Digs, sniffs and wiggles on a state ordinal.
    Sniffer,
}

impl Species {
    /// The species an entity type path names, if it has keyframe timers.
    #[must_use]
    pub fn of(type_path: &str) -> Option<Self> {
        Some(match type_path {
            "rabbit" => Self::Rabbit,
            "bat" => Self::Bat,
            "frog" => Self::Frog,
            "camel" | "camel_husk" => Self::Camel,
            "armadillo" => Self::Armadillo,
            "sniffer" => Self::Sniffer,
            _ => return None,
        })
    }
}

/// What one tick of [`KeyframeTimers::step`] reads.
#[derive(Debug, Clone, Default)]
pub struct StepInput {
    /// The entity's pose, if it has reported one.
    pub pose: Option<EntityPose>,
    /// Its species appearance fields.
    pub appearance: MobAppearance,
    /// Entity-event bytes received since the last tick.
    pub events: Vec<u8>,
    /// Whether a lead holds it.
    pub leashed: bool,
    /// The client's game time in ticks.
    pub game_time: i64,
    /// Whether it is in water.
    pub in_water: bool,
    /// Whether its walk animation is moving.
    pub walk_moving: bool,
}

/// How a species overrides the walk animation's target amplitude.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkProfile {
    /// Multiplier on the horizontal distance moved this tick.
    pub distance_scale: f32,
    /// Smoothing toward the target per tick.
    pub smoothing: f32,
    /// Whether the animation follows movement at all this tick; when `false` the
    /// target is zero.
    pub active: bool,
}

/// A track's keyframe animation timers.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct KeyframeTimers {
    species: Species,
    tick: i32,
    started: [i32; SLOT_COUNT],
    flags: [bool; 4],
    rng: u32,
    idle_timeout: i32,
    jump_ticks: i32,
    jump_duration: i32,
    previous_pose: Option<EntityPose>,
    previous_state: u8,
    state_ticks: i64,
    peek_received: bool,
    walk: Option<WalkProfile>,
}

/// Index of a flag in [`KeyframeTimers::flags`].
fn flag_index(flag: Flag) -> usize {
    flag as usize
}

/// Vanilla's pose ordinals for the poses the shared set does not name.
const POSE_CROAKING: u32 = 8;
const POSE_USING_TONGUE: u32 = 9;

/// The rabbit's hop lasts this many ticks.
const RABBIT_HOP_TICKS: i32 = 15;
/// The entity-event byte that starts a rabbit's hop.
const RABBIT_HOP_EVENT: u8 = 1;
/// The entity-event byte that tells an armadillo to restart its peek.
const ARMADILLO_PEEK_EVENT: u8 = 64;
/// Ticks a scared armadillo's peek is fast-forwarded when the state begins.
const ARMADILLO_SCARED_TICKS: i32 = 50;

impl KeyframeTimers {
    /// Fresh timers for an entity `id` of `species`.
    #[must_use]
    pub fn new(species: Species, id: i32) -> Self {
        let mut timers = KeyframeTimers {
            species,
            tick: 0,
            started: [STOPPED; SLOT_COUNT],
            flags: [false; 4],
            rng: (id as u32).wrapping_mul(2_654_435_761) | 1,
            idle_timeout: 0,
            jump_ticks: 0,
            jump_duration: 0,
            previous_pose: None,
            previous_state: 0,
            state_ticks: 0,
            peek_received: false,
            walk: None,
        };
        // A rabbit's first idle tilt comes a random 180..220 ticks after it appears.
        if species == Species::Rabbit {
            timers.idle_timeout = timers.next_int(40) + 180;
        }
        timers
    }

    /// The species this runs.
    #[must_use]
    pub fn species(&self) -> Species {
        self.species
    }

    /// The walk-amplitude override this species applies, if any.
    #[must_use]
    pub fn walk_profile(&self) -> Option<WalkProfile> {
        self.walk
    }

    fn next_int(&mut self, bound: i32) -> i32 {
        // xorshift32: deterministic per entity, which the tests rely on.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x % bound as u32) as i32
    }

    fn start(&mut self, slot: Slot) {
        self.started[slot as usize] = self.tick;
    }

    fn start_if_stopped(&mut self, slot: Slot) {
        if !self.is_started(slot) {
            self.start(slot);
        }
    }

    fn stop(&mut self, slot: Slot) {
        self.started[slot as usize] = STOPPED;
    }

    fn animate_when(&mut self, slot: Slot, condition: bool) {
        if condition {
            self.start_if_stopped(slot);
        } else {
            self.stop(slot);
        }
    }

    fn is_started(&self, slot: Slot) -> bool {
        self.started[slot as usize] != STOPPED
    }

    /// Advances one 20 Hz tick.
    pub fn step(&mut self, input: &StepInput) {
        self.tick += 1;
        match self.species {
            Species::Rabbit => self.step_rabbit(input),
            Species::Bat => self.step_bat(input),
            Species::Frog => self.step_frog(input),
            Species::Camel => self.step_camel(input),
            Species::Armadillo => self.step_armadillo(input),
            Species::Sniffer => self.step_sniffer(input),
        }
    }

    fn step_rabbit(&mut self, input: &StepInput) {
        if input.events.contains(&RABBIT_HOP_EVENT) {
            self.jump_duration = RABBIT_HOP_TICKS;
            self.jump_ticks = 0;
        }
        if self.idle_timeout <= 0 && !input.leashed {
            self.idle_timeout = self.next_int(40) + 180;
            self.start(Slot::IdleHeadTilt);
        } else if self.jump_ticks > 0 {
            self.start_if_stopped(Slot::Hop);
            self.stop(Slot::IdleHeadTilt);
        } else {
            self.idle_timeout -= 1;
            self.stop(Slot::Hop);
        }
        if input.leashed {
            self.stop(Slot::IdleHeadTilt);
        }
        // The jump counters advance after the animation states, as they do in the
        // reference's per-tick order.
        if self.jump_ticks != self.jump_duration {
            self.jump_ticks += 1;
        } else if self.jump_duration != 0 {
            self.jump_ticks = 0;
            self.jump_duration = 0;
        }
    }

    fn step_bat(&mut self, input: &StepInput) {
        let resting = input.appearance.bat_flags.is_some_and(|flags| flags & 1 != 0);
        self.flags[flag_index(Flag::Resting)] = resting;
        if resting {
            self.stop(Slot::Fly);
            self.start_if_stopped(Slot::Rest);
        } else {
            self.stop(Slot::Rest);
            self.start_if_stopped(Slot::Fly);
        }
    }

    fn step_frog(&mut self, input: &StepInput) {
        let pose = input.pose.unwrap_or(EntityPose::Standing);
        if pose != self.previous_pose.unwrap_or(EntityPose::Standing) {
            for (slot, matches) in [
                (Slot::Jump, pose == EntityPose::LongJumping),
                (Slot::Croak, pose == EntityPose::Other(POSE_CROAKING)),
                (Slot::Tongue, pose == EntityPose::Other(POSE_USING_TONGUE)),
            ] {
                if matches {
                    self.start(slot);
                } else {
                    self.stop(slot);
                }
            }
        }
        self.previous_pose = Some(pose);
        self.animate_when(Slot::SwimIdle, input.in_water && !input.walk_moving);
        self.flags[flag_index(Flag::Swimming)] = input.in_water;
        self.walk = Some(WalkProfile {
            distance_scale: 25.0,
            smoothing: 0.4,
            active: !self.is_started(Slot::Jump),
        });
    }

    fn step_camel(&mut self, input: &StepInput) {
        let stamp = input.appearance.camel_last_pose_change_tick.unwrap_or(0);
        let sitting = stamp < 0;
        let pose_time = input.game_time - stamp.abs();
        let dashing = input.appearance.camel_dash.unwrap_or(false);
        if self.idle_timeout <= 0 {
            self.idle_timeout = self.next_int(40) + 80;
            self.start(Slot::Idle);
        } else {
            self.idle_timeout -= 1;
        }
        let visually_sitting = (pose_time < 0) != sitting;
        let in_transition = pose_time < if sitting { 40 } else { 52 };
        if visually_sitting {
            self.stop(Slot::SitUp);
            self.stop(Slot::Dash);
            if sitting && (0..40).contains(&pose_time) {
                self.start_if_stopped(Slot::Sit);
                self.stop(Slot::SitPose);
            } else {
                self.stop(Slot::Sit);
                self.start_if_stopped(Slot::SitPose);
            }
        } else {
            self.stop(Slot::Sit);
            self.stop(Slot::SitPose);
            self.animate_when(Slot::Dash, dashing);
            self.animate_when(Slot::SitUp, in_transition && pose_time >= 0);
        }
        let standing = matches!(input.pose, None | Some(EntityPose::Standing));
        self.walk = Some(WalkProfile {
            distance_scale: 6.0,
            smoothing: 0.2,
            active: standing && !self.is_started(Slot::Dash),
        });
    }

    fn step_armadillo(&mut self, input: &StepInput) {
        let state = input.appearance.armadillo_state.unwrap_or(0);
        if input.events.contains(&ARMADILLO_PEEK_EVENT) {
            self.peek_received = true;
        }
        if state != self.previous_state {
            self.previous_state = state;
            self.state_ticks = 0;
        }
        match state {
            1 => {
                self.stop(Slot::RollOut);
                self.start_if_stopped(Slot::RollUp);
                self.stop(Slot::Peek);
            }
            2 => {
                self.stop(Slot::RollOut);
                self.stop(Slot::RollUp);
                if self.peek_received {
                    self.stop(Slot::Peek);
                    self.peek_received = false;
                }
                if self.state_ticks == 0 {
                    self.start(Slot::Peek);
                    // Fast-forward: the animation has already played for the state's length.
                    self.started[Slot::Peek as usize] -= ARMADILLO_SCARED_TICKS;
                } else {
                    self.start_if_stopped(Slot::Peek);
                }
            }
            3 => {
                self.start_if_stopped(Slot::RollOut);
                self.stop(Slot::RollUp);
                self.stop(Slot::Peek);
            }
            _ => {
                self.stop(Slot::RollOut);
                self.stop(Slot::RollUp);
                self.stop(Slot::Peek);
            }
        }
        let hiding = match state {
            1 => self.state_ticks > 5,
            2 => true,
            3 => self.state_ticks < 26,
            _ => false,
        };
        self.flags[flag_index(Flag::Hiding)] = hiding;
        self.state_ticks += 1;
    }

    fn step_sniffer(&mut self, input: &StepInput) {
        let state = input.appearance.sniffer_state.unwrap_or(0);
        if state != self.previous_state {
            self.previous_state = state;
            for slot in [Slot::Dig, Slot::LongSniff, Slot::Rise, Slot::Happy, Slot::Scent] {
                self.stop(slot);
            }
            match state {
                1 => self.start_if_stopped(Slot::Happy),
                2 => self.start_if_stopped(Slot::Scent),
                3 => self.start_if_stopped(Slot::LongSniff),
                5 => self.start_if_stopped(Slot::Dig),
                6 => self.start_if_stopped(Slot::Rise),
                _ => {}
            }
        }
        self.flags[flag_index(Flag::Searching)] = state == 4;
    }

    /// The render input at `partial_tick` into the current tick.
    #[must_use]
    pub fn keyframes(&self, partial_tick: f32) -> Keyframes {
        let age = self.tick as f32 + partial_tick;
        let mut out = Keyframes::NONE;
        for slot in ALL_SLOTS {
            let start = self.started[slot as usize];
            if start != STOPPED {
                let millis = ((age - start as f32) * 50.0) as i64;
                out = out.with(slot, millis.clamp(0, i64::from(i32::MAX) - 1) as i32);
            }
        }
        for flag in [Flag::Swimming, Flag::Hiding, Flag::Searching, Flag::Resting] {
            out = out.flag(flag, self.flags[flag_index(flag)]);
        }
        out
    }
}

/// `GameTick` / `TickSet::Animate`: runs every keyframe-animated track's state machine
/// one tick, from the ingest entity's facts bridged through [`EntityIndex`].
pub fn tick_keyframe_timers(
    index: Res<EntityIndex>,
    world_time: Option<Res<lodestone_ecs::WorldTime>>,
    chunks: Option<Res<lodestone_ecs::ChunkWorld>>,
    mut game_clock: Local<(i64, i64)>,
    mut facts: Query<(
        Option<&lodestone_ecs::entity::Pose>,
        Option<&lodestone_ecs::entity::Appearance>,
        Option<&lodestone_ecs::entity::Leashed>,
        Option<&mut lodestone_ecs::entity::StatusEvents>,
    )>,
    mut tracks: Query<(
        &lodestone_ecs::entity::MinecraftEntityId,
        &mut KeyframeTimers,
        &super::InterpTo,
        &super::WalkAnim,
    )>,
) {
    // The server sends the world age about once a second; between reports the
    // client's own game time advances a tick at a time.
    let reported = world_time.as_deref().map_or(0, |t| t.age);
    if reported != game_clock.0 {
        *game_clock = (reported, reported);
    } else {
        game_clock.1 += 1;
    }
    let game_time = game_clock.1;
    for (id, mut timers, to, walk) in &mut tracks {
        let mut input = StepInput { game_time, walk_moving: walk.walk.speed() > 1.0e-5, ..StepInput::default() };
        if let Some(entity) = index.get(id.0)
            && let Ok((pose, appearance, leashed, events)) = facts.get_mut(entity)
        {
            input.pose = pose.map(|p| p.0);
            input.appearance = appearance.map(|a| a.0).unwrap_or_default();
            input.leashed = leashed.is_some_and(|l| l.0.is_some());
            if let Some(mut events) = events {
                input.events = std::mem::take(&mut events.0);
            }
        }
        if timers.species() == Species::Frog
            && let Some(chunks) = chunks.as_deref()
        {
            let at = to.feet + glam::Vec3::Y * FROG_WATER_PROBE_HEIGHT;
            let (x, y, z) = (at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32);
            input.in_water = chunks
                .read()
                .block_state_at(x, y, z)
                .and_then(lodestone_data::block_states::block_name)
                == Some("minecraft:water");
        }
        timers.step(&input);
    }
}

/// How far above a frog's feet the water probe samples, in blocks: the middle of its body.
const FROG_WATER_PROBE_HEIGHT: f32 = 0.25;

use lodestone_ecs::entity::EntityIndex;

#[cfg(test)]
mod tests {
    use super::*;

    fn run(timers: &mut KeyframeTimers, ticks: usize, input: &StepInput) {
        for _ in 0..ticks {
            timers.step(input);
        }
    }

    fn rabbit() -> KeyframeTimers {
        KeyframeTimers::new(Species::Rabbit, 7)
    }

    /// The hop event sets a 15 tick jump; the hop animation runs from the tick after
    /// the event through the last jump tick, then stops. Counted by hand from the
    /// reference's per-tick order: animation states first, then the counters.
    #[test]
    fn a_rabbit_hops_for_the_tick_window_after_its_event() {
        let mut t = rabbit();
        let event = StepInput { events: vec![1], ..StepInput::default() };
        t.step(&event); // tick 1: counters 0 -> 1 after the animation step
        assert!(!t.is_started(Slot::Hop), "the animation step precedes the counter");
        let quiet = StepInput::default();
        let mut hopping = Vec::new();
        for _ in 0..20 {
            t.step(&quiet);
            hopping.push(t.is_started(Slot::Hop));
        }
        // Steps 2..=16 see jump_ticks 1..=15; step 17 sees the reset counter.
        assert_eq!(hopping.iter().filter(|h| **h).count(), 15);
        assert!(hopping[..15].iter().all(|h| *h) && !hopping[15]);
        // Control: a rabbit that never gets the event never hops.
        let mut idle = rabbit();
        run(&mut idle, 40, &quiet);
        assert!(!idle.is_started(Slot::Hop));
    }

    #[test]
    fn a_rabbit_tilts_its_head_after_its_idle_timeout_unless_leashed() {
        let mut t = rabbit();
        let timeout = t.idle_timeout;
        assert!((180..220).contains(&timeout));
        run(&mut t, (timeout + 1) as usize, &StepInput::default());
        assert!(t.is_started(Slot::IdleHeadTilt));
        let mut leashed = rabbit();
        run(&mut leashed, 400, &StepInput { leashed: true, ..StepInput::default() });
        assert!(!leashed.is_started(Slot::IdleHeadTilt));
    }

    #[test]
    fn a_bat_flies_until_its_flag_says_it_roosts() {
        let mut t = KeyframeTimers::new(Species::Bat, 1);
        let mut appearance = MobAppearance::default();
        appearance.bat_flags = Some(0);
        t.step(&StepInput { appearance, ..StepInput::default() });
        assert!(t.is_started(Slot::Fly) && !t.is_started(Slot::Rest));
        assert!(!t.keyframes(0.0).has(Flag::Resting));
        appearance.bat_flags = Some(1);
        t.step(&StepInput { appearance, ..StepInput::default() });
        assert!(!t.is_started(Slot::Fly) && t.is_started(Slot::Rest));
        assert!(t.keyframes(0.0).has(Flag::Resting));
    }

    #[test]
    fn a_frog_starts_the_animation_its_pose_names_and_stops_the_rest() {
        let mut t = KeyframeTimers::new(Species::Frog, 3);
        let pose = |p| StepInput { pose: Some(p), ..StepInput::default() };
        t.step(&pose(EntityPose::Standing));
        t.step(&pose(EntityPose::Other(POSE_CROAKING)));
        assert!(t.is_started(Slot::Croak) && !t.is_started(Slot::Jump));
        t.step(&pose(EntityPose::LongJumping));
        assert!(t.is_started(Slot::Jump) && !t.is_started(Slot::Croak));
        assert!(!t.walk_profile().unwrap().active, "the walk is zeroed while jumping");
        t.step(&pose(EntityPose::Other(POSE_USING_TONGUE)));
        assert!(t.is_started(Slot::Tongue) && !t.is_started(Slot::Jump));
        assert!(t.walk_profile().unwrap().active);
    }

    #[test]
    fn a_frog_bobs_only_in_still_water() {
        let mut t = KeyframeTimers::new(Species::Frog, 3);
        t.step(&StepInput { in_water: true, ..StepInput::default() });
        assert!(t.is_started(Slot::SwimIdle) && t.keyframes(0.0).has(Flag::Swimming));
        t.step(&StepInput { in_water: true, walk_moving: true, ..StepInput::default() });
        assert!(!t.is_started(Slot::SwimIdle) && t.keyframes(0.0).has(Flag::Swimming));
    }

    fn camel_input(stamp: i64, game_time: i64) -> StepInput {
        let mut appearance = MobAppearance::default();
        appearance.camel_last_pose_change_tick = Some(stamp);
        StepInput { appearance, game_time, ..StepInput::default() }
    }

    /// A negative stamp is "sitting since"; the sit animation covers the first 40
    /// ticks and the held pose follows. A positive stamp is standing since, and the
    /// stand-up animation covers the first 52.
    #[test]
    fn a_camel_sits_then_holds_and_stands_up_then_walks() {
        let mut t = KeyframeTimers::new(Species::Camel, 5);
        t.step(&camel_input(-100, 110));
        assert!(t.is_started(Slot::Sit) && !t.is_started(Slot::SitPose));
        t.step(&camel_input(-100, 150));
        assert!(!t.is_started(Slot::Sit) && t.is_started(Slot::SitPose));
        let mut up = KeyframeTimers::new(Species::Camel, 5);
        up.step(&camel_input(100, 120));
        assert!(up.is_started(Slot::SitUp) && !up.is_started(Slot::SitPose));
        up.step(&camel_input(100, 200));
        assert!(!up.is_started(Slot::SitUp));
        assert!(up.walk_profile().unwrap().active);
    }

    #[test]
    fn a_camel_dash_follows_its_flag_while_standing() {
        let mut t = KeyframeTimers::new(Species::Camel, 5);
        let mut input = camel_input(100, 1000);
        input.appearance.camel_dash = Some(true);
        t.step(&input);
        assert!(t.is_started(Slot::Dash) && !t.walk_profile().unwrap().active);
        input.appearance.camel_dash = Some(false);
        t.step(&input);
        assert!(!t.is_started(Slot::Dash));
    }

    fn armadillo(state: u8) -> StepInput {
        let mut appearance = MobAppearance::default();
        appearance.armadillo_state = Some(state);
        StepInput { appearance, ..StepInput::default() }
    }

    #[test]
    fn an_armadillo_hides_its_shell_parts_on_the_reference_schedule() {
        let mut t = KeyframeTimers::new(Species::Armadillo, 9);
        // Rolling: hidden once more than 5 ticks into the state.
        let mut hidden = Vec::new();
        for _ in 0..8 {
            t.step(&armadillo(1));
            hidden.push(t.keyframes(0.0).has(Flag::Hiding));
        }
        assert_eq!(hidden, [false, false, false, false, false, false, true, true]);
        assert!(t.is_started(Slot::RollUp));
        // Scared: hidden throughout, and the peek starts already 50 ticks in.
        let mut s = KeyframeTimers::new(Species::Armadillo, 9);
        s.step(&armadillo(2));
        assert!(s.keyframes(0.0).has(Flag::Hiding));
        assert_eq!(s.keyframes(0.0).elapsed(Slot::Peek), Some(((1.0 + 50.0) * 50.0) as i32 - 50));
        // Unrolling: hidden for the first 26 ticks only.
        let mut u = KeyframeTimers::new(Species::Armadillo, 9);
        let seen: Vec<bool> = (0..28)
            .map(|_| {
                u.step(&armadillo(3));
                u.keyframes(0.0).has(Flag::Hiding)
            })
            .collect();
        assert!(seen[..26].iter().all(|h| *h) && !seen[26] && !seen[27]);
        // Control: idle never hides.
        let mut idle = KeyframeTimers::new(Species::Armadillo, 9);
        run(&mut idle, 10, &armadillo(0));
        assert!(!idle.keyframes(0.0).has(Flag::Hiding));
    }

    #[test]
    fn a_sniffer_state_change_replaces_the_running_animation() {
        let mut t = KeyframeTimers::new(Species::Sniffer, 2);
        let state = |s: u8| {
            let mut appearance = MobAppearance::default();
            appearance.sniffer_state = Some(s);
            StepInput { appearance, ..StepInput::default() }
        };
        t.step(&state(5));
        assert!(t.is_started(Slot::Dig));
        t.step(&state(6));
        assert!(!t.is_started(Slot::Dig) && t.is_started(Slot::Rise));
        t.step(&state(4));
        assert!(!t.is_started(Slot::Rise) && t.keyframes(0.0).has(Flag::Searching));
        t.step(&state(0));
        assert!(!t.keyframes(0.0).has(Flag::Searching));
    }

    #[test]
    fn elapsed_milliseconds_advance_with_the_partial_tick() {
        let mut t = KeyframeTimers::new(Species::Bat, 1);
        t.step(&StepInput::default());
        let a = t.keyframes(0.0).elapsed(Slot::Fly).unwrap();
        let b = t.keyframes(0.5).elapsed(Slot::Fly).unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 25);
    }
}
