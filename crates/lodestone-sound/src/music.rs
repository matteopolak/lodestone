//! Situational music selection and scheduling: the thing that decides *when* to
//! ask the audio engine for a music track, and *which* one.
//!
//! # Why this is its own module and holds no audio
//!
//! The sound engine below it was never the gap. `SoundRegistry`/[`SoundResolver`]
//! already resolve any named event to a decoded `.ogg`; what did not exist was
//! anything that ever asked for a *music* event. That decision is pure logic over
//! (screen, dimension, biome, gamemode, whether you are underwater) plus a
//! randomised countdown, so it lives here as a device-free, clock-free state
//! machine and the caller supplies a [`MusicSink`].
//!
//! Concretely: this module has no `Mixer`, no `AudioEngine`, no `Instant`, no
//! filesystem. Ticking it cannot make a sound, which is what lets the gates below
//! assert the *chosen identifier and the computed delay* rather than "audio
//! happened".
//!
//! # Matched to vanilla 26.2, with the constants cited
//!
//! 26.2 restructured this relative to older versions, and the restructure is the
//! thing most likely to be got wrong from memory: **music is no longer read off
//! the biome directly**. `Minecraft.getSituationalMusic`
//! (`Minecraft.java:2601-2621`) asks the camera's *environment-attribute probe*
//! for `EnvironmentAttributes.BACKGROUND_MUSIC`
//! (`EnvironmentAttributes.java:94-95`, registered as `audio/background_music`),
//! and that attribute's value is a [`BackgroundMusic`] record with three optional
//! slots. So a biome contributes music by *setting an attribute*, and the
//! selection between the slots is [`BackgroundMusic::select`]
//! (`BackgroundMusic.java:35-41`).
//!
//! The order in `getSituationalMusic` is:
//!
//! 1. A screen's own `getBackgroundMusic()`, if any — this wins outright.
//! 2. Otherwise, if there is a player: `END_BOSS` when the dimension is the End
//!    *and* the boss overlay wants music; else the probed
//!    `BackgroundMusic.select(isCreative, isUnderwater)`, which may be `None`.
//! 3. Otherwise (no player — the title screen): [`musics::MENU`].
//!
//! `isCreative` is **`instabuild && mayfly`** (`Minecraft.java:2615`), not
//! "gamemode == creative"; that distinction is why it is a separate flag on
//! [`MusicSituation`] rather than a gamemode enum.
//!
//! # The delay constants are the load-bearing part
//!
//! Getting these wrong is invisible in a short test and obvious in play, so they
//! are transcribed with cites and asserted against, never approximated:
//!
//! | music | min | max | replaces | cite |
//! |---|---|---|---|---|
//! | `MENU` | 20 | 600 | yes | `Musics.java:11` |
//! | `CREATIVE` | 12000 | 24000 | no | `Musics.java:12` |
//! | `CREDITS` | 0 | 0 | yes | `Musics.java:13` |
//! | `END_BOSS` | 0 | 0 | yes | `Musics.java:14` |
//! | `END` | 6000 | 24000 | yes | `Musics.java:15` |
//! | `UNDER_WATER` | 12000 | 24000 | no | `Musics.java:16` + `:19-21` |
//! | `GAME` | 12000 | 24000 | no | `Musics.java:17` + `:19-21` |
//!
//! and the scheduler's own: [`STARTING_DELAY`] is 100 ticks
//! (`MusicManager.java:19`, and `nextSongDelay` is initialised to it at `:25`);
//! [`MusicFrequency`] converts *minutes* to ticks as `minutes * 1200`
//! (`MusicManager.java:170`), giving 24000 / 12000 / 0 ticks.
//!
//! # A missing track is silence, not a panic and not a stall
//!
//! Music is the one part of the corpus `cargo xtask fetch-sounds` does **not**
//! fetch by default: 70 tracks and 22 records, 293 MB, behind `--all`. So on an
//! ordinary checkout every track this module can choose is absent from disk, and
//! that has to be an ordinary outcome rather than an error path.
//!
//! It is, and pleasingly it is *vanilla's own* outcome rather than a bespoke
//! degradation. `MusicManager.startPlaying` (`MusicManager.java:69-82`) assigns
//! `currentMusic` **before** playing and switches on the result: `STARTED` shows
//! the now-playing toast, `STARTED_SILENTLY` does not — and either way
//! `nextSongDelay` becomes `Integer.MAX_VALUE`. On the following tick the
//! `!isActive(currentMusic)` branch (`:52-55`) clears `currentMusic` and recomputes
//! the delay from [`MusicFrequency::next_song_delay`]. So a track that does not
//! start behaves exactly like a track that finished instantly: the manager
//! re-arms a normal 12000..=24000-tick countdown and tries again later.
//!
//! Hence [`MusicStart::Silent`]. There is no panic, no `unwrap`, no blocking wait,
//! and — the part worth gating — **no busy loop**: the retry is a full randomised
//! interval away, not the next tick. `tests/music_selection.rs` asserts that with
//! a sink whose every `start` is `Silent`, and its negative control makes the
//! absence panic and watches the gate fail.

