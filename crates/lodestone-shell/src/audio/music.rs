//! Driving vanilla's `MusicManager` from the shell — the call site `that fix` did not
//! have, and without which the selector it built was an island.
//!
//! # What this is
//!
//! [`ShellMusic`] owns the three pieces of per-session music state vanilla keeps
//! on `Minecraft`: the [`MusicManager`] itself, the [`JavaRandom`] its delays are
//! drawn from, and the sink's sticky "is a track playing" flag. It is ticked at
//! 20 Hz and asks [`ShellAudio`] to start a track whenever the manager says so.
//!
//! # Why the sink is a type and not a closure
//!
//! `MusicManager::tick` needs a sink it can call `start`, `stop`, `is_active` and
//! `set_music_gain` on across a single tick, and `is_active` has to answer for
//! state set on a *previous* tick — so the flag cannot live in the sink's own
//! lifetime. [`ShellMusic`] holds it and lends it, which is why
//! [`ShellMusicSink`] is constructed fresh per tick and is not itself the state.
//!
//! # A test must never open a device or make a sound
//!
//! This repo has shipped exactly that defect: an accounts-screen unit test
//! spawned `Command::new("open")` and opened a Microsoft OAuth URL in the owner's
//! browser on **every** `cargo test -p lodestone-shell` run, invisible because the
//! suite passed. So playback here forks on **`#[cfg(test)]`**, not on
//! `cfg!(test)`. The difference matters: a `cfg!(test)` early return is a silent
//! skip that nothing can assert, whereas the two `#[cfg]` arms below also define
//! [`PLAYBACK`], so [`the_test_build_intercepts_playback`] can assert *which arm
//! compiled*. Delete the test arm and that gate fails rather than quietly
//! reaching for a device.
//!
//! # Playback is real; this module still owns none of it
//!
//! [`ShellAudio::start_music`] now resolves a stream and hands it to a real
//! streaming voice (`lodestone_sound::AudioEngine::start_music` natively, the
//! main-thread pump in `ShellAudio`'s wasm arm in the browser) — see
//! `docs/music-selection.md` for the ring/producer wiring. This module still
//! holds none of that: it is the same device-free `MusicManager` driver it
//! always was, and [`ShellMusicSink::is_active`]/[`ShellMusicSink::stop`] now
//! poll/drive the real voice through [`ShellAudio`] rather than a sticky flag
//! this module invented — see those methods' own docs for why the flag is
//! still kept as the test-build fallback.

use std::time::Duration;

// The portable clock: `Instant::now()` traps on wasm32. `crate::platform::Instant`
// *is* `std::time::Instant` on native (`web_time` re-exports `std::time` there), so
// this changes nothing off the browser. See `crate::platform`.
use crate::platform::Instant;

use lodestone_sound::JavaRandom;
use lodestone_sound::music::{
    BackgroundMusic, Music, MusicFrequency, MusicManager, MusicSink, MusicSituation, MusicStart,
    STARTING_DELAY,
};

use super::ShellAudio;

/// Real time per client tick — vanilla's 20 Hz.
const TICK: Duration = Duration::from_millis(50);

/// Cap on catch-up ticks in one call, mirroring `app::pacing`'s own
/// `MAX_TICKS_PER_UPDATE` reasoning: alt-tabbing away for a minute must cost ten
/// ticks of music bookkeeping, not 1200.
const MAX_CATCH_UP_TICKS: u32 = 10;

/// Which playback path this build compiled.
///
/// Exists so the `#[cfg(test)]` interception is **assertable** rather than merely
/// present — see this module's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Playback {
    /// Requests reach [`ShellAudio`] and thus a real output device.
    RealDevice,
    /// Requests are recorded and go no further.
    InterceptedForTests,
}

#[cfg(test)]
pub(crate) const PLAYBACK: Playback = Playback::InterceptedForTests;
#[cfg(not(test))]
pub(crate) const PLAYBACK: Playback = Playback::RealDevice;

