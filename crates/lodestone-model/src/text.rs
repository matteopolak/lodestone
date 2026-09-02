//! Format-agnostic Minecraft chat/text components (the F3 `Text` seam).
//!
//! A Minecraft text component is a *tree*: a node carries some content (a
//! literal string or a `translate` key with arguments), a partial style, an
//! ordered list of `extra` child components, and optional interactivity
//! (`clickEvent`, `hoverEvent`, `insertion`). The **same tree** is serialized
//! two different ways on the wire depending on protocol era:
//!
//! * pre-1.20.3 (including 1.8, protocol 47) sends it as a **JSON string**;
//! * modern versions send it as **binary NBT**.
//!
//! Everything about a component *except the outer serialization* is identical
//! across versions: component structure, `extra` children, style inheritance
//! down the tree, `translate` with `with` arguments, click/hover events, and the
//! legacy `§` colour codes. So the tree type and every operation on it
//! (inheritance, flattening to plain text, formatting back to legacy codes) live
//! here **once**, and JSON ([`Text::from_json`]) and NBT ([`Text::from_nbt`])
//! are thin parsing front-ends that both produce the same [`Text`].
//!
//! This is what lets a cross-format oracle work: a semantically identical
//! message sent as JSON by a 1.8 server and as NBT by a modern server parses to
//! trees that flatten to the *same* plain text.

use lodestone_core::Nbt;

/// Maximum component nesting depth explored while parsing or flattening. Chat
/// components deeper than this are truncated rather than risking stack
/// exhaustion on hostile network input.
const MAX_DEPTH: usize = 64;

/// The section sign that introduces a legacy formatting code —
/// `ChatFormatting.PREFIX_CODE`, U+00A7. Named because it appears in a parser, a
/// re-serialiser and an expansion pass, and `'\u{00a7}'` at a call site reads as
/// an arbitrary codepoint.
pub const LEGACY_PREFIX: char = '\u{00a7}';

/// A Minecraft text colour: one of the sixteen named colours, or a modern
/// 24-bit hex colour (`#rrggbb`, introduced in 1.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextColor {
    /// `black` (`§0`).
    Black,
    /// `dark_blue` (`§1`).
    DarkBlue,
    /// `dark_green` (`§2`).
    DarkGreen,
    /// `dark_aqua` (`§3`).
    DarkAqua,
    /// `dark_red` (`§4`).
    DarkRed,
    /// `dark_purple` (`§5`).
    DarkPurple,
    /// `gold` (`§6`).
    Gold,
    /// `gray` (`§7`).
    Gray,
    /// `dark_gray` (`§8`).
    DarkGray,
    /// `blue` (`§9`).
    Blue,
    /// `green` (`§a`).
    Green,
    /// `aqua` (`§b`).
    Aqua,
    /// `red` (`§c`).
    Red,
    /// `light_purple` (`§d`).
    LightPurple,
    /// `yellow` (`§e`).
    Yellow,
    /// `white` (`§f`).
    White,
    /// A 24-bit RGB colour (modern hex colour), stored as `0x00rrggbb`.
    Rgb(u32),
}

impl TextColor {
    /// The sixteen named colours in `§`-code order (`0`..=`f`).
    const NAMED: [(Self, char, &'static str); 16] = [
        (Self::Black, '0', "black"),
        (Self::DarkBlue, '1', "dark_blue"),
        (Self::DarkGreen, '2', "dark_green"),
        (Self::DarkAqua, '3', "dark_aqua"),
        (Self::DarkRed, '4', "dark_red"),
        (Self::DarkPurple, '5', "dark_purple"),
        (Self::Gold, '6', "gold"),
        (Self::Gray, '7', "gray"),
        (Self::DarkGray, '8', "dark_gray"),
        (Self::Blue, '9', "blue"),
        (Self::Green, 'a', "green"),
        (Self::Aqua, 'b', "aqua"),
        (Self::Red, 'c', "red"),
        (Self::LightPurple, 'd', "light_purple"),
        (Self::Yellow, 'e', "yellow"),
        (Self::White, 'f', "white"),
    ];

    /// Parses a colour name as it appears in JSON/NBT: a named colour
    /// (`"red"`), or a hex colour (`"#ff00ff"`). Returns `None` for anything
    /// else (including the pseudo-colour `"reset"`, which is a style reset, not
    /// a colour).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        if let Some(hex) = name.strip_prefix('#') {
            if hex.len() == 6 {
                return u32::from_str_radix(hex, 16).ok().map(Self::Rgb);
            }
            return None;
        }
        Self::NAMED
            .iter()
            .find(|(_, _, text)| *text == name)
            .map(|(color, _, _)| *color)
    }

    /// The canonical colour name (`"red"`, or `"#ff00ff"` for a hex colour).
    #[must_use]
    pub fn name(&self) -> String {
        if let Self::Rgb(value) = self {
            return format!("#{:06x}", value & 0x00ff_ffff);
        }
        Self::NAMED
            .iter()
            .find(|(color, _, _)| color == self)
            .map(|(_, _, text)| (*text).to_owned())
            .unwrap_or_default()
    }

    /// The legacy `§` code character, if this is one of the sixteen named
    /// colours. Hex colours have no legacy representation.
    #[must_use]
    pub fn legacy_code(&self) -> Option<char> {
        Self::NAMED
            .iter()
            .find(|(color, _, _)| color == self)
            .map(|(_, code, _)| *code)
    }

    /// The named colour for a legacy `§` code (`0`..=`9`, `a`..=`f`, either
    /// case), or `None` for a format/reset code.
    ///
    /// Public so a renderer can reach the sixteen RGB values through
    /// [`Self::rgb`] instead of keeping its own `§`-keyed copy of the table. The
    /// shell had exactly such a duplicate, and a second transcription of
    /// sixteen hex constants is a drift hazard with no upside.
    #[must_use]
    pub fn from_legacy_code(code: char) -> Option<Self> {
        let lower = code.to_ascii_lowercase();
        Self::NAMED
            .iter()
            .find(|(_, c, _)| *c == lower)
            .map(|(color, _, _)| *color)
    }

    /// This colour's 24-bit RGB value, packed as `0x00rrggbb`.
    ///
    /// The sixteen named values are vanilla's own, transcribed from
    /// `TextColor.java` in 26.2. **Do not look for them in
    /// `ChatFormatting`**: in 26.2 that enum's constructor is
    /// `ChatFormatting(final char code)` and carries *no colour at all* — the
    /// obvious place to check is empty, and its emptiness looks like "vanilla
    /// has no table" rather than "the table moved". Vanilla writes them in
    /// decimal (`named("gold", 16755200)`), so each arm below carries the
    /// decimal alongside, because the hex is the reviewable form and the
    /// decimal is the citable one.
    ///
    /// This is the only bridge from a model colour to a pixel colour. Before it
    /// existed the sole route was `legacy_code()` → `char` → the renderer's own
    /// `§`-keyed table, which structurally **cannot** carry [`Self::Rgb`]: a
    /// hex colour has no legacy code, so `legacy_code()` returned `None` and
    /// every 1.16+ hex-coloured run silently fell back to the base colour.
    #[must_use]
    pub fn rgb(&self) -> u32 {
        match self {
            Self::Black => 0x0000_0000,       // 0
            Self::DarkBlue => 0x0000_00aa,    // 170
            Self::DarkGreen => 0x0000_aa00,   // 43520
            Self::DarkAqua => 0x0000_aaaa,    // 43690
            Self::DarkRed => 0x00aa_0000,     // 11141120
            Self::DarkPurple => 0x00aa_00aa,  // 11141290
            Self::Gold => 0x00ff_aa00,        // 16755200
            Self::Gray => 0x00aa_aaaa,        // 11184810
            Self::DarkGray => 0x0055_5555,    // 5592405
            Self::Blue => 0x0055_55ff,        // 5592575
            Self::Green => 0x0055_ff55,       // 5635925
            Self::Aqua => 0x0055_ffff,        // 5636095
            Self::Red => 0x00ff_5555,         // 16733525
            Self::LightPurple => 0x00ff_55ff, // 16733695
            Self::Yellow => 0x00ff_ff55,      // 16777045
            Self::White => 0x00ff_ffff,       // 16777215
            // `TextColor(final int value)` masks with 16777215 (`TextColor.java`),
            // so a hex colour carrying stray high bits is truncated, not rejected.
            Self::Rgb(value) => value & 0x00ff_ffff,
        }
    }
}

