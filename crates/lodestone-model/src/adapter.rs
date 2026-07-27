use thiserror::Error;
use uuid::Uuid;

use crate::{action::ClientAction, event::ClientEvent, text::Text};

/// Packet direction relative to an endpoint, re-exported from `lodestone-core`
/// so adapter id tables can use the same direction type as packet metadata.
pub use lodestone_core::Bound;
/// Version-free connection state, re-exported from `lodestone-core` so protocol
/// packets and canonical model adapters share one type identity.
pub use lodestone_core::State as ConnectionState;

/// The world write seam handed to [`VersionAdapter::handle_packet`], re-exported
/// so protocol crates and consumers share one trait identity. Adapters apply
/// decoded chunks through this rather than surfacing them as events.
pub use lodestone_world::{LoadedChunk, WorldSink};

/// A side effect an adapter asks the connection layer to perform.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum Directive {
    /// Write a packet with this protocol-specific id and body.
    Send {
        /// Protocol-specific packet id.
        packet_id: i32,
        /// Encoded packet body.
        payload: Vec<u8>,
    },
    /// Move the connection to a new state.
    ///
    /// Applies after any [`Directive::Send`] values emitted before it in the
    /// same batch.
    SetState(ConnectionState),
    /// Enable or reconfigure zlib compression.
    ///
    /// Negative thresholds disable compression.
    SetCompression(i32),
    /// Surface a canonical event to the library user.
    Emit(ClientEvent),
    /// Begin the online-mode encryption handshake.
    ///
    /// Carries only the *protocol-shaped* inputs the server sent in its
    /// encryption request: the ASCII server id used in the auth hash, the
    /// server's DER RSA public key, the verify token to echo back, and whether
    /// the server expects a Mojang session-server call. No crypto or I/O is
    /// implied here — the driver generates the shared secret, RSA-wraps it and
    /// the token, optionally authenticates, then asks the adapter to frame the
    /// reply via [`VersionAdapter::build_encryption_response`] and enables its
    /// cipher. Keeping the *framing* (packet id + byte-array layout, which
    /// differs across versions) in the adapter and the *crypto* in the driver is
    /// the whole point of this split.
    BeginEncryption {
        /// ASCII server id used when computing the authentication hash.
        server_id: String,
        /// The server's DER-encoded RSA public key.
        public_key: Vec<u8>,
        /// Verify token the client must echo back encrypted.
        verify_token: Vec<u8>,
        /// Whether the server expects a Mojang session-server join call.
        should_authenticate: bool,
    },
    /// The connection should be closed.
    Disconnect(Text),
}

/// Identity the client presents during login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginProfile {
    /// Player username.
    pub username: String,
    /// Player profile UUID.
    pub uuid: Uuid,
}

/// Where the client is connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddress {
    /// Server hostname or IP literal.
    pub host: String,
    /// Server port.
    pub port: u16,
}

/// Error returned by a [`VersionAdapter`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdapterError {
    /// The packet payload could not be decoded.
    #[error("failed to decode packet: {0}")]
    Decode(String),
    /// The action could not be encoded.
    #[error("failed to encode action: {0}")]
    Encode(String),
    /// The server requires a protocol feature this adapter does not implement.
    #[error("unsupported protocol feature: {0}")]
    Unsupported(String),
    /// The state is not supported by the adapter for this inbound packet.
    #[error("unsupported packet state {state:?}")]
    UnsupportedPacketState {
        /// Connection state.
        state: ConnectionState,
    },
    /// The action is not supported by the adapter in the current state.
    #[error("unsupported client action {action:?} in state {state:?}")]
    UnsupportedAction {
        /// Connection state.
        state: ConnectionState,
        /// Unsupported action.
        action: ClientActionKind,
    },
}

