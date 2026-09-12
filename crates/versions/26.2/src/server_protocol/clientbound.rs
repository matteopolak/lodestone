//! Clientbound phase packet encoders.
//!
//! This private module is part of the V770ServerProtocol facade. Its
//! re-exported helpers preserve the existing public API and wire behaviour.

use super::*;

/// Hand-written encoder for the clientbound `player_position` (teleport)
/// packet, which has no existing struct in `packets::game` because it is
/// currently only ever *decoded* (see `V770Adapter::handle_player_position`).
///
/// Wire layout (mirrors the decode side exactly): VarInt teleport id, position
/// `f64`×3, delta-movement `f64`×3 (zero — an absolute teleport carries no
/// velocity), yaw/pitch `f32`, then a big-endian `i32` relative-flags bit set
/// (`0` — every field here is absolute).
pub(super) fn encode_player_position_teleport(
    id: i32,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(id);
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.f32(yaw);
    w.f32(pitch);
    w.i32(0);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `set_chunk_cache_center` packet:
/// two VarInt chunk coordinates, no other fields.
pub(super) fn encode_chunk_cache_center_body(cx: i32, cz: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(cx);
    w.var_i32(cz);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `forget_level_chunk` packet: a
/// single packed `i64` — `x` in the low 32 bits, `z` in the high 32 — mirroring
/// vanilla's vanilla's own chunk-position type's own pack exactly as `V770Adapter::handle_play`'s
/// `FORGET_LEVEL_CHUNK` decode arm already reads it (`adapter/chunk.rs`, the
/// `packed as i32` / `(packed >> 32) as i32` pair).
pub(super) fn encode_forget_chunk_body(cx: i32, cz: i32) -> Vec<u8> {
    let packed = (i64::from(cx) & 0xFFFF_FFFF) | ((i64::from(cz) & 0xFFFF_FFFF) << 32);
    let mut w = Writer::default();
    w.i64(packed);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `block_update` packet: a packed
/// `BlockPos` long ([`pack_block_pos`]) followed by a VarInt block-state
/// registry id — mirrors vanilla's own clientbound block-update packet's own stream codec
/// (vanilla's own block-position type's own stream codec composed with vanilla's own codec library's own id mapper(Block
/// .BLOCK_STATE_REGISTRY)`, vanilla's own clientbound block-update packet's own stream codec) and
/// this crate's own decode of the same packet in `V770Adapter::handle_play`'s
/// `BLOCK_UPDATE` arm (`adapter/chunk.rs`), which reads the identical
/// packed-i64-then-VarInt shape.
pub(super) fn encode_block_update_body(x: i32, y: i32, z: i32, state_id: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(x, y, z));
    w.var_i32(state_id as i32);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `game_event` packet (wire id 38),
/// the small keyed world-state channel vanilla uses for weather transitions.
/// Wire layout: an unsigned byte event id, then a big-endian `f32` param —
/// exactly `ClientboundGameEventPacket`'s `writeByte(event) + writeFloat(param)`
/// (vanilla's own clientbound game-event packet's own stream codec), and exactly the shape
/// `packets::game::GameEvent`'s `Decode` impl reads back on this crate's own
/// client side (`V770Adapter`'s `GAME_EVENT` arm, `adapter/chunk.rs`).
pub(super) fn game_event_body(kind: u8, value: f32) -> Vec<u8> {
    let mut w = Writer::default();
    w.u8(kind);
    w.f32(value);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `set_time` packet, mirroring
/// `packets::time::SetTime`'s `Decode` impl exactly (that struct has no
/// `Encode` impl to reuse — the map-valued field it decodes cannot come from
/// an existing bidirectional struct the way this module's other reused types
/// do, so a hand-written mirror is the documented fallback here, same as
/// `encode_player_position_teleport`/`encode_chunk_cache_center_body` above).
///
/// Wire layout: `i64` `game_time`, then a VarInt-counted list of clock
/// updates. `day_time` of `None` sends an empty list (vanilla's
/// once-a-second `forceGameTimeSynchronization` broadcast, which
/// deliberately leaves the client's held day/night anchor untouched — see
/// `packets::time::SetTime::day_clock`'s doc comment). `day_time` of
/// `Some(total_ticks)` sends exactly one update, anchoring the overworld
/// clock (`OVERWORLD_CLOCK_HOLDER_ID`) to `total_ticks` at the normal 1:1
/// rate: a **plain** VarInt holder id (no `+1`/inline convention — see
/// `packets::time::ClockUpdate::holder_id`'s doc comment on why that
/// differs from the *other* holder codec), a VarLong tick count, then two
/// big-endian `f32`s (partial tick `0.0`, rate `1.0`).
pub(super) fn encode_set_time_body(game_time: i64, day_time: Option<i64>) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(game_time);
    match day_time {
        Some(total_ticks) => {
            w.var_i32(1);
            w.var_i32(OVERWORLD_CLOCK_HOLDER_ID);
            w.var_i64(total_ticks);
            w.f32(0.0); // partial_tick
            w.f32(1.0); // rate: normal day/night speed, never paused
        }
        None => {
            w.var_i32(0);
        }
    }
    w.into_vec()
}

/// The `minecraft:command_argument_type` registry id of one parser, plus that
/// parser's own network payload, written to `w`.
///
/// # Where the ids come from
///
/// `.cache/mc/26.2/generated/reports/registries.json`'s
/// `minecraft:command_argument_type` block, whose `protocol_id`s are `0..=56` in
/// exactly this order. Mojang's generator, not a community table and not this
/// crate's own decoder — which matters, because writing the encoder from
/// `V770Adapter`'s `read_argument_parser` alone would inherit any mistake that
/// already had. The two agree; that agreement is now evidence rather than an
/// assumption.
///
/// # Payloads, each from its own `ArgumentTypeInfo::serializeToNetwork`
///
/// | parsers | payload |
/// |---|---|
/// | the four Brigadier numerics | vanilla's own argument-utils helper's own create number flags byte (bit 0 min, bit 1 max) then only the **present** bounds |
/// | `brigadier:string` | `writeEnum`, i.e. a VarInt `StringType` ordinal |
/// | `minecraft:entity` | one flags byte, bit 0 `single`, bit 1 `playersOnly` |
/// | `minecraft:score_holder` | one flags byte, bit 0 `multiple` |
/// | `minecraft:time` | a bare big-endian `int` minimum — **no** flags byte |
/// | the five `resource*` | `writeResourceKey` → `writeIdentifier`, one VarInt-length UTF-8 string |
/// | everything else | nothing at all (`SingletonArgumentInfo::serializeToNetwork` is empty) |
///
/// A bound is *absent* exactly when it equals its type's sentinel — vanilla's own
/// test is `template.min != the JDK's own integer type's own min-value accessor and, for the floating types,
/// `!= -the JDK's own float type's own max-value accessor / the JDK's own float type's own max-value accessor. So the flags byte is derived here
/// from the same comparison rather than from a separate "has bound" field, which
fn write_argument_parser(w: &mut Writer, parser: &ArgumentParser) {
    /// vanilla's own argument-utils helper's own number-flag-min accessor.
    const HAS_MIN: u8 = 1;
    /// vanilla's own argument-utils helper's own number-flag-max accessor.
    const HAS_MAX: u8 = 2;

    match parser {
        ArgumentParser::Bool => w.var_i32(0),
        ArgumentParser::Float { min, max } => {
            w.var_i32(1);
            let has_min = *min != -f32::MAX;
            let has_max = *max != f32::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.f32(*min);
            }
            if has_max {
                w.f32(*max);
            }
        }
        ArgumentParser::Double { min, max } => {
            w.var_i32(2);
            let has_min = *min != -f64::MAX;
            let has_max = *max != f64::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.f64(*min);
            }
            if has_max {
                w.f64(*max);
            }
        }
        ArgumentParser::Integer { min, max } => {
            w.var_i32(3);
            let has_min = *min != i32::MIN;
            let has_max = *max != i32::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.i32(*min);
            }
            if has_max {
                w.i32(*max);
            }
        }
        ArgumentParser::Long { min, max } => {
            w.var_i32(4);
            let has_min = *min != i64::MIN;
            let has_max = *max != i64::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.i64(*min);
            }
            if has_max {
                w.i64(*max);
            }
        }
        ArgumentParser::String(kind) => {
            w.var_i32(5);
            w.var_i32(match kind {
                StringKind::SingleWord => 0,
                StringKind::QuotablePhrase => 1,
                StringKind::GreedyPhrase => 2,
            });
        }
        ArgumentParser::Entity { single, players_only } => {
            w.var_i32(6);
            w.u8(u8::from(*single) | u8::from(*players_only) << 1);
        }
        ArgumentParser::GameProfile => w.var_i32(7),
        ArgumentParser::BlockPos => w.var_i32(8),
        ArgumentParser::ColumnPos => w.var_i32(9),
        ArgumentParser::Vec3 => w.var_i32(10),
        ArgumentParser::Vec2 => w.var_i32(11),
        ArgumentParser::BlockState => w.var_i32(12),
        ArgumentParser::BlockPredicate => w.var_i32(13),
        ArgumentParser::ItemStack => w.var_i32(14),
        ArgumentParser::ItemPredicate => w.var_i32(15),
        ArgumentParser::TeamColor => w.var_i32(16),
        ArgumentParser::HexColor => w.var_i32(17),
        ArgumentParser::Component => w.var_i32(18),
        ArgumentParser::Style => w.var_i32(19),
        ArgumentParser::Message => w.var_i32(20),
        ArgumentParser::NbtCompoundTag => w.var_i32(21),
        ArgumentParser::NbtTag => w.var_i32(22),
        ArgumentParser::NbtPath => w.var_i32(23),
        ArgumentParser::Objective => w.var_i32(24),
        ArgumentParser::ObjectiveCriteria => w.var_i32(25),
        ArgumentParser::Operation => w.var_i32(26),
        ArgumentParser::Particle => w.var_i32(27),
        ArgumentParser::Angle => w.var_i32(28),
        ArgumentParser::Rotation => w.var_i32(29),
        ArgumentParser::ScoreboardSlot => w.var_i32(30),
        ArgumentParser::ScoreHolder { multiple } => {
            w.var_i32(31);
            w.u8(u8::from(*multiple));
        }
        ArgumentParser::Swizzle => w.var_i32(32),
        ArgumentParser::Team => w.var_i32(33),
        ArgumentParser::ItemSlot => w.var_i32(34),
        ArgumentParser::ItemSlots => w.var_i32(35),
        ArgumentParser::ResourceLocation => w.var_i32(36),
        ArgumentParser::Function => w.var_i32(37),
        ArgumentParser::EntityAnchor => w.var_i32(38),
        ArgumentParser::IntRange => w.var_i32(39),
        ArgumentParser::FloatRange => w.var_i32(40),
        ArgumentParser::Dimension => w.var_i32(41),
        ArgumentParser::GameMode => w.var_i32(42),
        ArgumentParser::Time { min } => {
            w.var_i32(43);
            w.i32(*min);
        }
        ArgumentParser::ResourceOrTag { registry } => {
            w.var_i32(44);
            w.string(&registry.to_string());
        }
        ArgumentParser::ResourceOrTagKey { registry } => {
            w.var_i32(45);
            w.string(&registry.to_string());
        }
        ArgumentParser::Resource { registry } => {
            w.var_i32(46);
            w.string(&registry.to_string());
        }
        ArgumentParser::ResourceKeyArg { registry } => {
            w.var_i32(47);
            w.string(&registry.to_string());
        }
        ArgumentParser::ResourceSelector { registry } => {
            w.var_i32(48);
            w.string(&registry.to_string());
        }
        ArgumentParser::TemplateMirror => w.var_i32(49),
        ArgumentParser::TemplateRotation => w.var_i32(50),
        ArgumentParser::Heightmap => w.var_i32(51),
        ArgumentParser::LootTable => w.var_i32(52),
        ArgumentParser::LootPredicate => w.var_i32(53),
        ArgumentParser::LootModifier => w.var_i32(54),
        ArgumentParser::Dialog => w.var_i32(55),
        ArgumentParser::Uuid => w.var_i32(56),
        // A parser id this build does not model. Nothing but the raw id can be
        // written — the payload was never decoded, so there is none to reproduce —
        // and that is exactly what our own decoder assumes for an unknown id, so
        // the two ends stay aligned. Unreachable from a decode (an unmodeled id
        // becomes `NodeKind::Unrecognized`, handled by the node writer) and
        // unreachable from `lodestone-server`'s projection, which only ever names
        // parsers its own `McArg`s declare.
        ArgumentParser::Unknown(id) => w.var_i32(*id),
    }
}

