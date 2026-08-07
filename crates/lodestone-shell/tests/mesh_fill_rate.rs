//! **How long does it take, standing still, for the whole render distance to
//! reach the GPU as meshed geometry?** — the owner's own metric, measured on
//! the real path.
//!
//! # Why this exists
//!
//! The reported defect is "standing still, only ~10 chunks in each direction
//! render, and additional ones take over a minute". A `chore` commit
//! (`45a93e4`) once recorded "~13s from first frame to full sections,
//! bottleneck is mesh submission rate" and then **deleted the diagnostics that
//! produced the figure**, so the number became unobservable. This file makes it
//! observable again and keeps it that way.
//!
//! # What it drives, and what it deliberately omits
//!
//! Everything from the integrated server down to `Sim::drain_meshes` is the
//! **real production path**, reached the same way `app::begin_singleplayer`
//! reaches it:
//!
//! ```text
//! NetClient::open_singleplayer(view_radius = render_distance + 1)
//!   → Sim::attach_net → Sim::step
//!       → run_schedule(Update) → FrameSet::Terrain → heal_dirty_columns
//!       → poll_net → on_column_arrived → mark_column_dirty
//!                                      + mark_neighbours_dirty
//!   → Sim::drain_meshes            (records `uploaded_sections`, as the app does)
//! ```
//!
//! It omits exactly one thing: `RenderState::upload_section` and the draw
//! loops, because there is no GPU here. That makes every number in this file an
//! **optimistic bound** on the real client — the shell additionally pays buffer
//! creation per drained mesh and an unculled draw call per resident section
//! (`gpu/frame.rs`), both of which lengthen the frame and therefore *reduce*
//! how many `heal_dirty_columns` budgets fit in a second. If this harness says
//! the fill is slow, the real client is slower. That direction is the whole
//! reason the omission is acceptable.
//!
//! # Which instrument is the claim depends on what is limiting, and the
//! harness reports which
//!
//! `heal_dirty_columns` drains a fixed `DIRTY_COLUMN_BUDGET` **per frame**, so
//! *if the mesher is the constraint* the frame-rate-independent quantity is
//! frames-to-fill and `seconds ≈ frames / fps` is the wait a player sees. That
//! was the assumption this harness was built on, and **the first run falsified
//! it**: `frames with a full queue` came back `0 / 26,168,839`. The queue was
//! never full, the worker pool was idle, and meshing was never the constraint —
//! so the frame count is an artifact of how fast this harness spins
//! (unthrottled, ~10^5 fps) and converting it to seconds invents a fictional
//! number. The output therefore only makes that conversion when the budget was
//! actually saturated, and says so explicitly when it was not.
//!
//! When meshing is not the constraint, **wall time and the `store` column are
//! the claim**: `store` is how many columns the server has actually delivered,
//! and a plateau there localises the defect upstream of everything this file
//! owns. (Wall clock on this machine reproduces to only ~10.8%, so lean on the
//! shape and the counters; the effect measured here is "6.3s versus never",
//! which is far above that floor.)
//!
//! # Reading the output
//!
//! `--nocapture` prints, per ring of columns around the spawn column:
//! how many of that ring's columns have had at least one section meshed, the
//! frame and second at which the ring completed, and the incremental cost of
//! the ring. Plus the two aggregate numbers that separate the hypotheses:
//!
//! * **mesh events per section** — 1.0 means every section was built once.
//!   Substantially above 1.0 is the neighbour-invalidation cascade: columns
//!   being rebuilt because a neighbour arrived, each rebuild costing a full
//!   fresh mesh.
//! * **peak dirty-queue depth** — how far `heal_dirty_columns` fell behind.
//!
//! # Gotchas
//!
//! * `#[ignore]`d: it opens a real server and generates hundreds of columns, so
//!   it is minutes long in debug and is a measurement rather than a fast gate.
//!   Run it with `--release` or the generation cost dominates the mesh cost and
//!   the answer is about the wrong subsystem.
//! * It writes to its own directory under `std::env::temp_dir()` and **never**
//!   `saves::default_world_dir()`, so it cannot touch the developer's real
//!   world. A fresh directory means every column generates cold, which is the
//!   honest first-join case.
//! * The render distance is read from `Config::default()`, not hardcoded, so
//!   this measures whatever the shipped default actually is.
//!
//! ```text
//! cargo test -p lodestone-shell --release --test mesh_fill_rate -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use lodestone::mesher::TerrainMesh;
use lodestone::net::NetClient;
use lodestone::sim::Sim;