/// The shell's music state: vanilla's `MusicManager` plus what it needs to run.
#[derive(Debug)]
pub(crate) struct ShellMusic {
    manager: MusicManager,
    rng: JavaRandom,
    /// The sink's `is_active`, which must survive between ticks.
    active: bool,
    /// The last gain the manager set, kept for the same reason.
    gain: f32,
    /// Every track a tick has asked to start, in order.
    ///
    /// This is the observable `that fix`'s gate is written against — "the engine
    /// received exactly one play request for a named identifier" — and the reason
    /// it is a **count of named requests** and not a duration is that a "music
    /// started within N ms" assertion is the sequential-duration trap that has
    /// already flaked a gate in this repo under concurrent load.
    requests: Vec<String>,
    /// When [`Self::advance`] last ran, for deriving whole ticks from frame time.
    last_tick: Option<Instant>,
}

impl ShellMusic {
    /// Fresh music state. `seed` seeds the delay RNG.
    pub(crate) fn new(seed: i64) -> Self {
        Self {
            manager: MusicManager::new(MusicFrequency::Default),
            rng: JavaRandom::new(seed),
            active: false,
            gain: 1.0,
            requests: Vec::new(),
            last_tick: None,
        }
    }

    /// The track currently playing, if any.
    pub(crate) fn current_track(&self) -> Option<&str> {
        self.manager.current_track()
    }

    /// Every start request so far, oldest first.
    pub(crate) fn requests(&self) -> &[String] {
        &self.requests
    }

    /// Run exactly `ticks` client ticks of music bookkeeping.
    ///
    /// The tick count is the **caller's** to supply rather than derived from a
    /// clock in here, so a gate can drive an exact number of ticks and assert a
    /// count. [`Self::advance`] is the wall-clock wrapper for the render loop.
    pub(crate) fn tick(
        &mut self,
        ticks: u32,
        situation: &MusicSituation<'_>,
        audio: Option<&mut ShellAudio>,
    ) {
        if ticks == 0 {
            return;
        }
        let mut sink = ShellMusicSink {
            audio,
            requests: &mut self.requests,
            active: self.active,
            gain: self.gain,
        };
        for _ in 0..ticks {
            self.manager.tick(situation, &mut self.rng, &mut sink);
        }
        self.active = sink.active;
        self.gain = sink.gain;
    }

    /// Tick from wall time: advance by however many whole 20 Hz ticks have elapsed
    /// since the last call, capped at [`MAX_CATCH_UP_TICKS`].
    ///
    /// The render loop calls this per frame, which is *not* 20 Hz — hence the
    /// accumulator. Calling `MusicManager::tick` once per frame instead would
    /// advance vanilla's delay bookkeeping at three times the intended rate on a
    /// 60 Hz display, so the tracks would come round sooner than the game's.
    pub(crate) fn advance(
        &mut self,
        now: Instant,
        situation: &MusicSituation<'_>,
        audio: Option<&mut ShellAudio>,
    ) {
        let Some(last) = self.last_tick else {
            // First frame: establish the baseline and run one tick, so a screen
            // that appears and is dismissed inside 50 ms still gets its music
            // request. Vanilla's `MENU` music is chosen on the first tick a
            // screen is up, not after a delay.
            self.last_tick = Some(now);
            self.tick(1, situation, audio);
            return;
        };
        let elapsed = now.saturating_duration_since(last);
        let ticks = u32::try_from(elapsed.as_millis() / TICK.as_millis()).unwrap_or(u32::MAX);
        if ticks == 0 {
            return;
        }
        self.last_tick = Some(last + TICK * ticks.min(MAX_CATCH_UP_TICKS));
        self.tick(ticks.min(MAX_CATCH_UP_TICKS), situation, audio);
    }
}

/// The situation for a **menu** screen: not in world, so
/// `MusicSituation::situational_music` selects `musics::MENU`.
///
/// `MENU` is `20/600` ticks with `replaceCurrentMusic` set (`Musics.java`),
/// so it *interrupts* rather than queues — and `Music::can_replace`'s second
/// clause is what stops it restarting itself every tick.
pub(crate) fn menu_situation() -> MusicSituation<'static> {
    MusicSituation {
        in_world: false,
        ..MusicSituation::default()
    }
}

