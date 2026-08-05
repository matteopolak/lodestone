//! A persistent world writes a real `level.dat`, and its age accumulates
//! across sessions rather than restarting — issue
//! [#468](https://github.com/matteopolak/lodestone/issues/468)'s gap list.
//!
//! # What this gate evidences, and where the expected values come from
//!
//! | claim | evidenced by |
//! |---|---|
//! | our `level.dat` schema matches a real 26.2 server's | **externally**, against the checked-in vanilla file — this gate compares key sets against *Mojang's own*, never a hand-written list |
//! | a world directory gets a `level.dat` at all | here, at the production constructor |
//! | `Time` accumulates across close and reopen | here, and the cross-session half is an **exact** equality, not a direction |
//!
//! The accumulation claim is the one worth being careful about, because "time
//! went up" is the *magnitude* species of vacuous test — satisfied by a clock
//! that resets to zero and ticks once. So the assertion that carries the
//! weight is `base_ticks() == the Time the previous session wrote`, an exact
//! equality between two sessions, and the within-session check brackets the
//! tick counter read either side of the save rather than asserting a
//! direction. The bracket has its own precondition check: if no tick ever ran,
//! the bracket would be `0 <= Time <= 0` and would pass while measuring
//! nothing.
//!
//! # The control, run and observed
//!
//! `LevelDatHandle::write` made a no-op (an early `Ok(())` before stamping),
//! applied in a throwaway worktree at `c31ccd8` and **observed failing**, not
//! described. The exact output:
//!
//! ```text
//! a_worlds_age_accumulates_across_sessions ... FAILED
//!   Time 0 must be the tick count at the moment of the save, which was between 7 and 7
//! a_new_world_gets_a_level_dat_matching_the_real_schema ... ok
//! ```
//!
//! The second line is the informative one, and is why the control was worth
//! running rather than assuming: the two gates produce **different** failure
//! sets, because creating the file genuinely still works when only the stamp
//! is broken. A red run here therefore says which half broke. Note the failure
//! lands on the within-session bracket before the cross-session equality is
//! ever reached — so the bracket is load-bearing, not decoration.

use std::path::Path;
use std::time::Duration;

use lodestone_core::{Nbt, State};
use lodestone_server::{ChunkColumn, ChunkSource, ServerBound, ServerDirective, ServerProtocol};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// Long enough that the 50 ms tick loop has certainly run several times, and
/// short enough not to matter. Nothing is asserted about *how many* ticks
/// happen in it — only that at least one did, which is checked rather than
/// assumed.
const LET_IT_TICK: Duration = Duration::from_millis(400);

#[derive(Debug)]
struct TestProtocol;

impl ServerProtocol for TestProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

/// Flat, cheap terrain. This gate is about a 400-byte metadata file, so the
/// generator is deliberately the least interesting thing in it.
#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 60, z, "minecraft:stone");
            }
        }
        column
    }
}

const SPAWN_CENTER: (i32, i32) = (24, -8);

async fn open(dir: &Path) -> lodestone_server::IntegratedServer {
    let (server, _client, _world) = lodestone_server::IntegratedServer::open_persistent_with_mobs(
        TestProtocol,
        dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        SPAWN_CENTER,
        0,
        1,
        // Far longer than this test runs: every write below is an explicit
        // one, so nothing here depends on the autosave timer firing.
        Duration::from_secs(3600),
    )
    .expect("open persistent world");
    server
}