/// `V770ServerProtocol::begin_play`'s hardcoded spawn, which puts the player in
/// column `(0, 0)` — the centre every ring in the report is measured from.
const SPAWN_COLUMN: (i32, i32) = (0, 0);

/// Long enough that a slow fill is *measured* rather than truncated into a
/// timeout, since the whole point is to put a number on "over a minute".
///
/// The first run of this harness plateaued at 1.8s and then made **zero**
/// progress for the remaining 598s of a 600s deadline, so a long deadline buys
/// nothing but wall time; it is kept generous enough to prove a plateau is a
/// plateau and no more.
const DEADLINE: Duration = Duration::from_secs(120);

/// The frame delta handed to `Sim::step`. It paces physics and the 20 Hz tick
/// loop; it is **not** what paces meshing, which is one heal budget per call.
const FRAME_DT: f64 = 1.0 / 60.0;

/// A realistic client frame rate, used only to turn the frame count into the
/// seconds a player would actually experience.
const ASSUMED_FPS: f64 = 60.0;

/// The control knob, read from the environment so the two arms are two
/// invocations of one binary rather than two tests sharing a process.
///
/// Setting `LODESTONE_MESH_FILL_TICK_SPEED=0` sends `gamerule
/// random_tick_speed 0` as soon as the session is in-world, which makes
/// `RandomTickScheduler::tick_chunk` return at its `tick_speed == 0` guard
/// (`random_tick.rs:347`) **before** reaching
/// `section_has_randomly_ticking_block` — the 4096-block-per-section string
/// scan that the profile attributes 97.6% of the server tick thread to.
///
/// This is the negative control for the whole diagnosis, and it is a *game
/// rule*, not a code edit: nothing else about the build changes between arms.
/// If the random-tick scan is not what starves chunk delivery, this arm
/// plateaus exactly like the default one and the diagnosis is wrong.
const TICK_SPEED_ENV: &str = "LODESTONE_MESH_FILL_TICK_SPEED";

/// Self-deleting temp world directory.
struct TempWorld(std::path::PathBuf);

impl TempWorld {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("lodestone-mesh-fill-{tag}"));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for TempWorld {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One frame's worth of observation.
struct Sample {
    frame: u64,
    secs: f64,
    /// Distinct columns with at least one section meshed — the owner's metric.
    columns: usize,
    /// Distinct section keys meshed at least once.
    sections: usize,
    /// Cumulative mesh completions, including rebuilds of an already-meshed key.
    mesh_events: u64,
    dirty: usize,
    forced: usize,
    pending: usize,
    deferred: u64,
    /// Columns resident in the **client's** chunk store. This is the
    /// disambiguation that matters: a plateau with `store` also flat means the
    /// server stopped delivering, whereas a plateau with `store` still climbing
    /// would mean delivery is fine and the mesher is dropping columns.
    store: usize,
}

/// Chebyshev ring index of a column around [`SPAWN_COLUMN`] — ring `k` is the
/// square shell of `8k` columns at distance `k` (ring 0 being the centre).
fn ring_of(cx: i32, cz: i32) -> i32 {
    (cx - SPAWN_COLUMN.0).abs().max((cz - SPAWN_COLUMN.1).abs())
}

/// Columns in ring `k`: `1` for the centre, `8k` otherwise.
fn ring_size(k: i32) -> usize {
    if k == 0 { 1 } else { 8 * k as usize }
}

fn open_session(view_radius: i32, world_dir: std::path::PathBuf, sim: &Sim) -> Option<NetClient> {
    let protocol = lodestone::Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)?;
    Some(NetClient::open_singleplayer(
        server_protocol,
        protocol,
        0,
        view_radius,
        Some((sim.ecs().clone(), sim.local_player())),
        Some(world_dir),
    ))
}