use std::borrow::Cow;

use lodestone_audio::JavaRandom;

/// `MusicManager.STARTING_DELAY` — `MusicManager.java:19`. Also the initial value
/// of `nextSongDelay` (`:25`), the floor applied when nothing is selected
/// (`:44`), the flat `CONSTANT`-frequency delay (`:180`), and the `+ 100` added
/// when music is stopped explicitly (`:104`).
pub const STARTING_DELAY: i32 = 100;

/// `Musics.TEN_MINUTES` — the min delay every `createGameMusic` track uses
/// (`Musics.java:8`, `:19-21`).
pub const GAME_MUSIC_MIN_DELAY: i32 = 12_000;

/// `Musics.TWENTY_MINUTES` — the max delay every `createGameMusic` track uses
/// (`Musics.java:9`, `:19-21`).
pub const GAME_MUSIC_MAX_DELAY: i32 = 24_000;

/// Ticks per minute, the conversion `MusicFrequency` applies to its
/// minutes-valued setting (`MusicManager.java:170`). 20 ticks/s * 60 s.
pub const TICKS_PER_MINUTE: i32 = 1_200;

/// One music track and its scheduling parameters — vanilla's `Music` record
/// (`Music.java:8`).
///
/// `sound` is the **event key path with the namespace stripped** (`music.menu`,
/// not `minecraft:music.menu`), because that is what
/// [`SoundDriver::play_sound`](crate::SoundDriver::play_sound) and the shell's
/// `NetUpdate::Sound` path both use. The generated biome table strips it at
/// generation time.
///
/// [`Cow`] rather than `String` so the seven `Musics` constants stay `const` while
/// a track parsed out of biome JSON can still own its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Music {
    /// Namespace-stripped sound event key, e.g. `music.overworld.jungle`.
    pub sound: Cow<'static, str>,
    /// Minimum ticks between plays.
    pub min_delay: i32,
    /// Maximum ticks between plays.
    pub max_delay: i32,
    /// Whether selecting this track interrupts a *different* track already
    /// playing, rather than waiting for it to finish. See [`Music::can_replace`].
    pub replace_current_music: bool,
}

impl Music {
    /// A track with a `'static` name, usable in a `const`.
    pub const fn of(
        sound: &'static str,
        min_delay: i32,
        max_delay: i32,
        replace_current_music: bool,
    ) -> Self {
        Self {
            sound: Cow::Borrowed(sound),
            min_delay,
            max_delay,
            replace_current_music,
        }
    }

    /// A track whose name is owned (parsed from data).
    pub fn owned(
        sound: impl Into<String>,
        min_delay: i32,
        max_delay: i32,
        replace_current_music: bool,
    ) -> Self {
        Self {
            sound: Cow::Owned(sound.into()),
            min_delay,
            max_delay,
            replace_current_music,
        }
    }

