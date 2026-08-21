//! Typed parse of a sign block entity's NBT.
//!
//! See `docs/block-entity-renderers.md`'s "Sign" section for how this shape
//! was confirmed: a standalone wire probe joined the live creative oracle, placed
//! a real `oak_sign` with `front_text`/`back_text` over RCON, and read the
//! decoded record straight out of a live [`crate::World`]. The captured
//! payload is quoted verbatim in that doc; this module parses exactly that
//! shape, not a guess at it.
//!
//! # Field names, from the real codec
//!
//! `SignText.DIRECT_CODEC` is `{ messages: [Component; 4],
//! filtered_messages?: [Component; 4], color: DyeColor = black,
//! has_glowing_text: bool = false }`, and `SignBlockEntity.saveAdditional`
//! stores one of these per side under `front_text`/`back_text`, plus a
//! sibling `is_waxed` boolean. `filtered_messages` is the server's
//! profanity-filter shadow copy for chat-filtering clients
//! (`SignText.getMessages(shouldFilter)`); this port has no client-side text
//! filtering setting (Minecraft's own default is *off*), so it is not parsed
//! — [`SignSide::lines`] always reads `messages`, matching vanilla's
//! unfiltered default.
//!
//! # Messages are JSON text, not the NBT-structural component shape
//!
//! Every other resolved text in this codebase
//! ([`lodestone_core::plain_text_from_nbt_component`]) walks a *structural*
//! NBT encoding of a `Component` (`Compound { text, extra }`, as chat/player
//! list/entity-metadata components arrive). A sign's `messages` list is
//! different, and this was the one surprising part of the wire probe: each
//! element is an `Nbt::String` whose *content* is the **JSON** serialization
//! of the component, quotes included. A plain "LODESTONE PROBE" line arrived
//! as the 18-character NBT string `"LODESTONE PROBE"` (opening and closing
//! `"` are part of the payload, not Rust's `Debug` escaping) — i.e. the JSON
//! text a bare string component serializes to, stored as-is inside an NBT
//! string rather than unwrapped into one. `resolve_message` parses that JSON
//! into [`SignTextSpan`]s: colour (named or `#rrggbb` hex), bold, italic,
//! underline and strikethrough, inherited from an enclosing node down through
//! its `extra` children the same way vanilla's `Style.inherit` resolves a
//! component tree — a bare JSON string is its own (unstyled) text; an
//! object's own `"text"` plus its `"extra"` array recursively, each level's
//! own style fields winning over whatever it inherited.
//!
//! # Why this is not `lodestone_model::text::Text`
//!
//! Every other styled-text surface in this codebase (entity nametags,
//! `text_display`) carries a real [`lodestone_model::text::Text`] from
//! packet decode to draw. A sign's text cannot use that type:
//! `lodestone-model` depends on `lodestone-world` itself (`pub use
//! lodestone_world::{LoadedChunk, WorldSink}` in its `adapter` module, and
//! `event.rs`'s `WorldSink`-driven `ClientEvent` handlers apply decoded
//! packets straight into a live [`crate::World`]), so a
//! `lodestone-world -> lodestone-model` edge back the other way would be a
//! dependency cycle. [`SignTextSpan`] is this crate's own minimal analogue —
//! same shape as [`lodestone_model::text::TextSpan`], already flattened and
//! fully inherited rather than a tree, since sign text has no click/hover
//! events or translation keys to preserve — and
//! `lodestone-shell`'s `gpu/sign_text.rs` (which already depends on both
//! crates) converts one into a real `lodestone_model::text::TextSpan` on its
//! way into `gpu::nametag::layout_styled_ink_runs`, the same world-space
//! styled-glyph layout the other two surfaces use. No second styled-text
//! *layout* implementation exists because of this; only the JSON-to-spans
//! *parse* is duplicated, and only because the crate graph leaves no other
//! path.
//!
//! # Where this parse belongs, and why not a version crate
//!
//! [`BlockEntity`](crate::BlockEntity)'s own module doc says the NBT *schema*
//! is version-specific and "belongs to a version crate" — true in principle,
//! but `crates/protocol/v770/src/server_protocol.rs` was another agent's
//! in-flight work for the whole of this session (see `CLAUDE.md`'s
//! file-ownership notes), so this parse lives here instead, pragmatically,
//! against the crate this task was granted outright. If a second protocol
//! version is ever added, this module is the one that would need to move or
//! branch on it — nothing about its *shape* assumes v770-only, but its field
//! names are only checked against the real 26.2 codec above.

