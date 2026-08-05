//! Chat: an inbound scrollback log and an outbound input line.
//!
//! This is deliberately pure — no winit, no GPU, no client handle — so the
//! interesting behaviour (what a line of typed text *means*, how the log
//! bounds itself, how editing behaves at char boundaries) is unit-testable
//! without a window or a server. The platform layer ([`crate::app`]) feeds it
//! keystrokes and drains [`compose_chat_action`] onto the outbound
//! [`ClientAction`] seam; the HUD ([`crate::hud`]) reads the received scrollback
//! and the in-progress [`ChatInput`] to draw them.
//!
//! The received log itself is **not here**. `docs/bevy-migration.md` Stage 5
//! moved it to `lodestone_game::chat::ChatLog` so it could be the payload of a
//! `lodestone_ecs::SessionChat` component — `lodestone-ecs` cannot depend on this
//! crate. What is left here is the outbound half only.
//!
//! Routing a chat line to the wire goes through the *same* `ClientAction` seam
//! as movement, not a bespoke path: a leading `/` is a command, everything else
//! is a chat message, matching vanilla. The shell never names a packet.
//!
//! ## Command-tree consumption (issue #46)
//!
//! [`highlight`] and [`complete`] are this crate's half of the Brigadier
//! command-tree UX. Both walk a [`CommandTree`] (decoded upstream, from the
//! `minecraft:commands` packet — see `lodestone_model::command_tree`'s own
//! doc for the wire shape and, importantly, **what still has to be brokered
//! into a protocol-crate decode arm**, since this crate cannot touch
//! `crates/protocol/**`) against the *current* input line, which
//! [`ChatInput`]'s own doc already establishes is always edited at its end —
//! so there is no separate cursor position to track here, only "the line so
//! far".
//!
//! Both functions share one internal walker ([`parse_line`]) that consumes
//! tokens left to right: a literal child matches by exact text; an argument
//! child is read according to its [`lodestone_model::command_tree::ArgumentParser`] (a
//! greedy phrase or `message` argument swallows the rest of the line; a
//! quoted phrase reads to its closing `"`; everything else reads to the next
//! space) and validated where a parser's grammar is simple enough to check
//! locally (numeric bounds, `bool`, and the small fixed-domain parsers in
//! [`local_domain`]). The first token that fails to match anything —
//! diverging from every viable child of the tree — ends the walk, and
//! everything from there to the end of the line is
//! [`HighlightKind::Unparsed`].
//!
//! **That failure is only sometimes fatal to [`complete`], and getting this
//! wrong was this module's own first bug.** Since the cursor is always at
//! the end of the line, a failing token that is *also the last thing
//! typed* is still being typed — vanilla's own `CommandSuggestions` colours
//! it `UNPARSED_STYLE` and offers suggestions for it **in the same pass**
//! (`updateCommandInfo`/`formatText` both read the same `currentParse`;
//! neither waits for the other to decide the text is "valid" first). Typing
//! `/g` against a tree with a `gamemode` literal is red *and* offers
//! `gamemode` as a completion, simultaneously. Only a failing token with
//! more input *after* it — text no further typing of *that token* can ever
//! rescue — ends [`complete`] with [`Completion::None`] too; see
//! `tests::command_ux::completes_literal_siblings_by_prefix_alphabetically`
//! for the "still typing" case and
//! `tests::command_ux::no_completions_past_a_failed_token` for the
//! unrecoverable one.
//!
//! **A redirect is a same-position jump, not a token-consuming one**, so a
//! server-sent redirect cycle is a real possibility the walker must not hang
//! on. The actual cycle guard lives in
//! [`lodestone_model::command_tree::CommandTree::effective_children`] (a
//! visited-node set), which both [`parse_line`] and [`complete`] call
//! instead of reading `children`/`redirect` directly — see that function's
//! own doc, `lodestone-model`'s
//! `command_tree::tests::effective_children_terminates_on_a_redirect_cycle`,
//! and this module's own
//! `tests::command_ux::complete_and_highlight_terminate_on_a_redirect_cycle`
//! for the control proving it fires from *this* crate's call sites too, not
//! only in isolation.
//!
//! **What is deliberately simplified, named rather than hidden:**
//!
//! - Any argument node carrying a `suggestions` provider id, and any
//!   argument parser outside [`local_domain`]'s small fixed-domain set
//!   (entity selectors, resource-registry-backed types, score holders,
//!   block/item predicates, NBT, …), is answered by the server round trip
//!   ([`Completion::NeedsServer`]) rather than guessed at locally. Vanilla's
//!   own client actually answers several of these locally too (entity/player
//!   names from the tab list, team names from the scoreboard, …) via
//!   `ClientSuggestionProvider` — see `SuggestionProviders.java` and
//!   `ClientSuggestionProvider.java` in `.cache/mc/26.2/client-src`. Doing
//!   the same here needs session/world state this crate deliberately does
//!   not hold (`chat.rs` stays pure — no client handle, no ECS query). Until
//!   that state is threaded in, routing these to the server is strictly
//!   *slower* than vanilla, never *wrong*: the server's own Brigadier
//!   dispatcher computes the identical merged suggestion set vanilla's
//!   client would have shown, this project's `command_suggestion` round
//!   trip included. Filed as a named follow-up rather than built now — see
//!   `docs/commands.md`.
//! - At most one argument child per node is tried, with no backtracking
//!   across several — real vanilla command trees essentially never branch
//!   into more than one argument type at the same position, so this has no
//!   observed effect on any real command, but a datapack that did do so
//!   would see only the first-registered branch.
//! - Opaque argument types (anything with no [`local_domain`] entry and no
//!   numeric/`bool`/string-kind rule) are read as a single space-delimited
//!   token and always accepted — never validated, never marked
//!   [`HighlightKind::Unparsed`]. This under-validates types whose grammar
//!   can itself contain spaces (NBT, block states with `[...]`, quoted
//!   messages typed without the `message` parser's own quoting) but never
//!   produces a false red on input that is actually fine, which is the
//!   safer direction for a UX feature to be wrong in.
//!
//! `highlight`/`complete` return **byte spans into the input string**, not
//! screen pixels — this crate has no font metrics and does not compute any.
//! Mapping a span to a pixel run belongs wherever the draw call already
//! measures real glyph advances for word wrap (`hud.rs`'s
//! `Builder::legacy_width`/`VanillaFont`, per `docs/chat.md`); a caller that
//! instead assumed one span character equals one fixed-width column would be
//! wrong for the same reason a character-count word-wrap would be, since
//! Minecraft's font is proportional.

