//! Login-state packets for protocol 340.
//!
//! `LoginStart`, `EncryptionRequest`, `EncryptionResponse`, `LoginDisconnect`
//! and `SetCompression` are byte-identical to v1-8's and v1-14's own login
//! packets (measured: no hand-written codec on either side), so they now
//! live in `lodestone-protocol-common`. `LoginSuccess` sends the profile
//! UUID as a **dashed string**, matching v1-8 but not 754 (1.16, which
//! switched to a 128-bit binary UUID) -- shared only with v1-8 via
//! `#[mc(protocols = "47..=340")]`; see that crate's `packets::login` module
//! docs.

pub use lodestone_protocol_common::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, LoginSuccess,
    SetCompression,
};
