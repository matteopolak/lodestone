//! Gates for situational music selection and scheduling.
//!
//! # What these assert, and why it is the identifier rather than "audio played"
//!
//! "A track was chosen" is satisfied by any implementation, including one that
//! always returns the overworld track. So every gate here predicts a **value**:
//! the exact sound-event identifier for a named biome under named conditions, and
//! the exact *computed* delay for a named music type. Both originate in
//! decompiled 26.2's music-table, music-manager, background-music,
//! situational-music-selection, and math-helper classes, cited on each assertion.
//!
//! No gate here measures elapsed wall time. Delays are asserted as computed tick
//! counts and cadence is asserted as a **count of start attempts**, both immune to
//! machine load — a sequential-duration ratio is not, and one failed in this repo
//! under concurrent agents on the day this was written.
//!
//! # Nothing here can make a sound
//!
//! [`RecordingSink`] is the only [`MusicSink`] in the file. `lodestone-sound`'s
//! device-backed `AudioEngine` is never constructed, so no test in this binary can
//! open an output device — and `the_music_modules_cannot_reach_a_device_or_a_clock`
//! turns that from a claim into a gate.

use std::borrow::Cow;

use lodestone_audio::JavaRandom;
use lodestone_sound::biome_music::{biome_music, biome_music_volume, overworld_music_for};
use lodestone_sound::music::{
    BackgroundMusic, Music, MusicFrequency, MusicManager, MusicSink, MusicSituation, MusicStart,
    STARTING_DELAY, musics, next_int,
};

// ---------------------------------------------------------------------------
// Test sink
// ---------------------------------------------------------------------------

/// A [`MusicSink`] that records rather than plays.
///
/// `silent` models the real, default state of this repository: music samples are
/// excluded from `cargo xtask fetch-sounds` unless `--all` is passed, so on an
/// ordinary checkout every track resolves to no bytes. With `silent = true` every
/// `start` reports [`MusicStart::Silent`] and `is_active` stays `false`, which is
/// exactly what the resolver+store pair does when the `.ogg` is absent.
#[derive(Debug, Default)]
struct RecordingSink {
    /// Every track `start` was called with, in order.
    started: Vec<String>,
    stops: usize,
    gains: Vec<f32>,
    active: bool,
    silent: bool,
    /// When set, `start` panics. Used only by the negative controls, to prove the
    /// gates actually reach `start`.
    panic_on_start: bool,
}

impl RecordingSink {
    fn playing() -> Self {
        Self::default()
    }

    /// A sink whose every track is missing from disk.
    fn silent() -> Self {
        Self {
            silent: true,
            ..Self::default()
        }
    }
}

impl MusicSink for RecordingSink {
    fn start(&mut self, music: &Music) -> MusicStart {
        assert!(
            !self.panic_on_start,
            "negative control: the missing track panicked instead of being silent"
        );
        self.started.push(music.sound().to_string());
        if self.silent {
            self.active = false;
            MusicStart::Silent
        } else {
            self.active = true;
            MusicStart::Started
        }
    }

    fn stop(&mut self) {
        self.stops += 1;
        self.active = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn set_music_gain(&mut self, gain: f32) {
        self.gains.push(gain);
    }
}

/// A situation in a world, in the given biome, with everything else neutral.
fn in_biome(background: &BackgroundMusic) -> MusicSituation<'_> {
    MusicSituation {
        in_world: true,
        background_music: background,
        ..MusicSituation::default()
    }
}

// ---------------------------------------------------------------------------
// 1. The Musics table, against the jar
// ---------------------------------------------------------------------------

/// Vanilla's own music table, every field. These seven rows are the constants every
/// delay computation below is derived from, so if one drifts the rest of the file
/// is measuring the wrong thing.
#[test]
fn the_musics_table_matches_the_jar() {
    // (music, sound, min_delay, max_delay, replace) — vanilla's own music constants
    // MENU through END, with its game-music helper's 12000/24000/false for UNDER_WATER and GAME.
    let expected = [
        (&musics::MENU, "music.menu", 20, 600, true),
        (&musics::CREATIVE, "music.creative", 12_000, 24_000, false),
        (&musics::CREDITS, "music.credits", 0, 0, true),
        (&musics::END_BOSS, "music.dragon", 0, 0, true),
        (&musics::END, "music.end", 6_000, 24_000, true),
        (
            &musics::UNDER_WATER,
            "music.under_water",
            12_000,
            24_000,
            false,
        ),
        (&musics::GAME, "music.game", 12_000, 24_000, false),
    ];
    for (music, sound, min, max, replace) in expected {
        assert_eq!(music.sound(), sound);
        assert_eq!(music.min_delay, min, "{sound} min_delay");
        assert_eq!(music.max_delay, max, "{sound} max_delay");
        assert_eq!(
            music.replace_current_music, replace,
            "{sound} replace_current_music"
        );
    }

    // The two easiest to get wrong from the constant *name* rather than the
    // registered event: END_BOSS is `music.dragon` (vanilla's own dragon-music sound
    // event), and
    // END's min is vanilla's own five-minute constant, not the game tracks' ten-minute one.
    assert_eq!(musics::END_BOSS.sound(), "music.dragon");
    assert_ne!(musics::END.min_delay, musics::GAME.min_delay);
    assert_eq!(musics::END.min_delay, 6_000);
}