use lodestone_core::Nbt;

/// One dyed colour a sign's text can use — `DyeColor`'s sixteen enum
/// constants, the type [`SignSide::color`] resolves `SignText`'s `color`
/// field into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignDyeColor {
    /// `minecraft:white`.
    White,
    /// `minecraft:orange`.
    Orange,
    /// `minecraft:magenta`.
    Magenta,
    /// `minecraft:light_blue`.
    LightBlue,
    /// `minecraft:yellow`.
    Yellow,
    /// `minecraft:lime`.
    Lime,
    /// `minecraft:pink`.
    Pink,
    /// `minecraft:gray`.
    Gray,
    /// `minecraft:light_gray`.
    LightGray,
    /// `minecraft:cyan`.
    Cyan,
    /// `minecraft:purple`.
    Purple,
    /// `minecraft:blue`.
    Blue,
    /// `minecraft:brown`.
    Brown,
    /// `minecraft:green`.
    Green,
    /// `minecraft:red`.
    Red,
    /// `minecraft:black` — the codec's own default when `color` is absent.
    Black,
}

impl SignDyeColor {
    /// Resolves `DyeColor.CODEC`'s serialized name, or `None` for anything
    /// else — a malformed or future value degrades to
    /// [`SignDyeColor::Black`] at the call site, the codec's own default,
    /// rather than here, so a caller can tell "absent" from "unrecognised"
    /// if it ever needs to.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "white" => SignDyeColor::White,
            "orange" => SignDyeColor::Orange,
            "magenta" => SignDyeColor::Magenta,
            "light_blue" => SignDyeColor::LightBlue,
            "yellow" => SignDyeColor::Yellow,
            "lime" => SignDyeColor::Lime,
            "pink" => SignDyeColor::Pink,
            "gray" => SignDyeColor::Gray,
            "light_gray" => SignDyeColor::LightGray,
            "cyan" => SignDyeColor::Cyan,
            "purple" => SignDyeColor::Purple,
            "blue" => SignDyeColor::Blue,
            "brown" => SignDyeColor::Brown,
            "green" => SignDyeColor::Green,
            "red" => SignDyeColor::Red,
            "black" => SignDyeColor::Black,
            _ => return None,
        })
    }
}

/// One already-flattened, fully-inherited styled run of one sign line's
/// text — this crate's own minimal analogue of
/// `lodestone_model::text::TextSpan`; see the module doc for why that real
/// type cannot be used here.
///
/// `color` is `None` when neither this JSON node nor any of its ancestors up
/// to the message root specified a colour. **`None` means "draw in the
/// side's own dye colour", not "draw black" or "draw white" — the dye colour
/// is a run's *default*, not an override.** This is
/// `AbstractSignRenderer.submitSignText` passing the side's resolved colour
/// as `Font`'s own default-colour argument, and `Font.java::getTextColor`
/// only substituting a glyph's own `Style` colour when that style actually
/// carries one — the identical "child wins when specified, otherwise inherit
/// the surface's own base" rule [`lodestone_model::text`]'s `resolved_rgb`
/// already applies for nametags and `text_display`. A run whose own colour
/// *is* specified always wins over the dye, at any brightness.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignTextSpan {
    /// This run's plain text. Never empty — a JSON node whose own text is
    /// empty contributes no span at all, the same "never emits an
    /// empty-text span" contract `Text::to_spans` has.
    pub text: String,
    /// This run's own explicit colour (`0x00rrggbb`), resolved from a named
    /// colour or a `#rrggbb` hex literal. `None` means unspecified — see the
    /// type doc.
    pub color: Option<u32>,
    /// Bold, fully resolved (defaults to `false` at the message root, same
    /// as vanilla's `Style.EMPTY`).
    pub bold: bool,
    /// Italic, fully resolved.
    pub italic: bool,
    /// Underlined, fully resolved.
    pub underlined: bool,
    /// Struck through, fully resolved.
    pub strikethrough: bool,
}