/// An interned `"namespace:path"` font identifier, as carried by
/// [`TextStyle::font`].
///
/// A resource location is a plain [`String`] everywhere else in this crate's
/// text model, but [`TextStyle`] is `Copy` — every existing consumer resolves
/// inheritance down a tree by copying it, not cloning it, and it is embedded
/// by value in [`TextSpan`]/[`InteractiveTextSpan`], which key a wrap cache
/// off `Hash`. Adding a `String` field would end `Copy` for all of them.
/// Interning keeps the field a plain `u32` instead: [`Self::intern`] leaks the
/// backing string once per distinct id (font ids are a small, effectively
/// bounded set — the fonts a session's active resource packs define — not one
/// per message), and [`Self::name`] hands back the `'static` string with no
/// lock held past the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontId(u32);

/// Backing table for [`FontId`]. A `Mutex` rather than a lock-free structure:
/// interning only happens while parsing a component (JSON/NBT decode), never
/// per-frame in a draw loop, so contention is not a concern.
struct FontInterner {
    names: Vec<&'static str>,
    ids: std::collections::HashMap<&'static str, u32>,
}

fn font_interner() -> &'static std::sync::Mutex<FontInterner> {
    static INTERNER: std::sync::OnceLock<std::sync::Mutex<FontInterner>> =
        std::sync::OnceLock::new();
    INTERNER.get_or_init(|| {
        std::sync::Mutex::new(FontInterner {
            names: Vec::new(),
            ids: std::collections::HashMap::new(),
        })
    })
}

impl FontId {
    /// Interns `name` (a `"namespace:path"` resource location, e.g.
    /// `"democracycraft:icons"`), returning the same [`FontId`] for the same
    /// string on every call.
    #[must_use]
    pub fn intern(name: &str) -> Self {
        let mut interner = font_interner()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(&id) = interner.ids.get(name) {
            return FontId(id);
        }
        let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
        let id = u32::try_from(interner.names.len()).unwrap_or(u32::MAX);
        interner.names.push(leaked);
        interner.ids.insert(leaked, id);
        FontId(id)
    }

    /// The `"namespace:path"` string this id was interned from.
    #[must_use]
    pub fn name(self) -> &'static str {
        let interner = font_interner()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        interner.names.get(self.0 as usize).copied().unwrap_or("minecraft:default")
    }
}

/// Formatting attributes for a [`Text`] component.
///
/// Every attribute is an [`Option`], and the distinction is load-bearing:
/// `None` means **unspecified** (inherit from the parent), while `Some(false)`
/// means **explicitly disabled** (do not inherit — turn it off even if the
/// parent had it on). Collapsing these two into a plain `bool` looks correct on
/// flat messages and silently corrupts nested ones, which is exactly when it is
/// hardest to debug. See [`TextStyle::inherit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextStyle {
    /// Text colour (`None` = inherit).
    pub color: Option<TextColor>,
    /// Bold (`None` = inherit, `Some(false)` = explicitly not bold).
    pub bold: Option<bool>,
    /// Italic (`None` = inherit).
    pub italic: Option<bool>,
    /// Underlined (`None` = inherit).
    pub underlined: Option<bool>,
    /// Struck through (`None` = inherit).
    pub strikethrough: Option<bool>,
    /// Obfuscated (`None` = inherit).
    pub obfuscated: Option<bool>,
    /// The `"font": "<namespace>:<name>"` a text component can request
    /// (`None` = inherit, and at the root, the client's default font). Vanilla
    /// resolves this against the active resource pack's `font/*.json`
    /// definitions; this model only carries the id — a draw surface looks it
    /// up (see `lodestone-shell`'s `hud::vanilla_font`).
    pub font: Option<FontId>,
}

impl TextStyle {
    /// Resolves this (child) style against its `parent`'s already-resolved
    /// style. For every attribute the child's own value wins when it is
    /// specified (`Some`), otherwise the parent's value is inherited. This is
    /// the one place inheritance is defined; both flattening and formatting go
    /// through it.
    #[must_use]
    pub fn inherit(&self, parent: &TextStyle) -> TextStyle {
        TextStyle {
            color: self.color.or(parent.color),
            bold: self.bold.or(parent.bold),
            italic: self.italic.or(parent.italic),
            underlined: self.underlined.or(parent.underlined),
            strikethrough: self.strikethrough.or(parent.strikethrough),
            obfuscated: self.obfuscated.or(parent.obfuscated),
            font: self.font.or(parent.font),
        }
    }

    /// Whether every attribute is unspecified.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == TextStyle::default()
    }
}

/// A `clickEvent` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickAction {
    /// Open a URL in the browser.
    OpenUrl,
    /// Open a file (single-player only).
    OpenFile,
    /// Run a command as the player.
    RunCommand,
    /// Place text in the player's chat input.
    SuggestCommand,
    /// Change page in a written book.
    ChangePage,
    /// Copy text to the clipboard (1.15+).
    CopyToClipboard,
    /// An action name this version of the model does not recognise.
    Other(String),
}

