//! Singleplayer worlds actually save — driven through the **shell's own**
//! session path, not through `IntegratedServer` (issue
//! [#468](https://github.com/matteopolak/lodestone/issues/468)).
//!
//! # Why this exists when #437 already gates persistence
//!
//! Issue #437 built world save/load and evidenced it in both directions
//! against a real Mojang 26.2 server. It reached **zero players**, because
//! `net.rs` opened every session through the *non-persistent* constructor.
//! That is this repo's dominant defect class — the island — one layer above
//! the code, and a server-side gate structurally cannot see it: the server
//! test constructs the persistent server itself, so it proves the thing it
//! constructs works, never that anything constructs it.
//!
//! So these gates start at [`NetClient::open_singleplayer`] — the same
//! function `app::launch_singleplayer` calls — and go through the real
//! `Origin::Integrated` arm, the real constructor choice, the real wire, and
//! the real end-of-session shutdown. **A mutation is made by sending a dig
//! over the wire and is read back over the wire**, so nothing here can pass by
//! consulting a world handle the product does not have.
//!
//! # The two things being asserted, and why the second is not optional
//!
//! 1. **Blocks survive.** Break a block, close the session, reopen: still
//!    broken.
//! 2. **The seed survives.** Generate with seed A, close, reopen *asking for a
//!    different seed B*, and check terrain the first session never generated
//!    still matches A. Without this, "saving" produces a world that is
//!    self-inconsistent at the edge of wherever the player explored — and gate
//!    1 cannot see it, because every block gate 1 checks is one that was saved.
//!
//! # Negative control
//!
//! Both gates were observed to fail with `net.rs` pointed back at
//! `open_in_memory_with_mobs`; see the commit message for the measured
//! output. That control is not automatable here without editing `net.rs` from
//! a test, which is worse than the thing it would prove.
//!
//! # Gotchas if you change these
//!
//! - **Never point them at [`lodestone::saves::default_world_dir`].** They
//!   pass an explicit temporary directory, so they cannot write into the
//!   developer's real `~/Library/Application Support/lodestone`. The
//!   `LODESTONE_DATA_DIR` route is deliberately not used: `std::env::set_var`
//!   is `unsafe` under edition 2024 and is process-global, so two tests in
//!   this binary would race.
//! - **View radii are kept tiny.** A composed column is expensive to generate
//!   (`docs/world-open-latency.md`), and these run in debug. Radius 1 is nine
//!   columns; radius 2 is twenty-five. Raising them is how this file becomes a
//!   multi-minute test.
//! - The seed gate compares **surface heights**, not block-state ids, because
//!   the client speaks numeric wire ids and `ChunkColumn` speaks block-name
//!   strings, and there is no id↔name mapping available to both. A height
//!   profile is derivable identically on both sides.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_client::{BlockPos, ChunkPos};
use lodestone_model::{BlockActionKind, BlockFace, ClientAction};
use lodestone_server::ChunkSource;

/// The join spawn column: `V770ServerProtocol::begin_play`'s hardcoded
/// `spawn_x`/`spawn_z`, which is why chunk `(0, 0)` is always streamed first.
const SPAWN_X: i32 = 8;
const SPAWN_Z: i32 = 8;

/// Comfortably above the overworld's 319 ceiling-most block, so a read here is
/// air in every world. Used to learn the wire id of air without a registry.
const DEFINITELY_AIR_Y: i32 = 310;

/// The top of the search when looking for the terrain surface.
const SURFACE_SEARCH_TOP: i32 = 300;
/// The bottom of that search — below the overworld floor.
const SURFACE_SEARCH_BOTTOM: i32 = -64;

/// Generous because a debug-profile column is slow to generate and these
/// deadlines must not become the thing that fails.
const SESSION_DEADLINE: Duration = Duration::from_secs(240);

/// Opens a real shell singleplayer session, or `None` when this build has no
/// hostable version family.
///
/// Mirrors `app::launch_singleplayer`, which is `pub(crate)` and so
/// unreachable from an integration test — the three lines are inlined rather
/// than the visibility being widened for a test's convenience.
fn open_session(seed: i64, view_radius: i32, world_dir: Option<PathBuf>) -> Option<NetClient> {
    let protocol = lodestone::Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)?;
    Some(NetClient::open_singleplayer(
        server_protocol,
        protocol,
        seed,
        view_radius,
        None,
        world_dir,
    ))
}