/// Writes one vanilla's own clientbound commands packet's own entry: `Entry::write`'s exact order —
/// the flags byte, the child-index array (`writeVarIntArray`, so a VarInt count
/// then VarInt elements), the redirect index **only** when `FLAG_REDIRECT` is
/// set, then the type-dependent stub.
///
/// The stub order for an argument is `writeUtf(name)`, the parser id, the parser
/// payload, and only then the custom-suggestions identifier — the suggestions id
/// comes **after** the payload, which is the one field order here that cannot be
/// guessed from field names and which `ArgumentNodeStub::write` fixes.
///
/// A [`NodeKind::Unrecognized`] node is written as a **root-type** entry, keeping
/// its children, redirect and executable bit. That is not a fallback invented
/// here: it is what a client already does with such a node, since
/// vanilla's own clientbound commands packet's own read returns a null stub and vanilla's own command-node resolver's own resolve
/// builds a bare `RootCommandNode` for it. Re-encoding it as an argument is
/// impossible anyway — a node that failed to decode carries neither a name nor a
/// payload.
fn write_command_node(w: &mut Writer, node: &RawCommandNode) {
    /// `TYPE_ROOT`, and the type of an unrecognised node's degraded form.
    const TYPE_ROOT: u8 = 0;
    /// `TYPE_LITERAL`.
    const TYPE_LITERAL: u8 = 1;
    /// `TYPE_ARGUMENT`.
    const TYPE_ARGUMENT: u8 = 2;
    /// `FLAG_EXECUTABLE`.
    const EXECUTABLE: u8 = 4;
    /// `FLAG_REDIRECT`.
    const REDIRECT: u8 = 8;
    /// `FLAG_CUSTOM_SUGGESTIONS`.
    const CUSTOM_SUGGESTIONS: u8 = 16;
    /// `FLAG_RESTRICTED`.
    const RESTRICTED: u8 = 32;

    let mut flags = match &node.kind {
        NodeKind::Root | NodeKind::Unrecognized { .. } => TYPE_ROOT,
        NodeKind::Literal { .. } => TYPE_LITERAL,
        NodeKind::Argument { .. } => TYPE_ARGUMENT,
    };
    if node.executable {
        flags |= EXECUTABLE;
    }
    if node.redirect.is_some() {
        flags |= REDIRECT;
    }
    if node.restricted {
        flags |= RESTRICTED;
    }
    if let NodeKind::Argument { suggestions: Some(_), .. } = &node.kind {
        flags |= CUSTOM_SUGGESTIONS;
    }
    w.u8(flags);

    // `writeVarIntArray`: count then elements. The cast is checked against the
    // node count by the caller, which is the only place that knows it.
    w.var_i32(node.children.len() as i32);
    for &child in &node.children {
        w.var_i32(child as i32);
    }
    if let Some(redirect) = node.redirect {
        w.var_i32(redirect as i32);
    }
    match &node.kind {
        NodeKind::Root | NodeKind::Unrecognized { .. } => {}
        NodeKind::Literal { name } => w.string(name),
        NodeKind::Argument { name, parser, suggestions } => {
            w.string(name);
            write_argument_parser(w, parser);
            if let Some(provider) = suggestions {
                w.string(&provider.to_string());
            }
        }
    }
}

