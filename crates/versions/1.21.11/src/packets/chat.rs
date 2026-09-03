//! Chat packets for this era (protocol 774).
//!
//! # Three clientbound packets, split by what the client may trust
//!
//! | packet | who wrote the text | signed |
//! |---|---|---|
//! | [`PlayerChat`] | a player | optionally, over exact bytes it also carries |
//! | [`ProfilelessChat`] | the server, on a player's behalf | no |
//! | [`SystemChat`] | the server | no |
//!
//! # Three things separate this era's chat from the 1.20.6 era's
//!
//! * **A `global_index` varint leads [`PlayerChat`].** It is a per-connection
//!   monotone counter over *all* delivered chat, and it sits before the sender
//!   uuid — so a decoder carried forward reads the counter's first byte as the
//!   first byte of a uuid and every field after it is garbage.
//! * **The chat type is a registry-entry holder, not a bare id.** A holder
//!   writes `id + 1`, reserving `0` for "the definition follows inline". The
//!   off-by-one is silent: reading the raw value picks the chat format one
//!   past the intended one, which is a plausible format rather than an error.
//!   See [`ChatTypeHolder`].
//! * **Both serverbound signed-chat packets end with a checksum byte.** One
//!   byte over the acknowledgement window, after the fixed three-byte bit set.
//!   Omitting it leaves the server reading the next packet's length prefix as
//!   the checksum, which manifests as a disconnect a packet later rather than
//!   at the packet that caused it.
//!
//! # The acknowledgement chain, and why an unsigned client still pays for it
//!
//! The server keeps a per-connection list of pending signed messages. A
//! serverbound chat message — signed or not — carries a *last-seen* update: an
//! offset counting newly-seen messages plus a fixed 20-bit set naming which of
//! the tracked window they were, packed into three bytes with no length
//! prefix. A client that never signs anything still has to send that tail, and
//! still has to drain the pending list with [`MessageAcknowledgement`] when it
//! is not sending chat, or the list grows until the server drops the
//! connection.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::common::{NetworkNbt, read_registry_holder_id};

/// A full 256-byte message signature.
///
/// A newtype rather than a bare `[u8; 256]` so the fixed width is stated once
/// and cannot be given a length prefix by accident: the wire form carries no
/// count, and a signature written with one desynchronises every byte after it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct MessageSignature(#[mc(fixed = 256)] pub [u8; 256]);

/// A chat type named as a **registry-entry holder**.
///
/// The wire value is `id + 1`, with `0` reserved for an inline definition.
/// Decoding refuses the inline form rather than guessing its width — a chat
/// type's inline payload is a component-decoration structure whose length is
/// implied by its own type, so there is nothing to skip past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatTypeHolder(pub i32);

impl Encode for ChatTypeHolder {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        w.var_i32(self.0 + 1);
        Ok(())
    }
}

impl Decode for ChatTypeHolder {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self(read_registry_holder_id(r, "chat type")?))
    }
}

/// One entry of a [`PlayerChat`] message's last-seen chain.
///
/// The wire writes `id + 1`, so a zero means "no cache reference; a full
/// 256-byte signature follows" and any other value is a one-based index into
/// the connection's own signature cache. Modelling the raw wire value keeps
/// the `0` sentinel visible at the one place it is interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousMessage {
    /// The raw wire value: `0` means an inline signature follows, otherwise
    /// a one-based cache index.
    pub id: i32,
    /// The inline signature, present exactly when `id` is `0`.
    pub signature: Option<MessageSignature>,
}

impl Encode for PreviousMessage {
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        w.var_i32(self.id);
        if let Some(signature) = &self.signature {
            signature.encode(w, ctx)?;
        }
        Ok(())
    }
}

impl Decode for PreviousMessage {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let id = r.var_i32()?;
        let signature = if id == 0 {
            Some(MessageSignature::decode(r, ctx)?)
        } else {
            None
        };
        Ok(Self { id, signature })
    }
}