impl ClickAction {
    fn from_name(name: &str) -> Self {
        match name {
            "open_url" => Self::OpenUrl,
            "open_file" => Self::OpenFile,
            "run_command" => Self::RunCommand,
            "suggest_command" => Self::SuggestCommand,
            "change_page" => Self::ChangePage,
            "copy_to_clipboard" => Self::CopyToClipboard,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A `clickEvent`: an action plus its string value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickEvent {
    /// What clicking does.
    pub action: ClickAction,
    /// The action's argument (URL, command text, page number, ...).
    pub value: String,
}

/// A `hoverEvent` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverAction {
    /// Show a text tooltip.
    ShowText,
    /// Show an item tooltip (payload kept as raw text).
    ShowItem,
    /// Show an entity tooltip (payload kept as raw text).
    ShowEntity,
    /// An action name this version of the model does not recognise.
    Other(String),
}

/// A `hoverEvent`. For `show_text` the payload is itself a text component; for
/// the item/entity variants the payload is preserved as a literal text node so
/// no information is lost even though this model does not interpret it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverEvent {
    /// What hovering shows.
    pub action: HoverAction,
    /// The tooltip contents.
    pub value: Box<Text>,
}

/// The content of a single text node: either a literal string, or a translation
/// key with arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextContent {
    /// A literal string.
    Literal(String),
    /// A client-side translated message: a translation `key`, its ordered
    /// arguments (`with`), and an optional `fallback` format string used when
    /// the key is unknown.
    Translate {
        /// The translation key, e.g. `multiplayer.player.joined`.
        key: String,
        /// Ordered substitution arguments, each itself a component.
        with: Vec<Text>,
        /// Optional fallback format string (modern `fallback` field).
        fallback: Option<String>,
    },
}

impl Default for TextContent {
    fn default() -> Self {
        Self::Literal(String::new())
    }
}

/// A version-free Minecraft chat component tree.
///
/// Construct literals with [`Text::literal`] and translations with
/// [`Text::translate`]; parse wire forms with [`Text::from_json`] /
/// [`Text::from_nbt`] / [`Text::from_legacy`]; and render with
/// [`Text::to_plain_string`] or [`Text::to_legacy_string`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Text {
    /// This node's own content.
    pub content: TextContent,
    /// Style applied to this node (partial; unspecified attributes inherit).
    pub style: TextStyle,
    /// Child components rendered after this node's content, inheriting its
    /// resolved style.
    pub extra: Vec<Text>,
    /// Optional click interaction.
    pub click: Option<ClickEvent>,
    /// Optional hover interaction.
    pub hover: Option<HoverEvent>,
    /// Optional shift-click insertion text.
    pub insertion: Option<String>,
}

/// A resolved run of text with its fully-inherited style, produced by
/// [`Text::to_spans`].
///
/// `Hash` (alongside the derived `Eq`) is what lets a `Vec<TextSpan>` key a
/// wrap cache the way a plain `String` already keys `hud::ChatWrapCache` —
/// see `hud::ChatWrapCacheSpans`, the span-aware sibling that exists because a
/// `TextColor::Rgb` cannot survive being flattened to a `§`-coded `String`
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextSpan {
    /// The plain text of this run.
    pub text: String,
    /// The fully-resolved style (all inheritance applied).
    pub style: TextStyle,
}

/// [`TextSpan`]'s interactive sibling: the same flattened, fully-inherited run,
/// plus whichever `click`/`hover`/`insertion` apply to it — produced by
/// [`Text::to_interactive_spans`].
///
/// **A new type rather than new fields on [`TextSpan`] itself.** `click_event`/
/// `hover_event` decode into [`Text::click`]/[`Text::hover`] correctly and
/// always have (see `json_click`/`json_hover`/`nbt_click`/`nbt_hover`), but
/// [`Text::to_spans`] — the function every existing consumer flattens a tree
/// through — never read them, so they were silently discarded exactly at the
/// tree-to-span boundary. Sixteen call sites across `lodestone-shell` build a
/// `TextSpan` struct literal directly, so widening that type would be a
/// breaking change landing blind in a crate two other agents hold. This is
/// additive instead: a chat hit-test can call [`Text::to_interactive_spans`]
/// once it exists, and nothing that already calls [`Text::to_spans`] changes.
///
/// No `Hash` derive (unlike [`TextSpan`]): [`HoverEvent`] carries a `Box<Text>`
/// payload, and hashing a whole nested component tree is not a cost this type
/// should impose on every cache lookup the way [`TextSpan`]'s flat style does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveTextSpan {
    /// The plain text of this run.
    pub text: String,
    /// The fully-resolved style (all inheritance applied).
    pub style: TextStyle,
    /// The click action in effect for this run — this node's own
    /// [`Text::click`], or the nearest ancestor's, the same
    /// child-overrides-parent inheritance [`TextStyle::inherit`] uses.
    pub click: Option<ClickEvent>,
    /// The hover action in effect for this run, inherited the same way as
    /// [`Self::click`].
    pub hover: Option<HoverEvent>,
    /// The shift-click insertion text in effect for this run, inherited the
    /// same way as [`Self::click`].
    pub insertion: Option<String>,
}

impl Text {
    /// Creates a literal text component.
    #[must_use]
    pub fn literal(content: impl Into<String>) -> Self {
        Self {
            content: TextContent::Literal(content.into()),
            ..Self::default()
        }
    }

    /// Creates a `translate` component with the given key and arguments.
    #[must_use]
    pub fn translate(key: impl Into<String>, with: Vec<Text>) -> Self {
        Self {
            content: TextContent::Translate {
                key: key.into(),
                with,
                fallback: None,
            },
            ..Self::default()
        }
    }

    /// Flattens this tree to plain text using the built-in English translation
    /// table (see [`default_translation`]). Style and interactivity are
    /// ignored; this is the canonical "what does the message say" operation and
    /// the basis of the cross-format oracle.
    ///
    /// # This is not a translator
    ///
    /// [`default_translation`] is a **fourteen-key stub** — chat, join/leave and
    /// six death messages. Every other key falls through to the component's
    /// `fallback`, then to the key itself. So on any component a real server
    /// authored, this method is only as correct as the tree is *already resolved*:
    ///
    /// ```text
    /// Text::translate("container.crafting", vec![]).to_plain_string()
    ///     == "container.crafting"      // the raw key, on screen
    /// ```
    ///
    /// Safe uses are (a) a tree with no `translate` nodes at all — notably the
    /// output of `lodestone_game::text::resolve`, which lowers every `translate`
    /// node to a literal first, and (b) logs, panics and tests, where a key is a
    /// perfectly good identifier.
    ///
    /// **Anything that reaches a pixel must go through the language table**, i.e.
    /// `lodestone_game::text::resolve_to_string(&text, translate)` or
    /// [`Self::to_plain_string_with`]. Four shell surfaces already do (chat, the
    /// tab list, the scoreboard sidebar, boss bars); the container-screen title
    /// did not, and shipped `container.crafting` where "Crafting" belonged.
    #[must_use]
    pub fn to_plain_string(&self) -> String {
        self.to_plain_string_with(&default_translation)
    }

    /// Like [`Text::to_plain_string`] but resolves translation keys through a
    /// caller-supplied table. `translate` returns the format string for a key,
    /// or `None` to fall back to the component's `fallback`/key.
    #[must_use]
    pub fn to_plain_string_with(&self, translate: &dyn Fn(&str) -> Option<String>) -> String {
        let mut out = String::new();
        self.write_plain(&mut out, translate, 0);
        out
    }

