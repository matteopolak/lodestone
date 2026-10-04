//! A title-screen `MenuNav` that is past the ownership gate.
//!
//! A multiplayer-capable build draws the ownership gate in place of every
//! non-session screen until the account roster holds an account that owns the
//! game, so a gate that wants the real title screen needs a roster on disk next
//! to its `servers.json`. The account is a made-up one: nothing here signs in.

use lodestone::menu::nav::MenuNav;

/// A nav whose files live under a fresh temp directory named for `tag` and the
/// process, with one selected account in `profiles.json`.
pub fn owned_nav(tag: &str) -> MenuNav {
    let dir = std::env::temp_dir().join(format!("lodestone-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the temp roster directory must be creatable");
    let profile_id = uuid::Uuid::new_v4();
    let mut metadata = lodestone_auth::AccountsMetadata::default();
    metadata.upsert(lodestone_auth::AccountProfile {
        profile_id,
        username: "HudGateAccount".to_owned(),
        skin_url: None,
        last_used: 1,
    });
    metadata.selected = Some(profile_id);
    metadata
        .save_to(&dir.join("profiles.json"))
        .expect("the temp roster must be writable");
    MenuNav::with_path(dir.join("servers.json"))
}