/// Encodes a whole `minecraft:commands` payload (clientbound id 16).
///
/// `ClientboundCommandsPacket::write` is `writeCollection(entries, …)` then
/// `writeVarInt(rootIndex)` — the node list **first**, the root index last, which
/// is the mirror of `V770Adapter`'s `decode_command_tree` and the ordering a
/// round-trip cannot catch you getting wrong if both ends agree wrongly. Read
/// against the vanilla record, not against the decoder.
fn encode_commands_body(tree: &WireCommandTree) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(tree.len() as i32);
    for index in 0..tree.len() {
        let node = tree.node(index).expect("index < len is always in range");
        write_command_node(&mut w, node);
    }
    w.var_i32(tree.root() as i32);
    w.into_vec()
}

/// Encodes a whole `minecraft:command_suggestions` payload (clientbound id 15).
///
/// vanilla's own clientbound command-suggestions packet's own stream codec (mirrored from the
/// decode side in `V770Adapter::decode_command_suggestions`, which this crate's
/// own client half uses to read a *real* server's reply): three VarInts (`id`,
/// `start`, `length`), then a list of `Entry(String text, Optional<Component>
/// tooltip)`. This server never attaches a tooltip to a suggestion — the
/// `false` presence byte matches `CommandSuggestionEntry::tooltip` being
/// `None` for every candidate `ServerCommands::suggest` produces — so the
/// `Some` arm below has no production caller today, but it encodes through
/// [`command_suggestion_tooltip_nbt`] rather than a bare `{"text": ...}`
/// compound precisely so a future caller that does attach a styled tooltip
/// does not silently lose it the way the client-side decode used to.
fn encode_command_suggestions_body(response: &CommandSuggestionsResponse) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(response.id);
    w.var_i32(response.start);
    w.var_i32(response.length);
    w.var_i32(response.suggestions.len() as i32);
    for entry in &response.suggestions {
        w.string(&entry.text);
        match &entry.tooltip {
            Some(tooltip) => {
                w.bool(true);
                write_network_nbt(&mut w, &command_suggestion_tooltip_nbt(tooltip))
                    .expect("a command-suggestion tooltip built from a `Text` always encodes");
            }
            None => w.bool(false),
        }
    }
    w.into_vec()
}