use lodestone_client::ClientAction;
use lodestone_model::command_tree::{
    ArgumentParser, CommandSuggestionEntry, CommandTree, NodeKind, StringKind,
};

/// The line currently being typed. Kept separate from the log so opening the
/// chat box, editing, and cancelling never touch the received history.
#[derive(Debug, Clone, Default)]
pub struct ChatInput {
    buf: String,
}

impl ChatInput {
    /// An empty input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the buffer (e.g. a leading `/` when chat is opened with the command
    /// key). Replaces any current contents.
    pub fn set(&mut self, text: impl Into<String>) {
        self.buf = text.into();
    }

    /// Append typed text. Control characters (newlines, the section sign used by
    /// legacy colour codes) are filtered so a paste or an IME can't inject them.
    pub fn push_str(&mut self, text: &str) {
        for ch in text.chars() {
            self.push_char(ch);
        }
    }

    /// Append a single character if it is printable and the line has room.
    /// Vanilla caps a chat line at 256 characters.
    pub fn push_char(&mut self, ch: char) {
        if ch.is_control() || ch == '\u{00a7}' {
            return;
        }
        if self.buf.chars().count() >= 256 {
            return;
        }
        self.buf.push(ch);
    }

    /// Delete the last character (char-boundary safe). No-op when empty.
    pub fn backspace(&mut self) {
        self.buf.pop();
    }

    /// The current text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Clear and return the typed line, ready to compose into an action.
    #[must_use]
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.buf)
    }
}

/// Lower a typed line onto the outbound action seam, matching vanilla's rule:
/// a leading `/` is a command (sent without the slash), anything else is chat.
/// A blank line — or a bare `/` — sends nothing.
#[must_use]
pub fn compose_chat_action(line: &str) -> Option<ClientAction> {
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    if let Some(command) = line.strip_prefix('/') {
        if command.is_empty() {
            return None;
        }
        Some(ClientAction::SendCommand {
            command: command.to_string(),
        })
    } else {
        Some(ClientAction::SendChat {
            text: line.to_string(),
        })
    }
}

/// One highlighted run of a command line, mirroring vanilla's
/// `CommandSuggestions.formatText`
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/components/CommandSuggestions.java:402-438`).
/// Byte offsets into the input string; see this module's own doc for why no
/// pixel width is computed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Start byte offset (inclusive).
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
    /// What this run is.
    pub kind: HighlightKind,
}

/// What one [`HighlightSpan`] is, matching vanilla's three styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// The leading `/`, or a matched literal keyword. Vanilla's
    /// `LITERAL_STYLE` (`ChatFormatting.GRAY`).
    Literal,
    /// A matched, valid argument value. The carried index cycles `0..5` once
    /// per argument regardless of nesting depth, selecting vanilla's
    /// `ARGUMENT_STYLES` in order — `AQUA, YELLOW, GREEN, LIGHT_PURPLE, GOLD`
    /// (`CommandSuggestions.java:58-62`). Colour-to-`ChatFormatting` mapping
    /// is a draw-call concern (`hud.rs`, brokered — not this crate), not
    /// modelled as an actual colour here.
    Argument(u8),
    /// Everything from the point parsing first failed to the end of the
    /// line. Vanilla's `UNPARSED_STYLE` (`ChatFormatting.RED`).
    Unparsed,
}

/// One tab-completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The literal replacement text.
    pub text: String,
    /// An optional tooltip, when the source (a server suggestion) had one.
    pub tooltip: Option<String>,
}

/// The result of [`complete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Computed entirely from the tree; safe to show immediately, replacing
    /// `line[start..]` with a chosen candidate's text.
    Local {
        /// Byte offset where a chosen candidate's text should be spliced in,
        /// replacing everything from there to the end of the line.
        start: usize,
        /// The candidates, alphabetically sorted (matching Brigadier's own
        /// `SuggestionsBuilder::build`; case-insensitive primary, then
        /// case-sensitive — Brigadier library behaviour, not sourced from
        /// `.cache/mc/26.2` this session).
        candidates: Vec<Candidate>,
    },
    /// This position needs the server's answer — send
    /// [`SuggestionRequests::request`]'s action and wait for the reply. See
    /// this module's own doc for exactly which positions this covers.
    NeedsServer {
        /// Byte offset where the eventual answer should be spliced in.
        start: usize,
    },
    /// Nothing to suggest: the line is not a command, a prior token already
    /// failed to parse, or the current position has no reachable children.
    None,
}

