//! Ambient sound loops: the biome/dimension loop, the randomised "mood" sound
//! (cave ambience), and the per-tick "additions".
//!
//! Like [`crate::music`], this is a device-free, clock-free state machine — it
//! decides *what* to play and *where*, and the caller plays it. Nothing here can
//! open an output device or read the clock, and
//! `tests/ambient_loops.rs::the_ambient_module_cannot_reach_a_device_or_a_clock`
//! enforces that rather than merely asserting it.
//!
//! # The structure is not what it looks like from the outside
//!
//! Three things about 26.2 that are easy to get wrong, all of which the issue this
//! closes guessed differently:
//!
//! **1. Cave ambience is attached to the *dimension*, not the biome.**
//! `AmbientSounds.LEGACY_CAVE_SETTINGS` is set as an `EnvironmentAttributes`
//! `AMBIENT_SOUNDS` value on the **overworld** dimension type
//! (`DimensionTypes.java:43`) and on **the End** (`DimensionTypes.java:125`). The
//! Nether dimension sets *nothing*; instead its five biomes each override the
//! attribute with their own loop + mood + additions (`NetherBiomes.java:67`, `:111`,
//! `:153`, `:191`, `:230`). So a per-biome-only lookup finds cave ambience in zero
//! biomes and concludes it does not exist, and a dimension-only lookup silences the
//! entire Nether. Both layers are needed — see [`AmbientSounds::resolve`].
//!
//! **2. The trigger is not "Y below sea level".** It is a *moodiness accumulator*
//! over randomly sampled nearby blocks, described in [`MoodAccumulator`]. A player
//! standing in a lit room at Y=-40 accumulates nothing; a player in an unlit box at
//! Y=200 accumulates at full rate.
//!
//! **3. The loop is a real looping voice with a 40-tick crossfade**
//! ([`LoopFade`]), not a repeated one-shot. The mood and additions *are* one-shots.
//! Conflating them either produces a stuttering loop or a mood sound that never
//! stops.
//!
//! # What is deliberately not here
//!
//! **Rain.** Issue #183 scopes the weather loop to land with the rain-level state of
//! issue #25, and a correct primitive for it **already exists and is unwired**:
//! `lodestone_render::weather::RainAmbience`, which carries the exact
//! `ClientLevel.java:388-392` constants (`weather.rain` at 0.2/1.0,
//! `weather.rain.above` at 0.1/0.5), vanilla's post-increment `rainSoundTime`
//! counter, and the exact `landing.y > camera.y + 1 && heightmap > floor(camera.y)`
//! conjunction. Writing a second one here would be the duplicate-subsystem mistake
//! this repo warns about; it needs a *caller*, not a reimplementation.
//!
//! **Underwater ambience.** `LocalPlayer.java:1186`/`:1191` play
//! `ambient.underwater.enter`/`.exit` via `playLocalSound` on the water-state
//! transition, and there is a separate `UnderwaterAmbientSoundHandler` for the loop.
//! That is a distinct handler from this one and is left for whoever wires the
//! water-state edge.

use std::borrow::Cow;

use glam::{DVec3, IVec3};
use lodestone_audio::JavaRandom;

/// `BiomeAmbientSoundsHandler.LOOP_SOUND_CROSS_FADE_TIME` — `:22`. Ticks for a
/// loop to fade fully in or out.
pub const LOOP_SOUND_CROSS_FADE_TIME: i32 = 40;

/// `BiomeAmbientSoundsHandler.SKY_MOOD_RECOVERY_RATE` — `:23`. How fast sky light
/// *drains* accumulated moodiness.
pub const SKY_MOOD_RECOVERY_RATE: f32 = 0.001;

/// Vanilla's maximum sky/block light level, the divisor in the sky-recovery term
/// (`BiomeAmbientSoundsHandler.java:81`).
pub const MAX_LIGHT: f32 = 15.0;