    fn write_plain(
        &self,
        out: &mut String,
        translate: &dyn Fn(&str) -> Option<String>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        match &self.content {
            TextContent::Literal(text) => out.push_str(text),
            TextContent::Translate {
                key,
                with,
                fallback,
            } => {
                let pattern = translate(key)
                    .or_else(|| fallback.clone())
                    .unwrap_or_else(|| key.clone());
                write_translation(&pattern, with, out, translate, depth);
            }
        }
        for child in &self.extra {
            child.write_plain(out, translate, depth + 1);
        }
    }

    /// Flattens this tree into styled runs ready to draw: inheritance resolved
    /// against an empty root style, **and** legacy `§` codes found inside
    /// literal content expanded into their own runs.
    ///
    /// This is the one function a render surface should call. Vanilla has no
    /// non-expanding string path either — `Font.drawInBatch` and `Font.width`
    /// both go through `StringDecomposer.iterateFormatted`, which applies `§`
    /// codes at *draw* time. That is exactly why a plugin server can put `§7`
    /// inside a modern component and have it colour, and why a client that
    /// flattens without expanding puts `§7` on screen as two glyphs.
    ///
    /// Both conventions exist in one field, and a server-list MOTD is where they
    /// collide hardest: `description` arrives as a bare string full of `§c`
    /// codes, or as a component tree with `color` keys, or — routinely — as a
    /// component tree whose `text` values *also* contain `§` codes, because the
    /// server built the string with a legacy formatter and wrapped it in modern
    /// JSON. All three shapes are handled here; only the second is handled by
    /// [`to_spans_ignoring_legacy_codes`](Self::to_spans_ignoring_legacy_codes).
    ///
    /// An expanded run inherits the enclosing component's style
    /// ([`TextStyle::inherit`]), so `{"color":"gold","text":"a§cb"}` yields gold
    /// `a` then red `b` — the legacy code overrides the colour it names and the
    /// component's colour still governs the run before it. `§r` resets to the
    /// *enclosing component's* style rather than to nothing, which is
    /// `iterateFormatted`'s `resetStyle` parameter: it is seeded with the
    /// component's own style, not `Style.EMPTY`.
    ///
    /// Adjacent runs are **not** merged. `translate` nodes are rendered as a
    /// single run carrying the node's resolved style (their argument sub-styles
    /// collapse to plain within that run); literal and `extra` inheritance is
    /// modelled exactly.
    #[must_use]
    pub fn to_spans(&self) -> Vec<TextSpan> {
        let mut out = Vec::new();
        for span in self.to_spans_ignoring_legacy_codes() {
            if !span.text.contains(LEGACY_PREFIX) {
                out.push(span);
                continue;
            }
            // `from_legacy` consumes every `§`+code pair, so the inner spans can
            // carry no `§` of their own and this cannot recurse.
            for inner in Self::from_legacy(&span.text).to_spans_ignoring_legacy_codes() {
                out.push(TextSpan {
                    text: inner.text,
                    style: inner.style.inherit(&span.style),
                });
            }
        }
        out
    }

    /// Flattens this tree into styled runs, resolving inheritance against an
    /// empty root style, **without** expanding legacy `§` codes inside literal
    /// content — so a `§7` in a component's `text` survives into a span as two
    /// literal characters.
    ///
    /// **Not for rendering.** A render surface that calls this draws `§7` as
    /// glyphs, which is the defect the long name exists to advertise; use
    /// [`to_spans`](Self::to_spans). `render_surfaces_do_not_bypass_legacy_expansion`
    /// in this crate's `tests/legacy_expansion_guard.rs` enforces that
    /// mechanically, because a doc comment stating an invariant is documentation
    /// of intent and not a guard.
    ///
    /// The two legitimate uses are re-serialisation — [`to_legacy_string`], which
    /// is putting the codes *back* and must not double-expand them — and
    /// [`to_spans`]'s own inner pass.
    ///
    /// [`to_legacy_string`]: Self::to_legacy_string
    /// [`to_spans`]: Self::to_spans
    #[must_use]
    pub fn to_spans_ignoring_legacy_codes(&self) -> Vec<TextSpan> {
        let mut spans = Vec::new();
        self.collect_spans(&TextStyle::default(), &default_translation, &mut spans, 0);
        spans
    }

    fn collect_spans(
        &self,
        parent: &TextStyle,
        translate: &dyn Fn(&str) -> Option<String>,
        out: &mut Vec<TextSpan>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let style = self.style.inherit(parent);
        let mut own = String::new();
        match &self.content {
            TextContent::Literal(text) => own.push_str(text),
            TextContent::Translate {
                key,
                with,
                fallback,
            } => {
                let pattern = translate(key)
                    .or_else(|| fallback.clone())
                    .unwrap_or_else(|| key.clone());
                write_translation(&pattern, with, &mut own, translate, depth);
            }
        }
        if !own.is_empty() {
            out.push(TextSpan { text: own, style });
        }
        for child in &self.extra {
            child.collect_spans(&style, translate, out, depth + 1);
        }
    }

    /// [`Self::to_spans`]'s interactive sibling: the same flattening, plus
    /// `click`/`hover`/`insertion` — see [`InteractiveTextSpan`]'s own doc for
    /// why this is a separate method rather than a change to [`TextSpan`].
    #[must_use]
    pub fn to_interactive_spans(&self) -> Vec<InteractiveTextSpan> {
        let mut out = Vec::new();
        for span in self.to_interactive_spans_ignoring_legacy_codes() {
            if !span.text.contains(LEGACY_PREFIX) {
                out.push(span);
                continue;
            }
            // Same reasoning as `to_spans`: `from_legacy` consumes every
            // `§`+code pair, so the inner spans carry no `§` of their own and
            // this cannot recurse. The inner text is freshly parsed from a
            // plain string, so it has no click/hover/insertion of its own —
            // the outer span's (already fully inherited) values apply to
            // every piece it splits into, the same way its style does.
            for inner in Self::from_legacy(&span.text).to_spans_ignoring_legacy_codes() {
                out.push(InteractiveTextSpan {
                    text: inner.text,
                    style: inner.style.inherit(&span.style),
                    click: span.click.clone(),
                    hover: span.hover.clone(),
                    insertion: span.insertion.clone(),
                });
            }
        }
        out
    }

    /// [`Self::to_spans_ignoring_legacy_codes`]'s interactive sibling.
    #[must_use]
    fn to_interactive_spans_ignoring_legacy_codes(&self) -> Vec<InteractiveTextSpan> {
        let mut spans = Vec::new();
        self.collect_interactive_spans(
            &TextStyle::default(),
            None,
            None,
            None,
            &default_translation,
            &mut spans,
            0,
        );
        spans
    }

