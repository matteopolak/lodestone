//! Chat and command-tree packets: signed chat, the last-seen signature
//! cache, and the wire-shaped command tree/suggestions decode. Split out of
//! the former monolithic `adapter.rs`.
use super::*;

impl V770Adapter {
    /// Clientbound play-state packets in the chat domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_chat(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::PLAYER_CHAT {
            let mut reader = Reader::new(payload);
            let global_index = reader.var_i32().map_err(dec_err)?;
            let sender = reader.uuid().map_err(dec_err)?;
            // The signed-message-link index — this message's position in the
            // sender's signing chain. Used to be discarded (`let _index`);
            // now carried through `ChatAckInfo::message_index` so the client
            // driver can reconstruct the exact link a signature verification
            // has to hash (issue #283).
            let message_index = reader.var_i32().map_err(dec_err)?;
            let signature = if reader.bool().map_err(dec_err)? {
                reader.bytes(256).map_err(dec_err)?.to_vec()
            } else {
                Vec::new()
            };
            // Vanilla's packed signed-message body: raw content, timestamp,
            // salt, last-seen.
            let content = reader.string(256).map_err(dec_err)?;
            // Epoch **milliseconds** on the wire — see `ChatAckInfo::timestamp_millis`'s
            // own doc for why this used to be discarded (`let _timestamp`) and
            // is now carried verbatim rather than pre-converted.
            let timestamp_millis = reader.i64().map_err(dec_err)?;
            let salt = reader.i64().map_err(dec_err)?;
            // Resolve the packed last-seen list against the signature cache and
            // push it — plus this message's own signature — back in, mirroring
            // vanilla's own signed-chat handler. The push keeps the client's cache id
            // space aligned with the server's; pushing only the *new* signatures
            // would drift every subsequent cache id, so the complete resolved
            // list is pushed.
            let Some(last_seen) = self.read_last_seen_packed(&mut reader)? else {
                // An unresolvable cache reference is a benign desync; drop this
                // message rather than fail the connection.
                return Ok(vec![]);
            };
            let own = (!signature.is_empty()).then(|| MessageSignature::new(signature.clone()));
            if let Ok(mut cache) = self.chat_cache.lock() {
                cache.push(&last_seen, own.as_ref());
            }
            let unsigned = if reader.bool().map_err(dec_err)? {
                Some(read_network_nbt(&mut reader).map_err(dec_err)?)
            } else {
                None
            };
            let was_shown = read_filter_mask(&mut reader)?;
            read_chat_type_bound(&mut reader)?;
            reader.ensure_empty().map_err(dec_err)?;
            // The server-decorated form (if any) is preferred for display; a
            // plain signed message carries only its raw content. The decorated
            // component keeps its colour/style tree; the raw content is a bare
            // string.
            //
            // `raw_content` below keeps a copy of the *signed* string
            // regardless of which arm wins: a signature is always hashed over
            // this exact text, never over the server's decorated form, so a
            // verifier needs it even when `text` came from `unsigned`.
            let text = match unsigned {
                Some(component) => Text::from_nbt(&component),
                None => Text::literal(content.clone()),
            };
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text,
                kind: ChatKind::Chat,
                // `PLAYER_CHAT` is the one chat format whose wire
                // carries the sender's profile UUID — this is what the Social
                // Interactions Hide-in-Chat filter keys on.
                sender: Some(sender),
                ack: Some(ChatAckInfo {
                    signature,
                    global_index,
                    was_shown,
                    message_index,
                    timestamp_millis,
                    salt,
                    raw_content: content,
                    last_seen: last_seen.iter().map(|sig| sig.as_bytes().to_vec()).collect(),
                    // Fail-closed — see `ChatAckInfo::verified`'s own doc.
                    // Only `lodestone_client::driver`'s `emit` can raise this,
                    // since only it holds the sender's public key.
                    verified: false,
                }),
            })]);
        }
        if packet_id == play::clientbound::DISGUISED_CHAT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            read_chat_type_bound(&mut reader)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_nbt(&component),
                kind: ChatKind::Chat,
                // Disguised chat is server-decorated and unsigned; it carries
                // no profile UUID on the wire, so nothing to filter on.
                sender: None,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::SYSTEM_CHAT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let overlay = reader
                .bool()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let kind = if overlay {
                ChatKind::GameInfo
            } else {
                ChatKind::System
            };
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_nbt(&component),
                kind,
                sender: None,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::COMMANDS {
            let tree = decode_command_tree(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::CommandTreeUpdated {
                tree: Box::new(tree),
            })]);
        }
        if packet_id == play::clientbound::COMMAND_SUGGESTIONS {
            let response = decode_command_suggestions(payload)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::CommandSuggestionsReceived {
                    id: response.id,
                    start: response.start,
                    length: response.length,
                    suggestions: response.suggestions,
                },
            )]);
        }
        if packet_id == play::clientbound::DELETE_CHAT {
            // Vanilla's packed message signature: a VarInt `id + 1`; `0` is followed by
            // a full 256-byte signature, any other value is `id - 1` into the
            // last-seen signature cache. Cached references are resolved here
            // against the connection's `chat_cache`; one that cannot be
            // resolved is a benign cache desync and the event is dropped rather
            // than disconnecting (see the `ChatMessageDeleted` route in
            // `lodestone_model::event`).
            let mut reader = Reader::new(payload);
            let id_plus_one = reader.var_i32().map_err(dec_err)?;
            let signature = if id_plus_one == 0 {
                PackedMessageSignature::Full(reader.bytes(256).map_err(dec_err)?.to_vec())
            } else {
                match self.resolve_cached_signature(id_plus_one - 1) {
                    Some(signature) => {
                        PackedMessageSignature::Full(signature.as_bytes().to_vec())
                    }
                    None => {
                        tracing::warn!(
                            id = id_plus_one - 1,
                            "delete_chat references an unknown cached signature; dropping"
                        );
                        return Ok(vec![]);
                    }
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ChatMessageDeleted {
                signature,
            })]);
        }
        if packet_id == play::clientbound::CUSTOM_CHAT_COMPLETIONS {
            let mut reader = Reader::new(payload);
            let action = match reader.var_i32().map_err(dec_err)? {
                0 => ChatCompletionsAction::Add,
                1 => ChatCompletionsAction::Remove,
                2 => ChatCompletionsAction::Set,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown custom_chat_completions action {other}"
                    )));
                }
            };
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid chat completion count {count}"))
            })?;
            let mut entries = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                entries.push(reader.string(32767).map_err(dec_err)?);
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ChatCompletionsChanged {
                action,
                entries,
            })]);
        }
        Ok(Vec::new())
    }
}