/// The literal completion domain for argument parsers whose entire
/// suggestion set is small, fixed, and independent of world/session state.
/// `None` means "not locally enumerable" — see this module's own doc for why
/// that routes to the server rather than offering nothing.
///
/// Doubles as the validity check for these same parsers during highlighting:
/// a value for one of these types is only valid when it is a member of its
/// domain, which is exactly the completion condition too.
///
/// Each domain is sourced from that parser's own vanilla `listSuggestions`
/// (or, for `ScoreboardSlot`/`TeamColor`, the enum it suggests every member
/// of) — see [`ArgumentParser`]'s own doc for the exact citations.
#[must_use]
pub fn local_domain(parser: &ArgumentParser) -> Option<&'static [&'static str]> {
    match parser {
        ArgumentParser::Bool => Some(&["true", "false"]),
        ArgumentParser::Operation => {
            Some(&["=", "+=", "-=", "*=", "/=", "%=", "<", ">", "><"])
        }
        ArgumentParser::EntityAnchor => Some(&["feet", "eyes"]),
        ArgumentParser::GameMode => {
            Some(&["survival", "creative", "adventure", "spectator"])
        }
        ArgumentParser::TeamColor => Some(&[
            "black",
            "dark_blue",
            "dark_green",
            "dark_aqua",
            "dark_red",
            "dark_purple",
            "gold",
            "gray",
            "dark_gray",
            "blue",
            "green",
            "aqua",
            "red",
            "light_purple",
            "yellow",
            "white",
        ]),
        ArgumentParser::ScoreboardSlot => Some(&[
            "list",
            "sidebar",
            "below_name",
            "sidebar.team.black",
            "sidebar.team.dark_blue",
            "sidebar.team.dark_green",
            "sidebar.team.dark_aqua",
            "sidebar.team.dark_red",
            "sidebar.team.dark_purple",
            "sidebar.team.gold",
            "sidebar.team.gray",
            "sidebar.team.dark_gray",
            "sidebar.team.blue",
            "sidebar.team.green",
            "sidebar.team.aqua",
            "sidebar.team.red",
            "sidebar.team.light_purple",
            "sidebar.team.yellow",
            "sidebar.team.white",
        ]),
        _ => None,
    }
}

/// Byte offset of the next ASCII space in `body` at or after `start`, or
/// `body.len()` when there isn't one. `start` and the result are always
/// valid `char` boundaries: a single-byte ASCII space can only sit on one.
fn next_space(body: &str, start: usize) -> usize {
    body[start..]
        .find(' ')
        .map_or(body.len(), |offset| start + offset)
}

/// Reads a Brigadier-style quoted string starting at `body[start] == b'"'`.
/// Supports `\"`/`\\` escapes (`StringReader.readQuotedString`'s own escape
/// set — Brigadier library behaviour; see [`StringKind`]'s doc for the same
/// "not sourced from `.cache/mc/26.2` this session" caveat). Returns
/// `(end, valid)`; `valid` is `false` when the closing quote is never found,
/// in which case `end == body.len()`.
fn read_quoted(body: &str, start: usize) -> (usize, bool) {
    let bytes = body.as_bytes();
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            return (index + 1, true);
        }
        index += 1;
    }
    (body.len(), false)
}

/// Whether `text` is a legal Brigadier unquoted string
/// (`StringReader.isAllowedInUnquotedString`'s charset — again Brigadier
/// library behaviour, not from this session's decompile).
fn is_unquoted_string(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'_' | b'-' | b'.' | b'+'))
}

/// Whether `text` (already isolated as a single token) is a valid value for
/// `parser`, for the parsers this module can check locally. Every other
/// parser — anything with no numeric/`bool`/string-kind rule and no
/// [`local_domain`] entry — is treated as always valid; see this module's
/// own doc for why that is the deliberately safe direction to simplify in.
fn validate_simple(parser: &ArgumentParser, text: &str) -> bool {
    match parser {
        ArgumentParser::Integer { min, max } => {
            text.parse::<i32>().is_ok_and(|v| v >= *min && v <= *max)
        }
        ArgumentParser::Long { min, max } => {
            text.parse::<i64>().is_ok_and(|v| v >= *min && v <= *max)
        }
        ArgumentParser::Float { min, max } => {
            text.parse::<f32>().is_ok_and(|v| v >= *min && v <= *max)
        }
        ArgumentParser::Double { min, max } => {
            text.parse::<f64>().is_ok_and(|v| v >= *min && v <= *max)
        }
        ArgumentParser::String(StringKind::SingleWord) => is_unquoted_string(text),
        _ => local_domain(parser).is_none_or(|domain| domain.contains(&text)),
    }
}

/// Reads the next argument token starting at `body[start]` (`start < body.len()`
/// is the caller's invariant), per `parser`'s own grammar. Returns
/// `(end, valid)`.
fn read_argument_token(body: &str, start: usize, parser: &ArgumentParser) -> (usize, bool) {
    match parser {
        ArgumentParser::String(StringKind::GreedyPhrase) | ArgumentParser::Message => {
            (body.len(), true)
        }
        ArgumentParser::String(StringKind::QuotablePhrase) if body.as_bytes()[start] == b'"' => {
            read_quoted(body, start)
        }
        _ => {
            let end = next_space(body, start);
            (end, validate_simple(parser, &body[start..end]))
        }
    }
}

/// A completed walk of a command line against a [`CommandTree`]: every span
/// produced so far, the node reached after the last fully-matched token, and
/// where the next (possibly empty, in-progress) token starts.
struct ParseWalk {
    spans: Vec<HighlightSpan>,
    node: usize,
    next_token_start: usize,
    failed: bool,
}