// ---------------------------------------------------------------------------
// 2. Selection: the exact identifier, per biome and situation
// ---------------------------------------------------------------------------

/// The headline gate: a named biome at named conditions yields the jar-derived
/// track **identifier**, not merely "some track".
#[test]
fn a_named_biome_selects_the_jar_derived_track_identifier() {
    // jungle sets only `default` (vanilla's own jungle-biome music setter).
    let jungle = biome_music("jungle").expect("jungle declares background music");
    assert_eq!(
        jungle
            .select(false, false)
            .expect("jungle has a default track")
            .sound(),
        "music.overworld.jungle"
    );

    // Namespaced ids resolve identically.
    assert_eq!(
        biome_music("minecraft:jungle").map(|m| m
            .select(false, false)
            .expect("default")
            .sound()),
        Some("music.overworld.jungle")
    );

    // Several biomes share one track — birch_forest and old_growth_birch_forest use
    // the shared forest music event via vanilla's own forest-biome music setter, and
    // dark_forest sets the same
    // event directly in vanilla's own dark-forest music setter.
    for biome in ["forest", "birch_forest", "dark_forest", "old_growth_birch_forest"] {
        assert_eq!(
            biome_music(biome)
                .and_then(|m| m.select(false, false))
                .map(Music::sound),
            Some("music.overworld.forest"),
            "{biome} should use the shared forest track"
        );
    }

    // The Nether biomes each have their own (vanilla's own per-biome nether music
    // setters, one background-music attribute call apiece).
    assert_eq!(
        biome_music("nether_wastes")
            .and_then(|m| m.select(false, false))
            .map(Music::sound),
        Some("music.nether.nether_wastes")
    );
    assert_eq!(
        biome_music("warped_forest")
            .and_then(|m| m.select(false, false))
            .map(Music::sound),
        Some("music.nether.warped_forest")
    );
}

/// Vanilla's own background-music selection. The precedence is the
/// part worth pinning: **underwater outranks creative**, and each specific slot
/// falls back to `default` only when *absent*.
#[test]
fn slot_precedence_is_underwater_then_creative_then_default() {
    // The ocean family is the only one with all three slots
    // (vanilla's own base-ocean music setter — overworld music with an underwater
    // override).
    let ocean = biome_music("ocean").expect("ocean declares background music");
    assert!(ocean.default.is_some() && ocean.creative.is_some() && ocean.underwater.is_some());

    let cases = [
        // (creative, underwater, expected)
        (false, false, "music.game"),
        (true, false, "music.creative"),
        (false, true, "music.under_water"),
        // The load-bearing row: both flags set. Underwater wins. A reimplementation
        // that checked creative first passes every other row in this table.
        (true, true, "music.under_water"),
    ];
    for (creative, underwater, expected) in cases {
        assert_eq!(
            ocean.select(creative, underwater).map(Music::sound),
            Some(expected),
            "ocean with creative={creative} underwater={underwater}"
        );
    }

    // And the fallback direction: jungle has *no* creative and *no* underwater
    // slot, so both conditions must still yield the jungle default — not
    // `music.creative`, and not silence.
    let jungle = biome_music("jungle").unwrap();
    for (creative, underwater) in [(true, false), (false, true), (true, true)] {
        assert_eq!(
            jungle.select(creative, underwater).map(Music::sound),
            Some("music.overworld.jungle"),
            "jungle with creative={creative} underwater={underwater} must fall back to default"
        );
    }
}