/// Consumes a packed `LastSeenMessages` collection: a VarInt count (capped at
/// 20 by vanilla) then that many packed message signatures. Each packed
/// signature is a VarInt: `0` is followed by a full 256-byte signature (a
/// newly-seen message), and any positive value references a cached signature by
/// index and carries no further bytes.
///
impl V770Adapter {
    /// Returns the resolved signature list, or `None` when a cache reference cannot
    /// be resolved — a benign desync the caller drops rather than dying on (the
    /// connection must survive a reference we never pushed; the next signed body's
    /// push re-syncs the id space).
    fn read_last_seen_packed(
    &self,
    reader: &mut Reader<'_>,
) -> Result<Option<Vec<MessageSignature>>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid last-seen count {count}")))?;
    // Same cap as `decode_vec` and the command-tree readers below: `count`
    // comes off the wire and each entry costs at least one byte, so
    // `remaining()` is a sound ceiling on how many can exist.
    let mut last_seen = Vec::with_capacity(count.min(reader.remaining()));
    for _ in 0..count {
        let raw = reader.var_i32().map_err(dec_err)?;
        if raw == 0 {
            last_seen.push(MessageSignature::new(
                reader.bytes(256).map_err(dec_err)?.to_vec(),
            ));
        } else {
            match self.resolve_cached_signature(raw - 1) {
                Some(signature) => last_seen.push(signature),
                None => {
                    tracing::warn!(
                        id = raw - 1,
                        "last-seen references an unknown cached signature; dropping chat message"
                    );
                    return Ok(None);
                }
            }
        }
    }
    Ok(Some(last_seen))
}

