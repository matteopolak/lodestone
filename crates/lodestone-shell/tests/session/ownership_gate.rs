//! **Nothing is playable until a stored account owns the game** — driven
//! through the real menu, not through the gate's own predicate.
//!
//! ## What this guards
//!
//! The requirement is compliance-shaped: a client that lets someone play without
//! an account that owns Minecraft is a client that should not ship. A gate that
//! silently fails open is therefore worse than no gate, and the way this one
//! could fail open is the shape this repo pays for over and over — a second
//! entry path that nobody remembered to route through the check.
//!
//! So the enforcement is a **type**, not a check.
//! `lodestone_auth::Entitlement` has private fields and one constructor, which
//! reads an account roster and answers `None` for an empty one; and both play
//! verbs — `MenuAction::Singleplayer` and `MenuAction::Connect` — carry one. A
//! future entry path that forgets the gate does not compile.
//!
//! ## Why these tests drive `MenuNav` rather than calling the predicate
//!
//! Asserting `MenuNav::ownership_gate_blocks(..) == true` would be a closed
//! loop: it proves the predicate agrees with itself and says nothing about
//! whether any keystroke or click is actually routed through it. Every test
//! below therefore presses keys and clicks rows the way `app.rs`'s input handler
//! does, and asserts on the `MenuAction` that comes back and on the frame
//! `render::frame_for` builds.
//!
//! ## The positive control is load-bearing
//!
//! [`the_same_walk_reaches_singleplayer_once_an_account_owns_the_game`] runs the
//! *identical* walk against a roster with one account and requires it to reach
//! `MenuAction::Singleplayer`. Without it, this file would keep passing if the
//! walk stopped being able to reach singleplayer for some unrelated reason — an
//! absence assertion with no evidence its detector works.
//!
//! Its sibling in `menu::accounts`'s own unit tests carries the half this file
//! structurally cannot reach: the sign-in state machine, and therefore the
//! account that authenticates but does **not** own the game.

use lodestone::menu::nav::{MainButton, MenuAction, MenuKey, MenuNav, OwnershipButton};
use lodestone::menu::render::{FaviconCache, frame_for};
use lodestone::menu::status::StatusCache;
use lodestone::menu::{Screen, UiState};

/// A fresh temp directory, with the server list, options, roster and `saves/`
/// all inside it — so nothing here can reach the developer's real files.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lodestone-ownership-gate-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp dir for the gate fixture");
    dir
}

/// Writes a roster holding one account into `dir`, i.e. the on-disk state a
/// completed sign-in leaves behind.
///
/// This is the *only* thing that differs between the gated and ungated arms
/// below, which is what makes the pair a controlled comparison rather than two
/// unrelated walks.
fn write_owning_account(dir: &std::path::Path) {
    let mut meta = lodestone_auth::AccountsMetadata::default();
    let id = uuid::Uuid::new_v4();
    meta.upsert(lodestone_auth::AccountProfile {
        profile_id: id,
        username: "OwnerAccount".to_owned(),
        skin_url: None,
        last_used: 1,
    });
    meta.selected = Some(id);
    meta.save_to(&dir.join("profiles.json"))
        .expect("the temp roster must be writable");
}

/// Puts a real world directory under this nav's own `saves/`, so **Play
/// Selected World** is live at all — with an empty `saves/` that button is
/// greyed and the ungated arm could not reach singleplayer for a reason that
/// has nothing to do with the gate.
fn plant_world(nav: &MenuNav, dir_name: &str) {
    let dir = nav.saves_root().join(dir_name);
    std::fs::create_dir_all(&dir).expect("create world dir");
    let level = lodestone_anvil::level_dat::LevelDat::for_new_world(
        dir_name,
        &lodestone_anvil::level_dat::Spawn::default(),
        0,
    );
    lodestone_anvil::level_dat::write_to_file(
        &level,
        &lodestone_anvil::level_dat::path_in(&dir),
    )
    .expect("write level.dat");
}