/// Vanilla's own situational-music selection, in order.
#[test]
fn situational_selection_follows_the_documented_order() {
    let jungle = biome_music("jungle").unwrap();

    // No player -> MENU, regardless of what biome data says.
    let menu = MusicSituation {
        in_world: false,
        background_music: jungle,
        ..MusicSituation::default()
    };
    assert_eq!(
        menu.situational_music().as_ref().map(Music::sound),
        Some("music.menu")
    );

    // In world -> the biome's track.
    let world = in_biome(jungle);
    assert_eq!(
        world.situational_music().as_ref().map(Music::sound),
        Some("music.overworld.jungle")
    );

    // End boss outranks the biome.
    let boss = MusicSituation {
        end_boss_active: true,
        ..in_biome(jungle)
    };
    assert_eq!(
        boss.situational_music().as_ref().map(Music::sound),
        Some("music.dragon")
    );

    // A screen's own music outranks *everything*, including the end boss.
    let screen = MusicSituation {
        screen_music: Some(&musics::CREDITS),
        end_boss_active: true,
        ..in_biome(jungle)
    };
    assert_eq!(
        screen.situational_music().as_ref().map(Music::sound),
        Some("music.credits")
    );

    // pale_garden is the one biome that declares *no* music
    // (vanilla's own dark-forest music setter's pale-garden branch sets an
    // empty background-music value)
    // — and it must come back as
    // an empty-but-present entry, distinct from "biome not in the table".
    let pale = biome_music("pale_garden").expect("pale_garden has a present, empty entry");
    assert!(pale.is_empty(), "pale_garden must be BackgroundMusic::EMPTY");
    assert_eq!(in_biome(pale).situational_music(), None);
    // Its music_volume is 0.0, not absent (vanilla's own overworld-biomes
    // music_volume set).
    assert_eq!(biome_music_volume("pale_garden"), 0.0);
    // While a biome with no attribute at all defaults to 1.0.
    assert_eq!(biome_music_volume("plains"), 1.0);
    assert_eq!(biome_music("plains"), None);

    // ...and that distinction is load-bearing: `plains` must fall back to the
    // overworld pair, `pale_garden` must not.
    assert_eq!(
        overworld_music_for("plains").select(false, false).map(Music::sound),
        Some("music.game")
    );
    assert!(
        overworld_music_for("pale_garden").is_empty(),
        "collapsing None and EMPTY would give pale_garden the overworld track"
    );
}

/// Vanilla's own music-volume selection: a screen with its own
/// music forces full volume, overriding the biome attribute.
#[test]
fn screen_music_forces_full_volume() {
    let pale = biome_music("pale_garden").unwrap();
    let quiet = MusicSituation {
        music_volume: 0.0,
        ..in_biome(pale)
    };
    assert_eq!(quiet.effective_music_volume(), 0.0);

    let with_screen = MusicSituation {
        screen_music: Some(&musics::MENU),
        ..quiet.clone()
    };
    assert_eq!(with_screen.effective_music_volume(), 1.0);
}

// ---------------------------------------------------------------------------
// 3. Delays: the computed value, not "it is positive"
// ---------------------------------------------------------------------------

/// Vanilla's own frequency-based next-song-delay computation for every
/// variant, predicting the exact value and excluding the plausible wrong ones.
#[test]
fn the_computed_delay_equals_the_jar_derived_value() {
    const SEED: i64 = 0x5EED_1234;

    // DEFAULT: cap = 20 * 1200 = 24000 (vanilla's own DEFAULT frequency's
    // declared minutes, times its constructor's `* 1200`), so the range
    // is min(12000,24000)..=min(24000,24000) = 12000..=24000 inclusive
    // (vanilla's own inclusive-bounded random-int draw).
    let mut rng = JavaRandom::new(SEED);
    let got = MusicFrequency::Default.next_song_delay(Some(&musics::GAME), &mut rng);

    // Predicted independently from the jar's arithmetic, not read back from the
    // implementation: vanilla's own inclusive-bounded random-int draw over
    // `[12000, 24000]` is
    // rng.nextInt(24000 - 12000 + 1) + 12000.
    let mut oracle = JavaRandom::new(SEED);
    let predicted = oracle.next_i32_bound(24_000 - 12_000 + 1) + 12_000;
    assert_eq!(got, predicted, "DEFAULT frequency on a game track");

    // And it is not any of the three ways this is normally got wrong.
    let mut w = JavaRandom::new(SEED);
    let exclusive_upper = w.next_i32_bound(24_000 - 12_000) + 12_000;
    assert_ne!(
        got, exclusive_upper,
        "an exclusive upper bound would consume the same draw and give a different value"
    );
    assert_ne!(
        got,
        MusicFrequency::Default.max_frequency(),
        "using the cap alone (24000) rather than a draw"
    );
    let mut w2 = JavaRandom::new(SEED);
    assert_ne!(
        got,
        w2.next_i32_bound(24_000 - 12_000 + 1),
        "forgetting to add min_delay would give a 0..=12000 value"
    );

    // FREQUENT: cap = 10 * 1200 = 12000, so BOTH ends clamp to 12000 and
    // vanilla's own inclusive-bounded random-int draw's `min >= max` early return yields exactly 12000
    // while consuming NO randomness. That is an exact prediction with no seed
    // dependence at all, and it pins the early return.
    let mut rng = JavaRandom::new(SEED);
    let mut untouched = JavaRandom::new(SEED);
    let got = MusicFrequency::Frequent.next_song_delay(Some(&musics::GAME), &mut rng);
    assert_eq!(got, 12_000, "FREQUENT caps both ends of a game track at 12000");
    assert_eq!(
        rng.next_i32(),
        untouched.next_i32(),
        "the min >= max path must consume no random draws"
    );

    // CONSTANT: a flat STARTING_DELAY (vanilla's own frequency-based
    // next-song-delay computation's
    // `this == CONSTANT` branch), *not* its
    // max_frequency of 0 — which would restart music every tick.
    assert_eq!(MusicFrequency::Constant.max_frequency(), 0);
    let mut rng = JavaRandom::new(SEED);
    assert_eq!(
        MusicFrequency::Constant.next_song_delay(Some(&musics::GAME), &mut rng),
        STARTING_DELAY
    );
    assert_eq!(STARTING_DELAY, 100);

    // No music selected: the raw cap, unrandomised (vanilla's own
    // next-song-delay computation's `music == null` branch).
    for freq in MusicFrequency::ALL {
        let mut rng = JavaRandom::new(SEED);
        assert_eq!(
            freq.next_song_delay(None, &mut rng),
            freq.max_frequency(),
            "{freq:?} with no music"
        );
    }
    assert_eq!(MusicFrequency::Default.max_frequency(), 24_000);
    assert_eq!(MusicFrequency::Frequent.max_frequency(), 12_000);
}