/// Clientbound `minecraft:player_chat` — a message a player wrote, carrying
/// everything a verifier needs to check the signature over it.
///
/// The two text fields are **not** interchangeable. `plain_message` is the
/// exact string the signature was taken over; `unsigned_content` is the
/// server's decorated form, which is what a client displays. A verifier that
/// hashes the decorated form fails every message on a server with a chat
/// format plugin, and a client that displays the plain form loses the server's
/// formatting — so both are carried.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_chat", state = Play, bound = Client, protocols = "774..=774")]
pub struct PlayerChat {
    /// Per-connection counter over all delivered chat, in delivery order.
    #[mc(varint)]
    pub global_index: i32,
    /// Profile UUID of the sending player — the filter key.
    pub sender: Uuid,
    /// This message's position in the sender's own signing chain.
    #[mc(varint)]
    pub index: i32,
    /// The message signature, when the sender had a signing session.
    pub signature: Option<MessageSignature>,
    /// The exact signed text.
    #[mc(max = 256)]
    pub plain_message: String,
    /// Signed timestamp, epoch **milliseconds** (the wire's own unit).
    pub timestamp: i64,
    /// Signed random salt.
    pub salt: i64,
    /// The last-seen chain this message was signed over.
    pub previous_messages: Vec<PreviousMessage>,
    /// The server-decorated form as a component, when present.
    pub unsigned_content: Option<NetworkNbt>,
    /// Filter result: `0` shown in full, `1` fully filtered, `2` partially
    /// filtered with a bit mask naming which characters.
    #[mc(varint)]
    pub filter_type: i32,
    /// The partial-filter bit mask, present only for `filter_type == 2`.
    #[mc(present_if = "filter_type == 2")]
    pub filter_mask: Option<Vec<i64>>,
    /// Chat-type holder, into the registry the configuration phase delivered.
    pub chat_type: ChatTypeHolder,
    /// The sender's display name, as a component.
    pub network_name: NetworkNbt,
    /// The target's display name — whispers name their target.
    pub target_name: Option<NetworkNbt>,
}

/// Clientbound `minecraft:disguised_chat` — a message in a player-chat
/// *format* whose author the server cannot vouch for (a command-issued
/// broadcast, a plugin message).
///
/// Carries no sender id and no signature, so nothing here is verifiable and
/// nothing contributes to the acknowledgement window.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:disguised_chat", state = Play, bound = Client, protocols = "774..=774")]
pub struct ProfilelessChat {
    /// The message body.
    pub message: NetworkNbt,
    /// Chat-type holder.
    pub chat_type: ChatTypeHolder,
    /// The claimed sender's display name.
    pub network_name: NetworkNbt,
    /// The target's display name, when the format names one.
    pub target_name: Option<NetworkNbt>,
}

/// Clientbound `minecraft:system_chat` — server text: join/leave notices,
/// command output, action-bar overlays.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:system_chat", state = Play, bound = Client, protocols = "774..=774")]
pub struct SystemChat {
    /// The message body.
    pub content: NetworkNbt,
    /// Whether to render on the action bar instead of the chat pane.
    pub is_action_bar: bool,
}

/// Clientbound `minecraft:delete_chat` — retract a previously-delivered signed
/// message.
///
/// The wire writes `id + 1` with the same sentinel [`PreviousMessage`]
/// documents: zero means a full signature follows.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:delete_chat", state = Play, bound = Client, protocols = "774..=774")]
pub struct DeleteChat {
    /// `0` means an inline signature follows; otherwise a one-based cache
    /// index.
    pub id: i32,
    /// The inline signature, present exactly when `id` is `0`.
    pub signature: Option<MessageSignature>,
}

impl Encode for DeleteChat {
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        w.var_i32(self.id);
        if let Some(signature) = &self.signature {
            signature.encode(w, ctx)?;
        }
        Ok(())
    }
}

impl Decode for DeleteChat {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let id = r.var_i32()?;
        let signature = if id == 0 {
            Some(MessageSignature::decode(r, ctx)?)
        } else {
            None
        };
        Ok(Self { id, signature })
    }
}

