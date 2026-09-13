//! Reclamation changes *when* a stage product is dropped, never what it
//! contains. This is the gate that says so with real terrain, and the two-arm
//! dumper that says it against a build where reclamation is switched off.
//!
//! # What it is
//!
//! `docs/worldgen-store-distance-leak.md`: `StagedStore`'s
//! 512-entry retention ceiling was unreachable from `open_view`, the only
//! insertion path `OverworldGenerator::column` uses, so `reclaim` never ran in a
//! real session and the store grew without bound as a player walked. The fix
//! makes `open_view` check the ceiling once its whole box is pinned. That turns
//! reclamation on for the first time on the path the game uses, which raises a
//! question no existing gate answers: **does a stage recomputed after its entry
//! was evicted produce the same bytes as the first time it ran?**
//!
//! It has to, because a slot's value is a pure function of its key and the
//! generator's fixed state — that purity is the store's central assumption, and
//! the whole "eviction can only ever cost a recompute" argument in
//! `overworld/store.rs`'s module doc rests on it. An assumption load-bearing
//! enough to license dropping data is worth measuring rather than restating.
//!
//! # How it works
//!
//! [`reclaimed_columns_regenerate_byte_identically`] walks a **1-D strip** with
//! one generator, which is the cheapest shape that overflows the ceiling: a
//! straight walk of `n` columns closes over roughly `5 · (n + 4)` chunks at the
//! pre-ore stage, so [`STRIP`] = 140 wants ~720 entries against 512 and forces
//! about 200 evictions. The first [`SAMPLE`] columns' wire bytes are captured
//! while the store is still cold and has evicted nothing; the strip then walks
//! far enough that those entries are the oldest unpinned ones in the store and
//! are reclaimed; then the same coordinates are regenerated and compared.
//!
//! Three things make the comparison mean something, and each is asserted:
//!
//! * **Reclamation actually happened** — `store_evictions() > 0`. Without it the
//!   test is a re-run of `column_is_byte_identical_across_two_independently_constructed_generators`
//!   and says nothing about eviction. This is the detector control.
//! * **The sampled entries were themselves evicted**, not merely *some* entries:
//!   `store_len()` is bounded by the ceiling while the strip is 140 columns
//!   long, so the early columns cannot still be resident.
//! * **The bytes are not degenerate** — distinct columns must differ from each
//!   other, or byte-equality would hold under any change at all.
//!
//! # The two-arm dumper
//!
//! Set `LODESTONE_STORE_WALK_DUMP` and the same test writes its sample to that
//! path, so the working tree (reclamation on) and the same tree with
//! `open_view`'s ceiling check reverted (reclamation off, the shipped defect)
//! can be compared with `cmp`/`md5`. That comparison is the one
//! `u15_column_dump.rs` structurally cannot make: its scene is a fresh generator
//! per seed over a 3×3 patch, a 49-entry closure, so it never reaches the
//! ceiling on **either** arm and is byte-identical whatever `open_view` does.
//! A dump from a scene that cannot exercise the change is the *world* species of
//! vacuous test, which is the same species that hid this defect in the first
//! place — so both dumps are worth running and only this one is evidence about
//! reclamation.
//!
//! # How to change it
//!
//! [`STRIP`] is derived from `STORE_RETENTION` via the `5 · (n + 4)` closure and
//! must stay comfortably past it; shortening it below ~100 columns silently
//! stops reaching the retention path and the test goes green while measuring
//! nothing. Widening the sample or the strip is free and strictly better.
//! Do **not** hash the dump inside this file — `cmp` on raw bytes is what makes
//! a mismatch localisable to a column.
//!
//! `#[ignore]`d: ~10 s in `--release` and several times that unopposed in a
//! debug build, which is too slow for `cargo test --workspace`. The fast,
//! always-run guard for the *boundedness* half lives in
//! `overworld/store.rs`'s `a_view_walked_across_the_world_stays_inside_the_retention_ceiling`.
//!
//! ```text
//! cargo test --release -p lodestone-server \
//!   --test store_reclaim_identity -- --ignored --nocapture \
//!   reclaimed_columns_regenerate_byte_identically
//! ```
//!
//! **Name the test.** A bare `--ignored` run of this binary reports one failure,
//! and it is not a defect: [`dump_walked_strip`] `expect`s
//! `LODESTONE_STORE_WALK_DUMP` and panics without it, the same convention
//! `u15_column_dump.rs`'s dumper follows. It is an `expect` rather than a silent
//! early return on purpose — a dumper that skipped itself when unconfigured is
//! the *precondition* species of vacuous test, and a harness whose dump never
//! got written would look like it had passed.

use lodestone_server::overworld_generator;