fn level_dat_at(dir: &Path) -> lodestone_anvil::level_dat::LevelDat {
    let path = lodestone_anvil::level_dat::path_in(dir);
    lodestone_anvil::level_dat::read_from_file(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn field_names(level: &lodestone_anvil::level_dat::LevelDat) -> Vec<String> {
    let Some(Nbt::Compound(fields)) = level.data().cloned() else {
        panic!("level.dat has no Data compound");
    };
    let mut names: Vec<String> = fields.into_iter().map(|(name, _)| name).collect();
    names.sort();
    names
}

/// A world directory with region files but no `level.dat` is not a world any
/// other tool will open, so this asserts the file exists *and* that it carries
/// the same field set a real 26.2 server writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_world_gets_a_level_dat_matching_the_real_schema() {
    let dir = tempdir("new_world");
    let server = open(&dir).await;

    let handle = server.level_dat().expect("a persistent server has one");
    assert!(handle.created(), "a fresh directory must create level.dat");
    assert_eq!(
        handle.writes(),
        1,
        "creating the world writes the file exactly once"
    );
    assert!(
        lodestone_anvil::level_dat::path_in(&dir).exists(),
        "level.dat must exist on disk before anything else is saved"
    );

    let ours = level_dat_at(&dir);
    // The expected key set is a real Mojang file's, read off disk — not a list
    // written here, which would only restate whatever we happen to emit.
    let vanilla_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-anvil/tests/support/level_dat_26_2_vanilla.dat");
    let vanilla_bytes = std::fs::read(&vanilla_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", vanilla_path.display()));
    let vanilla = lodestone_anvil::level_dat::read(&vanilla_bytes).expect("decodes the real file");
    assert_eq!(
        field_names(&ours),
        field_names(&vanilla),
        "our level.dat's field set must match a real 26.2 server's"
    );

    assert_eq!(
        ours.level_name(),
        Some("lodestone-468-4m8k-new_world"),
        "the world name comes from the directory"
    );
    assert_eq!(ours.data_version().expect("has DataVersion"), 4903);
    assert_eq!(ours.time(), Some(0), "a brand-new world has run no ticks");
    let spawn = ours.spawn().expect("has a spawn compound");
    assert_eq!(
        spawn.pos,
        [SPAWN_CENTER.0, 64, SPAWN_CENTER.1],
        "spawn follows the world's centre"
    );
    assert_eq!(spawn.dimension, "minecraft:overworld");

    server.shutdown().await;
}

/// **The gate.** A world's age must carry across a close and reopen.
///
/// A blocks-only persistence gate cannot see this: every block it checks is
/// one that was saved, while a world whose `Time` resets to zero every session
/// looks perfect right up until anything depends on the world's age.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worlds_age_accumulates_across_sessions() {
    let dir = tempdir("age");

    // --- session one --------------------------------------------------------
    let server = open(&dir).await;
    tokio::time::sleep(LET_IT_TICK).await;

    let before = server.tick_stats().expect("a ticking server").tick_count;
    server.save_now().expect("save");
    let after = server.tick_stats().expect("a ticking server").tick_count;
    // The precondition the bracket below would otherwise be vacuous without:
    // with zero ticks it reads `0 <= Time <= 0` and measures nothing.
    assert!(
        after > 0,
        "no tick ran in {LET_IT_TICK:?}, so this gate would assert nothing"
    );

    let stamped = level_dat_at(&dir).time().expect("has Time");
    assert!(
        (i64::try_from(before).expect("fits")..=i64::try_from(after).expect("fits"))
            .contains(&stamped),
        "Time {stamped} must be the tick count at the moment of the save, which was between \
         {before} and {after}"
    );

    server.shutdown().await;
    let session_one_final = level_dat_at(&dir).time().expect("has Time");
    assert!(
        session_one_final >= stamped,
        "the shutdown stamp ({session_one_final}) cannot go backwards from the save's \
         ({stamped})"
    );

    // --- session two --------------------------------------------------------
    let server = open(&dir).await;
    let handle = server.level_dat().expect("persistent");
    assert!(
        !handle.created(),
        "reopening an existing world must not recreate its level.dat"
    );
    // **The exact claim.** Not "greater than", not "nonzero" — session two
    // starts from precisely the number session one left behind.
    assert_eq!(
        handle.base_ticks(),
        session_one_final,
        "session two's base must equal session one's final Time"
    );

    tokio::time::sleep(LET_IT_TICK).await;
    server.save_now().expect("save");
    let session_two = level_dat_at(&dir).time().expect("has Time");
    assert!(
        session_two > session_one_final,
        "session two's Time ({session_two}) must exceed session one's final \
         ({session_one_final}); a world that restarts its clock would read equal"
    );

    // And the world's identity is untouched by the reopen: same name, same
    // spawn, same data version. A "reopen" that rewrote those would be
    // creating a new world over the top of the old one.
    let reopened = level_dat_at(&dir);
    assert_eq!(reopened.level_name(), Some("lodestone-468-4m8k-age"));
    assert_eq!(
        reopened.spawn().expect("has spawn").pos,
        [SPAWN_CENTER.0, 64, SPAWN_CENTER.1]
    );

    server.shutdown().await;
}

/// A unique scratch directory, named the way `world_persistence_round_trip.rs`
/// names its own: a literal nonce rather than a pid or a random, because the
/// temp directory is shared between agents and a collision would read as a
/// persistence bug.
fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-468-4m8k-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}