/// Lowers a [`Text`] to a network-NBT chat component for the
/// `minecraft:command_suggestions` tooltip field — the exact mirror of
/// `V770Adapter::decode_command_suggestions`'s read side (`Text::from_nbt`),
/// field for field: `text`/`translate`/`with`/`fallback`/`extra` plus every
/// [`TextStyle`] field `lodestone_model::text::nbt_style` reads back
/// (`color`, `bold`, `italic`, `underlined`, `strikethrough`, `obfuscated`,
/// `font`).
///
/// Deliberately **not** [`text_to_nbt`]: that function is scoped to the
/// disconnect-reason field alone and its own doc forbids reuse as a general
/// serializer, precisely because it drops style — the one thing a command
/// suggestion tooltip exists to carry (a hex colour has no legacy-code
/// fallback). Click/hover/insertion are still omitted, matching
/// [`text_to_nbt`]'s scope for the same reason: a tab-complete tooltip is a
/// hover-only informational popup with no interactivity of its own to carry.
fn command_suggestion_tooltip_nbt(text: &Text) -> Nbt {
    let mut fields: Vec<(String, Nbt)> = Vec::new();
    match &text.content {
        TextContent::Literal(literal) => {
            fields.push(("text".to_owned(), Nbt::String(literal.clone())));
        }
        TextContent::Translate {
            key,
            with,
            fallback,
        } => {
            fields.push(("translate".to_owned(), Nbt::String(key.clone())));
            if let Some(fallback) = fallback {
                fields.push(("fallback".to_owned(), Nbt::String(fallback.clone())));
            }
            if !with.is_empty() {
                fields.push((
                    "with".to_owned(),
                    Nbt::List {
                        element_type: NbtTag::Compound,
                        elements: with.iter().map(command_suggestion_tooltip_nbt).collect(),
                    },
                ));
            }
        }
    }
    let style = &text.style;
    if let Some(color) = style.color {
        fields.push(("color".to_owned(), Nbt::String(color.name())));
    }
    if let Some(bold) = style.bold {
        fields.push(("bold".to_owned(), Nbt::Byte(bold as i8)));
    }
    if let Some(italic) = style.italic {
        fields.push(("italic".to_owned(), Nbt::Byte(italic as i8)));
    }
    if let Some(underlined) = style.underlined {
        fields.push(("underlined".to_owned(), Nbt::Byte(underlined as i8)));
    }
    if let Some(strikethrough) = style.strikethrough {
        fields.push(("strikethrough".to_owned(), Nbt::Byte(strikethrough as i8)));
    }
    if let Some(obfuscated) = style.obfuscated {
        fields.push(("obfuscated".to_owned(), Nbt::Byte(obfuscated as i8)));
    }
    if let Some(font) = style.font {
        fields.push(("font".to_owned(), Nbt::String(font.name().to_owned())));
    }
    if !text.extra.is_empty() {
        fields.push((
            "extra".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: text.extra.iter().map(command_suggestion_tooltip_nbt).collect(),
            },
        ));
    }
    Nbt::Compound(fields)
}

/// Hand-written encoder for the clientbound `system_chat` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_play`'s `SYSTEM_CHAT` arm). Wire layout (mirrors the
/// decode side exactly): a network-form NBT text component (root tag id +
/// payload, no root name — vanilla's vanilla's own component-serialization helper's own trusted-stream-codec accessor),
/// then a big-endian `bool` overlay flag (`false` selects normal chat history,
/// `true` the action-bar overlay).
fn encode_system_chat(message: &str, overlay: bool) -> Vec<u8> {
    let component = Nbt::Compound(vec![("text".to_owned(), Nbt::String(message.to_owned()))]);
    let mut w = Writer::default();
    write_network_nbt(&mut w, &component).expect("plain string NBT component always encodes");
    w.bool(overlay);
    w.into_vec()
}

