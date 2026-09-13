//! Version-free encoding and decoding contracts for hosted protocols.
//!
//! Implementations supply packet ids and bytes; this module owns the shared trait defaults,
//! chunk encoding contract, and boxed forwarding required by dynamic protocol selection.

use lodestone_core::State;
use lodestone_model::command_tree::{CommandSuggestionsResponse, CommandTree};
use lodestone_model::{
    BlockPos, Difficulty, EntityAttributeSnapshot, GameMode, ItemStack, ResourceKey, SoundCategory,
    Text, Vec3, Vec3f,
};
use uuid::Uuid;

use crate::chunk::{ChunkColumn, ColumnLightSettlement};
use crate::dimension::Dimension;

use super::{
    Abilities, EntitySnapshot, MerchantOfferOut, MetadataField, PlayerListing, ResourcePackPush,
    ServerBound, ServerDirective,
};

/// Which worldgen data bundle a [`ServerProtocol`]'s hosting needs — the
/// version gate between the worldgen data this crate embeds and
/// the protocol family being served.
///
/// The only bundle `lodestone-server` embeds is 26.2 (protocol 776): the
/// `assets/worldgen/` table [`crate::worldgen_data`] serves. A family whose
/// worldgen is **not** the embedded 26.2 bundle must say so and supply its own
/// data — per `docs/plans/worldgen-parity.md` §4 that will be a second engine
/// behind [`crate::ChunkSource`], not a second JSON bundle. The
/// [`None`](Self::None) report is what makes "no worldgen for this version"
/// surfaced rather than silently serving the wrong terrain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldgenScope {
    /// 26.2 (protocol 776) worldgen data — the one bundle this crate embeds.
    V26_2,
    /// No worldgen data: the protocol does not host world generation, or has
    /// not declared a bundle this crate can serve.
    None,
}

/// An owned failure from encoding one terrain column.
///
/// This error deliberately contains no transport or connection state: chunk
/// workers create it before a connection task chooses how to finish an open
/// batch, send a disconnect, and return the failure to its caller.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ChunkEncodeError {
    message: String,
}

impl ChunkEncodeError {
    /// Builds an encoding error with owned diagnostic text.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The diagnostic text supplied by the encoder.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

pub trait ChunkEncoder: Send + Sync + 'static {
    /// Encodes one terrain column into a client-bound packet — byte-identical to
    /// [`ServerProtocol::encode_chunk`] for the same arguments.
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective;

    /// Fallible counterpart of [`encode_chunk`](Self::encode_chunk).
    ///
    /// The default preserves every existing encoder's successful bytes. An
    /// encoder that can reject a column overrides this method and returns an
    /// owned [`ChunkEncodeError`] without needing access to a connection.
    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(self.encode_chunk(cx, cz, column))
    }

    /// Dimension-aware chunk encoding. The default retains the historical
    /// overworld-only contract for encoders that do not derive light.
    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        _dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        self.try_encode_chunk(cx, cz, column)
    }

}

impl<E: ChunkEncoder + ?Sized> ChunkEncoder for Box<E> {
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        (**self).encode_chunk(cx, cz, column)
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        (**self).try_encode_chunk(cx, cz, column)
    }

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        (**self).try_encode_chunk_in_dimension(cx, cz, column, dimension)
    }

}

pub trait ServerProtocol: Send + Sync {
    /// Lifts one inbound (server-bound) packet into [`ServerBound`].
    ///
    /// `packet_id` is protocol-specific and must not escape the implementor.
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound;

    /// Emits the login-success reply for a freshly-presented username/uuid.
    /// For protocols with a Configuration phase, the client's own
    /// acknowledgement (lifted to [`ServerBound::LoginAcknowledged`]) drives
    /// the transition to [`State::Configuration`]; legacy protocols use
    /// [`has_configuration_phase`](Self::has_configuration_phase) to transition
    /// directly to Play after these directives.
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective>;

    /// Whether login success is followed by a Configuration phase before Play.
    ///
    /// Modern protocols return `true` (the default) and wait for the client's
    /// login/configuration acknowledgements. Older protocols transition directly
    /// to Play after [`login_success`](Self::login_success), because their wire
    /// has no packets for those acknowledgements.
    fn has_configuration_phase(&self) -> bool {
        true
    }

    /// Emits the online-mode encryption request, mirroring
    /// `ClientboundHelloPacket`: an empty server-id string, the DER-encoded
    /// RSA public key, the verify-token challenge, and a fixed
    /// `should_authenticate = true` (vanilla only ever constructs this packet
    /// with `true` — there is no wire concept of "encrypt without also
    /// verifying with the session server").
    ///
    /// Pure encode, no crypto and no I/O: the caller generates the keypair and
    /// verify token and owns decrypting whatever `EncryptionResponse` comes
    /// back. The default returns [`ServerDirective::None`], which the
    /// connection loop reads as "this protocol has no online-mode wire
    /// support" and falls back to an offline login rather than sending a
    /// request no decoder on the other end would recognise.
    fn encode_encryption_request(
        &self,
        public_key_der: &[u8],
        verify_token: &[u8],
    ) -> ServerDirective {
        let _ = (public_key_der, verify_token);
        ServerDirective::None
    }

    /// Emits the directives sent once the connection has moved into
    /// [`State::Configuration`] (in reply to
    /// [`ServerBound::LoginAcknowledged`]), ending with whatever finishes the
    /// configuration phase from the server's side.
    fn begin_configuration(&self) -> Vec<ServerDirective>;

    /// Emits the Configuration-phase `registry_data` packets — one per
    /// synchronized registry, so the client can resolve the bare holder ids
    /// later packets carry (`login`'s `dimension_type` index, `set_time`'s
    /// `world_clock` keys). Sent by the server loop **before**
    /// [`begin_configuration`](ServerProtocol::begin_configuration)'s finish
    /// signal; a real client expects the registries to precede
    /// `FINISH_CONFIGURATION`.
    ///
    /// The default emits nothing. A protocol that does not host (every legacy
    /// family — only a family with a `ServerProtocol` implementation hosts),
    /// or a host that has no registry data to declare yet, sends no packets
    /// and behaves exactly as it did before this method existed — the same
    /// additive, version-free seam [`encode_status_response`](ServerProtocol::encode_status_response)
    /// established. This is deliberately a separate call rather than a
    /// prefix inside [`begin_configuration`](ServerProtocol::begin_configuration):
    /// routing the registry stream through its own method makes "registries
    /// before finish" a version-free invariant of the choreography in
    /// `crate::server`'s `serve_connection_inner`, instead of something each
    /// implementor must remember inside its own `begin_configuration`.
    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    /// Encodes the server-list status reply to a [`ServerBound::StatusRequest`]
    /// (vanilla `ClientboundStatusResponsePacket`, whose whole body is one
    /// length-prefixed JSON document — vanilla's own lenient-JSON byte-buf codec, capped at 32767).
    ///
    /// The parameters are deliberately scalars rather than a struct: everything
    /// here is version-free, but the two fields vanilla's own `ServerStatus`
    /// also carries — `version.name` and `version.protocol` — are *not*, so the
    /// implementor fills those from its own protocol number, exactly as
    /// vanilla's own status-version constructor does. This crate must never name a
    /// protocol number, so it cannot pass them in.
    ///
    /// * `description` — the MOTD, serialized as a text component.
    /// * `players_online` / `players_max` — vanilla's `players.online` /
    ///   `players.max`.
    /// * `sample` — `players.sample`, a list of `(uuid, name)` pairs
    ///   (vanilla's own name-and-id record). Empty is legal
    ///   and is what vanilla sends when the sample is disabled.
    /// * `favicon_png` — raw PNG bytes, which the implementor base64-encodes
    ///   behind vanilla's mandatory `data:image/png;base64,` prefix.
    ///   `None` omits the field entirely.
    /// * `enforces_secure_chat` — vanilla's `enforcesSecureChat`, default
    ///   `false`.
    ///
    /// The default emits nothing, so a protocol with no status support behaves
    /// exactly as it did before this method existed.
    fn encode_status_response(
        &self,
        description: &str,
        players_online: i32,
        players_max: i32,
        sample: &[(Uuid, String)],
        favicon_png: Option<&[u8]>,
        enforces_secure_chat: bool,
    ) -> ServerDirective {
        let _ = (
            description,
            players_online,
            players_max,
            sample,
            favicon_png,
            enforces_secure_chat,
        );
        ServerDirective::None
    }

