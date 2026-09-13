//! Chat packets for this era (protocol 762) — the wire generation where a
//! chat message became a signed, ordered, acknowledged object rather than a
//! string.
//!
//! # What 1.19 replaced, and why it is three packets instead of one
//!
//! Every era below carries one clientbound `chat` packet: a JSON component
//! and a position byte. That single packet cannot express the distinction
//! 1.19 introduced, so it was split by *what the client may trust*:
//!
//! | packet | who wrote the text | signed |
//! |---|---|---|
//! | [`PlayerChat`] | a player | optionally, over exact bytes it also carries |
//! | [`ProfilelessChat`] | the server, on a player's behalf | no |
//! | [`SystemChat`] | the server | no |
//!
//! Only the first carries a sender profile id, which is the key a
//! hide-in-chat filter needs, and only the first contributes to the
//! acknowledgement window below.
//!
//! # The acknowledgement chain, and why an unsigned client still pays for it
//!
//! The server keeps a per-connection list of pending signed messages. Every
//! serverbound chat packet — signed or not — carries a *last-seen* update:
//! an offset saying how many newly-seen messages are being acknowledged, plus
//! a fixed 20-bit set naming which of the tracked window they were. A client
//! that never signs anything still has to send that tail, and still has to
//! drain the pending list with [`MessageAcknowledgement`] when it is not
//! sending chat of its own, or the server's list grows until it disconnects
//! the connection. That is why this crate carries the whole mechanism
//! rather than the string half of it.
//!
//! # Where this era's shape differs from the modern one
//!
//! Three differences from the 26.2 family's otherwise similar packets, each
//! of which silently mis-frames a stream if inherited:
//!
//! * **Components are JSON strings here, not network NBT.** 1.20.3 moved
//!   them; at 762 every text field on the wire is a length-prefixed JSON
//!   string.
//! * **There is no server-global message index** on [`PlayerChat`]. The
//!   modern packet opens with one; this one opens with the sender UUID.
//! * **There is no acknowledgement checksum byte** on the serverbound
//!   packets. It is 1.21.5's addition; writing one here appends a byte the
//!   server reads as the next packet's length.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

/// A full 256-byte message signature.
///
/// A newtype rather than a bare `[u8; 256]` so the fixed width is stated once
/// and cannot be given a length prefix by accident: the wire form carries no
/// count, and a signature written with one desynchronises every byte after
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct MessageSignature(#[mc(fixed = 256)] pub [u8; 256]);

/// One entry of a [`PlayerChat`] message's last-seen chain.
///
/// The wire writes `id + 1`, so a zero means "no cache reference; a full
/// 256-byte signature follows" and any other value is a one-based index into
/// the connection's own signature cache. Modelling the raw wire value rather
/// than the decoded index keeps the `0` sentinel visible at the one place it
/// is interpreted.
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

/// Clientbound `player_chat` — a message a player wrote, carrying everything
/// a verifier needs to check the signature over it.
///
/// The two text fields are **not** interchangeable. `plain_message` is the
/// exact string the signature was taken over; `unsigned_content` is the
/// server's decorated form, which is what a client displays. A verifier that
/// hashes the decorated form fails every message on a server with a chat
/// format plugin, and a client that displays the plain form loses the
/// server's formatting — so both are carried.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_chat", state = Play, bound = Client, protocols = "762..=762")]
pub struct PlayerChat {
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
    /// Whether a server-decorated form follows.
    pub has_unsigned_content: bool,
    /// The server-decorated form as a JSON component, when present.
    #[mc(present_if = "has_unsigned_content == true")]
    pub unsigned_content: Option<String>,
    /// Filter result: `0` shown in full, `1` fully filtered, `2` partially
    /// filtered with a bit mask naming which characters.
    #[mc(varint)]
    pub filter_type: i32,
    /// The partial-filter bit mask, present only for `filter_type == 2`.
    #[mc(present_if = "filter_type == 2")]
    pub filter_mask: Option<Vec<i64>>,
    /// Chat-type registry id, into the registry the join packet delivered.
    #[mc(varint)]
    pub chat_type: i32,
    /// The sender's display name, as a JSON component.
    pub network_name: String,
    /// Whether a target name follows (whispers name their target).
    pub has_target_name: bool,
    /// The target's display name, as a JSON component.
    #[mc(present_if = "has_target_name == true")]
    pub target_name: Option<String>,
}

/// Clientbound `profileless_chat` — a message in a player-chat *format* whose
/// author the server cannot vouch for (a command-issued `/say`, a plugin
/// broadcast).
///
/// Carries no sender id and no signature, so nothing here is verifiable and
/// nothing contributes to the acknowledgement window.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:profileless_chat", state = Play, bound = Client, protocols = "762..=762")]
pub struct ProfilelessChat {
    /// The message body, as a JSON component.
    pub message: String,
    /// Chat-type registry id.
    #[mc(varint)]
    pub chat_type: i32,
    /// The claimed sender's display name, as a JSON component.
    pub network_name: String,
    /// Whether a target name follows.
    pub has_target_name: bool,
    /// The target's display name, as a JSON component.
    #[mc(present_if = "has_target_name == true")]
    pub target_name: Option<String>,
}

/// Clientbound `system_chat` — server text: join/leave notices, command
/// output, action-bar overlays.
///
/// The trailing boolean selects the action bar rather than the chat pane. It
/// replaces the position byte the eras below use, which had three states; the
/// system/chat distinction it also carried is now the packet's own identity.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:system_chat", state = Play, bound = Client, protocols = "762..=762")]
pub struct SystemChat {
    /// The message body, as a JSON component.
    pub content: String,
    /// Whether to render on the action bar instead of the chat pane.
    pub is_action_bar: bool,
}

/// Clientbound `hide_message` — retract a previously-delivered signed message.
///
/// The wire writes `id + 1` with the same sentinel [`PreviousMessage`]
/// documents: zero means a full signature follows.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:hide_message", state = Play, bound = Client, protocols = "762..=762")]
pub struct HideMessage {
    /// `0` means an inline signature follows; otherwise a one-based cache
    /// index.
    pub id: i32,
    /// The inline signature, present exactly when `id` is `0`.
    pub signature: Option<MessageSignature>,
}

impl Encode for HideMessage {
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        w.var_i32(self.id);
        if let Some(signature) = &self.signature {
            signature.encode(w, ctx)?;
        }
        Ok(())
    }
}

