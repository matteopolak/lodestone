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

/// Serves one client connection through login, configuration, the play join
/// sequence, and the initial chunk view — then keeps serving until the client
/// disconnects.
///
/// The loop transitions Handshaking → Login → Configuration → Play driven
/// entirely by the [`ServerProtocol`], acknowledgement by acknowledgement,
/// exactly mirroring the client-side `VersionAdapter`'s choreography:
///
/// 1. [`ServerBound::LoginStart`] → [`ServerProtocol::login_success`] (no
///    state change yet).
/// 2. [`ServerBound::LoginAcknowledged`] → state becomes
///    [`State::Configuration`], then [`ServerProtocol::begin_configuration`].
/// 3. [`ServerBound::ConfigurationFinished`] → state becomes [`State::Play`],
///    then [`ServerProtocol::begin_play`], then every column in
///    `[-view_radius, view_radius]²` (chunk coordinates) from `source` as a
///    single flow-controlled chunk batch
///    ([`ServerProtocol::begin_chunk_batch`]/
///    [`ServerProtocol::encode_chunk`]/[`ServerProtocol::end_chunk_batch`]).
///
/// Unlike the initial version of this loop, it does not return once the view
/// has been delivered: a real client stays connected past the join sequence
/// (keep-alives, movement, chunk-batch acknowledgements), so the loop keeps
/// reading and lifting packets — dispatching to [`ServerBound::Ignored`] for
/// anything not yet acted on — until the client closes the connection. The
/// summary is only available once that happens.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::ClosedBeforeLogin`] if the client hangs up before it ever
/// reaches [`ServerBound::LoginStart`].
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
    let mut username: Option<String> = None;
    let mut chunks_sent = 0usize;

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        match proto.decode(state, packet_id, &payload) {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            ServerBound::LoginStart {
                username: name,
                uuid,
            } => {
                username = Some(name.clone());
                for directive in proto.login_success(&name, uuid) {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::LoginAcknowledged => {
                state = State::Configuration;
                for directive in proto.begin_configuration() {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::ConfigurationFinished => {
                state = State::Play;
                for directive in proto.begin_play(view_radius) {
                    apply(conn, &mut state, directive).await?;
                }

                apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                let mut batch_size = 0;
                for cz in -view_radius..=view_radius {
                    for cx in -view_radius..=view_radius {
                        let column = source.column(cx, cz);
                        apply(conn, &mut state, proto.encode_chunk(cx, cz, &column)).await?;
                        batch_size += 1;
                    }
                }
                apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
                chunks_sent = batch_size as usize;
            }
            ServerBound::Ignored => {}
        }
    }

    match username {
        Some(username) => Ok(ServeSummary {
            username,
            chunks_sent,
        }),
        None => Err(ServerError::ClosedBeforeLogin),
    }
}