    /// [`Self::collect_spans`]'s interactive sibling. `parent_click`/
    /// `parent_hover`/`parent_insertion` thread down exactly the way
    /// `parent: &TextStyle` already does — a node's own value wins when
    /// present, else the nearest ancestor's applies, matching vanilla's
    /// `Style` inheritance (`click_event`/`hover_event`/`insertion` are
    /// ordinary `Style` fields there, inherited the same way as colour).
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors collect_spans's own shape, widened by the three inherited events"
    )]
    fn collect_interactive_spans(
        &self,
        parent_style: &TextStyle,
        parent_click: Option<&ClickEvent>,
        parent_hover: Option<&HoverEvent>,
        parent_insertion: Option<&String>,
        translate: &dyn Fn(&str) -> Option<String>,
        out: &mut Vec<InteractiveTextSpan>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let style = self.style.inherit(parent_style);
        let click = self.click.as_ref().or(parent_click);
        let hover = self.hover.as_ref().or(parent_hover);
        let insertion = self.insertion.as_ref().or(parent_insertion);
        let mut own = String::new();
        match &self.content {
            TextContent::Literal(text) => own.push_str(text),
            TextContent::Translate {
                key,
                with,
                fallback,
            } => {
                let pattern = translate(key)
                    .or_else(|| fallback.clone())
                    .unwrap_or_else(|| key.clone());
                write_translation(&pattern, with, &mut own, translate, depth);
            }
        }
        if !own.is_empty() {
            out.push(InteractiveTextSpan {
                text: own,
                style: style.clone(),
                click: click.cloned(),
                hover: hover.cloned(),
                insertion: insertion.cloned(),
            });
        }
        for child in &self.extra {
            child.collect_interactive_spans(
                &style, click, hover, insertion, translate, out, depth + 1,
            );
        }
    }

    /// Renders this tree back to a legacy `§`-code string. Colour and each
    /// active format flag are emitted as codes ahead of each run; a `§r` reset
    /// is emitted whenever a run turns a flag *off* relative to the previous
    /// run. Hex colours (no legacy code) are dropped.
    ///
    /// Flattens with [`to_spans_ignoring_legacy_codes`](Self::to_spans_ignoring_legacy_codes)
    /// on purpose: this function is putting `§` codes *back*, so a code already
    /// literal in the tree's content is passed through unchanged rather than
    /// expanded and re-emitted.
    ///
    /// # What this is for
    ///
    /// Two call shapes are legitimate; a third is a bug.
    ///
    /// 1. **Serialising into an actual legacy-string wire field** — a
    ///    protocol whose own packet definition carries a `§`-coded string,
    ///    e.g. `v47`/`v340`'s pre-1.13 `SCOREBOARD_TEAM` prefix/suffix
    ///    (verified in both adapters' own decode arms), where the flattening
    ///    is the wire format's own lossiness, not a bug we introduced. No
    ///    encoder in this workspace constructs such a field today —
    ///    `v47`/`v340` only ever *decode* one (`Text::from_legacy`, the
    ///    reverse direction), because neither implements `ServerProtocol` and
    ///    so never emits a clientbound `SCOREBOARD_TEAM` of its own. Keep
    ///    this method for when one does.
    /// 2. **A colour-blind, non-drawing use** — box-width measurement through
    ///    a `§`-aware `font.width`, or an identity/equality comparison that
    ///    only needs a stable content key — where the flattened string is
    ///    never itself painted to the screen, so a dropped hex colour changes
    ///    nothing about the result.
    ///
    /// Anything **draw-adjacent** — building the string a renderer actually
    /// puts on screen — is a bug: hex colours (`TextColor::Rgb`, added in
    /// 1.16) have no legacy code and silently vanish. Use
    /// [`to_spans`](Self::to_spans) and draw the spans instead. This was the
    /// shape of three now-fixed production bugs (`styled_hover_name`'s
    /// tooltip title/held-item draw sites, `ChatLog::recent`'s HUD draw
    /// path, and `v735`'s `TAB_COMPLETE` tooltip decode, which used to
    /// flatten straight into this call before `CommandSuggestionEntry::
    /// tooltip` was widened to carry a real [`Text`] end to end).
    #[must_use]
    pub fn to_legacy_string(&self) -> String {
        let mut out = String::new();
        let mut previous = TextStyle::default();
        for span in self.to_spans_ignoring_legacy_codes() {
            let style = span.style;
            let turns_off = flag_on(previous.bold) && !flag_on(style.bold)
                || flag_on(previous.italic) && !flag_on(style.italic)
                || flag_on(previous.underlined) && !flag_on(style.underlined)
                || flag_on(previous.strikethrough) && !flag_on(style.strikethrough)
                || flag_on(previous.obfuscated) && !flag_on(style.obfuscated)
                || (previous.color.is_some() && style.color != previous.color);
            if turns_off {
                out.push_str("§r");
                previous = TextStyle::default();
            }
            if let Some(color) = style.color
                && previous.color != Some(color)
                && let Some(code) = color.legacy_code()
            {
                out.push('§');
                out.push(code);
            }
            push_flag(&mut out, 'l', previous.bold, style.bold);
            push_flag(&mut out, 'o', previous.italic, style.italic);
            push_flag(&mut out, 'n', previous.underlined, style.underlined);
            push_flag(&mut out, 'm', previous.strikethrough, style.strikethrough);
            push_flag(&mut out, 'k', previous.obfuscated, style.obfuscated);
            out.push_str(&span.text);
            previous = style;
        }
        out
    }

    /// Parses legacy section-sign formatting codes into a styled tree.
    ///
    /// Each `§`-code starts a new sibling run. A colour code resets all
    /// formatting (legacy semantics); `§r` resets to default.
    ///
    /// # An unrecognised code, and a dangling `§`, are both *dropped*
    ///
    /// Not printed literally, and not partially printed. This is
    /// `StringDecomposer.iterateFormatted`, whose `§` branch is:
    ///
    /// ```text
    /// if (ch == 167) {
    ///    if (i + 1 >= size) break;               // dangling §: the § is dropped
    ///    ChatFormatting f = ChatFormatting.getByCode(string.charAt(i + 1));
    ///    if (f != null) { … apply … }            // null: style untouched …
    ///    i++;                                    // … but i++ runs regardless
    /// }
    /// ```
    ///
    /// `i++` is outside the `f != null` test, so an unrecognised pair consumes
    /// **both** characters and emits neither — the sink never sees them. Three
    /// answers were plausible here (print the pair, drop the `§` and keep the
    /// code, drop both) and this is the one vanilla gives; the previous version
    /// of this function chose the first.
    ///
    /// The consequence worth knowing: `§x§r§r§g§g§b§b`, the BungeeCord hex
    /// dialect, is **not** honoured by vanilla 26.2 — `getByCode('x')` is null,
    /// so `§x` vanishes and the six following pairs are read as six ordinary
    /// colour codes, leaving the run coloured by the last one. Ours does the
    /// same, deliberately: a client that honoured the dialect would disagree with
    /// vanilla on every such string.
    #[must_use]
    pub fn from_legacy(input: &str) -> Self {
        let mut root = Self::default();
        let mut current = TextStyle::default();
        let mut buffer = String::new();
        let mut chars = input.chars().peekable();

        while let Some(character) = chars.next() {
            if character != LEGACY_PREFIX {
                buffer.push(character);
                continue;
            }
            // Dangling `§`: vanilla `break`s without feeding it to the sink.
            let Some(code) = chars.next() else { break };
            if let Some(next) = apply_legacy_code(current, code) {
                flush_legacy_segment(&mut root, &mut buffer, current);
                current = next;
            }
            // Unrecognised code: both characters already consumed, style
            // untouched, nothing emitted — vanilla's unconditional `i++`.
        }
        flush_legacy_segment(&mut root, &mut buffer, current);
        root
    }

    /// Parses a 1.8-style JSON chat component into a [`Text`]. On any parse
    /// failure the raw input is returned as a literal (surfacing *something* to
    /// the user is better than an error for malformed server text). Never
    /// panics and is depth-limited against hostile input.
    #[must_use]
    pub fn from_json(input: &str) -> Self {
        match JsonValue::parse(input) {
            Some(value) => text_from_json(&value, 0),
            None => Text::literal(input),
        }
    }

    /// Parses a modern NBT chat component (as decoded by
    /// [`lodestone_core::read_network_nbt`]) into a [`Text`]. Non-panicking and
    /// depth-limited.
    #[must_use]
    pub fn from_nbt(nbt: &Nbt) -> Self {
        text_from_nbt(nbt, 0)
    }
}

