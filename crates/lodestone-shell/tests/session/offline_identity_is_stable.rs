//! **The gate the offline-identity fix exists for: the same account every
//! launch.**
//!
//! # What it is
//!
//! Two *independent* constructions of the offline join identity must produce the
//! same username and the same UUID. That is the property the owner's report was
//! about — *"I keep spawning in the air even if I rejoin"* — and the property the
//! pre-fix expression could not have: `lodestone_testsupport::unique_username()`
//! cannot return the same name twice by construction, and `Uuid::new_v4()` is
//! random.
//!
//! # How it works
//!
//! One predicate, [`stability_verdict`], applied in both directions:
//!
//! * to [`OfflineIdentity`], which must be **stable** — and whose expected values
//!   come from the persisted file the test wrote and from UUIDs computed outside
//!   this workspace (CPython's `hashlib.md5` over the documented
//!   `nameUUIDFromBytes` rule), never from a second call to the code under test;
//! * to the pre-fix expression, which must be **unstable** — the control, without
//!   which "the two matched" is equally consistent with a predicate that matches
//!   anything.
//!
//! Both *worlds* are exercised, because the fallback is where the generated name
//! used to live: a directory with a stored name, and a directory with no file at
//! all. Each arm asserts which one it is in — `path.exists()` either way — rather
//! than skipping, so neither can pass vacuously on a machine where the fixture
//! failed to appear.
//!
//! Nothing here reads or writes the developer's real data directory: every call
//! uses the `_from`/`_to` twins with a temp path. The end-to-end half — that
//! `NetClient::connect` actually *consumes* this, rather than the module being a
//! well-tested island — is `net::tests::two_offline_sessions_publish_the_same_identity`,
//! which observes the published `local_uuid` from the real production
//! constructor.
//!
//! # How to change it
//!
//! If the persisted shape grows a field, the fixtures below stay valid (unknown
//! keys are ignored and absent ones default), but add an arm for it. **Do not
//! replace the hand-written UUIDs with `offline_uuid(..)` calls** — that turns
//! the strongest assertion in the file into a tautology, and it is the only thing
//! standing between the vanilla derivation and the plausible-but-wrong
//! `Uuid::new_v3` namespaced reading.
//!
//! # Dependencies
//!
//! `lodestone::offline_identity`, and `lodestone_testsupport::unique_username`
//! for the control (a dev dependency — production cannot reach it, which is the
//! other half of this change).

use std::path::{Path, PathBuf};

use lodestone::offline_identity::{DEFAULT_USERNAME, OfflineIdentity};
use uuid::Uuid;

/// The UUID an offline-mode server derives for [`DEFAULT_USERNAME`], computed
/// outside this workspace — see `offline_identity`'s unit tests for the exact
/// CPython transcript. Also the value every offline server on the internet
/// assigns to the name "Player".
const DEFAULT_UUID: &str = "a01e3843-e521-3998-958a-f459800e4d11";

/// Likewise for the name the fixtures store.
const STEVE_UUID: &str = "5627dd98-e6be-3c21-b8a8-e92344183641";

/// The one predicate, so the subject and the control are measured by the same
/// instrument.
///
/// Returns the reason on disagreement rather than asserting, so the control can
/// require a disagreement *and print what it was*.
fn stability_verdict(a: &(String, Uuid), b: &(String, Uuid)) -> Result<(), String> {
    if a.0 != b.0 {
        return Err(format!(
            "the two constructions produced different usernames: {:?} then {:?} \
             — a new offline account every launch",
            a.0, b.0
        ));
    }
    if a.1 != b.1 {
        return Err(format!(
            "the two constructions produced different uuids for name {:?}: {} then {} \
             — a new offline account every launch",
            a.0, a.1, b.1
        ));
    }
    Ok(())
}