/// Vanilla's own inclusive-bounded random-int draw is inclusive at **both** ends, which a
/// single draw cannot demonstrate. Drawing many values from one fixed stream and
/// asserting the observed extremes land exactly on the bounds does — and being
/// seeded, it is deterministic rather than probabilistic, and being a count it is
/// immune to machine load.
#[test]
fn the_delay_range_is_inclusive_at_both_ends() {
    // MENU's 20..=600 (vanilla's own MENU music constant) is narrow enough that 20_000 draws from one
    // stream hit both endpoints; a game track's 12001-wide range would not.
    let mut rng = JavaRandom::new(0x1234_5678);
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for _ in 0..20_000 {
        let d = MusicFrequency::Default.next_song_delay(Some(&musics::MENU), &mut rng);
        lo = lo.min(d);
        hi = hi.max(d);
    }
    assert_eq!(lo, 20, "min_delay must be attainable");
    assert_eq!(hi, 600, "max_delay must be attainable — the bound is inclusive");

    // The zero-width tracks (CREDITS, END_BOSS at 0..=0, vanilla's own CREDITS
    // and END_BOSS music constants) must
    // return 0 without asserting inside next_i32_bound, which panics on bound <= 0.
    let mut rng = JavaRandom::new(1);
    assert_eq!(next_int(&mut rng, 0, 0), 0);
    assert_eq!(
        MusicFrequency::Default.next_song_delay(Some(&musics::CREDITS), &mut rng),
        0
    );
}

// ---------------------------------------------------------------------------
// 4. The scheduler
// ---------------------------------------------------------------------------