/// Resolves a packed signature-cache index against this connection's
/// [`MessageSignatureCache`]. Returns `None` when the id is out of range or the
/// cache lock is poisoned — both benign desyncs the caller drops rather than
/// dying on.
fn resolve_cached_signature(&self, id: i32) -> Option<MessageSignature> {
    let id = usize::try_from(id).ok()?;
    self.chat_cache.lock().ok()?.unpack(id).cloned()
    }
}

/// Reads a `FilterMask` and returns whether the message is shown to the player.
///
/// Ordinal: `0` = pass-through (shown), `1` = fully filtered (hidden), `2` =
/// partially filtered (shown) followed by a `BitSet` of filtered word indices
/// (a VarInt long-count then that many `i64` words).
fn read_filter_mask(reader: &mut Reader<'_>) -> Result<bool, AdapterError> {
    let ordinal = reader.var_i32().map_err(dec_err)?;
    match ordinal {
        0 => Ok(true),
        1 => Ok(false),
        2 => {
            let words = reader.var_i32().map_err(dec_err)?;
            let words = usize::try_from(words).map_err(|_| {
                AdapterError::Decode(format!("invalid filter mask bitset length {words}"))
            })?;
            for _ in 0..words {
                reader.i64().map_err(dec_err)?;
            }
            Ok(true)
        }
        other => Err(AdapterError::Decode(format!(
            "invalid filter mask ordinal {other}"
        ))),
    }
}

/// Consumes a `vanilla's own chat type's own bound`: a `Holder<ChatType>`, a trusted NBT name
/// component, and an optional trusted NBT target-name component.
///
/// The holder is a VarInt: `0` would introduce an inline chat-type definition
/// (decoration plus style), which vanilla servers never send in chat packets
/// and which Phase 1 does not model; any positive value references the
/// `minecraft:chat_type` registry at index `value - 1` and carries no further
/// bytes. An inline holder fails loudly rather than misparsing the rest of the
/// stream.
fn read_chat_type_bound(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    if reader.var_i32().map_err(dec_err)? == 0 {
        return Err(AdapterError::Decode(
            "inline chat_type definitions are not supported".to_owned(),
        ));
    }
    read_network_nbt(reader).map_err(dec_err)?;
    if reader.bool().map_err(dec_err)? {
        read_network_nbt(reader).map_err(dec_err)?;
    }
    Ok(())
}

/// The clientbound command-tree packet's own node-flag bits, confirmed
/// against the decompiled 26.2 client source.
mod command_node_flags {
    /// `MASK_TYPE`: the low two bits select root / literal / argument.
    pub(super) const MASK_TYPE: u8 = 3;
    /// `TYPE_ROOT`.
    pub(super) const TYPE_ROOT: u8 = 0;
    /// `TYPE_LITERAL`.
    pub(super) const TYPE_LITERAL: u8 = 1;
    /// `TYPE_ARGUMENT`.
    pub(super) const TYPE_ARGUMENT: u8 = 2;
    /// `FLAG_EXECUTABLE`.
    pub(super) const EXECUTABLE: u8 = 4;
    /// `FLAG_REDIRECT`.
    pub(super) const REDIRECT: u8 = 8;
    /// `FLAG_CUSTOM_SUGGESTIONS`.
    pub(super) const CUSTOM_SUGGESTIONS: u8 = 16;
    /// `FLAG_RESTRICTED`.
    pub(super) const RESTRICTED: u8 = 32;
}