/// The walk to return when the token just matched runs all the way to the end
/// of the line — i.e. the player has typed it fully but has **not** typed the
/// space after it.
///
/// Such a token is still being typed, and completion must therefore be offered
/// by its **parent**, filtered by the token as a prefix — not by the token's own
/// children. This is not a simplification; it is what vanilla does.
/// `CommandContextBuilder.findSuggestionContext(cursor)` takes the
/// `range.getEnd() < cursor` branch only when the cursor is strictly *past* the
/// parsed range; with the cursor sitting exactly on the end it falls into the
/// loop that returns `new SuggestionContext<>(prev, nodeRange.getStart())` —
/// `prev` being the node *before* the one containing the cursor. So `/gamemode`
/// suggests `gamemode`, and only `/gamemode ` suggests the four game modes.
///
/// Getting this wrong is silent and was measured: advancing into the matched
/// node makes the in-progress token look like an empty next token, so every
/// fully-typed command name completes to its own arguments and no half-typed
/// name ever completes to itself. It is the same class of defect as a
/// `canonicalize` that `.trim()`s the line — the trailing space is load-bearing
/// data, not whitespace to normalise away. Gated by
/// `crates/lodestone-shell/tests/command_tree_completion.rs`'s
/// `a_trailing_space_decides_between_finishing_a_token_and_starting_the_next`,
/// against a tree captured from a real 26.2 server.
///
/// `spans` is passed through untouched, so **highlighting is unaffected**: the
/// token was matched and has already had its span pushed by the caller. Only the
/// completion position differs.
fn still_typing(spans: Vec<HighlightSpan>, parent: usize, token_start: usize) -> ParseWalk {
    ParseWalk {
        spans,
        node: parent,
        next_token_start: token_start,
        failed: false,
    }
}

/// The shared walker behind [`highlight`] and [`complete`]. `None` when
/// `line` is not a command (`highlight`/`complete` both treat that as
/// "nothing to say" — this module covers commands only, not chat-message
/// player-name completion).
fn parse_line(tree: &CommandTree, line: &str) -> Option<ParseWalk> {
    if !line.starts_with('/') {
        return None;
    }

    let mut spans = vec![HighlightSpan {
        start: 0,
        end: 1,
        kind: HighlightKind::Literal,
    }];
    let mut pos = 1usize;
    let mut node = tree.root();
    let mut arg_color = 0u8;
    let len = line.len();

    loop {
        while pos < len && line.as_bytes()[pos] == b' ' {
            pos += 1;
        }
        if pos >= len {
            return Some(ParseWalk {
                spans,
                node,
                next_token_start: pos,
                failed: false,
            });
        }

        let token_start = pos;
        let word_end = next_space(line, token_start);
        let word = &line[token_start..word_end];

        // A redirect is a same-position jump; `effective_children` is the
        // one place that follows it, and it is cycle-guarded — see this
        // module's own doc.
        let reachable: Vec<usize> = tree
            .effective_children(node)
            .into_iter()
            .filter(|&idx| {
                !matches!(
                    tree.node(idx).map(|n| &n.kind),
                    Some(NodeKind::Unrecognized { .. })
                )
            })
            .collect();

        let literal_match = reachable.iter().copied().find(|&idx| {
            matches!(
                tree.node(idx).map(|n| &n.kind),
                Some(NodeKind::Literal { name }) if name == word
            )
        });

        if let Some(matched) = literal_match {
            spans.push(HighlightSpan {
                start: token_start,
                end: word_end,
                kind: HighlightKind::Literal,
            });
            if word_end == len {
                return Some(still_typing(spans, node, token_start));
            }
            node = matched;
            pos = word_end;
            continue;
        }

        let argument_match = reachable.iter().copied().find_map(|idx| match tree
            .node(idx)
            .map(|n| &n.kind)
        {
            Some(NodeKind::Argument { parser, .. }) => Some((idx, parser)),
            _ => None,
        });

        let unparsed_end = if let Some((idx, parser)) = argument_match {
            let (end, valid) = read_argument_token(line, token_start, parser);
            if valid {
                spans.push(HighlightSpan {
                    start: token_start,
                    end,
                    kind: HighlightKind::Argument(arg_color % 5),
                });
                arg_color = arg_color.wrapping_add(1);
                if end == len {
                    return Some(still_typing(spans, node, token_start));
                }
                node = idx;
                pos = end;
                continue;
            }
            end
        } else {
            word_end
        };

        // Nothing matched this token — either no child's name/grammar
        // accepted it, or the sole argument candidate rejected it.
        //
        // **This is not always a hard failure.** `line`'s cursor is always
        // at the end (`ChatInput`'s own invariant), so when this failing
        // token is *also the last thing typed* (`unparsed_end == len`),
        // vanilla's own dispatcher is in exactly this state too — Brigadier
        // tries to match/parse it, throws, and leaves the reader's cursor
        // at `token_start` — and `CommandSuggestions` still asks that
        // *parent* node's children for suggestions filtered by the
        // unmatched text as a live prefix, while simultaneously colouring
        // the same text `UNPARSED_STYLE` (`CommandSuggestions.java`'s
        // `updateCommandInfo`/`formatText` both run off the same
        // `currentParse`, and neither waits for the other). A token with
        // more input *after* it, though, can never become anything else by
        // more typing — that is the genuine, unrecoverable failure
        // `complete` must refuse to suggest through.
        spans.push(HighlightSpan {
            start: token_start,
            end: len,
            kind: HighlightKind::Unparsed,
        });
        let last_token = unparsed_end == len;
        return Some(ParseWalk {
            spans,
            node,
            next_token_start: if last_token { token_start } else { len },
            failed: !last_token,
        });
    }
}

/// Syntax-highlights `line` against `tree`, mirroring vanilla's
/// `CommandSuggestions.formatText`. Returns an empty list when `line` is not
/// a command (does not start with `/`) — plain chat gets no highlighting,
/// matching vanilla.
#[must_use]
pub fn highlight(tree: &CommandTree, line: &str) -> Vec<HighlightSpan> {
    parse_line(tree, line).map_or_else(Vec::new, |walk| walk.spans)
}