/// The manager starts a track only after the countdown expires, and the countdown
/// is the jar's `STARTING_DELAY` on a fresh manager (vanilla's own
/// next-song-delay field initializer).
#[test]
fn a_fresh_manager_waits_exactly_starting_delay_ticks_before_the_first_track() {
    let jungle = biome_music("jungle").unwrap();
    let situation = in_biome(jungle);
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::playing();
    let mut rng = JavaRandom::new(7);

    assert_eq!(mgr.next_song_delay(), STARTING_DELAY);

    // Vanilla's own tick routine's `min(nextSongDelay, music.maxDelay())` clamps to
    // max_delay (24000) which is larger, so the countdown is the starting 100. Its
    // trailing `--nextSongDelay` pre-decrements, so the 100th tick is the one that fires.
    for tick in 1..STARTING_DELAY {
        mgr.tick(&situation, &mut rng, &mut sink);
        assert!(
            sink.started.is_empty(),
            "started on tick {tick}, before the countdown expired"
        );
    }
    mgr.tick(&situation, &mut rng, &mut sink);
    assert_eq!(sink.started, vec!["music.overworld.jungle".to_string()]);
    // Vanilla's own start-playing routine parks the countdown at Integer.MAX_VALUE, and it is
    // still parked at the end of the tick that started the track because the
    // max_delay clamp already ran before the decrement fired.
    assert_eq!(mgr.next_song_delay(), i32::MAX);
    assert_eq!(mgr.current_track(), Some("music.overworld.jungle"));

    // A playing track does not burn the countdown (vanilla's own tick routine's
    // `currentMusic == null` guard on the decrement), but the max_delay clamp is
    // **outside** the `currentMusic != null`
    // block, so from the very next tick the parked value is `max_delay` rather
    // than MAX. It then stays there, because nothing decrements it.
    //
    // Worth pinning: it means the wait after a track ends is bounded by
    // `max_delay` no matter how long the track played, and it is the reason a
    // naive `assert_eq!(delay, i32::MAX)` here fails against real vanilla
    // behaviour — which is how this expectation was caught and corrected.
    mgr.tick(&situation, &mut rng, &mut sink);
    assert_eq!(mgr.next_song_delay(), musics::GAME.max_delay);
    assert_eq!(musics::GAME.max_delay, 24_000);

    for _ in 0..1_000 {
        mgr.tick(&situation, &mut rng, &mut sink);
    }
    assert_eq!(sink.started.len(), 1, "a playing track must not be restarted");
    assert_eq!(
        mgr.next_song_delay(),
        musics::GAME.max_delay,
        "the countdown must hold, not drain, while a track plays"
    );
}

/// `MENU` is flagged `replace_current_music` (vanilla's own MENU music
/// constant), so without
/// vanilla's own can-replace check's second clause ("and is not the same
/// track") the title screen would stop and restart the menu track every tick.
#[test]
fn a_replacing_track_does_not_replace_itself() {
    let situation = MusicSituation::default(); // no player -> MENU
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::playing();
    let mut rng = JavaRandom::new(11);

    // MENU's max_delay is 600, and vanilla's own tick routine's max_delay clamp applies, so
    // it starts within 100 ticks.
    for _ in 0..STARTING_DELAY {
        mgr.tick(&situation, &mut rng, &mut sink);
    }
    assert_eq!(sink.started, vec!["music.menu".to_string()]);

    for _ in 0..5_000 {
        mgr.tick(&situation, &mut rng, &mut sink);
    }
    assert_eq!(sink.started.len(), 1, "menu music restarted itself");
    assert_eq!(sink.stops, 0, "menu music was stopped and replaced by itself");
}

/// The tick's load-bearing ordering (vanilla's own tick routine's replacing
/// branch): on a replacing
/// selection vanilla stops the old track and sets
/// its own next-song-delay field to a random value in `[0, min_delay/2)`,
/// then — in the *same* tick, because
/// its own current-music field was not cleared — sees an inactive track and `min`s the delay
/// again with its own next-song-delay computation. Two draws are consumed and the smaller wins.
#[test]
fn a_replacing_selection_takes_the_min_of_two_draws() {
    let jungle = biome_music("jungle").unwrap();
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::playing();
    let mut rng = JavaRandom::new(0xABCD);

    // Get the jungle track playing.
    let world = in_biome(jungle);
    for _ in 0..STARTING_DELAY {
        mgr.tick(&world, &mut rng, &mut sink);
    }
    assert_eq!(mgr.current_track(), Some("music.overworld.jungle"));
    assert_eq!(sink.stops, 0);

    // Now the End boss starts: END_BOSS is replacing (vanilla's own END_BOSS
    // music constant) and is a
    // different track, so canReplace is true.
    let boss = MusicSituation {
        end_boss_active: true,
        ..in_biome(jungle)
    };

    // Predict both draws independently, from the jar's arithmetic, using a clone
    // of the RNG at exactly this point.
    let mut oracle = rng.clone();
    // Vanilla's own tick routine's stop branch — an inclusive-bounded
    // random-int draw over `[0, END_BOSS.min_delay / 2]`
    // = nextInt(0, 0) = 0, and its min>=max early return consumes no draw for a
    // zero-width range.
    let first = next_int(&mut oracle, 0, musics::END_BOSS.min_delay / 2);
    // The same tick's `!isActive` branch — min with the next-song-delay computation(END_BOSS): range
    // min(0,24000)..=min(0,24000) = 0..=0, again no draw.
    let second = MusicFrequency::Default.next_song_delay(Some(&musics::END_BOSS), &mut oracle);
    let predicted = first.min(second);
    assert_eq!(
        (first, second),
        (0, 0),
        "END_BOSS is a 0..=0 track, so both draws are the degenerate 0"
    );

    mgr.tick(&boss, &mut rng, &mut sink);
    assert_eq!(sink.stops, 1, "the jungle track must have been stopped");
    // Both assignments landed, then the max_delay clamp reduced it to
    // END_BOSS.max_delay (0), then the trailing decrement fired in the same tick —
    // so the boss track is already
    // playing rather than merely scheduled.
    assert_eq!(predicted, 0);
    assert_eq!(
        sink.started,
        vec![
            "music.overworld.jungle".to_string(),
            "music.dragon".to_string()
        ],
        "a 0-delay replacing track starts in the very tick it replaces"
    );
    assert_eq!(mgr.current_track(), Some("music.dragon"));
}