const fn flag_on(value: Option<bool>) -> bool {
    matches!(value, Some(true))
}

fn push_flag(out: &mut String, code: char, previous: Option<bool>, current: Option<bool>) {
    if flag_on(current) && !flag_on(previous) {
        out.push('§');
        out.push(code);
    }
}

/// Substitutes `with` arguments into a translation format `pattern`, supporting
/// `%s` (sequential), `%N$s` (indexed, 1-based), and `%%` (literal `%`).
fn write_translation(
    pattern: &str,
    with: &[Text],
    out: &mut String,
    translate: &dyn Fn(&str) -> Option<String>,
    depth: usize,
) {
    let mut chars = pattern.chars().peekable();
    let mut next_auto = 0usize;
    while let Some(character) = chars.next() {
        if character != '%' {
            out.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some('s') => {
                chars.next();
                push_arg(with, next_auto, out, translate, depth);
                next_auto += 1;
            }
            Some(digit) if digit.is_ascii_digit() => {
                let mut index = 0usize;
                while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                    chars.next();
                    index = index
                        .saturating_mul(10)
                        .saturating_add((d as usize) - ('0' as usize));
                }
                // Consume the `$s` that follows an indexed argument.
                if chars.peek() == Some(&'$') {
                    chars.next();
                    if chars.peek() == Some(&'s') {
                        chars.next();
                    }
                }
                push_arg(with, index.saturating_sub(1), out, translate, depth);
            }
            _ => out.push('%'),
        }
    }
}

fn push_arg(
    with: &[Text],
    index: usize,
    out: &mut String,
    translate: &dyn Fn(&str) -> Option<String>,
    depth: usize,
) {
    if let Some(arg) = with.get(index) {
        arg.write_plain(out, translate, depth + 1);
    }
}

fn flush_legacy_segment(root: &mut Text, buffer: &mut String, style: TextStyle) {
    if buffer.is_empty() {
        return;
    }
    root.extra.push(Text {
        content: TextContent::Literal(std::mem::take(buffer)),
        style,
        ..Text::default()
    });
}

/// `Style.applyLegacyFormat`, as a `TextStyle` transform. `None` means the code
/// is not one vanilla's `ChatFormatting.getByCode` recognises.
///
/// Two asymmetries, and swapping them makes `§c§lFoo` render in a way that looks
/// almost right:
///
/// * **A colour code clears the five flags; a formatting code leaves the colour
///   alone.** Vanilla's `default:` arm (every colour) assigns
///   `bold = italic = strikethrough = underlined = obfuscated = false`
///   *explicitly*, then sets the colour; the five named arms touch one field
///   each and nothing else.
/// * **`Some(false)`, not `None`.** The cleared flags are explicit-off, because
///   `to_spans`'s expansion pass inherits an expanded run's style from the
///   enclosing component: leaving them unspecified would let
///   `{"bold":true,"text":"a§cb"}` inherit bold onto `b`, where vanilla turns it
///   off. `None` here would be the `Some(false)` vs `None` collapse
///   [`TextStyle`]'s own docs warn about, arrived at from the other direction.
///
/// `§r` stays all-`None` on purpose, and that is not the same claim: under
/// `iterateFormatted` a reset restores `resetStyle`, which is seeded with the
/// *component's own* style rather than `Style.EMPTY`, so all-unspecified plus
/// [`TextStyle::inherit`] reproduces it exactly. At the root, where there is no
/// enclosing style, all-unspecified *is* `Style.EMPTY`. One representation, both
/// cases right.
fn apply_legacy_code(mut style: TextStyle, code: char) -> Option<TextStyle> {
    if let Some(color) = TextColor::from_legacy_code(code) {
        // `Style.applyLegacyFormat`'s colour branch passes `this.font`
        // through unchanged (its constructor call ends `..., this.font`) —
        // only colour and the five format flags are legacy-codeable, so a
        // colour code must not drop whatever font the component itself set.
        return Some(TextStyle {
            color: Some(color),
            bold: Some(false),
            italic: Some(false),
            underlined: Some(false),
            strikethrough: Some(false),
            obfuscated: Some(false),
            font: style.font,
        });
    }
    match code.to_ascii_lowercase() {
        'k' => style.obfuscated = Some(true),
        'l' => style.bold = Some(true),
        'm' => style.strikethrough = Some(true),
        'n' => style.underlined = Some(true),
        'o' => style.italic = Some(true),
        'r' => style = TextStyle::default(),
        _ => return None,
    }
    Some(style)
}

/// The built-in English (`en_us`) translation table. It covers the handful of
/// keys the client actually observes (chat, join/leave, common deaths); unknown
/// keys return `None` so the component's `fallback`/key is used, matching
/// vanilla's behaviour for missing translations.
#[must_use]
pub fn default_translation(key: &str) -> Option<String> {
    let pattern = match key {
        "chat.type.text" => "<%s> %s",
        "chat.type.announcement" => "[%s] %s",
        "chat.type.emote" => "* %s %s",
        "chat.type.admin" => "[%s: %s]",
        "multiplayer.player.joined" => "%s joined the game",
        "multiplayer.player.joined.renamed" => "%s (formerly known as %s) joined the game",
        "multiplayer.player.left" => "%s left the game",
        "death.attack.mob" => "%1$s was slain by %2$s",
        "death.attack.player" => "%1$s was slain by %2$s",
        "death.attack.arrow" => "%1$s was shot by %2$s",
        "death.attack.fall" => "%1$s fell from a high place",
        "death.attack.lava" => "%1$s tried to swim in lava",
        "death.attack.drown" => "%1$s drowned",
        "death.attack.explosion" => "%1$s blew up",
        _ => return None,
    };
    Some(pattern.to_owned())
}

