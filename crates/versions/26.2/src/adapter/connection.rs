//! Connection-lifecycle packets: login/configuration state dispatch, plus
//! the Play-state equivalents of the same concepts (keep-alive, cookies,
//! resource packs, transfer, disconnect, custom payload). Split out of the
//! former monolithic `adapter.rs`.
use super::*;

impl V770Adapter {
    /// Clientbound play-state packets in the connection domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_connection(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::KEEP_ALIVE {
            let keep_alive: KeepAlive = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
                id: keep_alive.id,
            })]);
        }
        if packet_id == play::clientbound::PING {
            let ping: Pong = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::Ping { id: ping.id })]);
        }
        if packet_id == play::clientbound::START_CONFIGURATION {
            // The server is pulling us back into configuration mid-session
            // (resource-pack/datapack reload, `transfer`). Acknowledge on the
            // play protocol, then switch state so subsequent packets decode as
            // configuration. The packet body is empty.
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            // A proxy backend switch looks exactly like this from here — see
            // the `xfer` module's doc for the whole chain this target records.
            tracing::debug!(
                target: "transfer",
                seq = super::xfer::next_seq(),
                path = "backend-swap",
                "xfer: state -- START_CONFIGURATION; leaving Play for Configuration \
                 (a mid-session reconfigure, or a proxy moving us to another backend \
                 on this same socket -- the second LOGIN that follows is the tell)"
            );
            return Ok(vec![
                send(
                    play::serverbound::CONFIGURATION_ACKNOWLEDGED,
                    &ConfigurationAcknowledged,
                )?,
                Directive::SetState(ConnectionState::Configuration),
            ]);
        }
        if packet_id == play::clientbound::DISCONNECT {
            return Ok(vec![Directive::Disconnect(nbt_reason_text(payload)?)]);
        }
        if packet_id == play::clientbound::TRANSFER {
            let mut reader = Reader::new(payload);
            let host = reader.string(32767).map_err(dec_err)?;
            let port = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            tracing::debug!(
                target: "transfer",
                seq = super::xfer::next_seq(),
                host = %host,
                port,
                state = "Play",
                path = "reconnect",
                "xfer: state -- TRANSFER; the server is sending us to a new address, \
                 which is a different thing from a proxy backend swap: this ends the \
                 session and starts a new connection"
            );
            return Ok(vec![Directive::Emit(ClientEvent::TransferRequested {
                host,
                port,
            })]);
        }
        if packet_id == play::clientbound::COOKIE_REQUEST {
            return decode_cookie_request(payload);
        }
        if packet_id == play::clientbound::STORE_COOKIE {
            let mut reader = Reader::new(payload);
            let key = reader.string(32767).map_err(dec_err)?;
            let key = parse_key(&key, "cookie")?;
            let cookie_payload = reader.var_bytes(5120).map_err(dec_err)?.to_vec();
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CookieStored {
                key,
                payload: cookie_payload,
            })]);
        }
        if packet_id == play::clientbound::RESOURCE_PACK_PUSH {
            let mut reader = Reader::new(payload);
            let id = reader.uuid().map_err(dec_err)?;
            let url = reader.string(32767).map_err(dec_err)?;
            let hash = reader.string(40).map_err(dec_err)?;
            let required = reader.bool().map_err(dec_err)?;
            let has_prompt = reader.bool().map_err(dec_err)?;
            let prompt = if has_prompt {
                let component = read_network_nbt(&mut reader).map_err(dec_err)?;
                Some(Text::from_nbt(&component))
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ResourcePackPushed {
                id,
                url,
                hash,
                required,
                prompt,
            })]);
        }
        if packet_id == play::clientbound::RESOURCE_PACK_POP {
            let mut reader = Reader::new(payload);
            let has_id = reader.bool().map_err(dec_err)?;
            let id = if has_id {
                Some(reader.uuid().map_err(dec_err)?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ResourcePackPopped { id })]);
        }
        if packet_id == play::clientbound::CUSTOM_PAYLOAD {
            return decode_custom_payload(payload);
        }
        if packet_id == play::clientbound::SERVER_DATA {
            let mut reader = Reader::new(payload);
            let motd_nbt = read_network_nbt(&mut reader).map_err(dec_err)?;
            let motd = Text::from_nbt(&motd_nbt);
            let has_icon = reader.bool().map_err(dec_err)?;
            let icon = if has_icon {
                let remaining = reader.remaining();
                Some(reader.var_bytes(remaining).map_err(dec_err)?.to_vec())
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ServerDataReceived {
                motd,
                icon,
            })]);
        }
        if packet_id == play::clientbound::PONG_RESPONSE {
            // The clientbound pong-response packet (the play-state ping
            // protocol's own one), distinct from the `PING`/`ClientEvent::Ping`
            // pair handled above.
            let mut reader = Reader::new(payload);
            let time = reader.i64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PongReceived { time })]);
        }
        if packet_id == play::clientbound::UPDATE_TAGS {
            // Same wire shape and override as the Configuration-state arm —
            // vanilla can resend tags in Play too (e.g. a
            // reload), and vanilla's own tag-update handler is shared by both
            // states in the decompiled source.
            self.install_update_tags(payload)?;
            return Ok(Vec::new());
        }
        if packet_id == play::clientbound::BUNDLE_DELIMITER {
            // No fields: vanilla's own bundle-delimiter packet base class
            // overrides neither a reader nor a writer (confirmed against the
            // decompiled 26.2 client source) — vanilla's own pipeline never
            // puts a body on the wire for it either;
            // it is purely a toggle the pipeline uses to group the packets
            // between two delimiters into one atomic apply. Before
            // this arm existed, `BUNDLE_DELIMITER` fell through to the catch-all below
            // and decoded to zero directives, silently and safely (each real
            // packet is still independently length-framed by the transport, so
            // nothing about framing was ever at risk) — the actual gap was that
            // the atomicity guarantee itself did not exist. See
            // `Directive::BundleDelimiter`'s own doc for why pairing the two
            // delimiters happens above this crate, not here.
            let reader = Reader::new(payload);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::BundleDelimiter]);
        }
        if packet_id == play::clientbound::LOW_DISK_SPACE_WARNING {
            // Vanilla's own unit stream codec: zero bytes. `ensure_empty` is the
            // whole decode, and it is worth keeping — a non-empty body would mean
            // the id table is wrong, not that the packet grew a field.
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::LowDiskSpaceWarning)]);
        }
        if packet_id == play::clientbound::CUSTOM_REPORT_DETAILS {
            return decode_custom_report_details(payload);
        }
        if packet_id == play::clientbound::SERVER_LINKS {
            return decode_server_links(payload);
        }
        Ok(Vec::new())
    }
}

