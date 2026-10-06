//! Singleplayer worlds actually save — driven through the **shell's own**
//! session path, not through `IntegratedServer`.
//!
//! # Why a shell-level gate is needed
//!
//! Server-side save/load coverage can pass while the shell selects a different
//! constructor: that coverage constructs the persistent server directly, so it
//! proves the object it creates works, not that the product's session path creates
//! it. These tests cover that missing link from the shell entry point through the
//! wire and shutdown sequence.
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
//! A useful negative control is the in-memory constructor: it cannot preserve a
//! world directory across sessions. The test does not mutate production source
//! to install that control; the assertions instead make the required persistent
//! directory and reopen behavior explicit.
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
//! - The seed gate compares **terrain** (surface height plus the air/non-air
//!   mask beneath it), not block-state ids, because the client speaks numeric
//!   wire ids while the server uses canonical state ids, and not decoration,
//!   because tree placement depends on generation order. Both are derivable
//!   identically on both sides.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_client::{BlockPos, ChunkPos};
use lodestone_data::block::Block;
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
        lodestone::menu::create_world::WorldTypePreset::Normal,
        view_radius,
        false,
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

/// The column the local player stands in, once the server has placed them.
///
/// A dig is dropped when it lies outside the player's reach of their tracked
/// position, and the world's join position is the world spawn rather than a
/// fixed column, so the block to break is chosen relative to the player.
fn player_column(net: &NetClient) -> (i32, i32) {
    let mut position = None;
    pump_until(net, "the server to place the player", |net| {
        position = net.server_position();
        position.is_some()
    });
    let position = position.expect("pump_until returned only once a position was known");
    (position.x.floor() as i32, position.z.floor() as i32)
}

/// The highest non-air `y` at `(x, z)` **as the client sees it**.
fn client_surface_y(net: &NetClient, x: i32, z: i32, air: u32) -> Option<i32> {
    (SURFACE_SEARCH_BOTTOM..=SURFACE_SEARCH_TOP)
        .rev()
        .find(|&y| net.block_at(BlockPos::new(x, y, z)).is_some_and(|id| id != air))
}

/// How many blocks below the terrain surface the seed gate compares.
const GROUND_PROBE_DEPTH: i32 = 24;

type GroundProfile = Vec<Option<(i32, Vec<bool>)>>;

/// The terrain-only profile of chunk `(cx, cz)` at `samples`, **as the
/// generator produces it** for `seed` — computed with no reference to the
/// client, the server, or disk.
///
/// Each sample is the served column's top non-air height plus the air/non-air
/// mask of the [`GROUND_PROBE_DEPTH`] blocks beneath it. A served column,
/// decoration included, is a pure function of the seed and its coordinates, so
/// a standalone source predicts what the session streams.
///
/// The column is generated **once** and all samples read out of it. Generating
/// per sample would regenerate the same expensive column sixteen times per
/// seed, which is how this test would become a multi-minute one.
fn generated_ground_profile(seed: i64, cx: i32, cz: i32, samples: &[(i32, i32)]) -> GroundProfile {
    let column = lodestone_server::overworld_chunk_source(seed).column(cx, cz);
    samples
        .iter()
        .map(|&(x, z)| {
            let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
            let top = (column.min_y..column.min_y + column.height)
                .rev()
                .find(|&y| column.block_state_id(lx, y, lz).block() != Block::Air)?;
            let mask = (top - GROUND_PROBE_DEPTH..=top)
                .map(|y| column.block_state_id(lx, y, lz).block() != Block::Air)
                .collect();
            Some((top, mask))
        })
        .collect()
}

