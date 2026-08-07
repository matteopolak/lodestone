//! Does the cost of generating a chunk column depend on how far that column is
//! from the world origin? The curve, and the control that says the curve is
//! about distance and not about anything else.
//!
//! # Why this exists
//!
//! The owner reports the game getting "exponentially slower to generate as I
//! walk away from the spawn point". Every per-chunk cost this project has
//! optimised is a *constant* — allocations per warm column are 64, the ore
//! engine draws 503, climate does 278,298 comparisons — and none of them can
//! explain a term that grows with `|chunk coord|`. So either the claim is about
//! something outside the generator, or there is a coordinate-magnitude term
//! nothing here has ever measured. This file measures it.
//!
//! # The design, and what each arm rules out
//!
//! Two hypotheses produce the same complaint from a walking player and are
//! distinguished only by holding one variable at a time:
//!
//! * **H_dist** — cost is a function of `|cx|`/`|cz|`. Walking further makes each
//!   column cost more, and a fresh generator at a far coordinate is slow on its
//!   very first column.
//! * **H_age** — cost is a function of *how many columns this generator has
//!   already produced* (a structure that grows and is then scanned; the staged
//!   store's own [`STORE_RETENTION`] doc comment names "a session gradually
//!   exploring a large area" as a real if slow leak). Walking further means
//!   generating more, so the two are perfectly confounded in a real session.
//!
//! `distance_curve` varies coordinate with generator age held fixed: **a fresh
//! generator per band**, so every band's Nth column is the Nth column that
//! generator has ever produced. Any slope is H_dist and cannot be H_age.
//!
//! `control_distance_held_fixed` is the same procedure with the *coordinate*
//! held at the origin — nine fresh generators, nine six-column walks, all at
//! `(0..6, 0)`. It is the run where distance does not vary, and it must come out
//! flat. If it slopes, the instrument is measuring machine state (thermal, load,
//! allocator warmth) and no reading in `distance_curve` means anything. **Read
//! the control first.**
//!
//! # Wall clock, and why it is admissible here
//!
//! `CLAUDE.md` records this machine reproducing a wall-clock worldgen figure to
//! only 10.8%, with one stage swinging 22% across three runs of an identical
//! binary. That is a hard floor on believing any single ratio. It is also far
//! below the effect size under test: "exponentially slower" is a claim about
//! orders of magnitude, and a term that is invisible at 10.8% noise is not the
//! term the owner is feeling. So the rule for reading this output is: **a band
//! ratio under ~1.5× is noise and must not be reported as a finding**; only a
//! monotone trend spanning several bands counts.
//!
//! Nothing here asserts a timing threshold, for that reason. The gate assertion
//! is on `store_evictions` and on the *shape* being reported at all; the numbers
//! are printed for a human, per the same rule `freeze_coordinate_sweep` follows.
//!
//! # Running it
//!
//! ```text
//! cargo test --release -p lodestone-server \
//!   --test walk_distance_curve -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d: it is a multi-minute release-profile sweep, and its product is
//! a curve for a human to read rather than a pass/fail. Run it with
//! `--nocapture`; each band prints as it finishes, so a stall at a far band
//! still leaves the near bands' curve on screen.
//!
//! Do not add a test to this binary that generates terrain outside
//! [`walk_at`]: `lodestone_worldgen::counters` are process-global and
//! `store_len` is per generator, but wall clock is per *machine*, and a
//! concurrent sweep in the same binary lands squarely in this one's window.
//! `staged_store_counters.rs` paid for that lesson already.

use lodestone_server::overworld_generator;
use std::time::Instant;

/// Any seed; the effect under test is about coordinates, not about terrain.
const SEED: i64 = 0x5EED_1234;

/// Columns generated per band. Six, because the first column at a fresh band
/// pays the whole 5×5 pre-ore closure cold (25 chunks of stages 1–4) and only
/// later columns are the marginal cost a walking player actually feels — so the
/// run has to contain both, and be able to report them apart.
const COLUMNS_PER_BAND: usize = 6;

/// How many of the trailing columns make up the reported warm mean.
const WARM_TAIL: usize = 3;

