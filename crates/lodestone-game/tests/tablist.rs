//! Tab list / player info tests.

use lodestone_game::tablist::{GameProfile, PlayerListEntry, ProfileProperty, TabList};
use lodestone_model::{GameMode, Text};
use uuid::Uuid;

fn profile(name: &str) -> GameProfile {
    GameProfile::new(Uuid::new_v4(), name)
}

#[test]
fn skin_texture_reads_textures_property() {
    let mut p = profile("steve");
    p.properties.push(ProfileProperty {
        name: "textures".into(),
        value: "BASE64BLOB".into(),
        signature: Some("sig".into()),
    });
    assert_eq!(p.skin_texture(), Some("BASE64BLOB"));
    assert_eq!(profile("nobody").skin_texture(), None);
}

#[test]
fn partial_update_mutates_only_targeted_fields() {
    let mut tl = TabList::new();
    let p = profile("alice");
    let id = p.id;
    tl.insert(PlayerListEntry::new(p));

    // UPDATE_LATENCY-style partial edit.
    tl.get_mut(&id).unwrap().latency = 42;
    // UPDATE_GAME_MODE-style partial edit.
    tl.get_mut(&id).unwrap().game_mode = GameMode::Creative;

    let e = tl.get(&id).unwrap();
    assert_eq!(e.latency, 42);
    assert_eq!(e.game_mode, GameMode::Creative);
    assert!(e.listed);
}

#[test]
fn unlisted_players_are_hidden_from_render_order() {
    let mut tl = TabList::new();
    let mut a = PlayerListEntry::new(profile("alice"));
    a.listed = false;
    let b = PlayerListEntry::new(profile("bob"));
    tl.insert(a);
    tl.insert(b);
    let names: Vec<&str> = tl
        .ordered()
        .iter()
        .map(|e| e.profile.name.as_str())
        .collect();
    assert_eq!(names, ["bob"]);
    assert_eq!(tl.len(), 2, "unlisted still tracked");
}

#[test]
fn ordering_is_list_order_then_spectator_then_name() {
    let mut tl = TabList::new();

    let mut high = PlayerListEntry::new(profile("zzz"));
    high.list_order = 10;

    let normal_a = PlayerListEntry::new(profile("adam"));
    let normal_b = PlayerListEntry::new(profile("bob"));

    let mut spec = PlayerListEntry::new(profile("aaa"));
    spec.game_mode = GameMode::Spectator;

    tl.insert(normal_b);
    tl.insert(spec);
    tl.insert(high);
    tl.insert(normal_a);

    let names: Vec<&str> = tl
        .ordered()
        .iter()
        .map(|e| e.profile.name.as_str())
        .collect();
    // zzz first (list_order 10), then non-spectators by name (adam, bob),
    // then the spectator last despite its name sorting first.
    assert_eq!(names, ["zzz", "adam", "bob", "aaa"]);
}

#[test]
fn display_name_overrides_profile_name() {
    let mut e = PlayerListEntry::new(profile("steve"));
    assert_eq!(e.effective_name().to_plain_string(), "steve");
    e.display_name = Some(Text::literal("Steve the Great"));
    assert_eq!(e.effective_name().to_plain_string(), "Steve the Great");
}

#[test]
fn header_and_footer_round_trip() {
    let mut tl = TabList::new();
    tl.header = Some(Text::literal("Welcome"));
    tl.footer = Some(Text::literal("Goodbye"));
    assert_eq!(tl.header.unwrap().to_plain_string(), "Welcome");
    assert_eq!(tl.footer.unwrap().to_plain_string(), "Goodbye");
}
