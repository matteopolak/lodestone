//! The generic integrated-server driver.
//!
//! [`serve_connection`] runs the server side of a single client connection over
//! any [`Transport`]: it reads packets through the shared
//! [`Connection`](lodestone_net::Connection) codec, lifts them with a
//! [`ServerProtocol`], plays the login sequence, and streams the initial view's
//! chunks from a [`ChunkSource`]. The identical loop serves an in-memory
//! [`memory_pair`](lodestone_net::memory_pair) client (singleplayer) or a
//! `TcpStream` client (open-to-LAN).

use lodestone_core::State;
use lodestone_net::{Connection, NetError, Transport};

use crate::chunk::ChunkSource;
use crate::protocol::{ServerBound, ServerDirective, ServerProtocol};

/// Outcome of serving a connection's initial view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeSummary {
    /// The username the client logged in as.
    pub username: String,
    /// Number of chunk columns sent for the initial view.
    pub chunks_sent: usize,
}

/// Errors from the integrated-server driver.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The underlying transport/codec failed.
    #[error("network error: {0}")]
    Net(#[from] NetError),
    /// The client disconnected before completing login.
    #[error("client closed before login completed")]
    ClosedBeforeLogin,
}

async fn apply<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    directive: ServerDirective,
) -> Result<(), ServerError> {
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload).await?;
        }
        ServerDirective::SetState(next) => *state = next,
        ServerDirective::SetCompression(threshold) => conn.set_compression(threshold),
    }
    Ok(())
}

/// Serves one client connection through login and the initial chunk view.
///
/// The loop transitions Handshaking → Login → Play driven entirely by the
/// [`ServerProtocol`]. On [`ServerBound::LoginStart`] it runs the login
/// sequence, then generates and sends every column in
/// `[-view_radius, view_radius]²` (chunk coordinates) from `source`. It returns
/// once the initial view has been delivered.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::ClosedBeforeLogin`] if the client hangs up first.
pub async fn serve_connection<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    view_radius: i32,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    let mut state = State::Handshaking;

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        match proto.decode(state, packet_id, &payload) {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            ServerBound::LoginStart { username } => {
                for directive in proto.login_sequence(&username) {
                    apply(conn, &mut state, directive).await?;
                }

                let mut chunks_sent = 0;
                for cz in -view_radius..=view_radius {
                    for cx in -view_radius..=view_radius {
                        let column = source.column(cx, cz);
                        apply(conn, &mut state, proto.encode_chunk(cx, cz, &column)).await?;
                        chunks_sent += 1;
                    }
                }

                return Ok(ServeSummary {
                    username,
                    chunks_sent,
                });
            }
            ServerBound::Ignored => {}
        }
    }

    Err(ServerError::ClosedBeforeLogin)
}
