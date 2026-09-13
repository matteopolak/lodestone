//! Hermetic Friends entry-point coverage.
//!
//! The service worker is intentionally not started here: these tests pin the
//! shell's title/pause/settings lifecycle, which is the part that must remain
//! correct even when a credential is unavailable. HTTP behavior belongs to the
//! loopback service tests in `lodestone-auth`.

use lodestone::menu::nav::{friends_overlay_frame, MenuKey, MenuNav};
use lodestone::menu::render::MenuBackdrop;
use lodestone::menu::{Screen, UiState};

fn nav(label: &str) -> MenuNav {
    let root = std::env::temp_dir().join(format!(
        "lodestone-friends-overlay-{label}-{}",
        std::process::id()
    ));
    let profile_id = uuid::Uuid::new_v4();
    let mut metadata = lodestone_auth::AccountsMetadata::default();
    metadata.upsert(lodestone_auth::AccountProfile {
        profile_id,
        username: "FriendsLifecycleAccount".to_owned(),
        skin_url: None,
        last_used: 1,
    });
    metadata.selected = Some(profile_id);
    metadata
        .save_to(&root.join("profiles.json"))
        .expect("the lifecycle roster must be writable");
    MenuNav::with_paths(
        root.join("servers.json"),
        root.join("options.json"),
        root.join("profiles.json"),
    )
}

#[test]
fn title_friends_route_is_a_full_menu_and_returns_to_title() {
    let mut ui = UiState::new();
    let nav = nav("title");

    ui.open_friends_from_title();
    assert_eq!(ui.screen(), Screen::Friends);
    assert!(ui.is_menu(), "the title route owns the full menu frame");
    assert!(!ui.friends_in_world());
    assert!(
        friends_overlay_frame(&ui, &nav).is_none(),
        "only the pause route is an in-world overlay"
    );

    ui.close_friends();
    assert_eq!(ui.screen(), Screen::MainMenu);
}

#[test]
fn pause_friends_route_is_dimmed_overlay_and_returns_to_pause() {
    let mut ui = UiState::new();
    let nav = nav("pause");
    ui.enter_dev_world();
    ui.pause();
    ui.open_friends_from_pause();

    assert_eq!(ui.screen(), Screen::Friends);
    assert!(ui.friends_in_world());
    assert!(!ui.is_menu(), "the world remains the rendered background");
    let frame = friends_overlay_frame(&ui, &nav).expect("pause Friends has an overlay frame");
    assert_eq!(frame.backdrop, MenuBackdrop::Dim);
    assert!(frame.blur);

    ui.close_friends();
    assert_eq!(ui.screen(), Screen::Paused);
}

#[test]
fn online_settings_route_opens_friends_settings_and_preserves_title_return() {
    let mut ui = UiState::new();
    let mut nav = nav("settings");
    ui.open_settings();

    // Reach the Online button through the same cursor path a player has. The
    // independent control census supplies the target index; no private nav
    // field is used as a shortcut.
    let root = lodestone::menu::options::all_controls(
        lodestone::menu::options::SettingsPage::Root,
        false,
    );
    let online = root
        .iter()
        .position(|cell| {
            matches!(
                cell,
                lodestone::menu::options::Cell::Nav {
                    page: Some(lodestone::menu::options::SettingsPage::Online),
                    ..
                }
            )
        })
        .expect("the root settings page exposes Online");
    for _ in 0..=root.len() {
        if nav.settings().cursor() == online {
            break;
        }
        nav.key(&mut ui, MenuKey::Down);
    }
    assert_eq!(nav.settings().cursor(), online);
    nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(nav.settings().page(), lodestone::menu::options::SettingsPage::Online);

    let online_controls = lodestone::menu::options::all_controls(
        lodestone::menu::options::SettingsPage::Online,
        false,
    );
    let friends_settings = online_controls
        .iter()
        .position(|cell| {
            matches!(
                cell,
                lodestone::menu::options::Cell::Act {
                    act: lodestone::menu::options::Action::OpenFriendsSettings,
                    ..
                }
            )
        })
        .expect("Online exposes account-scoped Friends settings");
    for _ in 0..=online_controls.len() {
        if nav.settings().cursor() == friends_settings {
            break;
        }
        nav.key(&mut ui, MenuKey::Down);
    }
    assert_eq!(nav.settings().cursor(), friends_settings);
    nav.key(&mut ui, MenuKey::Enter);

    assert_eq!(ui.screen(), Screen::Friends);
    assert!(!ui.friends_in_world());
    assert_eq!(nav.friends().tab(), lodestone::menu::friends::FriendsTab::Settings);
    ui.close_friends();
    assert_eq!(ui.screen(), Screen::MainMenu);
}