impl Decode for HideMessage {
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

/// Serverbound `chat_message` — send a chat message plus the last-seen
/// acknowledgement update every send piggybacks.
///
/// Note what is **absent** relative to the modern family's twin: no
/// acknowledgement checksum byte. Writing one appends a byte the server reads
/// as the next packet's length prefix.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat_message", state = Play, bound = Server, protocols = "762..=762")]
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
}

impl ChatMessage {
    /// Builds an unsigned chat message with an empty acknowledgement window.
    ///
    /// Accepted by any server not running with secure-profile enforcement,
    /// which is the only mode this crate can reach: signing needs a session
    /// key from an authenticated profile, and online-mode login is not
    /// implemented for this era.
    #[must_use]
    pub fn unsigned(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timestamp: 0,
            salt: 0,
            signature: None,
            last_seen_offset: 0,
            acknowledged: [0; 3],
        }
    }
}

/// Serverbound `chat_command` — send a command.
///
/// At 762 this is the *only* command packet: the signed-argument variant the
/// modern family splits out does not exist yet, and the per-argument
/// signature list is carried inline here instead. An unsigned client sends an
/// empty list.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat_command", state = Play, bound = Server, protocols = "762..=762")]
pub struct ChatCommand {
    /// Command text without the leading `/`.
    pub command: String,
    /// Client timestamp, epoch milliseconds.
    pub timestamp: i64,
    /// Random salt used for signing.
    pub salt: i64,
    /// Per-argument signatures; empty for an unsigned client.
    pub argument_signatures: Vec<ArgumentSignatureEntry>,
    /// Offset of the last-seen acknowledgement window.
    #[mc(varint)]
    pub last_seen_offset: i32,
    /// Fixed 20-bit acknowledged bit set, packed into 3 bytes.
    #[mc(fixed = 3)]
    pub acknowledged: [u8; 3],
}

impl ChatCommand {
    /// Builds an unsigned command with an empty acknowledgement window.
    #[must_use]
    pub fn unsigned(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            timestamp: 0,
            salt: 0,
            argument_signatures: Vec::new(),
            last_seen_offset: 0,
            acknowledged: [0; 3],
        }
    }
}

/// One per-argument signature inside [`ChatCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ArgumentSignatureEntry {
    /// Argument name.
    #[mc(max = 16)]
    pub name: String,
    /// Signature over that argument's raw text.
    pub signature: MessageSignature,
}

/// Serverbound `message_acknowledgement` — drain the server's pending signed
/// message list without sending chat.
///
/// Without it the pending list grows until the server's own cap forces a
/// disconnect, so a client that reads chat and never writes it still has to
/// send this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:message_acknowledgement",
    state = Play,
    bound = Server,
    protocols = "762..=762"
)]
pub struct MessageAcknowledgement {
    /// Number of newly-acknowledged pending signed messages.
    #[mc(varint)]
    pub count: i32,
}

/// Serverbound `chat_session_update` — announce this client's chat-signing
/// session.
///
/// Required before a server accepts any signed message this session sends.
/// Unlike the modern family's twin, the key and its signature are the only
/// payload beyond the session id and expiry — there is no version byte.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chat_session_update",
    state = Play,
    bound = Server,
    protocols = "762..=762"
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