fn nav_in(dir: &std::path::Path) -> MenuNav {
    MenuNav::with_path(dir.join("servers.json"))
}

/// The walk a player takes to start a singleplayer world: Enter on the title's
/// Singleplayer row, then Enter on the world list's **Play Selected World**.
///
/// Returns every action the walk produced, so a caller can assert on the whole
/// sequence rather than on one step — a gate that let the *second* press through
/// would pass a check that only read the first.
fn walk_to_singleplayer(nav: &mut MenuNav, ui: &mut UiState) -> Vec<MenuAction> {
    use lodestone::menu::world_select::WorldSelectButton;
    let mut actions = vec![nav.key(ui, MenuKey::Enter)];
    // The world list's Play button by its own row index, the same route
    // `app.rs`'s mouse handler takes.
    actions.push(nav.click(ui, WorldSelectButton::Play.row()));
    actions
}

fn is_play_verb(action: &MenuAction) -> bool {
    matches!(
        action,
        MenuAction::Singleplayer(..) | MenuAction::Connect(..)
    )
}

/// **The gate, from a cold start with no accounts.** Singleplayer is not merely
/// greyed — the walk that starts a world cannot produce a play verb at all, and
/// the screen the player is looking at is the gate.
#[test]
fn an_empty_account_store_makes_singleplayer_unreachable() {
    let dir = temp_dir("empty-store");
    let mut nav = nav_in(&dir);
    plant_world(&nav, "planted");
    let mut ui = UiState::new();

    // Premise: the cursor really is on Singleplayer, so the Enter below is the
    // press this test claims it is.
    assert_eq!(
        nav.main_button(),
        MainButton::Singleplayer,
        "premise: the title screen starts on Singleplayer"
    );

    // The press that would open the world list. It reaches the gate instead,
    // and is **swallowed** rather than also pressing a gate button: the player
    // sees the gate before acting on it.
    let first = nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(
        first,
        MenuAction::None,
        "Enter on Singleplayer must not launch anything, and must not activate a \
         gate button the player has not seen yet"
    );
    assert_eq!(
        ui.screen(),
        Screen::Ownership,
        "the press must land the player on the ownership gate"
    );
    assert_eq!(
        nav.ownership_button(),
        OwnershipButton::AddAccount,
        "the gate opens with its own cursor on Add Account"
    );

    // And there is no continuation of the walk that gets past it. Enter is
    // pressed repeatedly rather than once, because a gate that only intercepted
    // the *first* press would satisfy the assertions above.
    let mut produced = vec![first];
    for _ in 0..8 {
        produced.push(nav.key(&mut ui, MenuKey::Enter));
        produced.push(nav.key(&mut ui, MenuKey::Down));
    }
    assert!(
        !produced.iter().any(is_play_verb),
        "no press on the singleplayer path may produce a play verb with an \
         empty account store; got {produced:?}"
    );
    assert!(
        !ui.is_playing() && ui.kind().is_none(),
        "no session may have begun; screen is {:?}",
        ui.screen()
    );
}

/// The multiplayer half of the same claim: a saved server cannot be joined
/// either, by key or by click.
#[test]
fn an_empty_account_store_makes_multiplayer_unreachable() {
    let dir = temp_dir("empty-store-mp");
    // A saved server, written before the nav loads it, so the list is non-empty
    // and the Join path is otherwise live.
    std::fs::write(
        dir.join("servers.json"),
        r#"[{"name":"Test","address":"h0.example"}]"#,
    )
    .expect("seed a server list");
    let mut nav = nav_in(&dir);
    let mut ui = UiState::new();

    // Every key and every plausible row, from the title screen. None of them may
    // produce a play verb, and none may start a session.
    let mut produced: Vec<MenuAction> = Vec::new();
    for _ in 0..8 {
        for key in [MenuKey::Down, MenuKey::Up, MenuKey::Tab, MenuKey::Enter] {
            produced.push(nav.key(&mut ui, key));
        }
        for row in 0..12 {
            nav.hover(&ui, row);
            produced.push(nav.click(&mut ui, row));
        }
    }
    assert!(
        !produced.iter().any(is_play_verb),
        "a brute walk of the menu produced a play verb with no account: {:?}",
        produced.iter().filter(|a| is_play_verb(a)).collect::<Vec<_>>()
    );
    assert!(
        !ui.is_playing() && ui.kind().is_none(),
        "no session may have begun; screen is {:?}",
        ui.screen()
    );
}