    /// `Musics.createGameMusic` (`Musics.java:19-21`): 12000..=24000 ticks,
    /// non-replacing. Every biome's `default` slot is one of these, which is why
    /// the generated table's delays are all identical.
    pub const fn game(sound: &'static str) -> Self {
        Self::of(sound, GAME_MUSIC_MIN_DELAY, GAME_MUSIC_MAX_DELAY, false)
    }

    /// The namespace-stripped event key.
    pub fn sound(&self) -> &str {
        &self.sound
    }

    /// `MusicManager.canReplace` (`MusicManager.java:65-67`): this track may cut
    /// off `current` only if it is flagged replacing **and** is not the same
    /// track. The second half is what stops `MENU` (which *is* replacing)
    /// restarting itself every tick on the title screen.
    pub fn can_replace(&self, current: &str) -> bool {
        self.replace_current_music && self.sound() != current
    }
}

/// Vanilla's `Musics` table (`Musics.java:11-21`). Every value is transcribed with
/// its line cited on the constant.
pub mod musics {
    use super::Music;

    /// `Musics.MENU` — `Musics.java:11`. `20..=600`, replacing.
    pub const MENU: Music = Music::of("music.menu", 20, 600, true);
    /// `Musics.CREATIVE` — `Musics.java:12`. `12000..=24000`, non-replacing.
    pub const CREATIVE: Music = Music::of("music.creative", 12_000, 24_000, false);
    /// `Musics.CREDITS` — `Musics.java:13`. `0..=0`, replacing.
    pub const CREDITS: Music = Music::of("music.credits", 0, 0, true);
    /// `Musics.END_BOSS` — `Musics.java:14`. `0..=0`, replacing. The event is
    /// `music.dragon` (`SoundEvents.java:1040`), not `music.end_boss`.
    pub const END_BOSS: Music = Music::of("music.dragon", 0, 0, true);
    /// `Musics.END` — `Musics.java:15`. `6000..=24000`, replacing. Note the min
    /// is `FIVE_MINUTES`, not the game tracks' `TEN_MINUTES`.
    pub const END: Music = Music::of("music.end", 6_000, 24_000, true);
    /// `Musics.UNDER_WATER` — `Musics.java:16`, via `createGameMusic`.
    pub const UNDER_WATER: Music = Music::game("music.under_water");
    /// `Musics.GAME` — `Musics.java:17`, via `createGameMusic`.
    pub const GAME: Music = Music::game("music.game");
}

/// The three-slot music set an environment attribute carries — vanilla's
/// `BackgroundMusic` record (`BackgroundMusic.java:11`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundMusic {
    /// Played when neither of the more specific slots applies.
    pub default: Option<Music>,
    /// Played in creative (`instabuild && mayfly`) when present.
    pub creative: Option<Music>,
    /// Played while underwater when present — and it outranks `creative`.
    pub underwater: Option<Music>,
}

impl BackgroundMusic {
    /// `BackgroundMusic.EMPTY` (`BackgroundMusic.java:12`) — the attribute's
    /// default value, and what `pale_garden` sets explicitly to have no music at
    /// all.
    pub const EMPTY: Self = Self {
        default: None,
        creative: None,
        underwater: None,
    };

    /// `BackgroundMusic.OVERWORLD` (`BackgroundMusic.java:13`): `GAME` as the
    /// default, `CREATIVE` in creative, no underwater track. The ocean/river
    /// biomes use `OVERWORLD.withUnderwater(UNDER_WATER)`
    /// (`OverworldBiomes.java:355`, `:705`).
    pub fn overworld() -> Self {
        Self {
            default: Some(musics::GAME),
            creative: Some(musics::CREATIVE),
            underwater: None,
        }
    }

    /// `BackgroundMusic.withUnderwater` (`BackgroundMusic.java:31-33`).
    #[must_use]
    pub fn with_underwater(mut self, underwater: Music) -> Self {
        self.underwater = Some(underwater);
        self
    }