// ---------------------------------------------------------------------------
// JSON front-end
// ---------------------------------------------------------------------------

fn text_from_json(value: &JsonValue, depth: usize) -> Text {
    if depth > MAX_DEPTH {
        return Text::default();
    }
    match value {
        JsonValue::String(text) => Text::literal(text.clone()),
        JsonValue::Bool(b) => Text::literal(b.to_string()),
        JsonValue::Number(n) => Text::literal(n.clone()),
        JsonValue::Null => Text::default(),
        JsonValue::Array(items) => {
            text_from_sequence(items.iter().map(|v| text_from_json(v, depth + 1)))
        }
        JsonValue::Object(fields) => {
            let get = |key: &str| lookup_json(fields, key);
            let mut text = Text::default();

            if let Some(JsonValue::String(literal)) = get("text") {
                text.content = TextContent::Literal(literal.clone());
            } else if let Some(JsonValue::String(key)) = get("translate") {
                let with = match get("with") {
                    Some(JsonValue::Array(args)) => {
                        args.iter().map(|a| text_from_json(a, depth + 1)).collect()
                    }
                    _ => Vec::new(),
                };
                let fallback = match get("fallback") {
                    Some(JsonValue::String(f)) => Some(f.clone()),
                    _ => None,
                };
                text.content = TextContent::Translate {
                    key: key.clone(),
                    with,
                    fallback,
                };
            }

            text.style = json_style(fields);
            text.insertion = match get("insertion") {
                Some(JsonValue::String(s)) => Some(s.clone()),
                _ => None,
            };
            text.click = json_click(get("clickEvent"));
            text.hover = json_hover(get("hoverEvent"), depth);

            if let Some(JsonValue::Array(extra)) = get("extra") {
                text.extra = extra.iter().map(|c| text_from_json(c, depth + 1)).collect();
            }
            text
        }
    }
}

fn json_style(fields: &[(String, JsonValue)]) -> TextStyle {
    let get = |key: &str| lookup_json(fields, key);
    TextStyle {
        color: match get("color") {
            Some(JsonValue::String(name)) => TextColor::from_name(name),
            _ => None,
        },
        bold: json_bool(get("bold")),
        italic: json_bool(get("italic")),
        underlined: json_bool(get("underlined")),
        strikethrough: json_bool(get("strikethrough")),
        obfuscated: json_bool(get("obfuscated")),
        font: match get("font") {
            Some(JsonValue::String(name)) => Some(FontId::intern(name)),
            _ => None,
        },
    }
}

fn json_bool(value: Option<&JsonValue>) -> Option<bool> {
    match value? {
        JsonValue::Bool(b) => Some(*b),
        JsonValue::String(s) if s == "true" => Some(true),
        JsonValue::String(s) if s == "false" => Some(false),
        _ => None,
    }
}

fn json_click(value: Option<&JsonValue>) -> Option<ClickEvent> {
    let JsonValue::Object(fields) = value? else {
        return None;
    };
    let action = match lookup_json(fields, "action") {
        Some(JsonValue::String(name)) => ClickAction::from_name(name),
        _ => return None,
    };
    let value = match lookup_json(fields, "value") {
        Some(JsonValue::String(v)) => v.clone(),
        _ => String::new(),
    };
    Some(ClickEvent { action, value })
}

fn json_hover(value: Option<&JsonValue>, depth: usize) -> Option<HoverEvent> {
    let JsonValue::Object(fields) = value? else {
        return None;
    };
    let action = match lookup_json(fields, "action") {
        Some(JsonValue::String(name)) => match name.as_str() {
            "show_text" => HoverAction::ShowText,
            "show_item" => HoverAction::ShowItem,
            "show_entity" => HoverAction::ShowEntity,
            other => HoverAction::Other(other.to_owned()),
        },
        _ => return None,
    };
    // Modern uses `contents`; legacy uses `value`. Either can be a component.
    let payload = lookup_json(fields, "contents").or_else(|| lookup_json(fields, "value"));
    let value = match payload {
        Some(v) => text_from_json(v, depth + 1),
        None => Text::default(),
    };
    Some(HoverEvent {
        action,
        value: Box::new(value),
    })
}

fn lookup_json<'a>(fields: &'a [(String, JsonValue)], key: &str) -> Option<&'a JsonValue> {
    fields.iter().find(|(name, _)| name == key).map(|(_, v)| v)
}

// ---------------------------------------------------------------------------
// NBT front-end
// ---------------------------------------------------------------------------

fn text_from_nbt(nbt: &Nbt, depth: usize) -> Text {
    if depth > MAX_DEPTH {
        return Text::default();
    }
    match nbt {
        Nbt::String(text) => Text::literal(text.clone()),
        Nbt::Byte(b) => Text::literal(b.to_string()),
        Nbt::Short(v) => Text::literal(v.to_string()),
        Nbt::Int(v) => Text::literal(v.to_string()),
        Nbt::Long(v) => Text::literal(v.to_string()),
        Nbt::Float(v) => Text::literal(v.to_string()),
        Nbt::Double(v) => Text::literal(v.to_string()),
        Nbt::List { elements, .. } => {
            text_from_sequence(elements.iter().map(|e| text_from_nbt(e, depth + 1)))
        }
        Nbt::Compound(fields) => {
            let get = |key: &str| lookup_nbt(fields, key);
            let mut text = Text::default();

            if let Some(Nbt::String(literal)) = get("text") {
                text.content = TextContent::Literal(literal.clone());
            } else if let Some(Nbt::String(key)) = get("translate") {
                let with = match get("with") {
                    Some(Nbt::List { elements, .. }) => elements
                        .iter()
                        .map(|a| text_from_nbt(a, depth + 1))
                        .collect(),
                    _ => Vec::new(),
                };
                let fallback = match get("fallback") {
                    Some(Nbt::String(f)) => Some(f.clone()),
                    _ => None,
                };
                text.content = TextContent::Translate {
                    key: key.clone(),
                    with,
                    fallback,
                };
            }

            text.style = nbt_style(fields);
            text.insertion = match get("insertion") {
                Some(Nbt::String(s)) => Some(s.clone()),
                _ => None,
            };
            text.click = nbt_click(get("clickEvent"));
            text.hover = nbt_hover(get("hoverEvent"), depth);

            if let Some(Nbt::List { elements, .. }) = get("extra") {
                text.extra = elements
                    .iter()
                    .map(|c| text_from_nbt(c, depth + 1))
                    .collect();
            }
            text
        }
        Nbt::End | Nbt::ByteArray(_) | Nbt::IntArray(_) | Nbt::LongArray(_) => Text::default(),
    }
}