/// The `minecraft:block` registry's wire key
/// (the block registry's key), matching the
/// `minecraft:worldgen/biome` precedent in `packets/registry.rs`'s
/// `ClientRegistries::BIOME` — the registry's own resource key, not a name we
/// invent.
const BLOCK_REGISTRY_KEY: &str = "minecraft:block";
/// Decodes `update_tags`, shared by the Configuration and Play
/// states — vanilla's clientbound tag-update packet is a common-listener
/// packet with one wire shape used in both (confirmed against the
/// decompiled 26.2 source):
///
/// ```text
/// VarInt registry_count
/// registry_count * {
///     String registry_key           // e.g. "minecraft:block"
///     VarInt tag_count
///     tag_count * {
///         String tag_name           // without the leading '#'
///         VarInt id_count
///         id_count * VarInt element_id
///     }
/// }
/// ```
///
/// Every registry is validated before a block snapshot is returned. Block
/// members are translated into canonical identities at this boundary. A
/// present block registry replaces the complete session snapshot; an absent
/// registry leaves the previous snapshot untouched.
fn decode_update_tags(
    payload: &[u8],
    dialect: &ProtocolDialect,
) -> Result<Option<lodestone_data::tool::BlockTagSnapshot>, AdapterError> {
    let mut reader = Reader::new(payload);
    let registry_count = tag_count(&mut reader)?;
    let mut registries = std::collections::HashSet::new();
    let mut snapshot = None;
    for _ in 0..registry_count {
        let registry_key = reader.string(32767).map_err(dec_err)?;
        let registry_key = parse_key(&registry_key, "tag registry")?.to_string();
        if !registries.insert(registry_key.clone()) {
            return Err(AdapterError::Decode(format!("invalid or repeated tag registry {registry_key}")));
        }
        let is_block_registry = registry_key == BLOCK_REGISTRY_KEY;
        let count = tag_count(&mut reader)?;
        let mut names = std::collections::HashSet::new();
        let mut block_tags = is_block_registry.then(HashMap::new);
        for _ in 0..count {
            let tag_name = reader.string(32767).map_err(dec_err)?;
            let tag_name = parse_key(&tag_name, "block tag")?.to_string();
            if !names.insert(tag_name.clone()) {
                return Err(AdapterError::Decode(format!("invalid or repeated block tag {tag_name}")));
            }
            let id_count = tag_count(&mut reader)?;
            let mut ids = Vec::new();
            for _ in 0..id_count {
                let raw = reader.var_i32().map_err(dec_err)?;
                let raw = u32::try_from(raw).map_err(|_| {
                    AdapterError::Decode(format!("negative tag member {raw}"))
                })?;
                if is_block_registry {
                    ids.push(dialect.block_from_wire(raw).ok_or_else(|| {
                        AdapterError::Decode(format!("unmapped block tag member {raw}"))
                    })?);
                }
            }
            if let Some(map) = block_tags.as_mut() {
                map.insert(tag_name, ids);
            }
        }
        if let Some(map) = block_tags {
            snapshot = Some(lodestone_data::tool::BlockTagSnapshot::new(map));
        }
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(snapshot)
}

fn tag_count(reader: &mut Reader<'_>) -> Result<usize, AdapterError> {
    let raw = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(raw)
        .map_err(|_| AdapterError::Decode(format!("invalid tag list count {raw}")))?;
    if count > reader.remaining() {
        return Err(AdapterError::Decode(format!("tag list count {count} exceeds remaining payload")));
    }
    Ok(count)
}

impl V770Adapter {
    fn install_update_tags(&self, payload: &[u8]) -> Result<(), AdapterError> {
        if let Some(tags) = decode_update_tags(payload, &self.dialect)? {
            *self.block_tags.write().unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(tags));
        }
        Ok(())
    }
}