/// The situation for an **in-world** frame.
///
/// # The selection input is not the biome, and not `GameMode`
///
/// 26.2 asks the camera's environment-attribute probe for
/// `audio/background_music` (`Minecraft.java`,
/// `EnvironmentAttributes.java`), which yields a three-slot
/// [`BackgroundMusic`] whose precedence is **underwater → creative → default**
/// (`BackgroundMusic.java`). Our biome table *is* that probe's answer
/// (a biome is what sets the attribute), so `background_music` is looked up per
/// biome — but the final pick is [`BackgroundMusic::select`], never the biome id.
///
/// And `creative` there is **`instabuild && mayfly`** (`Minecraft.java`), not
/// a gamemode check. Wiring it to `GameMode::Creative` would look right and be
/// wrong: a survival player granted both abilities gets creative music in vanilla,
/// and a creative player with `mayfly` revoked does not.
pub(crate) fn world_situation<'a>(
    background_music: &'a BackgroundMusic,
    creative: bool,
    underwater: bool,
    music_volume: f32,
) -> MusicSituation<'a> {
    MusicSituation {
        in_world: true,
        background_music,
        creative,
        underwater,
        music_volume,
        ..MusicSituation::default()
    }
}

/// A [`MusicSink`] over the shell's optional audio, recording every request.
///
/// `active`/`gain` are the **test-build and audio-disabled fallback** — see
/// [`ShellMusicSink::is_active`]/[`ShellMusicSink::set_music_gain`]. When a
/// real [`ShellAudio`] is present these two fields still get written (kept
/// harmlessly in sync by [`start`](MusicSink::start)), but the *authoritative*
/// answer comes from the engine, not from them: a sticky "I was told
/// `Started` once" bool can never notice a track finishing on its own, which
/// is exactly the bug `MusicManager::tick`'s `!isActive` branch exists to
/// react to.
struct ShellMusicSink<'a> {
    audio: Option<&'a mut ShellAudio>,
    requests: &'a mut Vec<String>,
    active: bool,
    gain: f32,
}

impl ShellMusicSink<'_> {
    /// Test builds record the request and stop there — no device, no sound.
    ///
    /// The assertion is the point: it converts "we believe tests never construct
    /// `ShellAudio`" into something that fails loudly if that stops being true,
    /// rather than a comment.
    #[cfg(test)]
    fn begin(&mut self, _music: &Music) -> MusicStart {
        assert!(
            self.audio.is_none(),
            "a test reached the music sink holding a real ShellAudio — that opens \
             an output device and makes a sound on every `cargo test` run, which \
             is the defect class that opened an OAuth URL in the owner's browser"
        );
        MusicStart::Silent
    }

    #[cfg(not(test))]
    fn begin(&mut self, music: &Music) -> MusicStart {
        match self.audio.as_deref_mut() {
            Some(audio) => audio.start_music(music),
            None => MusicStart::Silent,
        }
    }
}