/// One face's text — `SignText`'s `messages`, `color` and `hasGlowingText`
/// fields, minus `filteredMessages` (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignSide {
    /// One entry per line, top to bottom. Always four — vanilla's
    /// `SignText.LINES = 4`. An empty `Vec` is a blank line, not "no line".
    pub lines: [Vec<SignTextSpan>; 4],
    /// `has_glowing_text` (`SignText.hasGlowingText`) — full-bright dye
    /// colour instead of the darkened default when set.
    pub glowing: bool,
    /// `color` (`SignText.color`), defaulting to
    /// [`SignDyeColor::Black`] exactly as the codec's own
    /// `optionalAlwaysPresentFieldOf(..., DyeColor.BLACK)` does.
    pub color: SignDyeColor,
}

impl Default for SignSide {
    /// Four empty lines, black, not glowing — vanilla's own
    /// `new SignText()` no-arg constructor default, and what a sign
    /// block entity with no NBT at all (a freshly-placed sign the server
    /// has not yet sent text for) should draw: nothing, rather than an
    /// error.
    fn default() -> Self {
        SignSide {
            lines: Default::default(),
            glowing: false,
            color: SignDyeColor::Black,
        }
    }
}

/// A sign block entity's full typed NBT — `front_text`/`back_text`/
/// `is_waxed`, the exact three top-level keys the probe in
/// `docs/block-entity-renderers.md` measured on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignText {
    /// The side a player reading the sign from its front sees.
    pub front: SignSide,
    /// The side a player reading the sign from behind sees.
    pub back: SignSide,
    /// `is_waxed` — disables further editing server-side. No render effect
    /// in vanilla (`SignBlockEntity` carries it purely for interaction
    /// gating; there is no `WaxedSignRenderer` or visual overlay), parsed
    /// here for completeness rather than left silently unavailable to a
    /// future interaction system.
    pub waxed: bool,
}

impl SignText {
    /// Parses a sign block entity's NBT. Always returns a value —
    /// `Nbt::End` (a sign the server sent no extra data for) and a malformed
    /// or unexpectedly-shaped compound both degrade to
    /// [`SignText::default`] rather than `None`, the same fail-open contract
    /// [`crate::BlockEntity`]'s own NBT field has: a sign with no
    /// (yet-parseable) text still draws as a real, blank sign rather than
    /// vanishing.
    #[must_use]
    pub fn parse(nbt: &Nbt) -> Self {
        let Nbt::Compound(fields) = nbt else {
            return SignText::default();
        };
        SignText {
            front: find(fields, "front_text").map_or_else(SignSide::default, parse_side),
            back: find(fields, "back_text").map_or_else(SignSide::default, parse_side),
            waxed: find(fields, "is_waxed").and_then(as_bool).unwrap_or(false),
        }
    }
}

fn parse_side(nbt: &Nbt) -> SignSide {
    let Nbt::Compound(fields) = nbt else {
        return SignSide::default();
    };
    let color = find(fields, "color")
        .and_then(as_string)
        .and_then(SignDyeColor::from_name)
        .unwrap_or(SignDyeColor::Black);
    let glowing = find(fields, "has_glowing_text")
        .and_then(as_bool)
        .unwrap_or(false);
    let mut lines: [Vec<SignTextSpan>; 4] = Default::default();
    if let Some(Nbt::List { elements, .. }) = find(fields, "messages") {
        for (slot, element) in lines.iter_mut().zip(elements.iter()) {
            if let Nbt::String(raw) = element {
                *slot = resolve_message(raw);
            }
        }
    }
    SignSide {
        lines,
        glowing,
        color,
    }
}

/// Maximum component nesting depth explored while parsing one `messages`
/// element — matches [`lodestone_model::text`]'s own `MAX_DEPTH`, the same
/// guard against hostile/malformed network input for the same reason (see
/// the module doc for why this crate cannot simply depend on that one).
const MAX_DEPTH: usize = 64;

