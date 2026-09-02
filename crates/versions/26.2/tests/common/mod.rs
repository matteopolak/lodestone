//! Shared helpers for `lodestone-v26-2`'s live integration tests.
//!
//! This is compiled as an inline module (`mod common;`) into each test binary,
//! not as a separate crate, so it adds no Cargo dependency edge and keeps
//! `cargo xtask check-isolation` green. A sibling copy lives in
//! `lodestone-client`'s tests for that crate's own live joins; duplicating a
//! three-line helper across two test suites is cheaper than a shared
//! test-support crate that would create a real dependency edge.
// Not every test binary that pulls in `read_login_packet` below also needs a
// unique username (`server_disconnect.rs` does not), so this re-export is
// allowed to go unused in some of them.
#[allow(unused_imports)]
pub use lodestone_testsupport::unique_username;

use lodestone_core::Reader;
use lodestone_net::{Connection, Transport};
use lodestone_v26_2::packet_ids::login;

/// Reads one packet off `client`, transparently handling
/// `login::clientbound::LOGIN_COMPRESSION` along the way.
///
/// A real client is required to switch its own connection's compression state
/// the instant it decodes `login_compression` — `V770ServerProtocol::login_success`'s
/// own doc comment names the ordering hazard: the packet itself must arrive
/// **uncompressed** (the client cannot decompress a packet that announces
/// compression is starting), and every packet after it — starting with
/// `login_finished` — is framed **compressed**. A hand-rolled test join that
/// reads a fixed number of packets and never inspects them has no way to do
/// this, so once the server started sending `login_compression` every such
/// join misparsed every frame after it.
///
/// Every login-phase read in this test suite should go through this (or
/// [`drain`]) instead of `client.read_packet()` directly, so the next
/// directive added to the login sequence breaks in one place rather than in
/// every hand-rolled join.
pub async fn read_login_packet<T: Transport>(
    client: &mut Connection<T>,
) -> lodestone_net::Result<Option<(i32, Vec<u8>)>> {
    loop {
        let Some((id, payload)) = client.read_packet().await? else {
            return Ok(None);
        };
        if id == login::clientbound::LOGIN_COMPRESSION {
            // `LoginCompression`'s wire layout is a single VarInt threshold.
            let threshold = Reader::new(&payload)
                .var_i32()
                .expect("login_compression payload is a single VarInt threshold");
            client.set_compression(threshold);
            continue;
        }
        return Ok(Some((id, payload)));
    }
}