/// Chunk-coordinate bands, geometric so a power law shows as a straight line on
/// a log axis. In blocks these are 0, 64, 256, 1,024, 4,096, 16,384, 65,536,
/// 262,144, 1,048,576 — the first five bracket where a player actually walks and
/// the rest exist to make a weak slope unmistakable. All are far inside the
/// vanilla world border (29,999,984 blocks).
const BANDS: [i32; 9] = [0, 4, 16, 64, 256, 1024, 4096, 16384, 65536];

/// This process's resident set size in MiB, or `-1.0` if it could not be read.
///
/// Shells to `ps` rather than taking a dependency: this is a diagnostic in an
/// `#[ignore]`d report, and a wrong-by-a-page answer changes nothing about a
/// curve measured in hundreds of MiB. It reads only this process's own
/// accounting and has no side effect — the `Command::new` here is not the
/// browser-opening species `CLAUDE.md` bans, but it is the reason this helper is
/// documented rather than inlined.
///
/// **`store_len` is the primary counter and RSS is corroboration, not the
/// reverse.** RSS is confounded by the allocator's own retention: a flat RSS
/// would not prove the store is not growing, only that malloc had not returned
/// pages. A growing RSS that tracks `store_len` is what makes the pair
/// convincing.
fn rss_mib() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map_or(-1.0, |kib| kib / 1024.0)
}

/// One band's measurement.
struct Band {
    /// Chunk coordinate the walk started at (`(band, band)`).
    band: i32,
    /// Wall time in milliseconds for each of the [`COLUMNS_PER_BAND`] columns,
    /// in generation order. Index 0 is the cold column.
    per_column_ms: Vec<f64>,
    /// Store entries held when the walk finished.
    store_len: usize,
    /// Entries the store dropped over the walk. Expected zero at this scale
    /// (six columns' closure is far under the 512-entry ceiling); a non-zero
    /// value invalidates the timings, because an eviction makes a later column
    /// recompute a stage rather than reuse it.
    evictions: usize,
}

impl Band {
    fn cold_ms(&self) -> f64 {
        self.per_column_ms[0]
    }

    /// Mean of the trailing [`WARM_TAIL`] columns — the marginal per-column cost
    /// a player walking through already-neighboured terrain pays.
    fn warm_mean_ms(&self) -> f64 {
        let tail = &self.per_column_ms[self.per_column_ms.len() - WARM_TAIL..];
        tail.iter().sum::<f64>() / tail.len() as f64
    }
}