/// One randomised "mood" sound — vanilla's `AmbientMoodSettings` record
/// (`AmbientMoodSettings.java:9`).
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientMoodSettings {
    /// Namespace-stripped event key, e.g. `ambient.cave`.
    pub sound: Cow<'static, str>,
    /// Ticks of complete darkness needed to reach full moodiness. Also the divisor
    /// in the block-light term, which is why it is not merely a countdown.
    pub tick_delay: i32,
    /// Half-width of the cube of blocks sampled around the player.
    pub block_search_extent: i32,
    /// Extra distance pushed *beyond* the sampled block, so the sound seems to come
    /// from further away than the block it was derived from.
    pub sound_position_offset: f64,
}

impl AmbientMoodSettings {
    /// A settings value with a `'static` name, usable in a `const`.
    pub const fn of(
        sound: &'static str,
        tick_delay: i32,
        block_search_extent: i32,
        sound_position_offset: f64,
    ) -> Self {
        Self {
            sound: Cow::Borrowed(sound),
            tick_delay,
            block_search_extent,
            sound_position_offset,
        }
    }

    /// `AmbientMoodSettings.LEGACY_CAVE_SETTINGS` — `AmbientMoodSettings.java:19`:
    /// `(ambient.cave, 6000, 8, 2.0)`.
    pub const LEGACY_CAVE: Self = Self::of("ambient.cave", 6_000, 8, 2.0);

    /// The side length of the sampled cube: `extent * 2 + 1`
    /// (`BiomeAmbientSoundsHandler.java:73`).
    pub const fn search_span(&self) -> i32 {
        self.block_search_extent * 2 + 1
    }
}

/// A sound with a flat per-tick probability — vanilla's `AmbientAdditionsSettings`
/// (`AmbientAdditionsSettings.java:8`).
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientAdditionsSettings {
    /// Namespace-stripped event key.
    pub sound: Cow<'static, str>,
    /// Probability per tick. Real values are around `0.0111`, i.e. roughly once
    /// every 90 ticks.
    pub tick_chance: f64,
}

impl AmbientAdditionsSettings {
    /// A settings value with a `'static` name, usable in a `const`.
    pub const fn of(sound: &'static str, tick_chance: f64) -> Self {
        Self {
            sound: Cow::Borrowed(sound),
            tick_chance,
        }
    }

    /// `BiomeAmbientSoundsHandler.java:65` — fires on
    /// `random.nextDouble() < tick_chance`.
    ///
    /// Strictly `<`, so a `tick_chance` of `0.0` never fires even though
    /// `next_f64` can return exactly `0.0`.
    pub fn fires(&self, rng: &mut JavaRandom) -> bool {
        rng.next_f64() < self.tick_chance
    }
}

/// The whole ambient-sound attribute value — vanilla's `AmbientSounds` record
/// (`AmbientSounds.java:11`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AmbientSounds {
    /// A continuously looping, head-relative sound.
    pub loop_sound: Option<Cow<'static, str>>,
    /// The randomised darkness-driven one-shot.
    pub mood: Option<AmbientMoodSettings>,
    /// Flat-probability one-shots. Vanilla's codec is a *compact list*
    /// (`AmbientSounds.java:20`), so the JSON is either a single object or an array;
    /// the generator normalises both to a list.
    ///
    /// [`Cow`] rather than `Vec` so the generated biome table can be a `static` —
    /// a `Vec` with elements is not const-constructible, and that constraint is the
    /// only reason this is not the plainer type.
    pub additions: Cow<'static, [AmbientAdditionsSettings]>,
}

impl AmbientSounds {
    /// `AmbientSounds.EMPTY` — `AmbientSounds.java:12`, the attribute default.
    pub const EMPTY: Self = Self {
        loop_sound: None,
        mood: None,
        additions: Cow::Borrowed(&[]),
    };

