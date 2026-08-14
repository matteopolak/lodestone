//! End-to-end: **raising** render distance mid-session actually streams the new
//! rings.
//!
//! The owner's report was *"render distance doesn't seem to apply to the server
//! until I relog"*. Two halves. The client half was that the shell never
//! told the server at all; that is fixed, and `Session::set_render_distance` now
//! sends a `SetClientSettings`. The server half is this file's subject: the
//! `ClientInformationChanged` consumer clamped a live request against *this
//! connection's own join radius*, so **lowering** worked and **raising** was
//! silently clamped straight back. One value doing two jobs — see
//! `crate::server::ViewTracker::max_radius`.
//!
//! Why an end-to-end gate rather than a unit test on the clamp: the clamp is
//! three lines and both hypotheses satisfy it in isolation. What this asserts is
//! that a real `lodestone-client`, over the real `V770Adapter` and the real
//! `V770ServerProtocol`, ends up **holding more columns** than the join view had
//! — the thing the owner could see and the thing the arithmetic cannot fake.
//!
//! `WorldgenChunkSource` (the cheap solidity-only source), not the real
//! generator: this file never edits a block, and the real overworld generator
//! costs ~900 ms per column, which at 25 columns is most of a minute.

use std::time::Duration;

use lodestone_client::{ClientAction, ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::action::{
    ChatMode, ClientSettings, DisplayedSkinParts, MainHand, ParticleStatus,
};
use lodestone_server::{IntegratedServer, WorldgenChunkSource};
use lodestone_worldgen::density::Density;
use lodestone_v770::{V770ServerProtocol, adapter};

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// A cheap, deterministic terrain source — the same `YClampedGradient` shape
/// `drowning.rs`'s `dry_source` and `server_liveness.rs`'s `cheap_source` use.
/// The real overworld generator costs ~900 ms per column, and this file needs 49
/// of them; nothing here reads a block.
fn cheap_source() -> WorldgenChunkSource {
    let density = Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    };
    WorldgenChunkSource::new(density, -64, 384)
}

/// A settings packet carrying `view_distance`; every other field is a plausible
/// constant, exactly as `serverbound_protocol_hygiene.rs`'s own `settings()`.
fn settings(view_distance: i8) -> ClientSettings {
    ClientSettings {
        locale: "en_us".to_owned(),
        view_distance,
        chat_mode: ChatMode::Full,
        chat_colors: true,
        skin_parts: DisplayedSkinParts {
            cape: true,
            jacket: false,
            left_sleeve: true,
            right_sleeve: false,
            left_pants_leg: true,
            right_pants_leg: false,
            hat: true,
        },
        main_hand: MainHand::Right,
        text_filtering: false,
        allow_server_listing: true,
        particle_status: ParticleStatus::All,
    }
}

/// **Raised-render-distance acceptance gate.**
///
/// Joins at `view_radius = 1` (a 3×3, 9 columns), then asks for `3` (a 7×7, 49
/// columns) exactly as the render-distance slider does, and requires the client
/// to actually receive them.
///
/// Three predictions rather than "more than before", because a
/// plausible-but-wrong fix fails a different one of them:
///
/// 1. the join view is **exactly 9** columns — the precondition, and the thing a
///    fix that simply streamed everything at join would break;
/// 2. after the raise the client holds **exactly 49** — not "at least 10", which
///    an off-by-one ceiling (`view_radius + 1`, say) would also satisfy;
/// 3. **lowering still works**, in the same session and after the raise: back to
///    `1` forgets exactly the 40 columns just added. The ceiling must be a
///    ceiling, not a floor — a "fix" that stopped clamping altogether, or that
///    pinned the radius at its maximum, passes 1 and 2 and fails this.
#[tokio::test]
async fn raising_render_distance_mid_session_streams_the_new_rings() {
    let join_radius = 1; // 3x3 = 9 columns
    let source = cheap_source();
    let (server, client_io) =
        IntegratedServer::open_in_memory(V770ServerProtocol, source, join_radius);
    let (handle, _events) = ClientBuilder::new(address(), profile("Slider"), Box::new(adapter()))
        .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(9, Duration::from_secs(30))
        .await
        .expect("the 3x3 join view never arrived");
    assert_eq!(
        handle.loaded_chunks().len(),
        9,
        "precondition: the join view is the 3x3 square and nothing more — \
         without this the raise below could be measuring the join"
    );

    // The raise. This is byte-for-byte what `Session::set_render_distance`
    // sends when the slider moves.
    handle
        .send_action(ClientAction::SetClientSettings(settings(3)))
        .expect("client still connected");
    handle
        .wait_for(Duration::from_secs(30), |h| h.loaded_chunks().len() >= 49)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "raising render distance streamed nothing: still {} columns. \
                 This used to be clamped against this \
                 connection's own join radius, so a raise was a no-op.",
                handle.loaded_chunks().len()
            )
        });
    assert_eq!(
        handle.loaded_chunks().len(),
        49,
        "a request for radius 3 is the 7x7 square exactly, not a rounded-up or \
         off-by-one window"
    );

    // And the ceiling is still a ceiling: lowering in the same session, after
    // the raise, must forget exactly what the raise added.
    handle
        .send_action(ClientAction::SetClientSettings(settings(1)))
        .expect("client still connected");
    handle
        .wait_for(Duration::from_secs(30), |h| h.loaded_chunks().len() == 9)
        .await
        .expect("lowering render distance no longer forgets the outer rings");

    server.shutdown().await;
}