/// Style attributes accumulated while walking one JSON component tree —
/// every field `None`/unset until a node along the walk specifies it, then
/// carried down to that node's `extra` children by [`ResolvedStyle::inherit`]
/// exactly as `Style.inherit` (child wins when specified, otherwise the
/// parent's value survives).
#[derive(Debug, Clone, Copy, Default)]
struct ResolvedStyle {
    color: Option<u32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underlined: Option<bool>,
    strikethrough: Option<bool>,
}

impl ResolvedStyle {
    fn own_from_object(map: &serde_json::Map<String, serde_json::Value>) -> Self {
        ResolvedStyle {
            color: map
                .get("color")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_text_color_name),
            bold: map.get("bold").and_then(serde_json::Value::as_bool),
            italic: map.get("italic").and_then(serde_json::Value::as_bool),
            underlined: map.get("underlined").and_then(serde_json::Value::as_bool),
            strikethrough: map.get("strikethrough").and_then(serde_json::Value::as_bool),
        }
    }

    fn inherit(self, parent: ResolvedStyle) -> Self {
        ResolvedStyle {
            color: self.color.or(parent.color),
            bold: self.bold.or(parent.bold),
            italic: self.italic.or(parent.italic),
            underlined: self.underlined.or(parent.underlined),
            strikethrough: self.strikethrough.or(parent.strikethrough),
        }
    }

    /// Pushes a non-empty `text` node as one [`SignTextSpan`], resolving
    /// every `Option<bool>` flag to `false` (the message-root default) if
    /// still unset at this point in the walk.
    fn push_text(self, text: &str, out: &mut Vec<SignTextSpan>) {
        if text.is_empty() {
            return;
        }
        out.push(SignTextSpan {
            text: text.to_owned(),
            color: self.color,
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underlined: self.underlined.unwrap_or(false),
            strikethrough: self.strikethrough.unwrap_or(false),
        });
    }
}

/// The sixteen legacy chat-colour names' RGB values
/// (`0x00rrggbb`), transcribed from `ChatFormatting.java`'s constructor
/// arguments — the same table `lodestone_model::text::TextColor::rgb`
/// carries, duplicated here rather than depended on (see the module doc) —
/// plus `#rrggbb` hex, `TextColor.java`'s modern colour form. `None` for
/// anything else, including the pseudo-colour `"reset"` (a style reset, not
/// a colour — sign text has nothing above the message root to reset to, so
/// this parse has no use for it).
fn parse_text_color_name(name: &str) -> Option<u32> {
    if let Some(hex) = name.strip_prefix('#') {
        return (hex.len() == 6)
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten();
    }
    Some(match name {
        "black" => 0x0000_0000,
        "dark_blue" => 0x0000_00aa,
        "dark_green" => 0x0000_aa00,
        "dark_aqua" => 0x0000_aaaa,
        "dark_red" => 0x00aa_0000,
        "dark_purple" => 0x00aa_00aa,
        "gold" => 0x00ff_aa00,
        "gray" => 0x00aa_aaaa,
        "dark_gray" => 0x0055_5555,
        "blue" => 0x0055_55ff,
        "green" => 0x0055_ff55,
        "aqua" => 0x0055_ffff,
        "red" => 0x00ff_5555,
        "light_purple" => 0x00ff_55ff,
        "yellow" => 0x00ff_ff55,
        "white" => 0x00ff_ffff,
        _ => return None,
    })
}

/// Recursively walks one parsed JSON component value, appending flattened
/// [`SignTextSpan`]s onto `out` — the styled sibling of the plain-text
/// `text`/`extra` recursion every other resolved text in this codebase uses
/// ([`lodestone_core::plain_text_from_nbt_component`]), over a
/// [`serde_json::Value`] instead of an [`Nbt`]: a bare JSON string is its
/// own (unstyled, inherited-only) text; an array concatenates its elements;
/// an object contributes its own `"text"` (styled by its own fields plus
/// whatever it inherited) then recurses into `"extra"` with its own
/// style now the parent for that subtree.
fn append_json_component_spans(
    value: &serde_json::Value,
    parent: ResolvedStyle,
    depth: usize,
    out: &mut Vec<SignTextSpan>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        serde_json::Value::String(s) => parent.push_text(s, out),
        serde_json::Value::Array(items) => {
            for item in items {
                append_json_component_spans(item, parent, depth + 1, out);
            }
        }
        serde_json::Value::Object(map) => {
            let resolved = ResolvedStyle::own_from_object(map).inherit(parent);
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                resolved.push_text(text, out);
            }
            if let Some(extra) = map.get("extra") {
                append_json_component_spans(extra, resolved, depth + 1, out);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Resolves one `messages` element's JSON-text content into styled spans —
/// see the module doc for why this is JSON, not the NBT-structural component
/// shape [`lodestone_core::plain_text_from_nbt_component`] handles.
/// Malformed JSON degrades to one unstyled span carrying the raw string
/// verbatim (fail open, matching the rest of this parse) rather than losing
/// the line entirely.
fn resolve_message(raw: &str) -> Vec<SignTextSpan> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => {
            let mut out = Vec::new();
            append_json_component_spans(&value, ResolvedStyle::default(), 0, &mut out);
            out
        }
        Err(_) => vec![SignTextSpan {
            text: raw.to_owned(),
            ..Default::default()
        }],
    }
}