    /// Encodes a disconnect packet carrying `reason`, for the phase the
    /// connection is currently in.
    ///
    /// **The packet is phase-specific in both id *and* encoding**, which is the
    /// one thing to get right here:
    ///
    /// | phase | vanilla packet | reason encoded as |
    /// |---|---|---|
    /// | Login | `ClientboundLoginDisconnectPacket` | **JSON string** (vanilla's own lenient-JSON byte-buf codec, capped at 262144) |
    /// | Configuration | `ClientboundDisconnectPacket` | **NBT** (vanilla's own `TRUSTED_CONTEXT_FREE_STREAM_CODEC`) |
    /// | Play | `ClientboundDisconnectPacket` | **NBT**, same codec |
    ///
    /// Login is the odd one out for historical reasons — the login phase predates
    /// NBT components on the wire — and an implementor that writes NBT there
    /// produces a packet a real client cannot parse. `Status` has no disconnect
    /// packet at all in 26.2 (its clientbound set is `status_response` and
    /// `pong_response` only), so vanilla just closes the channel there; an
    /// implementor should return [`ServerDirective::None`] for it rather than
    /// inventing an id.
    ///
    /// Sending this does **not** close the connection — the caller does that,
    /// after the write, exactly as vanilla's `Connection::disconnect` flushes the
    /// packet before closing.
    ///
    /// The default emits nothing, so a protocol without disconnect support closes
    /// silently, which is how every family behaved before this method existed.
    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        let _ = (state, reason);
        ServerDirective::None
    }

    /// Encodes the reply to a [`ServerBound::PingRequest`] (vanilla
    /// `ClientboundPongResponsePacket`: the same single big-endian `long`,
    /// echoed unchanged).
    ///
    /// The default emits nothing.
    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        let _ = time;
        ServerDirective::None
    }

    /// Encodes one line of server-originated chat to the calling client
    /// (vanilla `ClientboundSystemChatPacket`: a text component plus an
    /// `overlay` flag, where `false` selects the normal chat history and
    /// `true` the action bar).
    ///
    /// This is command feedback's only route back to the player — a refusal and
    /// a success are both delivered through it, which is
    /// why the failure to implement it is silent rather than loud: the command
    /// still *runs*, the player just never learns what happened. A family that
    /// wants commands must implement this.
    ///
    /// `message` is plain text, not a serialized component: this crate must
    /// never name a wire format, so the implementor wraps it. The default
    /// emits nothing, matching every other optional encoder here.
    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        let _ = message;
        ServerDirective::None
    }

    /// Encodes a server→client plugin-message payload (vanilla
    /// `ClientboundCustomPayloadPacket`, wire id `custom_payload`).
    /// `channel` is the namespaced channel identifier; `data` is the
    /// channel-specific raw bytes, written verbatim — the same two-field shape
    /// [`ServerBound::CustomPayload`] lifts on the inbound side.
    ///
    /// This is the one wire-level route a server-initiated payload takes to a
    /// connected client. The default emits nothing, so a protocol without
    /// plugin-message support need not override it — the same convention as
    /// every other optional encoder here.
    fn encode_custom_payload(&self, channel: &ResourceKey, data: &[u8]) -> ServerDirective {
        let _ = (channel, data);
        ServerDirective::None
    }

    /// Emits the join sequence once the connection has moved into
    /// [`State::Play`] (in reply to [`ServerBound::ConfigurationFinished`]):
    /// the join-game packet, default spawn position, initial teleport, and
    /// chunk-cache center. Does not send any chunks; the loop calls
    /// [`begin_chunk_batch`](Self::begin_chunk_batch)/
    /// [`encode_chunk`](Self::encode_chunk)/
    /// [`end_chunk_batch`](Self::end_chunk_batch) separately so it can drive
    /// the view radius itself.
    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective>;

    /// Like [`begin_play`](Self::begin_play), but derives the spawn teleport
    /// and default-spawn-position coordinates from `spawn` (world-space, feet
    /// position) rather than from hardcoded version-specific literals.
    /// Spawn Y is terrain-derived, and the server computes it; the
    /// protocol only needs to encode it. The chunk-cache center is also
    /// derived from `spawn` rather than assumed to be `(0, 0)`.
    ///
    /// The default delegates to [`begin_play`](Self::begin_play), so a family
    /// that has not adopted terrain-derived spawn yet keeps its existing
    /// hardcoded join behaviour unchanged.
    fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
        let _ = (spawn, mode);
        self.begin_play(view_radius)
    }

    /// Whether this family attaches an acknowledgement id to clientbound player
    /// position packets. The server only waits for confirmations when this is
    /// true, so older families retain their existing movement contract.
    fn uses_teleport_acknowledgements(&self) -> bool {
        false
    }

    /// Like [`begin_play_at`](Self::begin_play_at), but gives an
    /// acknowledgement-capable family the id its initial position packet must
    /// carry. The default deliberately retains the legacy join sequence.
    fn begin_play_at_with_teleport_id(
        &self,
        view_radius: i32,
        spawn: Vec3,
        mode: GameMode,
        teleport_id: i32,
    ) -> Vec<ServerDirective> {
        let _ = teleport_id;
        self.begin_play_at(view_radius, spawn, mode)
    }

    /// Encodes a game-mode change for the local player (vanilla
    /// `ClientboundGameEventPacket` with `CHANGE_GAME_MODE`, whose float
    /// parameter is the `GameType` id).
    ///
    /// This is *only* the mode; the abilities it implies travel in
    /// [`encode_player_abilities`](Self::encode_player_abilities), exactly as
    /// vanilla sends two packets from its own set-game-mode routine. The default
    /// emits nothing.
    fn encode_game_mode(&self, mode: GameMode) -> ServerDirective {
        let _ = mode;
        ServerDirective::None
    }

    /// Encodes the local player's movement abilities (vanilla
    /// `ClientboundPlayerAbilitiesPacket`) — what actually grants creative
    /// flight and instant build on the client.
    ///
    /// Sent at join and on every game-mode change. Without it a client told it
    /// is in creative still cannot fly, because flight permission lives in this
    /// packet and not in the mode. The default emits nothing.
    fn encode_player_abilities(&self, abilities: Abilities) -> ServerDirective {
        let _ = abilities;
        ServerDirective::None
    }

    /// Marks the start of a chunk batch (vanilla's `CHUNK_BATCH_START`, an
    /// empty body in every known protocol).
    fn begin_chunk_batch(&self) -> ServerDirective;

    /// Encodes one terrain column into a client-bound packet.
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective;

    /// Fallible counterpart of [`encode_chunk`](Self::encode_chunk).
    ///
    /// The default preserves the legacy, infallible implementation. An
    /// implementation that can reject a column returns an owned error so the
    /// server can close a previously written chunk batch before disconnecting.
    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(self.encode_chunk(cx, cz, column))
    }

    /// Dimension-aware one-column initial encoding and full-column fallback.
    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        _dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        self.try_encode_chunk(cx, cz, column)
    }

    /// Encodes one terrain column with its eight adjacent columns available.
    ///
    /// Initial chunk batches use this when a protocol includes light derived
    /// across a column border. The default keeps every one-column encoder's
    /// previous output.
    fn try_encode_chunk_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        _neighbours: &[(i32, i32, ChunkColumn)],
    ) -> Result<ServerDirective, ChunkEncodeError> {
        self.try_encode_chunk(cx, cz, column)
    }

    /// Dimension-aware neighbour-bearing initial chunk encoding.
    fn try_encode_chunk_with_neighbours_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        _dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        self.try_encode_chunk_with_neighbours(cx, cz, column, neighbours)
    }

    /// Computes the exact light snapshot to retain for a fresh initial chunk.
    ///
    /// A source calls this only after the centre and every contributing
    /// neighbour have been admitted and the source's light fence has completed.
    /// The default keeps existing protocol families on their ordinary light
    /// computation; a family with a distinct initial wire representation may
    /// override it. The resulting snapshot is stored on the resident column
    /// and is later consumed verbatim by the initial chunk encoder.
    fn compute_initial_column_light_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        self.compute_column_light_with_neighbours_in_dimension(column, neighbours, dimension)
    }

    /// Computes every retained light snapshot produced by one admitted
    /// footprint. The default wraps the historical centre-only result, keeping
    /// existing protocol families on their one-column settlement path until a
    /// version adapter opts into [`ColumnLightSettlement`].
    fn compute_initial_column_lights_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: Dimension,
    ) -> Option<ColumnLightSettlement> {
        self.compute_initial_column_light_with_neighbours_in_dimension(
            column,
            neighbours,
            dimension,
        )
        .map(ColumnLightSettlement::centre)
    }

    /// The same encoder as [`encode_chunk`](Self::encode_chunk), detached from
    /// `&self` so it can be **moved into the blocking worker that generated the
    /// column** — see [`ChunkEncoder`] for the measurement that made this
    /// necessary and `docs/server-chunk-encode-offload.md` for the shape.
    ///
    /// The default returns `None`, which means "no off-task encoder": every
    /// caller then falls back to calling [`encode_chunk`](Self::encode_chunk) on
    /// its own task, which is exactly what every caller did before this method
    /// existed. So a family that has not adopted it — and every test protocol in
    /// this workspace — keeps byte-identical behaviour.
    ///
    /// # The one invariant an implementor owes
    ///
    /// The returned encoder must produce **byte-identical** output to
    /// [`encode_chunk`](Self::encode_chunk) for the same arguments. The only
    /// safe way to guarantee that is to have one body and make one call the
    /// other; `V770ServerProtocol` implements [`ChunkEncoder`] and its
    /// `encode_chunk` delegates to it, so there is a single implementation and
    /// nothing to keep in sync.
    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn ChunkEncoder>> {
        None
    }

    /// Marks the end of a chunk batch of `batch_size` columns (vanilla's
    /// `CHUNK_BATCH_FINISHED`).
    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective;

    /// Encodes a **light-only** update for one column (vanilla's
    /// `ClientboundLightUpdatePacket`, wire id `light_update`) — the packet that
    /// makes a placed torch light its column without re-sending the terrain.
    ///
    /// This is the ninth of nine links in the "torches emit no light" chain, and
    /// it was the only missing one: the client decode, `LightPatch`'s three-state
    /// merge and the re-mesh signal all already existed. See [`crate::light`] for
    /// the audit and `docs/server-light.md` for the wire format.
    ///
    /// # The wire order is not [`ColumnLight`]'s argument order
    ///
    /// The body is `cx`, `cz`, then **exactly** what
    /// [`lodestone_world::ColumnLight::encode`] writes: four section bitsets in
    /// the order sky / block / empty-sky / empty-block, then the two array lists.
    /// That is *not* the order `LightPatch::from_light_masks` takes its arguments
    /// in (it interleaves each layer's mask with its empty mask), and an
    /// implementor that follows the constructor instead produces a packet a real
    /// client mis-merges silently. `ColumnLight::encode` is already the exact
    /// `ClientboundLightUpdatePacketData` shape, so an implementor should call it
    /// rather than reimplement the four bitsets.
    ///
    /// The default emits nothing, so a family without light support falls back to
    /// the whole-column resend [`crate::light`] describes.
    fn encode_light_update(
        &self,
        cx: i32,
        cz: i32,
        light: &lodestone_world::ColumnLight,
    ) -> ServerDirective {
        let _ = (cx, cz, light);
        ServerDirective::None
    }

    /// Computes the light for one column, so
    /// [`encode_light_update`](Self::encode_light_update) has something to send.
    ///
    /// This is version-specific and this crate cannot do it: the light engine
    /// runs over `lodestone_world`'s **state-id** column, and resolving a
    /// canonical state string to a registry id is exactly the seam
    /// [`encode_chunk`](Self::encode_chunk) crosses. So the implementor converts
    /// and floods, and the server only decides *when* to ask.
    ///
    /// The default result is an **isolated** compute. Families that opt into
    /// [`compute_column_light_with_neighbours`](Self::compute_column_light_with_neighbours)
    /// receive the loaded 3×3 neighbourhood instead, which supplies every cell
    /// that can contribute across the centre column's border. This remains a
    /// transient compute: no column carries a light cache that block writes could
    /// leave stale.
    ///
    /// The default answers `None`, which the server reads as "this family cannot
    /// compute light", and it falls back to the column resend.
    fn compute_column_light(&self, column: &ChunkColumn) -> Option<lodestone_world::ColumnLight> {
        let _ = column;
        None
    }

    /// Dimension-aware isolated light computation for a later light update.
    fn compute_column_light_in_dimension(
        &self,
        column: &ChunkColumn,
        _dimension: Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        self.compute_column_light(column)
    }

    /// Whether this family uses the loaded 3×3 chunk neighbourhood when it
    /// computes light. The default preserves the isolated light compute for
    /// families that have not adopted cross-column propagation.
    fn uses_cross_column_light(&self) -> bool {
        false
    }

    /// Whether the server should settle and retain the exact light snapshot
    /// consumed by this family's initial chunk packet. Legacy families leave
    /// this disabled because they do not consume the retained representation;
    /// an opting-in family must make its initial encoder prefer the column's
    /// retained snapshot over a fresh reconstruction.
    fn retains_initial_column_light(&self) -> bool {
        false
    }

    /// Computes light for `column` with the eight adjacent columns available.
    /// Each tuple is a chunk-relative `(dx, dz)` in `-1..=1`, excluding
    /// `(0, 0)`. This is called only when [`uses_cross_column_light`](Self::uses_cross_column_light)
    /// is true; the default delegates to the isolated compute so an implementor
    /// cannot accidentally claim the capability without producing light.
    fn compute_column_light_with_neighbours(
        &self,
        column: &ChunkColumn,
        _neighbours: &[(i32, i32, ChunkColumn)],
    ) -> Option<lodestone_world::ColumnLight> {
        self.compute_column_light(column)
    }

    /// Dimension-aware neighbour-bearing light computation for a later update.
    fn compute_column_light_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        _dimension: Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        self.compute_column_light_with_neighbours(column, neighbours)
    }

    /// Emits any directives to send right after the initial chunk batch has
    /// gone out (a post-join system chat message, say). Optional: the default
    /// sends nothing, so an implementor that has no such content need not
    /// override it.
    fn welcome_message(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    /// Encodes an entity's initial appearance for a client that has not seen it
    /// (vanilla `ADD_ENTITY`, plus any immediate follow-up the protocol bundles).
    /// The default emits nothing, so a protocol without entity support need not
    /// override it.
    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        let _ = entity;
        ServerDirective::None
    }

    /// Encodes a per-tick update for an entity the client already tracks, given
    /// the previously-sent snapshot (`None` before the first update was sent) so
    /// the protocol can choose an absolute or relative encoding without holding
    /// any per-connection state itself. The default emits nothing.
    fn encode_entity_update(
        &self,
        prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        let _ = (prev, current);
        Vec::new()
    }

    /// Encodes the removal of a batch of entities in one packet (vanilla
    /// `REMOVE_ENTITIES`, a count-prefixed id list). The default emits nothing.
    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        let _ = ids;
        ServerDirective::None
    }

    /// Encodes vanilla's `TAKE_ITEM_ENTITY` — the **pickup animation**: the item
    /// entity arcs toward the collector and shrinks. The default emits nothing.
    ///
    /// # This is an animation cue, not the pickup itself
    ///
    /// The inventory write and the entity's removal are separate and already
    /// happen. This packet exists only so the client can *show* the take, and the
    /// client deliberately keeps the item entity alive to interpolate it, removing
    /// it when the animation finishes (vanilla's own client-side take-item-entity handler;
    /// our own `lodestone-shell`'s `entities.rs` carries the matching lerp).
    ///
    /// **So the ordering is load-bearing and it is easy to get wrong in a way that
    /// looks fixed.** Vanilla's own item-entity player-touch routine calls `player.take(this,
    /// orgCount)` and only *then* `this.discard()`. A server that removes the entity
    /// first — or emits `REMOVE_ENTITIES` in the same pass, before this — leaves the
    /// client with nothing to interpolate and produces no animation at all, with the
    /// packet present and correct on the wire.
    ///
    /// `amount` is the item entity's stack count **before** the inventory took any
    /// of it — vanilla passes `orgCount`, captured ahead of
    /// `player.getInventory().add(itemStack)`, which shrinks the stack in place. It
    /// is *not* the amount that actually fitted, and the two differ exactly when a
    /// pickup is partial. It drives the client's pickup sound pitch, so a hardcoded
    /// `1` is audible rather than merely wrong.
    fn encode_take_item_entity(
        &self,
        item_entity_id: i32,
        collector_entity_id: i32,
        amount: i32,
    ) -> ServerDirective {
        let _ = (item_entity_id, collector_entity_id, amount);
        ServerDirective::None
    }

    /// Encodes the Brigadier command tree (vanilla `ClientboundCommandsPacket`,
    /// wire id `commands`) — the packet that makes tab completion and command
    /// syntax highlighting possible at all.
    ///
    /// `tree` is already the **per-player** projection: pruned to what this
    /// connection's permission level may see, with every child and redirect index
    /// renumbered against the pruned node list
    /// (`crate::commands::wire::project_filtered`). An implementation writes the
    /// nodes out and must not re-derive an index, because the only thing that
    /// makes a flat index graph self-consistent is that one walk assigned all of
    /// it.
    ///
    /// # The default is silence, and that is the right default
    ///
    /// A protocol family with no override sends nothing, and the client simply
    /// has no tree — which is exactly the state every family was in before this
    /// existed. The failure mode of the alternative (a required method) would be
    /// a legacy family forced to grow an encoder for a packet whose id it may
    /// number differently.
    /// Encodes vanilla's `ClientboundHurtAnimationPacket` — **the camera damage
    /// tilt**, and the red hurt flash on a remote entity.
    ///
    /// # Where vanilla sends it
    ///
    /// vanilla's own deal-default-knockback routine calls `indicateDamage(xd, zd)` with the
    /// horizontal offset from the damage source to the victim, and only
    /// `ServerPlayer` overrides it — the base entity's own indicate-damage routine is empty, and
    /// its own get-hurt-dir routine is a constant `0.0F`. So in vanilla this packet
    /// goes to **one** connection, the hurt player's own, and never for a mob.
    ///
    /// `yaw` is vanilla's own per-player indicate-damage routine's own expression,
    /// `atan2(zd, xd) * 180 / PI - yRot` — degrees, in the victim's frame, so a hit
    /// from straight ahead is `0`. See [`crate::vitals::hurt_dir_degrees`], which
    /// is that formula and the one place it should be computed.
    ///
    /// # The one deliberate widening, and why the screen needs it
    ///
    /// `dealDefaultKnockback` runs only for a source **outside**
    /// `#minecraft:damage_type/no_knockback`, and that tag holds `fall`, `drown`,
    /// `starve`, `lava`, `in_fire`, `cactus`, `freeze`, `magic` — i.e. very nearly
    /// every way a singleplayer world currently hurts anyone. Vanilla still tilts
    /// the camera for those, because `ClientboundDamageEventPacket` also sets
    /// `hurtTime`, and this crate encodes no `damage_event`. This crate therefore
    /// sends `hurt_animation` for a directionless hit too, with `yaw` **exactly
    /// `0.0`** — the pure-roll case, which is what a vanilla client shows for a
    /// player who has not been knocked back since spawning. It is a substitution
    /// on the *route*, not on the pixels; encoding `damage_event` (which needs a
    /// `minecraft:damage_type` registry id per source) is the follow-up that makes
    /// the route vanilla's as well.
    ///
    /// The default emits nothing, so a protocol family without hurt-animation
    /// support need not override it and the tilt simply never fires there.
    fn encode_hurt_animation(&self, entity_id: i32, yaw: f32) -> ServerDirective {
        let _ = (entity_id, yaw);
        ServerDirective::None
    }

    /// Encodes vanilla's `ClientboundEntityEventPacket` — one raw per-entity-type
    /// status byte, vanilla's own level broadcast-entity-event routine's whole payload.
    ///
    /// `event` is vanilla's own `EntityEvent` constant, and the
    /// values are **not** a registry: they are reused across entity types, so the
    /// same byte means different things on different species. The ones this crate
    /// sends today, read off `EntityEvent`:
    ///
    /// | byte | constant | meaning |
    /// |---|---|---|
    /// | 3 | `DEATH` | vanilla's own entity-die routine's broadcast — the fall-over animation |
    /// | 6 | `TAMING_FAILED` | smoke puff |
    /// | 7 | `TAMING_SUCCEEDED` | hearts |
    /// | 18 | `IN_LOVE_HEARTS` | breeding hearts |
    ///
    /// **Note the wire shape**: the entity id is a plain big-endian `int`, *not* a
    /// VarInt — vanilla's own entity-event-packet writer is `writeInt` then
    /// `writeByte`, one of the few remaining fixed-width ids in play. Porting from
    /// the field list rather than from `write` would give the same two fields in
    /// the same order at the wrong widths, which desynchronises the stream instead
    /// of merely mis-animating.
    ///
    /// The default emits nothing, so a protocol family without entity-event
    /// support need not override it and a dying mob simply pops out of existence.
    fn encode_entity_event(&self, entity_id: i32, event: u8) -> ServerDirective {
        let _ = (entity_id, event);
        ServerDirective::None
    }

    fn encode_commands(&self, tree: &CommandTree) -> ServerDirective {
        let _ = tree;
        ServerDirective::None
    }

    /// Encodes vanilla's `ClientboundCommandSuggestionsPacket` (id 15) — the
    /// answer to a `ServerboundCommandSuggestionPacket` (`ServerBound::CommandSuggestion`),
    /// vanilla's own custom-command-suggestions handler's reply.
    ///
    /// `response.id` echoes the request's transaction id verbatim; `start`/`length`
    /// name the byte range of the *request's* command text the suggestions
    /// replace. `crate::commands::ServerCommands::suggest` is the source of the
    /// candidate strings — see `crate::server`'s `ServerBound::CommandSuggestion`
    /// consumer for how the range is derived from the request.
    ///
    /// The default emits nothing, so a protocol family without server-side
    /// tab-completion need not override it and a client typing `/` there simply
    /// gets no suggestions, exactly as it gets no `COMMANDS` tree either.
    fn encode_command_suggestions(&self, response: &CommandSuggestionsResponse) -> ServerDirective {
        let _ = response;
        ServerDirective::None
    }

    /// Encodes vanilla's `ClientboundSetPassengersPacket` — the packet that makes
    /// a player *be* in a boat.
    ///
    /// vanilla's own server-entity send-pairing-data routine sends it on spawn and
    /// its own `startRiding`/`stopRiding` re-send it on every change, always as the
    /// vehicle's **whole** passenger list rather than a delta: dismounting is this
    /// packet with an empty list, which is why `passenger_ids` is a slice and not an
    /// `Option`.
    ///
    /// # This is the only channel, and without it riding cannot exist
    ///
    /// A client learns it is a passenger from nothing else. Our own shell folds it
    /// into `session::Riding` (`ClientEvent::EntityPassengersChanged`), and
    /// `lodestone_ecs::vehicle::tick_controlled_vehicle` reads that scalar to decide
    /// which vehicle to simulate — so with no producer here the whole
    /// client-authoritative boat pipeline is unreachable, however complete it is.
    /// That was the state of this tree before boats were placeable: `SET_PASSENGERS`
    /// was decoded by the v770 adapter, routed by `ingest`, consumed by the seat
    /// pin, and **emitted by nobody**.
    ///
    /// The wire shape is a VarInt vehicle id then `writeVarIntArray` — a VarInt
    /// length followed by that many VarInts, *not* the generic collection codec.
    ///
    /// The default emits nothing, so a protocol family with no passenger support
    /// need not override it and riding simply never engages there.
    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        let _ = (vehicle_id, passenger_ids);
        ServerDirective::None
    }

    /// Encodes `SET_ENTITY_LINK` — vanilla `ClientboundSetEntityLinkPacket`,
    /// which draws the rope between a leashed mob and its holder.
    /// `source_id` is the leashed entity; `target_id` is `None` for a detach
    /// (vanilla's own sentinel: `write` sends the holder's id or `0` when there
    /// is none, vanilla's own leashable drop-leash/remove-leash routines'
    /// `new ClientboundSetEntityLinkPacket(entity, null)`) and `Some` for an
    /// attach, carrying the holder's own wire entity id (a player or another
    /// leashed mob — see [`EntitySnapshot::leash_link`] for how each resolves).
    ///
    /// [`crate::server::EntityStreamer::sync`] is the one caller, and calls this
    /// exactly like [`encode_set_entity_data`](Self::encode_set_entity_data):
    /// once on spawn when [`EntitySnapshot::leash_link`] is `Some` — which is
    /// what puts a rope on a mob a client only just entered view range of,
    /// mirroring `ServerEntity`'s own pairing-time
    /// `sendToTrackingPlayers(entity, new ClientboundSetEntityLinkPacket(entity,
    /// leashable.getLeashHolder()))` — and again on any update where the field
    /// changed, covering both a fresh attach and a detach.
    ///
    /// The default emits nothing, so a protocol family with no leash support
    /// need not override it and a leashed mob simply draws no rope there.
    fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
        let _ = (source_id, target_id);
        ServerDirective::None
    }

    /// Encodes a `SET_ENTITY_DATA` metadata update for an arbitrary entity id,
    /// given every [`MetadataField`] that entity currently wants
    /// synced — not a hardcoded single field for a hardcoded entity id, the
    /// shape [`encode_air_supply_update`](Self::encode_air_supply_update) is
    /// stuck in for exactly that reason (`LOCAL_PLAYER_ENTITY_ID` only, one
    /// `INT` field only). [`crate::server::EntityStreamer::sync`] is the one
    /// caller: it calls this whenever an entity spawns with non-empty
    /// [`EntitySnapshot::metadata`], or an update changes it, passing the
    /// entity's *current* full field list each time. The default emits
    /// nothing, so a protocol without per-species metadata support need not
    /// override it and a swelling creeper simply never reaches that client's
    /// screen.
    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        let _ = (entity_id, fields);
        ServerDirective::None
    }

    /// Encodes a `BOSS_EVENT` `ADD` operation — vanilla's own
    /// boss-event-packet add-packet constructor.
    /// `id` is the bar's own id (see [`BossBarSnapshot::id`]), `name` its title,
    /// `progress` its `[0.0, 1.0]` fill.
    ///
    /// [`crate::server::EntityStreamer`] is the one caller, sending this the
    /// first time a [`BossBarSnapshot`] with `visible: true` is seen for a
    /// given id — the wire has no "visible" flag of its own; see
    /// [`BossBarSnapshot`]'s doc for why visibility is spelled as add/remove.
    ///
    /// The default emits nothing, so a protocol family with no boss-bar
    /// support need not override it and a dragon fight simply shows no bar
    /// there.
    fn encode_boss_event_add(&self, id: Uuid, name: &Text, progress: f32) -> ServerDirective {
        let _ = (id, name, progress);
        ServerDirective::None
    }

    /// Encodes a `BOSS_EVENT` `UPDATE_PROGRESS` operation — vanilla's own
    /// boss-event-packet update-progress-packet constructor. Sent on every
    /// streaming pass where a previously-added bar's `progress` changed and it
    /// is still `visible`; see [`encode_boss_event_add`](Self::encode_boss_event_add).
    fn encode_boss_event_update_progress(&self, id: Uuid, progress: f32) -> ServerDirective {
        let _ = (id, progress);
        ServerDirective::None
    }

    /// Encodes a `BOSS_EVENT` `REMOVE` operation — vanilla's own
    /// boss-event-packet remove-packet constructor. Sent once a
    /// previously-added bar's [`BossBarSnapshot::visible`] goes `false`, or the
    /// id vanishes from the source entirely (the dragon itself despawned);
    /// see [`encode_boss_event_add`](Self::encode_boss_event_add).
    fn encode_boss_event_remove(&self, id: Uuid) -> ServerDirective {
        let _ = id;
        ServerDirective::None
    }

    /// Encodes the tab-list additions for players this connection has not been
    /// told about yet (vanilla `ClientboundPlayerInfoUpdatePacket`
    /// with the `ADD_PLAYER` action, wire id `player_info_update`).
    ///
    /// **This is not cosmetic, and it is not optional for player entities.** A
    /// real client *drops* an `ADD_ENTITY` whose type is `minecraft:player`
    /// when it holds no `PlayerInfo` for that uuid:
    /// vanilla's own client-side create-entity-from-packet routine logs
    /// `"Server attempted to add player prior to sending player info"` and
    /// returns `null`, so the entity is never added to the level
    /// (the real client's own packet-listener logs this and bails).
    /// [`crate::players::PlayerListStreamer`]
    /// is the one caller and `crate::server`'s streaming pass emits its
    /// directives **before** the entity diff for exactly that reason.
    ///
    /// The default emits nothing, like every other encoder here, so a protocol
    /// with no tab-list support need not override it — at the cost that a
    /// player entity will not reach that version's clients at all, which is
    /// the honest consequence rather than a half-sent spawn.
    fn encode_player_info_add(&self, players: &[PlayerListing]) -> Vec<ServerDirective> {
        let _ = players;
        Vec::new()
    }

    /// Encodes the tab-list removals for players that have left (vanilla
    /// vanilla `ClientboundPlayerInfoRemovePacket`, wire id
    /// `player_info_remove`) — the counterpart to
    /// [`encode_player_info_add`](Self::encode_player_info_add), emitted by the
    /// same [`crate::players::PlayerListStreamer`] pass. Without it a departed
    /// player's `PlayerInfo` lingers, so their name stays in the tab list even
    /// though the entity diff already sent a `REMOVE_ENTITIES` for them. The
    /// default emits nothing, for the same reason as above.
    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        let _ = uuids;
        Vec::new()
    }

    /// Updates existing tab-list entries' game modes (vanilla's
    /// `ClientboundPlayerInfoUpdatePacket` carrying **only** the
    /// `UPDATE_GAME_MODE` action, ordinal 2).
    ///
    /// Needed by `/gamemode`. `encode_player_info_add` sends a game mode too, but
    /// only at join and only the mode the player joined in — it has no
    /// per-connection mode to read, and says so in its own doc comment. Without
    /// this, changing mode leaves every client's tab list reporting the join mode
    /// forever, including the player's own.
    ///
    /// A slice of pairs rather than one uuid because the packet is a list and a
    /// command may change several players at once; an empty slice must emit
    /// nothing (a zero-length entry list is a legal but pointless frame).
    ///
    /// The default emits nothing, so a protocol without tab-list support needs no
    /// override and the mode change is simply invisible there rather than a
    /// failure.
    fn encode_player_info_game_mode(&self, entries: &[(Uuid, GameMode)]) -> Vec<ServerDirective> {
        let _ = entries;
        Vec::new()
    }

    /// Encodes a detonation (vanilla `ClientboundExplodePacket`,
    /// wire id `explode`), fed from [`crate::mobs::MobSim::take_detonations`]
    /// via [`crate::tick::ExplosionFeed`] — the handoff that finally gives
    /// [`crate::mobs::MobSim::explode`] (the exposure/damage
    /// maths) a wire-visible consequence. Without this encoder, a creeper's
    /// fuse completion removed the creeper and landed real damage on nearby
    /// mobs, but no connected client saw a particle or heard a sound because
    /// no packet represented the detonation.
    ///
    /// `centre`/`radius` are the blast's own. This crate tracks no block-
    /// destruction model, so an implementor has nothing to report for
    /// vanilla's `blockCount`/`blockParticles`/`playerKnockback` fields
    /// beyond a faithful "none of that happened" — see the v770
    /// implementation's own doc comment for exactly which fields that
    /// leaves stubbed versus real. The default emits nothing, so a protocol
    /// without explosion support need not override it and a detonation
    /// simply stays silent and invisible.
    fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
        let _ = (centre, radius);
        ServerDirective::None
    }

    /// Encodes a positioned sound (vanilla
    /// `ClientboundSoundPacket`, wire id `sound`).
    ///
    /// `sound` is a `minecraft:sound_event` registry id
    /// ([`crate::effects`] validates every name it derives against the real
    /// registry before it reaches here, so an implementor may send the
    /// registry-reference holder form rather than an inline definition).
    /// `seed` picks between a sound event's variants — vanilla's per-play
    /// `random.nextLong()`.
    ///
    /// The default emits nothing, so a protocol without sound support need not
    /// override it and the world is simply silent for that client, which is
    /// exactly the state every protocol here was in before this method existed.
    fn encode_sound(
        &self,
        sound: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> ServerDirective {
        let _ = (sound, category, pos, volume, pitch, seed);
        ServerDirective::None
    }

    /// Encodes one of vanilla's numbered composite effects (vanilla
    /// `ClientboundLevelEventPacket`, wire id `level_event`) — see
    /// [`crate::effects::PARTICLES_DESTROY_BLOCK`], which is a sound *and* a
    /// particle burst in one packet. The default emits nothing.
    fn encode_level_event(&self, event: i32, pos: BlockPos, data: i32, global: bool) -> ServerDirective {
        let _ = (event, pos, data, global);
        ServerDirective::None
    }

    /// Encodes a particle burst (`ClientboundLevelParticlesPacket`,
    /// wire id `level_particles`).
    ///
    /// `particle` is a `minecraft:particle_type` registry id. Only
    /// argument-less (`SimpleParticleType`) particles are expressible: the
    /// per-type option payload — dust colour, block state, item stack — has no
    /// representation here, the same scope
    /// [`crate::effects::WorldEffect::Particles`] carries and the same one the
    /// v770 *decoder* already declares. The default emits nothing.
    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        offset: Vec3f,
        max_speed: f32,
        count: i32,
        long_distance: bool,
    ) -> ServerDirective {
        let _ = (particle, pos, offset, max_speed, count, long_distance);
        ServerDirective::None
    }

    /// Encodes one block entity's update tag (vanilla
    /// `ClientboundBlockEntityDataPacket`, wire id `block_entity_data`).
    ///
    /// `block_entity_type` is a `minecraft:block_entity_type` registry **key**, not
    /// a numeric id — resolving it is version-specific, so the implementor does it.
    /// A key this version does not have must emit nothing rather than guess.
    ///
    /// This is the mid-play counterpart to the block-entity array a chunk packet
    /// carries at load time: without it, a record that changes while the chunk is
    /// already resident never reaches the client at all. The default emits nothing.
    fn encode_block_entity_data(
        &self,
        pos: BlockPos,
        block_entity_type: &str,
        nbt: &lodestone_core::Nbt,
    ) -> ServerDirective {
        let _ = (pos, block_entity_type, nbt);
        ServerDirective::None
    }

    /// Answers an operator NBT query. `None` means no block entity exists.
    /// Unsupported hosting families emit nothing.
    fn encode_tag_query(&self, transaction_id: i32, tag: Option<&lodestone_core::Nbt>) -> ServerDirective {
        let _ = (transaction_id, tag);
        ServerDirective::None
    }

    /// Encodes one [`crate::effects::WorldEffect`] by dispatching to whichever
    /// of the encoders above it names.
    ///
    /// Provided rather than required: it is pure dispatch, so no implementor
    /// should override it, and it is the only method a *publisher* has to know
    /// about — `serve_play`'s drain calls this and nothing else, so adding a
    /// fourth effect kind is a change here plus one encoder, never a change at
    /// every drain site.
    fn encode_world_effect(&self, effect: &crate::effects::WorldEffect) -> ServerDirective {
        match effect {
            crate::effects::WorldEffect::Sound {
                sound,
                category,
                pos,
                volume,
                pitch,
                seed,
            } => self.encode_sound(sound, *category, *pos, *volume, *pitch, *seed),
            crate::effects::WorldEffect::LevelEvent {
                event,
                pos,
                data,
                global,
            } => self.encode_level_event(*event, *pos, *data, *global),
            crate::effects::WorldEffect::Particles {
                particle,
                pos,
                offset,
                max_speed,
                count,
                long_distance,
            } => self.encode_level_particles(particle, *pos, *offset, *max_speed, *count, *long_distance),
            crate::effects::WorldEffect::BlockEntityData {
                pos,
                block_entity_type,
                nbt,
            } => self.encode_block_entity_data(*pos, block_entity_type.name(), nbt),
            // This variant is not a packet — see its own doc
            // for why. `crate::server`'s world-effect drain intercepts it
            // before ever reaching this dispatcher in the one real consumer
            // that needs its payload; any caller that lets it fall through
            // here (a test driving `encode_world_effect` directly, say)
            // gets an honest no-op rather than a made-up packet.
            crate::effects::WorldEffect::PistonPlayerPush { .. } => ServerDirective::None,
        }
    }

    /// Encodes a server-initiated keep-alive challenge (vanilla
    /// `ClientboundKeepAlivePacket`, wire id `keep_alive`). `id` is the
    /// challenge value the loop expects echoed back as
    /// [`ServerBound::KeepAlive`]. The default emits nothing, so a protocol
    /// without keep-alive support need not override it.
    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        let _ = id;
        ServerDirective::None
    }

    /// Encodes a time-of-day update (vanilla `ClientboundSetTimePacket`, wire
    /// id `set_time`).
    ///
    /// `game_time` is the monotonic world age in ticks. `day_time`, when
    /// `Some`, anchors the day/night clock to that many elapsed ticks at the
    /// normal 1:1 rate — sent once at join, mirroring vanilla's full clock
    /// sync (`ServerClockManager::createFullSyncPacket`, sent from
    /// vanilla's own send-level-info routine). `None` sends only the monotonic
    /// game-time broadcast vanilla repeats every 20 ticks
    /// (`MinecraftServer::forceGameTimeSynchronization`) without touching the
    /// client's already-held day/night anchor. The default emits nothing.
    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        let _ = (game_time, day_time);
        ServerDirective::None
    }

    /// Encodes a chunk-cache-center update (vanilla
    /// `ClientboundSetChunkCacheCenterPacket`, wire id
    /// `set_chunk_cache_center`), sent whenever the player's tracked chunk
    /// column changes (`ChunkMap::applyChunkTrackingView`). The default emits
    /// nothing.
    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        let _ = (cx, cz);
        ServerDirective::None
    }

    /// Encodes a forget/unload signal for one chunk column leaving view
    /// (vanilla `ClientboundForgetLevelChunkPacket`, wire id
    /// `forget_level_chunk`; `ChunkMap::dropChunk`). The default emits
    /// nothing.
    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        let _ = (cx, cz);
        ServerDirective::None
    }

    /// Encodes a single block-state change (vanilla
    /// `ClientboundBlockUpdatePacket`, wire id `block_update`), confirming a
    /// break or placement back to the acting client — mirroring vanilla's
    /// own `ServerPlayerGameMode`/`ServerGamePacketListenerImpl`, which
    /// answer every dig/place with this same packet whether or not the edit
    /// actually took effect (see `crate::server`'s `UseItemOn` handling for
    /// why it sends two of these per placement).
    ///
    /// `state` is the canonical block-state string [`ChunkColumn`] itself
    /// stores (e.g. `"minecraft:air"`, `"minecraft:stone"`); resolving it to
    /// a wire registry id is the implementor's job, the same seam
    /// `encode_chunk` already crosses. The default emits nothing.
    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        let _ = (x, y, z, state);
        ServerDirective::None
    }

    /// Encodes an air-supply update for the local player (vanilla's entity
    /// metadata `DATA_AIR_SUPPLY_ID`, sent over `SET_ENTITY_DATA` — see
    /// `crates/protocol/v770/src/packets/metadata.rs`'s `IDX_AIR_SUPPLY`,
    /// the decode side this mirrors). `air` is the new value, `-20..=300`
    /// (never sent negative on the wire in practice — `crate::vitals`
    /// resets to `0` the same tick air crosses the drowning threshold, and
    /// [`PlayerVitals::tick`](crate::PlayerVitals::tick) reports that via
    /// [`VitalsTick::air_changed`](crate::VitalsTick::air_changed) alongside
    /// the reset, not the transient negative value). The default emits
    /// nothing, so a protocol without air-supply support need not override
    /// it and drowning simply never reaches that client's HUD.
    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        let _ = air;
        ServerDirective::None
    }

    /// Encodes the local player's experience bar (vanilla
    /// `ClientboundSetExperiencePacket`).
    ///
    /// **The client half already existed**: `V770Adapter::handle_play` decodes
    /// `SET_EXPERIENCE` into `ClientEvent::ExperienceChanged`, complete with the
    /// note that the wire order is *progress, level, total* rather than declaration
    /// order. Nothing produced the packet, which is the island in the serverbound
    /// direction — a decoder with no encoder.
    ///
    /// `progress` is the bar fill in `0.0..1.0`, `level` the number shown on it, and
    /// `total` vanilla's lifetime `totalExperience`, which is **not** derivable from
    /// the other two (see [`crate::experience::PlayerExperience`]).
    fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
        let _ = (progress, level, total);
        ServerDirective::None
    }

    /// Encodes a health update for the local player (vanilla's
    /// `ClientboundSetHealthPacket`, the same packet
    /// [`begin_play`](Self::begin_play) sends once at join with the
    /// fresh-spawn default). Sent whenever
    /// [`PlayerVitals::tick`](crate::PlayerVitals::tick) reports damage —
    /// currently only drowning. This crate tracks no food/hunger, so an implementor that
    /// reuses vanilla's combined health/food/saturation packet must supply
    /// its own constant food/saturation (see
    /// `V770ServerProtocol::encode_set_health` for the value it picks and
    /// why). The default emits nothing.
    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        let _ = (health, food, saturation);
        ServerDirective::None
    }

    /// Encodes an attribute update for the local player (vanilla
    /// `ClientboundUpdateAttributesPacket`).
    ///
    /// **The client half already existed and the server never fed it**: the
    /// HUD's armour row (`lodestone_shell::hud`), `Session::armour_value` and
    /// the v770 adapter's `UPDATE_ATTRIBUTES` decode were all in place, but
    /// this crate had no encoder at all — the armour bar read a permanent
    /// `None` in singleplayer no matter what was equipped. This is the
    /// island bug running in the *send* direction: a complete consumer chain
    /// with nothing at the other end producing the packet.
    ///
    /// `attributes` is whatever the caller has already folded — in practice
    /// `PlayerInventory::combat_stats().attributes`, so equipment maths lives
    /// in one place (`lodestone_entity::equipment`) rather than being
    /// re-derived at the wire. The default emits nothing, so a protocol
    /// without attribute support behaves exactly as it did before this
    /// method existed and the HUD row simply never appears.
    fn encode_update_attributes(&self, attributes: &[EntityAttributeSnapshot]) -> ServerDirective {
        let _ = attributes;
        ServerDirective::None
    }

    /// Encodes the death notification (vanilla `ClientboundPlayerCombatKillPacket`,
    /// wire id `player_combat_kill`) — **the packet that raises the death screen**.
    ///
    /// # Why this exists, and why nothing else does the job
    ///
    /// Health reaching `0.0` is *not* what opens the death screen, in vanilla or
    /// here. The client's ordinary health update handler only applies the new
    /// health, food and saturation values; it does not touch the screen stack. A
    /// separate, later handler is the one that raises the death screen — it checks
    /// whether the player is configured to show one at all, and either opens the
    /// death screen carrying the kill message or, if the screen is suppressed,
    /// respawns the player immediately instead.
    ///
    /// So a server that only sends `set_health(0.0)` leaves a real client — and
    /// this workspace's own client, whose `Screen::Death` and `death_frame` are
    /// fully wired and were reaching zero pixels for exactly this reason — sitting
    /// at zero hearts with no screen, no respawn button and no way out. That reads
    /// as a server hang, which is how it was reported.
    ///
    /// `player_entity_id` is the *victim's* entity id (the client discards it, but
    /// it is on the wire). `message` is the localized death message: the
    /// `death.attack.<id>` translation key for the damage source, formatted with
    /// the victim's name, used when nothing living gets the kill credit.
    ///
    /// The default emits nothing, so a protocol without death support need not
    /// override it — and its client simply never gets a death screen, which is a
    /// gap rather than a wrong packet.
    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        let _ = (player_entity_id, message);
        ServerDirective::None
    }

    /// Encodes a post-death respawn (vanilla `ClientboundRespawnPacket` plus the
    /// placement teleport `PlayerList::respawn` sends after it), moving the client
    /// off the death screen and to `spawn`.
    ///
    /// # Why the respawn packet is not optional
    ///
    /// This is the other half of [`encode_player_combat_kill`](Self::encode_player_combat_kill)
    /// and it fails in a nastier way when missing: the client's `Dead` marker is
    /// cleared only by `ClientEvent::Respawned`, which its adapter decodes from
    /// `player_combat_kill`'s counterpart `respawn` — **not** from a
    /// `set_health(20.0)`. A server that answers `client_command(perform_respawn)`
    /// by resetting vitals and sending health alone therefore refills the hearts
    /// and leaves the death screen up forever, with the player's own respawn
    /// button doing nothing. Sending health without this is strictly worse than
    /// sending neither, because it looks like it worked.
    ///
    /// Returns a directive *list* rather than one directive because the respawn is
    /// two packets: the dimension/data-to-keep record, then the position. The
    /// default emits nothing.
    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        let _ = spawn;
        Vec::new()
    }

    /// Encodes a generic post-join teleport/position-sync
    /// (`ClientboundPlayerPositionPacket`) — `/tp`'s producer, and any future
    /// caller that needs to move an already-joined player without a dimension
    /// change or a respawn.
    ///
    /// # Why this did not already exist
    ///
    /// The join sequence (`begin_play_at`) and the respawn path
    /// ([`encode_respawn`](Self::encode_respawn)) each build this exact packet
    /// with a free function private to their own implementing module, because
    /// neither needed it exposed through the trait. `dispatch_play_packet` is
    /// generic over `P: ServerProtocol` and cannot reach a family's private
    /// free function, so `/tp` needed this method before it could be wired at
    /// all — see `docs/server-commands.md`'s own note on why the command was
    /// previously unregistered.
    ///
    /// `yaw`/`pitch` are always absolute here, never relative: the *value*
    /// resolved by [`crate::commands::Effect::Teleport`]'s applier is already
    /// the target's final facing (see that variant's own doc for how a missing
    /// rotation is resolved to the target's current one before this is ever
    /// called), so this method carries no relative-flags bitset the way the
    /// wire packet's own optional relative-move fields could.
    ///
    /// The default emits nothing, so a protocol family with no `/tp` support
    /// need not override it and `/tp` typed against it produces a command that
    /// runs and reports success with no visible effect — the same documented
    /// posture [`encode_system_chat`](Self::encode_system_chat)'s own doc
    /// describes for a family that wants commands but not chat.
    fn encode_teleport(&self, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> ServerDirective {
        let _ = (x, y, z, yaw, pitch);
        ServerDirective::None
    }

    /// Encodes a same-dimension position correction with the server-selected
    /// acknowledgement id. Families without acknowledgement ids delegate to
    /// [`encode_teleport`](Self::encode_teleport).
    fn encode_teleport_with_id(
        &self,
        teleport_id: i32,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    ) -> ServerDirective {
        let _ = teleport_id;
        self.encode_teleport(x, y, z, yaw, pitch)
    }

    /// Encodes a death respawn with the id on its placement correction. The
    /// default preserves families whose respawn packet carries no such id.
    fn encode_respawn_with_teleport_id(&self, teleport_id: i32, spawn: Vec3) -> Vec<ServerDirective> {
        let _ = teleport_id;
        self.encode_respawn(spawn)
    }

    /// Encodes a dimension change with the id on its placement correction. An
    /// empty result still means the destination is unsupported.
    fn encode_dimension_change_with_teleport_id(
        &self,
        teleport_id: i32,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        let _ = teleport_id;
        self.encode_dimension_change(dimension, spawn, mode)
    }

    /// Encodes `ClientboundAnimatePacket` — the arm-swing animation, the
    /// [`ServerBound::Swing`] consumer's whole output. `action` is vanilla's
    /// own byte constant (`0` main-hand swing, `3` off-hand swing); this
    /// method carries it verbatim rather than a `hand` field, so a future
    /// caller that wants `WAKE_UP`/`CRITICAL_HIT`/`MAGIC_CRITICAL_HIT` (`2`,
    /// `4`, `5`) is not blocked on a new method.
    ///
    /// The default emits nothing, so a protocol family with no animation
    /// support need not override it — a swing that reaches this call and
    /// produces nothing is a documented gap, not a silently wrong packet.
    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        let _ = (entity_id, action);
        ServerDirective::None
    }

    /// Encodes `ClientboundSetCameraPacket` — attaches the receiving client's
    /// rendered viewpoint to `entity_id`, the whole of
    /// [`ServerBound::SpectatorAction`]'s consumer output. See that variant's
    /// own doc comment for why there is no corresponding "reset to self"
    /// encoder.
    ///
    /// The default emits nothing, matching every other encoder in this trait.
    fn encode_set_camera(&self, entity_id: i32) -> ServerDirective {
        let _ = entity_id;
        ServerDirective::None
    }

    /// Encodes a **dimension change** — the same `ClientboundRespawnPacket` pair
    /// [`encode_respawn`](Self::encode_respawn) sends, aimed at another level.
    ///
    /// # Why this is not `encode_respawn` with an argument
    ///
    /// The two differ in the one field that decides what the client throws away.
    /// Vanilla's own player-list respawn routine passes `KEEP_ALL_DATA` (`KEEP_ATTRIBUTE_MODIFIERS |
    /// KEEP_ENTITY_DATA`) for a dimension change and **zero** for a death, and
    /// `encode_respawn`'s own doc comment explains why zero is right there: it is
    /// what makes the client rebuild its player state. Rebuilding player state is
    /// exactly what a portal trip must *not* do — inventory, XP and health survive
    /// a trip — so folding the two into one encoder with a boolean would leave the
    /// dangerous default (`0`) one forgotten argument away.
    ///
    /// `dimension` is the destination *level key* (`minecraft:the_nether`), not a
    /// holder id: mapping a key to the `dimension_type` index its own
    /// [`encode_registry_data`](Self::encode_registry_data) published is the
    /// protocol family's business, and a version-free caller cannot know it. An
    /// implementation that does not recognise the key must emit **nothing** rather
    /// than guess a holder id, because a wrong id reframes every subsequent chunk
    /// against the wrong build height.
    ///
    /// # The empty return is load-bearing
    ///
    /// The default emits nothing, and `crate::server`'s travel path treats an empty
    /// list as "this protocol cannot change dimension" and **does not move the
    /// player**. That is the difference between a family without Nether support
    /// having no portals and having portals that silently drop players into terrain
    /// their client is still framing as the overworld.
    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        let _ = (dimension, spawn, mode);
        Vec::new()
    }

    /// Encodes a difficulty confirmation (vanilla
    /// `ClientboundChangeDifficultyPacket`, wire id `change_difficulty`),
    /// sent back to the requesting connection after
    /// [`ServerBound::DifficultyChanged`]/[`DifficultyLockChanged`](ServerBound::DifficultyLockChanged).
    /// `locked` is always the connection's *current* lock
    /// state, not necessarily what this particular request changed — see
    /// `crate::server`'s consumer, which always passes both fields together
    /// regardless of which of the two `ServerBound` variants triggered the
    /// call. The default emits nothing.
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        let _ = (difficulty, locked);
        ServerDirective::None
    }

    /// Encodes a game-rule confirmation (vanilla
    /// `ClientboundGameRuleValuesPacket`, wire id `game_rule_values`) for
    /// exactly the entries a [`ServerBound::GameRuleChanged`] request just
    /// set — not vanilla's full current-rule-table broadcast,
    /// since this crate models no default rule set to broadcast the rest of;
    /// see `crate::server`'s consumer for the full scope note. The default
    /// emits nothing.
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        let _ = entries;
        ServerDirective::None
    }

    /// Encodes a weather transition (vanilla `ClientboundGameEventPacket`,
    /// wire id `game_event` — the same packet the client's adapter decodes
    /// into `ClientEvent::WeatherChanged`).
    /// `kind` is the vanilla event id: 1 = `START_RAINING`, 2 = `STOP_RAINING`,
    /// 7 = `RAIN_LEVEL_CHANGE`, 8 = `THUNDER_LEVEL_CHANGE`. `value` is the
    /// float parameter — 0.0 for the start/stop pair, the level for the
    /// level-change pair — matching `ClientboundGameEventPacket`'s own
    /// `writeByte(event) + writeFloat(param)` layout. The default emits
    /// nothing, so a protocol without weather support simply never rains.
    fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
        let _ = (kind, value);
        ServerDirective::None
    }

    /// Opens a container's screen on the client (vanilla
    /// `ClientboundOpenScreenPacket`, sent by `ServerPlayer::openMenu`).
    /// `window_id` is the container id every subsequent `container_click`/
    /// `container_close` for this window will carry (vanilla's
    /// `nextContainerCounter`: `1..=100`, wrapping — see `crate::server`'s
    /// consumer for why this crate mirrors that exact scheme rather than a
    /// plain counter). `menu` is the vanilla `minecraft:*` menu identifier
    /// (e.g. `"minecraft:furnace"`); `title` is the screen's display name
    /// (this crate sends a plain literal string rather than vanilla's
    /// translatable `container.furnace`-style component — cosmetic only, see
    /// `crate::server`'s consumer for the fixed title table). The default
    /// emits nothing, so a protocol without container support need not
    /// override it.
    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        let _ = (window_id, menu, title);
        ServerDirective::None
    }

    /// Encodes the clientbound `merchant_offers` packet (vanilla
    /// `ClientboundMerchantOffersPacket`) that a villager or wandering trader
    /// interaction sends right after [`encode_open_screen`](Self::encode_open_screen).
    /// `level`/`xp` are the villager's own
    /// [`crate::mobs::SimMob::villager_level`]/[`villager_xp`](crate::mobs::SimMob::villager_xp);
    /// `show_progress` is whether the level/xp bar should be shown (`false`
    /// for a wandering trader, which has no level); `can_restock` is whether
    /// working at the workstation refreshes uses (also `false` for a
    /// wandering trader). The default emits nothing, so a protocol without
    /// merchant support need not override it.
    fn encode_merchant_offers(
        &self,
        window_id: i32,
        offers: &[MerchantOfferOut],
        level: i32,
        xp: i32,
        show_progress: bool,
        can_restock: bool,
    ) -> ServerDirective {
        let _ = (window_id, offers, level, xp, show_progress, can_restock);
        ServerDirective::None
    }

    /// Encodes the clientbound `container_set_content` packet: every slot in
    /// `items`, in vanilla menu order (the container's own slots first, then
    /// the player's standard 27-main + 9-hotbar inventory rows every such
    /// menu appends — never armour/off-hand, which only the player's own
    /// window `0` exposes), plus the cursor/carried stack. `state_id` is
    /// vanilla's own container-menu `stateId` field at the time of the send —
    /// this crate does not validate a click's echoed value against it (see
    /// `docs/server-inventory.md`'s existing scope note for window `0`,
    /// which now applies identically to any other window). The default
    /// emits nothing.
    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        let _ = (window_id, state_id, items, carried);
        ServerDirective::None
    }

    /// Encodes the clientbound `container_set_slot` packet for exactly one
    /// changed slot (vanilla `ClientboundContainerSetSlotPacket`), in the
    /// same menu-slot numbering [`encode_container_content`](Self::encode_container_content)
    /// uses. The default emits nothing.
    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        let _ = (window_id, state_id, slot, item);
        ServerDirective::None
    }

    /// Encodes the clientbound `set_held_slot` packet (vanilla
    /// `ClientboundSetHeldSlotPacket`) — a single VarInt hotbar index. Vanilla
    /// sends this as the unconditional first half of
    /// `ServerGamePacketListenerImpl::tryPickItem` (the pick-block action),
    /// whether or not the pick actually moved anything, so the client's
    /// selection is always resynchronised to the server's own
    /// selected-slot value after a middle-click. The default emits nothing.
    fn encode_set_held_slot(&self, slot: u8) -> ServerDirective {
        let _ = slot;
        ServerDirective::None
    }

    /// Encodes the clientbound `container_set_data` packet for one changed
    /// menu-local property (vanilla's `ContainerData`, e.g. a furnace's four
    /// burn/cook timers — see `crate::furnace::Furnace::container_data`'s own
    /// doc comment for the index table this feeds). Unlike a slot change,
    /// vanilla does not bump the container's `stateId` for a data change
    /// (`AbstractContainerMenu::broadcastChanges` calls `setData` directly,
    /// never `incrementStateId`), so this carries no `state_id` parameter at
    /// all. The default emits nothing.
    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        let _ = (window_id, property, value);
        ServerDirective::None
    }

    /// Encodes the clientbound `update_mob_effect` packet
    /// (`ClientboundUpdateMobEffectPacket`) — a status effect newly applied
    /// or refreshed on the entity at `entity_id`, `effect` a canonical
    /// `minecraft:*` key. `ambient`/`visible`/`show_icon` are vanilla's own
    /// three independent flags (a beacon/conduit application sets `ambient`
    /// and both display flags; `/effect give … true` sets neither display
    /// flag). `blend` is **not** an effect-type hint (nausea/darkness have no
    /// special case in `ClientboundUpdateMobEffectPacket` itself) — it is the
    /// call site: vanilla's own on-effect-added routine (a genuinely new instance)
    /// passes `true`, `onEffectUpdated` (an existing instance refreshed —
    /// same or higher amplifier, longer duration) passes `false`. The
    /// default emits nothing.
    #[allow(clippy::too_many_arguments)]
    fn encode_update_mob_effect(
        &self,
        entity_id: i32,
        effect: &str,
        amplifier: u32,
        duration_ticks: i32,
        ambient: bool,
        visible: bool,
        show_icon: bool,
        blend: bool,
    ) -> ServerDirective {
        let _ = (entity_id, effect, amplifier, duration_ticks, ambient, visible, show_icon, blend);
        ServerDirective::None
    }

    /// Encodes the clientbound `remove_mob_effect` packet
    /// (`ClientboundRemoveMobEffectPacket`) — `effect` cleared entirely from
    /// the entity at `entity_id` (an expired duration, or an explicit
    /// `/effect clear`). The default emits nothing.
    fn encode_remove_mob_effect(&self, entity_id: i32, effect: &str) -> ServerDirective {
        let _ = (entity_id, effect);
        ServerDirective::None
    }

    /// Encodes the clientbound `initialize_border` packet (vanilla
    /// `ClientboundInitializeBorderPacket`, wire id 43 in 26.2) — the border
    /// state a player is told about on join, sent by vanilla's own
    /// per-player level-info send routine
    /// **before** the time sync and spawn-position packets. The default emits nothing.
    ///
    /// Passed the whole [`WorldBorder`](crate::border::WorldBorder) because
    /// the packet's `old_size`/`new_size`/`lerp_time` triple is exactly the
    /// extent's `size`/`lerp_target`/`lerp_time` readout, and its
    /// `absolute_max_size`/`warning_blocks`/`warning_time` are flat fields —
    /// the encoder should not have to re-derive that mapping from primitives.
    fn encode_initialize_border(&self, border: &crate::border::WorldBorder) -> ServerDirective {
        let _ = border;
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_center` packet (vanilla
    /// `ClientboundSetBorderCenterPacket`, wire id 88 in 26.2). The default
    /// emits nothing.
    fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
        let _ = (x, z);
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_lerp_size` packet (vanilla
    /// `ClientboundSetBorderLerpSizePacket`, wire id 89 in 26.2) — the *live*
    /// resize delta a border shrink/grow broadcasts, carrying `old_size`,
    /// `new_size` and the lerp time in **milliseconds**. Vanilla writes
    /// `border.getLerpTime()` — remaining server **ticks** — directly
    /// (no ×50), but this crate's
    /// client decodes the field as `lerp_time_ms` and interpolates on wall-clock
    /// (`lodestone-game::worldborder`'s `BorderExtent::Moving`), so the caller

    /// converts ticks → ms (`* 50`) before calling and this method writes the ms
    /// value verbatim — the same deliberate divergence
    /// [`encode_initialize_border`](Self::encode_initialize_border) documents
    /// for its own lerp-time field. The default emits nothing.
    fn encode_set_border_lerp_size(
        &self,
        old_size: f64,
        new_size: f64,
        lerp_time_ms: i64,
    ) -> ServerDirective {
        let _ = (old_size, new_size, lerp_time_ms);
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_size` packet (vanilla
    /// `ClientboundSetBorderSizePacket`, wire id 90 in 26.2) — the instant
    /// snap a `set_size` broadcasts. The default emits nothing.
    fn encode_set_border_size(&self, size: f64) -> ServerDirective {
        let _ = size;
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_warning_delay` packet (vanilla
    /// `ClientboundSetBorderWarningDelayPacket`, wire id 91 in 26.2).
    /// The default emits nothing.
    fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
        let _ = warning_time;
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_warning_distance` packet (vanilla
    /// `ClientboundSetBorderWarningDistancePacket`, wire id 92 in 26.2).
    /// The default emits nothing.
    fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
        let _ = warning_blocks;
        ServerDirective::None
    }

    /// Encodes the clientbound `resource_pack_push` packet (vanilla
    /// `ClientboundResourcePackPushPacket`) — the server-initiated half of the
    /// resource-pack lifecycle. The body is the [`ResourcePackPush`]
    /// record verbatim: a raw 16-byte uuid, a VarInt-prefixed UTF-8 url, a
    /// VarInt-prefixed UTF-8 SHA-1 hash capped at 40 characters (vanilla's
    /// `MAX_HASH_LENGTH`), a bool `required` flag, then — only if present — an
    /// NBT chat component prompt. The default emits nothing.
    fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
        let _ = push;
        ServerDirective::None
    }

    /// Encodes the full `ClientboundUpdateAdvancementsPacket` (26.2) — the
    /// advancement tree plus per-player progress. The payload is
    /// [`crate::advancements::AdvancementUpdate`] verbatim, built by
    /// [`AdvancementManager::initial_update`](crate::advancements::AdvancementManager::initial_update)
    /// on join (`reset` true, the whole tree as `added`) and by
    /// [`flush_dirty`](crate::advancements::AdvancementManager::flush_dirty)
    /// on every tick that something changed (incremental `added`/`removed`
    /// deltas plus the changed `progress`). Vanilla's `AdvancementHolder`
    /// travels as the `added` list and `CriterionProgress` as each
    /// `AdvancementProgressUpdate` entry's epoch-millis. The default emits
    /// nothing, so a protocol without advancement support never shows a tree.
    fn encode_update_advancements(&self, update: &crate::advancements::AdvancementUpdate) -> ServerDirective {
        let _ = update;
        ServerDirective::None
    }

    /// Encodes the `ClientboundAwardStatsPacket` (26.2): a batch of
    /// `(StatKey, count)` pairs, sent in reply to the client's
    /// `ClientCommand(REQUEST_STATS)`. Each `StatKey` is the
    /// stat-type registry id (e.g. `minecraft:mined`) plus the value key
    /// (item/block/entity id, or the custom-stat id), exactly vanilla's own
    /// per-stat wire codec dispatch; an implementor maps those to registry
    /// ids and writes the count as a varint. The default emits nothing.
    fn encode_award_stats(&self, stats: &[(crate::advancements::StatKey, i32)]) -> ServerDirective {
        let _ = stats;
        ServerDirective::None
    }

    /// Encodes the `ClientboundRecipeBookAddPacket` (26.2) — the packet that
    /// **hands out `RecipeDisplayId`s**.
    ///
    /// Without it `PLACE_RECIPE` is structurally unreachable rather than merely
    /// unimplemented: the id a client echoes back is a position in *this* list, so
    /// no client — ours or a real vanilla 26.2 one — can ever send a valid one
    /// until something encodes it. `crate::crafting::recipe_at_index` and
    /// [`crate::crafting::recipe_book_entries`] walk the same id-sorted corpus
    /// order, so the index space cannot disagree.
    ///
    /// `replace` is vanilla's flag for "this is the whole book" (`true` at join)
    /// versus "add these to what you have". The default emits nothing, so a
    /// protocol without recipe-book support simply leaves the book empty — which
    /// is what every family other than v770 does.
    fn encode_recipe_book_add(
        &self,
        entries: &[crate::crafting::RecipeBookEntry],
        replace: bool,
    ) -> ServerDirective {
        let _ = (entries, replace);
        ServerDirective::None
    }

    /// Encodes the `ClientboundSelectAdvancementsTabPacket` (26.2), sent in
    /// reply to the client's `select_advancements_tab` request.
    /// `tab` is the advancement id to open, or `None` to close the screen —
    /// vanilla answers the client's own request with the same id it was given.
    /// The default emits nothing.
    fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
        let _ = tab;
        ServerDirective::None
    }

    /// Which worldgen data bundle this protocol's hosting needs, for the
    /// [`crate::worldgen_data`] version gate.
    ///
    /// The only bundle this crate embeds is 26.2
    /// ([`WorldgenScope::V26_2`]) — the `assets/worldgen/` data
    /// [`crate::overworld_generator`] serves. A hosting family must report
    /// [`WorldgenScope::V26_2`] if and only if that bundle is the terrain it
    /// actually wants to serve. The default reports [`WorldgenScope::None`],
    /// so a protocol that has not adopted the gate — every test double, and
    /// every family whose worldgen is not the embedded 26.2 bundle — is
    /// treated as "no worldgen data", never silently served the wrong
    /// bundle. The one production override is the v770 host (→
    /// [`WorldgenScope::V26_2`]): that is the family the embedded data
    /// belongs to.
    fn worldgen_scope(&self) -> WorldgenScope {
        WorldgenScope::None
    }
}

/// Forwards every method to the boxed implementor, so a **trait object** can be
/// handed to the parts of this crate that take `P: ServerProtocol` by value —
/// [`IntegratedServer::open_in_memory`](crate::IntegratedServer::open_in_memory)
/// and [`bind`](crate::IntegratedServer::bind).
///
/// This is what makes the version seam work in the *serverbound* direction. The
/// clientbound side hands out `Box<dyn VersionAdapter>` from
/// `lodestone_registry::adapter_for_protocol`, and its serverbound twin
/// (`lodestone_registry::server_protocol_for_protocol`) has to do the same: a
/// registry that resolves a protocol *number* cannot return a concrete type, so
/// the only thing it can return is a box. [`ServerProtocol`] is object-safe
/// already (every method takes `&self`, none is generic, none mentions `Self`),
/// but `Box<dyn ServerProtocol>` does not implement the trait for free — without
/// this impl the box could not be served, and the only way to start singleplayer
/// would be for the caller to name the concrete version type. Which is precisely
/// what the seam exists to forbid.
///
/// Written over `Box<P>` with `P: ?Sized` rather than over `Box<dyn ServerProtocol>`
/// so it also covers a boxed *concrete* protocol; `Box` is `#[fundamental]`, so
/// the impl is coherent here in the trait's own crate.
///
/// **When you add a method to [`ServerProtocol`], add its forward here.** A
/// defaulted method that is not forwarded is not a compile error — the box would
/// silently answer with the trait's default (usually
/// [`ServerDirective::None`]) instead of asking the real protocol, so a boxed
/// v770 would stop sending, say, keep-alives while a directly-owned one kept
/// working. That asymmetry is invisible to any test that uses one shape only.
impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P> {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        (**self).decode(state, packet_id, payload)
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        (**self).login_success(username, uuid)
    }

    fn has_configuration_phase(&self) -> bool {
        (**self).has_configuration_phase()
    }

    fn encode_encryption_request(
        &self,
        public_key_der: &[u8],
        verify_token: &[u8],
    ) -> ServerDirective {
        (**self).encode_encryption_request(public_key_der, verify_token)
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        (**self).begin_configuration()
    }

    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        (**self).encode_registry_data()
    }

    fn encode_status_response(
        &self,
        description: &str,
        players_online: i32,
        players_max: i32,
        sample: &[(Uuid, String)],
        favicon_png: Option<&[u8]>,
        enforces_secure_chat: bool,
    ) -> ServerDirective {
        (**self).encode_status_response(
            description,
            players_online,
            players_max,
            sample,
            favicon_png,
            enforces_secure_chat,
        )
    }

    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        (**self).encode_pong_response(time)
    }

    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        (**self).encode_disconnect(state, reason)
    }

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        (**self).encode_system_chat(message)
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        (**self).begin_play(view_radius)
    }

    // **The forwarding contract requires exactly this three-line
    // delegation.** `begin_play_at` has a default that discards `spawn` and calls
    // `begin_play`, so without this the box silently took that default: the
    // spiral search ran, `server.rs` passed its answer in, and the boxed
    // protocol threw it away and emitted `V770ServerProtocol::begin_play`'s
    // hardcoded `(8, 100, 8)`. Singleplayer is the *only* path that boxes the
    // protocol, so the symptom was "every join lands at y=100 at (8, 8)" with a
    // fully correct spawn search sitting one call frame away — and no live
    // oracle covers the boxed path, which is what the parity test below is for.
    fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
        (**self).begin_play_at(view_radius, spawn, mode)
    }

    fn uses_teleport_acknowledgements(&self) -> bool {
        (**self).uses_teleport_acknowledgements()
    }

    fn begin_play_at_with_teleport_id(
        &self,
        view_radius: i32,
        spawn: Vec3,
        mode: GameMode,
        teleport_id: i32,
    ) -> Vec<ServerDirective> {
        (**self).begin_play_at_with_teleport_id(view_radius, spawn, mode, teleport_id)
    }

    // Forwarded for the same reason `begin_play_at` is: both have defaults that
    // emit nothing, so a missing forward here would silently mute the game-mode
    // and abilities packets on the singleplayer (boxed) path alone.
    fn encode_game_mode(&self, mode: GameMode) -> ServerDirective {
        (**self).encode_game_mode(mode)
    }

    fn encode_player_abilities(&self, abilities: Abilities) -> ServerDirective {
        (**self).encode_player_abilities(abilities)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        (**self).begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        (**self).encode_chunk(cx, cz, column)
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        (**self).try_encode_chunk(cx, cz, column)
    }

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        (**self).try_encode_chunk_in_dimension(cx, cz, column, dimension)
    }

    fn try_encode_chunk_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
    ) -> Result<ServerDirective, ChunkEncodeError> {
        (**self).try_encode_chunk_with_neighbours(cx, cz, column, neighbours)
    }

    fn try_encode_chunk_with_neighbours_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        (**self).try_encode_chunk_with_neighbours_in_dimension(
            cx, cz, column, neighbours, dimension,
        )
    }

    fn compute_initial_column_light_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        (**self).compute_initial_column_light_with_neighbours_in_dimension(
            column,
            neighbours,
            dimension,
        )
    }

    fn compute_initial_column_lights_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: Dimension,
    ) -> Option<ColumnLightSettlement> {
        (**self).compute_initial_column_lights_with_neighbours_in_dimension(
            column,
            neighbours,
            dimension,
        )
    }

    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn ChunkEncoder>> {
        (**self).chunk_encoder()
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        (**self).end_chunk_batch(batch_size)
    }

    fn encode_light_update(
        &self,
        cx: i32,
        cz: i32,
        light: &lodestone_world::ColumnLight,
    ) -> ServerDirective {
        (**self).encode_light_update(cx, cz, light)
    }

    fn compute_column_light(&self, column: &ChunkColumn) -> Option<lodestone_world::ColumnLight> {
        (**self).compute_column_light(column)
    }

    fn compute_column_light_in_dimension(
        &self,
        column: &ChunkColumn,
        dimension: Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        (**self).compute_column_light_in_dimension(column, dimension)
    }

    fn uses_cross_column_light(&self) -> bool {
        (**self).uses_cross_column_light()
    }

    fn retains_initial_column_light(&self) -> bool {
        (**self).retains_initial_column_light()
    }

    fn compute_column_light_with_neighbours(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
    ) -> Option<lodestone_world::ColumnLight> {
        (**self).compute_column_light_with_neighbours(column, neighbours)
    }

    fn compute_column_light_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        (**self).compute_column_light_with_neighbours_in_dimension(column, neighbours, dimension)
    }

    fn welcome_message(&self) -> Vec<ServerDirective> {
        (**self).welcome_message()
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        (**self).encode_add_entity(entity)
    }

    fn encode_entity_update(
        &self,
        prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        (**self).encode_entity_update(prev, current)
    }

    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        (**self).encode_remove_entity(ids)
    }

    fn encode_take_item_entity(
        &self,
        item_entity_id: i32,
        collector_entity_id: i32,
        amount: i32,
    ) -> ServerDirective {
        (**self).encode_take_item_entity(item_entity_id, collector_entity_id, amount)
    }

    fn encode_hurt_animation(&self, entity_id: i32, yaw: f32) -> ServerDirective {
        (**self).encode_hurt_animation(entity_id, yaw)
    }

    fn encode_entity_event(&self, entity_id: i32, event: u8) -> ServerDirective {
        (**self).encode_entity_event(entity_id, event)
    }

    fn encode_commands(&self, tree: &CommandTree) -> ServerDirective {
        (**self).encode_commands(tree)
    }

    fn encode_command_suggestions(&self, response: &CommandSuggestionsResponse) -> ServerDirective {
        (**self).encode_command_suggestions(response)
    }

    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        (**self).encode_set_passengers(vehicle_id, passenger_ids)
    }

    fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
        (**self).encode_set_entity_link(source_id, target_id)
    }

    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        (**self).encode_set_entity_data(entity_id, fields)
    }

    fn encode_boss_event_add(&self, id: Uuid, name: &Text, progress: f32) -> ServerDirective {
        (**self).encode_boss_event_add(id, name, progress)
    }

    fn encode_boss_event_update_progress(&self, id: Uuid, progress: f32) -> ServerDirective {
        (**self).encode_boss_event_update_progress(id, progress)
    }

    fn encode_boss_event_remove(&self, id: Uuid) -> ServerDirective {
        (**self).encode_boss_event_remove(id)
    }

    fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
        (**self).encode_explode(centre, radius)
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        (**self).encode_keep_alive(id)
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        (**self).encode_set_time(game_time, day_time)
    }

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        (**self).encode_chunk_cache_center(cx, cz)
    }

    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        (**self).encode_forget_chunk(cx, cz)
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        (**self).encode_block_update(x, y, z, state)
    }

    fn encode_block_entity_data(
        &self,
        pos: BlockPos,
        block_entity_type: &str,
        nbt: &lodestone_core::Nbt,
    ) -> ServerDirective {
        (**self).encode_block_entity_data(pos, block_entity_type, nbt)
    }

    fn encode_tag_query(&self, transaction_id: i32, tag: Option<&lodestone_core::Nbt>) -> ServerDirective {
        (**self).encode_tag_query(transaction_id, tag)
    }

    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        (**self).encode_air_supply_update(air)
    }

    fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
        (**self).encode_set_experience(progress, level, total)
    }

    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        (**self).encode_set_health(health, food, saturation)
    }

    fn encode_update_attributes(&self, attributes: &[EntityAttributeSnapshot]) -> ServerDirective {
        (**self).encode_update_attributes(attributes)
    }

    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        (**self).encode_change_difficulty(difficulty, locked)
    }

    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        (**self).encode_game_rule_values(entries)
    }

    fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
        (**self).encode_game_event(kind, value)
    }

    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        (**self).encode_open_screen(window_id, menu, title)
    }

    fn encode_merchant_offers(
        &self,
        window_id: i32,
        offers: &[MerchantOfferOut],
        level: i32,
        xp: i32,
        show_progress: bool,
        can_restock: bool,
    ) -> ServerDirective {
        (**self).encode_merchant_offers(window_id, offers, level, xp, show_progress, can_restock)
    }

    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        (**self).encode_container_content(window_id, state_id, items, carried)
    }

    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        (**self).encode_container_slot(window_id, state_id, slot, item)
    }

    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        (**self).encode_container_data(window_id, property, value)
    }

    fn encode_update_mob_effect(
        &self,
        entity_id: i32,
        effect: &str,
        amplifier: u32,
        duration_ticks: i32,
        ambient: bool,
        visible: bool,
        show_icon: bool,
        blend: bool,
    ) -> ServerDirective {
        (**self).encode_update_mob_effect(
            entity_id,
            effect,
            amplifier,
            duration_ticks,
            ambient,
            visible,
            show_icon,
            blend,
        )
    }

    fn encode_remove_mob_effect(&self, entity_id: i32, effect: &str) -> ServerDirective {
        (**self).encode_remove_mob_effect(entity_id, effect)
    }

    fn encode_set_held_slot(&self, slot: u8) -> ServerDirective {
        (**self).encode_set_held_slot(slot)
    }

    fn encode_initialize_border(&self, border: &crate::border::WorldBorder) -> ServerDirective {
        (**self).encode_initialize_border(border)
    }

    fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
        (**self).encode_set_border_center(x, z)
    }

    fn encode_set_border_lerp_size(
        &self,
        old_size: f64,
        new_size: f64,
        lerp_time_ms: i64,
    ) -> ServerDirective {
        (**self).encode_set_border_lerp_size(old_size, new_size, lerp_time_ms)
    }

    fn encode_set_border_size(&self, size: f64) -> ServerDirective {
        (**self).encode_set_border_size(size)
    }

    fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
        (**self).encode_set_border_warning_delay(warning_time)
    }

    fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
        (**self).encode_set_border_warning_distance(warning_blocks)
    }

    fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
        (**self).encode_resource_pack_push(push)
    }

    fn encode_update_advancements(
        &self,
        update: &crate::advancements::AdvancementUpdate,
    ) -> ServerDirective {
        (**self).encode_update_advancements(update)
    }

    fn encode_award_stats(&self, stats: &[(crate::advancements::StatKey, i32)]) -> ServerDirective {
        (**self).encode_award_stats(stats)
    }

    fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
        (**self).encode_select_advancements_tab(tab)
    }

    fn encode_custom_payload(&self, channel: &ResourceKey, data: &[u8]) -> ServerDirective {
        (**self).encode_custom_payload(channel, data)
    }

    fn encode_player_info_add(&self, players: &[PlayerListing]) -> Vec<ServerDirective> {
        (**self).encode_player_info_add(players)
    }

    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        (**self).encode_player_info_remove(uuids)
    }

    fn encode_player_info_game_mode(&self, entries: &[(Uuid, GameMode)]) -> Vec<ServerDirective> {
        (**self).encode_player_info_game_mode(entries)
    }

    // The three world-effect encoders and their dispatcher. Every one of them had
    // an emit-nothing default and no forward, so a boxed protocol — i.e. every
    // singleplayer session — produced **no sounds, no level events and no
    // particles at all**, silently, while a directly-owned protocol emitted them
    // normally. Same shape as `begin_play_at` above, and the same reason it went
    // unnoticed: the drain site calls `encode_world_effect` and gets a
    // `ServerDirective::None` that is indistinguishable from "nothing happened".
    fn encode_sound(
        &self,
        sound: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> ServerDirective {
        (**self).encode_sound(sound, category, pos, volume, pitch, seed)
    }

    fn encode_level_event(&self, event: i32, pos: BlockPos, data: i32, global: bool) -> ServerDirective {
        (**self).encode_level_event(event, pos, data, global)
    }

    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        offset: Vec3f,
        max_speed: f32,
        count: i32,
        long_distance: bool,
    ) -> ServerDirective {
        (**self).encode_level_particles(particle, pos, offset, max_speed, count, long_distance)
    }

    // Forwarded even though the trait's own body is pure dispatch and would
    // already reach the inner protocol through the four forwards above: an
    // implementor that *does* override the dispatcher would otherwise have its
    // override skipped by the box, and the parity guard below requires a forward
    // for every trait method rather than reasoning about which ones are
    // redundant.
    fn encode_world_effect(&self, effect: &crate::effects::WorldEffect) -> ServerDirective {
        (**self).encode_world_effect(effect)
    }

    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        (**self).encode_player_combat_kill(player_entity_id, message)
    }

    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        (**self).encode_dimension_change(dimension, spawn, mode)
    }

    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        (**self).encode_respawn(spawn)
    }

    fn encode_teleport(&self, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> ServerDirective {
        (**self).encode_teleport(x, y, z, yaw, pitch)
    }

    fn encode_teleport_with_id(
        &self,
        teleport_id: i32,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    ) -> ServerDirective {
        (**self).encode_teleport_with_id(teleport_id, x, y, z, yaw, pitch)
    }

    fn encode_respawn_with_teleport_id(&self, teleport_id: i32, spawn: Vec3) -> Vec<ServerDirective> {
        (**self).encode_respawn_with_teleport_id(teleport_id, spawn)
    }

    fn encode_dimension_change_with_teleport_id(
        &self,
        teleport_id: i32,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        (**self).encode_dimension_change_with_teleport_id(teleport_id, dimension, spawn, mode)
    }

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        (**self).encode_animate(entity_id, action)
    }

    fn encode_set_camera(&self, entity_id: i32) -> ServerDirective {
        (**self).encode_set_camera(entity_id)
    }

    fn encode_recipe_book_add(
        &self,
        entries: &[crate::crafting::RecipeBookEntry],
        replace: bool,
    ) -> ServerDirective {
        (**self).encode_recipe_book_add(entries, replace)
    }

    fn worldgen_scope(&self) -> WorldgenScope {
        (**self).worldgen_scope()
    }
}
