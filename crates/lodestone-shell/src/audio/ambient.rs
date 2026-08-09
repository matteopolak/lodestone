//! Driving vanilla's `BiomeAmbientSoundsHandler` and `ClientLevel`'s rain
//! cadence from the shell — the call sites `#183` built the state machines for
//! and never had.
//!
//! # What this is
//!
//! [`ShellAmbience`] owns the four pieces of per-session ambience state:
//! [`MoodAccumulator`] (cave ambience), [`AmbientLoops`] (the biome/dimension
//! loop with its 40-tick crossfade), [`RainAmbience`] (the repeated one-shot
//! `ClientLevel.tickWeatherEffects` drives) and the [`JavaRandom`] all three draw
//! from. It is ticked at 20 Hz from the render loop.
//!
//! # Why the tick is pure and playing is a second step
//!
//! [`ShellAmbience::tick`] returns [`AmbienceEvent`]s and touches no device, so a
//! test can drive an exact number of ticks with a synthetic light probe and
//! assert what would be played. [`ShellAmbience::submit`] is the half that needs
//! a [`ShellAudio`], and is also where the loop **voice table** lives — a loop
//! handle has to outlive the tick that started it, which is why it is a field
//! here and not a return value.
//!
//! Splitting it this way is also what keeps this module from being able to make a
//! sound under `cargo test`: nothing in `tick` can reach a device, so there is no
//! interception to get wrong (contrast [`super::music`], whose sink *is* the play
//! path and therefore forks on `#[cfg(test)]`).
//!
//! # The trigger condition for cave ambience is darkness, not depth
//!
//! [`MoodAccumulator`]'s doc has the arithmetic; the consequence for this module
//! is that the light probe has to be a real per-tick world read at a *randomly
//! sampled* block, not a cached value at the player. A probe that answered "the
//! light at the eye" would make moodiness a function of the player's own torch
//! and cave ambience would essentially never fire.

use std::borrow::Cow;
use std::time::Duration;

// The portable clock: `Instant::now()` traps on wasm32. `crate::platform::Instant`
// *is* `std::time::Instant` on native (`web_time` re-exports `std::time` there), so
// this changes nothing off the browser. See `crate::platform`.
use crate::platform::Instant;

use glam::{DVec3, IVec3};
use lodestone_render::{RainAmbience, WeatherState};
use lodestone_sound::JavaRandom;
use lodestone_sound::ambient::{AmbientLoops, AmbientSounds, LightSample, LoopAction};
use lodestone_sound::predict::{PredictionLedger, StepAccumulator};

use super::ShellAudio;

/// Real time per client tick — vanilla's 20 Hz, the same constant
/// [`super::music`] uses for the same reason.
const TICK: Duration = Duration::from_millis(50);

/// Cap on catch-up ticks in one call. Ten, matching `app::pacing` and
/// [`super::music`]: alt-tabbing away must not run five minutes of mood
/// accumulation in one frame.
const MAX_CATCH_UP_TICKS: u32 = 10;

/// Vanilla's `rainSoundTime` roll bound — `ClientLevel.java:385`
/// (`random.nextInt(3)`).
const RAIN_ROLL_BOUND: i32 = 3;

/// Everything one ambience tick needs from the world, gathered by the caller.
///
/// Borrowed rather than owned so a frame can build it from live reads without a
/// clone; it must not outlive the frame it was built for, for the same staleness
/// reason `app::weather`'s `ShellWeatherProbe` must not.
pub(crate) struct AmbienceInput<'a> {
    /// The player's `(x, eyeY, z)` — the centre of the mood sample cube.
    pub(crate) eye: DVec3,
    /// The [`AmbientSounds`] in force here, already resolved through
    /// `AmbientSounds::resolve` (biome overrides dimension, never merges).
    pub(crate) ambient: &'a AmbientSounds,
    /// The world's weather, or `None` off a live session.
    pub(crate) weather: Option<&'a WeatherState>,
    /// Where rain is landing near the player (vanilla's `rainParticlePosition`).
    pub(crate) landing: Option<[i32; 3]>,
    /// Whether there is something over the player's own ear.
    pub(crate) roof_above: bool,
}

