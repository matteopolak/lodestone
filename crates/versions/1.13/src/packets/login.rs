//! Login-state packets for protocol 404.
//!
//! Every one of them is shared. `login` is the one state the 1.13 flattening
//! left completely alone: measured against `minecraft-data`, no login packet
//! changes shape between 1.12.2 and 1.13.2, and the two 1.13 *additions*
//! (`login_plugin_request`/`login_plugin_response`, the plugin-negotiation
//! pair) are not on the join path and are not modelled here.
//!
//! [`LoginSuccess`] is the dashed-*string* UUID form, which every protocol
//! from 47 to 578 sends; 1.16 replaced it with sixteen raw bytes. The shared
//! definition is already ranged `#[mc(protocols = "47..=578")]`, so 404 needed
//! no widening -- and the choice is load-bearing rather than cosmetic:
//! reading sixteen raw bytes where a length-prefixed 36-character string was
//! sent does not fail, it consumes the username too.

pub use lodestone_protocol_common::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart, LoginSuccess,
    SetCompression,
};
