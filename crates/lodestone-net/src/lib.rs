//! Lodestone networking: Minecraft packet framing, compression, and transport.
//!
//! The crate is layered so that the tricky wire logic is testable without any
//! I/O. [`Codec`] is a pure, synchronous framing state machine; [`Transport`]
//! abstracts the byte stream; and [`Connection`] combines the two into an async
//! packet interface.

mod codec;
mod connection;
mod crypto;
mod error;
#[cfg(any(
    all(feature = "ws-native", not(target_arch = "wasm32")),
    all(feature = "ws-web", target_arch = "wasm32")
))]
mod inbox;
mod ping;
mod resolve;
mod status;
mod transport;
#[cfg(all(feature = "ws-native", not(target_arch = "wasm32")))]
mod ws_native;
#[cfg(all(feature = "ws-web", target_arch = "wasm32"))]
mod ws_web;

pub use codec::{Codec, MAX_DECOMPRESSED_LEN, MAX_LENGTH_VARINT_BYTES, MAX_PACKET_LEN};
pub use connection::Connection;
pub use crypto::SHARED_SECRET_LEN;
#[cfg(not(target_arch = "wasm32"))]
pub use crypto::{generate_shared_secret, rsa_encrypt};
pub use error::{NetError, Result};
#[cfg(not(target_arch = "wasm32"))]
pub use ping::legacy_status;
pub use ping::{
    LegacyStatus, ServerListPing, StatusResponse, encode_legacy_ping_request, legacy_status_over,
    parse_legacy_status,
};
pub use resolve::{
    DEFAULT_PORT, ResolvedAddress, choose_address, should_query_srv, srv_query_name,
};
pub use status::{
    MAX_FAVICON_BYTES, PlayerSample, ServerStatus, decode_base64, decode_favicon, parse_status_json,
};
#[cfg(not(target_arch = "wasm32"))]
pub use status::server_status;
#[cfg(not(target_arch = "wasm32"))]
pub use resolve::{lookup_minecraft_srv, resolve_server_address};
pub use transport::{DEFAULT_MEMORY_BUFFER, Transport, memory_pair};
#[cfg(all(feature = "ws-native", not(target_arch = "wasm32")))]
pub use ws_native::WsTransport;
#[cfg(all(feature = "ws-web", target_arch = "wasm32"))]
pub use ws_web::WsWebTransport;
