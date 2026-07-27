//! G6 live acceptance: the tab list, end-to-end through the *real* client.
//!
//! This is the acceptance gate the brief demands: not "my decoder parses a
//! captured payload" (which can pass while wrong), but "the value is readable
//! through the client's **public API**". It drives the real
//! [`lodestone_client`] stack — connect, handshake, login, configuration, play —
//! and asserts that the server's `player_info_update` reaches
//! [`ClientHandle::players()`](lodestone_client::ClientHandle::players) with the
//! right name, game mode, and listed flag.
//!
//! The whole path is exercised: v770 decodes the action-bitmask packet, the
//! adapter lifts it to `ClientEvent::PlayerListUpdate`, the client folds it into
//! its read-model, and the public API returns it. A misparse anywhere shows up
//! as a missing or wrong entry.
//!
//! Gated behind the `live-tablist` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic and version-free. Run against the creative
//! server on `127.0.0.1:25570`:
//!
//! ```text
//! cargo test -p lodestone-game --features live-tablist \
//!     --test live_tablist -- --ignored --nocapture
//! ```
#![cfg(feature = "live-tablist")]

use lodestone_testsupport::unique_username;
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_model::{GameMode, PlayerListEntry};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires the lodestone-creative server on 127.0.0.1:25570"]
async fn tab_list_reaches_client_public_api() {
    println!("=== LIVE TAB-LIST ORACLE (protocol 776, creative :25570) ===");
    let username = unique_username();
    println!("username (unique per run): {username}");

    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25570,
    };
    let profile = LoginProfile {
        username: username.clone(),
        uuid: Uuid::new_v4(),
    };

    // Version selection through the registry; `lodestone-game` names no version.
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via lodestone-client/live-v770");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect(
            "connect to lodestone-creative on 127.0.0.1:25570 — start it with: \
             docker run --rm -d -p 25570:25570 -p 25571:25571 --name lodestone-creative <creative-image>",
        );

    // The driver runs as its own task and pushes events onto a *bounded*
    // channel, so we must keep draining it or the driver backpressures and
    // stops folding. After each event, re-query the public API.
    let deadline = Duration::from_secs(30);
    let result: Option<PlayerListEntry> = tokio::time::timeout(deadline, async {
        loop {
            match events.recv().await {
                Some(ClientEvent::Disconnect { reason }) => {
                    panic!("server disconnected us: {}", reason.to_plain_string());
                }
                Some(_) => {}
                None => return None,
            }
            // Poll, never assert immediately: the tab-list entry arrives over
            // one or more `player_info_update` packets after we reach Play.
            if let Some(entry) = handle
                .players()
                .into_iter()
                .find(|p| p.name.as_deref() == Some(username.as_str()))
            {
                return Some(entry);
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for the tab list to reach the client API \
             (health={:?}, alive={})",
            handle.health(),
            handle.is_alive()
        )
    });

    let me = result.expect("event stream closed before the tab list populated");
    println!("found self in tab list via ClientHandle::players(): {me:?}");

    // Assert on the server's own view, read back through the public API.
    assert_eq!(
        me.name.as_deref(),
        Some(username.as_str()),
        "tab-list name must match our profile"
    );
    assert_eq!(
        me.game_mode,
        Some(GameMode::Creative),
        "creative server forces creative game mode"
    );
    assert_eq!(
        me.listed,
        Some(true),
        "a joined player is listed in the tab list"
    );

    println!("=== TAB-LIST ORACLE PASSED: player_info_update reaches ClientHandle::players() ===");
    handle.shutdown();
}