    /// Whether no slot is filled — i.e. this biome contributes no music.
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.creative.is_none() && self.underwater.is_none()
    }

    /// `BackgroundMusic.select` (`BackgroundMusic.java:35-41`).
    ///
    /// **Underwater outranks creative**, and each specific slot falls back to
    /// `default` only when *absent* — a present-but-different slot wins. Getting
    /// the precedence backwards is silent: it only shows up as the wrong track
    /// while swimming in creative.
    pub fn select(&self, is_creative: bool, is_underwater: bool) -> Option<&Music> {
        if is_underwater && self.underwater.is_some() {
            return self.underwater.as_ref();
        }
        if is_creative && self.creative.is_some() {
            return self.creative.as_ref();
        }
        self.default.as_ref()
    }
}

/// `MusicManager.MusicFrequency` (`MusicManager.java:158-196`) — the
/// "Music Frequency" option, which caps how long the manager will wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MusicFrequency {
    /// 20 minutes (`MusicManager.java:159`) — the default.
    #[default]
    Default,
    /// 10 minutes (`MusicManager.java:160`).
    Frequent,
    /// 0 minutes (`MusicManager.java:161`) — but see
    /// [`MusicFrequency::next_song_delay`]: it returns a flat
    /// [`STARTING_DELAY`], not zero.
    Constant,
}

impl MusicFrequency {
    /// Every variant, in vanilla's declaration order.
    pub const ALL: [Self; 3] = [Self::Default, Self::Frequent, Self::Constant];

    /// The option's value in *minutes*, as declared
    /// (`MusicManager.java:159-161`).
    pub const fn minutes(self) -> i32 {
        match self {
            Self::Default => 20,
            Self::Frequent => 10,
            Self::Constant => 0,
        }
    }

    /// `maxFrequency`, in ticks: `minutes * 1200` (`MusicManager.java:170`).
    /// 24000 / 12000 / 0.
    pub const fn max_frequency(self) -> i32 {
        self.minutes() * TICKS_PER_MINUTE
    }

    /// `MusicFrequency.getNextSongDelay` (`MusicManager.java:174-186`).
    ///
    /// Three distinct behaviours, and the first two are easy to lose:
    ///
    /// * `music == None` → the raw `max_frequency()`, with **no** randomisation
    ///   and no `STARTING_DELAY` floor.
    /// * `Constant` → a flat [`STARTING_DELAY`], *ignoring* both the track's
    ///   delays and its own `max_frequency()` of 0. A literal reading of
    ///   "0 minutes" would give 0 and restart music every tick.
    /// * otherwise → `Mth.nextInt(rng, min(min_delay, cap), min(max_delay, cap))`,
    ///   inclusive at both ends.
    pub fn next_song_delay(self, music: Option<&Music>, rng: &mut JavaRandom) -> i32 {
        let Some(music) = music else {
            return self.max_frequency();
        };
        if self == Self::Constant {
            return STARTING_DELAY;
        }
        let cap = self.max_frequency();
        next_int(rng, music.min_delay.min(cap), music.max_delay.min(cap))
    }
}

/// `Mth.nextInt(RandomSource, int, int)` — `Mth.java:146-148`. **Inclusive at
/// both ends**, and it returns `min` unchanged when `min >= max` rather than
/// panicking, which is what keeps the `0..=0` tracks (`CREDITS`, `END_BOSS`) from
/// calling `next_i32_bound(1)`… and more importantly what keeps a `0`-width range
/// from asserting.
pub fn next_int(rng: &mut JavaRandom, min_inclusive: i32, max_inclusive: i32) -> i32 {
    if min_inclusive >= max_inclusive {
        return min_inclusive;
    }
    rng.next_i32_bound(max_inclusive - min_inclusive + 1) + min_inclusive
}