/// Computes tab-completion candidates for `line` against `tree`. See this
/// module's own doc for exactly what is answered locally versus deferred to
/// [`Completion::NeedsServer`].
#[must_use]
pub fn complete(tree: &CommandTree, line: &str) -> Completion {
    let Some(walk) = parse_line(tree, line) else {
        return Completion::None;
    };
    if walk.failed {
        return Completion::None;
    }

    let partial = &line[walk.next_token_start..];
    let reachable: Vec<usize> = tree
        .effective_children(walk.node)
        .into_iter()
        .filter(|&idx| {
            !matches!(
                tree.node(idx).map(|n| &n.kind),
                Some(NodeKind::Unrecognized { .. })
            )
        })
        .collect();
    if reachable.is_empty() {
        return Completion::None;
    }

    let mut candidates = Vec::new();
    let mut needs_server = false;

    for idx in reachable {
        match tree.node(idx).map(|n| &n.kind) {
            Some(NodeKind::Literal { name }) => {
                if name.starts_with(partial) {
                    candidates.push(Candidate {
                        text: name.clone(),
                        tooltip: None,
                    });
                }
            }
            Some(NodeKind::Argument {
                parser,
                suggestions,
                ..
            }) => {
                if suggestions.is_some() {
                    needs_server = true;
                    continue;
                }
                match local_domain(parser) {
                    Some(domain) => {
                        for &value in domain {
                            if value.starts_with(partial) {
                                candidates.push(Candidate {
                                    text: value.to_string(),
                                    tooltip: None,
                                });
                            }
                        }
                    }
                    None => needs_server = true,
                }
            }
            _ => {}
        }
    }

    if needs_server {
        return Completion::NeedsServer {
            start: walk.next_token_start,
        };
    }

    if candidates.is_empty() {
        return Completion::None;
    }

    // Brigadier's own `SuggestionsBuilder::build` sorts suggestions
    // case-insensitively with a case-sensitive tiebreak.
    candidates.sort_by(|a, b| {
        a.text
            .to_ascii_lowercase()
            .cmp(&b.text.to_ascii_lowercase())
            .then_with(|| a.text.cmp(&b.text))
    });

    Completion::Local {
        start: walk.next_token_start,
        candidates,
    }
}

/// One in-flight serverbound `command_suggestion` round trip.
#[derive(Debug, Clone)]
struct PendingSuggestionRequest {
    id: i32,
}

/// Tracks the request/response round trip [`Completion::NeedsServer`] hands
/// off to. Mirrors vanilla's own `ClientSuggestionProvider`: a monotonically
/// increasing transaction id, and a reply is honoured only when its id
/// matches the one currently in flight (`completeCustomSuggestions`'s own
/// check) — a reply to a request the input has since outgrown is dropped
/// rather than stomping a newer one's answer.
#[derive(Debug, Clone, Default)]
pub struct SuggestionRequests {
    next_id: i32,
    pending: Option<PendingSuggestionRequest>,
}

impl SuggestionRequests {
    /// A tracker with no request in flight.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the serverbound action for a [`Completion::NeedsServer`]
    /// position and starts tracking its transaction id. `command` is the
    /// full input line **including the leading slash** — matching
    /// `ClientAction::CommandSuggestion::command`'s own doc ("the command
    /// text typed so far, including the leading slash"), which is exactly
    /// what [`ChatInput::as_str`] already holds, since [`ChatInput`] only
    /// ever edits at the end of the line.
    pub fn request(&mut self, command: &str) -> ClientAction {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending = Some(PendingSuggestionRequest { id });
        ClientAction::CommandSuggestion {
            id,
            command: command.to_string(),
        }
    }