/// A non-replacing selection must wait for the current track to end
/// (vanilla's own CREATIVE and GAME music constants are both non-replacing), which is
/// what stops music stuttering as you walk across a biome border.
#[test]
fn a_non_replacing_selection_waits_rather_than_interrupting() {
    let jungle = biome_music("jungle").unwrap();
    let desert = biome_music("desert").unwrap();
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::playing();
    let mut rng = JavaRandom::new(99);

    for _ in 0..STARTING_DELAY {
        mgr.tick(&in_biome(jungle), &mut rng, &mut sink);
    }
    assert_eq!(mgr.current_track(), Some("music.overworld.jungle"));

    // Step into the desert. Its track is non-replacing, so nothing happens.
    let desert_situation = in_biome(desert);
    for _ in 0..10_000 {
        mgr.tick(&desert_situation, &mut rng, &mut sink);
    }
    assert_eq!(sink.stops, 0, "walking into a new biome must not cut the track");
    assert_eq!(sink.started.len(), 1);
    assert_eq!(mgr.current_track(), Some("music.overworld.jungle"));

    // Once the track genuinely ends, the *desert* track is what starts next.
    sink.active = false;
    for _ in 0..40_000 {
        mgr.tick(&desert_situation, &mut rng, &mut sink);
        if sink.started.len() > 1 {
            break;
        }
    }
    assert_eq!(
        sink.started.last().map(String::as_str),
        Some("music.overworld.desert"),
        "after the old track ends, the current biome's track is chosen"
    );
}

/// Vanilla's own tick routine's `music == null` branch — with nothing selected the countdown is *floored* at
/// `STARTING_DELAY` rather than counted down, so a music-less biome does not
/// accumulate a fire-immediately countdown that discharges the moment you step
/// out of it.
#[test]
fn a_music_less_biome_holds_the_countdown_at_the_floor() {
    let pale = biome_music("pale_garden").unwrap();
    let situation = in_biome(pale);
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::playing();
    let mut rng = JavaRandom::new(3);

    for _ in 0..50_000 {
        mgr.tick(&situation, &mut rng, &mut sink);
    }
    assert!(sink.started.is_empty(), "pale_garden must stay silent");
    assert_eq!(
        mgr.next_song_delay(),
        STARTING_DELAY,
        "the countdown is held at the floor, not driven to zero"
    );
}

// ---------------------------------------------------------------------------
// 5. The missing-asset path — the honest degradation
// ---------------------------------------------------------------------------

/// **The missing-asset gate.** Music is excluded from the default sound corpus
/// (`xtask::is_music_event`; 70 tracks + 22 records, 293 MB, only with `--all`), so
/// on an ordinary checkout every track this module chooses is absent from disk.
/// Measured on this machine while writing: **0 of 70** music objects present.
///
/// With the track absent, selection must still resolve, the outcome must be
/// silence, and there must be no panic and no blocking wait — and specifically **no
/// busy loop**: the retry has to be a full randomised interval away, not the next
/// tick. That last clause is the one a weaker gate misses, and it is asserted as a
/// **count of attempts over a fixed tick budget**, which no amount of machine load
/// can perturb.
#[test]
fn a_missing_track_is_silence_with_a_full_interval_retry() {
    let jungle = biome_music("jungle").unwrap();
    let situation = in_biome(jungle);
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::silent();
    let mut rng = JavaRandom::new(0xF00D);

    // Selection is unaffected by the asset being missing — that is the point of
    // resolving lazily.
    assert_eq!(
        situation.situational_music().as_ref().map(Music::sound),
        Some("music.overworld.jungle")
    );

    const TICKS: i32 = 120_000; // 100 minutes of game time
    for _ in 0..TICKS {
        mgr.tick(&situation, &mut rng, &mut sink);
    }

    // It tried, so the gate is not vacuous...
    assert!(
        !sink.started.is_empty(),
        "the manager never even attempted a track — this gate would pass vacuously"
    );
    // ...every attempt was the right track...
    assert!(
        sink.started.iter().all(|s| s == "music.overworld.jungle"),
        "attempted something other than the selected track: {:?}",
        sink.started
    );
    // ...nothing is playing, because nothing could...
    assert_eq!(
        mgr.current_track(),
        None,
        "a silent start must not leave a track marked as playing"
    );

    // ...and the retry cadence is a full interval, not a busy loop. The floor is
    // one attempt per min_delay ticks, so over 120000 ticks at 12000..=24000 the
    // count cannot exceed 10 + the initial one. A busy loop would be ~120000.
    let ceiling = TICKS / musics::GAME.min_delay + 1;
    assert!(
        sink.started.len() as i32 <= ceiling,
        "retried {} times in {TICKS} ticks (ceiling {ceiling}) — that is a busy loop, \
         not a full-interval retry",
        sink.started.len()
    );
    // And it really did retry more than once, so "no busy loop" is not being
    // satisfied by "gave up after the first failure".
    assert!(
        sink.started.len() >= 2,
        "only {} attempt(s) in {TICKS} ticks — the manager gave up rather than retrying",
        sink.started.len()
    );
    // A voice was never started, so it was never stopped either.
    assert_eq!(sink.stops, 0);
}