/// Everything `Minecraft.getSituationalMusic` and `Minecraft.getMusicVolume` read,
/// gathered by the caller once per tick.
///
/// Deliberately a plain data struct with no world access: the shell resolves the
/// player's biome, gamemode flags and dimension its own way (a chunk-section
/// biome-palette walk, in this codebase), and hands the answers over. That keeps
/// this module hermetically testable and stops it growing a `Level`.
#[derive(Debug, Clone)]
pub struct MusicSituation<'a> {
    /// `Screen.getBackgroundMusic()` for the open screen, if it has one. Wins
    /// outright over everything else (`Minecraft.java:2602-2605`), and also forces
    /// the music volume to 1.0 (`Minecraft.java:2624-2626`).
    pub screen_music: Option<&'a Music>,
    /// Whether there is a local player at all. `false` is the title screen, which
    /// yields [`musics::MENU`] (`Minecraft.java:2618-2620`).
    pub in_world: bool,
    /// Dimension is the End **and** `BossHealthOverlay.shouldPlayMusic()`
    /// (`Minecraft.java:2610-2612`).
    pub end_boss_active: bool,
    /// The probed `audio/background_music` attribute value at the camera.
    pub background_music: &'a BackgroundMusic,
    /// `instabuild && mayfly` (`Minecraft.java:2615`) — *not* a gamemode check.
    pub creative: bool,
    /// `player.isUnderWater()` (`Minecraft.java:2616`).
    pub underwater: bool,
    /// The probed `audio/music_volume` attribute (default 1.0). `pale_garden`
    /// sets it to 0.0, which fades music out rather than muting it abruptly.
    pub music_volume: f32,
    /// Whether a level-loading screen is up; vanilla refuses to *start* a track
    /// while one is (`MusicManager.java:59`).
    pub level_loading: bool,
}

impl Default for MusicSituation<'_> {
    fn default() -> Self {
        Self {
            screen_music: None,
            in_world: false,
            end_boss_active: false,
            background_music: &BackgroundMusic::EMPTY,
            creative: false,
            underwater: false,
            music_volume: 1.0,
            level_loading: false,
        }
    }
}

impl MusicSituation<'_> {
    /// `Minecraft.getSituationalMusic` (`Minecraft.java:2601-2621`).
    ///
    /// Returns a clone rather than a borrow because the screen/menu answers are
    /// `'static` constants while the biome answer borrows from
    /// `background_music`; unifying the lifetimes buys nothing at one clone per
    /// tick.
    pub fn situational_music(&self) -> Option<Music> {
        if let Some(screen) = self.screen_music {
            return Some(screen.clone());
        }
        if !self.in_world {
            return Some(musics::MENU);
        }
        if self.end_boss_active {
            return Some(musics::END_BOSS);
        }
        self.background_music
            .select(self.creative, self.underwater)
            .cloned()
    }

    /// `Minecraft.getMusicVolume` (`Minecraft.java:2623-2627`): a screen that
    /// supplies its own music forces full volume, overriding the biome's
    /// `audio/music_volume`.
    pub fn effective_music_volume(&self) -> f32 {
        if self.screen_music.is_some() {
            1.0
        } else {
            self.music_volume
        }
    }
}

/// What happened when a [`MusicSink`] was asked to start a track — vanilla's
/// `SoundManager.play` result, narrowed to the two cases `MusicManager` switches
/// on (`MusicManager.java:72-79`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicStart {
    /// A voice is playing. Vanilla additionally shows the now-playing toast.
    Started,
    /// Nothing is audible — the track resolved to no file, its bytes are absent
    /// from the object store, or the music bus is muted. Vanilla's
    /// `STARTED_SILENTLY`. **Not an error**, and specifically the case a
    /// default-corpus checkout is always in, since `fetch-sounds` excludes music.
    Silent,
}

/// The playback side of music, so [`MusicManager`] can stay device-free.
///
/// Implemented by the shell over its `AudioEngine`, and by a recording fake in
/// the gates. Deliberately tiny: the manager needs to start a track, stop it, ask
/// whether it is still sounding, and set the music bus gain for the fade.
pub trait MusicSink {
    /// Begin `music`. Returning [`MusicStart::Silent`] must not be treated as an
    /// error by the implementor — resolve failures included.
    fn start(&mut self, music: &Music) -> MusicStart;

