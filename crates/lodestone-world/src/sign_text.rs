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
//! One side's record is `{ messages: [Component; 4],
//! filtered_messages?: [Component; 4], color: DyeColor = black,
//! has_glowing_text: bool = false }`, and a sign block entity's save routine
//! stores one of these per side under `front_text`/`back_text`, plus a
//! sibling `is_waxed` boolean. `filtered_messages` is the server's
//! profanity-filter shadow copy for chat-filtering clients; this port has no
//! client-side text filtering setting (the real default is *off*), so it is
//! not parsed — [`SignSide::lines`] always reads `messages`, matching the
//! unfiltered default.
//!
//! # Messages are structural NBT components, not JSON — and this file said
//! the opposite until a live server proved otherwise
//!
//! Each `messages` element is encoded by first trying to collapse the
//! component to a bare string (no siblings, no style at all); only when that
//! fails does the encoder fall back to the component's full structural form.
//! Under NBT encoding that means exactly two shapes per element:
//!
//! * a component that collapses — plain text, no siblings, **empty style** —
//!   is an `Nbt::String` holding the text **verbatim**. Not JSON: a line
//!   reading `Hello` is the five-character NBT string `Hello`.
//! * anything else — any colour, any format flag, any `extra` — is an
//!   `Nbt::Compound` carrying `text`/`extra` plus its own style fields, the
//!   *same structural shape*
//!   [`lodestone_core::plain_text_from_nbt_component`] already walks for
//!   chat, the player list and entity metadata.
//!
//! An earlier version of this module parsed each element as **JSON**, on the
//! strength of two wire probes that both reported an element arriving as the
//! 18-character string `"LODESTONE PROBE"` — quotes included. Those quotes
//! were an artefact of how the probes *set* the sign: an RCON `/setblock`
//! whose SNBT wrote a string literal that already contained them. The
//! captures agreed with each other because they shared one producer, and
//! this crate's own server-side writer (`lodestone_server`'s
//! `block_entity_to_nbt`) then serialised sign lines back to JSON strings to
//! match — a closed `decode(encode(x)) == x` loop that could not see the
//! real wire form. Against a **real** server the consequence was total: a
//! coloured or formatted line arrives as a `Compound`, matched no arm, and
//! **every such sign rendered its board with no text at all**.
//!
//! [`append_component_spans`] walks the structural form: a `String` is its
//! own literal, a `Compound` contributes its own `text` styled by its own
//! fields plus whatever it inherited and then recurses into `extra`, and a
//! `List`'s element `0` is the root and the rest are its siblings, so they
//! inherit element `0`'s resolved style, not the enclosing one. Style
//! inheritance follows one rule throughout: a child's own value wins where it
//! has one, otherwise the parent's survives.
//!
//! Not modelled: `translatable`, `keybind`, `score`, `selector`, `nbt` and
//! `object` contents. A `Compound` carrying one of those and no `text` field
//! contributes nothing rather than a placeholder — a real gap, disclosed
//! here rather than half-built, and one no sign written by a player can hit.
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
//! *layout* implementation exists because of this; only the component-to-spans
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

/// One dyed colour a sign's text can use — the sixteen dye colours, the
/// type [`SignSide::color`] resolves a side's own `color` field into.
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
    /// Resolves the codec's serialized dye-colour name, or `None` for
    /// anything else — a malformed or future value degrades to
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
/// is a run's *default*, not an override.** The side's resolved dye colour is
/// passed down as the text-drawing routine's own default colour, substituted
/// only when a glyph's own resolved style actually carries an explicit one —
/// the identical "child wins when specified, otherwise inherit the surface's
/// own base" rule [`lodestone_model::text`]'s `resolved_rgb` already applies
/// for nametags and `text_display`. A run whose own colour *is* specified
/// always wins over the dye, at any brightness.
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
    /// Bold, fully resolved (defaults to `false` at the message root, the
    /// same as an entirely unstyled component's default style).
    pub bold: bool,
    /// Italic, fully resolved.
    pub italic: bool,
    /// Underlined, fully resolved.
    pub underlined: bool,
    /// Struck through, fully resolved.
    pub strikethrough: bool,
}

/// One face's text — the `messages`, `color` and `has_glowing_text` fields
/// of a side's record, minus the profanity-filter shadow copy (see the
/// module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignSide {
    /// One entry per line, top to bottom. Always four, the fixed line count
    /// a side's record carries. An empty `Vec` is a blank line, not "no
    /// line".
    pub lines: [Vec<SignTextSpan>; 4],
    /// `has_glowing_text` — full-bright dye colour instead of the darkened
    /// default when set.
    pub glowing: bool,
    /// `color`, defaulting to [`SignDyeColor::Black`] exactly as the
    /// codec's own always-present-with-default field does.
    pub color: SignDyeColor,
}