fn find<'a>(fields: &'a [(String, Nbt)], name: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

fn as_bool(nbt: &Nbt) -> Option<bool> {
    match nbt {
        Nbt::Byte(b) => Some(*b != 0),
        _ => None,
    }
}

fn as_string(nbt: &Nbt) -> Option<&str> {
    match nbt {
        Nbt::String(s) => Some(s.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::NbtTag;

    fn plain(text: &str) -> SignTextSpan {
        SignTextSpan {
            text: text.to_owned(),
            ..Default::default()
        }
    }

    /// Builds the exact NBT `docs/block-entity-renderers.md`'s live probe
    /// captured for a real `oak_sign` with `front_text`/`back_text` set over
    /// RCON — the expected value here is a measurement transcribed from that
    /// doc, not authored to match this parser.
    fn probe_nbt() -> Nbt {
        let side = |messages: [&str; 4], color: &str| {
            Nbt::Compound(vec![
                ("has_glowing_text".to_owned(), Nbt::Byte(0)),
                ("color".to_owned(), Nbt::String(color.to_owned())),
                (
                    "messages".to_owned(),
                    Nbt::List {
                        element_type: NbtTag::String,
                        elements: messages
                            .into_iter()
                            .map(|m| Nbt::String(m.to_owned()))
                            .collect(),
                    },
                ),
            ])
        };
        Nbt::Compound(vec![
            (
                "back_text".to_owned(),
                side(["\"\"", "\"\"", "\"\"", "\"\""], "black"),
            ),
            ("is_waxed".to_owned(), Nbt::Byte(0)),
            (
                "front_text".to_owned(),
                side(
                    ["\"LODESTONE PROBE\"", "\"\"", "\"\"", "\"\""],
                    "red",
                ),
            ),
        ])
    }

    #[test]
    fn parses_the_real_probe_capture() {
        let text = SignText::parse(&probe_nbt());
        assert_eq!(text.front.lines[0], vec![plain("LODESTONE PROBE")]);
        assert!(text.front.lines[1].is_empty());
        assert_eq!(text.front.color, SignDyeColor::Red);
        assert!(!text.front.glowing);
        assert!(text.back.lines.iter().all(Vec::is_empty));
        assert_eq!(text.back.color, SignDyeColor::Black);
        assert!(!text.waxed);
    }

    #[test]
    fn end_nbt_degrades_to_the_default_blank_sign() {
        let text = SignText::parse(&Nbt::End);
        assert_eq!(text, SignText::default());
        assert!(text.front.lines.iter().all(Vec::is_empty));
        assert_eq!(text.front.color, SignDyeColor::Black);
    }

    #[test]
    fn missing_color_defaults_to_black_like_the_real_codec() {
        let nbt = Nbt::Compound(vec![(
            "front_text".to_owned(),
            Nbt::Compound(vec![(
                "messages".to_owned(),
                Nbt::List {
                    element_type: NbtTag::String,
                    elements: vec![
                        Nbt::String("\"hi\"".to_owned()),
                        Nbt::String("\"\"".to_owned()),
                        Nbt::String("\"\"".to_owned()),
                        Nbt::String("\"\"".to_owned()),
                    ],
                },
            )]),
        )]);
        let text = SignText::parse(&nbt);
        assert_eq!(text.front.color, SignDyeColor::Black);
        assert_eq!(text.front.lines[0], vec![plain("hi")]);
    }

    /// A rich (object-shaped) component with `extra` runs must concatenate,
    /// the same recursion `plain_text_from_nbt_component` performs for the
    /// NBT-structural form — this is the control that a naive "only handle
    /// bare JSON strings" implementation would fail.
    #[test]
    fn a_rich_component_concatenates_text_and_extra() {
        let raw = r#"{"text":"hello ","extra":[{"text":"world"},"!"]}"#;
        let spans = resolve_message(raw);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "hello world!");
    }

    #[test]
    fn malformed_json_degrades_to_the_raw_string_rather_than_losing_the_line() {
        assert_eq!(resolve_message("not json"), vec![plain("not json")]);
    }

    /// The central control: an explicit **hex** colour on one run must
    /// survive as `Some`, and a sibling run with no colour of its own must
    /// come back `None` — never coerced to black, white, or the sibling's
    /// colour. Hex, not one of the sixteen legacy colours, because a lossy
    /// path (e.g. accidentally rounding to the nearest legacy colour) could
    /// still happen to survive a legacy-colour fixture.
    #[test]
    fn an_explicit_hex_colour_survives_and_an_unset_sibling_has_none() {
        let raw = r##"{"text":"a","color":"#123456","extra":[{"text":"b"}]}"##;
        let spans = resolve_message(raw);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].text, "a");
        assert_eq!(spans[0].color, Some(0x0012_3456));
        assert_eq!(spans[1].text, "b");
        // Not `None`-coerced-to-something and not silently dropped: "b" has
        // no colour of its own, so it *inherits* "a"'s explicit colour, the
        // same way a real nested extra component would — see the type doc's
        // "child wins when specified, otherwise inherit" rule.
        assert_eq!(spans[1].color, Some(0x0012_3456));
    }

    /// The other half of inheritance: a child that *does* specify its own
    /// colour overrides the parent's, rather than the parent always winning.
    #[test]
    fn a_child_can_override_the_parents_colour() {
        let raw = r##"{"text":"a","color":"red","extra":[{"text":"b","color":"#00ff00"}]}"##;
        let spans = resolve_message(raw);
        assert_eq!(spans[0].color, Some(0x00ff_5555));
        assert_eq!(spans[1].color, Some(0x0000_ff00));
    }

    /// A truly colour-less message (no node anywhere specifies one) must
    /// resolve to `None`, not some fallback colour baked in at parse time —
    /// the dye-colour fallback is the *draw site*'s job, not this parser's.
    #[test]
    fn a_message_with_no_colour_anywhere_resolves_to_none() {
        let spans = resolve_message(r#"{"text":"plain"}"#);
        assert_eq!(spans, vec![plain("plain")]);
        assert_eq!(spans[0].color, None);
    }

    /// Bold set on the parent is inherited by a child that does not mention
    /// it, and a child can explicitly turn a flag back off.
    #[test]
    fn style_flags_inherit_and_can_be_explicitly_cleared() {
        let raw = r#"{"text":"a","bold":true,"underlined":true,"extra":[
            {"text":"b"},
            {"text":"c","underlined":false}
        ]}"#;
        let spans = resolve_message(raw);
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert!(spans[0].bold && spans[0].underlined, "{:?}", spans[0]);
        // Inherits both flags from "a".
        assert!(spans[1].bold && spans[1].underlined, "{:?}", spans[1]);
        // Inherits bold, explicitly clears underline.
        assert!(spans[2].bold, "{:?}", spans[2]);
        assert!(!spans[2].underlined, "{:?}", spans[2]);
    }

    #[test]
    fn every_dye_name_round_trips() {
        let names = [
            "white",
            "orange",
            "magenta",
            "light_blue",
            "yellow",
            "lime",
            "pink",
            "gray",
            "light_gray",
            "cyan",
            "purple",
            "blue",
            "brown",
            "green",
            "red",
            "black",
        ];
        for name in names {
            assert!(
                SignDyeColor::from_name(name).is_some(),
                "{name} did not resolve"
            );
        }
        assert_eq!(SignDyeColor::from_name("not_a_colour"), None);
    }
}