    /// Stop whatever is playing, if anything. Must make [`MusicSink::is_active`]
    /// return `false`.
    fn stop(&mut self);

    /// Whether the track most recently [`start`](MusicSink::start)ed is still
    /// sounding. A [`MusicStart::Silent`] start is never active.
    fn is_active(&self) -> bool;

    /// Set the music bus's runtime gain, for the volume fade
    /// (`MusicManager.java:133`, `updateCategoryVolume(SoundSource.MUSIC, gain)`).
    fn set_music_gain(&mut self, gain: f32);
}

/// `MusicManager` (`MusicManager.java:18-156`), minus the toast and the
/// `SoundManager` — a countdown plus "what is currently playing".
///
/// # How to change it
///
/// The tick is a faithful transcription and the *order* inside it is
/// load-bearing in a way that reads like a bug. On a replacing selection vanilla
/// stops the old track and sets `nextSongDelay = nextInt(0, min_delay/2)`, but it
/// does **not** clear `currentMusic`; the very next `if` therefore sees an
/// inactive track and clears it *and* `min`s the delay again with
/// [`MusicFrequency::next_song_delay`]. Both assignments land in one tick, so the
/// post-replace delay is the *smaller* of the two draws, consuming **two** RNG
/// values. Reordering or short-circuiting changes both the timing and the random
/// stream. `tests/music_selection.rs::a_replacing_selection_takes_the_min_of_two_draws`
/// pins it.
#[derive(Debug)]
pub struct MusicManager {
    /// The sound key of the track last handed to the sink, cleared once the sink
    /// reports it inactive. Mirrors vanilla's `currentMusic` instance.
    current: Option<Cow<'static, str>>,
    /// Ticks until the next track starts. Counts down only while nothing is
    /// playing.
    next_song_delay: i32,
    /// The "Music Frequency" option.
    frequency: MusicFrequency,
    /// The fading music-bus gain (`MusicManager.java:24`).
    current_gain: f32,
}

impl MusicManager {
    /// A manager in its start-of-process state: nothing playing, and
    /// [`STARTING_DELAY`] ticks on the clock (`MusicManager.java:25`).
    pub fn new(frequency: MusicFrequency) -> Self {
        Self {
            current: None,
            next_song_delay: STARTING_DELAY,
            frequency,
            current_gain: 1.0,
        }
    }