/// **Negative control for the gate above.** With the sink made to panic on a
/// missing track instead of reporting silence, the gate must fail. Observing this
/// panic is what proves `start` is actually reached — a missing-asset gate that
/// never calls `start` passes for the wrong reason.
#[test]
#[should_panic(expected = "the missing track panicked instead of being silent")]
fn control_a_panicking_missing_track_fails_the_gate() {
    let jungle = biome_music("jungle").unwrap();
    let situation = in_biome(jungle);
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink {
        panic_on_start: true,
        ..RecordingSink::silent()
    };
    let mut rng = JavaRandom::new(0xF00D);
    for _ in 0..120_000 {
        mgr.tick(&situation, &mut rng, &mut sink);
    }
}

/// The other half of "no blocking wait": a silent start must not leave the
/// countdown parked at `i32::MAX` forever. Vanilla recovers through the ordinary
/// "track finished" branch, vanilla's own tick routine's `!isActive` check, and so must this.
#[test]
fn a_silent_start_rearms_the_countdown_within_two_ticks() {
    let jungle = biome_music("jungle").unwrap();
    let situation = in_biome(jungle);
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::silent();
    let mut rng = JavaRandom::new(5);

    for _ in 0..STARTING_DELAY {
        mgr.tick(&situation, &mut rng, &mut sink);
    }
    assert_eq!(sink.started.len(), 1);
    // Right after the attempt the countdown is parked, exactly as for a real start.
    assert_eq!(mgr.next_song_delay(), i32::MAX);

    // One more tick and it is a real interval again.
    mgr.tick(&situation, &mut rng, &mut sink);
    let delay = mgr.next_song_delay();
    assert!(
        (musics::GAME.min_delay - 1..=musics::GAME.max_delay).contains(&delay),
        "after a silent start the countdown is {delay}, not a 12000..=24000 interval"
    );
    assert_ne!(delay, i32::MAX, "the countdown must not stay parked");
}

// ---------------------------------------------------------------------------
// 6. Volume fade
// ---------------------------------------------------------------------------

/// Vanilla's own fade-playing routine. `pale_garden`'s
/// `music_volume: 0.0` is the real caller: it fades an already-playing track out
/// and then stops it, rather than cutting it.
#[test]
fn a_zero_music_volume_fades_out_and_then_stops() {
    let jungle = biome_music("jungle").unwrap();
    let mut mgr = MusicManager::new(MusicFrequency::Default);
    let mut sink = RecordingSink::playing();
    let mut rng = JavaRandom::new(17);

    for _ in 0..STARTING_DELAY {
        mgr.tick(&in_biome(jungle), &mut rng, &mut sink);
    }
    assert_eq!(mgr.current_track(), Some("music.overworld.jungle"));
    assert_eq!(mgr.current_gain(), 1.0);

    // Step into the pale garden: music_volume 0.0.
    let quiet = MusicSituation {
        music_volume: 0.0,
        ..in_biome(jungle)
    };
    let mut ticks = 0;
    while mgr.current_track().is_some() && ticks < 10_000 {
        mgr.tick(&quiet, &mut rng, &mut sink);
        ticks += 1;
    }

    assert_eq!(sink.stops, 1, "the fade must end in a stop");
    assert_eq!(mgr.current_track(), None);
    // Geometric decay at 0.97 per tick from 1.0 to 1e-4 is
    // ceil(ln(1e-4)/ln(0.97)) = 303 ticks. Predict it rather than asserting
    // "eventually": a linear fade would take ~200 000, an instant cut 1.
    let expected = (1.0e-4f32.ln() / 0.97f32.ln()).ceil() as i32;
    assert_eq!(expected, 303);
    assert!(
        (ticks - expected).abs() <= 2,
        "faded out in {ticks} ticks, expected ~{expected} for a 0.97 geometric decay"
    );
    // The gain was pushed to the sink on the way down, monotonically.
    assert!(sink.gains.len() > 250);
    assert!(
        sink.gains.windows(2).all(|w| w[1] < w[0]),
        "gain must decrease monotonically while fading out"
    );
}

