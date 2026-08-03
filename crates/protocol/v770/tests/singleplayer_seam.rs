//! The **registry seam** for singleplayer, driven end-to-end to a joined
//! session (issue #287).
//!
//! `server_liveness.rs` already proves the real client can join the real
//! [`V770ServerProtocol`] over an in-memory duplex — but it names
//! `V770ServerProtocol` directly, which the shell is forbidden to do (see
//! `CLAUDE.md`'s version seam, and `cargo check -p lodestone-shell
//! --no-default-features`). So it proves the *server* works and says nothing
//! about the path production actually takes.
//!
//! This file takes the production path: a protocol **number** goes into
//! [`lodestone_registry::server_protocol_for_protocol`], a
//! `Box<dyn lodestone_server::ServerProtocol>` comes out, and *that box* is
//! served. Two things can only fail here:
//!
//! 1. **The registry has no serverbound entry.** `adapter_for_protocol` and
//!    `server_protocol_for_protocol` are separate tables, so a family can be
//!    joinable and unhostable. In the shell that is a launch error with a
//!    message, which is indistinguishable from a wiring bug unless something
//!    asserts the entry exists.
//! 2. **The box does not forward.** Thirteen of `ServerProtocol`'s eighteen
//!    methods have defaults, so a `Box<dyn ServerProtocol>` that failed to
//!    forward one would still compile and still serve — just silently without
//!    keep-alives, or without chunks. `lodestone-server`'s own
//!    `a_boxed_protocol_answers_exactly_as_the_concrete_one_does` covers that
//!    method-by-method; this covers the composition, which is the part a unit
//!    test cannot see.
//!
//! What is deliberately *not* here: the shell's own wiring
//! (`NetClient::open_singleplayer`, and the Play Selected World button that
//! calls it). That lives in `lodestone-shell`, which this crate must not
//! depend on; `lodestone-shell`'s
//! `pressing_play_reaches_a_running_integrated_server` is its opposite number.
//!
//! Terrain is [`WorldgenChunkSource`] over a trivial constant density, for
//! `server_liveness.rs`'s reason: this is about the seam, not the blocks, and
//! the real generator costs ~12 ms per column. The vertical extent is still the
//! real `min_y = -64` / `height = 384` overworld shape, because the client
//! hardcodes that by dimension name rather than reading it off the wire.

use std::time::Duration;

use lodestone_client::{ChunkPos, ClientBuilder, LoginProfile, ServerAddress};
use lodestone_server::{IntegratedServer, WorldgenChunkSource};
use lodestone_v770::adapter;
use lodestone_worldgen::density::Density;

/// Vanilla 26.2. The number the shell's `Config::protocol` defaults to, and the
/// only thing it knows about the version it is playing.
const PROTOCOL: i32 = 776;

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

/// See the module docs: cheap, deterministic, real vertical extent.
fn cheap_source() -> WorldgenChunkSource {
    let density = Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    };
    WorldgenChunkSource::new(density, -64, 384)
}

/// A protocol number, resolved through the registry, reaches a joined session
/// with terrain — no caller naming a version anywhere.
///
/// This is the whole of singleplayer minus the shell's thread and the button:
/// resolve, serve the box, join, receive the initial view.
#[tokio::test(start_paused = true)]
async fn a_registry_resolved_server_protocol_serves_a_real_joined_session() {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("the v770 family must be hostable, not just joinable");

    // `open_in_memory` takes `P: ServerProtocol` **by value**, so this line is
    // also the assertion that `Box<dyn ServerProtocol>` is servable at all.
    let (server, client_io) = IntegratedServer::open_in_memory(protocol, cheap_source(), 0);
    let (handle, _events) =
        ClientBuilder::new(address(), profile("RegistrySeam"), Box::new(adapter()))
            .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("a registry-resolved protocol never carried the client to spawn");

    // Spawn is chunk (0, 0) (`V770ServerProtocol::begin_play` puts the player at
    // 8, 100, 8) and `view_radius` is 0, so the initial view is exactly that one
    // column. Chunks are the load-bearing assertion rather than spawn alone:
    // login/configuration/join are five `ServerProtocol` methods with no
    // defaults, so they cannot silently fall through a box — `encode_chunk` and
    // the two batch markers are where an unforwarded method would show up as a
    // client that joins into a void.
    handle
        .wait_for_chunks(1, Duration::from_secs(60))
        .await
        .expect("the initial column never arrived through the boxed protocol");
    assert!(
        handle.is_chunk_loaded(ChunkPos::new(0, 0)),
        "the spawn column is not the one that arrived"
    );

    // Past several keep-alive intervals (virtual time — the clock is paused), so
    // a boxed protocol whose `encode_keep_alive` fell through to the trait
    // default would be caught: the server disconnects a client that never
    // answers a challenge, and no challenge is ever sent.
    tokio::time::sleep(Duration::from_secs(2 * 15 + 5)).await;
    assert!(
        !handle.is_finished(),
        "the session died across the keep-alive interval — the boxed protocol is \
         not answering the loop"
    );

    server.shutdown().await;
}

/// The negative side of the seam: a protocol number no compiled-in family
/// supports resolves to `None`.
///
/// The shell turns this into "singleplayer is unavailable in this build" and
/// says so on the error screen, so it is a real code path, not a hypothetical.
/// Without this the test above would pass just as well against a `find` that
/// matched unconditionally and handed out v770 for every number.
#[test]
fn an_unsupported_protocol_number_has_no_server_protocol() {
    assert!(lodestone_registry::server_protocol_for_protocol(-1).is_none());
    assert!(lodestone_registry::server_protocol_for_protocol(PROTOCOL - 1).is_none());
}