    /// The sound key currently playing, if any.
    pub fn current_track(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Ticks remaining before the next track starts. `i32::MAX` right after a
    /// track begins.
    pub fn next_song_delay(&self) -> i32 {
        self.next_song_delay
    }

    /// The faded music-bus gain.
    pub fn current_gain(&self) -> f32 {
        self.current_gain
    }

    /// The configured frequency.
    pub fn frequency(&self) -> MusicFrequency {
        self.frequency
    }

    /// `MusicManager.setMinutesBetweenSongs` (`MusicManager.java:153-156`):
    /// changing the option re-arms the countdown immediately.
    pub fn set_frequency(
        &mut self,
        frequency: MusicFrequency,
        situation: &MusicSituation<'_>,
        rng: &mut JavaRandom,
    ) {
        self.frequency = frequency;
        self.next_song_delay =
            frequency.next_song_delay(situation.situational_music().as_ref(), rng);
    }

    /// `MusicManager.tick` (`MusicManager.java:33-63`). Call once per client tick.
    pub fn tick(
        &mut self,
        situation: &MusicSituation<'_>,
        rng: &mut JavaRandom,
        sink: &mut impl MusicSink,
    ) {
        let volume = situation.effective_music_volume();
        if self.current.is_some()
            && self.current_gain != volume
            && !self.fade_playing(volume, situation, rng, sink)
        {
            return;
        }

        let Some(music) = situation.situational_music() else {
            // `:44` — a floor, not an assignment: an already-shorter countdown is
            // pushed back out to STARTING_DELAY, a longer one is left alone.
            self.next_song_delay = self.next_song_delay.max(STARTING_DELAY);
            return;
        };

        if let Some(current) = self.current.clone() {
            // `:47-50`
            if music.can_replace(&current) {
                sink.stop();
                self.next_song_delay = next_int(rng, 0, music.min_delay / 2);
            }
            // `:52-55` — reached in the same tick as the branch above, by design.
            if !sink.is_active() {
                self.current = None;
                self.next_song_delay = self
                    .next_song_delay
                    .min(self.frequency.next_song_delay(Some(&music), rng));
            }
        }

        // `:58`
        self.next_song_delay = self.next_song_delay.min(music.max_delay);

        // `:59-61` — the decrement happens *only* when we are otherwise ready to
        // start, so a playing track does not burn the countdown.
        if self.current.is_none() && !situation.level_loading {
            self.next_song_delay -= 1;
            if self.next_song_delay <= 0 {
                self.start_playing(&music, sink);
            }
        }
    }

    /// `MusicManager.startPlaying` (`MusicManager.java:69-82`).
    ///
    /// `current` is set from the *requested* track regardless of the sink's
    /// answer, exactly as vanilla assigns `currentMusic` before consulting
    /// `play`'s result. That is what makes [`MusicStart::Silent`] recover through
    /// the ordinary "track finished" path instead of needing one of its own.
    pub fn start_playing(&mut self, music: &Music, sink: &mut impl MusicSink) -> MusicStart {
        let started = sink.start(music);
        self.current = Some(music.sound.clone());
        self.next_song_delay = i32::MAX;
        started
    }

    /// `MusicManager.stopPlaying()` (`MusicManager.java:97-105`). Note the
    /// `+ STARTING_DELAY` on top of the frequency draw — stopping deliberately
    /// waits longer than a track simply ending.
    pub fn stop_playing(
        &mut self,
        situation: &MusicSituation<'_>,
        rng: &mut JavaRandom,
        sink: &mut impl MusicSink,
    ) {
        if self.current.is_some() {
            sink.stop();
            self.current = None;
        }
        self.next_song_delay = self
            .frequency
            .next_song_delay(situation.situational_music().as_ref(), rng)
            + STARTING_DELAY;
    }

    /// `MusicManager.fadePlaying` (`MusicManager.java:107-136`). Returns whether
    /// the caller should carry on with the rest of the tick.
    ///
    /// Asymmetric on purpose: fading **up** is a small additive step
    /// (`clamp(gain, 5e-4, 5e-3)`), fading **down** is geometric
    /// (`0.03*target + 0.97*gain`). Once the gain reaches 1e-4 the track is
    /// stopped outright, which is how `pale_garden`'s `music_volume: 0.0`
    /// silences music without a hard cut.
    fn fade_playing(
        &mut self,
        volume: f32,
        situation: &MusicSituation<'_>,
        rng: &mut JavaRandom,
        sink: &mut impl MusicSink,
    ) -> bool {
        if self.current.is_none() {
            return false;
        }
        if self.current_gain == volume {
            return true;
        }

        if self.current_gain < volume {
            self.current_gain += self.current_gain.clamp(5.0e-4, 5.0e-3);
            if self.current_gain > volume {
                self.current_gain = volume;
            }
        } else {
            self.current_gain = 0.03 * volume + 0.97 * self.current_gain;
            if (self.current_gain - volume).abs() < 1.0e-4 || self.current_gain < volume {
                self.current_gain = volume;
            }
        }

        self.current_gain = self.current_gain.clamp(0.0, 1.0);
        if self.current_gain <= 1.0e-4 {
            self.stop_playing(situation, rng, sink);
            false
        } else {
            sink.set_music_gain(self.current_gain);
            true
        }
    }
}