/// One thing an ambience tick decided should happen.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AmbienceEvent {
    /// A positional one-shot: cave mood, an ambient addition, or rain.
    OneShot {
        /// Namespace-stripped event key.
        name: Cow<'static, str>,
        /// World position.
        position: DVec3,
        /// Volume, already carrying vanilla's per-source scaling.
        volume: f32,
        /// Pitch.
        pitch: f32,
    },
    /// Start a looping, head-relative voice at volume 0.
    LoopStart(Cow<'static, str>),
    /// Ramp a live loop.
    LoopVolume {
        /// Which loop.
        sound: Cow<'static, str>,
        /// Its new volume.
        volume: f32,
    },
    /// Stop and forget a loop.
    LoopStop(Cow<'static, str>),
}

/// The shell's ambience state.
#[derive(Debug)]
pub(crate) struct ShellAmbience {
    mood: lodestone_sound::ambient::MoodAccumulator,
    loops: AmbientLoops,
    rain: RainAmbience,
    rng: JavaRandom,
    /// Live loop voices, so a handle outlives the tick that started it.
    voices: Vec<(String, lodestone_sound::PlayHandle)>,
    last_tick: Option<Instant>,
    /// The local player's step-distance accumulator, and the echo ledger guarding
    /// the sounds it predicts. Here rather than in a resource of their own because
    /// they are the same "per-session, config-scoped audio state" as everything
    /// else in this struct, and a second `Option<_>` move-out slot would double
    /// the ceremony for nothing.
    steps: StepAccumulator,
    ledger: PredictionLedger,
}

impl ShellAmbience {
    /// Fresh ambience state. `seed` seeds the shared RNG.
    pub(crate) fn new(seed: i64) -> Self {
        Self {
            mood: lodestone_sound::ambient::MoodAccumulator::new(),
            loops: AmbientLoops::new(),
            rain: RainAmbience::default(),
            rng: JavaRandom::new(seed),
            voices: Vec::new(),
            last_tick: None,
            steps: StepAccumulator::new(),
            ledger: PredictionLedger::new(),
        }
    }

    /// Accumulate one physics tick's achieved movement, returning whether a
    /// footstep should sound. The caller resolves the block's own sound type and
    /// then calls [`Self::record_step`] — the split is vanilla's, which re-arms
    /// the threshold only when a sound was actually produced.
    pub(crate) fn advance_step(
        &mut self,
        moved: DVec3,
        climbing: bool,
        supporting_is_air: bool,
    ) -> bool {
        self.steps.advance(moved, climbing, supporting_is_air)
    }

    /// Re-arm the step threshold and record the prediction for echo suppression.
    pub(crate) fn record_step(&mut self, name: &'static str, position: glam::Vec3, tick: u64) {
        self.steps.consume();
        self.ledger.record(name, position, tick);
    }

    /// Whether an incoming server sound matches a prediction and must be dropped.
    pub(crate) fn should_suppress(
        &mut self,
        name: &str,
        position: glam::Vec3,
        tick: u64,
    ) -> bool {
        self.ledger.should_suppress(name, position, tick)
    }

    /// Current moodiness, `0.0..=1.0`. Vanilla shows this on the debug screen.
    pub(crate) fn moodiness(&self) -> f32 {
        self.mood.moodiness()
    }

    /// The loops currently live, with their crossfade volumes.
    pub(crate) fn live_loops(&self) -> Vec<(String, f32)> {
        self.loops
            .live()
            .map(|(n, v)| (n.to_string(), v))
            .collect()
    }

    /// Advance exactly `ticks` client ticks, returning what to play.
    ///
    /// Device-free: `probe` is the caller's world read, and nothing here can
    /// reach a [`ShellAudio`].
    pub(crate) fn tick(
        &mut self,
        ticks: u32,
        input: &AmbienceInput<'_>,
        probe: &mut impl FnMut(IVec3) -> LightSample,
    ) -> Vec<AmbienceEvent> {
        let mut events = Vec::new();
        for _ in 0..ticks {
            self.tick_once(input, probe, &mut events);
        }
        events
    }

    fn tick_once(
        &mut self,
        input: &AmbienceInput<'_>,
        probe: &mut impl FnMut(IVec3) -> LightSample,
        events: &mut Vec<AmbienceEvent>,
    ) {
        // The loop half first, matching `BiomeAmbientSoundsHandler.tick`'s own
        // order (`:43-62` before the mood and additions at `:64-107`) — it is the
        // order the RNG draws happen in, so swapping it would desynchronise the
        // additions' stream from the jar's.
        for action in self.loops.tick(input.ambient.loop_sound.as_deref()) {
            events.push(match action {
                LoopAction::Start(sound) => AmbienceEvent::LoopStart(sound),
                LoopAction::SetVolume { sound, volume } => {
                    AmbienceEvent::LoopVolume { sound, volume }
                }
                LoopAction::Stop(sound) => AmbienceEvent::LoopStop(sound),
            });
        }

        for addition in input.ambient.additions.iter() {
            if addition.fires(&mut self.rng) {
                events.push(AmbienceEvent::OneShot {
                    name: addition.sound.clone(),
                    position: input.eye,
                    volume: 1.0,
                    pitch: 1.0,
                });
            }
        }

        if let Some(mood) = input.ambient.mood.as_ref()
            && let Some(play) = self.mood.tick(mood, input.eye, &mut self.rng, probe)
        {
            events.push(AmbienceEvent::OneShot {
                name: play.sound,
                position: play.position,
                volume: 1.0,
                pitch: 1.0,
            });
        }

        // Rain, from `ClientLevel.tickWeatherEffects` rather than the biome
        // handler. The roll is taken unconditionally so the stream does not
        // depend on whether it is raining — vanilla reaches `nextInt(3)` only
        // under rain, but our `RainAmbience::tick` takes the roll as an
        // argument, so drawing it here keeps the call shape single-branch.
        let roll = self.rng.next_i32_bound(RAIN_ROLL_BOUND).max(0) as u32;
        if let Some(weather) = input.weather
            && let Some(sound) = self.rain.tick(
                weather,
                input.landing,
                input.eye.y,
                input.roof_above,
                roll,
            )
        {
            events.push(AmbienceEvent::OneShot {
                name: Cow::Borrowed(sound.name),
                // Vanilla plays this at the *listener*, relative, not at the
                // landing block — `ClientLevel.java:388-392` passes the camera
                // position.
                position: input.eye,
                volume: sound.volume,
                pitch: sound.pitch,
            });
        }
    }

    /// Tick from wall time: however many whole 20 Hz ticks have elapsed since the
    /// last call, capped at [`MAX_CATCH_UP_TICKS`].
    pub(crate) fn advance(
        &mut self,
        now: Instant,
        input: &AmbienceInput<'_>,
        probe: &mut impl FnMut(IVec3) -> LightSample,
    ) -> Vec<AmbienceEvent> {
        let Some(last) = self.last_tick else {
            self.last_tick = Some(now);
            return self.tick(1, input, probe);
        };
        let elapsed = now.saturating_duration_since(last);
        let ticks = u32::try_from(elapsed.as_millis() / TICK.as_millis()).unwrap_or(u32::MAX);
        if ticks == 0 {
            return Vec::new();
        }
        let ticks = ticks.min(MAX_CATCH_UP_TICKS);
        self.last_tick = Some(last + TICK * ticks);
        self.tick(ticks, input, probe)
    }

    /// Apply a tick's events to the device, maintaining the loop voice table.
    ///
    /// A `LoopStart` whose sound does not resolve (the ordinary case with no
    /// `.ogg` corpus on disk) simply records no voice, and the subsequent
    /// `LoopVolume`/`LoopStop` for it are no-ops — so a missing sample degrades
    /// to silence rather than to a leaked table entry.
    pub(crate) fn submit(&mut self, events: &[AmbienceEvent], audio: &mut ShellAudio) {
        for event in events {
            match event {
                AmbienceEvent::OneShot {
                    name,
                    position,
                    volume,
                    pitch,
                } => audio.play_sound(
                    name,
                    lodestone_model::event::SoundCategory::Ambient,
                    position.as_vec3(),
                    *volume,
                    *pitch,
                    0,
                ),
                AmbienceEvent::LoopStart(sound) => {
                    if let Some(handle) = audio.start_loop(sound, 0.0) {
                        self.voices.push((sound.to_string(), handle));
                    }
                }
                AmbienceEvent::LoopVolume { sound, volume } => {
                    if let Some((_, handle)) = self.voices.iter().find(|(n, _)| n == sound) {
                        audio.set_loop_volume(*handle, *volume);
                    }
                }
                AmbienceEvent::LoopStop(sound) => {
                    if let Some(index) = self.voices.iter().position(|(n, _)| n == sound) {
                        let (_, handle) = self.voices.remove(index);
                        audio.stop_loop(handle);
                    }
                }
            }
        }
    }
}

/// Vanilla's step volume/pitch for a block's `SoundType` —
/// `Entity.playStepSound` (`Entity.java:1473`): `volume * 0.15`, pitch as-is.
///
/// Split out here rather than inlined at the call site so the two multipliers
/// live beside the rest of the ambience constants and are not retyped.
#[must_use]
pub(crate) fn step_volume(sound_type_volume: f32) -> f32 {
    sound_type_volume * lodestone_sound::predict::STEP_VOLUME_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_sound::ambient::{AmbientAdditionsSettings, AmbientMoodSettings};

    /// A probe reporting pitch darkness everywhere — the state cave ambience
    /// accumulates in.
    fn dark(_: IVec3) -> LightSample {
        LightSample { sky: 0, block: 0 }
    }

    /// A probe reporting full sky light — the state that drains moodiness.
    fn daylight(_: IVec3) -> LightSample {
        LightSample { sky: 15, block: 0 }
    }

    fn cave_input(eye: DVec3, ambient: &AmbientSounds) -> AmbienceInput<'_> {
        AmbienceInput {
            eye,
            ambient,
            weather: None,
            landing: None,
            roof_above: false,
        }
    }

    /// Cave ambience fires after vanilla's 6000 dark ticks and not before, and
    /// **daylight never fires it at all**.
    ///
    /// The second half is the control that matters: an accumulator wired to depth
    /// or to a timer would fire in both arms.
    #[test]
    fn cave_ambience_needs_pitch_darkness_and_the_full_mood_delay() {
        let ambient = AmbientSounds::LEGACY_CAVE;
        let delay = u32::try_from(AmbientMoodSettings::LEGACY_CAVE.tick_delay).expect("positive");

        let mut early = ShellAmbience::new(0);
        let events = early.tick(delay - 1, &cave_input(DVec3::new(0.0, 40.0, 0.0), &ambient), &mut dark);
        assert!(
            events.is_empty(),
            "one tick short of the {delay}-tick mood delay must be silent, got {events:?}"
        );

        // `delay + 2`, not `delay`: moodiness accumulates `1/6000` per tick in f32,
        // so 6000 additions land a hair *below* 1.0 and the fire is one tick late.
        // The boundary that matters is the one asserted above — a tick short of the
        // delay must be silent — not the exact tick the rounding puts it on.
        let mut ready = ShellAmbience::new(0);
        let events = ready.tick(
            delay + 2,
            &cave_input(DVec3::new(0.0, 40.0, 0.0), &ambient),
            &mut dark,
        );
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AmbienceEvent::OneShot { name, .. } => Some(name.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["ambient.cave"],
            "the mood-delay tick must play ambient.cave exactly once"
        );

        let mut lit = ShellAmbience::new(0);
        let events = lit.tick(delay * 2, &cave_input(DVec3::new(0.0, 40.0, 0.0), &ambient), &mut daylight);
        assert!(
            events.is_empty(),
            "sky light drains moodiness, so twice the delay in daylight must still \
             be silent — if this fires, the trigger is depth or time, not darkness"
        );
        assert_eq!(lit.moodiness(), 0.0, "moodiness is floored at zero");
    }

    /// A biome loop starts once and crossfades in over 40 ticks, and walking into
    /// a second biome keeps both voices while the first fades out.
    #[test]
    fn a_biome_loop_starts_once_and_crossfades_when_the_biome_changes() {
        let first = AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.crimson_forest.loop")),
            mood: None,
            additions: Cow::Borrowed(&[]),
        };
        let mut ambience = ShellAmbience::new(0);
        let events = ambience.tick(40, &cave_input(DVec3::ZERO, &first), &mut dark);
        let starts = events
            .iter()
            .filter(|e| matches!(e, AmbienceEvent::LoopStart(_)))
            .count();
        assert_eq!(starts, 1, "one loop, one start — got {events:?}");
        let live = ambience.live_loops();
        assert_eq!(live.len(), 1);
        assert_eq!(
            live[0].1, 1.0,
            "40 ticks is the full crossfade, so the loop must be at full volume"
        );

        let second = AmbientSounds {
            loop_sound: Some(Cow::Borrowed("ambient.warped_forest.loop")),
            mood: None,
            additions: Cow::Borrowed(&[]),
        };
        let events = ambience.tick(1, &cave_input(DVec3::ZERO, &second), &mut dark);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AmbienceEvent::LoopStart(s) if s == "ambient.warped_forest.loop")),
            "the new biome's loop must start: {events:?}"
        );
        assert_eq!(
            ambience.live_loops().len(),
            2,
            "the outgoing loop must survive its fade-out rather than being cut"
        );
    }

    /// An empty `AmbientSounds` produces nothing at all — the control that the
    /// events above come from the *data* and not from the act of ticking.
    #[test]
    fn control_empty_ambient_sounds_produce_nothing() {
        let empty = AmbientSounds::EMPTY;
        let mut ambience = ShellAmbience::new(0);
        let events = ambience.tick(6_000, &cave_input(DVec3::ZERO, &empty), &mut dark);
        assert!(events.is_empty(), "got {events:?}");
    }

    /// Additions fire on their flat per-tick probability. `1.0` is the boundary
    /// vanilla's strict `<` makes always-true.
    #[test]
    fn additions_fire_on_their_flat_probability() {
        static ALWAYS: &[AmbientAdditionsSettings] =
            &[AmbientAdditionsSettings::of("ambient.basalt_deltas.additions", 1.0)];
        let ambient = AmbientSounds {
            loop_sound: None,
            mood: None,
            additions: Cow::Borrowed(ALWAYS),
        };
        let mut ambience = ShellAmbience::new(0);
        let events = ambience.tick(3, &cave_input(DVec3::ZERO, &ambient), &mut dark);
        assert_eq!(
            events.len(),
            3,
            "a tick_chance of 1.0 fires every tick: {events:?}"
        );
    }

    /// Rain plays repeatedly while it is raining and never on a dry tick.
    #[test]
    fn rain_ambience_plays_only_while_it_is_raining() {
        let ambient = AmbientSounds::EMPTY;
        let mut weather = WeatherState::clear();
        weather.apply_rain_level(1.0);

        let mut wet = ShellAmbience::new(0);
        let events = wet.tick(
            40,
            &AmbienceInput {
                eye: DVec3::new(0.0, 70.0, 0.0),
                ambient: &ambient,
                weather: Some(&weather),
                landing: Some([0, 70, 0]),
                roof_above: false,
            },
            &mut dark,
        );
        assert!(
            events.iter().any(
                |e| matches!(e, AmbienceEvent::OneShot { name, .. } if name == "weather.rain")
            ),
            "40 rainy ticks must produce at least one rain play: {events:?}"
        );

        let dry = WeatherState::clear();
        let mut clear = ShellAmbience::new(0);
        let events = clear.tick(
            40,
            &AmbienceInput {
                eye: DVec3::new(0.0, 70.0, 0.0),
                ambient: &ambient,
                weather: Some(&dry),
                landing: Some([0, 70, 0]),
                roof_above: false,
            },
            &mut dark,
        );
        assert!(events.is_empty(), "a dry sky must be silent: {events:?}");
    }
}