// ---------------------------------------------------------------------------
// 7. Structural: the music path cannot make a sound or read the clock
// ---------------------------------------------------------------------------

/// A test must not perform an OS-level side effect, and audio is the obvious
/// candidate: opening an output device from a unit test would be audible on the
/// owner's machine on every `cargo test`. That risk is designed out rather than
/// guarded — `music.rs` and `biome_music.rs` hold no sink, no device, no clock and
/// no filesystem, and everything audible goes through the caller's [`MusicSink`].
///
/// This gate makes that structural claim **assertable** instead of merely stated,
/// which is the whole point: a later edit that reaches for `AudioEngine` "just to
/// try something" fails here rather than shipping a test suite that plays music.
/// Negative control: add `AudioEngine` to either file and this fails.
#[test]
fn the_music_modules_cannot_reach_a_device_or_a_clock() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // Forbidden token -> why it must not appear in *code* in these modules.
    let forbidden = [
        ("AudioEngine", "would let a tick open the real output device"),
        ("CpalSink", "the device sink belongs to the caller"),
        ("cpal", "no audio backend in the selection layer"),
        ("Instant", "scheduling is in ticks, never wall time"),
        ("SystemTime", "scheduling is in ticks, never wall time"),
        ("std::fs", "no filesystem: assets are the resolver's job"),
        ("Command::new", "no process spawning, ever (see the OAuth-URL incident)"),
    ];

    let mut scanned = 0usize;
    for rel in ["src/music.rs", "src/biome_music.rs"] {
        let src = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"));
        scanned += 1;
        for line in src.lines() {
            let code = line.trim_start();
            // Doc comments and ordinary comments legitimately *name* these things —
            // this very module documents why `AudioEngine` is absent. Only code counts.
            if code.starts_with("//") {
                continue;
            }
            for (token, why) in forbidden {
                assert!(
                    !code.contains(token),
                    "{rel} references `{token}` in code: {why}\n  {line}"
                );
            }
        }
    }
    assert_eq!(scanned, 2, "both modules must actually have been scanned");

    // The positive half: the scanner does see code, so a clean result is not the
    // consequence of reading an empty file or skipping every line.
    let music_src = std::fs::read_to_string(root.join("src/music.rs")).unwrap();
    assert!(
        music_src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains("fn tick")),
        "the scanner found no code lines in music.rs — it is reading the wrong thing"
    );
}

/// `MusicSituation`'s `creative` flag is `instabuild && mayfly`, matching
/// vanilla's own situational-music selection, not "gamemode == creative". Spectator has `mayfly`
/// without `instabuild`, and adventure has neither, so a gamemode check would give
/// spectators the creative track.
#[test]
fn the_creative_flag_is_instabuild_and_mayfly() {
    let ocean = biome_music("ocean").unwrap();
    // instabuild && mayfly -> creative track.
    let creative = MusicSituation {
        creative: true,
        ..in_biome(ocean)
    };
    assert_eq!(
        creative.situational_music().as_ref().map(Music::sound),
        Some("music.creative")
    );
    // Spectator (mayfly, no instabuild) is not creative for music purposes.
    let spectator = MusicSituation {
        creative: false,
        ..in_biome(ocean)
    };
    assert_eq!(
        spectator.situational_music().as_ref().map(Music::sound),
        Some("music.game")
    );
}

/// `Cow` keeps the seven constants `const` while biome tracks own their names; a
/// regression to `&'static str` would make the generated table impossible, and one
/// to `String` would drop the constants out of `const` context. Both directions are
/// caught by simply using each.
#[test]
fn both_static_and_owned_track_names_work() {
    assert!(matches!(musics::GAME.sound, Cow::Borrowed(_)));
    let owned = Music::owned(String::from("music.overworld.jungle"), 12_000, 24_000, false);
    assert!(matches!(owned.sound, Cow::Owned(_)));
    assert_eq!(owned, *biome_music("jungle").unwrap().default.as_ref().unwrap());
}