/// **Something on screen changes.** The gate is not just an input refusal: the
/// frame the renderer builds is the gate's own, with its two buttons, from the
/// very first frame — before any input has arrived to move `UiState`.
#[test]
fn the_first_frame_drawn_with_no_account_is_the_gate_not_the_title_screen() {
    let dir = temp_dir("first-frame");
    let nav = nav_in(&dir);
    let ui = UiState::new();
    assert_eq!(
        ui.screen(),
        Screen::MainMenu,
        "premise: `UiState` has not been reconciled — this is the untouched \
         startup state, which is exactly the frame this test is about"
    );

    let statuses = StatusCache::new();
    let mut favicons = FaviconCache::new();
    let frame = frame_for(&ui, &nav, &statuses, &mut favicons)
        .expect("the gate is a full-frame menu screen");
    let labels: Vec<&str> = frame.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            OwnershipButton::AddAccount.label(),
            OwnershipButton::Quit.label()
        ],
        "the untouched startup frame must be the gate's rows, not the title \
         screen's"
    );
    assert!(
        frame
            .notice
            .as_ref()
            .is_some_and(|n| n.text.contains("owns Minecraft")),
        "the gate must say what it is asking for; a screen with two buttons and \
         no explanation reads as a broken build"
    );
}

/// **The positive control.** The same walk, the same fixture, one account added
/// — and it reaches singleplayer.
///
/// Without this, every assertion above could be passing because the walk cannot
/// reach a play verb for some reason unrelated to the gate.
#[test]
fn the_same_walk_reaches_singleplayer_once_an_account_owns_the_game() {
    let dir = temp_dir("owned-store");
    write_owning_account(&dir);
    let mut nav = nav_in(&dir);
    plant_world(&nav, "planted");
    let mut ui = UiState::new();

    assert_eq!(nav.main_button(), MainButton::Singleplayer);
    let actions = walk_to_singleplayer(&mut nav, &mut ui);
    let launched = actions.iter().any(is_play_verb);
    assert!(
        launched,
        "with an account that owns the game the identical walk must reach a \
         play verb; got {actions:?}"
    );
}

/// **Revocation while running.** Removing the last account closes the gate again
/// on the next input, rather than leaving a title screen that looks usable.
///
/// The account screen is the only screen the gate exempts, so this is also the
/// one transition where the player can be *on* an exempt screen and become
/// unentitled — the exact edge a screen-based gate would get wrong.
#[test]
fn removing_the_last_account_returns_the_player_to_the_gate() {
    let dir = temp_dir("revoke");
    write_owning_account(&dir);
    let mut nav = nav_in(&dir);
    let mut ui = UiState::new();

    // Premise: the gate is open to start with.
    assert!(
        nav.entitlement().is_some(),
        "premise: the seeded roster entitles"
    );

    // Walk to the account screen the way a player does, then delete the
    // highlighted account and leave.
    while nav.main_button() != MainButton::Accounts {
        nav.key(&mut ui, MenuKey::Down);
    }
    nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(
        ui.screen(),
        Screen::Accounts,
        "premise: the Accounts row opens the account screen"
    );
    nav.key(&mut ui, MenuKey::Delete);
    assert!(
        nav.entitlement().is_none(),
        "premise: deleting the only account empties the roster"
    );

    nav.key(&mut ui, MenuKey::Escape);
    assert_eq!(
        ui.screen(),
        Screen::Ownership,
        "leaving the account screen with nothing left must land on the gate, \
         not on a title screen the player cannot use"
    );
}