/// Decodes a clientbound `custom_payload`: a channel identifier followed by
/// however many bytes remain in the packet.
/// Shared by the Configuration and Play states — Configuration used to have
/// no arm for this at all; only Play did.
///
/// Only `minecraft:brand` gets a specially-typed codec in vanilla (a single
/// UTF-8 string); every other channel is a discarded payload, which just
/// consumes whatever bytes remain in the packet. Carrying the raw bytes for
/// every channel (rather than special-casing brand) loses nothing and avoids
/// guessing at channel-specific shapes this adapter cannot verify.
fn decode_custom_payload(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let channel = reader.string(32767).map_err(dec_err)?;
    let channel = parse_key(&channel, "custom payload channel")?;
    let remaining = reader.remaining();
    let data = reader.bytes(remaining).map_err(dec_err)?.to_vec();
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::CustomPayload {
        channel,
        data,
    })])
}

/// Decodes a clientbound `custom_query` (Login state only): a VarInt
/// transaction id, a channel identifier, then however many bytes remain
/// (vanilla's clientbound custom-query packet). This is the older,
/// pre-`custom_payload` plugin-message mechanism (historically Forge/FML's
/// login handshake); even vanilla's own reference client never interprets a
/// payload — every channel decodes to a discarded-payload stand-in, which
/// just skips the remaining bytes — and unconditionally answers with no
/// payload (vanilla's own login-handshake handler builds its answer packet
/// with a null payload unconditionally, no channel check, no UI). This
/// mirrors that exactly: decode to stay byte-aligned, answer `None`,
/// surface no event — there is nothing to observe that vanilla itself does
/// not already discard silently.
fn decode_custom_query(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let transaction_id = reader.var_i32().map_err(dec_err)?;
    let _channel = reader.string(32767).map_err(dec_err)?;
    let remaining = reader.remaining();
    reader.bytes(remaining).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![send(
        login::serverbound::CUSTOM_QUERY_ANSWER,
        &CustomQueryAnswer {
            transaction_id,
            payload: None,
        },
    )?])
}