    /// `AmbientSounds.LEGACY_CAVE_SETTINGS` — `AmbientSounds.java:13-15`: the cave
    /// mood and nothing else. This is what the **overworld and End dimension types**
    /// carry (`DimensionTypes.java:43`, `:125`).
    pub const LEGACY_CAVE: Self = Self {
        loop_sound: None,
        mood: Some(AmbientMoodSettings::LEGACY_CAVE),
        additions: Cow::Borrowed(&[]),
    };

    /// [`AmbientSounds::LEGACY_CAVE`], as a function for call sites that read better
    /// that way.
    pub fn legacy_cave() -> Self {
        Self::LEGACY_CAVE
    }

    /// Whether this value would produce nothing at all.
    pub fn is_empty(&self) -> bool {
        self.loop_sound.is_none() && self.mood.is_none() && self.additions.is_empty()
    }

    /// Resolves the value in force at the player, given the biome's attribute (if
    /// it sets one) and the dimension's.
    ///
    /// Environment attributes **override rather than merge**: a biome that sets
    /// `audio/ambient_sounds` replaces the dimension's value wholesale, which is why
    /// the Nether biomes lose the cave mood and gain their own. Merging them instead
    /// would give every Nether biome cave ambience on top of its own loop, which is
    /// audibly wrong and is the mistake this method exists to prevent.
    pub fn resolve(biome: Option<&Self>, dimension: &Self) -> Self {
        biome.cloned().unwrap_or_else(|| dimension.clone())
    }
}

/// A sampled block's light levels, as `Level.getBrightness` reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightSample {
    /// `LightLayer.SKY` brightness at the sampled block, `0..=15`.
    pub sky: i32,
    /// `LightLayer.BLOCK` brightness, `0..=15`.
    pub block: i32,
}

/// A mood sound the caller should play, with the position already computed.
#[derive(Debug, Clone, PartialEq)]
pub struct MoodPlay {
    /// Namespace-stripped event key.
    pub sound: Cow<'static, str>,
    /// World position, pushed `sound_position_offset` blocks *past* the sampled
    /// block along the same direction.
    pub position: DVec3,
}

/// Vanilla's `moodiness` accumulator — `BiomeAmbientSoundsHandler.java:70-107`.
///
/// # The trigger condition, which is not what it is usually assumed to be
///
/// Every tick, one block is sampled uniformly from a `span³` cube centred on the
/// player's *eye* (`span = extent * 2 + 1`, so `17³` for the cave settings), and:
///
/// * if that block sees **any** sky light, moodiness *drops* by
///   `sky / 15 * 0.001`;
/// * otherwise moodiness changes by `-(block_light - 1) / tick_delay`.
///
/// Read the second term carefully, because its sign flips: at block light `0` it is
/// `+1/6000`, at block light `1` it is exactly `0`, and above `1` it is **negative**.
/// So moodiness only accumulates in *pitch darkness*, needing `tick_delay` = 6000
/// consecutive fully-dark samples — five minutes — and a single torch nearby
/// actively drains it.
///
/// This is why "Y below sea level" is the wrong model: depth is irrelevant, darkness
/// is everything, and the sampled cube means a *nearby* dark pocket contributes even
/// when the player stands in light.
///
/// At `moodiness >= 1.0` the sound plays and the accumulator resets to `0.0`;
/// otherwise it is floored at `0.0` (`:105`), so drainage cannot bank negative
/// credit that would delay the next sound.
#[derive(Debug, Clone, Default)]
pub struct MoodAccumulator {
    moodiness: f32,
}

impl MoodAccumulator {
    /// A fresh accumulator at zero moodiness.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current moodiness, in `0.0..=1.0`. Exposed because vanilla shows it on the
    /// debug screen (`DebugEntrySoundMood`), which makes it a legitimate readout
    /// rather than just test surface.
    pub fn moodiness(&self) -> f32 {
        self.moodiness
    }

    /// Force the accumulator, for tests and for the debug screen's controls.
    pub fn set_moodiness(&mut self, moodiness: f32) {
        self.moodiness = moodiness;
    }