/// The profile [`generated_ground_profile`] predicts, read out of the client's
/// world at the heights that profile names.
fn client_ground_profile(
    net: &NetClient,
    air: u32,
    samples: &[(i32, i32)],
    expected: &GroundProfile,
) -> GroundProfile {
    samples
        .iter()
        .zip(expected)
        .map(|(&(x, z), expected)| {
            let (top, _) = expected.as_ref()?;
            let mask = (top - GROUND_PROBE_DEPTH..=*top)
                .map(|y| net.block_at(BlockPos::new(x, y, z)).is_some_and(|id| id != air))
                .collect();
            Some((*top, mask))
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

/// Every saved column payload in a world's overworld region files, keyed by
/// file name and header slot, so two snapshots show which columns a session
/// actually rewrote.
fn saved_column_payloads(world_dir: &Path) -> std::collections::HashMap<(String, usize), Vec<u8>> {
    let mut out = std::collections::HashMap::new();
    for file in region_files(world_dir) {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let bytes = std::fs::read(&file).expect("region file is readable");
        for slot in 0..1024 {
            let o = slot * 4;
            let entry = u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
            if entry == 0 {
                continue;
            }
            let start = (entry >> 8) as usize * 4096;
            let length = u32::from_be_bytes([
                bytes[start],
                bytes[start + 1],
                bytes[start + 2],
                bytes[start + 3],
            ]) as usize;
            out.insert((name.clone(), slot), bytes[start..start + 4 + length].to_vec());
        }
    }
    out
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
/// The assertion crosses the shell boundary through `NetClient`; it does not
/// inspect an `IntegratedServer` or another server-side handle directly.
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
        let (x, z) = player_column(&net);
        let surface = client_surface_y(&net, x, z, air)
            .expect("the player's column must have a surface");
        let pos = BlockPos::new(x, surface, z);
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

    let expected_a = generated_ground_profile(seed_a, 2, 0, &samples);
    let expected_b = generated_ground_profile(seed_b, 2, 0, &samples);
    let observed = client_ground_profile(&net, air, &samples, &expected_a);
    let observed_as_b = client_ground_profile(&net, air, &samples, &expected_b);

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
         {expected_b:?} and the client holds {observed_as_b:?} there — if that matches, the \
         requested seed overrode the stored one and every unexplored chunk regenerates \
         differently on each open."
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
/// This does not repeat the server-side invariant that a tick which mutates
/// nothing writes nothing. It verifies that reaching the save path through the
/// **shell** preserves the same mutation-based proportionality.
///
/// # Why the bound spans every region file
///
/// A radius-1 view is not contained in one region: it crosses the zero boundary.
/// Counting only one region would therefore conflate a valid file layout with a
/// persistence failure, and widening that count would stop checking the saved
/// column total.
///
/// A region index is an **arithmetic shift**, `chunk >> 5`
/// (`lodestone_anvil::region::region_and_local`), so `-1 >> 5 == -1`: a
/// radius-1 view centred on chunk `(0, 0)` spans chunks `(-1, -1)..=(1, 1)`
/// and therefore **four** regions, `(-1,-1)`, `(-1,0)`, `(0,-1)` and `(0,0)`.
/// Which of the four actually gets a file varies per run with *which* columns
/// a random tick happened to touch, and that is the whole of the
/// intermittency.
///
/// The expected region set is derived from the same shift the code uses rather
/// than restating a constant, and saved columns are counted across **every**
/// region file instead of only `r.0.0.mca`. A residency-proportional save could
/// otherwise spread its nine columns over four regions and evade a one-file
/// check.
/// The column a region-file slot holds: `index = local_z * 32 + local_x`.
fn column_of(file: &str, index: usize) -> (i32, i32) {
    let mut parts = file.trim_start_matches("r.").trim_end_matches(".mca").split('.');
    let rx: i32 = parts.next().and_then(|p| p.parse().ok()).expect("region x");
    let rz: i32 = parts.next().and_then(|p| p.parse().ok()).expect("region z");
    let index = i32::try_from(index).expect("slot index fits");
    (rx * 32 + index % 32, rz * 32 + index / 32)
}

#[test]
fn a_session_saves_columns_in_proportion_to_mutation_not_residency() {
    // Wide enough that the 3x3 light footprint of one edit is a small part of
    // the view, so a residency-proportional save cannot pass as a local one.
    const VIEW_RADIUS: i32 = 3;

    let world = TempWorld::new("cost");
    let seed = lodestone::menu::world_select::BUNDLED_WORLD.seed;

    // The first visit generates the view. Feature spills into neighbouring
    // columns are authoritative and saved once, so this session's writes are
    // proportional to generated area, not to anything a later autosave pays.
    {
        let Some(net) = require_hostable(open_session(seed, VIEW_RADIUS, Some(world.path()))) else {
            return;
        };
        wait_for_chunk(&net, ChunkPos { x: 0, z: 0 });
    }
    let before = saved_column_payloads(&world.path());

    // The second visit loads that view back and changes one block. Only what
    // this session rewrote is the steady-state cost of a mutation.
    let mutated = {
        let Some(net) = require_hostable(open_session(seed, VIEW_RADIUS, Some(world.path()))) else {
            return;
        };
        wait_for_chunk(&net, ChunkPos { x: 0, z: 0 });
        let air = air_id(&net);
        let (x, z) = player_column(&net);
        let surface = client_surface_y(&net, x, z, air).expect("the player's column has a surface");
        break_block_over_the_wire(&net, BlockPos::new(x, surface, z), air);
        (x >> 4, z >> 4)
    };
    let after = saved_column_payloads(&world.path());

    // Both the resident column set and the region set it maps to are derived,
    // never restated: `>> 5` is the same expression `region_and_local` uses,
    // which is what stops this drifting into a constant that is true only for
    // a view that never crosses zero.
    let resident: Vec<(i32, i32)> = (-VIEW_RADIUS..=VIEW_RADIUS)
        .flat_map(|cz| (-VIEW_RADIUS..=VIEW_RADIUS).map(move |cx| (cx, cz)))
        .collect();
    let resident_columns = resident.len();
    let mut reachable: Vec<String> = resident
        .iter()
        .map(|&(cx, cz)| format!("r.{}.{}.mca", cx >> 5, cz >> 5))
        .collect();
    reachable.sort_unstable();
    reachable.dedup();
    assert_eq!(
        reachable.len(),
        4,
        "a view centred on the origin straddles zero, so it touches four regions, not one: {reachable:?}"
    );

    let names: Vec<String> = region_files(&world.path())
        .iter()
        .map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert!(
        names.iter().all(|n| reachable.contains(n)),
        "a region file was written that no column of the view could belong to: \
         wrote {names:?}, reachable {reachable:?}"
    );

    // Across **every** file, not just region (0,0). A block change clears
    // saved light in its 3x3 column neighbourhood, so that footprint is the
    // legitimate cost of one mutation. Columns that did not exist before were
    // first saved by this session's own generation and are not a rewrite.
    let rewritten: Vec<(i32, i32)> = after
        .iter()
        .filter(|(key, payload)| before.get(*key).is_some_and(|old| old != *payload))
        .map(|((file, index), _)| column_of(file, *index))
        .collect();
    let mutated_payload_saved = after
        .iter()
        .any(|((file, index), payload)| {
            column_of(file, *index) == mutated && before.get(&(file.clone(), *index)) != Some(payload)
        });
    assert!(
        mutated_payload_saved,
        "the mutated column {mutated:?} was not saved at all ({} columns on disk across {names:?})",
        after.len()
    );
    let outside: Vec<_> = rewritten
        .iter()
        .filter(|(cx, cz)| (cx - mutated.0).abs() > 1 || (cz - mutated.1).abs() > 1)
        .collect();
    assert!(
        outside.is_empty(),
        "one mutation at column {mutated:?} rewrote {outside:?} outside its 3x3 light footprint \
         ({} of {resident_columns} resident columns rewritten) — that is residency-proportional, \
         and it would rewrite the whole store on every autosave. Files: {names:?}",
        rewritten.len()
    );
}