fn nbt_style(fields: &[(String, Nbt)]) -> TextStyle {
    let get = |key: &str| lookup_nbt(fields, key);
    TextStyle {
        color: match get("color") {
            Some(Nbt::String(name)) => TextColor::from_name(name),
            _ => None,
        },
        bold: nbt_bool(get("bold")),
        italic: nbt_bool(get("italic")),
        underlined: nbt_bool(get("underlined")),
        strikethrough: nbt_bool(get("strikethrough")),
        obfuscated: nbt_bool(get("obfuscated")),
        font: match get("font") {
            Some(Nbt::String(name)) => Some(FontId::intern(name)),
            _ => None,
        },
    }
}

fn nbt_bool(value: Option<&Nbt>) -> Option<bool> {
    match value? {
        Nbt::Byte(b) => Some(*b != 0),
        Nbt::String(s) if s == "true" => Some(true),
        Nbt::String(s) if s == "false" => Some(false),
        _ => None,
    }
}

fn nbt_click(value: Option<&Nbt>) -> Option<ClickEvent> {
    let Nbt::Compound(fields) = value? else {
        return None;
    };
    let action = match lookup_nbt(fields, "action") {
        Some(Nbt::String(name)) => ClickAction::from_name(name),
        _ => return None,
    };
    let value = match lookup_nbt(fields, "value") {
        Some(Nbt::String(v)) => v.clone(),
        _ => String::new(),
    };
    Some(ClickEvent { action, value })
}

fn nbt_hover(value: Option<&Nbt>, depth: usize) -> Option<HoverEvent> {
    let Nbt::Compound(fields) = value? else {
        return None;
    };
    let action = match lookup_nbt(fields, "action") {
        Some(Nbt::String(name)) => match name.as_str() {
            "show_text" => HoverAction::ShowText,
            "show_item" => HoverAction::ShowItem,
            "show_entity" => HoverAction::ShowEntity,
            other => HoverAction::Other(other.to_owned()),
        },
        _ => return None,
    };
    let payload = lookup_nbt(fields, "contents").or_else(|| lookup_nbt(fields, "value"));
    let value = match payload {
        Some(v) => text_from_nbt(v, depth + 1),
        None => Text::default(),
    };
    Some(HoverEvent {
        action,
        value: Box::new(value),
    })
}

fn lookup_nbt<'a>(fields: &'a [(String, Nbt)], key: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(name, _)| name == key).map(|(_, v)| v)
}

/// Builds a component from a sequence: the first element is the parent and the
/// remainder become its `extra` children, matching how both JSON arrays and NBT
/// lists of components are interpreted.
fn text_from_sequence(items: impl Iterator<Item = Text>) -> Text {
    let mut iter = items;
    let Some(mut root) = iter.next() else {
        return Text::default();
    };
    // Siblings are appended after any children the first element already had.
    for sibling in iter {
        root.extra.push(sibling);
    }
    root
}

// ---------------------------------------------------------------------------
// Minimal dependency-free JSON reader
// ---------------------------------------------------------------------------

/// A minimal owned JSON value. Numbers are kept as their source text since chat
/// components never need their numeric value. This exists so `lodestone-model`
/// can parse JSON chat without pulling in `serde_json`.
#[derive(Debug, Clone, PartialEq)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    fn parse(input: &str) -> Option<Self> {
        let mut parser = JsonParser {
            bytes: input.as_bytes(),
            pos: 0,
        };
        parser.skip_whitespace();
        let value = parser.parse_value(0)?;
        parser.skip_whitespace();
        (parser.pos == parser.bytes.len()).then_some(value)
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl JsonParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.peek() {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self, depth: usize) -> Option<JsonValue> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_whitespace();
        match self.peek()? {
            b'"' => self.parse_string().map(JsonValue::String),
            b'{' => self.parse_object(depth),
            b'[' => self.parse_array(depth),
            b't' => self.parse_literal("true", JsonValue::Bool(true)),
            b'f' => self.parse_literal("false", JsonValue::Bool(false)),
            b'n' => self.parse_literal("null", JsonValue::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_literal(&mut self, literal: &str, value: JsonValue) -> Option<JsonValue> {
        let end = self.pos + literal.len();
        if self.bytes.get(self.pos..end) == Some(literal.as_bytes()) {
            self.pos = end;
            Some(value)
        } else {
            None
        }
    }

    fn parse_number(&mut self) -> Option<JsonValue> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if matches!(byte, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        Some(JsonValue::Number(text.to_owned()))
    }

    fn parse_string(&mut self) -> Option<String> {
        if self.bump()? != b'"' {
            return None;
        }
        let mut out = String::new();
        loop {
            match self.bump()? {
                b'"' => return Some(out),
                b'\\' => {
                    let escaped = self.bump()?;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.parse_unicode_escape()?),
                        _ => return None,
                    }
                }
                byte if byte < 0x20 => return None,
                byte => {
                    let len = utf8_len(byte)?;
                    let mut buf = [byte, 0, 0, 0];
                    for slot in buf.iter_mut().take(len).skip(1) {
                        *slot = self.bump()?;
                    }
                    let text = std::str::from_utf8(&buf[..len]).ok()?;
                    out.push_str(text);
                }
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Option<char> {
        let high = self.parse_hex4()?;
        if (0xd800..=0xdbff).contains(&high) {
            if self.bump()? != b'\\' || self.bump()? != b'u' {
                return None;
            }
            let low = self.parse_hex4()?;
            if !(0xdc00..=0xdfff).contains(&low) {
                return None;
            }
            let combined =
                0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
            char::from_u32(combined)
        } else {
            char::from_u32(u32::from(high))
        }
    }

    fn parse_hex4(&mut self) -> Option<u16> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let digit = (self.bump()? as char).to_digit(16)?;
            value = value.checked_mul(16)?.checked_add(digit as u16)?;
        }
        Some(value)
    }

    fn parse_array(&mut self, depth: usize) -> Option<JsonValue> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek()? == b']' {
            self.pos += 1;
            return Some(JsonValue::Array(items));
        }
        loop {
            items.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();
            match self.bump()? {
                b',' => self.skip_whitespace(),
                b']' => return Some(JsonValue::Array(items)),
                _ => return None,
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Option<JsonValue> {
        self.pos += 1; // consume '{'
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.peek()? == b'}' {
            self.pos += 1;
            return Some(JsonValue::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.bump()? != b':' {
                return None;
            }
            let value = self.parse_value(depth + 1)?;
            fields.push((key, value));
            self.skip_whitespace();
            match self.bump()? {
                b',' => {}
                b'}' => return Some(JsonValue::Object(fields)),
                _ => return None,
            }
        }
    }
}

/// Returns the length in bytes of a UTF-8 sequence given its lead byte.
const fn utf8_len(lead: u8) -> Option<usize> {
    match lead {
        0x00..=0x7f => Some(1),
        0xc0..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf7 => Some(4),
        _ => None,
    }
}