    /// Advance one tick.
    ///
    /// `eye` is the player's `(x, eyeY, z)`. `rng` supplies the three
    /// `nextInt(span)` draws vanilla takes, **in x, y, z order** — the order matters
    /// for stream parity. `probe` returns the light levels at a sampled block
    /// position; the caller owns the world lookup, so this stays hermetic.
    ///
    /// Returns the sound to play, or `None`.
    pub fn tick(
        &mut self,
        mood: &AmbientMoodSettings,
        eye: DVec3,
        rng: &mut JavaRandom,
        probe: &mut impl FnMut(IVec3) -> LightSample,
    ) -> Option<MoodPlay> {
        let span = mood.search_span();
        let extent = mood.block_search_extent;

        // `BlockPos.containing` floors, and the three draws happen in x, y, z order
        // (`:74-78`). Note the y draw is around the *eye*, not the feet.
        let sample = IVec3::new(
            (eye.x + f64::from(rng.next_i32_bound(span) - extent)).floor() as i32,
            (eye.y + f64::from(rng.next_i32_bound(span) - extent)).floor() as i32,
            (eye.z + f64::from(rng.next_i32_bound(span) - extent)).floor() as i32,
        );

        let light = probe(sample);
        if light.sky > 0 {
            self.moodiness -= light.sky as f32 / MAX_LIGHT * SKY_MOOD_RECOVERY_RATE;
        } else {
            // Integer subtraction *then* float divide, as vanilla writes it
            // (`:83`). At block light 0 this ADDS 1/tick_delay.
            self.moodiness -= (light.block - 1) as f32 / mood.tick_delay as f32;
        }

        if self.moodiness >= 1.0 {
            let centre = DVec3::new(
                f64::from(sample.x) + 0.5,
                f64::from(sample.y) + 0.5,
                f64::from(sample.z) + 0.5,
            );
            let direction = centre - eye;
            let distance = direction.length();
            let source_distance = distance + mood.sound_position_offset;
            self.moodiness = 0.0;

            // A sample landing exactly on the eye would divide by zero. Vanilla can
            // hit this (the cube includes the player's own block) and produces NaN;
            // we fall back to the sampled centre, which is the same audible result
            // without poisoning the mixer's position maths.
            let position = if distance > 0.0 {
                eye + direction / distance * source_distance
            } else {
                centre
            };
            return Some(MoodPlay {
                sound: mood.sound.clone(),
                position,
            });
        }

        self.moodiness = self.moodiness.max(0.0);
        None
    }
}

/// One looping ambient voice's crossfade state — vanilla's
/// `BiomeAmbientSoundsHandler.LoopSoundInstance` (`:111-142`).
///
/// The fade is a plain integer counter, `volume = clamp(fade / 40, 0, 1)`, and the
/// **stop check happens before the counter moves** (`:125-127`), so a faded-out loop
/// survives one extra tick at negative fade before stopping. Reordering that makes
/// the loop stop a tick early and clip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoopFade {
    fade: i32,
    fade_direction: i32,
}

impl LoopFade {
    /// A loop at zero volume, not yet fading either way.
    pub fn new() -> Self {
        Self::default()
    }

    /// `LoopSoundInstance.fadeIn` (`:138-141`).
    pub fn fade_in(&mut self) {
        self.fade = self.fade.max(0);
        self.fade_direction = 1;
    }

    /// `LoopSoundInstance.fadeOut` (`:133-136`).
    pub fn fade_out(&mut self) {
        self.fade = self.fade.min(LOOP_SOUND_CROSS_FADE_TIME);
        self.fade_direction = -1;
    }

    /// The raw fade counter.
    pub fn fade(&self) -> i32 {
        self.fade
    }

    /// Advance one tick. Returns the new volume, or `None` when the voice should
    /// stop.
    pub fn tick(&mut self) -> Option<f32> {
        // `:125-127` — checked before the increment, deliberately.
        if self.fade < 0 {
            return None;
        }
        self.fade += self.fade_direction;
        Some((self.fade as f32 / LOOP_SOUND_CROSS_FADE_TIME as f32).clamp(0.0, 1.0))
    }