/// Decodes a clientbound `cookie_request`: a single identifier key, no other
/// fields.
/// Shared by the Login, Configuration and Play states — the same "aren't
/// handled in `handle_login` at all" gap applied equally to
/// `handle_configuration`, which also had no arm for this before now, only
/// `handle_play` did.
fn decode_cookie_request(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let key = reader.string(32767).map_err(dec_err)?;
    let key = parse_key(&key, "cookie")?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::CookieRequested { key })])
}

/// Decodes an NBT text-component disconnect reason into plain text, falling back
/// to a generic message when the component carries no text.
fn nbt_reason_text(payload: &[u8]) -> Result<Text, AdapterError> {
    let mut reader = Reader::new(payload);
    let component =
        read_network_nbt(&mut reader).map_err(|err| AdapterError::Decode(err.to_string()))?;
    let reason = Text::from_nbt(&component);
    if reason.to_plain_string().is_empty() {
        Ok(Text::literal("Disconnected"))
    } else {
        Ok(reason)
    }
}

impl V770Adapter {
    /// Handles a clientbound packet while in the login state.
    pub(super) fn handle_login(
        &self,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == login::clientbound::LOGIN_COMPRESSION {
            let body: LoginCompression = decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        if packet_id == login::clientbound::LOGIN_FINISHED {
            // Validate the profile decodes, then acknowledge and advance.
            let _profile: LoginFinished = decode_body(payload)?;
            return Ok(vec![
                send(login::serverbound::LOGIN_ACKNOWLEDGED, &LoginAcknowledged)?,
                Directive::SetState(ConnectionState::Configuration),
                send(
                    configuration::serverbound::CLIENT_INFORMATION,
                    &ClientInformation::default(),
                )?,
            ]);
        }
        if packet_id == login::clientbound::HELLO {
            let request: EncryptionRequest = decode_body(payload)?;
            // Hand the driver the protocol-shaped crypto inputs; it performs the
            // key exchange and session auth and asks us back to frame the reply.
            return Ok(vec![Directive::BeginEncryption {
                server_id: request.server_id,
                public_key: request.public_key,
                verify_token: request.challenge,
                should_authenticate: request.should_authenticate,
            }]);
        }
        if packet_id == login::clientbound::LOGIN_DISCONNECT {
            let body: LoginDisconnect = decode_body(payload)?;
            // Login state predates the NBT text migration — `reason` is a JSON
            // chat component on the wire (see the packet's own field doc), not
            // NBT, so `nbt_reason_text` is the wrong helper here despite the
            // Play/Configuration arms using it. `Text::from_json` falls back to
            // a literal on a parse failure on its own, so a malformed or
            // legacy-plain reason still surfaces *something* rather than
            // erroring the disconnect away.
            return Ok(vec![Directive::Disconnect(Text::from_json(&body.reason))]);
        }
        if packet_id == login::clientbound::COOKIE_REQUEST {
            // This state had no arm at all before now, a
            // different code path from the Play-state one below.
            return decode_cookie_request(payload);
        }
        if packet_id == login::clientbound::CUSTOM_QUERY {
            // Zero decode existed for this at all before now. See
            // `decode_custom_query`'s own doc for why the reply is
            // unconditionally empty, matching vanilla's own client.
            return decode_custom_query(payload);
        }
        Ok(Vec::new())
    }
    /// Handles a clientbound packet while in the configuration state.
    pub(super) fn handle_configuration(
        &self,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == configuration::clientbound::SELECT_KNOWN_PACKS {
            return Ok(vec![send(
                configuration::serverbound::SELECT_KNOWN_PACKS,
                &ServerboundKnownPacks { packs: Vec::new() },
            )?]);
        }
        if packet_id == configuration::clientbound::REGISTRY_DATA {
            // This arm used to not exist, so 29 registries a join hit
            // the `Ok(Vec::new())` fall-through below and dimension heights, sky
            // light and the day clock were all hardcoded by level name instead.
            //
            // Decoded with a trailing-byte check, like every other packet here:
            // a registry we do not model still has to consume its payload
            // exactly, or a silently-wrong `Optional<Tag>` framing would go
            // unnoticed until the one registry we *do* read happened to sort
            // after it.
            let data: RegistryData = decode_full(payload)?;
            self.apply_registry_data(data);
            return Ok(Vec::new());
        }
        if packet_id == configuration::clientbound::FINISH_CONFIGURATION {
            tracing::debug!(
                target: "transfer",
                seq = super::xfer::next_seq(),
                "xfer: state -- FINISH_CONFIGURATION; returning to Play"
            );
            return Ok(vec![
                send(
                    configuration::serverbound::FINISH_CONFIGURATION,
                    &FinishConfiguration,
                )?,
                Directive::SetState(ConnectionState::Play),
            ]);
        }
        if packet_id == configuration::clientbound::KEEP_ALIVE {
            let keep_alive: KeepAlive = decode_body(payload)?;
            return Ok(vec![send(
                configuration::serverbound::KEEP_ALIVE,
                &keep_alive,
            )?]);
        }
        if packet_id == configuration::clientbound::PING {
            let ping: Pong = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::Ping { id: ping.id })]);
        }
        if packet_id == configuration::clientbound::UPDATE_TAGS {
            self.install_update_tags(payload)?;
            return Ok(Vec::new());
        }
        if packet_id == configuration::clientbound::COOKIE_REQUEST {
            // Also missing here, not just in `handle_login` — a fix
            // named Login and Play explicitly; Configuration had the
            // identical gap.
            return decode_cookie_request(payload);
        }
        if packet_id == configuration::clientbound::CUSTOM_PAYLOAD {
            // Only `handle_play` decoded this before now; a
            // server that sends plugin messages during Configuration (the
            // vanilla mod-handshake window, before `minecraft:brand` is even
            // announced by some servers) hit the fall-through below and lost
            // the message entirely.
            return decode_custom_payload(payload);
        }
        if packet_id == configuration::clientbound::RESOURCE_PACK_PUSH {
            // `handle_play` decoded this before now; vanilla
            // servers commonly push a required resource pack during
            // Configuration, before the client reaches Play, and the
            // fall-through below dropped it silently. Wire format is
            // identical in both states.
            let mut reader = Reader::new(payload);
            let id = reader.uuid().map_err(dec_err)?;
            let url = reader.string(32767).map_err(dec_err)?;
            let hash = reader.string(40).map_err(dec_err)?;
            let required = reader.bool().map_err(dec_err)?;
            let has_prompt = reader.bool().map_err(dec_err)?;
            let prompt = if has_prompt {
                let component = read_network_nbt(&mut reader).map_err(dec_err)?;
                Some(Text::from_nbt(&component))
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ResourcePackPushed {
                id,
                url,
                hash,
                required,
                prompt,
            })]);
        }
        if packet_id == configuration::clientbound::RESOURCE_PACK_POP {
            // Same story as `RESOURCE_PACK_PUSH` just above: the
            // pop for a pack pushed during Configuration never arrived at the
            // client before now.
            let mut reader = Reader::new(payload);
            let has_id = reader.bool().map_err(dec_err)?;
            let id = if has_id {
                Some(reader.uuid().map_err(dec_err)?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ResourcePackPopped { id })]);
        }
        if packet_id == configuration::clientbound::STORE_COOKIE {
            // Same gap as the two above, found by the same scan: the
            // configuration-phase `minecraft:store_cookie` fell through and was
            // lost, while `handle_play` decoded it. Format is state-independent.
            let mut reader = Reader::new(payload);
            let key = reader.string(32767).map_err(dec_err)?;
            let key = parse_key(&key, "cookie")?;
            let cookie_payload = reader.var_bytes(5120).map_err(dec_err)?.to_vec();
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CookieStored {
                key,
                payload: cookie_payload,
            })]);
        }
        if packet_id == configuration::clientbound::TRANSFER {
            // Same gap: `minecraft:transfer` is valid in Configuration (a
            // redirect during the handshake), but only `handle_play` decoded
            // it before now. Format is state-independent.
            let mut reader = Reader::new(payload);
            let host = reader.string(32767).map_err(dec_err)?;
            let port = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            tracing::debug!(
                target: "transfer",
                seq = super::xfer::next_seq(),
                host = %host,
                port,
                state = "Configuration",
                path = "reconnect",
                "xfer: state -- TRANSFER; the server is sending us to a new address, \
                 which is a different thing from a proxy backend swap: this ends the \
                 session and starts a new connection"
            );
            return Ok(vec![Directive::Emit(ClientEvent::TransferRequested {
                host,
                port,
            })]);
        }
        if packet_id == configuration::clientbound::CODE_OF_CONDUCT {
            return Ok(vec![send(
                configuration::serverbound::ACCEPT_CODE_OF_CONDUCT,
                &AcceptCodeOfConduct,
            )?]);
        }
        if packet_id == configuration::clientbound::DISCONNECT {
            return Ok(vec![Directive::Disconnect(nbt_reason_text(payload)?)]);
        }
        Ok(Vec::new())
    }
}