/// A defensive cap on the node and child counts a single `commands` packet may
/// declare, so a hostile or corrupt VarInt cannot drive a multi-gigabyte
/// `Vec::with_capacity` before the reader runs out of bytes. Vanilla 26.2's own
/// tree is ~1.2k nodes; four times the payload's byte length is a bound no
/// legitimate tree can exceed, since every node costs at least two bytes on the
/// wire.
fn command_count(reader: &mut Reader<'_>, payload_len: usize, what: &str) -> Result<usize, AdapterError> {
    let raw = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(raw)
        .map_err(|_| AdapterError::Decode(format!("negative {what} count {raw}")))?;
    if count > payload_len {
        return Err(AdapterError::Decode(format!(
            "{what} count {count} exceeds the {payload_len}-byte payload"
        )));
    }
    Ok(count)
}

/// Reads one `minecraft:command_argument_type` payload into an
/// [`ArgumentParser`], given the registry id already read off the wire.
///
/// Every branch mirrors that parser's own `ArgumentTypeInfo::deserializeFromNetwork`
/// — see `lodestone_model::command_tree`'s module doc for the file list. Ids
/// with no branch here use vanilla's single-instance argument-info kind,
/// whose network deserializer consumes nothing, so falling through to
/// [`ArgumentParser::from_registry_id_no_payload`] reads zero bytes and is
/// correct rather than a guess.
///
/// Returns `None` for an id this build doesn't model, having consumed no
/// payload for it. **This is a deliberate, documented divergence from vanilla**:
/// vanilla's own command-tree packet reader bails out of the *whole* node
/// the moment the argument-type registry lookup by id returns nothing,
/// without reading the payload or the custom-suggestions id, which leaves its own
/// reader mid-node and corrupts every subsequent entry. Assuming "no payload"
/// keeps the stream in sync for the 44-of-57 ids that genuinely have none, so a
/// datapack or mod argument type we don't model costs one unusable node instead
/// of the entire tree. See `lodestone_model::command_tree`'s doc on why that
/// tolerance is load-bearing.
fn read_argument_parser(
    reader: &mut Reader<'_>,
    parser_id: i32,
) -> Result<Option<ArgumentParser>, AdapterError> {
    // Vanilla's number-argument flag helpers (has-min / has-max): bit 0 and
    // bit 1 of a leading flags byte, an absent bound meaning the type's own
    // extreme.
    const HAS_MIN: u8 = 1;
    const HAS_MAX: u8 = 2;

    let parser = match parser_id {
        1 => {
            let flags = reader.u8().map_err(dec_err)?;
            ArgumentParser::Float {
                min: if flags & HAS_MIN != 0 {
                    reader.f32().map_err(dec_err)?
                } else {
                    -f32::MAX
                },
                max: if flags & HAS_MAX != 0 {
                    reader.f32().map_err(dec_err)?
                } else {
                    f32::MAX
                },
            }
        }
        2 => {
            let flags = reader.u8().map_err(dec_err)?;
            ArgumentParser::Double {
                min: if flags & HAS_MIN != 0 {
                    reader.f64().map_err(dec_err)?
                } else {
                    -f64::MAX
                },
                max: if flags & HAS_MAX != 0 {
                    reader.f64().map_err(dec_err)?
                } else {
                    f64::MAX
                },
            }
        }
        3 => {
            let flags = reader.u8().map_err(dec_err)?;
            ArgumentParser::Integer {
                min: if flags & HAS_MIN != 0 {
                    reader.i32().map_err(dec_err)?
                } else {
                    i32::MIN
                },
                max: if flags & HAS_MAX != 0 {
                    reader.i32().map_err(dec_err)?
                } else {
                    i32::MAX
                },
            }
        }
        4 => {
            let flags = reader.u8().map_err(dec_err)?;
            ArgumentParser::Long {
                min: if flags & HAS_MIN != 0 {
                    reader.i64().map_err(dec_err)?
                } else {
                    i64::MIN
                },
                max: if flags & HAS_MAX != 0 {
                    reader.i64().map_err(dec_err)?
                } else {
                    i64::MAX
                },
            }
        }
        // `StringArgumentSerializer`: `writeEnum` is a VarInt ordinal into
        // Brigadier's `StringType`.
        5 => {
            let ordinal = reader.var_i32().map_err(dec_err)?;
            let kind = match ordinal {
                0 => StringKind::SingleWord,
                1 => StringKind::QuotablePhrase,
                2 => StringKind::GreedyPhrase,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "brigadier:string ordinal {other} is outside StringType"
                    )));
                }
            };
            ArgumentParser::String(kind)
        }
        // `vanilla's own entity argument's own info`: bit 0 `single`, bit 1 `playersOnly`.
        6 => {
            let flags = reader.u8().map_err(dec_err)?;
            ArgumentParser::Entity {
                single: flags & 1 != 0,
                players_only: flags & 2 != 0,
            }
        }
        // `vanilla's own score holder argument's own info`: bit 0 `multiple`.
        31 => {
            let flags = reader.u8().map_err(dec_err)?;
            ArgumentParser::ScoreHolder {
                multiple: flags & 1 != 0,
            }
        }
        // The time-argument type: a plain big-endian `int`, no flags byte.
        43 => ArgumentParser::Time {
            min: reader.i32().map_err(dec_err)?,
        },
        // The five `resource*` parsers: vanilla's registry-key reader, which is
        // just its identifier reader — a VarInt-length UTF-8 string, not a
        // namespace/path pair.
        44..=48 => {
            let raw = reader.string(32767).map_err(dec_err)?;
            let registry = parse_key(&raw, "command argument registry")?;
            match parser_id {
                44 => ArgumentParser::ResourceOrTag { registry },
                45 => ArgumentParser::ResourceOrTagKey { registry },
                46 => ArgumentParser::Resource { registry },
                47 => ArgumentParser::ResourceKeyArg { registry },
                _ => ArgumentParser::ResourceSelector { registry },
            }
        }
        other => match ArgumentParser::from_registry_id_no_payload(other) {
            ArgumentParser::Unknown(_) => return Ok(None),
            known => known,
        },
    };
    Ok(Some(parser))
}

