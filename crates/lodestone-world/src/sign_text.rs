//! Typed parse of a sign block entity's NBT (issue #23's sign scope).
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
//! `SignText.DIRECT_CODEC`
//! (`.cache/mc/26.2/src/net/minecraft/world/level/block/entity/SignText.java:33-41`)
//! is `{ messages: [Component; 4], filtered_messages?: [Component; 4], color:
//! DyeColor = black, has_glowing_text: bool = false }`, and
//! `SignBlockEntity.saveAdditional`
//! (`.cache/mc/26.2/src/net/minecraft/world/level/block/entity/SignBlockEntity.java:94-99`)
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
//! and extracts plain text the same shape
//! [`lodestone_core::plain_text_from_nbt_component`] uses for the
//! NBT-structural form: a bare JSON string is its own text; an object's
//! `"text"` field plus its `"extra"` array, recursively. Formatting (colour,
//! bold, click events) is discarded — this port draws sign text in the
//! side's own dye colour, not per-run formatting, matching what
//! `docs/block-entity-renderers.md` scoped in.
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

/// One dyed colour a sign's text can use — `DyeColor`'s sixteen values
/// (`.cache/mc/26.2/client-src/net/minecraft/world/item/DyeColor.java:30-45`),
/// the type [`SignSide::color`] resolves `SignText`'s `color` field into.
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

/// One face's text — `SignText`
/// (`.cache/mc/26.2/src/.../SignText.java:43-46`), minus `filteredMessages`
/// (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignSide {
    /// Resolved plain text, one entry per line, top to bottom. Always four —
    /// vanilla's `SignText.LINES = 4`. An empty string is a blank line, not
    /// "no line".
    pub lines: [String; 4],
    /// `has_glowing_text` (`SignText.java:46`) — full-bright dye colour
    /// instead of the darkened default when set.
    pub glowing: bool,
    /// `color` (`SignText.java:45`), defaulting to
    /// [`SignDyeColor::Black`] exactly as the codec's own
    /// `optionalAlwaysPresentFieldOf(..., DyeColor.BLACK)` does.
    pub color: SignDyeColor,
}

impl Default for SignSide {
    /// Four empty lines, black, not glowing — vanilla's own
    /// `new SignText()` default (`SignText.java:50-52`), and what a sign
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
    let mut lines: [String; 4] = Default::default();
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

/// Resolves one `messages` element's JSON-text content into plain text — see
/// the module doc for why this is JSON, not the NBT-structural component
/// shape [`lodestone_core::plain_text_from_nbt_component`] handles.
/// Malformed JSON degrades to the raw string verbatim (fail open, matching
/// the rest of this parse) rather than losing the line entirely.
fn resolve_message(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => {
            let mut out = String::new();
            append_json_component_text(&value, &mut out);
            out
        }
        Err(_) => raw.to_owned(),
    }
}

/// Same recursive shape as
/// [`lodestone_core::plain_text_from_nbt_component`]'s NBT walk
/// (`text` + `extra`), over a parsed [`serde_json::Value`] instead of
/// [`Nbt`] — a bare JSON string is its own text, an array concatenates its
/// elements, and an object contributes its `"text"` field plus its
/// `"extra"` array, recursively.
fn append_json_component_text(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => out.push_str(s),
        serde_json::Value::Array(items) => {
            for item in items {
                append_json_component_text(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                out.push_str(text);
            }
            if let Some(extra) = map.get("extra") {
                append_json_component_text(extra, out);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
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
        assert_eq!(text.front.lines[0], "LODESTONE PROBE");
        assert_eq!(text.front.lines[1], "");
        assert_eq!(text.front.color, SignDyeColor::Red);
        assert!(!text.front.glowing);
        assert_eq!(text.back.lines, ["", "", "", ""]);
        assert_eq!(text.back.color, SignDyeColor::Black);
        assert!(!text.waxed);
    }

    #[test]
    fn end_nbt_degrades_to_the_default_blank_sign() {
        let text = SignText::parse(&Nbt::End);
        assert_eq!(text, SignText::default());
        assert_eq!(text.front.lines, ["", "", "", ""]);
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
        assert_eq!(text.front.lines[0], "hi");
    }

    /// A rich (object-shaped) component with `extra` runs must concatenate,
    /// the same recursion `plain_text_from_nbt_component` performs for the
    /// NBT-structural form — this is the control that a naive "only handle
    /// bare JSON strings" implementation would fail.
    #[test]
    fn a_rich_component_concatenates_text_and_extra() {
        let raw = r#"{"text":"hello ","extra":[{"text":"world"},"!"]}"#;
        assert_eq!(resolve_message(raw), "hello world!");
    }

    #[test]
    fn malformed_json_degrades_to_the_raw_string_rather_than_losing_the_line() {
        assert_eq!(resolve_message("not json"), "not json");
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