/// Decodes the custom-report-details packet: at most 32 `(title,
/// description)` string pairs, titles capped at 128 chars and descriptions at
/// 4096.
fn decode_custom_report_details(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count).map_err(|_| {
        AdapterError::Decode(format!("invalid custom_report_details count {count}"))
    })?;
    if count > 32 {
        return Err(AdapterError::Decode(format!(
            "custom_report_details carries {count} entries, over the wire's 32 limit"
        )));
    }
    let mut details = Vec::with_capacity(count);
    for _ in 0..count {
        let title = reader.string(128).map_err(dec_err)?;
        let description = reader.string(4096).map_err(dec_err)?;
        details.push((title, description));
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::CustomReportDetails {
        details,
    })])
}

/// Decodes vanilla's clientbound server-links packet.
///
/// Each entry is an either-codec over a known-link-type id and a text
/// component, then the URL, and **`true` selects the left arm, which is
/// the known-type id** — not the custom component. Getting that polarity
/// backwards produces a decode that succeeds
/// while reading a VarInt as the start of an NBT blob, which is why it is called
/// out here and gated in `tests/remaining_clientbound.rs`.
fn decode_server_links(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid server_links count {count}")))?;
    let mut links = Vec::with_capacity(count.min(256));
    for _ in 0..count {
        let kind = if reader.bool().map_err(dec_err)? {
            ServerLinkKind::Known(reader.var_i32().map_err(dec_err)?)
        } else {
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            ServerLinkKind::Custom(Text::from_nbt(&component))
        };
        let raw_url = reader.string(32767).map_err(dec_err)?;
        let url = lodestone_model::event::ServerLinkUrl::parse(&raw_url)
            .map_err(|error| AdapterError::Decode(format!("invalid server link URL: {error}")))?;
        links.push(ServerLink { kind, url });
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::ServerLinksReceived {
        links,
    })])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::TextColor;

    fn synchronized_tags(registries: &[(&str, &[(&str, &[i32])])]) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.var_i32(registries.len() as i32);
        for (registry, tags) in registries {
            writer.string(registry);
            writer.var_i32(tags.len() as i32);
            for (name, members) in *tags {
                writer.string(name);
                writer.var_i32(members.len() as i32);
                for member in *members {
                    writer.var_i32(*member);
                }
            }
        }
        writer.into_vec()
    }

    fn tagged_test_tool() -> ItemStack {
        let mut components = ItemComponents::default();
        components.tool = ToolPatch::Set(ItemTool::new(
            vec![ToolRule::new(
                ToolBlocks::Tag("test:fast".parse().unwrap()),
                Some(7.25),
                Some(true),
            )],
            1.5,
            1,
            true,
        ));
        ItemStack { item: "minecraft:stick".parse().unwrap(), count: 1, components }
    }

    fn tag_speed(adapter: &dyn VersionAdapter) -> f32 {
        // State 1 and block 1 are stone in the independent 26.2 registry report.
        adapter.tool_mining(Some(&tagged_test_tool()), 1).unwrap().speed
    }

    #[test]
    fn synchronized_tags_are_transactional_and_session_isolated() {
        let first: Arc<dyn VersionAdapter> = Arc::new(V770Adapter::default());
        let second: Arc<dyn VersionAdapter> = Arc::new(V770Adapter::default());
        let live_reader = Arc::clone(&first);
        let mut world = lodestone_world::World::default();
        let matching = synchronized_tags(&[("minecraft:block", &[("test:fast", &[1])])]);
        let nonmatching = synchronized_tags(&[("minecraft:block", &[("test:fast", &[0])])]);
        let apply = |adapter: &dyn VersionAdapter, world: &mut dyn WorldSink, payload: &[u8]| {
            adapter.handle_packet(world, ConnectionState::Configuration,
                configuration::clientbound::UPDATE_TAGS, payload)
        };
        assert_eq!(tag_speed(&*live_reader), 1.5);
        apply(&*first, &mut world, &matching).unwrap();
        apply(&*second, &mut world, &nonmatching).unwrap();
        assert_eq!(tag_speed(&*live_reader), 7.25);
        assert_eq!(tag_speed(&*second), 1.5);

        let mut trailing = nonmatching.clone();
        trailing.push(0);
        assert!(apply(&*first, &mut world, &trailing).is_err());
        assert_eq!(tag_speed(&*live_reader), 7.25);
        assert_eq!(tag_speed(&*second), 1.5);

        let invalid_later_registry = synchronized_tags(&[
            ("minecraft:block", &[("test:fast", &[0])]),
            ("minecraft:item", &[("test:invalid", &[-1])]),
        ]);
        assert!(apply(&*first, &mut world, &invalid_later_registry).is_err());
        assert_eq!(tag_speed(&*live_reader), 7.25);
        apply(&*first, &mut world, &nonmatching).unwrap();
        assert_eq!(tag_speed(&*live_reader), 1.5);
        apply(&*second, &mut world, &matching).unwrap();
        assert_eq!(tag_speed(&*first), 1.5);
        assert_eq!(tag_speed(&*second), 7.25);
    }

    #[test]
    fn synchronized_tags_validate_members_duplicates_and_complete_replacement() {
        let adapter = V770Adapter::default();
        let matching = synchronized_tags(&[("minecraft:block", &[("test:fast", &[1, 1])])]);
        adapter.install_update_tags(&matching).unwrap();
        assert_eq!(tag_speed(&adapter), 7.25);
        for malformed in [
            synchronized_tags(&[("minecraft:block", &[("test:fast", &[i32::MAX])])]),
            synchronized_tags(&[("minecraft:block", &[("test:fast", &[-1])])]),
            synchronized_tags(&[("minecraft:block", &[("test:fast", &[0]), ("test:fast", &[1])])]),
            synchronized_tags(&[("minecraft:block", &[]), ("block", &[])]),
            vec![0xff, 0xff, 0xff, 0xff, 0x07],
        ] {
            assert!(adapter.install_update_tags(&malformed).is_err());
            assert_eq!(tag_speed(&adapter), 7.25);
        }
        adapter.install_update_tags(&synchronized_tags(&[("minecraft:item", &[])])).unwrap();
        assert_eq!(tag_speed(&adapter), 7.25);
        adapter.install_update_tags(&synchronized_tags(&[("minecraft:block", &[])])).unwrap();
        assert_eq!(tag_speed(&adapter), 1.5);
    }

    #[test]
    fn synchronized_tag_members_use_the_selected_wire_registry() {
        use lodestone_data::{GameDataVersion, block::Block};
        // Independent reports assign wire block 51 to birch in 26.2 and oak in 26.3.
        let payload = synchronized_tags(&[("minecraft:block", &[("test:wood", &[51])])]);
        let old = decode_update_tags(&payload, &ProtocolDialect::v26_2()).unwrap().unwrap();
        let latest = decode_update_tags(&payload,
            &ProtocolDialect::v26_2().with_game_data_version(GameDataVersion::V26_3))
            .unwrap().unwrap();
        assert!(old.contains("test:wood", Block::BirchLog));
        assert!(!old.contains("test:wood", Block::OakLog));
        assert!(latest.contains("test:wood", Block::OakLog));
        assert!(!latest.contains("test:wood", Block::BirchLog));
    }

    /// A length-prefixed string, exactly how `login::clientbound::
    /// LOGIN_DISCONNECT`'s body is framed (see `nbt_reason_text`'s sibling
    /// arm and `crates/versions/26.2/tests/server_disconnect.rs`'s own
    /// `login_phase_reason_is_json_and_play_phase_reason_is_nbt`).
    fn framed_string(s: &str) -> Vec<u8> {
        let mut w = Writer::default();
        w.string(s);
        w.into_vec()
    }

    /// **The owner-reported bug, reproduced from a real captured shape.**
    /// A server refusing a login (banned/whitelisted/wrong version) sends a
    /// styled component — `extra` children, a root `color`, a `bold` run —
    /// as a **JSON** chat component (vanilla's own login-disconnect packet
    /// uses a lenient-JSON stream codec there), not NBT. Before this fix the
    /// login-state arm ran the raw JSON straight through `Text::literal`, so
    /// a player saw the brace-and-quote source text instead of a message —
    /// exactly the report: `{"extra":... with colour, bold, etc.}` on
    /// screen. This asserts the *parsed* result, not merely that it stopped
    /// containing braces: the plain text, the `extra` child's own colour,
    /// and the root's `bold` flag must all survive.
    #[test]
    fn login_disconnect_parses_the_real_json_shape_including_extra_and_styles() {
        let json = r#"{
            "text": "You are not white-listed on this server!",
            "bold": true,
            "extra": [
                {"text": " Contact an admin.", "color": "red", "bold": false}
            ]
        }"#;
        let payload = framed_string(json);

        let adapter = V770Adapter::default();
        let directives = adapter
            .handle_login(login::clientbound::LOGIN_DISCONNECT, &payload)
            .expect("a real captured login_disconnect JSON shape must decode");
        let Some(Directive::Disconnect(text)) = directives.into_iter().next() else {
            panic!("expected a Disconnect directive");
        };

        assert_eq!(
            text.to_plain_string(),
            "You are not white-listed on this server! Contact an admin.",
            "the root text and the extra child's text must both survive, \
             concatenated — not the raw JSON source",
        );
        assert_eq!(
            text.style.bold,
            Some(true),
            "the root component's bold flag must survive",
        );
        assert_eq!(
            text.extra.len(),
            1,
            "the styled `extra` child must survive as its own node, not be \
             flattened away",
        );
        assert_eq!(
            text.extra[0].style.color,
            Some(TextColor::Red),
            "the extra child's own colour must survive",
        );
        assert_eq!(
            text.extra[0].style.bold,
            Some(false),
            "the extra child's explicit bold=false must survive rather than \
             inheriting the root's bold=true",
        );

        // The control this fix replaces: the *old* behaviour (`Text::literal`
        // on the raw JSON) would have put the brace-laden source verbatim
        // into the plain string. Watch it fail under the old code path.
        let literal = Text::literal(json);
        assert_ne!(
            literal.to_plain_string(),
            text.to_plain_string(),
            "sanity: a literal wrap of the raw JSON must differ from the \
             parsed result, or this test cannot discriminate the fix from \
             the bug it replaces",
        );
    }

    /// A malformed/legacy-plain reason must still show *something* — the
    /// `Text::from_json` fallback this fix relies on, exercised through the
    /// real login arm rather than asserted only at the `Text` layer.
    #[test]
    fn login_disconnect_falls_back_to_literal_on_malformed_json() {
        let payload = framed_string("not actually json");
        let adapter = V770Adapter::default();
        let directives = adapter
            .handle_login(login::clientbound::LOGIN_DISCONNECT, &payload)
            .expect("a malformed reason must still decode, not error the disconnect away");
        let Some(Directive::Disconnect(text)) = directives.into_iter().next() else {
            panic!("expected a Disconnect directive");
        };
        assert_eq!(text.to_plain_string(), "not actually json");
    }

    /// The negative control the coordinator asked for: the configuration-state
    /// arm shares vanilla's clientbound disconnect packet with Play (NBT, not
    /// JSON) — its trusted, context-free stream codec there, confirmed
    /// against the decompiled 26.2 client source, versus login's own
    /// lenient-JSON stream codec. A JSON payload fed to
    /// the configuration arm must NOT decode as if it were NBT.
    #[test]
    fn configuration_disconnect_rejects_a_json_payload_as_not_nbt() {
        let json = r#"{"text":"nope"}"#;
        let payload = framed_string(json);
        let adapter = V770Adapter::default();
        let result = adapter.handle_configuration(configuration::clientbound::DISCONNECT, &payload);
        assert!(
            result.is_err(),
            "a JSON-framed string is not valid network NBT; the configuration \
             arm must reject it rather than silently misparsing it, which is \
             the tell that it is still using `nbt_reason_text` and not the \
             login arm's JSON path",
        );
    }
}