    /// Accepts a `ClientEvent::CommandSuggestionsReceived` reply. Returns the
    /// candidates when `id` matches the request in flight (and clears the
    /// pending state); `None` for a stale id, which the caller should
    /// silently drop rather than apply.
    pub fn receive(&mut self, id: i32, entries: Vec<CommandSuggestionEntry>) -> Option<Vec<Candidate>> {
        match &self.pending {
            Some(pending) if pending.id == id => {
                self.pending = None;
                Some(
                    entries
                        .into_iter()
                        .map(|entry| Candidate {
                            text: entry.text,
                            tooltip: entry.tooltip,
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// Whether a request is currently awaiting a reply.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_edits_are_char_boundary_safe() {
        let mut input = ChatInput::new();
        input.push_str("héllo"); // multi-byte é
        input.backspace(); // removes 'o'
        input.backspace(); // removes 'l'
        assert_eq!(input.as_str(), "hél");
        input.backspace();
        input.backspace(); // removes é (2 bytes) as one char
        assert_eq!(input.as_str(), "h");
    }

    #[test]
    fn input_filters_control_chars_and_section_sign() {
        let mut input = ChatInput::new();
        input.push_str("hi\nthere\u{00a7}c");
        assert_eq!(input.as_str(), "hitherec");
    }

    #[test]
    fn input_caps_at_256_chars() {
        let mut input = ChatInput::new();
        input.push_str(&"a".repeat(300));
        assert_eq!(input.as_str().chars().count(), 256);
    }

    #[test]
    fn take_clears_and_returns() {
        let mut input = ChatInput::new();
        input.set("hello");
        assert_eq!(input.take(), "hello");
        assert!(input.is_empty());
    }

    #[test]
    fn plain_text_composes_a_chat_message() {
        match compose_chat_action("hello world") {
            Some(ClientAction::SendChat { text }) => assert_eq!(text, "hello world"),
            other => panic!("expected SendChat, got {other:?}"),
        }
    }

    #[test]
    fn slash_composes_a_command_without_the_slash() {
        match compose_chat_action("/gamemode creative") {
            Some(ClientAction::SendCommand { command }) => {
                assert_eq!(command, "gamemode creative", "slash must be stripped");
            }
            other => panic!("expected SendCommand, got {other:?}"),
        }
    }

    #[test]
    fn blank_and_bare_slash_send_nothing() {
        assert!(compose_chat_action("").is_none());
        assert!(compose_chat_action("   ").is_none());
        assert!(compose_chat_action("/").is_none());
    }

    /// `/givedebug` was a bespoke wrapper that rewrote itself into the server's
    /// real `/give @s <item> <amount>`; issue #382 deleted it. What replaced it
    /// is nothing at all — a `/givedebug…` line is now just a command the
    /// server does not know, and it must reach the wire as one rather than
    /// being swallowed by a leftover parser.
    #[test]
    fn givedebug_is_no_longer_intercepted_and_goes_to_the_server_verbatim() {
        match compose_chat_action("/givedebug minecraft:diamond_pickaxe 1") {
            Some(ClientAction::SendCommand { command }) => {
                assert_eq!(command, "givedebug minecraft:diamond_pickaxe 1");
            }
            other => panic!("expected a plain SendCommand, got {other:?}"),
        }
        // The malformed form used to be answered locally and never sent. It is
        // now the server's problem too — nothing here may absorb it.
        match compose_chat_action("/givedebug") {
            Some(ClientAction::SendCommand { command }) => assert_eq!(command, "givedebug"),
            other => panic!("expected a plain SendCommand, got {other:?}"),
        }
    }

    /// Command-tree tab completion and syntax highlighting (issue #46).
    mod command_ux {
        use lodestone_model::{ResourceKey, command_tree::RawCommandNode};

        use super::*;

        fn root(children: Vec<usize>) -> RawCommandNode {
            RawCommandNode {
                kind: NodeKind::Root,
                executable: false,
                restricted: false,
                redirect: None,
                children,
            }
        }

        fn literal(name: &str, executable: bool, children: Vec<usize>) -> RawCommandNode {
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: name.to_string(),
                },
                executable,
                restricted: false,
                redirect: None,
                children,
            }
        }

        fn argument(
            name: &str,
            parser: ArgumentParser,
            suggestions: Option<ResourceKey>,
            executable: bool,
            children: Vec<usize>,
        ) -> RawCommandNode {
            RawCommandNode {
                kind: NodeKind::Argument {
                    name: name.to_string(),
                    parser,
                    suggestions,
                },
                executable,
                restricted: false,
                redirect: None,
                children,
            }
        }

        /// A `/gamemode <mode>` / `/give` tree, shaped like vanilla's own:
        /// two literal siblings under the root, one of which has a single
        /// `minecraft:gamemode` argument child with no custom suggestions
        /// provider (so its four values are enumerable from
        /// [`local_domain`] alone, matching `GameModeArgument`'s real
        /// `listSuggestions`).
        fn gamemode_and_give_tree() -> CommandTree {
            let nodes = vec![
                root(vec![1, 3]),
                literal("gamemode", false, vec![2]),
                argument("mode", ArgumentParser::GameMode, None, true, vec![]),
                literal("give", true, vec![]),
            ];
            CommandTree::new(nodes, 0).unwrap()
        }

        /// **Predicted, before running anything**: with an empty partial
        /// token right after `/gamemode `, the only reachable child is the
        /// `GameMode` argument with no suggestions provider, so
        /// [`local_domain`] must supply all four vanilla game modes —
        /// sourced from `GameType.java:17-20`'s declaration order
        /// (`survival, creative, adventure, spectator`), then resorted here
        /// alphabetically to match Brigadier's own `SuggestionsBuilder`.
        /// **Rejected ranking**: declaration order
        /// (`survival, creative, adventure, spectator`) — a completion gate
        /// that only checked "four candidates, non-empty" would pass for
        /// either ordering; this asserts the exact list.
        #[test]
        fn completes_gamemode_values_from_the_local_domain() {
            let tree = gamemode_and_give_tree();
            match complete(&tree, "/gamemode ") {
                Completion::Local { start, candidates } => {
                    assert_eq!(start, "/gamemode ".len());
                    let texts: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
                    assert_eq!(
                        texts,
                        vec!["adventure", "creative", "spectator", "survival"],
                        "expected alphabetical order, not declaration order \
                         [survival, creative, adventure, spectator]"
                    );
                }
                other => panic!("expected Completion::Local, got {other:?}"),
            }
        }

        /// **Predicted**: a bare `/g` partial matches both root literals by
        /// prefix, alphabetically ordered (`gamemode` before `give`, since
        /// `'a' < 'i'` at the first differing character).
        /// **Rejected ranking**: registration order (`give` before
        /// `gamemode`, since `give` is node index 3 and `gamemode` is index
        /// 1 — a naive "return children in tree order" implementation would
        /// produce this).
        #[test]
        fn completes_literal_siblings_by_prefix_alphabetically() {
            let tree = gamemode_and_give_tree();
            match complete(&tree, "/g") {
                Completion::Local { start, candidates } => {
                    assert_eq!(start, 1);
                    let texts: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
                    assert_eq!(texts, vec!["gamemode", "give"]);
                }
                other => panic!("expected Completion::Local, got {other:?}"),
            }
        }

        /// A prefix that only one literal matches must exclude the other —
        /// distinguishing real prefix filtering from "return everything".
        #[test]
        fn completes_literal_siblings_excludes_non_matching_prefix() {
            let tree = gamemode_and_give_tree();
            match complete(&tree, "/gi") {
                Completion::Local { candidates, .. } => {
                    let texts: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
                    assert_eq!(texts, vec!["give"]);
                }
                other => panic!("expected Completion::Local, got {other:?}"),
            }
        }

        /// An argument node whose `suggestions` provider is present (here,
        /// `minecraft:ask_server`, the only provider id vanilla's own
        /// `SuggestionProviders` maps every *unrecognised* id to as well —
        /// see this module's own doc) must defer to the round trip rather
        /// than guess.
        #[test]
        fn defers_to_the_server_when_a_suggestions_provider_is_present() {
            let nodes = vec![
                root(vec![1]),
                literal("tp", false, vec![2]),
                argument(
                    "target",
                    ArgumentParser::Entity {
                        single: true,
                        players_only: true,
                    },
                    Some("minecraft:ask_server".parse().unwrap()),
                    true,
                    vec![],
                ),
            ];
            let tree = CommandTree::new(nodes, 0).unwrap();
            match complete(&tree, "/tp ") {
                Completion::NeedsServer { start } => assert_eq!(start, "/tp ".len()),
                other => panic!("expected Completion::NeedsServer, got {other:?}"),
            }
        }

        /// An opaque argument parser with **no** suggestions provider and
        /// **no** [`local_domain`] entry (`minecraft:block_pos`, which has no
        /// `listSuggestions` override in vanilla either) is also routed to
        /// the server here — the documented "never wrong, sometimes slower
        /// than vanilla" simplification, not a silent `Completion::None`.
        #[test]
        fn an_opaque_argument_with_no_provider_still_needs_the_server() {
            let nodes = vec![
                root(vec![1]),
                literal("tp", false, vec![2]),
                argument("pos", ArgumentParser::BlockPos, None, true, vec![]),
            ];
            let tree = CommandTree::new(nodes, 0).unwrap();
            match complete(&tree, "/tp ") {
                Completion::NeedsServer { .. } => {}
                other => panic!("expected Completion::NeedsServer, got {other:?}"),
            }
        }

        /// Once a token fails to parse, nothing further is offered — matching
        /// vanilla's own `updateUsageInfo`, which only fills usage/suggestions
        /// from `findSuggestionContext`, never past a hard parse failure.
        #[test]
        fn no_completions_past_a_failed_token() {
            let nodes = vec![
                root(vec![1]),
                literal("gamemode", false, vec![2]),
                argument("mode", ArgumentParser::GameMode, None, true, vec![]),
            ];
            let tree = CommandTree::new(nodes, 0).unwrap();
            // "spectatorx" is not a member of the GameMode domain, so it must
            // be treated as an unparseable token, not a prefix.
            assert_eq!(complete(&tree, "/gamemode spectatorx "), Completion::None);
        }

        /// Highlighting: a valid literal then a valid argument, matching
        /// vanilla's grey-then-cycling-colour shape.
        #[test]
        fn highlights_a_valid_literal_and_argument() {
            let tree = gamemode_and_give_tree();
            let spans = highlight(&tree, "/gamemode creative");
            assert_eq!(
                spans,
                vec![
                    HighlightSpan {
                        start: 0,
                        end: 1,
                        kind: HighlightKind::Literal,
                    },
                    HighlightSpan {
                        start: 1,
                        end: 9,
                        kind: HighlightKind::Literal,
                    },
                    HighlightSpan {
                        start: 10,
                        end: 18,
                        kind: HighlightKind::Argument(0),
                    },
                ]
            );
        }

        /// Highlighting: an out-of-range integer must redden from the point
        /// of failure to the end of the line, not just the offending token —
        /// matching vanilla's single trailing `UNPARSED_STYLE` run.
        #[test]
        fn highlights_an_out_of_range_integer_as_unparsed_to_end_of_line() {
            let nodes = vec![
                root(vec![1]),
                literal("xp", false, vec![2]),
                argument(
                    "amount",
                    ArgumentParser::Integer {
                        min: 0,
                        max: 100,
                    },
                    None,
                    true,
                    vec![],
                ),
            ];
            let tree = CommandTree::new(nodes, 0).unwrap();
            let line = "/xp 9999 extra";
            let spans = highlight(&tree, line);
            assert_eq!(
                spans,
                vec![
                    HighlightSpan {
                        start: 0,
                        end: 1,
                        kind: HighlightKind::Literal,
                    },
                    HighlightSpan {
                        start: 1,
                        end: 3,
                        kind: HighlightKind::Literal,
                    },
                    HighlightSpan {
                        start: 4,
                        end: line.len(),
                        kind: HighlightKind::Unparsed,
                    },
                ]
            );
        }

        /// A quoted `message`/`QuotablePhrase`-shaped string can contain
        /// spaces; the walker must read to the closing quote, not the next
        /// space, and an unterminated quote must redden from its opening
        /// quote to end of line.
        #[test]
        fn quoted_phrase_reads_to_the_closing_quote_not_the_next_space() {
            let nodes = vec![
                root(vec![1]),
                literal("say", false, vec![2]),
                argument(
                    "text",
                    ArgumentParser::String(StringKind::QuotablePhrase),
                    None,
                    true,
                    vec![],
                ),
            ];
            let tree = CommandTree::new(nodes, 0).unwrap();
            let line = r#"/say "hello there""#;
            let spans = highlight(&tree, line);
            assert_eq!(
                spans[2],
                HighlightSpan {
                    start: 5,
                    end: line.len(),
                    kind: HighlightKind::Argument(0),
                }
            );

            let unterminated = r#"/say "hello"#;
            let spans = highlight(&tree, unterminated);
            assert_eq!(
                spans[2],
                HighlightSpan {
                    start: 5,
                    end: unterminated.len(),
                    kind: HighlightKind::Unparsed,
                }
            );
        }

        /// A greedy phrase (`message`, or `brigadier:string` with
        /// `GreedyPhrase`) swallows the rest of the line, spaces included —
        /// unlike every other parser, whose token stops at the next space.
        #[test]
        fn greedy_phrase_consumes_the_rest_of_the_line() {
            let nodes = vec![
                root(vec![1]),
                literal("say", false, vec![2]),
                argument("text", ArgumentParser::Message, None, true, vec![]),
            ];
            let tree = CommandTree::new(nodes, 0).unwrap();
            let line = "/say hello there friend";
            let spans = highlight(&tree, line);
            assert_eq!(
                spans[2],
                HighlightSpan {
                    start: 5,
                    end: line.len(),
                    kind: HighlightKind::Argument(0),
                }
            );
            // The greedy argument runs to the cursor with no trailing space, so
            // it is *still being typed* and completion is offered by its parent
            // — see `still_typing`. `start: 5` is exactly where vanilla's
            // `findSuggestionContext` puts it: it returns
            // `SuggestionContext(prev = the "say" literal, start = 4)` in
            // slash-stripped coordinates, which is 5 with the slash.
            //
            // The result is `NeedsServer`, not `None`, because `Message` has no
            // `local_domain` entry — the same documented "never wrong, sometimes
            // slower than vanilla" route as
            // `an_opaque_argument_with_no_provider_still_needs_the_server`.
            // Identical on screen: `MessageArgument` has no `listSuggestions`
            // override, so Brigadier's default returns `Suggestions.empty()`
            // and the server answers our round trip with an empty list.
            //
            // This assertion previously read `Completion::None`, which was an
            // artefact of the walker advancing *into* the matched argument and
            // finding it childless — the same defect that made every
            // fully-typed command name complete to its own arguments. Both are
            // fixed by the same change.
            assert_eq!(complete(&tree, line), Completion::NeedsServer { start: 5 });
        }

        /// **The termination control this module's own doc promises.** A
        /// two-node redirect cycle (`jump` redirects to `loop`, `loop`
        /// redirects back to `jump`) must not hang either `complete` or
        /// `highlight` when the input position needs their *effective*
        /// children — the same guard proven in isolation by
        /// `lodestone_model::command_tree::tests::effective_children_terminates_on_a_redirect_cycle`,
        /// exercised here from this crate's own call sites instead of
        /// calling `effective_children` directly.
        #[test]
        fn complete_and_highlight_terminate_on_a_redirect_cycle() {
            // `jump` (index 1) redirects to `loop` (index 2), which redirects
            // straight back to `jump`. Neither has any real child of its
            // own, so the *only* way to reach either side of the cycle is
            // through the other's redirect — exactly the shape
            // `effective_children` must not hang expanding.
            let mut jump = literal("jump", false, vec![]);
            jump.redirect = Some(2);
            let mut loop_back = literal("loop", false, vec![]);
            loop_back.redirect = Some(1);
            let nodes = vec![root(vec![1, 2]), jump, loop_back];
            let tree = CommandTree::new(nodes, 0).unwrap();

            // Matching the literal `jump` moves `complete`'s walk onto node 1
            // — from there, asking "what comes next" is exactly the call
            // that must walk the cycle. Neither call below may hang; reaching
            // either assertion at all is the proof. Node 1 has no children of
            // its own and node 2 (its redirect target) has none either, so
            // the effective set is empty and there is nothing to suggest.
            assert_eq!(complete(&tree, "/jump "), Completion::None);
            assert_eq!(highlight(&tree, "/jump ").len(), 2);
        }

        /// Lines that are not commands (`chat`, or an empty buffer) get no
        /// highlighting and no completions — this module covers the
        /// Brigadier tree only.
        #[test]
        fn non_command_lines_are_untouched() {
            let tree = gamemode_and_give_tree();
            assert!(highlight(&tree, "hello world").is_empty());
            assert_eq!(complete(&tree, "hello world"), Completion::None);
            assert!(highlight(&tree, "").is_empty());
            assert_eq!(complete(&tree, ""), Completion::None);
        }

        /// The request/response round trip: a request assigns and tracks a
        /// transaction id; a reply with a *different* id (representing a
        /// stale answer to a request the input has since outgrown) must be
        /// dropped, and the correct id must still be honoured afterward.
        #[test]
        fn suggestion_requests_drop_a_stale_reply_and_honour_the_current_one() {
            let mut requests = SuggestionRequests::new();
            let action = requests.request("/tp @");
            let ClientAction::CommandSuggestion { id, command } = action else {
                panic!("expected CommandSuggestion action")
            };
            assert_eq!(command, "/tp @");
            assert!(requests.is_pending());

            // A reply to an old id (e.g. a previous keystroke's request) is
            // dropped, not applied.
            assert_eq!(
                requests.receive(
                    id - 1,
                    vec![CommandSuggestionEntry {
                        text: "stale".to_string(),
                        tooltip: None,
                    }]
                ),
                None
            );
            assert!(
                requests.is_pending(),
                "a stale reply must not clear the pending request"
            );

            let entries = vec![CommandSuggestionEntry {
                text: "@a".to_string(),
                tooltip: Some("all players".to_string()),
            }];
            let received = requests.receive(id, entries).expect("current id honoured");
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].text, "@a");
            assert_eq!(received[0].tooltip.as_deref(), Some("all players"));
            assert!(!requests.is_pending());
        }

        /// Two requests in a row: the second must get a different
        /// transaction id, and only the second's id is honoured afterward —
        /// the id is not just present but actually monotonically assigned.
        #[test]
        fn successive_requests_get_distinct_ids() {
            let mut requests = SuggestionRequests::new();
            let first = requests.request("/tp @a");
            let second = requests.request("/tp @p");
            let ClientAction::CommandSuggestion { id: first_id, .. } = first else {
                panic!("expected a CommandSuggestion action")
            };
            let ClientAction::CommandSuggestion { id: second_id, .. } = second else {
                panic!("expected a CommandSuggestion action")
            };
            assert_ne!(first_id, second_id);
            // The first id is now stale (a third request never happened, but
            // the second's request superseded it).
            assert_eq!(requests.receive(first_id, vec![]), None);
            assert_eq!(requests.receive(second_id, vec![]), Some(vec![]));
        }
    }
}