/// The `--no-default-features` contract: a build with no hostable family must
/// *report*, and in the default build reaching here is a failure, not a skip.
fn require_hostable(net: Option<NetClient>) -> Option<NetClient> {
    if net.is_none() {
        assert!(
            !cfg!(feature = "live"),
            "the default build must be able to host singleplayer"
        );
    }
    net
}

/// Pumps `net` until `ready` holds, collecting any reported errors.
///
/// Returns whether `ready` became true. Errors are collected rather than
/// ignored so a timeout's failure message carries the actual diagnosis instead
/// of only saying "timed out" — the reference test
/// `pressing_play_reaches_a_running_integrated_server` makes the same point.
fn pump_until(net: &NetClient, what: &str, mut ready: impl FnMut(&NetClient) -> bool) {
    let deadline = Instant::now() + SESSION_DEADLINE;
    let mut errors: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        for update in net.poll() {
            match update {
                NetUpdate::Error(e) => errors.push(e),
                NetUpdate::Disconnected(reason) => errors.push(format!("disconnected: {reason:?}")),
                _ => {}
            }
        }
        if ready(net) {
            assert!(
                errors.is_empty(),
                "reached `{what}` but the session reported errors: {errors:?}"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for `{what}`; errors: {errors:?}");
}

/// Waits for login and for `chunk` to be resident in the client's own world.
fn wait_for_chunk(net: &NetClient, chunk: ChunkPos) {
    pump_until(net, "the client's world to hold the chunk", |net| {
        net.is_chunk_loaded(chunk)
    });
}

/// The wire id of air, learned from a block that is air in every world rather
/// than assumed to be `0`.
fn air_id(net: &NetClient) -> u32 {
    net.block_at(BlockPos::new(SPAWN_X, DEFINITELY_AIR_Y, SPAWN_Z))
        .expect("a loaded chunk must answer for a y inside the world")
}

/// The highest non-air `y` at `(x, z)` **as the client sees it**.
fn client_surface_y(net: &NetClient, x: i32, z: i32, air: u32) -> Option<i32> {
    (SURFACE_SEARCH_BOTTOM..=SURFACE_SEARCH_TOP)
        .rev()
        .find(|&y| net.block_at(BlockPos::new(x, y, z)).is_some_and(|id| id != air))
}

/// The surface-height profile of chunk `(cx, cz)` at `samples`, **as the
/// generator produces it** for `seed` — computed with no reference to the
/// client, the server, or disk.
///
/// This is the outside-origin expectation the seed gate lands on: a session
/// that quietly used the wrong seed cannot agree with it.
///
/// The column is generated **once** and all samples read out of it. Generating
/// per sample would regenerate the same expensive column sixteen times per
/// seed, which is how this test would become a multi-minute one.
fn generated_surface_profile(seed: i64, cx: i32, cz: i32, samples: &[(i32, i32)]) -> Vec<Option<i32>> {
    let column = lodestone_server::overworld_chunk_source(seed).column(cx, cz);
    samples
        .iter()
        .map(|&(x, z)| {
            let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
            (column.min_y..column.min_y + column.height)
                .rev()
                .find(|&y| column.block_state(lx, y, lz) != "minecraft:air")
        })
        .collect()
}

/// A 4×4 sample grid inside chunk `(cx, cz)` — enough columns to separate two
/// seeds without paying for all 256.
fn sample_columns(cx: i32, cz: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for i in 0..4 {
        for j in 0..4 {
            out.push((cx * 16 + i * 4 + 2, cz * 16 + j * 4 + 2));
        }
    }
    out
}

/// How many chunk columns a region file actually contains, read straight out
/// of its 8 KiB header.
///
/// The header is 1024 big-endian `u32` location entries; a nonzero entry means
/// that column is present (`RegionFile.java`'s own emptiness test). Parsed by
/// hand here rather than through `lodestone-anvil`, which is not a dependency
/// of this crate — and adding one would edit `Cargo.lock`, which this change
/// has no business touching.
fn saved_column_count(region_file: &Path) -> usize {
    let bytes = std::fs::read(region_file).expect("region file is readable");
    assert!(
        bytes.len() >= 8192,
        "a region file shorter than its own 8 KiB header is corrupt: {} bytes",
        bytes.len()
    );
    (0..1024)
        .filter(|i| {
            let o = i * 4;
            u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) != 0
        })
        .count()
}

/// Breaks the block at `pos` by sending the same two actions the shell's own
/// mining driver sends, and waits for the server's block update to come back.
fn break_block_over_the_wire(net: &NetClient, pos: BlockPos, air: u32) {
    net.send_action(ClientAction::BlockAction {
        action: BlockActionKind::StartDestroy,
        pos,
        face: BlockFace::Up,
        sequence: 0,
    });
    net.send_action(ClientAction::BlockAction {
        action: BlockActionKind::StopDestroy,
        pos,
        face: BlockFace::Up,
        sequence: 1,
    });
    pump_until(net, "the server to confirm the break", |net| {
        net.block_at(pos) == Some(air)
    });
}

fn region_files(world_dir: &Path) -> Vec<PathBuf> {
    let region_dir = world_dir
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("region");
    let Ok(entries) = std::fs::read_dir(&region_dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mca"))
        .collect();
    out.sort();
    out
}

/// A temporary world directory that is removed when the test ends.
struct TempWorld(PathBuf);

impl TempWorld {
    fn new(tag: &str) -> Self {
        // A literal nonce per call site rather than a pid or a random: the
        // scratchpad and `std::env::temp_dir()` are shared, and a collision
        // between two runs would look like a persistence bug.
        let path = std::env::temp_dir().join(format!("lodestone-468-{tag}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp world dir");
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for TempWorld {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **Gate 1.** A block broken in one session is still broken in the next.
///
/// The whole of issue #468 in one assertion, at the layer the issue is about:
/// no `IntegratedServer` is named here, only `NetClient`.
#[test]
fn a_block_broken_in_one_session_is_still_broken_in_the_next() {
    let world = TempWorld::new("blocks");
    let seed = lodestone::menu::world_select::BUNDLED_WORLD.seed;

    // -- session one: break a block --------------------------------------
    let (broken_at, air, original) = {
        let Some(net) = require_hostable(open_session(seed, 1, Some(world.path()))) else {
            return;
        };
        wait_for_chunk(&net, ChunkPos { x: 0, z: 0 });
        let air = air_id(&net);
        let surface = client_surface_y(&net, SPAWN_X, SPAWN_Z, air)
            .expect("spawn column must have a surface");
        let pos = BlockPos::new(SPAWN_X, surface, SPAWN_Z);
        let original = net.block_at(pos).expect("surface block is readable");

        // Control for the gate below: if the surface block were already air,
        // "it is air after reopening" would be satisfied by a world that saved
        // nothing at all.
        assert_ne!(
            original, air,
            "the block chosen to break was already air, so this gate would pass vacuously"
        );

        break_block_over_the_wire(&net, pos, air);
        assert_eq!(net.block_at(pos), Some(air), "the break did not take effect");
        (pos, air, original)
        // `net` drops here: `NetClient::drop` joins the net thread, which now
        // awaits `IntegratedServer::shutdown()` and flushes the world. That
        // join is what makes the assertion below meaningful rather than racy.
    };

    assert!(
        !region_files(&world.path()).is_empty(),
        "the session ended without writing any region file, so nothing was saved at all"
    );

    // -- session two: the same world, reopened ---------------------------
    let Some(net) = require_hostable(open_session(seed, 1, Some(world.path()))) else {
        return;
    };
    wait_for_chunk(&net, ChunkPos { x: 0, z: 0 });

    let reopened = net.block_at(broken_at).expect("reopened chunk is readable");
    assert_eq!(
        reopened,
        air,
        "the broken block came back as {reopened} (it generates as {original}) — the world \
         did not save, or reopened through the non-persistent constructor"
    );
}

/// **Gate 2.** The seed survives, and governs chunks the first session never
/// generated.
///
/// Session one creates the world with seed A over a radius-1 view. Session two
/// reopens it **asking for a different seed B** over a radius-2 view, so chunk
/// `(2, 0)` is generated for the very first time in session two. Its terrain
/// must match A.
///
/// The `assert_ne!` on the two generated profiles is the load-bearing control:
/// without it, two seeds that happened to agree at these columns would make
/// the gate pass no matter which seed the session used.
#[test]
fn the_stored_seed_governs_chunks_the_first_session_never_generated() {
    let world = TempWorld::new("seed");
    let seed_a: i64 = 20_260_731;
    let seed_b: i64 = -8_123_456_789;

    // -- session one: create the world with seed A, touch nothing --------
    {
        let Some(net) = require_hostable(open_session(seed_a, 1, Some(world.path()))) else {
            return;
        };
        wait_for_chunk(&net, ChunkPos { x: 0, z: 0 });
        assert!(
            !net.is_chunk_loaded(ChunkPos { x: 2, z: 0 }),
            "chunk (2,0) must be outside session one's radius-1 view, or it is not a chunk \
             the first session never generated and this gate proves nothing"
        );
    }

    // Path spelled out rather than taken from
    // `lodestone_anvil::world_gen_settings::path_in`: `lodestone-anvil` is not
    // a dependency of this crate and making it one would edit `Cargo.lock`.
    // A drift between this literal and that function would show up as this
    // assertion failing, which is the right direction to fail in.
    let settings_path = world
        .path()
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat");
    assert!(
        settings_path.exists(),
        "session one wrote no world_gen_settings.dat, so the seed was never stored"
    );

    // -- session two: reopen asking for seed B ---------------------------
    let Some(net) = require_hostable(open_session(seed_b, 2, Some(world.path()))) else {
        return;
    };
    wait_for_chunk(&net, ChunkPos { x: 2, z: 0 });
    let air = air_id(&net);

    let samples = sample_columns(2, 0);

    let observed: Vec<Option<i32>> = samples
        .iter()
        .map(|&(x, z)| client_surface_y(&net, x, z, air))
        .collect();
    let expected_a = generated_surface_profile(seed_a, 2, 0, &samples);
    let expected_b = generated_surface_profile(seed_b, 2, 0, &samples);

    // The control: the two hypotheses must actually be distinguishable at
    // these columns, or agreement with A means nothing.
    assert_ne!(
        expected_a, expected_b,
        "seeds {seed_a} and {seed_b} produce identical terrain at the sampled columns, so \
         this gate could not tell them apart; pick different seeds"
    );

    assert_eq!(
        observed, expected_a,
        "chunk (2,0) does not match the stored seed {seed_a}. Seed {seed_b} would produce \
         {expected_b:?} — if that is what was observed, the requested seed overrode the \
         stored one and every unexplored chunk regenerates differently on each open."
    );
}

/// **The cost, as a count.** A session that mutates one column saves a number
/// of columns proportional to **mutation**, not to **residency**.
///
/// This is deliberately a count and not a duration: a timing taken while
/// sibling agents build is attributed to the wrong cause, and two sequential
/// durations are not protected by being expressed as a ratio.
///
/// # Both hypotheses, predicted from outside
///
/// A radius-1 view makes **nine** columns resident (`(-1..=1)²`), and exactly
/// **one** is mutated. So:
///
/// | hypothesis | saved columns |
/// |---|---|
/// | proportional to mutation (correct) | 1, plus any column a random tick also touched |
/// | proportional to residency (the defect) | 9 |
///
/// The bound is 4 rather than 1 because random ticks and the mob sim's grazing
/// genuinely do mutate the world, and they are supposed to be saved. It is
/// well clear of 9, so the two hypotheses are separated — which is the point;
/// asserting only "fewer than nine" would be the *magnitude* species of
/// vacuous test, satisfied by a save that wrote eight.
///
/// This does not re-derive #437's "a tick that mutates nothing writes nothing",
/// which is gated server-side. What it adds is that reaching the save path
/// through the **shell** did not change the proportionality.
#[test]
fn a_session_saves_columns_in_proportion_to_mutation_not_residency() {
    const RESIDENT_COLUMNS: usize = 9;
    const MUTATION_PROPORTIONAL_BOUND: usize = 4;

    let world = TempWorld::new("cost");
    let seed = lodestone::menu::world_select::BUNDLED_WORLD.seed;

    {
        let Some(net) = require_hostable(open_session(seed, 1, Some(world.path()))) else {
            return;
        };
        wait_for_chunk(&net, ChunkPos { x: 0, z: 0 });
        let air = air_id(&net);
        let surface =
            client_surface_y(&net, SPAWN_X, SPAWN_Z, air).expect("spawn column has a surface");
        break_block_over_the_wire(&net, BlockPos::new(SPAWN_X, surface, SPAWN_Z), air);
    }

    let files = region_files(&world.path());
    assert_eq!(
        files.len(),
        1,
        "every column of a radius-1 view is inside region (0,0), so one file is expected: {files:?}"
    );
    assert!(
        files[0].ends_with("r.0.0.mca"),
        "the spawn column belongs to region (0,0): {files:?}"
    );

    let saved = saved_column_count(&files[0]);
    assert!(
        saved >= 1,
        "the mutated column was not saved at all ({saved} columns on disk)"
    );
    assert!(
        saved <= MUTATION_PROPORTIONAL_BOUND,
        "saved {saved} columns for one mutation in a {RESIDENT_COLUMNS}-column view — that is \
         residency-proportional, not mutation-proportional, and it would write ~100 MiB per \
         autosave for a player standing still in a full {}-column store",
        512
    );
}