fn identity_pair(path: &Path) -> (String, Uuid) {
    let id = OfflineIdentity::load_from(path);
    (id.username().to_owned(), id.uuid())
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lodestone-offline-stable-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// **World 1: a stored name.** Two independent loads agree, and agree with the
/// value the fixture put on disk — not with each other only.
#[test]
fn a_persisted_offline_name_is_the_same_identity_on_every_construction() {
    let dir = temp_dir("stored");
    let path = dir.join("offline.json");
    let mut written = OfflineIdentity::default();
    written.set_username("Steve").expect("valid name");
    written.save_to(&path).expect("write the fixture");

    // Assert which world this is, loudly, rather than skipping if the fixture
    // did not appear.
    assert!(
        path.exists(),
        "the stored-name arm requires the fixture at {}; without it this test \
         would silently measure the *default* arm instead",
        path.display()
    );

    let first = identity_pair(&path);
    let second = identity_pair(&path);
    stability_verdict(&first, &second).expect("the persisted offline identity must be stable");

    // Expected values from outside: the literal the fixture stored, and a UUID
    // computed by a second implementation of the published derivation.
    assert_eq!(first.0, "Steve", "the stored name must be the one that joins");
    assert_eq!(
        first.1,
        Uuid::parse_str(STEVE_UUID).expect("vector parses"),
        "the uuid must be the one a vanilla offline server derives for that name"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **World 2: no file at all** — a fresh install, and the arm the generated name
/// used to live in. It must be just as stable, and equal to the placeholder.
#[test]
fn a_missing_file_still_yields_one_fixed_identity_rather_than_a_fresh_one() {
    let dir = temp_dir("absent");
    let path = dir.join("offline.json");
    assert!(
        !path.exists(),
        "the no-file arm requires {} to be absent; a leftover fixture would make \
         this measure the stored-name arm instead",
        path.display()
    );

    let first = identity_pair(&path);
    let second = identity_pair(&path);
    stability_verdict(&first, &second)
        .expect("the default offline identity must be stable too — this is the fallback");

    assert_eq!(first.0, DEFAULT_USERNAME);
    assert_eq!(
        first.1,
        Uuid::parse_str(DEFAULT_UUID).expect("vector parses"),
        "the default's uuid must be the vanilla derivation of {DEFAULT_USERNAME:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The control.** The same predicate, applied to the expression `net.rs` used
/// before this change, must report a disagreement — and the reason is printed so
/// the failure it *would* have produced is on the record rather than described.
///
/// ```text
/// username: unique_username(),
/// uuid: uuid::Uuid::new_v4(),
/// ```
///
/// Both halves are checked separately, because either one alone is enough to give
/// the player a new account: a stable name with a random UUID still changes
/// identity against our own integrated server, which echoes the UUID the client
/// presents instead of deriving one.
#[test]
fn the_pre_fix_expression_fails_the_same_stability_predicate() {
    let pre_fix = || {
        (
            lodestone_testsupport::unique_username(),
            Uuid::new_v4(),
        )
    };
    let verdict = stability_verdict(&pre_fix(), &pre_fix());
    // Observed failing before the fix landed, with this exact text (the probe was
    // `verdict.expect(..)` on the line below, and the run is quoted in
    // `DESIGN.md` §12.121):
    //
    //     the two constructions produced different usernames:
    //     "E0_172dq2y" then "E1_172dq2y" — a new offline account every launch
    //
    // `E0_`/`E1_` is the atomic counter in the first field, which is why the two
    // differ within one process and not merely across runs.
    let reason = verdict.expect_err(
        "the pre-fix expression must fail this predicate, or the gates above \
         prove nothing about stability",
    );
    println!("control: pre-fix expression rejected — {reason}");

    // The name half alone, with the uuid held fixed, so the control cannot pass
    // on the uuid difference while the name check is broken.
    let fixed = Uuid::new_v4();
    let name_only = stability_verdict(
        &(lodestone_testsupport::unique_username(), fixed),
        &(lodestone_testsupport::unique_username(), fixed),
    );
    let reason = name_only.expect_err("`unique_username` must be detected as unstable");
    assert!(
        reason.contains("usernames"),
        "the name half must be what fired: {reason}"
    );
    println!("control: name half alone rejected — {reason}");

    // And the uuid half alone, with the name held fixed — the case a
    // name-only fix would have shipped, and the one singleplayer actually hit.
    let name = DEFAULT_USERNAME.to_owned();
    let uuid_only = stability_verdict(
        &(name.clone(), Uuid::new_v4()),
        &(name, Uuid::new_v4()),
    );
    let reason = uuid_only.expect_err("`Uuid::new_v4` must be detected as unstable");
    assert!(
        reason.contains("uuids"),
        "the uuid half must be what fired: {reason}"
    );
    println!("control: uuid half alone rejected — {reason}");
}

/// The predicate must not be a rubber stamp in the *other* direction either: two
/// genuinely equal pairs pass, so `Ok` is reachable and the control's `Err` is a
/// real discrimination rather than "this function always fails".
#[test]
fn the_predicate_accepts_a_genuinely_equal_pair() {
    let one = ("Steve".to_owned(), Uuid::parse_str(STEVE_UUID).unwrap());
    assert_eq!(stability_verdict(&one, &one.clone()), Ok(()));
}
