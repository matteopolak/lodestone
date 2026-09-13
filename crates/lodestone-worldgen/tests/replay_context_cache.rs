//! Focused control for the bounded immutable FEATURES replay context.
//!
//! The test is ignored because it drives the embedded production generator.
//! It proves that preparing an authenticated-style admission set lazily builds
//! one context, reuses it for repeated source completions, and preserves the
//! source result byte-for-byte against an unprepared generator.

use lodestone_server::overworld_generator;
use std::time::Instant;

const SEED: i64 = 42;
const SOURCES: [(i32, i32); 2] = [(0, 0), (-8, -8)];

#[test]
#[ignore = "embedded production generation; run explicitly with --ignored"]
fn lifecycle_replay_context_is_lazy_reused_and_byte_identical() {
    let prepared = overworld_generator(SEED);
    prepared.prepare_lifecycle_replay(&SOURCES);
    assert_eq!(prepared.lifecycle_replay_contexts_ready(), 0);

    let first = SOURCES.map(|source| {
        prepared.parity_source_decoration_with_overrides(
            source.0,
            source.1,
            source.0,
            source.1,
            &[],
        )
    });
    assert_eq!(
        prepared.lifecycle_replay_contexts_ready(),
        SOURCES.len(),
        "the first completion for each source must materialize one prepared context"
    );

    let prepared_warm_start = Instant::now();
    let second = SOURCES.map(|source| {
        prepared.parity_source_decoration_with_overrides(
            source.0,
            source.1,
            source.0,
            source.1,
            &[],
        )
    });
    let prepared_warm_us = prepared_warm_start.elapsed().as_micros();
    assert_eq!(
        prepared.lifecycle_replay_contexts_ready(),
        SOURCES.len(),
        "repeating source completions must reuse their immutable contexts"
    );

    let unprepared = overworld_generator(SEED);
    // Warm the unprepared generator's terrain-prefix store first. This keeps
    // the timing below focused on rebuilding the immutable context rather than
    // charging the no-cache arm for its first 5x5 terrain computation.
    let _ = SOURCES.map(|source| {
        unprepared.parity_source_decoration_with_overrides(
            source.0,
            source.1,
            source.0,
            source.1,
            &[],
        )
    });
    let unprepared_warm_start = Instant::now();
    let unprepared_results = SOURCES.map(|source| {
        unprepared.parity_source_decoration_with_overrides(
            source.0,
            source.1,
            source.0,
            source.1,
            &[],
        )
    });
    let unprepared_warm_us = unprepared_warm_start.elapsed().as_micros();
    println!(
        "REPLAY_CONTEXT_BENCH sources={} prepared_warm_us={} unprepared_warm_us={}",
        SOURCES.len(),
        prepared_warm_us,
        unprepared_warm_us,
    );
    for (index, source) in SOURCES.into_iter().enumerate() {
        assert_eq!(
            first[index],
            unprepared_results[index],
            "prepared replay changed source {source:?}"
        );
        assert_eq!(
            second[index],
            unprepared_results[index],
            "reusing the replay context changed source {source:?}"
        );
        assert!(
            !unprepared_results[index].spills.is_empty()
                || !unprepared_results[index].block_entities.is_empty(),
            "source-result comparison for {source:?} must exercise a non-empty production feature result"
        );
    }
}