impl MusicSink for ShellMusicSink<'_> {
    fn start(&mut self, music: &Music) -> MusicStart {
        // Recorded before the fork, so the request log is identical in both
        // builds and the production path is the one under test.
        self.requests.push(music.sound().to_string());
        let start = self.begin(music);
        self.active = matches!(start, MusicStart::Started);
        start
    }

    fn stop(&mut self) {
        // Real audio owns the actual stop (and, natively, signals the
        // producer thread to exit — see `AudioEngine::stop_music`'s doc); the
        // sticky flag is the fallback for a test build or a disabled-audio
        // session, where there is no engine to ask.
        if let Some(audio) = self.audio.as_deref_mut() {
            audio.stop_music();
        }
        self.active = false;
    }

    fn is_active(&self) -> bool {
        // Poll the real engine when one exists, so a track that finished on
        // its own is noticed on the very next tick — a sticky bool set once
        // at `start` time can never see that on its own. Falls back to the
        // sticky flag only when there is no engine to ask (test builds always
        // take this branch, by `begin`'s own assertion that `audio` is `None`
        // in `#[cfg(test)]`).
        match self.audio.as_deref() {
            Some(audio) => audio.is_music_active(),
            None => self.active,
        }
    }

    fn set_music_gain(&mut self, gain: f32) {
        self.gain = gain;
        if let Some(audio) = self.audio.as_deref_mut() {
            audio.set_music_gain(gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both production call sites exist, in code rather than in a comment.
    ///
    /// # Why a source scan is the right instrument here
    ///
    /// Every other gate in this module drives `ShellMusic` directly, and all of
    /// them pass identically whether or not anything in the shell ever calls it —
    /// that is precisely the closed loop that left `that fix`'s selector reaching no
    /// speakers with a green suite. The observable that distinguishes "wired" from
    /// "unwired" is the *existence of the call*, and neither call site is reachable
    /// from a unit test: `draw_menu` needs a window and a swapchain, and the
    /// in-world one needs a live camera and session.
    ///
    /// So this is the same scanner shape `lodestone-sound`'s
    /// `the_music_modules_cannot_reach_a_device_or_a_clock` uses, inverted to
    /// assert presence. It is the negative control `that fix` describes ("remove the
    /// call site and observe zero") made into a standing gate: delete either call
    /// and this fails by name.
    ///
    /// Comment lines are skipped, so a doc comment *mentioning* `tick_music` cannot
    /// satisfy it — which matters, because both call sites carry a comment block
    /// that names neighbouring music APIs.
    #[test]
    fn both_production_call_sites_actually_call_tick_music() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let sites = [
            ("src/app/menus.rs", "menu music (draw_menu)"),
            ("src/app/redraw.rs", "in-world music (the world redraw path)"),
        ];

        let mut scanned = 0usize;
        for (rel, what) in sites {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            scanned += 1;

            let called = text.lines().any(|line| {
                let code = line.trim_start();
                !code.starts_with("//") && code.contains("tick_music(")
            });
            assert!(
                called,
                "{rel} does not call `tick_music` in code, so {what} is unwired — \
                 the selector runs, its own tests pass, and no sound is ever \
                 requested. This is the island #451 was filed for."
            );
        }
        assert_eq!(scanned, 2, "both call sites must actually have been scanned");

        // Positive control: the scanner must be reading real code lines. Without
        // this, a wrong path or an all-comments read would pass the loop above
        // vacuously — the failure mode that makes a source scan untrustworthy.
        let menus = std::fs::read_to_string(root.join("src/app/menus.rs"))
            .expect("read src/app/menus.rs");
        assert!(
            menus.lines().any(|line| {
                let code = line.trim_start();
                !code.starts_with("//") && code.contains("fn draw_menu")
            }),
            "the scanner found no code lines in menus.rs — it is reading the \
             wrong thing, and the assertions above prove nothing"
        );
    }

    /// The interception is compiled in, not merely written down.
    ///
    /// This is the gate the `#[cfg(test)]`-over-`cfg!(test)` rule exists for: a
    /// silent `cfg!(test)` early return cannot be distinguished from a device
    /// call by any assertion, whereas deleting the `#[cfg(test)]` arm of
    /// [`ShellMusicSink::begin`] forces this constant's other arm and fails here.
    #[test]
    fn the_test_build_intercepts_playback() {
        assert_eq!(PLAYBACK, Playback::InterceptedForTests);
    }

    /// A menu screen requests `music.menu` **exactly once**, on exactly the tick
    /// vanilla's starting delay expires.
    ///
    /// A count, not a duration — see [`ShellMusic::requests`]'s doc. Asserted on
    /// **both** sides of the boundary, which is what makes it a gate on the jar's
    /// `MusicManager.java` starting delay of 100 ticks rather than a vague
    /// "music eventually starts": one tick short must be silent, and the boundary
    /// tick must produce precisely one named request.
    #[test]
    fn a_menu_screen_requests_music_menu_exactly_once_at_the_starting_delay() {
        let mut early = ShellMusic::new(0);
        early.tick(
            u32::try_from(STARTING_DELAY - 1).expect("positive"),
            &menu_situation(),
            None,
        );
        assert!(
            early.requests().is_empty(),
            "one tick short of the {STARTING_DELAY}-tick starting delay must be \
             silent, got {:?}",
            early.requests()
        );

        let mut music = ShellMusic::new(0);
        music.tick(
            u32::try_from(STARTING_DELAY).expect("positive"),
            &menu_situation(),
            None,
        );
        assert_eq!(
            music.requests(),
            ["music.menu"],
            "the starting-delay tick must ask for vanilla's MENU track exactly once"
        );
    }

    /// Control: with `in_world` true and an **empty** `BackgroundMusic` there is
    /// nothing to select, so the same tick must produce **zero** requests.
    ///
    /// This is the negative control `that fix` asks for, in the form the code allows:
    /// removing the *call site* is what it describes, and driving the same call
    /// with a situation that selects nothing is the assertable equivalent — it
    /// proves the request came from the selection and not from the act of ticking.
    #[test]
    fn control_a_tick_that_selects_nothing_requests_nothing() {
        let mut music = ShellMusic::new(0);
        let empty = BackgroundMusic::EMPTY;
        // Twice the starting delay, so this is not merely 'not yet'.
        music.tick(
            u32::try_from(STARTING_DELAY * 2).expect("positive"),
            &world_situation(&empty, false, false, 1.0),
            None,
        );

        assert!(
            music.requests().is_empty(),
            "an in-world tick with no background music must request nothing, \
             got {:?} — if this fires, the gate above is measuring the tick \
             rather than the selection",
            music.requests()
        );
    }

    /// In-world selection runs through `BackgroundMusic::select`'s precedence, and
    /// `creative` beats `default` while `underwater` beats both.
    ///
    /// Predicts the *identifier*, so a wiring that consulted the biome id or
    /// `GameMode` instead of the three-slot record lands on the wrong name rather
    /// than merely on a different code path.
    #[test]
    fn in_world_selection_follows_underwater_then_creative_then_default() {
        let background = BackgroundMusic {
            default: Some(Music::game("music.overworld.jungle")),
            creative: Some(Music::game("music.creative")),
            underwater: Some(Music::game("music.under_water")),
        };

        for (creative, underwater, expected) in [
            (false, false, "music.overworld.jungle"),
            (true, false, "music.creative"),
            // Underwater wins even with creative also set — the precedence, not
            // just "a specific slot beats default".
            (true, true, "music.under_water"),
            (false, true, "music.under_water"),
        ] {
            let mut music = ShellMusic::new(0);
            music.tick(
                u32::try_from(STARTING_DELAY).expect("positive"),
                &world_situation(&background, creative, underwater, 1.0),
                None,
            );
            assert_eq!(
                music.requests(),
                [expected],
                "creative={creative} underwater={underwater} must select {expected}"
            );
        }
    }

    /// Silence must not latch. Every tick that resolves to nothing playable tries
    /// again rather than deciding music is over — the state a normal checkout is
    /// permanently in, since 0 of 70 music objects are on disk.
    ///
    /// Also the reason `advance`'s first-frame branch runs a tick: a screen up for
    /// less than 50 ms would otherwise never ask for anything.
    #[test]
    fn a_silent_start_does_not_latch_the_sink_active() {
        let mut music = ShellMusic::new(0);
        // The starting delay, then MENU's own worst-case redraw of 600 ticks,
        // then one more — so a second request is *guaranteed*, not merely likely.
        // The sink reports `Silent`, so `is_active` stays false and the manager
        // takes the ordinary 'track finished' path and retries.
        music.tick(
            u32::try_from(STARTING_DELAY + 600 + 1).expect("positive"),
            &menu_situation(),
            None,
        );

        assert!(
            music.requests().len() > 1,
            "a sink that never starts must be retried, got {} request(s)",
            music.requests().len()
        );
        assert!(
            music.current_track().is_none(),
            "a track that reported Silent must not be recorded as current"
        );
    }
}