/// Reads one `vanilla's own clientbound commands packet's own entry`: `readNode`'s exact order —
/// flags byte, VarInt child-index array, the redirect index when
/// `FLAG_REDIRECT` is set, then the type-dependent stub.
fn read_command_node(
    reader: &mut Reader<'_>,
    payload_len: usize,
) -> Result<RawCommandNode, AdapterError> {
    use command_node_flags as flag;

    let flags = reader.u8().map_err(dec_err)?;
    let child_count = command_count(reader, payload_len, "command node child")?;
    let mut children = Vec::with_capacity(child_count);
    for _ in 0..child_count {
        let raw = reader.var_i32().map_err(dec_err)?;
        children.push(
            usize::try_from(raw)
                .map_err(|_| AdapterError::Decode(format!("negative child index {raw}")))?,
        );
    }
    let redirect = if flags & flag::REDIRECT != 0 {
        let raw = reader.var_i32().map_err(dec_err)?;
        Some(
            usize::try_from(raw)
                .map_err(|_| AdapterError::Decode(format!("negative redirect index {raw}")))?,
        )
    } else {
        None
    };

    let kind = match flags & flag::MASK_TYPE {
        flag::TYPE_ROOT => NodeKind::Root,
        flag::TYPE_LITERAL => NodeKind::Literal {
            name: reader.string(32767).map_err(dec_err)?,
        },
        flag::TYPE_ARGUMENT => {
            let name = reader.string(32767).map_err(dec_err)?;
            let parser_id = reader.var_i32().map_err(dec_err)?;
            match read_argument_parser(reader, parser_id)? {
                Some(parser) => {
                    // Read *after* the parser payload, exactly as vanilla's own
                    // argument-node writer emits it.
                    let suggestions = if flags & flag::CUSTOM_SUGGESTIONS != 0 {
                        let raw = reader.string(32767).map_err(dec_err)?;
                        Some(parse_key(&raw, "command suggestions provider")?)
                    } else {
                        None
                    };
                    NodeKind::Argument {
                        name,
                        parser,
                        suggestions,
                    }
                }
                None => {
                    // Unmodeled parser: still consume the custom-suggestions id
                    // if the flag claims one, so the reader stays aligned for
                    // the next entry. See `read_argument_parser`'s doc.
                    if flags & flag::CUSTOM_SUGGESTIONS != 0 {
                        reader.string(32767).map_err(dec_err)?;
                    }
                    NodeKind::Unrecognized { parser_id }
                }
            }
        }
        other => {
            return Err(AdapterError::Decode(format!(
                "command node type {other} is outside TYPE_ROOT/LITERAL/ARGUMENT"
            )));
        }
    };

    Ok(RawCommandNode {
        kind,
        executable: flags & flag::EXECUTABLE != 0,
        restricted: flags & flag::RESTRICTED != 0,
        redirect,
        children,
    })
}