/// Compact action kind used in adapter errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientActionKind {
    /// [`ClientAction::SendChat`].
    SendChat,
    /// [`ClientAction::SendCommand`].
    SendCommand,
    /// [`ClientAction::ChatAck`].
    ChatAck,
    /// [`ClientAction::Move`].
    Move,
    /// [`ClientAction::KeepAliveResponse`].
    KeepAliveResponse,
    /// [`ClientAction::Respawn`].
    Respawn,
    /// [`ClientAction::SwingArm`].
    SwingArm,
    /// [`ClientAction::BlockAction`].
    BlockAction,
    /// [`ClientAction::DropSelectedItem`].
    DropSelectedItem,
    /// [`ClientAction::DropSelectedItemStack`].
    DropSelectedItemStack,
    /// [`ClientAction::SwapItemWithOffhand`].
    SwapItemWithOffhand,
    /// [`ClientAction::ReleaseUseItem`].
    ReleaseUseItem,
    /// [`ClientAction::Stab`].
    Stab,
    /// [`ClientAction::UseItemOn`].
    UseItemOn,
    /// [`ClientAction::UseItem`].
    UseItem,
    /// [`ClientAction::InteractEntity`].
    InteractEntity,
    /// [`ClientAction::ContainerClick`].
    ContainerClick,
    /// [`ClientAction::ContainerClose`].
    ContainerClose,
    /// [`ClientAction::SetCarriedItem`].
    SetCarriedItem,
    /// [`ClientAction::SetCreativeModeSlot`].
    SetCreativeModeSlot,
    /// [`ClientAction::SetPlayerInput`].
    SetPlayerInput,
    /// [`ClientAction::PlayerCommand`].
    PlayerCommand,
    /// [`ClientAction::Disconnect`].
    Disconnect,
    /// [`ClientAction::SetClientSettings`].
    SetClientSettings,
    /// [`ClientAction::SendBrand`].
    SendBrand,
    /// [`ClientAction::PongResponse`].
    PongResponse,
    /// [`ClientAction::ResourcePackResponse`].
    ResourcePackResponse,
    /// [`ClientAction::EndClientTick`].
    EndClientTick,
    /// [`ClientAction::ContainerButtonClick`].
    ContainerButtonClick,
    /// [`ClientAction::SetFlying`].
    SetFlying,
    /// [`ClientAction::RenameItem`].
    RenameItem,
    /// [`ClientAction::SelectTrade`].
    SelectTrade,
    /// [`ClientAction::PickItemFromBlock`].
    PickItemFromBlock,
    /// [`ClientAction::PickItemFromEntity`].
    PickItemFromEntity,
    /// [`ClientAction::SetBeaconEffects`].
    SetBeaconEffects,
    /// [`ClientAction::EditBook`].
    EditBook,
    /// [`ClientAction::SignUpdate`].
    SignUpdate,
    /// [`ClientAction::SetCommandBlock`].
    SetCommandBlock,
}

impl From<&ClientAction> for ClientActionKind {
    fn from(value: &ClientAction) -> Self {
        match value {
            ClientAction::SendChat { .. } => Self::SendChat,
            ClientAction::SendCommand { .. } => Self::SendCommand,
            ClientAction::ChatAck { .. } => Self::ChatAck,
            ClientAction::Move { .. } => Self::Move,
            ClientAction::KeepAliveResponse { .. } => Self::KeepAliveResponse,
            ClientAction::Respawn => Self::Respawn,
            ClientAction::SwingArm { .. } => Self::SwingArm,
            ClientAction::BlockAction { .. } => Self::BlockAction,
            ClientAction::DropSelectedItem => Self::DropSelectedItem,
            ClientAction::DropSelectedItemStack => Self::DropSelectedItemStack,
            ClientAction::SwapItemWithOffhand => Self::SwapItemWithOffhand,
            ClientAction::ReleaseUseItem => Self::ReleaseUseItem,
            ClientAction::Stab => Self::Stab,
            ClientAction::UseItemOn { .. } => Self::UseItemOn,
            ClientAction::UseItem { .. } => Self::UseItem,
            ClientAction::InteractEntity { .. } => Self::InteractEntity,
            ClientAction::ContainerClick { .. } => Self::ContainerClick,
            ClientAction::ContainerClose { .. } => Self::ContainerClose,
            ClientAction::SetCarriedItem { .. } => Self::SetCarriedItem,
            ClientAction::SetCreativeModeSlot { .. } => Self::SetCreativeModeSlot,
            ClientAction::SetPlayerInput(_) => Self::SetPlayerInput,
            ClientAction::PlayerCommand { .. } => Self::PlayerCommand,
            ClientAction::Disconnect => Self::Disconnect,
            ClientAction::SetClientSettings(_) => Self::SetClientSettings,
            ClientAction::SendBrand { .. } => Self::SendBrand,
            ClientAction::PongResponse { .. } => Self::PongResponse,
            ClientAction::ResourcePackResponse { .. } => Self::ResourcePackResponse,
            ClientAction::EndClientTick => Self::EndClientTick,
            ClientAction::ContainerButtonClick { .. } => Self::ContainerButtonClick,
            ClientAction::SetFlying { .. } => Self::SetFlying,
            ClientAction::RenameItem { .. } => Self::RenameItem,
            ClientAction::SelectTrade { .. } => Self::SelectTrade,
            ClientAction::PickItemFromBlock { .. } => Self::PickItemFromBlock,
            ClientAction::PickItemFromEntity { .. } => Self::PickItemFromEntity,
            ClientAction::SetBeaconEffects { .. } => Self::SetBeaconEffects,
            ClientAction::EditBook { .. } => Self::EditBook,
            ClientAction::SignUpdate { .. } => Self::SignUpdate,
            ClientAction::SetCommandBlock { .. } => Self::SetCommandBlock,
        }
    }
}

