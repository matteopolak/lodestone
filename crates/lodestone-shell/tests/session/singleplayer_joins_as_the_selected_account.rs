//! **Every join constructor asks `join_identity` who the player is** — nothing
//! in `net.rs` reads `offline.json` directly any more.
//!
//! ## What this guards, and why it is a source scan
//!
//! The defect: `NetClient::open_singleplayer` called `OfflineIdentity::load()`
//! while the account switcher wrote `profiles.json` and the skin cache followed
//! the Microsoft account, so singleplayer joined as the cracked player under the
//! signed-in player's skin. The fix routes all four constructors through
//! `crate::join_identity::join_identity`, and the regression is a one-token edit
//! back.
//!
//! A *behavioural* gate cannot see that edit, and it is worth saying why rather
//! than leaving the next reader to wonder. `join_identity()` deliberately forks
//! on `#[cfg(test)]` to the offline rung — a unit test must not join as whichever
//! account the developer happens to have selected — so in a test build the
//! pre-fix expression and the post-fix one *produce the same profile*. The two
//! hypotheses coincide on every input a hermetic test can supply. What
//! discriminates them is the call itself, so that is what this reads.
//!
//! It lives in `tests/` rather than beside the code on purpose: a source-grep
//! gate placed inside the file it greps matches its own assertion string and
//! passes with the real line deleted.
//!
//! `join_identity`'s own unit tests cover the decision this one only checks is
//! reachable: `a_selected_account_is_the_join_identity` (the selected account
//! wins), `nothing_selected_falls_back_to_the_offline_identity` (the negative
//! control) and `unit_tests_never_join_as_the_selected_account` (the fork).

const NET: &str = include_str!("../../src/net.rs");

/// Everything above `mod tests` — the production half. The test module below it
/// legitimately names `OfflineIdentity::load()` as an *expected value* read from
/// outside the code under test, so scanning the whole file would report it.
fn production_half() -> &'static str {
    let (production, tests) = NET
        .split_once("\nmod tests {")
        .expect("net.rs must still carry its `mod tests` block");
    assert!(
        tests.contains("fn two_offline_sessions_publish_the_same_identity"),
        "the split landed somewhere other than the test module; the scan below \
         would be measuring the wrong half"
    );
    production
}

/// The control for the split itself: the pattern this test hunts for must be
/// *present* somewhere, or a scan that found nothing would read as a pass.
#[test]
fn the_scan_can_see_the_production_join_constructors() {
    let production = production_half();
    for constructor in [
        "pub fn connect(",
        "pub fn connect_as(",
        "pub fn connect_online(",
        "pub fn open_singleplayer(",
        "pub fn open_to_lan(",
    ] {
        assert!(
            production.contains(constructor),
            "`{constructor}` is gone from net.rs's production half; this gate is \
             measuring nothing until it is renamed here too"
        );
    }
}

/// The defect, pinned: no join constructor may resolve the offline identity by
/// itself, and the four that must route through `join_identity` still do.
///
/// `connect_as` is the one exception and it is exempt by *expression*, not by
/// name — it builds `OfflineIdentity::from_username_unchecked(..)` from the
/// username a live gate passed in, which is a different call and is what keeps
/// every gate off the developer's premium player file. Both counts (rather
/// than a `contains`) live in one test: moving one constructor back to the
/// offline identity while another still calls the ladder correctly cannot
/// pass either check.
#[test]
fn no_join_constructor_bypasses_the_join_identity() {
    let production = production_half();
    assert_eq!(
        production.matches("OfflineIdentity::load()").count(),
        0,
        "a join constructor is reading `offline.json` again instead of \
         `join_identity::join_identity()`; that is the bug where selecting a \
         Microsoft account changed the skin and not the player"
    );
    assert_eq!(
        production.matches("join_identity::join_identity()").count(),
        4,
        "`connect`, `connect_online`, `open_singleplayer` and `open_to_lan` must \
         each resolve the join identity exactly once — `connect_as` is the only \
         constructor that names its own username"
    );
}