    /// The volume this fade currently implies, without advancing.
    pub fn volume(&self) -> f32 {
        (self.fade as f32 / LOOP_SOUND_CROSS_FADE_TIME as f32).clamp(0.0, 1.0)
    }
}

/// What the caller should do with a loop voice after [`AmbientLoops::tick`].
#[derive(Debug, Clone, PartialEq)]
pub enum LoopAction {
    /// Start this loop as a new looping, head-relative voice at volume 0.
    Start(Cow<'static, str>),
    /// Set an existing loop's volume.
    SetVolume {
        /// Which loop.
        sound: Cow<'static, str>,
        /// Its new volume.
        volume: f32,
    },
    /// Stop and forget this loop.
    Stop(Cow<'static, str>),
}

/// Tracks the set of live ambient loops and crossfades between them —
/// `BiomeAmbientSoundsHandler.tick`'s loop half (`:43-62`).
///
/// # Why more than one loop can be live at once
///
/// Walking from the crimson forest into the warped forest must not cut the first
/// loop off: vanilla keeps *both* voices, fading the old one out over 40 ticks while
/// the new one fades in. So this holds a small map, not a single current loop, and
/// entries are dropped only when their fade actually expires. Replacing the map with
/// one slot produces an audible seam at every biome border.
#[derive(Debug, Clone, Default)]
pub struct AmbientLoops {
    /// Live loops and their fades, in insertion order.
    loops: Vec<(Cow<'static, str>, LoopFade)>,
    /// The loop selected on the previous tick, for the change check.
    previous: Option<Cow<'static, str>>,
    /// Whether `previous` has ever been set, so that a first tick with no loop is
    /// distinguishable from "unchanged".
    primed: bool,
}

impl AmbientLoops {
    /// An empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// The loops currently live, with their volumes.
    pub fn live(&self) -> impl Iterator<Item = (&str, f32)> {
        self.loops.iter().map(|(n, f)| (n.as_ref(), f.volume()))
    }

    /// How many loop voices are live.
    pub fn len(&self) -> usize {
        self.loops.len()
    }

    /// Whether no loop voice is live.
    pub fn is_empty(&self) -> bool {
        self.loops.is_empty()
    }

    /// Advance one tick with `current` as the loop the player's position now
    /// selects, returning the actions the caller should apply in order.
    ///
    /// Mirrors `:43-62`: reap stopped voices, and *only on a change* fade every
    /// existing loop out and fade the newly selected one in (creating it if it is
    /// not already live). A tick where the selection is unchanged touches no fade
    /// directions, which is what lets a loop reach full volume and stay there.
    pub fn tick(&mut self, current: Option<&str>) -> Vec<LoopAction> {
        let mut actions = Vec::new();

        let changed = !self.primed || self.previous.as_deref() != current;
        if changed {
            self.primed = true;
            self.previous = current.map(|c| Cow::Owned(c.to_string()));

            for (_, fade) in &mut self.loops {
                fade.fade_out();
            }
            if let Some(current) = current {
                if let Some((_, fade)) = self.loops.iter_mut().find(|(n, _)| n == current) {
                    fade.fade_in();
                } else {
                    let mut fade = LoopFade::new();
                    fade.fade_in();
                    self.loops.push((Cow::Owned(current.to_string()), fade));
                    actions.push(LoopAction::Start(Cow::Owned(current.to_string())));
                }
            }
        }

        // Advance every fade, emitting a volume change or a stop.
        let mut still_live = Vec::with_capacity(self.loops.len());
        for (name, mut fade) in std::mem::take(&mut self.loops) {
            match fade.tick() {
                Some(volume) => {
                    actions.push(LoopAction::SetVolume {
                        sound: name.clone(),
                        volume,
                    });
                    still_live.push((name, fade));
                }
                None => actions.push(LoopAction::Stop(name)),
            }
        }
        self.loops = still_live;

        actions
    }
}