/// Adapter implemented by protocol crates to lift packets into this canonical
/// model and lower canonical actions back into packets.
///
/// This trait is the only intended coupling point between a protocol-specific
/// crate and the version-free model:
///
/// - [`VersionAdapter::begin_login`] emits the initial protocol-owned packets
///   required to begin a connection.
/// - [`VersionAdapter::handle_packet`] receives the already-decompressed
///   inbound packet body as raw bytes, plus the version-free connection state
///   and numeric packet id from the protocol layer. It returns an empty vector
///   when a packet is intentionally ignored by the model.
/// - [`VersionAdapter::encode_action`] receives a canonical client action and
///   returns `Ok(None)` only when no packet should be sent. If the protocol
///   cannot faithfully express the requested capability, it should return
///   [`AdapterError::Unsupported`] or [`AdapterError::UnsupportedAction`].
///
/// Directives are executed in returned order, so adapters can request a packet
/// write before a state transition. The trait intentionally does not expose
/// wire codecs, registries, NBT, JSON chat serialization, compression, or
/// protocol crate types, nor does it perform any encryption itself: the
/// encryption *crypto* (key generation, RSA, session auth, cipher state) lives
/// in the driver, and only the protocol-shaped *framing* of the response
/// crosses this seam via [`VersionAdapter::build_encryption_response`]. Those
/// details remain in version adapters.
pub trait VersionAdapter: Send + Sync + std::fmt::Debug {
    /// Returns the adapter's primary protocol number.
    fn protocol_version(&self) -> i32;

    /// Returns human-readable Minecraft release names supported by this adapter.
    fn minecraft_versions(&self) -> &'static [&'static str];

    /// Returns whether this adapter supports `protocol`.
    fn supports(&self, protocol: i32) -> bool;

    /// Returns directives to execute immediately on connect.
    ///
    /// Typical adapters use this to send the handshake and login start packets.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the initial login packets cannot be
    /// encoded.
    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError>;

    /// Handles one inbound protocol packet and returns connection directives.
    ///
    /// `packet_id` is protocol-specific and must not escape this boundary.
    /// Inbound packets are implicitly [`Bound::Client`]; [`Bound`] remains
    /// public for adapter-side packet id tables.
    ///
    /// `world` is the client-owned world write sink. Packets that carry world
    /// data (chunks, and later block updates) apply it here directly, so the
    /// heavy decoded state never travels the bounded event channel; the adapter
    /// then emits only a lightweight [`ClientEvent::ChunkLoaded`] notification.
    /// Packets that do not touch world state simply ignore it.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the packet context or bytes cannot be
    /// handled by the adapter.
    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError>;

    /// Encodes one canonical action into a protocol packet id and payload.
    ///
    /// The returned packet id is protocol-specific and must be interpreted only
    /// by the calling protocol layer.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the action cannot be represented in the
    /// given state. Capability gaps in older protocols should be reported here
    /// instead of being hidden behind lossy defaults.
    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError>;

    /// Frames the serverbound encryption-response packet for this protocol.
    ///
    /// Called by the driver during the online-mode handshake after it has
    /// generated the shared secret and RSA-encrypted both it and the verify
    /// token (see [`Directive::BeginEncryption`]). The adapter owns only the
    /// protocol-specific packet id and byte-array framing, which differs across
    /// versions (pre-1.19 shapes carry an optional salt/signature) — so this
    /// deliberately does not live in shared code. Both arguments are already
    /// ciphertext; no crypto happens here.
    ///
    /// The default returns [`AdapterError::Unsupported`]: a version that has not
    /// implemented online-mode encryption simply never emits
    /// [`Directive::BeginEncryption`], so this is never reached for it. Versions
    /// that do must override it with their version-specific framing.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the response cannot be framed, or
    /// [`AdapterError::Unsupported`] when the version does not implement
    /// encryption.
    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        let _ = (encrypted_secret, encrypted_token);
        Err(AdapterError::Unsupported(
            "online-mode encryption is not implemented for this protocol version".to_owned(),
        ))
    }
}