/// Decodes a whole `minecraft:commands` payload (clientbound id 16) into a
/// [`CommandTree`].
///
/// `ClientboundCommandsPacket`'s private constructor is
/// `readList(::readNode)` then `readVarInt()` for the root index — the node
/// list comes **first**, the root index last.
fn decode_command_tree(payload: &[u8]) -> Result<CommandTree, AdapterError> {
    let mut reader = Reader::new(payload);
    let node_count = command_count(&mut reader, payload.len(), "command node")?;
    let mut nodes = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        nodes.push(read_command_node(&mut reader, payload.len())?);
    }
    let raw_root = reader.var_i32().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    let root = usize::try_from(raw_root)
        .map_err(|_| AdapterError::Decode(format!("negative root index {raw_root}")))?;
    CommandTree::new(nodes, root).map_err(|err| AdapterError::Decode(err.to_string()))
}

/// Decodes a `minecraft:command_suggestions` payload (clientbound id 15) into a
/// [`CommandSuggestionsResponse`].
///
/// `vanilla's own clientbound command suggestions packet's own stream codec`: three VarInts (`id`,
/// `start`, `length`) then a list of `Entry(String text, Optional<Component>
/// tooltip)`. The tooltip uses `TRUSTED_OPTIONAL_STREAM_CODEC` — a `bool`
/// presence byte followed by a network-NBT component when set — and is kept
/// as a real [`Text`] via `Text::from_nbt`, the same route every other styled
/// component in this file uses, matching [`CommandSuggestionEntry::tooltip`]'s
/// own doc: a plain-text flatten here would silently drop a hex colour
/// (`TextColor::Rgb`) a tooltip can legitimately carry.
fn decode_command_suggestions(payload: &[u8]) -> Result<CommandSuggestionsResponse, AdapterError> {
    let mut reader = Reader::new(payload);
    let id = reader.var_i32().map_err(dec_err)?;
    let start = reader.var_i32().map_err(dec_err)?;
    let length = reader.var_i32().map_err(dec_err)?;
    let count = command_count(&mut reader, payload.len(), "command suggestion")?;
    let mut suggestions = Vec::with_capacity(count);
    for _ in 0..count {
        let text = reader.string(32767).map_err(dec_err)?;
        let tooltip = if reader.bool().map_err(dec_err)? {
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            Some(Text::from_nbt(&component))
        } else {
            None
        };
        suggestions.push(CommandSuggestionEntry { text, tooltip });
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(CommandSuggestionsResponse {
        id,
        start,
        length,
        suggestions,
    })
}