/// Generates [`COLUMNS_PER_BAND`] columns walking `+x` from `(at, at)` on a
/// **freshly built generator**, timing each.
///
/// The fresh generator is the whole point: it holds generator age constant
/// across bands, so the only thing differing between two calls is the
/// coordinate. Building it is not counted — `OverworldGenerator::new` parses
/// settings and builds density trees, which has nothing to do with the
/// coordinate.
fn walk_at(at: i32) -> Band {
    let generator = overworld_generator(SEED);
    let mut per_column_ms = Vec::with_capacity(COLUMNS_PER_BAND);
    for i in 0..COLUMNS_PER_BAND {
        let cx = at + i as i32;
        let start = Instant::now();
        let column = generator.column(cx, at);
        // Read something out of the result so nothing above can be optimised
        // away as dead: a discarded `GeneratedColumn` is a real risk in release.
        std::hint::black_box(column.block_state(0, 0, 0));
        per_column_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    Band {
        band: at,
        per_column_ms,
        store_len: generator.store_len(),
        evictions: generator.store_evictions(),
    }
}

/// Prints one band's row and returns it, so a stalled sweep still leaves every
/// completed band's numbers on screen under `--nocapture`.
fn report(label: &str, band: Band) -> Band {
    let per = band
        .per_column_ms
        .iter()
        .map(|ms| format!("{ms:8.1}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "{label:>10} chunk=({:>7},{:>7}) blocks={:>9}  cold={:>9.1}ms  warm_mean={:>9.1}ms  \
         store_len={:>4} evictions={:>4}  per_column_ms=[{per}]",
        band.band,
        band.band,
        band.band * 16,
        band.cold_ms(),
        band.warm_mean_ms(),
        band.store_len,
        band.evictions,
    );
    band
}

/// **The control, and it must be read before the curve.** Nine fresh
/// generators, nine six-column walks, every one of them at the origin —
/// distance does not vary. A flat result here is what licenses reading a slope
/// in [`distance_curve`] as being about distance; a sloping result here means
/// the instrument is tracking machine state and the curve is worthless.
///
/// This is deliberately not an assertion on flatness: at a 10.8% wall-clock
/// reproducibility floor, a threshold tight enough to be meaningful would be
/// flaky and a threshold loose enough to be stable would pass a real 40% drift.
/// The `spread` line is the number to read, and the assertion is on the store
/// instead — a control on the *control*, since a non-zero eviction count would
/// make every band's warm mean incomparable.
#[test]
#[ignore = "multi-minute release-profile sweep; a curve for a human to read"]
fn control_distance_held_fixed() {
    println!(
        "\n=== CONTROL: distance held at origin, {} repeats of a {COLUMNS_PER_BAND}-column walk ===",
        BANDS.len()
    );
    let mut warm = Vec::new();
    let mut evicted_anywhere = 0usize;
    for repeat in 0..BANDS.len() {
        let band = report(&format!("repeat {repeat}"), walk_at(0));
        evicted_anywhere += band.evictions;
        warm.push(band.warm_mean_ms());
    }
    let lo = warm.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = warm.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "CONTROL warm_mean spread: min={lo:.1}ms max={hi:.1}ms ratio={:.2}x \
         (>1.5x means the instrument is not stable enough to read the curve)",
        hi / lo.max(f64::MIN_POSITIVE)
    );
    assert_eq!(
        evicted_anywhere, 0,
        "the store evicted during a six-column walk, so a later column recomputed \
         a stage instead of reusing it and no warm mean here is comparable"
    );
}

/// The curve: warm and cold per-column cost against `|chunk coord|`, with
/// generator age held fixed by a fresh generator per band.
///
/// Reports rather than asserts, for the reason in the module doc. The one thing
/// it does assert is that the sweep actually varied the coordinate and actually
/// generated — a run that silently produced identical work at every band would
/// otherwise print a beautifully flat curve and prove nothing, which is the
/// *world* species of vacuous test.
#[test]
#[ignore = "multi-minute release-profile sweep; a curve for a human to read"]
fn distance_curve() {
    println!(
        "\n=== CURVE: {COLUMNS_PER_BAND}-column walk per band, fresh generator per band ===",
    );
    let mut bands = Vec::new();
    for at in BANDS {
        bands.push(report("band", walk_at(at)));
    }

    let base = &bands[0];
    println!("\n=== CURVE, normalised to band 0 ===");
    for band in &bands {
        println!(
            "  blocks={:>9}  cold={:>6.2}x  warm={:>6.2}x",
            band.band * 16,
            band.cold_ms() / base.cold_ms().max(f64::MIN_POSITIVE),
            band.warm_mean_ms() / base.warm_mean_ms().max(f64::MIN_POSITIVE),
        );
    }

    // Non-degeneracy: every band really did generate terrain, and really did so
    // at a distinct coordinate. Without this, a generator that early-returned
    // air outside some bound would produce the flattest possible curve.
    for band in &bands {
        assert!(
            band.per_column_ms.iter().all(|&ms| ms > 0.0),
            "band {} recorded a zero-duration column, so it cannot have generated",
            band.band
        );
        assert!(
            band.store_len > 0,
            "band {} left an empty store, so no stage was memoised and nothing ran",
            band.band
        );
    }
}

/// Columns walked by [`age_curve`]. Chosen against [`STORE_RETENTION`] (512),
/// not as a round number: one column's pre-ore closure is 5×5, so a straight
/// walk of `n` columns reaches roughly `5 * (n + 4)` distinct chunks at the
/// pre-ore stage. 400 columns therefore drives the store to about 2,020 entries
/// *wanted* against a 512-entry ceiling — nearly 4× over — which is the only
/// region where the retention path can be observed doing anything at all. A walk
/// that stayed under the ceiling would exercise the ceiling exactly zero times
/// and report a flat line, and that flat line would mean nothing.
const AGE_WALK_COLUMNS: usize = 400;

/// Interval at which [`age_curve`] reports. 20 columns is 320 blocks — about the
/// granularity at which a walking player would notice a change.
const AGE_REPORT_EVERY: usize = 20;

/// **`store_len` and per-column cost against columns generated, on ONE
/// generator** — the arm [`distance_curve`] deliberately holds fixed, and the
/// one that matches a real session: `OverworldChunkSource` builds a generator
/// once per world and keeps it for the world's whole lifetime, so a player
/// walking away from spawn is not nine fresh generators, it is one generator
/// getting older with every column.
///
/// [`distance_curve`] having come out flat is what makes this the live
/// hypothesis rather than a completeness exercise. It also means the two arms
/// together are a real separation: coordinate varies with age fixed → flat, so
/// any slope here is attributable to age and not to coordinate. To keep that
/// attribution clean this walk deliberately does **not** stay near the origin —
/// it starts at the origin and walks outward exactly as a player does, and the
/// flat result from the other arm is what licenses reading the result as being
/// about age anyway.
///
/// `STORE_RETENTION`'s own doc comment predicts the failure this looks for, in
/// so many words: "a session gradually exploring a large area would otherwise
/// grow this without bound, a real if slow leak". The question is whether the
/// pinning-plus-reclamation scheme that replaced the FIFO actually releases as
/// the view moves. `store_len` is the counter that answers it, and it answers it
/// independent of any timing.
#[test]
#[ignore = "multi-minute release-profile sweep; a curve for a human to read"]
fn age_curve() {
    println!(
        "\n=== AGE: one generator, {AGE_WALK_COLUMNS}-column straight walk from the origin ===\n\
         (store ceiling STORE_RETENTION=512; a straight walk of n columns wants ~5*(n+4) entries)"
    );
    let generator = overworld_generator(SEED);
    let mut window = Vec::with_capacity(AGE_REPORT_EVERY);
    let mut first_report: Option<f64> = None;

    for i in 0..AGE_WALK_COLUMNS {
        let start = Instant::now();
        let column = generator.column(i as i32, 0);
        std::hint::black_box(column.block_state(0, 0, 0));
        window.push(start.elapsed().as_secs_f64() * 1000.0);

        if window.len() == AGE_REPORT_EVERY {
            let mean = window.iter().sum::<f64>() / window.len() as f64;
            let base = *first_report.get_or_insert(mean);
            println!(
                "  columns={:>4} blocks={:>6}  mean_ms={mean:>8.1}  vs_first={:>5.2}x  \
                 store_len={:>6} evictions={:>6}  rss_mib={:>7.1}",
                i + 1,
                (i + 1) * 16,
                mean / base.max(f64::MIN_POSITIVE),
                generator.store_len(),
                generator.store_evictions(),
                rss_mib(),
            );
            window.clear();
        }
    }

    println!(
        "AGE final: store_len={} evictions={}",
        generator.store_len(),
        generator.store_evictions()
    );

    // Non-degeneracy, and the only thing here worth asserting: a 400-column walk
    // must have driven the store past its own ceiling. If it did not, this test
    // never reached the retention path and its flat line would be vacuous — the
    // *world* species, invisible in the source.
    assert!(
        generator.store_len() + generator.store_evictions() > STORE_RETENTION_UNDER_TEST,
        "a {AGE_WALK_COLUMNS}-column walk reached only {} live + {} evicted entries, which is \
         under the {STORE_RETENTION_UNDER_TEST}-entry ceiling — this run never exercised \
         retention at all, so nothing it printed is evidence about it",
        generator.store_len(),
        generator.store_evictions(),
    );
}

/// View radius the [`view_walk_curve`] arm simulates. 8 is a normal client
/// render distance and gives a 17×17 = 289-column view — the same 289-column
/// figure `4307b59` names and that `STORE_RETENTION` was sized against.
const VIEW_RADIUS: i32 = 8;

/// Chunk steps [`view_walk_curve`] walks. 100 steps is 1,600 blocks — a short
/// stroll, deliberately: the point is to measure the *rate* per block and let the
/// reader extrapolate, not to drive this machine into swap. `CLAUDE.md` is
/// explicit that unbounded test memory has force-rebooted this machine, so this
/// arm is bounded on purpose and the extrapolation is stated rather than run.
const VIEW_WALK_STEPS: i32 = 100;

/// **The owner-faithful rate: a moving view, not a line of single columns.**
///
/// [`age_curve`] walks one column at a time, which understates the real leak by
/// about 4×. A real player has a whole `(2R+1)²` view that slides: each chunk of
/// travel brings a full *column-strip* of `2R+1` newly-visible columns, and each
/// of those opens its own 5×5 pin. This arm reproduces that, so the MiB-per-block
/// figure it reports is the one that applies to the owner's session.
///
/// It generates exactly the columns a sliding square view newly exposes, which is
/// the set `ViewTracker::recenter` diffs out as `next.difference(&self.loaded)` in
/// `crates/lodestone-server/src/server.rs`. It does **not** go through
/// `ViewTracker` itself — that path needs a live `ServerProtocol` and a
/// connection, and the term under test is in the generator the tracker calls, so
/// routing through the tracker would add transport cost without adding evidence.
/// The coordinates are the same set; that equality is the thing to check if this
/// arm is ever doubted.
#[test]
#[ignore = "multi-minute release-profile sweep; a curve for a human to read"]
fn view_walk_curve() {
    println!(
        "\n=== VIEW WALK: one generator, sliding {}x{} view, {VIEW_WALK_STEPS} chunk steps ===",
        2 * VIEW_RADIUS + 1,
        2 * VIEW_RADIUS + 1
    );
    let generator = overworld_generator(SEED);
    let rss_start = rss_mib();

    // The initial view, as a join would produce it.
    for dz in -VIEW_RADIUS..=VIEW_RADIUS {
        for dx in -VIEW_RADIUS..=VIEW_RADIUS {
            std::hint::black_box(generator.column(dx, dz).block_state(0, 0, 0));
        }
    }
    println!(
        "  after join view ({} columns): store_len={} evictions={} rss_mib={:.1}",
        (2 * VIEW_RADIUS + 1) * (2 * VIEW_RADIUS + 1),
        generator.store_len(),
        generator.store_evictions(),
        rss_mib()
    );

    for step in 1..=VIEW_WALK_STEPS {
        // Walking +x by one chunk exposes exactly the leading edge strip.
        let cx = step + VIEW_RADIUS;
        for dz in -VIEW_RADIUS..=VIEW_RADIUS {
            std::hint::black_box(generator.column(cx, dz).block_state(0, 0, 0));
        }
        if step % 20 == 0 {
            let rss = rss_mib();
            println!(
                "  step={step:>4} blocks={:>6}  store_len={:>6} evictions={:>6}  rss_mib={rss:>7.1}  \
                 kib_per_block={:>7.1}",
                step * 16,
                generator.store_len(),
                generator.store_evictions(),
                (rss - rss_start) * 1024.0 / (step * 16) as f64,
            );
        }
    }

    println!(
        "VIEW WALK final: store_len={} evictions={} rss_mib={:.1} (started {rss_start:.1})",
        generator.store_len(),
        generator.store_evictions(),
        rss_mib()
    );

    // Non-degeneracy: the walk must have driven the store past its ceiling, or
    // this printed a flat line about a regime it never entered.
    assert!(
        generator.store_len() > STORE_RETENTION_UNDER_TEST,
        "store_len {} never exceeded the {STORE_RETENTION_UNDER_TEST}-entry ceiling",
        generator.store_len()
    );
}

/// `STORE_RETENTION` restated locally because it is private to
/// `lodestone_worldgen::overworld`. **This is a duplicated constant and that is
/// a hazard**: if the real ceiling changes, the non-degeneracy assertion above
/// silently checks the wrong number. It is only used to prove the walk went
/// *past* the ceiling, so being stale in the low direction weakens the check
/// rather than breaking it — but re-read `overworld/mod.rs`'s `STORE_RETENTION`
/// before trusting it.
const STORE_RETENTION_UNDER_TEST: usize = 512;