#[test]
#[ignore = "measurement: opens a real integrated server and generates the whole view"]
fn standing_still_the_whole_render_distance_meshes_and_we_report_how_long_it_took() {
    let config = lodestone::Config::default();
    let render_distance = config.render_distance;
    // `app/session.rs`'s `begin_singleplayer`: the server streams one ring wider
    // than the render distance, because the outermost ring is the buffer the
    // mesher's neighbour-complete invariant consumes and never draws.
    let view_radius = i32::try_from(render_distance).unwrap().saturating_add(1);
    // What the player can actually see: `render_distance` in each direction.
    let visible_radius = i32::try_from(render_distance).unwrap();
    let visible_columns = (2 * visible_radius as usize + 1).pow(2);

    println!(
        "\n=== mesh fill rate ===\n\
         render_distance   {render_distance} chunks  (Config::default(), i.e. the shipped value)\n\
         server view_radius {view_radius} (render_distance + 1, the mesher's buffer ring)\n\
         streamed columns  {}\n\
         visible columns   {visible_columns}  ({}x{}, the fill target)\n\
         heal budget       {} forced + {} dirty columns per frame\n",
        (2 * view_radius as usize + 1).pow(2),
        2 * visible_radius + 1,
        2 * visible_radius + 1,
        lodestone::mesher::DIRTY_COLUMN_BUDGET,
        lodestone::mesher::DIRTY_COLUMN_BUDGET,
    );

    let world = TempWorld::new("fill");
    let mut sim = Sim::new(config);
    let Some(net) = open_session(view_radius, world.0.clone(), &sim) else {
        assert!(
            !cfg!(feature = "live"),
            "the default build must be able to host singleplayer"
        );
        return;
    };
    sim.attach_net(net);

    // Every section key that has ever come back from the worker pool, and how
    // many times. `> 1` for a key is a rebuild, which is the cascade signal.
    let mut section_events: HashMap<lodestone::mesher::SectionKey, u32> = HashMap::new();
    let mut columns_meshed: HashSet<(i32, i32)> = HashSet::new();
    // Ring -> (columns of that ring meshed, frame at which it completed).
    let mut ring_columns: BTreeMap<i32, HashSet<(i32, i32)>> = BTreeMap::new();
    let mut ring_done: BTreeMap<i32, (u64, f64)> = BTreeMap::new();
    let mut samples: Vec<Sample> = Vec::new();

    let mut mesh_events: u64 = 0;
    let mut frame: u64 = 0;
    let mut peak_dirty = 0usize;
    let mut peak_pending = 0usize;
    // Frames in which the dirty budget was fully consumed, i.e. the queue was
    // the limiter rather than the arrival rate.
    let mut budget_saturated_frames: u64 = 0;

    let tick_speed_override = std::env::var(TICK_SPEED_ENV).ok();
    match &tick_speed_override {
        Some(v) => println!(
            "ARM: control — will send `gamerule random_tick_speed {v}` once in-world\n"
        ),
        None => println!("ARM: default — vanilla `random_tick_speed` (3), nothing sent\n"),
    }
    let mut rule_sent = false;

    let start = Instant::now();
    let mut filled_at: Option<(u64, f64)> = None;

    while start.elapsed() < DEADLINE {
        frame += 1;
        // The app's frame, in the app's order (`app/redraw.rs:32..101`).
        sim.step(FRAME_DT);
        let _ = sim.drain_removals();
        let drained = sim.drain_meshes();

        for meshed in &drained {
            mesh_events += 1;
            *section_events.entry(meshed.key).or_insert(0) += 1;
            let col = (meshed.key.cx, meshed.key.cz);
            if columns_meshed.insert(col) && ring_of(col.0, col.1) <= visible_radius {
                let k = ring_of(col.0, col.1);
                ring_columns.entry(k).or_default().insert(col);
            }
        }

        let (dirty, forced, pending, deferred, store) = lodestone_ecs::hold_read(sim.ecs(), |w| {
            let store = w
                .get_resource::<lodestone_ecs::chunks::ChunkWorld>()
                .map_or(0, lodestone_ecs::chunks::ChunkWorld::len);
            match w.get_resource::<TerrainMesh>() {
                Some(t) => (
                    t.dirty_columns.len(),
                    t.forced_columns.len(),
                    t.scheduler.pending(),
                    t.deferred,
                    store,
                ),
                None => (0, 0, 0, 0, store),
            }
        });
        // Sent once the session is demonstrably in-world (a column has landed),
        // because a command sent during login is dropped rather than queued.
        if !rule_sent && store > 0 {
            if let (Some(v), Some(net)) = (tick_speed_override.as_deref(), sim.net()) {
                net.send_action(lodestone_model::ClientAction::SendCommand {
                    command: format!("gamerule random_tick_speed {v}"),
                });
                println!("[control] sent `gamerule random_tick_speed {v}` at frame {frame}");
            }
            rule_sent = true;
        }

        peak_dirty = peak_dirty.max(dirty);
        peak_pending = peak_pending.max(pending);
        if dirty >= lodestone::mesher::DIRTY_COLUMN_BUDGET {
            budget_saturated_frames += 1;
        }

        let secs = start.elapsed().as_secs_f64();
        let visible_meshed = ring_columns.values().map(HashSet::len).sum::<usize>();

        // Note each ring's completion the first time it is full.
        for k in 0..=visible_radius {
            if !ring_done.contains_key(&k)
                && ring_columns.get(&k).map_or(0, HashSet::len) == ring_size(k)
            {
                ring_done.insert(k, (frame, secs));
            }
        }

        samples.push(Sample {
            frame,
            secs,
            columns: visible_meshed,
            sections: section_events.len(),
            mesh_events,
            dirty,
            forced,
            pending,
            deferred,
            store,
        });

        if visible_meshed >= visible_columns && filled_at.is_none() {
            filled_at = Some((frame, secs));
            break;
        }
    }

    let total_secs = start.elapsed().as_secs_f64();
    let visible_meshed = ring_columns.values().map(HashSet::len).sum::<usize>();

    // --- the curve -------------------------------------------------------
    println!("--- meshed columns against time (standing still) ---");
    println!(
        "{:>8}  {:>9}  {:>8}  {:>9}  {:>7}  {:>7}  {:>7}  {:>8}  {:>8}",
        "frame", "secs", "columns", "sections", "store", "dirty", "forced", "pending", "events"
    );
    // A readable number of rows: every sample early, then thinned.
    let step = (samples.len() / 40).max(1);
    for (i, s) in samples.iter().enumerate() {
        if i % step == 0 || i + 1 == samples.len() {
            println!(
                "{:>8}  {:>9.3}  {:>8}  {:>9}  {:>7}  {:>7}  {:>7}  {:>8}  {:>8}",
                s.frame,
                s.secs,
                s.columns,
                s.sections,
                s.store,
                s.dirty,
                s.forced,
                s.pending,
                s.mesh_events
            );
        }
    }

    // --- per-ring breakdown ----------------------------------------------
    println!("\n--- per-ring completion (the shape claim) ---");
    println!(
        "{:>5}  {:>8}  {:>7}  {:>10}  {:>10}  {:>12}  {:>12}",
        "ring", "columns", "meshed", "frame", "secs", "d(frame)", "d(secs)"
    );
    let mut prev: Option<(u64, f64)> = None;
    for k in 0..=visible_radius {
        let meshed = ring_columns.get(&k).map_or(0, HashSet::len);
        match ring_done.get(&k) {
            Some(&(f, s)) => {
                let (df, ds) = prev.map_or((f, s), |(pf, ps)| (f - pf, s - ps));
                println!(
                    "{k:>5}  {:>8}  {meshed:>7}  {f:>10}  {s:>10.3}  {df:>12}  {ds:>12.3}",
                    ring_size(k)
                );
                prev = Some((f, s));
            }
            None => println!(
                "{k:>5}  {:>8}  {meshed:>7}  {:>10}  {:>10}  {:>12}  {:>12}",
                ring_size(k),
                "NEVER",
                "-",
                "-",
                "-"
            ),
        }
    }

    // --- the aggregates that separate the hypotheses ----------------------
    let sections = section_events.len();
    let rebuilt: usize = section_events.values().filter(|&&n| n > 1).count();
    let events_per_section = if sections == 0 {
        0.0
    } else {
        mesh_events as f64 / sections as f64
    };
    let max_rebuilds = section_events.values().copied().max().unwrap_or(0);
    // Per *column*, which is the unit `dirty_columns` and the owner both speak.
    let mut column_events: HashMap<(i32, i32), u32> = HashMap::new();
    for (key, n) in &section_events {
        *column_events.entry((key.cx, key.cz)).or_insert(0) += n;
    }
    let sections_per_column = if column_events.is_empty() {
        0.0
    } else {
        sections as f64 / column_events.len() as f64
    };

    println!("\n--- aggregates ---");
    println!("visible columns meshed      {visible_meshed} / {visible_columns}");
    println!("distinct sections meshed    {sections}");
    println!("mesh events (incl. rebuild) {mesh_events}");
    println!("mesh events per section      {events_per_section:.3}   <- 1.0 means no rebuild cascade");
    println!("sections rebuilt at least 1x {rebuilt} / {sections}");
    println!("most rebuilds of one section {max_rebuilds}");
    println!("sections per column          {sections_per_column:.2}");
    println!("re-mesh events per column    {:.3}", if column_events.is_empty() { 0.0 } else { mesh_events as f64 / column_events.len() as f64 });
    println!("peak dirty-queue depth       {peak_dirty}");
    println!("peak worker backlog          {peak_pending}");
    println!("frames with a full queue     {budget_saturated_frames} / {frame}");
    println!("deferred first-builds        {}", samples.last().map_or(0, |s| s.deferred));
    println!("client store columns         {} / 361 streamed", samples.last().map_or(0, |s| s.store));
    println!("session phase                {:?}", sim.session_phase());

    match filled_at {
        Some((f, s)) => println!(
            "\nFILLED: {visible_columns} columns in {f} frames / {s:.2}s of harness wall time."
        ),
        None => println!(
            "\nDID NOT FILL: {visible_meshed} / {visible_columns} columns after {frame} frames / {total_secs:.1}s."
        ),
    }

    // Whether the frame count converts to player-visible seconds depends
    // entirely on *what was limiting*, and the harness knows which:
    //
    // * `budget_saturated_frames > 0` — the per-frame heal budget was the
    //   limiter at least sometimes, so frames are the meaningful unit and
    //   `frames / fps` is the wait a player would see.
    // * `budget_saturated_frames == 0` — the queue was never full, so the
    //   mesher was *never* the constraint and the frame count is an artifact of
    //   how fast this harness happens to spin (it runs unthrottled, orders of
    //   magnitude above a real frame rate). Converting it to seconds would
    //   invent an enormous, entirely fictional number. Wall time is the only
    //   meaningful figure in that case, and the limiter is upstream of meshing.
    if budget_saturated_frames > 0 {
        let f = filled_at.map_or(frame, |(f, _)| f);
        println!(
            "The heal budget was the limiter in {budget_saturated_frames} frames, so at \
             {ASSUMED_FPS:.0} fps those {f} frames are ~{:.1}s of play.",
            f as f64 / ASSUMED_FPS
        );
    } else {
        println!(
            "The heal budget was NEVER the limiter (0 saturated frames), so meshing was not \
             the constraint and the frame count is a spin artifact, not a wait. Read the wall \
             time, and look upstream of the mesher for the term."
        );
    }

    // This is a measurement, and the only thing it *asserts* is that the
    // measurement happened — a fill target reached, or an honest report of how
    // far it got. Turning the observed number into a threshold is a separate
    // decision that belongs with whoever fixes it, because a threshold picked
    // from a broken baseline locks the break in.
    assert!(
        visible_meshed > 0,
        "no column meshed at all in {frame} frames / {total_secs:.1}s — this harness \
         measured nothing, which is a harness failure and not a slow fill"
    );
}