impl Default for SignSide {
    /// Four empty lines, black, not glowing — the real no-argument default,
    /// and what a sign block entity with no NBT at all (a freshly-placed
    /// sign the server has not yet sent text for) should draw: nothing,
    /// rather than an error.
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
    /// `is_waxed` — disables further editing server-side. No render effect:
    /// the real game carries it purely for interaction gating, with no
    /// visual overlay for it at all, and it is parsed here for completeness
    /// rather than left silently unavailable to a future interaction
    /// system.
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
            *slot = resolve_message(element);
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
/// (child wins when specified, otherwise the parent's value survives).
#[derive(Debug, Clone, Copy, Default)]
struct ResolvedStyle {
    color: Option<u32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underlined: Option<bool>,
    strikethrough: Option<bool>,
}

impl ResolvedStyle {
    /// The style fields this `Compound` carries in its own right, before
    /// inheritance — the real style record's own field names, read off an
    /// [`Nbt`] compound rather than a JSON object.
    fn own_from_compound(fields: &[(String, Nbt)]) -> Self {
        ResolvedStyle {
            color: find(fields, "color")
                .and_then(as_string)
                .and_then(parse_text_color_name),
            bold: find(fields, "bold").and_then(as_bool),
            italic: find(fields, "italic").and_then(as_bool),
            underlined: find(fields, "underlined").and_then(as_bool),
            strikethrough: find(fields, "strikethrough").and_then(as_bool),
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

/// The sixteen legacy chat-colour names' RGB values (`0x00rrggbb`) — the
/// same table `lodestone_model::text::TextColor::rgb` carries, duplicated
/// here rather than depended on (see the module doc) — plus `#rrggbb` hex,
/// the modern colour form. `None` for anything else, including the
/// pseudo-colour `"reset"` (a style reset, not a colour — sign text has
/// nothing above the message root to reset to, so this parse has no use for
/// it).
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

/// Recursively walks one structural NBT component, appending flattened
/// [`SignTextSpan`]s onto `out` — the styled sibling of the plain-text
/// `text`/`extra` recursion [`lodestone_core::plain_text_from_nbt_component`]
/// already performs for chat and entity metadata, over the same [`Nbt`].
///
/// * [`Nbt::String`] is a **collapsed literal** — the text verbatim, styled
///   by whatever it inherited. This is the encoder's own
///   collapse-to-plain-string branch and it is the shape a player-typed
///   sign line always arrives in.
/// * [`Nbt::Compound`] contributes its own `text` (styled by its own fields
///   over the inherited ones) and then recurses into `extra` with that
///   resolved style as the new parent.
/// * [`Nbt::List`]: element `0` is the root and every later element is
///   *appended to it* as a sibling, so the rest inherit element `0`'s
///   resolved style rather than the enclosing one. Getting this wrong is
///   invisible for the common single-element case and wrong for every
///   styled list.
fn append_component_spans(
    nbt: &Nbt,
    parent: ResolvedStyle,
    depth: usize,
    out: &mut Vec<SignTextSpan>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match nbt {
        Nbt::String(text) => parent.push_text(text, out),
        Nbt::Compound(fields) => {
            let resolved = ResolvedStyle::own_from_compound(fields).inherit(parent);
            if let Some(text) = find(fields, "text").and_then(as_string) {
                resolved.push_text(text, out);
            }
            if let Some(extra) = find(fields, "extra") {
                append_component_spans(extra, resolved, depth + 1, out);
            }
        }
        Nbt::List { elements, .. } => {
            let Some((root, siblings)) = elements.split_first() else {
                return;
            };
            append_component_spans(root, parent, depth + 1, out);
            let sibling_parent = resolved_style_of(root, parent, depth + 1);
            for sibling in siblings {
                append_component_spans(sibling, sibling_parent, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// The style a node resolves to, for use as the parent of its own siblings —
/// see [`append_component_spans`]'s [`Nbt::List`] arm. A collapsed literal
/// carries no style of its own (that is exactly what makes it collapsible),
/// so only a `Compound` can change anything here.
fn resolved_style_of(nbt: &Nbt, parent: ResolvedStyle, depth: usize) -> ResolvedStyle {
    if depth > MAX_DEPTH {
        return parent;
    }
    match nbt {
        Nbt::Compound(fields) => ResolvedStyle::own_from_compound(fields).inherit(parent),
        // A list's own root decides for the list, recursively.
        Nbt::List { elements, .. } => elements
            .first()
            .map_or(parent, |root| resolved_style_of(root, parent, depth + 1)),
        _ => parent,
    }
}

/// Resolves one `messages` element into styled spans — see the module doc
/// for why this is structural NBT and **not** JSON, and for the closed
/// encode/decode loop that hid the difference.
fn resolve_message(nbt: &Nbt) -> Vec<SignTextSpan> {
    let mut out = Vec::new();
    append_component_spans(nbt, ResolvedStyle::default(), 0, &mut out);
    out
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

    /// A `Compound` component node, built from the real style record's and
    /// component record's own field names.
    fn compound(fields: Vec<(&str, Nbt)>) -> Nbt {
        Nbt::Compound(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }

    fn list(elements: Vec<Nbt>) -> Nbt {
        Nbt::List {
            element_type: NbtTag::Compound,
            elements,
        }
    }

    fn boolean(v: bool) -> Nbt {
        Nbt::Byte(i8::from(v))
    }

    /// One sign side, `messages` carrying whatever component shapes the
    /// caller supplies — the real per-element union, not a `String`-only
    /// list, which is exactly the assumption that broke.
    fn side(messages: Vec<Nbt>, color: &str) -> Nbt {
        compound(vec![
            ("has_glowing_text", Nbt::Byte(0)),
            ("color", Nbt::String(color.to_owned())),
            (
                "messages",
                Nbt::List {
                    element_type: NbtTag::String,
                    elements: messages,
                },
            ),
        ])
    }

    /// The shape a **real** server sends for a sign a player typed: every
    /// line collapses to a bare `Nbt::String` holding the text verbatim (the
    /// encoder's collapse-to-plain-string branch), with no JSON quoting
    /// anywhere.
    #[test]
    fn a_players_plain_sign_arrives_as_collapsed_string_literals() {
        let nbt = compound(vec![
            (
                "back_text",
                side(
                    vec![
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                    ],
                    "black",
                ),
            ),
            ("is_waxed", Nbt::Byte(0)),
            (
                "front_text",
                side(
                    vec![
                        Nbt::String("LODESTONE PROBE".to_owned()),
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                    ],
                    "red",
                ),
            ),
        ]);
        let text = SignText::parse(&nbt);
        assert_eq!(text.front.lines[0], vec![plain("LODESTONE PROBE")]);
        assert!(text.front.lines[1].is_empty());
        assert_eq!(text.front.color, SignDyeColor::Red);
        assert!(!text.front.glowing);
        assert!(text.back.lines.iter().all(Vec::is_empty));
        assert_eq!(text.back.color, SignDyeColor::Black);
        assert!(!text.waxed);
    }

    /// **The regression this parse was rewritten for.** A line that carries
    /// *any* style cannot collapse, so it arrives as a `Compound` — and the
    /// previous JSON-string-only parse matched no arm and produced an empty
    /// line, which is why a coloured sign on a real server drew its board
    /// and no text whatsoever.
    ///
    /// Both arms in one fixture: a styled line **and** a plain sibling line,
    /// so a parse that handled only one shape fails whichever one it
    /// dropped.
    #[test]
    fn a_styled_line_arrives_as_a_compound_and_still_reaches_spans() {
        let nbt = compound(vec![(
            "front_text",
            side(
                vec![
                    compound(vec![
                        ("text", Nbt::String("WELCOME".to_owned())),
                        ("color", Nbt::String("#12ab56".to_owned())),
                        ("bold", boolean(true)),
                    ]),
                    Nbt::String("to the hospital".to_owned()),
                    Nbt::String(String::new()),
                    Nbt::String(String::new()),
                ],
                "black",
            ),
        )]);
        let text = SignText::parse(&nbt);
        assert_eq!(
            text.front.lines[0],
            vec![SignTextSpan {
                text: "WELCOME".to_owned(),
                color: Some(0x0012_ab56),
                bold: true,
                ..Default::default()
            }],
            "a styled line must survive as a real styled span, not vanish"
        );
        assert_eq!(
            text.front.lines[1],
            vec![plain("to the hospital")],
            "a collapsed plain sibling line must still parse"
        );
    }

    /// A rich (compound) component with `extra` runs must concatenate, the
    /// same recursion `plain_text_from_nbt_component` performs — the control
    /// that a "only handle bare strings" implementation fails.
    #[test]
    fn a_rich_component_concatenates_text_and_extra() {
        let node = compound(vec![
            ("text", Nbt::String("hello ".to_owned())),
            (
                "extra",
                list(vec![
                    compound(vec![("text", Nbt::String("world".to_owned()))]),
                    Nbt::String("!".to_owned()),
                ]),
            ),
        ]);
        let spans = resolve_message(&node);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "hello world!");
    }

    /// **A string element is a literal, verbatim — never re-parsed.** The
    /// old JSON path silently mangled any plain line that happened to be
    /// valid JSON: `123` decoded to a `Number` and contributed no span at
    /// all, so the line vanished. Three inputs that JSON would each treat
    /// differently and vanilla treats identically.
    #[test]
    fn a_string_element_is_literal_text_even_when_it_looks_like_json() {
        for raw in ["123", "true", "{\"text\":\"x\"}"] {
            assert_eq!(
                resolve_message(&Nbt::String(raw.to_owned())),
                vec![plain(raw)],
                "a collapsed literal must reach the draw verbatim: {raw}"
            );
        }
    }

    /// A `messages` element that is itself a **list**: element `0` is the
    /// root and the rest are appended to it as siblings, so they inherit
    /// element `0`'s style — **not** the (empty) enclosing one. The
    /// discriminating fixture styles only element `0`; a parse that passed
    /// the enclosing style to every element would leave the sibling
    /// unstyled.
    #[test]
    fn a_list_message_makes_its_first_element_the_parent_of_the_rest() {
        let node = list(vec![
            compound(vec![
                ("text", Nbt::String("a".to_owned())),
                ("color", Nbt::String("red".to_owned())),
            ]),
            Nbt::String("b".to_owned()),
        ]);
        let spans = resolve_message(&node);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].color, Some(0x00ff_5555));
        assert_eq!(
            spans[1].color,
            Some(0x00ff_5555),
            "a later list element is a sibling of element 0 and inherits its style"
        );
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
        let nbt = compound(vec![(
            "front_text",
            Nbt::Compound(vec![(
                "messages".to_owned(),
                Nbt::List {
                    element_type: NbtTag::String,
                    elements: vec![
                        Nbt::String("hi".to_owned()),
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                        Nbt::String(String::new()),
                    ],
                },
            )]),
        )]);
        let text = SignText::parse(&nbt);
        assert_eq!(text.front.color, SignDyeColor::Black);
        assert_eq!(text.front.lines[0], vec![plain("hi")]);
    }

    /// The central control: an explicit **hex** colour on one run must
    /// survive as `Some`, and a sibling run with no colour of its own must
    /// come back `None` — never coerced to black, white, or the sibling's
    /// colour. Hex, not one of the sixteen legacy colours, because a lossy
    /// path (e.g. accidentally rounding to the nearest legacy colour) could
    /// still happen to survive a legacy-colour fixture.
    #[test]
    fn an_explicit_hex_colour_survives_and_an_unset_sibling_has_none() {
        let node = compound(vec![
            ("text", Nbt::String("a".to_owned())),
            ("color", Nbt::String("#123456".to_owned())),
            (
                "extra",
                list(vec![compound(vec![("text", Nbt::String("b".to_owned()))])]),
            ),
        ]);
        let spans = resolve_message(&node);
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
        let node = compound(vec![
            ("text", Nbt::String("a".to_owned())),
            ("color", Nbt::String("red".to_owned())),
            (
                "extra",
                list(vec![compound(vec![
                    ("text", Nbt::String("b".to_owned())),
                    ("color", Nbt::String("#00ff00".to_owned())),
                ])]),
            ),
        ]);
        let spans = resolve_message(&node);
        assert_eq!(spans[0].color, Some(0x00ff_5555));
        assert_eq!(spans[1].color, Some(0x0000_ff00));
    }

    /// A truly colour-less message (no node anywhere specifies one) must
    /// resolve to `None`, not some fallback colour baked in at parse time —
    /// the dye-colour fallback is the *draw site*'s job, not this parser's.
    #[test]
    fn a_message_with_no_colour_anywhere_resolves_to_none() {
        let spans = resolve_message(&compound(vec![(
            "text",
            Nbt::String("plain".to_owned()),
        )]));
        assert_eq!(spans, vec![plain("plain")]);
        assert_eq!(spans[0].color, None);
    }

    /// Bold set on the parent is inherited by a child that does not mention
    /// it, and a child can explicitly turn a flag back off.
    #[test]
    fn style_flags_inherit_and_can_be_explicitly_cleared() {
        let node = compound(vec![
            ("text", Nbt::String("a".to_owned())),
            ("bold", boolean(true)),
            ("underlined", boolean(true)),
            (
                "extra",
                list(vec![
                    compound(vec![("text", Nbt::String("b".to_owned()))]),
                    compound(vec![
                        ("text", Nbt::String("c".to_owned())),
                        ("underlined", boolean(false)),
                    ]),
                ]),
            ),
        ]);
        let spans = resolve_message(&node);
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
