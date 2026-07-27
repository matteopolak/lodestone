//! Native output-device smoke test for [`AudioEngine`].
//!
//! `#[ignore]` because it opens the machine's real default output device, which
//! a headless CI box may not have. But per the project's Rule 1, an ignored test
//! that has been *asked* to run (`-- --ignored`) treats a missing precondition
//! as a **failure, not a skip**: if no device is present, [`AudioEngine::new`]
//! errors and this test `panic!`s with the reason, rather than printing to
//! stderr and reporting `ok`.
//!
//! # Why this asserts on voice drain, not "play returned Ok"
//!
//! The load-bearing hazard the maintainer named: a test asserting "the mixer
//! received a play request" **passes with the output device disconnected**. So
//! this gate is deliberately placed *downstream of the physical device*. A
//! non-looping voice is removed from the mixer only inside
//! [`Mixer::render`](lodestone_audio::Mixer::render), and `render` is called
//! **only** by the `cpal` realtime callback on the device thread. Therefore
//! `voice_count` falling to 0 is itself proof that the device pulled and
//! rendered the whole sound — it cannot happen if the stream never runs.
//!
//! The `paused` control gives that its teeth (§12.53: a non-event is not
//! evidence without a control proving the mechanism would have fired). While the
//! device is paused the voice must **not** drain; only after `resume` does it
//! reach 0. Without the control, an auto-expiring voice (a wall-clock timer, a
//! frame counter advanced by something other than the device) would satisfy the
//! drain assertion while proving nothing about the device. With it, a drain that
//! happens while paused fails the test.

#![cfg(not(target_arch = "wasm32"))]

use std::thread::sleep;
use std::time::Duration;

use glam::Vec3;
use lodestone_assets::MemorySource;
use lodestone_assets::sound::SoundRegistry;
use lodestone_model::event::SoundCategory;
use lodestone_sound::AudioEngine;

/// The same real (synthetic, non-silent) 0.5 s stereo chirp the decode
/// validation uses. Embedded at compile time: a missing fixture is a **compile
/// error**, never a runtime skip.
const CHIRP_OGG: &[u8] = include_bytes!("fixtures/chirp_stereo_44100.ogg");

const SOUNDS_JSON: &str = r#"{
    "block.stone.break": { "sounds": [ { "name": "block/stone/break1" } ] }
}"#;

fn build_engine() -> AudioEngine {
    let registry = SoundRegistry::parse(SOUNDS_JSON.as_bytes()).expect("parse sounds.json");
    let mut source = MemorySource::new("test");
    source.insert(
        "assets/minecraft/sounds/block/stone/break1.ogg",
        CHIRP_OGG.to_vec(),
    );
    AudioEngine::new(registry, Box::new(source)).unwrap_or_else(|e| {
        panic!(
            "requires a default audio output device to run this ignored test \
             (got: {e}). Run on a machine with audio, or omit `--ignored`."
        )
    })
}

/// Polls `voice_count` every 20 ms up to `max_polls` times, returning as soon as
/// the predicate holds. Uses `sleep`, never a wall clock — the audio crate's
/// `Instant::now(` ban is about the shipped engine reading real time to drive
/// the mixer, which stays sample-driven; a test sleeping between polls does not
/// touch that path.
fn poll_until(engine: &AudioEngine, max_polls: u32, done: impl Fn(usize) -> bool) -> usize {
    let mut count = engine.with_mixer(|m| m.voice_count());
    let mut polls = 0;
    while polls < max_polls && !done(count) {
        sleep(Duration::from_millis(20));
        count = engine.with_mixer(|m| m.voice_count());
        polls += 1;
    }
    count
}

#[test]
#[ignore = "opens the real default audio output device"]
fn the_output_device_actually_pulls_and_drains_a_played_sound() {
    let mut engine = build_engine();
    assert_eq!(
        engine.with_mixer(|m| m.voice_count()),
        0,
        "engine must start with no live voices"
    );

    // Enqueue the 0.5 s chirp at the listener origin (full, un-attenuated).
    let handle = engine
        .play_sound(
            "block.stone.break",
            SoundCategory::Block,
            Vec3::ZERO,
            1.0,
            1.0,
            42,
        )
        .expect("play must not error")
        .expect("event resolves to a file");
    assert!(handle.0 > 0, "a real voice handle");
    assert_eq!(
        engine.decoded_file_count(),
        1,
        "exactly one ogg decoded — proves real decode work, not a coincidental buffer"
    );
    assert!(
        engine.with_mixer(|m| m.voice_count()) >= 1,
        "the voice is live immediately after play, before the device drains it"
    );

    // --- Control: while paused, the device renders nothing, so the voice must
    // NOT drain. This is what proves the drain below is *device-driven* and not
    // an auto-expiring timer. ~1 s of real time (50 * 20 ms) is far longer than
    // the 0.5 s clip would take to finish if anything were rendering it.
    engine.pause().expect("pause the device");
    let while_paused = poll_until(&engine, 50, |c| c == 0);
    assert!(
        while_paused >= 1,
        "voice drained while the device was paused ({while_paused} left) — the drain \
         is not device-driven, so this gate would pass with the device disconnected"
    );

    // --- Now resume: the device thread must pull the whole clip and remove the
    // finished voice. Budget ~3 s (150 * 20 ms) for a 0.5 s sound plus device
    // buffering and scheduling slack.
    engine.resume().expect("resume the device");
    let after_resume = poll_until(&engine, 150, |c| c == 0);
    assert_eq!(
        after_resume, 0,
        "the running device did not drain the voice — either it never pulled \
         samples (disconnected/failed stream) or render is not advancing voices"
    );
}