/// Serverbound `minecraft:chat` — send a chat message plus the last-seen
/// acknowledgement update every send piggybacks.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Server, protocols = "774..=774")]
pub struct ChatMessage {
    /// Message text.
    #[mc(max = 256)]
    pub message: String,
    /// Client timestamp, epoch milliseconds.
    pub timestamp: i64,
    /// Random salt used for signing.
    pub salt: i64,
    /// Message signature, absent for unsigned chat.
    pub signature: Option<MessageSignature>,
    /// Offset of the last-seen acknowledgement window.
    #[mc(varint)]
    pub last_seen_offset: i32,
    /// Fixed 20-bit acknowledged bit set, packed into 3 bytes with no length
    /// prefix.
    #[mc(fixed = 3)]
    pub acknowledged: [u8; 3],
    /// Checksum over the acknowledgement window; `0` means "not computed",
    /// which is what a server accepts from a client with no signing session.
    pub checksum: u8,
}

impl ChatMessage {
    /// Builds an unsigned chat message with an empty acknowledgement window.
    ///
    /// Accepted by any server not enforcing secure profiles, which is the only
    /// mode this crate can reach: signing needs a session key from an
    /// authenticated profile.
    #[must_use]
    pub fn unsigned(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timestamp: 0,
            salt: 0,
            signature: None,
            last_seen_offset: 0,
            acknowledged: [0; 3],
            checksum: 0,
        }
    }
}

/// Serverbound `minecraft:chat_command` — send a command, unsigned.
///
/// One string and nothing else: no timestamp, no acknowledgement tail, and so
/// no checksum byte either.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat_command", state = Play, bound = Server, protocols = "774..=774")]
pub struct ChatCommand {
    /// Command text without the leading `/`.
    pub command: String,
}

/// One per-argument signature inside [`ChatCommandSigned`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ArgumentSignatureEntry {
    /// Argument name.
    #[mc(max = 16)]
    pub name: String,
    /// Signature over that argument's raw text.
    pub signature: MessageSignature,
}

/// Serverbound `minecraft:chat_command_signed` — a command whose arguments
/// carry per-argument signatures.
///
/// Only a client with a chat-signing session can produce one. It is modelled
/// so a stream containing it round-trips, not because this crate sends it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chat_command_signed",
    state = Play,
    bound = Server,
    protocols = "774..=774"
)]
pub struct ChatCommandSigned {
    /// Command text without the leading `/`.
    pub command: String,
    /// Client timestamp, epoch milliseconds.
    pub timestamp: i64,
    /// Random salt used for signing.
    pub salt: i64,
    /// Per-argument signatures.
    pub argument_signatures: Vec<ArgumentSignatureEntry>,
    /// Offset of the last-seen acknowledgement window.
    #[mc(varint)]
    pub last_seen_offset: i32,
    /// Fixed 20-bit acknowledged bit set, packed into 3 bytes.
    #[mc(fixed = 3)]
    pub acknowledged: [u8; 3],
    /// Checksum over the acknowledgement window — see [`ChatMessage`].
    pub checksum: u8,
}

/// Serverbound `minecraft:chat_ack` — drain the server's pending signed
/// message list without sending chat.
///
/// Without it the pending list grows until the server's own cap forces a
/// disconnect, so a client that reads chat and never writes it still has to
/// send this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat_ack", state = Play, bound = Server, protocols = "774..=774")]
pub struct MessageAcknowledgement {
    /// Number of newly-acknowledged pending signed messages.
    #[mc(varint)]
    pub count: i32,
}

/// Serverbound `minecraft:chat_session_update` — announce this client's
/// chat-signing session.
///
/// Required before a server accepts any signed message this session sends.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chat_session_update",
    state = Play,
    bound = Server,
    protocols = "774..=774"
)]
pub struct ChatSessionUpdate {
    /// This client's chat-session UUID.
    pub session_id: Uuid,
    /// Profile public key expiry, epoch milliseconds.
    pub expires_at: i64,
    /// DER-encoded RSA public key, verbatim from the profile certificate.
    pub public_key: Vec<u8>,
    /// The session server's signature over `public_key`, verbatim.
    pub key_signature: Vec<u8>,
}