/// Seed 42 is the coordinate the checked-in JVM density dump anchors, so the
/// terrain compared here is known-verified rather than arbitrary.
const SEED: i64 = 42;

/// Columns in the strip. **Derived from `STORE_RETENTION` (512), not picked**: a
/// straight `+x` walk of `n` columns closes over ~`5 · (n + 4)` chunks at the
/// pre-ore stage, so the ceiling is first crossed at `n ≈ 99`. 140 wants ~720
/// entries — about 40% past it, enough that the early columns are long gone.
const STRIP: i32 = 140;

/// Columns whose bytes are captured and later re-compared. The first 20 of the
/// strip: the oldest entries in the store, hence the first reclaimed.
const SAMPLE: i32 = 20;

/// `STORE_RETENTION`, restated because it is private to
/// `lodestone_worldgen::overworld` — the same duplicated-constant hazard
/// `walk_distance_curve.rs` documents. Used only as an upper bound the strip
/// must be shown to have crossed, so a stale low value weakens this rather than
/// breaking it.
const STORE_RETENTION_UNDER_TEST: usize = 512;

/// One column's whole wire-facing product, as `u15_column_dump.rs` frames it:
/// `(min_y, height, palette, blocks, biomes)` from `GeneratedColumn::into_raw`,
/// not an internal structure. Palette **order** reaches the wire, so a change
/// that permuted the palette while placing identical blocks is caught here and
/// would be missed by a block-set comparison.
fn column_bytes(generator: &lodestone_worldgen::overworld::OverworldGenerator, cx: i32, cz: i32) -> Vec<u8> {
    let column = generator.column(cx, cz);
    let (min_y, height, palette, blocks, biomes) = column.into_raw();
    let mut out = Vec::new();
    out.extend_from_slice(b"COL:");
    out.extend_from_slice(&cx.to_le_bytes());
    out.extend_from_slice(&cz.to_le_bytes());
    out.extend_from_slice(&min_y.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    for state in &palette {
        out.extend_from_slice(&(state.len() as u32).to_le_bytes());
        out.extend_from_slice(state.as_bytes());
    }
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for b in &blocks {
        out.extend_from_slice(&b.to_le_bytes());
    }
    for biome in &biomes {
        out.extend_from_slice(&(biome.len() as u32).to_le_bytes());
        out.extend_from_slice(biome.as_bytes());
    }
    out
}

/// **A column regenerated after its store entry was reclaimed must be
/// byte-identical to the column generated before any reclamation had happened.**
///
/// See the module doc for why this is the one gate that exercises the retention
/// path with real terrain, and for the three assertions that keep it from being
/// vacuous.
#[test]
#[ignore = "~10s release-profile strip walk; the byte-identity evidence for #503's reclamation fix"]
fn reclaimed_columns_regenerate_byte_identically() {
    let generator = overworld_generator(SEED);

    // Phase 1: the sample, generated cold, before the store has evicted
    // anything. These are the reference bytes.
    let mut before: Vec<Vec<u8>> = Vec::with_capacity(SAMPLE as usize);
    for cx in 0..SAMPLE {
        before.push(column_bytes(&generator, cx, 0));
    }
    let evictions_at_sample = generator.store_evictions();
    println!(
        "phase 1: {SAMPLE} columns generated cold, store_len={} evictions={evictions_at_sample}",
        generator.store_len()
    );
    assert_eq!(
        evictions_at_sample, 0,
        "the reference sample was itself taken after {evictions_at_sample} evictions, so it \
         is not a pre-reclamation baseline"
    );

    // Phase 2: keep walking, well past the ceiling, so the sample's entries
    // become the oldest unpinned ones and are reclaimed.
    for cx in SAMPLE..STRIP {
        std::hint::black_box(generator.column(cx, 0).block_state(0, 0, 0));
    }
    let (len_after_walk, evicted_after_walk) = (generator.store_len(), generator.store_evictions());
    println!(
        "phase 2: strip of {STRIP} columns walked, store_len={len_after_walk} \
         evictions={evicted_after_walk}"
    );

    // The detector control. A run that never reclaimed would pass every
    // byte comparison below and prove nothing about reclamation.
    assert!(
        evicted_after_walk > 0,
        "a {STRIP}-column strip evicted nothing (store_len {len_after_walk} against a \
         {STORE_RETENTION_UNDER_TEST}-entry ceiling), so this run never reached the retention \
         path and the identity comparison below is vacuous"
    );
    // And the sample specifically cannot still be resident: the strip wants
    // ~5*(STRIP+4) entries and the store is holding far fewer.
    assert!(
        len_after_walk <= STORE_RETENTION_UNDER_TEST + 25,
        "store_len {len_after_walk} is not inside the ceiling, so the store may still hold \
         the sample and the regeneration below may be a cache hit rather than a recompute"
    );

    // Phase 3: regenerate the sample. Every one of these is now a real
    // recompute of a stage whose memoised product was dropped.
    let mut mismatches = Vec::new();
    let mut after: Vec<Vec<u8>> = Vec::with_capacity(SAMPLE as usize);
    for cx in 0..SAMPLE {
        let bytes = column_bytes(&generator, cx, 0);
        if bytes != before[cx as usize] {
            let at = bytes
                .iter()
                .zip(&before[cx as usize])
                .position(|(a, b)| a != b)
                .map_or_else(
                    || format!("length {} vs {}", bytes.len(), before[cx as usize].len()),
                    |off| format!("first differing byte at offset {off}"),
                );
            mismatches.push(format!("({cx},0): {at}"));
        }
        after.push(bytes);
    }
    println!(
        "phase 3: {SAMPLE} columns regenerated after reclamation, evictions={}",
        generator.store_evictions()
    );

    // Non-degeneracy: distinct columns must differ from one another, or
    // byte-equality above would be satisfied by a dump of pure air.
    let distinct: std::collections::HashSet<&Vec<u8>> = before.iter().collect();
    assert!(
        distinct.len() >= SAMPLE as usize - 1,
        "only {} of {SAMPLE} sampled columns are distinct — the scene is too uniform for a \
         byte comparison to detect anything",
        distinct.len()
    );
    assert!(
        before.iter().all(|b| b.len() > 4096),
        "a sampled column serialised to under 4 KiB, which is not a generated column"
    );

    assert!(
        mismatches.is_empty(),
        "reclamation changed generated output — a stage is not a pure function of its key: {}",
        mismatches.join("; ")
    );

    println!(
        "IDENTITY OK: {SAMPLE} columns byte-identical across {evicted_after_walk} evictions, \
         store_len={len_after_walk} (regeneration itself forced {} more)",
        generator.store_evictions() - evicted_after_walk
    );
    std::hint::black_box(after);
}

/// View radius of the concurrent burst below. 10 gives a 21×21 = 441-column
/// burst whose pre-ore closure is 25×25 = **625** chunks — past the 512-entry
/// ceiling, so reclamation runs *during* the burst. The existing
/// `staged_store_gates.rs` burst is deliberately R=8 (289 columns, a 441-entry
/// closure) so that it evicts nothing; this one is deliberately the opposite.
const BURST_RADIUS: i32 = 10;

/// **Reclaiming under concurrency must not change results.**
///
/// `reclaim` now runs on the `open_view` path, which is the path 8 worker
/// threads hammer during a join burst, so for the first time eviction and
/// concurrent generation happen at the same instant. The argument that this is
/// safe is structural — every `entry` call happens inside the calling thread's
/// own pin, and `reclaim` skips pinned entries and entries whose `Arc` is held
/// elsewhere, so a pass cannot drop a slot another thread is computing into —
/// but a structural argument about a lock-free interleaving is exactly the kind
/// worth measuring rather than restating.
///
/// The comparison is against a **serial** generator over the same coordinates.
/// Note that the serial arm reclaims too (same 625-entry closure), so what this
/// gate isolates is specifically *concurrent* reclamation against *serial*
/// reclamation. "Reclaiming at all versus not reclaiming" is the question
/// [`reclaimed_columns_regenerate_byte_identically`] and the two-arm dumper
/// answer; the two together cover both axes.
///
/// Memory: the serial bytes are held (~87 MiB) and each parallel column is
/// compared and dropped immediately rather than collected, because `CLAUDE.md`
/// records unbounded test memory force-rebooting this machine.
#[test]
#[ignore = "441 columns x2 of real embedded-data generation with reclamation live; ~1 min in release"]
fn a_concurrent_burst_past_the_ceiling_matches_serial_bytes() {
    let r = BURST_RADIUS;
    // Offset well away from every other gate's coordinates in this crate so the
    // two cannot share terrain or a store.
    let coords: Vec<(i32, i32)> = (-r..=r)
        .flat_map(|cx| (-r..=r).map(move |cz| (cx + 9000, cz + 9000)))
        .collect();
    assert_eq!(coords.len(), ((2 * r + 1) * (2 * r + 1)) as usize);

    let serial = overworld_generator(SEED);
    let expected: std::collections::HashMap<(i32, i32), Vec<u8>> = coords
        .iter()
        .map(|&(cx, cz)| ((cx, cz), column_bytes(&serial, cx, cz)))
        .collect();
    let (serial_len, serial_evicted) = (serial.store_len(), serial.store_evictions());
    println!(
        "serial arm: {} columns, store_len={serial_len} evictions={serial_evicted}",
        coords.len()
    );
    drop(serial);

    let expected = std::sync::Arc::new(expected);
    let parallel = std::sync::Arc::new(overworld_generator(SEED));
    let chunk_size = coords.len().div_ceil(8);
    let mut handles = Vec::new();
    for slice in coords.chunks(chunk_size) {
        let slice: Vec<(i32, i32)> = slice.to_vec();
        let generator = std::sync::Arc::clone(&parallel);
        let expected = std::sync::Arc::clone(&expected);
        handles.push(std::thread::spawn(move || {
            let mut bad = Vec::new();
            let mut seen = 0usize;
            for (cx, cz) in slice {
                let bytes = column_bytes(&generator, cx, cz);
                let want = &expected[&(cx, cz)];
                if &bytes != want {
                    let at = bytes
                        .iter()
                        .zip(want)
                        .position(|(a, b)| a != b)
                        .map_or_else(
                            || format!("length {} vs {}", bytes.len(), want.len()),
                            |off| format!("offset {off}"),
                        );
                    bad.push(format!("({cx},{cz}) differs at {at}"));
                }
                seen += 1;
            }
            (seen, bad)
        }));
    }
    let mut seen = 0usize;
    let mut bad = Vec::new();
    for h in handles {
        let (n, b) = h
            .join()
            .expect("a burst worker panicked — a deadlock on the once-guard under reclamation, \
                     or a poisoned shard lock");
        seen += n;
        bad.extend(b);
    }

    let (par_len, par_evicted) = (parallel.store_len(), parallel.store_evictions());
    println!("parallel arm: {seen} columns, store_len={par_len} evictions={par_evicted}");
    assert_eq!(seen, coords.len(), "the burst did not cover every column");

    // The detector control, on **both** arms: if neither reclaimed, this gate is
    // a re-run of the existing R=8 burst and says nothing about reclamation
    // under concurrency.
    assert!(
        serial_evicted > 0 && par_evicted > 0,
        "serial evicted {serial_evicted} and parallel evicted {par_evicted} — a burst that \
         reclaimed on neither arm never entered the regime this gate exists for"
    );
    assert!(
        par_len <= STORE_RETENTION_UNDER_TEST + 25,
        "store_len {par_len} escaped the ceiling under concurrency, so reclamation is not \
         keeping up with the `open_view` path when many threads insert at once"
    );
    assert!(
        bad.is_empty(),
        "{} of {seen} columns differ between a concurrent burst and the serial answer with \
         reclamation live: {}",
        bad.len(),
        bad.join("; ")
    );
    println!(
        "CONCURRENT IDENTITY OK: {seen} columns identical; serial evicted {serial_evicted}, \
         parallel evicted {par_evicted}"
    );
}

/// **The two-arm dumper**: the same strip walk, written to
/// `LODESTONE_STORE_WALK_DUMP` for `cmp`/`md5` against another build of this
/// same file.
///
/// Separate from the gate above rather than folded into it, for a mechanical
/// reason: the arm this has to be compared against is a tree with `open_view`'s
/// ceiling check reverted, where the gate's "reclamation actually happened"
/// detector fires *before* any bytes could be written. A dumper must therefore
/// assert nothing about eviction — it **prints** the eviction count instead, and
/// the harness reads it. That printed pair is the detector control for the
/// comparison: `evictions=0` on the reverted arm and non-zero here is what says
/// the two dumps were produced under genuinely different store behaviour, and
/// without it two byte-identical dumps would only show that neither arm
/// reclaimed.
///
/// It dumps the sample **after** the strip walk, so on the fixed arm every
/// column in the dump is a post-reclamation recompute.
#[test]
#[ignore = "two-arm byte-identity dumper for #503; driven by a shell harness, not by cargo test"]
fn dump_walked_strip() {
    let path = std::env::var("LODESTONE_STORE_WALK_DUMP")
        .expect("set LODESTONE_STORE_WALK_DUMP to the output path for this arm");
    let generator = overworld_generator(SEED);
    for cx in 0..STRIP {
        std::hint::black_box(generator.column(cx, 0).block_state(0, 0, 0));
    }
    let (len, evicted) = (generator.store_len(), generator.store_evictions());
    let mut out = Vec::new();
    for cx in 0..SAMPLE {
        out.extend_from_slice(&column_bytes(&generator, cx, 0));
    }
    // Non-degeneracy travels with the dump, so an arm cannot compare equal by
    // both being empty or both being air.
    assert!(
        out.len() > SAMPLE as usize * 4096,
        "dump is only {} bytes for {SAMPLE} columns — too small to be generated terrain",
        out.len()
    );
    std::fs::write(&path, &out).expect("write dump");
    println!(
        "STORE WALK DUMP: strip={STRIP} sample={SAMPLE} bytes={} store_len={len} \
         evictions={evicted} -> {path}",
        out.len()
    );
}
