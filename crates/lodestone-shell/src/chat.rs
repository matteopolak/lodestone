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
//! ## Command-tree consumption
//!
//! [`highlight`] and [`complete`] are this crate's half of the Brigadier
//! command-tree UX. Both walk a [`CommandTree`] (decoded upstream, from the
//! `minecraft:commands` packet — see `lodestone_model::command_tree`'s own
//! doc for the wire shape; that decode landed in `090f2ff`/that fix and the fold
//! into `net::CommandTreeCell` in `8b0aede`/that fix, so this doc's old "still
//! has to be brokered into a protocol-crate decode arm" is done) against the
//! *current* input line **up to the caret** — vanilla's own
//! `value.substring(0, cursorPosition)`, which
//! [`ChatInput::recompute_suggestions`] is what supplies. So there is still no
//! separate cursor position to track in here: the string these are handed
//! always ends at the caret, by construction at the call site. That used to be
//! true because the line had no caret to be anywhere but the end; it is now
//! true because the caller slices.
//!
//! Both functions share one internal walker ([`parse_line`]) that consumes
//! tokens left to right: a literal child matches by exact text; an argument
//! child is read according to its [`lodestone_model::command_tree::ArgumentParser`] (a
//! greedy phrase or `message` argument swallows the rest of the line; a
//! quoted phrase reads to its closing `"`; everything else reads to the next
//! space) and validated where a parser's grammar is simple enough to check
//! locally — the Brigadier primitives via `lodestone-command` (
//! next paragraph), and the small fixed-domain Minecraft parsers via
//! [`local_domain`]. The first token that fails to match anything —
//! diverging from every viable child of the tree — ends the walk, and
//! everything from there to the end of the line is
//! [`HighlightKind::Unparsed`].
//!
//! **Argument validation delegates to `lodestone-command`, not a
//! hand-rolled copy.** [`validate_simple`]'s numeric bounds, `bool`, and
//! string-kind checks run the matching `lodestone_command` argument type
//! (`IntegerArgument`/`LongArgument`/`FloatArgument`/`DoubleArgument`/
//! `BoolArgument`/`StringArgument`) over a `lodestone_command::StringReader`,
//! and [`read_quoted`] is that reader's `read_string` — Brigadier's
//! `\"`/`\\` escape handling and its `[0-9A-Za-z_.+-]` unquoted charset are
//! no longer reimplemented here. Only the Minecraft-flavoured parsers
//! (`GameMode`, `Operation`, `EntityAnchor`, `TeamColor`, `ScoreboardSlot`)
//! stay local, because `lodestone-command` deliberately ships no Minecraft
//! argument types; their fixed value sets remain in [`local_domain`]. One
//! consequence: `StringReader` counts positions in `char`s while this
//! module's spans are byte offsets, so [`read_quoted`] converts at the
//! boundary.
//!
//! **That failure is only sometimes fatal to [`complete`], and getting this
//! wrong was this module's own first bug.** Since the text these are handed
//! always ends at the caret, a failing token that is *also the last thing
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
//!   `docs/commands.md`. **This applies to argument nodes inside a command
//!   only.** The *non*-command case — Tab in an ordinary chat line — is
//!   answered locally, exactly as vanilla does; see [`ChatInput::tab`] and
//!   [`ChatInput::set_online_players`]. The names arrive as a plain
//!   `Vec<String>` the caller refreshes, so this module still holds no client
//!   handle.
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
//! ## What presses the key
//!
//! Two seams, not one, and mixing them up is how the dropdown would go back to
//! being Tab-only:
//!
//! * [`ChatInput::update_command_info`] is vanilla's `EditBox` responder
//!   (`ChatScreen.onEdited` → `CommandSuggestions.updateCommandInfo`) and is
//!   what makes the popup appear **while typing**.
//!   `app::menus::handle_chat_key` calls it after every edit.
//! * [`ChatInput::tab`] is the Tab key
//!   (`CommandSuggestions.keyPressed`), plus [`ChatInput::suggestion_up`],
//!   [`ChatInput::suggestion_down`] and [`ChatInput::suggestion_escape`] for the
//!   rest of `SuggestionsList.keyPressed`. The mouse half —
//!   [`ChatInput::suggestion_hover`], [`ChatInput::suggestion_click`],
//!   [`ChatInput::suggestion_scroll`] — is driven from `app::lifecycle`'s
//!   pointer arms against the rect `hud::suggestion_layout` resolved for the
//!   frame.
//!
//! Either seam can return a [`ClientAction`] for a
//! [`Completion::NeedsServer`] position; `app::menus::pump_command_suggestions`
//! polls `net::CommandTreeCell` for the reply and feeds it to
//! [`ChatInput::apply_suggestions`]. Before those arms existed, [`complete`] and
//! [`SuggestionRequests`] had **no production caller at all** — the island this
//! module's tests could not see, because a crate's own test suite is a closed
//! loop. `crates/lodestone-shell/tests/command_tree_completion.rs` is the gate
//! that drives the whole chain against a real 26.2 server's captured tree.
//!
//! The popup's own geometry — the row height, the visible window, the rect the
//! pointer is tested against — is [`hud::suggestion_layout`]'s, because it needs
//! font metrics this module deliberately has none of (see just below). This
//! module owns the *state machine* and nothing that measures a glyph.
//!
//! [`hud::suggestion_layout`]: crate::hud::suggestion_layout
//!
//! `highlight`/`complete` return **byte spans into the input string**, not
//! screen pixels — this crate has no font metrics and does not compute any.
//! Mapping a span to a pixel run belongs wherever the draw call already
//! measures real glyph advances for word wrap (`hud.rs`'s
//! `Builder::legacy_width`/`VanillaFont`, per `docs/chat.md`); a caller that
//! instead assumed one span character equals one fixed-width column would be
//! wrong for the same reason a character-count word-wrap would be, since
//! Minecraft's font is proportional.

use crate::menu::edit_box::EditBox;
use crate::menu::focus::KeyEvent;
use lodestone_client::ClientAction;
use lodestone_command::{
    ArgumentType, BoolArgument, DoubleArgument, FloatArgument, IntegerArgument, LongArgument,
    StringArgument, StringReader,
};
use lodestone_model::command_tree::{
    ArgumentParser, CommandSuggestionEntry, CommandTree, NodeKind, StringKind,
};
use lodestone_model::text::Text;

/// Vanilla's cap on the recent-chat store — `new ArrayListDeque<>(100)` plus the
/// `size() >= 100 → removeFirst()` guard in `ChatComponent.addRecentChat`.
pub const RECENT_CHAT_MAX: usize = 100;

/// `ChatScreen.init`'s `this.input.setMaxLength(256)`, in `char`s.
pub const MAX_CHAT_LENGTH: usize = 256;

/// What one key press did to the chat line — [`ChatInput::handle_key`]'s answer.
///
/// Two independent bits, not one: see that method's own doc for why a consumed
/// key and an edited line are different questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatKeyResult {
    /// The box acted on the key, so the caller must not also treat it as text.
    pub consumed: bool,
    /// The line's *value* changed, so the command-suggestion responder is due.
    pub edited: bool,
}

/// The lines the player has **sent**, oldest first, for the Up/Down arrows.
///
/// Vanilla's `ChatComponent.recentChat`. The thing to notice is that it does
/// **not** live on the chat screen: the deque belongs to the persistent HUD
/// component and the screen only holds a *cursor* into it
/// (`ChatScreen.historyPos`), so reopening chat still walks everything sent
/// earlier in the session.
///
/// Here it is a field of [`ChatInput`], which is the equivalent placement rather
/// than a shortcut: `ChatInput` is owned by the app for the whole process
/// lifetime and survives every chat open, close and cancel — only its `buf` is
/// screen-scoped. What matters is that the store outlives the screen, and it
/// does. The cursor into it is [`ChatInput::history_pos`], reset on open exactly
/// as `ChatScreen.init` resets `historyPos`.
///
/// Three behaviours transcribed from `addRecentChat`, each of which a plausible
/// implementation gets wrong:
///
/// * newest is **last**, so Up walks *backwards* from one past the end;
/// * a line equal to the current **last** entry is not stored at all —
///   consecutive duplicates collapse, but a line repeated with something else in
///   between is stored twice (`!message.equals(peekLast())`, not a set);
/// * the cap is [`RECENT_CHAT_MAX`] and the *oldest* is dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatHistory {
    entries: Vec<String>,
}

impl ChatHistory {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a line the player just sent — `ChatComponent.addRecentChat`.
    ///
    /// The line is **normalised first**, because vanilla records the normalised
    /// form and not the keystrokes: `ChatScreen.handleChatInput` runs
    /// `normalizeChatMessage` (`StringUtil.trimChatMessage(normalizeSpace(msg.trim()))`)
    /// and only then `if (!msg.isEmpty())` guards the store. `normalizeSpace`
    /// collapses every internal whitespace run to one space, so recalling
    /// `"hello    world"` really does give back `"hello world"` — and, less
    /// obviously, it is what makes the consecutive-duplicate check below fire for
    /// two sends that differed only in spacing.
    pub fn record(&mut self, line: &str) {
        let line = normalize_space(line);
        if line.is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|last| *last == line) {
            return;
        }
        if self.entries.len() >= RECENT_CHAT_MAX {
            self.entries.remove(0);
        }
        self.entries.push(line);
    }

    /// The stored lines, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// How many lines are stored — the value `historyPos` starts at, and the
    /// upper clamp `moveInHistory` uses.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been sent yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `StringUtils.normalizeSpace` — trim, then collapse every internal whitespace
/// run to a single space.
fn normalize_space(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The line currently being typed. Kept separate from the log so opening the
/// chat box, editing, and cancelling never touch the received history.
#[derive(Debug, Clone)]
pub struct ChatInput {
    /// The line itself — vanilla's `ChatScreen.input`, an
    /// [`EditBox`], not a bare `String`.
    ///
    /// This used to be a `String` edited only at its end, and the whole of
    /// ordinary text editing was missing as a result: no caret to move, so no
    /// Left/Right, no Home/End, no shift-selection, no word skip, no
    /// select-all, and copy/cut had nothing to read. `ChatScreen.init` builds
    /// a real `EditBox` (`setMaxLength(256)`, `setBordered(false)`,
    /// `setCanLoseFocus(false)`, focused on init), and every one of those
    /// behaviours is already ported on this type — routing the chat line
    /// through it is therefore reuse, not a second implementation. See
    /// [`Self::new_box`] for the construction and for what geometry means here.
    buf: EditBox,
    /// Tab-completion state for **this** line — see [`ChatCompletion`] for why
    /// it lives inside the input rather than beside it.
    completion: ChatCompletion,
    /// The player names Tab completes against when the line is **not** a
    /// command, refreshed by the caller from the live tab list — see
    /// [`Self::set_online_players`].
    online_players: Vec<String>,
    /// The cursor into [`ChatHistory`], in `0..=history.len()`.
    ///
    /// `history.len()` — one past the last entry — is vanilla's "live buffer"
    /// slot, the value `ChatScreen.init` assigns. Walking off that slot stashes
    /// the part-typed line in [`Self::history_buffer`]; walking back onto it
    /// restores it. Reset by [`Self::take`], which is what the chat-open path
    /// calls, so this reproduces `init`'s assignment without needing a hook of
    /// its own.
    history_pos: usize,
    /// The part-typed line stashed when the arrows first leave the live slot —
    /// `ChatScreen.historyBuffer`.
    history_buffer: String,
    /// Everything the player has sent this session — see [`ChatHistory`].
    history: ChatHistory,
    /// The received scrollback's scroll position — see [`ChatScroll`]. Lives
    /// here rather than beside the received log itself
    /// ([`lodestone_game::chat::ChatLog`]) for the same reason
    /// [`Self::history`] does: it is chat-*screen* state (open, closed,
    /// scrolled), not message content, and this crate's version-free log
    /// deliberately holds neither a client handle nor any UI notion of
    /// "open" to check against.
    scroll: ChatScroll,
}

impl Default for ChatInput {
    fn default() -> Self {
        Self {
            buf: Self::new_box(),
            completion: ChatCompletion::default(),
            online_players: Vec::new(),
            history_pos: 0,
            history_buffer: String::new(),
            history: ChatHistory::default(),
            scroll: ChatScroll::default(),
        }
    }
}

impl ChatInput {
    /// An empty input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The chat box's own [`EditBox`], configured as `ChatScreen.init`
    /// configures vanilla's.
    ///
    /// Three of the four settings are vanilla's literally: `setMaxLength(256)`,
    /// `setBordered(false)`, `setCanLoseFocus(false)`. The fourth —
    /// `widget.focused` — stands for `setInitialFocus(this.input)`, and it has
    /// to be set here rather than on open because this shell has no `Screen`
    /// focus layer around the chat prompt: `EditBox::handle_key` declines
    /// every key on an unfocused box, so an unfocused chat line would accept
    /// nothing at all.
    ///
    /// **The geometry is deliberately not vanilla's**, and the reason is worth
    /// stating because a reader will otherwise "fix" it. Vanilla's box is
    /// `(4, height - 12, width - 4, 12)`, and the only thing those numbers feed
    /// is `EditBox::display_pos` — the horizontal scroll of a box narrower than
    /// its contents — plus `text_x`/`text_y`, none of which the chat HUD reads:
    /// it draws the whole line itself at its own anchor. A width narrower than
    /// the value would therefore leave `display_pos` scrolled to a window
    /// nothing consults, which is state that disagrees with the screen. Sizing
    /// the box so the full 256-character budget always fits keeps `display_pos`
    /// pinned at `0`, which is what the draw actually does.
    fn new_box() -> EditBox {
        let mut b = EditBox::new(0.0, 0.0, 0.0, 0.0, "chat.editBox");
        b.set_max_length(MAX_CHAT_LENGTH);
        b.bordered = false;
        b.can_lose_focus = false;
        b.widget.focused = true;
        b.widget.width = b.advance * MAX_CHAT_LENGTH as f32;
        b
    }

    /// Seed the buffer (e.g. a leading `/` when chat is opened with the command
    /// key). Replaces any current contents, leaving the caret at the end —
    /// `EditBox.setValue`'s own `moveCursorToEnd`.
    pub fn set(&mut self, text: impl Into<String>) {
        self.buf.set_value(text.into());
        // A wholesale replacement is a different line, so any list and any
        // in-flight request are about text that no longer exists.
        self.completion.reset();
    }

    /// Insert typed or pasted text **at the caret**, replacing any selection —
    /// `EditBox.insertText`. Characters vanilla's chat filter rejects (the C0
    /// controls, DEL, and the `§` used by legacy colour codes) are dropped, and
    /// the whole insertion is truncated to what [`MAX_CHAT_LENGTH`] still
    /// allows, so a paste or an IME cannot inject either.
    pub fn push_str(&mut self, text: &str) {
        self.buf.insert_text(text);
    }

    /// One typed character — `EditBox.charTyped`. Same filter and same cap as
    /// [`Self::push_str`], which this now shares rather than restates.
    pub fn push_char(&mut self, ch: char) {
        self.buf.handle_char(ch);
    }

    /// The current text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.buf.value()
    }

    /// The caret's position, as a **`char`** index — `EditBox.getCursorPosition`.
    #[must_use]
    pub fn cursor_position(&self) -> usize {
        self.buf.cursor_position()
    }

    /// The selection as a `char` range, or `None` when the caret is a plain
    /// insertion point. Ordered — `EditBox.highlightPos` may sit either side of
    /// the caret, depending on which way the selection was dragged.
    #[must_use]
    pub fn selection(&self) -> Option<(usize, usize)> {
        let (cursor, highlight) = (self.buf.cursor_position(), self.buf.highlight_position());
        (cursor != highlight).then(|| (cursor.min(highlight), cursor.max(highlight)))
    }

    /// The caret's position as a **byte** offset into [`Self::as_str`] — what a
    /// draw or a splice needs, since every span in this module is a byte offset.
    #[must_use]
    fn cursor_byte(&self) -> usize {
        let cursor = self.buf.cursor_position();
        self.buf
            .value()
            .char_indices()
            .nth(cursor)
            .map_or_else(|| self.buf.value().len(), |(i, _)| i)
    }

    /// The line up to the caret — vanilla's
    /// `input.getValue().substring(0, input.getCursorPosition())`, which is what
    /// `CommandSuggestions` completes against rather than the whole value.
    #[must_use]
    fn partial(&self) -> &str {
        &self.buf.value()[..self.cursor_byte()]
    }

    /// Route one key through the box — the whole of copy/cut/paste, select-all,
    /// word-wise motion and deletion, shift-selection and Home/End, all of it
    /// [`EditBox::handle_key`]'s existing port rather than a second one here.
    ///
    /// Returns whether the key was **consumed** and whether the *value* changed,
    /// separately, because the caller needs both and they are different
    /// questions: a consumed key that only moved the caret must not fall
    /// through to the text-insertion path, and must not re-request suggestions
    /// either. Vanilla splits them the same way — `EditBox.keyPressed` returns
    /// the first, and the `setResponder` callback that drives
    /// `updateCommandInfo` fires only on the second (`onValueChange`).
    pub fn handle_key(&mut self, event: KeyEvent) -> ChatKeyResult {
        let before = self.buf.value().to_owned();
        let consumed = self.buf.handle_key(event);
        ChatKeyResult {
            consumed,
            edited: self.buf.value() != before,
        }
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Clear and return the typed line, ready to compose into an action.
    ///
    /// Also rewinds the history cursor, which is how `ChatScreen.init`'s
    /// `historyPos = getRecentChat().size()` is reproduced without a separate
    /// open hook: every path that opens the chat box clears the buffer through
    /// here first. [`Self::history_up`] resolves the cursor against the store it
    /// is handed, so "one past the end" needs no length stored on this side.
    #[must_use]
    pub fn take(&mut self) -> String {
        self.completion.reset();
        self.history_pos = usize::MAX;
        self.history_buffer.clear();
        let line = self.buf.value().to_owned();
        // `setValue("")` rather than a `mem::take` of a bare `String`: the caret
        // and the selection are part of the line's state now, and leaving them
        // pointing into text that no longer exists is the trap `on_value_change`
        // exists to close.
        self.buf.set_value("");
        line
    }

    /// The player names Tab offers when the line is not a command.
    ///
    /// Vanilla recomputes this on every keystroke through
    /// `ClientSuggestionProvider.getCustomTabSuggestions`; the caller refreshes
    /// it from the live tab list instead, which keeps this module free of a
    /// client handle exactly as its own header requires. Pass **every** entry,
    /// listed or not: vanilla's provider reads `getOnlinePlayers()`, not
    /// `getListedOnlinePlayers()`, so a player hidden from the tab overlay is
    /// still completable.
    pub fn set_online_players(&mut self, names: Vec<String>) {
        self.online_players = names;
    }

    /// Record a line the player just sent, so the arrows can recall it —
    /// `ChatComponent.addRecentChat`, called from
    /// `ChatScreen.handleChatInput(msg, addToRecent = true)`.
    ///
    /// **Call this before [`Self::take`]**, which is what actually clears the
    /// line: `take` is on the cancel path too, and a cancelled line is not part
    /// of the history.
    pub fn record_sent(&mut self, line: &str) {
        self.history.record(line);
    }

    /// The lines sent this session — for a draw, and for a test to assert the
    /// exact store rather than "something was remembered".
    #[must_use]
    pub fn history(&self) -> &ChatHistory {
        &self.history
    }

    /// Up in the chat box — `ChatScreen.moveInHistory(-1)`.
    ///
    /// Returns whether the line changed, so the caller can tell a consumed key
    /// from one that should fall through.
    pub fn history_up(&mut self) -> bool {
        self.move_in_history(-1)
    }

    /// Down in the chat box — `ChatScreen.moveInHistory(1)`.
    pub fn history_down(&mut self) -> bool {
        self.move_in_history(1)
    }

    /// `ChatScreen.moveInHistory`, transcribed.
    ///
    /// The three edge behaviours, all of which are easy to invent differently:
    ///
    /// * the cursor **clamps** at both ends rather than wrapping — Up at the
    ///   oldest entry and Down at the live slot both do nothing, and vanilla
    ///   detects that as `newPos == historyPos` and returns without touching the
    ///   line;
    /// * the part-typed line is **preserved**: leaving the live slot stashes it,
    ///   coming back restores it, so a half-written message survives a look
    ///   through the history;
    /// * moving onto a stored entry cancels any suggestion list
    ///   (`setAllowSuggestions(false)`), because the recalled line is not one the
    ///   player is mid-way through typing.
    fn move_in_history(&mut self, dir: i32) -> bool {
        let max = self.history.len();
        // `usize::MAX` is this side's spelling of "at the live slot", set by
        // `take` before the length of the store is known here.
        let pos = self.history_pos.min(max);
        let new_pos = (pos as i64 + i64::from(dir)).clamp(0, max as i64) as usize;
        if new_pos == pos {
            return false;
        }
        if new_pos == max {
            self.history_pos = max;
            self.buf.set_value(self.history_buffer.clone());
        } else {
            if pos == max {
                self.history_buffer = self.buf.value().to_owned();
            }
            let recalled = self.history.entries()[new_pos].clone();
            self.buf.set_value(recalled);
            self.completion.reset();
            self.history_pos = new_pos;
        }
        true
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

/// One highlighted run of a command line, mirroring vanilla's own
/// command-suggestions format-text routine.
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
    ///. Colour-to-`ChatFormatting` mapping
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
    /// Carried as a real [`Text`] rather than a flattened `§`-coded string so
    /// a hex-coloured tooltip (`TextColor::Rgb`, added in 1.16) survives to
    /// the draw site — see [`CommandSuggestionEntry::tooltip`]'s own doc.
    pub tooltip: Option<Text>,
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
/// Doubles as the validity check [`validate_simple`] falls back to during
/// highlighting for the Minecraft-flavoured parsers that have no
/// `lodestone-command` equivalent: a value for one of those types is only
/// valid when it is a member of its domain, which is exactly the completion
/// condition too. (The Brigadier primitives — `Bool` included — are validated
/// by their `lodestone-command` argument type instead, that fix; `Bool`
/// stays in this table because it also drives completion.)
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

/// The byte offset in `text` of the `char_offset`-th `char`, or `text.len()`
/// when `char_offset` is past the end. `lodestone_command::StringReader`
/// counts positions in `char`s while this module's spans are byte offsets;
/// [`read_quoted`] is where the two meet.
fn char_index_to_byte(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map_or(text.len(), |(b, _)| b)
}

/// Reads a Brigadier-style quoted string starting at `body[start] == b'"'`,
/// delegating to `lodestone_command::StringReader::read_string`
/// rather than reimplementing `readQuotedString`'s `\"`/`\\` escape handling
/// (Brigadier library behaviour — see [`StringKind`]'s doc for the "not
/// sourced from `.cache/mc/26.2` this session" caveat). Returns `(end,
/// valid)`, with `end` a **byte** offset into `body` (the reader counts in
/// `char`s, hence [`char_index_to_byte`]); `valid` is `false` when the
/// closing quote is never found, in which case `end == body.len()`.
fn read_quoted(body: &str, start: usize) -> (usize, bool) {
    let tail = &body[start..];
    let mut reader = StringReader::new(tail);
    match reader.read_string() {
        Ok(_) => (start + char_index_to_byte(tail, reader.cursor()), true),
        Err(_) => (body.len(), false),
    }
}

/// Runs a `lodestone-command` argument type over `text` (an already-isolated
/// token) and reports whether it consumed the token cleanly. The
/// full-consumption check matters: `StringReader` stops at the first
/// disallowed char (`read_int` on `12x` parses `12`, `read_unquoted_string`
/// on `hi!` returns `hi`), and a value with trailing text is a *failed* token
/// for a space-delimited argument, not a success.
fn parse_ok(argument_type: &dyn ArgumentType, text: &str) -> bool {
    let mut reader = StringReader::new(text);
    argument_type
        .parse(&mut reader)
        .is_ok_and(|_| reader.cursor() == reader.len())
}

/// Whether `text` (already isolated as a single token) is a valid value for
/// `parser`. The Brigadier primitives — numeric bounds, `bool`, and the
/// three string kinds — delegate to the matching `lodestone-command` argument
/// type via [`parse_ok`], and the small fixed-domain Minecraft
/// parsers check [`local_domain`] membership. Every other parser — opaque
/// Minecraft types with no locally checkable grammar — is treated as always
/// valid; see this module's own doc for why that is the deliberately safe
/// direction to simplify in.
fn validate_simple(parser: &ArgumentParser, text: &str) -> bool {
    match parser {
        ArgumentParser::Integer { min, max } => {
            parse_ok(&IntegerArgument::bounded(*min, *max), text)
        }
        ArgumentParser::Long { min, max } => parse_ok(&LongArgument::bounded(*min, *max), text),
        ArgumentParser::Float { min, max } => {
            parse_ok(&FloatArgument::bounded(*min, *max), text)
        }
        ArgumentParser::Double { min, max } => {
            parse_ok(&DoubleArgument::bounded(*min, *max), text)
        }
        ArgumentParser::Bool => parse_ok(&BoolArgument, text),
        ArgumentParser::String(StringKind::SingleWord) => {
            parse_ok(&StringArgument::word(), text)
        }
        ArgumentParser::String(StringKind::QuotablePhrase) => {
            parse_ok(&StringArgument::quotable(), text)
        }
        ArgumentParser::String(StringKind::GreedyPhrase) => {
            parse_ok(&StringArgument::greedy(), text)
        }
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
        // **This is not always a hard failure.** `line` always ends at the
        // caret (the caller slices it there), so when this failing
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

/// Byte offset of the word the cursor is in — `CommandSuggestions.getLastWordIndex`,
/// whose body walks `WHITESPACE_PATTERN` (`(\s+)`) and keeps the **end** of the
/// last match.
///
/// So it is the offset just past the final whitespace run, or `0` when there is
/// none. Note this is the end of the *last* run and not the position of the last
/// space: `"hi   Ste"` gives the index of `S`, three past the first space, which
/// is why a `rfind(' ') + 1` transcription is only accidentally right on
/// single-spaced input.
fn last_word_index(text: &str) -> usize {
    let mut result = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // ASCII whitespace is the whole of what `\s` matches for chat input, and
        // a multi-byte char never contains an ASCII byte, so a byte walk is safe.
        if bytes[i].is_ascii_whitespace() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            result = j;
            i = j;
        } else {
            i += 1;
        }
    }
    result
}

/// The characters `SharedSuggestionProvider.MATCH_SPLITTER` treats as word
/// boundaries inside a candidate — `CharMatcher.anyOf("._/")`.
const MATCH_SPLITTER: [char; 3] = ['.', '_', '/'];

/// `SharedSuggestionProvider.matchesSubStr`: does `input` start with `pattern`,
/// at offset `0` or immediately after any [`MATCH_SPLITTER`] character?
///
/// **Not a plain `starts_with`, and not a plain `contains` either.** It is
/// "prefix of some splitter-delimited segment", which for player names means
/// `Notch_The_Second` is matched by `the` as well as by `notch` — but *not* by
/// `otch`. Both callers pass already-lower-cased strings; the folding is the
/// caller's job in vanilla too.
fn matches_sub_str(pattern: &str, input: &str) -> bool {
    let mut index = 0;
    loop {
        if input[index..].starts_with(pattern) {
            return true;
        }
        match input[index..].find(MATCH_SPLITTER) {
            Some(off) => index += off + 1,
            None => return false,
        }
    }
}

/// The player-name candidates for a partly-typed word, in vanilla's own order.
///
/// Two orderings compose, and both come from vanilla:
///
/// 1. brigadier's `Suggestions.create` sorts the built list with
///    `compareToIgnoreCase`;
/// 2. `CommandSuggestions.sortSuggestions` then moves every candidate that
///    literally **starts with** the lower-cased partial word ahead of the ones
///    that only matched through a splitter, preserving (1) within each group.
///
/// So `Ste` offers `Steve` before `My_Steve`, and both before nothing else.
fn player_name_candidates(names: &[String], partial: &str) -> Vec<Candidate> {
    let needle = partial.to_lowercase();
    let mut matched: Vec<&String> = names
        .iter()
        .filter(|name| matches_sub_str(&needle, &name.to_lowercase()))
        .collect();
    matched.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    matched.dedup();
    let (prefix, rest): (Vec<&String>, Vec<&String>) = matched
        .into_iter()
        .partition(|name| name.to_lowercase().starts_with(&needle));
    prefix
        .into_iter()
        .chain(rest)
        .map(|name| Candidate {
            text: name.clone(),
            tooltip: None,
        })
        .collect()
}

/// The most rows the popup shows at once — `ChatScreen.init`'s
/// `suggestionLineLimit` argument to the `CommandSuggestions` constructor
/// (`new CommandSuggestions(…, 1, 10, true, -805306368)`). Candidates past this
/// are reachable by scrolling the window, not by growing the box.
pub const SUGGESTION_LINE_LIMIT: usize = 10;

/// `ChatScreen.init`'s `lineStartOffset` argument — the `1` in the same
/// constructor call. It appears in exactly one expression,
/// [`SuggestionsList::cycle`]'s forward scroll, and nowhere else; naming it
/// separately is what keeps that from reading as an off-by-one.
pub const SUGGESTION_LINE_START_OFFSET: usize = 1;

/// The dropdown itself — vanilla's `CommandSuggestions.SuggestionsList`,
/// transcribed field for field.
///
/// # The three pieces of state, and why each is separate
///
/// * `current` is the highlighted row, an index into **`candidates`**, not into
///   the visible window. It wraps at both ends ([`Self::select`]).
/// * `offset` is the first *visible* row, so the window is
///   `candidates[offset .. offset + rows()]`. Vanilla scrolls it only from
///   [`Self::cycle`] and the mouse wheel — never from a bare `select`, which is
///   why hover and click do not jump the window under the pointer.
/// * `tab_cycles` is what makes one key do two jobs. Vanilla's
///   `SuggestionsList.keyPressed` runs `if (tabCycles) cycle(...)` *before*
///   `useSuggestion()`, and only `useSuggestion` sets the flag: so the **first**
///   Tab commits the highlighted row without moving, and every Tab after that
///   moves first. The arrows clear it (`tabCycles = false`), so browsing with
///   Up/Down and then pressing Tab commits what you are looking at rather than
///   the one after it.
///
/// `original` is vanilla's `originalContents` — the line as it was when the list
/// was built. Every commit is computed from *that*, not from the current line,
/// which is what lets Tab cycle: the second Tab replaces the first Tab's text
/// rather than appending to it. Brigadier's `Suggestion.apply` splices over the
/// suggestion's own range, and in the chat box that range always ends at the
/// caret — [`Self::end`] — so the splice is
/// `original[..start] + text + original[end..]`. That tail is a no-op whenever
/// the caret is at the end of the line, which is the only case this type used
/// to have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionsList {
    /// Byte offset into [`Self::original`] the candidate text replaces from.
    start: usize,
    /// Byte offset into [`Self::original`] the candidate text replaces **to** —
    /// the caret at the moment the list was built, which is where brigadier's
    /// `StringRange` ends. Equal to `original.len()` whenever the caret is at
    /// the end of the line, which is the only case this type used to have.
    end: usize,
    /// The line as it was when this list was built — `originalContents`.
    original: String,
    candidates: Vec<Candidate>,
    /// First visible row; `0` until a cycle or a scroll moves the window.
    offset: usize,
    /// The highlighted row, an index into [`Self::candidates`].
    current: usize,
    /// Whether the next Tab moves the selection before committing it — see the
    /// struct doc.
    tab_cycles: bool,
}

impl SuggestionsList {
    /// How many rows are visible — `Math.min(suggestionList.size(),
    /// suggestionLineLimit)`.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.candidates.len().min(SUGGESTION_LINE_LIMIT)
    }

    /// The largest `offset` that still fills the window —
    /// `Math.max(size - suggestionLineLimit, 0)`, the clamp ceiling both
    /// [`Self::cycle`] and [`Self::scroll`] use.
    fn max_offset(&self) -> usize {
        self.candidates.len().saturating_sub(SUGGESTION_LINE_LIMIT)
    }

    /// `SuggestionsList.select`: set the highlighted row, **wrapping** by one
    /// list length at each end.
    ///
    /// Vanilla's body adds or subtracts `size()` exactly once, so it is only
    /// correct for a step of ±1 — which is all `cycle` and the mouse ever ask
    /// for. `index` is signed here for the same reason vanilla's can go negative.
    fn select(&mut self, index: isize) {
        let len = self.candidates.len() as isize;
        if len == 0 {
            return;
        }
        let mut current = index;
        if current < 0 {
            current += len;
        }
        if current >= len {
            current -= len;
        }
        self.current = current.clamp(0, len - 1) as usize;
    }

    /// `SuggestionsList.cycle`: move the selection one step and scroll the
    /// window to keep it visible.
    ///
    /// The two scroll branches are not symmetric in vanilla and that asymmetry
    /// is what makes a wrap land correctly. Stepping off the top clamps the
    /// selection itself as the new offset; stepping off the bottom uses
    /// `current + lineStartOffset - suggestionLineLimit`, so wrapping from row 0
    /// back to the last row scrolls all the way to [`Self::max_offset`] in one
    /// move instead of one row at a time.
    fn cycle(&mut self, direction: isize) {
        self.select(self.current as isize + direction);
        let first = self.offset;
        let last = self.offset + SUGGESTION_LINE_LIMIT - 1;
        if self.current < first {
            self.offset = self.current.min(self.max_offset());
        } else if self.current > last {
            self.offset = (self.current + SUGGESTION_LINE_START_OFFSET)
                .saturating_sub(SUGGESTION_LINE_LIMIT)
                .min(self.max_offset());
        }
    }

    /// `SuggestionsList.mouseScrolled`, minus the rect test the caller has
    /// already done: `offset = clamp(offset - scroll, 0, max_offset)` with
    /// `scroll` itself clamped to `-1..=1` by `CommandSuggestions.mouseScrolled`.
    ///
    /// **The selection does not move.** Scrolling changes only which rows are on
    /// screen, so the highlight can scroll out of view — vanilla's behaviour, and
    /// the reason `current` and `offset` are separate fields at all.
    fn scroll(&mut self, notches: i32) {
        let scroll = notches.clamp(-1, 1);
        let moved = self.offset as i64 - i64::from(scroll);
        self.offset = moved.clamp(0, self.max_offset() as i64) as usize;
    }

    /// The line committing the highlighted row produces —
    /// `Suggestion.apply(originalContents)`. See the struct doc for why this is
    /// computed from `original` rather than from the line on screen.
    fn applied(&self) -> String {
        let mut s = String::with_capacity(self.original.len() + 16);
        s.push_str(&self.original[..self.start]);
        if let Some(c) = self.candidates.get(self.current) {
            s.push_str(&c.text);
        }
        // Brigadier's `Suggestion.apply` splices over `[start, end)` and keeps
        // whatever follows. `end` is the caret at the moment the list was built,
        // so this is a plain append while the caret is at the end of the line
        // (the only case that used to exist) and a real splice otherwise.
        s.push_str(&self.original[self.end..]);
        s
    }

    /// Where the caret lands once [`Self::applied`] is committed —
    /// `SuggestionsList.useSuggestion`'s `setCursorPosition(end)`, in `char`s,
    /// i.e. just past the text this splices in rather than at the end of the
    /// whole line.
    fn applied_cursor(&self) -> usize {
        let inserted = self
            .candidates
            .get(self.current)
            .map_or(0, |c| c.text.chars().count());
        self.original[..self.start].chars().count() + inserted
    }

    /// The grey preview drawn after the caret — vanilla's
    /// `EditBox.setSuggestion(calculateSuggestionSuffix(input.getValue(),
    /// suggestion.apply(originalContents)))`, which is the applied line's tail
    /// past whatever is currently typed, or nothing when the applied line is not
    /// an extension of it (a candidate that *shortens* or rewrites the token).
    #[must_use]
    fn ghost(&self, line: &str) -> Option<String> {
        let applied = self.applied();
        applied
            .strip_prefix(line)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    /// The highlighted candidate.
    #[must_use]
    pub fn selected(&self) -> Option<&Candidate> {
        self.candidates.get(self.current)
    }

    /// Every candidate, in the order the rows draw.
    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// The highlighted row's index into [`Self::candidates`].
    #[must_use]
    pub fn current(&self) -> usize {
        self.current
    }

    /// The first visible row's index into [`Self::candidates`].
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Byte offset into the line the candidates replace from — the popup's own x
    /// anchor, `suggestions.getRange().getStart()`.
    #[must_use]
    pub fn start(&self) -> usize {
        self.start
    }
}

/// The chat box's Tab key: [`complete`] plus the state that makes pressing Tab
/// twice cycle rather than recompute, and the [`SuggestionRequests`] round trip
/// for a [`Completion::NeedsServer`] position.
///
/// Held **inside [`ChatInput`]** rather than beside it, mirroring vanilla,
/// where `ChatScreen` owns one `CommandSuggestions` bound to its one `EditBox`
/// (`ChatScreen.java`'s `commandSuggestions` field): the completion state is
/// only ever meaningful against one specific in-progress line, and separating
/// them is how the two drift apart.
///
/// # What is deliberately simpler than vanilla
///
/// Only the **usage box** is missing — vanilla's grey `commandUsage` lines
/// (`CommandSuggestions.extractUsage`), which restate a node's argument grammar
/// when there is no suggestion list to show. It needs Brigadier's
/// `getSmartUsage` over the whole reachable subtree, which is a second walker
/// rather than a draw. The list itself, its window, its navigation and its
/// tooltips are all here.
///
/// Dynamic, server-provided suggestions arrive through
/// [`SuggestionRequests`]; see this module's own doc for which positions need
/// them.
#[derive(Debug, Clone, Default)]
pub struct ChatCompletion {
    requests: SuggestionRequests,
    list: Option<SuggestionsList>,
    /// Vanilla's `allowSuggestions`. `false` from the moment the line is
    /// replaced wholesale — chat opened, or a history entry recalled — and set
    /// `true` again by the first real edit, exactly as `ChatScreen.onEdited`
    /// sets it and `ChatScreen.moveInHistory` clears it. Without it, opening
    /// chat with a seeded `/` would pop the whole command list before the player
    /// has typed anything.
    allow_suggestions: bool,
    /// The line as it was when [`SuggestionRequests::request`] was sent. The
    /// reply's `start` is a byte offset **into that text**, so applying it to a
    /// line that has since changed would splice at an offset that no longer
    /// means anything — the id check alone does not cover this, because a
    /// reply can be perfectly in-date for a line the player has already edited.
    pending_line: Option<String>,
}

impl ChatCompletion {
    /// No list, no request in flight.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget any list and any in-flight request, and stop offering suggestions
    /// until the next real edit — `setAllowSuggestions(false)` plus a cleared
    /// pending request. Called when the line is replaced wholesale (chat opened,
    /// a history entry recalled, or the line sent).
    pub fn reset(&mut self) {
        self.list = None;
        self.allow_suggestions = false;
        self.pending_line = None;
    }

    /// `CommandSuggestions.hide` — drop the list but leave suggestions allowed,
    /// so the next keystroke brings a fresh one back. This is what Escape does
    /// while the popup is up.
    pub fn hide(&mut self) {
        self.list = None;
    }

    /// The dropdown, when one is on screen. `None` is "no popup", which is what
    /// a draw and a mouse hit-test both key off.
    #[must_use]
    pub fn list(&self) -> Option<&SuggestionsList> {
        self.list.as_ref()
    }

    /// The candidates currently offered. Empty when there is no list — a draw
    /// can show this without asking again.
    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        self.list.as_ref().map_or(&[], |l| &l.candidates)
    }

    /// Which of [`Self::candidates`] is highlighted right now.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.list.as_ref().map(|l| l.current)
    }

    /// Whether a `command_suggestion` reply is still outstanding.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.requests.is_pending()
    }

    /// `CommandSuggestions.showSuggestions`: build the list and highlight its
    /// first row, **without touching the line**.
    ///
    /// The line is untouched on purpose and it is the whole difference between
    /// this and the pre-dropdown behaviour: vanilla shows the candidates and
    /// previews the highlighted one as grey ghost text, and only
    /// `useSuggestion` — Tab, or a click — actually edits. Browsing with the
    /// arrows therefore never rewrites what you typed.
    fn show(&mut self, start: usize, end: usize, line: &str, candidates: Vec<Candidate>) {
        if candidates.is_empty()
            || start > end
            || end > line.len()
            || !line.is_char_boundary(start)
            || !line.is_char_boundary(end)
        {
            return;
        }
        self.list = Some(SuggestionsList {
            start,
            end,
            original: line.to_owned(),
            candidates,
            offset: 0,
            current: 0,
            tab_cycles: false,
        });
    }
}

impl ChatInput {
    /// `ChatScreen.onEdited` → `CommandSuggestions.updateCommandInfo`: the line
    /// changed, so recompute the candidate set and show the popup.
    ///
    /// **Call this after every edit** — a typed character, a backspace, a paste.
    /// It is the seam that makes the dropdown appear while typing rather than
    /// only on Tab, which is the whole of what the player asked for, and it is
    /// deliberately a separate call rather than folded into
    /// [`ChatInput::push_char`]: those take no [`CommandTree`], and threading one
    /// into every edit method would put the tree lookup on the hot path of a
    /// paste. Vanilla splits it exactly here too, as the `EditBox` responder.
    ///
    /// Returns the same [`ClientAction`] [`Self::tab`] does, for a position only
    /// the server can answer. Vanilla re-requests on every keystroke as well
    /// (`getCompletionSuggestions` is called from `updateCommandInfo`), and
    /// [`SuggestionRequests`]' transaction id is what keeps a stale reply from
    /// landing on a newer line.
    pub fn update_command_info(&mut self, tree: Option<&CommandTree>) -> Option<ClientAction> {
        // `allowSuggestions = true` (`ChatScreen.onEdited`). The line the player
        // is *typing* always gets a popup; the line they opened the box with, or
        // recalled from history, does not until they touch it.
        self.completion.allow_suggestions = true;
        // `updateCommandInfo`'s own `this.suggestions = null` under
        // `!keepSuggestions`: the list about to be rebuilt describes the old
        // line, so it goes before anything can read it. `keepSuggestions` — the
        // flag that suppresses this during `useSuggestion` — has no analogue
        // here because `use_selected_suggestion` does not route its edit back
        // through this method.
        self.completion.list = None;
        self.recompute_suggestions(tree)
    }

    /// The shared body of [`Self::update_command_info`] and [`Self::tab`]'s
    /// show-the-list branch: pick the candidate source from the *line*, not from
    /// the key, and show what comes back.
    ///
    /// Vanilla forks here, and the fork is on the line:
    /// `CommandSuggestions.updateCommandInfo` computes `isCommand = commandsOnly
    /// || startsWithSlash`, and an ordinary chat line takes the `else if
    /// (!command.isBlank())` branch, which suggests online player names with no
    /// server round trip at all. So that path needs no command tree and works
    /// against any server — including one that has sent us no
    /// `minecraft:commands`.
    /// # It completes the line **up to the caret**, not the whole line
    ///
    /// `updateCommandInfo` reads `int cursorPosition = this.input.
    /// getCursorPosition()` and hands *that* to `getCompletionSuggestions`;
    /// the player-name branch is explicit about it —
    /// `command.substring(0, cursorPosition)` — and `sortSuggestions` ranks
    /// against the same prefix. While the caret sat permanently at the end of
    /// the line the two were the same string, which is why this used to read
    /// the whole value; with a caret that moves they are not, and completing
    /// the whole line would offer candidates for a word the player is not in.
    fn recompute_suggestions(&mut self, tree: Option<&CommandTree>) -> Option<ClientAction> {
        if !self.completion.allow_suggestions {
            return None;
        }
        let end = self.cursor_byte();
        let partial = self.partial().to_owned();
        if !partial.starts_with('/') {
            if partial.trim().is_empty() {
                return None;
            }
            let start = last_word_index(&partial);
            let candidates = player_name_candidates(&self.online_players, &partial[start..]);
            let line = self.buf.value().to_owned();
            self.completion.show(start, end, &line, candidates);
            return None;
        }
        let tree = tree?;
        match complete(tree, &partial) {
            Completion::Local { start, candidates } => {
                let line = self.buf.value().to_owned();
                self.completion.show(start, end, &line, candidates);
                None
            }
            Completion::NeedsServer { .. } => {
                // The prefix, not the whole line: the reply's `start` is a byte
                // offset into whatever text we asked about, so asking about one
                // string and splicing into another is how an offset stops
                // meaning anything.
                self.completion.pending_line = Some(partial.clone());
                Some(self.completion.requests.request(&partial))
            }
            Completion::None => None,
        }
    }

    /// The Tab key — `CommandSuggestions.keyPressed`'s `isCycleFocus` path,
    /// which is two different behaviours depending on whether the popup is up.
    ///
    /// * **Popup up**: `SuggestionsList.keyPressed` commits the highlighted row
    ///   ([`Self::use_selected_suggestion`]), cycling first if the previous Tab
    ///   already committed one — see [`SuggestionsList`]'s doc on `tab_cycles`.
    ///   `shift` reverses the cycle (`event.hasShiftDown()`).
    /// * **Popup down**: `showSuggestions(true)` — build and show the list,
    ///   editing nothing. Reachable because `ChatScreen.init` calls
    ///   `setAllowHiding(false)`, which is what makes the outer
    ///   `allowHiding && !isVisible` early return false for the chat box.
    ///
    /// So against the running client, where the popup is already up from typing,
    /// the first Tab commits — the behaviour this key had before the dropdown
    /// existed. It is the `autoSuggestions`-off ordering (show, *then* commit on
    /// the next Tab) that is new, and that is vanilla's too.
    ///
    /// Returns a [`ClientAction`] the caller must send when the position can
    /// only be answered by the server (`None` otherwise, including when there is
    /// no tree yet — a server that has sent no `minecraft:commands`, or any
    /// point before login completes, offers nothing rather than an empty list).
    pub fn tab(&mut self, tree: Option<&CommandTree>, shift: bool) -> Option<ClientAction> {
        if let Some(list) = self.completion.list.as_mut() {
            if list.tab_cycles {
                list.cycle(if shift { -1 } else { 1 });
            }
            self.use_selected_suggestion();
            return None;
        }
        // `showSuggestions` is reached from the *key*, so it must work even
        // where an edit would not have offered anything yet: opening the box
        // with a seeded `/` leaves `allow_suggestions` false, and vanilla's Tab
        // still shows the list there.
        self.completion.allow_suggestions = true;
        self.recompute_suggestions(tree)
    }

    /// `SuggestionsList.useSuggestion`: splice the highlighted candidate into
    /// the line and arm the next Tab to cycle.
    ///
    /// The list survives the edit — vanilla's `keepSuggestions = true` around
    /// `setValue` is what stops the responder tearing it down, and here the same
    /// thing falls out of not calling [`Self::update_command_info`]. That is
    /// what makes a run of Tabs walk the candidates instead of re-deriving a
    /// one-element list from the text the previous Tab just wrote.
    pub fn use_selected_suggestion(&mut self) {
        let Some(list) = self.completion.list.as_mut() else {
            return;
        };
        let (applied, cursor) = (list.applied(), list.applied_cursor());
        list.tab_cycles = true;
        self.buf.set_value(applied);
        // `setValue` leaves the caret at the end of the whole line; vanilla
        // follows it with `setCursorPosition(end)`/`setHighlightPos(end)`, which
        // is only the same place when the completion was an append.
        self.buf.set_cursor_position(cursor);
        self.buf.set_highlight_pos(cursor);
    }

    /// Up in the popup — `SuggestionsList.keyPressed`'s `event.isUp()` arm:
    /// `cycle(-1)` and clear `tabCycles`, so the following Tab commits *this*
    /// row rather than the next one. Returns whether the key was consumed, which
    /// is how the caller knows not to fall through to the chat history.
    pub fn suggestion_up(&mut self) -> bool {
        match self.completion.list.as_mut() {
            Some(list) => {
                list.cycle(-1);
                list.tab_cycles = false;
                true
            }
            None => false,
        }
    }

    /// Down in the popup — the `event.isDown()` arm. See [`Self::suggestion_up`].
    pub fn suggestion_down(&mut self) -> bool {
        match self.completion.list.as_mut() {
            Some(list) => {
                list.cycle(1);
                list.tab_cycles = false;
                true
            }
            None => false,
        }
    }

    /// Escape in the popup — the `event.isEscape()` arm: hide the list and drop
    /// the ghost preview, consuming the key.
    ///
    /// Consuming it is the point. `CommandSuggestions.keyPressed` runs *before*
    /// `ChatScreen`'s own handling, so the first Escape closes the popup and
    /// only a second one closes the chat box.
    pub fn suggestion_escape(&mut self) -> bool {
        if self.completion.list.is_none() {
            return false;
        }
        self.completion.hide();
        true
    }

    /// The mouse wheel over the popup — `SuggestionsList.mouseScrolled`. The
    /// caller has already established the pointer is inside the rect.
    pub fn suggestion_scroll(&mut self, notches: i32) -> bool {
        match self.completion.list.as_mut() {
            Some(list) => {
                list.scroll(notches);
                true
            }
            None => false,
        }
    }

    /// Hovering row `index` — the `mouseMoved` half of
    /// `SuggestionsList.extractRenderState`, which calls `select(i + offset)`
    /// for the row under a pointer that has *moved* since the last frame.
    ///
    /// Deliberately not a cycle: `select` alone leaves `offset` untouched, so
    /// hovering never scrolls the window out from under the pointer.
    pub fn suggestion_hover(&mut self, index: usize) -> bool {
        match self.completion.list.as_mut() {
            Some(list) if index < list.candidates.len() => {
                list.select(index as isize);
                true
            }
            _ => false,
        }
    }

    /// Clicking row `index` — `SuggestionsList.mouseClicked`: select it, then
    /// commit it. The caller resolves the row from the rect the draw used.
    pub fn suggestion_click(&mut self, index: usize) -> bool {
        if !self.suggestion_hover(index) {
            return false;
        }
        self.use_selected_suggestion();
        true
    }

    /// The grey preview drawn after the caret — see [`SuggestionsList::ghost`].
    #[must_use]
    pub fn suggestion_ghost(&self) -> Option<String> {
        self.completion.list.as_ref()?.ghost(self.buf.value())
    }

    /// The dropdown to draw, when there is one.
    #[must_use]
    pub fn suggestion_list(&self) -> Option<&SuggestionsList> {
        self.completion.list()
    }

    /// Apply a `command_suggestion` reply. Returns `true` when it was applied
    /// — i.e. it answered the request currently in flight *and* the line it was
    /// asked about is still the line being typed.
    ///
    /// **`false` is the normal, expected answer for a reply that is out of
    /// date**, and the caller must treat it as "ignore", not as an error: this
    /// is safe to poll every frame off `net::CommandTreeCell::suggestions`,
    /// because the id match consumes the pending request, so the second poll of
    /// the same response is already stale.
    ///
    /// The list uses the **server's own `start`**, not the local walker's: the
    /// response's range is authoritative for where its texts belong (a correct
    /// list at the wrong offset overwrites the wrong span on screen), and a
    /// `start` outside the requested line is rejected rather than clamped.
    pub fn apply_suggestions(
        &mut self,
        response: &lodestone_model::command_tree::CommandSuggestionsResponse,
    ) -> bool {
        let Some(candidates) = self
            .completion
            .requests
            .receive(response.id, response.suggestions.clone())
        else {
            return false;
        };
        let Some(asked) = self.completion.pending_line.take() else {
            return false;
        };
        // Against the **prefix** we asked about, not the whole line: a caret in
        // the middle means the tail was never part of the question, and it can
        // change without invalidating the answer.
        if asked != self.partial() {
            return false;
        }
        let Ok(start) = usize::try_from(response.start) else {
            return false;
        };
        let end = self.cursor_byte();
        if start > end || !self.buf.value().is_char_boundary(start) {
            return false;
        }
        if candidates.is_empty() {
            return false;
        }
        let line = self.buf.value().to_owned();
        self.completion.show(start, end, &line, candidates);
        true
    }

    /// The candidates currently offered — for a draw, and for a test to
    /// assert the exact set rather than "some suggestions appeared".
    #[must_use]
    pub fn completion_candidates(&self) -> &[Candidate] {
        self.completion.candidates()
    }

    /// The completion state itself, for callers that need [`ChatCompletion::
    /// is_pending`] or the selected index.
    #[must_use]
    pub fn completion(&self) -> &ChatCompletion {
        &self.completion
    }

    /// The scrollback's scroll position — see [`ChatScroll`].
    #[must_use]
    pub fn scroll(&self) -> &ChatScroll {
        &self.scroll
    }

    /// Mutable access for a wheel event or the per-frame sync — see
    /// [`ChatScroll::scroll`]/[`ChatScroll::sync`].
    pub fn scroll_mut(&mut self) -> &mut ChatScroll {
        &mut self.scroll
    }
}

/// The chat scrollback's scroll position while the box is open — vanilla's
/// `ChatComponent.chatScrollbarPos`/`newMessageSinceScroll`.
///
/// # Where this differs from vanilla, and why
///
/// **Granularity is one *logical entry*, not one *wrapped visual row*.**
/// Vanilla scrolls through `trimmedMessages` — messages already split into
/// `FormattedCharSequence` lines by `GuiMessage.splitLines`, which needs real
/// font metrics. This module has none ([`highlight`]/[`complete`] already
/// return byte spans rather than pixel runs for the identical reason — see
/// this module's own doc). Entry granularity is the exact one-line-per-entry
/// case of vanilla's own scheme and is wrong only when a message wraps into
/// more than one visual row, in which case a scroll step can move by more or
/// fewer visual rows than vanilla's would. Named here rather than silently
/// shipped, matching this module's other documented simplifications.
///
/// **Adjustment on a new arrival is read-time, not push-time.** Vanilla's
/// `ChatComponent.addMessageToDisplayQueue` checks `isChatFocused()` and
/// compensates the instant a message is queued
/// (`if (chatting && chatScrollbarPos > 0) { newMessageSinceScroll = true;
/// scrollChat(1); }`). This crate's received log ([`lodestone_game::chat::
/// ChatLog`]) is deliberately version- and UI-free and holds no notion of
/// "is the chat box open" — so [`Self::sync`] is called once per frame
/// instead, with the box's current open/closed state and its full history,
/// and detects every arrival since the last call by finding where last
/// frame's newest line now sits. This is not an approximation of vanilla's
/// behaviour, it is the same arithmetic batched: `scrollChat`'s clamp is
/// linear in both the scroll position and the total, so applying `k`
/// single-line increments in sequence and applying one increment of `k` land
/// on the same clamped result whenever the box stayed open across the whole
/// gap (the only case vanilla's own per-push check ever fires for either).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatScroll {
    /// Vanilla's `chatScrollbarPos`: entries scrolled back from the newest.
    /// `0` is "at the bottom" (live).
    scrolled: usize,
    /// Vanilla's `newMessageSinceScroll`.
    new_message_since_scroll: bool,
    /// The full history, oldest-first, as of the last [`Self::sync`] —
    /// what a new frame's history is diffed against in [`new_arrivals`].
    last_seen: Vec<String>,
}

impl ChatScroll {
    /// Not scrolled, no history seen yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `ChatComponent.scrollChat`, transcribed exactly — including the order
    /// of its two clamps. The upper clamp (`total - rows_per_page`) can be
    /// negative when everything already fits on screen; vanilla does not
    /// guard against that with a saturating subtraction, it relies on the
    /// second (`<= 0`) clamp to catch the resulting negative `pos` and pin it
    /// to `0` — reproduced with `i64` arithmetic for the same reason.
    pub fn scroll(&mut self, dir: i32, total: usize, rows_per_page: usize) {
        let mut pos = self.scrolled as i64 + i64::from(dir);
        let max = total as i64 - rows_per_page as i64;
        if pos > max {
            pos = max;
        }
        if pos <= 0 {
            pos = 0;
            self.new_message_since_scroll = false;
        }
        self.scrolled = pos as usize;
    }

    /// `ChatScreen.removed`'s `resetChatScroll()` — everything back to the
    /// live position. [`Self::sync`] calls this itself whenever the box is
    /// closed, so a caller need not hook every place the box can close
    /// separately (see [`Self::sync`]'s own doc).
    pub fn reset(&mut self) {
        self.scrolled = 0;
        self.new_message_since_scroll = false;
        self.last_seen.clear();
    }

    /// Call once per frame with whether the chat box is open and the
    /// **full** received history, oldest-first (the same order
    /// [`lodestone_game::chat::ChatLog::recent`] already returns).
    ///
    /// Closed resets outright — matching vanilla's `removed()` without
    /// needing a separate hook into every path that can close the screen
    /// (Escape, sending with `closeOnSubmit`, a disconnect): scroll position
    /// is meaningless while there is no box to scroll, so collapsing "closed"
    /// to "reset" is exact, not an approximation.
    ///
    /// Open, this reproduces `addMessageToDisplayQueue`'s no-jump
    /// compensation for every entry that arrived since the last call — see
    /// this type's own doc for why per-frame batching is exact here.
    pub fn sync(&mut self, chat_open: bool, history: &[String], rows_per_page: usize) {
        if !chat_open {
            self.reset();
            return;
        }
        let arrived = new_arrivals(&self.last_seen, history);
        self.last_seen = history.to_vec();
        if self.scrolled > 0 && arrived > 0 {
            self.new_message_since_scroll = true;
            self.scroll(
                i32::try_from(arrived).unwrap_or(i32::MAX),
                history.len(),
                rows_per_page,
            );
        }
    }

    /// Entries scrolled back from the newest. `0` is live.
    #[must_use]
    pub fn scrolled(&self) -> usize {
        self.scrolled
    }

    /// Whether anything is scrolled back from the live position.
    #[must_use]
    pub fn is_scrolled(&self) -> bool {
        self.scrolled > 0
    }

    /// Vanilla's `newMessageSinceScroll` — a message arrived while scrolled
    /// back, so the scrollbar should read as "new" (vanilla tints it
    /// differently; see [`crate::hud::ChatScrollbar`]).
    #[must_use]
    pub fn new_message_since_scroll(&self) -> bool {
        self.new_message_since_scroll
    }

    /// The visible window of `history` (oldest-first) at the current scroll
    /// position: up to `rows_per_page` entries, ending `self.scrolled`
    /// entries back from the newest. Shorter than `rows_per_page` only when
    /// the history itself is shorter.
    ///
    /// Generic over the element type (rather than fixed to `&[String]`) so a
    /// caller holding the paired `(String, age)` rows
    /// [`lodestone_game::chat::ChatLog::recent`] returns can window those
    /// directly, with [`Self::sync`]'s own `&[String]` (identity-only, no
    /// age) staying a separate, narrower view of the same history.
    #[must_use]
    pub fn window<'a, T>(&self, history: &'a [T], rows_per_page: usize) -> &'a [T] {
        let (start, end) = self.window_range(history.len(), rows_per_page);
        &history[start..end]
    }

    /// The `[start, end)` byte-index-free (entry-index) range [`Self::window`]
    /// slices with, exposed separately so a caller windowing two parallel
    /// slices (e.g. legacy strings and ages fetched together) can compute it
    /// once and apply it twice rather than trusting two `window` calls to
    /// agree.
    #[must_use]
    pub fn window_range(&self, total: usize, rows_per_page: usize) -> (usize, usize) {
        let end = total.saturating_sub(self.scrolled);
        let start = end.saturating_sub(rows_per_page);
        (start, end)
    }
}

/// How many entries are new in `current` (oldest-first) relative to
/// `previous` (also oldest-first, a prior frame's full history) — the count
/// appended at the newest end, accounting for the feed's own capacity
/// eviction (which drops the oldest and shifts every remaining index without
/// being a "new arrival"). `0` when nothing changed or `previous` is empty
/// (the first frame after opening).
///
/// Finds `previous`'s last (newest) entry scanning `current` from its own
/// newest end — the common case (zero or a handful of new lines) finds it in
/// the first few steps. Two entries sharing identical text is the one case
/// this can misjudge (it finds the *nearest* match to the newest end, which
/// undercounts only if a duplicate of the true previous-newest line was
/// itself one of the new arrivals); vanilla's own push-time trigger has no
/// such ambiguity, so this is a named, accepted approximation of the
/// read-time reconstruction, not a hidden one.
fn new_arrivals(previous: &[String], current: &[String]) -> usize {
    let Some(last) = previous.last() else {
        return 0;
    };
    match current.iter().rev().position(|s| s == last) {
        Some(idx_from_end) => idx_from_end,
        // `last` fell out of even a full-capacity history: more entries
        // arrived than the feed can hold at once. Report what we can see
        // rather than nothing.
        None => current.len().saturating_sub(previous.len()).max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an oldest-first `Vec<String>` of `n` distinct lines, so a
    /// mismatch prints exactly which line landed where instead of an opaque
    /// index.
    fn history(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    mod chat_scroll {
        use super::*;

        /// Both ends of `ChatComponent.scrollChat`'s clamp: cannot scroll past
        /// the newest (below `0`) and cannot scroll further back than the
        /// oldest entry that would still fill a page.
        #[test]
        fn scroll_clamps_at_both_ends() {
            let mut s = ChatScroll::new();
            // total = 10, rows_per_page = 4 -> the oldest reachable position
            // is `10 - 4 = 6`.
            s.scroll(3, 10, 4);
            assert_eq!(s.scrolled(), 3, "3 is within range, no clamp yet");
            s.scroll(100, 10, 4);
            assert_eq!(s.scrolled(), 6, "clamped at the top: total - rows_per_page");
            s.scroll(-100, 10, 4);
            assert_eq!(s.scrolled(), 0, "clamped at the bottom: never negative");
            assert!(!s.is_scrolled());
        }

        /// When everything already fits on screen (`total <= rows_per_page`),
        /// vanilla's own upper clamp goes negative and the lower clamp is what
        /// actually pins it — scrolling must land at `0` either way, not at
        /// the negative intermediate value.
        #[test]
        fn scroll_with_nothing_to_scroll_into_stays_at_zero() {
            let mut s = ChatScroll::new();
            s.scroll(5, 3, 10);
            assert_eq!(s.scrolled(), 0);
        }

        /// The core "no silent jump" behaviour: with the box open, a new
        /// arrival while scrolled back increments the scroll position so the
        /// *same* messages stay on screen — the view does not snap to the
        /// bottom just because something new came in.
        #[test]
        fn a_new_message_while_scrolled_and_open_keeps_the_view_steady() {
            let mut s = ChatScroll::new();
            let before = history(20);
            s.sync(true, &before, 5);
            s.scroll(4, 20, 5); // scroll back into history
            assert_eq!(s.scrolled(), 4);
            // Captured *before* the arrival, at the position that was
            // current then — this, not a re-derived window, is "what the
            // player was looking at".
            let visible_before: Vec<String> = s.window(&before, 5).to_vec();
            assert_eq!(
                visible_before,
                ["line 11", "line 12", "line 13", "line 14", "line 15"]
            );

            // One more line arrives at the newest end.
            let mut after = before.clone();
            after.push("line 20".to_string());
            s.sync(true, &after, 5);

            assert_eq!(
                s.scrolled(),
                5,
                "scrolled position must grow by exactly the 1 new arrival, \
                 so the visible window does not move"
            );
            assert!(s.new_message_since_scroll());
            assert_eq!(
                s.window(&after, 5),
                visible_before.as_slice(),
                "the same five lines must still be on screen after the arrival"
            );
        }

        /// The sibling clause a naive implementation drops: while **not**
        /// scrolled (at the live bottom), a new arrival must do nothing extra
        /// — the view is already following the newest message, and bumping
        /// `scrolled` here would scroll it *away* from live.
        #[test]
        fn a_new_message_while_not_scrolled_does_not_move_anything() {
            let mut s = ChatScroll::new();
            let before = history(10);
            s.sync(true, &before, 5);
            assert_eq!(s.scrolled(), 0);

            let mut after = before.clone();
            after.push("line 10".to_string());
            s.sync(true, &after, 5);

            assert_eq!(s.scrolled(), 0, "still live, not scrolled by an arrival");
            assert!(!s.new_message_since_scroll());
        }

        /// A message arriving while the box is **closed** must not leave a
        /// stale scroll position or a stale `new_message_since_scroll` for
        /// the next time it opens — `sync(false, ..)` resets outright.
        #[test]
        fn closing_resets_scroll_even_with_a_message_in_flight() {
            let mut s = ChatScroll::new();
            let before = history(20);
            s.sync(true, &before, 5);
            s.scroll(4, 20, 5);
            assert!(s.is_scrolled());

            s.sync(false, &before, 5); // box closed
            assert_eq!(s.scrolled(), 0);
            assert!(!s.new_message_since_scroll());

            // Reopening with more history must not resurrect the old
            // position or spuriously flag a "new message" from the gap.
            let mut after = before.clone();
            after.push("line 20".to_string());
            s.sync(true, &after, 5);
            assert_eq!(s.scrolled(), 0);
            assert!(!s.new_message_since_scroll());
        }

        /// [`ChatScroll::window`] returns exactly `rows_per_page` entries
        /// ending `scrolled` back from the newest, and the unscrolled case
        /// (`scrolled == 0`) is the plain "last `rows_per_page` entries" read
        /// — the same thing [`lodestone_game::chat::ChatLog::recent`] returns
        /// with no scroll applied.
        #[test]
        fn window_selects_the_right_slice() {
            let h = history(10);
            let mut s = ChatScroll::new();
            assert_eq!(
                s.window(&h, 3),
                &["line 7", "line 8", "line 9"],
                "unscrolled: the newest 3"
            );
            s.scroll(2, 10, 3);
            assert_eq!(
                s.window(&h, 3),
                &["line 5", "line 6", "line 7"],
                "scrolled back 2: the window shifts by exactly 2"
            );
        }

        /// A history shorter than a page must not panic or short-read past
        /// what exists — [`ChatScroll::window`] saturates rather than
        /// indexing negative.
        #[test]
        fn window_with_short_history_returns_everything() {
            let h = history(2);
            let s = ChatScroll::new();
            assert_eq!(s.window(&h, 10), &["line 0", "line 1"]);
        }

        /// Explicit [`ChatScroll::reset`] (the direct `resetChatScroll()`
        /// equivalent, for a caller with its own close hook) clears both the
        /// position and the "new message" flag.
        #[test]
        fn reset_clears_position_and_flag() {
            let mut s = ChatScroll::new();
            let before = history(20);
            s.sync(true, &before, 5);
            s.scroll(4, 20, 5);
            let mut after = before.clone();
            after.push("line 20".to_string());
            s.sync(true, &after, 5);
            assert!(s.is_scrolled());
            assert!(s.new_message_since_scroll());

            s.reset();
            assert_eq!(s.scrolled(), 0);
            assert!(!s.new_message_since_scroll());
        }
    }

    /// Backspace goes through [`ChatInput::handle_key`] rather than a
    /// `backspace()` helper, because that helper had no production caller once
    /// the line became an `EditBox` — every real Backspace arrives as a key.
    #[test]
    fn input_edits_are_char_boundary_safe() {
        let backspace = KeyEvent::new(crate::menu::focus::KEY_BACKSPACE);
        let mut input = ChatInput::new();
        input.push_str("héllo"); // multi-byte é
        input.handle_key(backspace); // removes 'o'
        input.handle_key(backspace); // removes 'l'
        assert_eq!(input.as_str(), "hél");
        input.handle_key(backspace);
        input.handle_key(backspace); // removes é (2 bytes) as one char
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
    /// real `/give @s <item> <amount>`; that fix deleted it. What replaced it
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

    /// Command-tree tab completion and syntax highlighting.
    // -- Up/Down chat history (`ChatScreen.moveInHistory`) --------------

    /// A chat box seeded with `sent`, already "reopened" — i.e. through `take`,
    /// which is what rewinds the cursor to the live slot.
    fn with_history(sent: &[&str]) -> ChatInput {
        let mut input = ChatInput::new();
        for line in sent {
            input.record_sent(line);
        }
        let _ = input.take();
        input
    }

    /// Up from the live slot recalls the **newest** line, not the oldest.
    ///
    /// The inversion this pins is the whole reason `recentChat` is a deque with
    /// `addLast`: `moveInHistory(-1)` steps from `size()` to `size() - 1`, so the
    /// first Up must land on the last thing sent. A store that pushed newest-first
    /// would answer `"first"` here and look perfectly reasonable doing it.
    #[test]
    fn the_first_up_recalls_the_most_recent_line() {
        let mut input = with_history(&["first", "second", "third"]);
        assert!(input.history_up());
        assert_eq!(input.as_str(), "third");
        assert!(input.history_up());
        assert_eq!(input.as_str(), "second");
        assert!(input.history_up());
        assert_eq!(input.as_str(), "first");
    }

    /// Both ends **clamp**; neither wraps.
    ///
    /// `newPos = Mth.clamp(historyPos + dir, 0, max)` followed by
    /// `if (newPos != this.historyPos)`, so an arrow at the end is a no-op on the
    /// line — and the caller still consumes the key, which is why
    /// `handle_chat_history_key` ignores this return value.
    #[test]
    fn the_history_cursor_clamps_at_both_ends_rather_than_wrapping() {
        let mut input = with_history(&["only"]);
        assert!(input.history_up());
        assert_eq!(input.as_str(), "only");
        // Already at index 0: another Up changes nothing and reports so.
        assert!(!input.history_up());
        assert_eq!(input.as_str(), "only");
        // Back to the live slot, then Down again — also a no-op, not a wrap
        // round to "only".
        assert!(input.history_down());
        assert_eq!(input.as_str(), "");
        assert!(!input.history_down());
        assert_eq!(input.as_str(), "");
    }

    /// The part-typed line survives a look through the history.
    ///
    /// `historyBuffer` is stashed on the way out of the live slot and restored on
    /// the way back in. Without it, glancing at an earlier message would silently
    /// destroy whatever the player was in the middle of writing.
    #[test]
    fn a_part_typed_line_is_stashed_and_restored() {
        let mut input = with_history(&["/gamemode creative"]);
        input.push_str("half a thought");
        assert!(input.history_up());
        assert_eq!(input.as_str(), "/gamemode creative");
        assert!(input.history_down());
        assert_eq!(input.as_str(), "half a thought");
    }

    /// **Consecutive** duplicates collapse; a repeat with something in between
    /// does not.
    ///
    /// `if (!message.equals(this.recentChat.peekLast()))` compares against the
    /// last entry only — it is not a set, and reading it as one would lose the
    /// second `"hi"` below.
    #[test]
    fn only_consecutive_duplicates_collapse() {
        let mut input = ChatInput::new();
        input.record_sent("hi");
        input.record_sent("hi");
        assert_eq!(input.history().entries(), ["hi"]);
        input.record_sent("there");
        input.record_sent("hi");
        assert_eq!(input.history().entries(), ["hi", "there", "hi"]);
    }

    /// The store normalises before comparing, so two sends differing only in
    /// spacing are the same entry — `normalizeSpace(msg.trim())`.
    #[test]
    fn the_store_normalises_whitespace_before_recording() {
        let mut input = ChatInput::new();
        input.record_sent("  hello    world  ");
        assert_eq!(input.history().entries(), ["hello world"]);
        // Same normalised form, so the duplicate check fires.
        input.record_sent("hello world");
        assert_eq!(input.history().entries().len(), 1);
        // Whitespace-only never enters at all (`if (!msg.isEmpty())`).
        input.record_sent("   ");
        assert_eq!(input.history().entries().len(), 1);
    }

    /// The cap drops the **oldest**, and the predicted contents are exact.
    ///
    /// `RECENT_CHAT_MAX + 1` distinct lines named `m0..=m100` go in; the store
    /// must hold `RECENT_CHAT_MAX` of them, `m1` first and `m100` last. A cap that
    /// dropped the newest instead — or an off-by-one that kept 101 — is
    /// separable from this, which a length-only assertion would not be.
    #[test]
    fn the_store_caps_at_a_hundred_and_drops_the_oldest() {
        let mut input = ChatInput::new();
        for i in 0..=RECENT_CHAT_MAX {
            input.record_sent(&format!("m{i}"));
        }
        let entries = input.history().entries();
        assert_eq!(entries.len(), RECENT_CHAT_MAX);
        assert_eq!(entries[0], "m1");
        assert_eq!(entries[RECENT_CHAT_MAX - 1], format!("m{RECENT_CHAT_MAX}"));
    }

    /// Reopening the chat box rewinds the cursor to the live slot, so the next Up
    /// starts from the newest line again rather than continuing where the last
    /// session left off — `ChatScreen.init`'s `historyPos = getRecentChat().size()`.
    #[test]
    fn reopening_the_box_rewinds_the_history_cursor() {
        let mut input = with_history(&["a", "b"]);
        assert!(input.history_up());
        assert!(input.history_up());
        assert_eq!(input.as_str(), "a");
        // Send/cancel, i.e. the box closes and reopens.
        let _ = input.take();
        assert!(input.history_up());
        assert_eq!(input.as_str(), "b");
    }

    // -- Tab-completing player names in ordinary chat -------------------

    /// `getLastWordIndex` keeps the **end** of the final whitespace run.
    #[test]
    fn the_last_word_index_lands_past_a_whitespace_run() {
        assert_eq!(last_word_index(""), 0);
        assert_eq!(last_word_index("Ste"), 0);
        assert_eq!(last_word_index("hi Ste"), 3);
        // Three spaces: the index is past all of them, not past the first.
        assert_eq!(last_word_index("hi   Ste"), 5);
        // A trailing run leaves the cursor at end-of-string, so the partial word
        // is empty and every name matches.
        assert_eq!(last_word_index("hi "), 3);
    }

    /// `matchesSubStr` is "prefix of a splitter-delimited segment" — **not**
    /// `contains`.
    ///
    /// `Another` contains `the` and must still be rejected; `Notch_The_Second`
    /// matches because `the` begins a segment after `_`. Those two inputs are
    /// chosen because they are the pair on which `contains` and the real rule
    /// disagree; a test that only offered `Steve` for `Ste` passes under either.
    #[test]
    fn a_name_matches_only_at_a_splitter_boundary() {
        let names = vec!["Another".to_owned(), "Notch_The_Second".to_owned()];
        let candidates = player_name_candidates(&names, "the");
        let got: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(got, ["Notch_The_Second"]);
        // And the mid-segment substring really is rejected, so the rule above is
        // not vacuously satisfied by everything matching.
        assert!(player_name_candidates(&names, "otch").is_empty());
    }

    /// Prefix matches come first, alphabetically within each group.
    ///
    /// Two orderings compose (brigadier's case-insensitive sort, then
    /// `sortSuggestions`'s partition), and the expected list below is the only one
    /// consistent with both: `My_Steve` matched through the `_` so it sorts
    /// *first* alphabetically and still lands *last*.
    #[test]
    fn candidates_put_prefix_matches_ahead_of_splitter_matches() {
        let names = vec![
            "My_Steve".to_owned(),
            "steven".to_owned(),
            "Steve".to_owned(),
            "Alex".to_owned(),
        ];
        let candidates = player_name_candidates(&names, "ste");
        let got: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(got, ["Steve", "steven", "My_Steve"]);
    }

    /// Type `text` the way the app does — one character, then the responder,
    /// per keystroke. `app::menus::handle_chat_key` calls
    /// [`ChatInput::update_command_info`] after every edit, so a test that only
    /// calls `push_str` is driving a seam production does not use, and would
    /// measure Tab against a popup that is never up.
    fn typed(input: &mut ChatInput, tree: Option<&CommandTree>, text: &str) {
        for ch in text.chars() {
            input.push_char(ch);
            let _ = input.update_command_info(tree);
        }
    }

    /// Tab in an ordinary chat line completes the **last word only**, leaving
    /// everything before it untouched, and needs no command tree.
    ///
    /// **Re-derived, not inverted**: this used to press Tab against a line typed
    /// with a bare `push_str`, where no popup existed and Tab's job was to
    /// compute *and* splice. It now types through the responder, so the popup is
    /// already up and Tab is `useSuggestion` — vanilla's ordering. The asserted
    /// line is unchanged because both orderings commit candidate 0 on the first
    /// Tab; what changed is that the popup is now observable before it.
    #[test]
    fn tab_completes_a_player_name_in_a_plain_chat_line() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned(), "Alex".to_owned()]);
        typed(&mut input, None, "hey Ste");
        // The popup is up from typing alone — the point of the whole unit.
        assert_eq!(
            input.completion_candidates().iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["Steve"]
        );
        // …and previews the highlighted row as ghost text without editing.
        assert_eq!(input.suggestion_ghost().as_deref(), Some("ve"));
        assert_eq!(input.as_str(), "hey Ste");
        // `None`: no tree, and none needed — this is the local branch.
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "hey Steve");
    }

    /// A second Tab cycles rather than recomputing — `tabCycles`, armed by the
    /// first `useSuggestion`.
    #[test]
    fn a_second_tab_cycles_through_matching_names() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned(), "steven".to_owned()]);
        typed(&mut input, None, "Ste");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "Steve");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "steven");
        // …and wraps.
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "Steve");
        // Shift+Tab walks the other way, from row 0 back round to the last.
        assert!(input.tab(None, true).is_none());
        assert_eq!(input.as_str(), "steven");
    }

    /// An empty (or whitespace-only) line offers nothing, matching vanilla's
    /// `else if (!command.isBlank())` guard — the `else` arm sets
    /// `pendingSuggestions = null`, so Tab on an empty box does not dump the whole
    /// roster into the line.
    #[test]
    fn tab_on_a_blank_line_offers_nothing() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned()]);
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "");
        assert!(input.suggestion_list().is_none());
        typed(&mut input, None, "   ");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "   ");
        assert!(input.suggestion_list().is_none());
    }

    /// A **command** line still takes the command path: with no tree it offers
    /// nothing, and it must never fall back to player names — a `/` line whose
    /// last word happened to prefix a player's name would otherwise be rewritten
    /// into a name where the tree meant an argument.
    #[test]
    fn a_command_line_never_falls_back_to_player_names() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned()]);
        typed(&mut input, None, "/msg Ste");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "/msg Ste");
        assert!(input.completion_candidates().is_empty());
    }

    /// The popup does **not** open on a line the player has not edited —
    /// `allowSuggestions` is `false` until `ChatScreen.onEdited` sets it.
    ///
    /// Opening chat with the command key seeds a `/`, and vanilla shows nothing
    /// there. Without this the whole root command list would appear the instant
    /// the box opened.
    #[test]
    fn a_seeded_line_shows_nothing_until_it_is_edited() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned(), "steven".to_owned()]);
        input.set("Ste");
        // The responder has not run, so `allow_suggestions` is still false.
        assert!(input.suggestion_list().is_none());
        // A recalled history entry behaves the same way
        // (`moveInHistory`'s `setAllowSuggestions(false)`).
        input.record_sent("Ste");
        let _ = input.take();
        assert!(input.history_up());
        assert_eq!(input.as_str(), "Ste");
        assert!(input.suggestion_list().is_none());
        // …and the next edit brings it back.
        typed(&mut input, None, "v");
        assert_eq!(
            input.completion_candidates().iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["Steve", "steven"]
        );
    }

    /// Escape closes the **popup**, not the box, and only once.
    ///
    /// `CommandSuggestions.keyPressed` runs before `ChatScreen`'s own handling,
    /// so the caller must be told the key was consumed — otherwise one Escape
    /// both hides the list and cancels the message.
    #[test]
    fn escape_consumes_once_and_then_falls_through() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned()]);
        typed(&mut input, None, "Ste");
        assert!(input.suggestion_escape());
        assert!(input.suggestion_list().is_none());
        // Nothing left to hide: the second Escape is the caller's.
        assert!(!input.suggestion_escape());
    }

    /// The arrows move the highlight, leave the line alone, and re-aim Tab.
    ///
    /// The exact indices are the assertion, not "it moved": `tabCycles` is
    /// cleared by an arrow, so the Tab after Down must commit **row 1**, the row
    /// on screen. A `SuggestionsList` that cycled unconditionally on Tab would
    /// commit row 2 here and still look like it was working.
    #[test]
    fn the_arrows_browse_without_editing_and_tab_commits_what_is_highlighted() {
        let mut input = ChatInput::new();
        input.set_online_players(vec![
            "Steve".to_owned(),
            "steven".to_owned(),
            "Stephanie".to_owned(),
        ]);
        typed(&mut input, None, "Ste");
        let names: Vec<&str> = input
            .completion_candidates()
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(names, ["Stephanie", "Steve", "steven"]);
        assert_eq!(input.suggestion_list().map(SuggestionsList::current), Some(0));
        assert!(input.suggestion_down());
        assert_eq!(input.suggestion_list().map(SuggestionsList::current), Some(1));
        // Browsing edits nothing.
        assert_eq!(input.as_str(), "Ste");
        assert_eq!(input.suggestion_ghost().as_deref(), Some("ve"));
        // Up wraps past the top to the last row.
        assert!(input.suggestion_up());
        assert!(input.suggestion_up());
        assert_eq!(input.suggestion_list().map(SuggestionsList::current), Some(2));
        assert!(input.suggestion_down());
        assert_eq!(input.suggestion_list().map(SuggestionsList::current), Some(0));
        // Down once, then Tab: `steven` (row 2) is what a Tab that cycled
        // unconditionally would give.
        assert!(input.suggestion_down());
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "Steve");
    }

    /// A click commits the row clicked, wherever the highlight was.
    #[test]
    fn a_click_selects_and_commits_that_row() {
        let mut input = ChatInput::new();
        input.set_online_players(vec![
            "Steve".to_owned(),
            "steven".to_owned(),
            "Stephanie".to_owned(),
        ]);
        typed(&mut input, None, "Ste");
        assert!(input.suggestion_click(2));
        assert_eq!(input.as_str(), "steven");
        // Out of range is refused rather than clamped onto a neighbour.
        assert!(!input.suggestion_click(3));
    }

    /// The window is capped and scrolls; the visible slice is predicted exactly.
    ///
    /// **The input has to exceed [`SUGGESTION_LINE_LIMIT`]** or the cap, the
    /// scroll offset and the wrap are all unobservable — with ten or fewer
    /// candidates `offset` is pinned at `0` under every implementation, correct
    /// or not. Twelve names, so the last two are off-screen at rest.
    ///
    /// The two hypotheses this separates: `cycle`'s forward branch uses
    /// `current + lineStartOffset - suggestionLineLimit`, and the naive
    /// `current - suggestionLineLimit + 1` happens to agree with it — they are
    /// the same expression. What does *not* agree is a `cycle` that scrolls by
    /// one row on a wrap: stepping Up from row 0 must jump `offset` straight to
    /// `max_offset` (2), not to 11 clamped or to 1.
    #[test]
    fn the_visible_window_caps_at_ten_rows_and_scrolls_with_the_selection() {
        let names: Vec<String> = (0..12).map(|i| format!("Player{i:02}")).collect();
        let mut input = ChatInput::new();
        input.set_online_players(names);
        typed(&mut input, None, "Player");
        let list = input.suggestion_list().expect("popup");
        assert_eq!(list.candidates().len(), 12);
        assert_eq!(list.rows(), SUGGESTION_LINE_LIMIT);
        assert_eq!((list.current(), list.offset()), (0, 0));
        // Down nine times reaches the last visible row without scrolling: rows
        // 0..=9 are on screen, so row 9 is still inside the window.
        for _ in 0..9 {
            assert!(input.suggestion_down());
        }
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (9, 0));
        // The tenth Down is the first that must scroll — one row, to 1.
        assert!(input.suggestion_down());
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (10, 1));
        assert!(input.suggestion_down());
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (11, 2));
        // Wrapping past the end lands back at row 0 with the window rewound.
        assert!(input.suggestion_down());
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (0, 0));
        // And wrapping the other way jumps the window all the way down at once.
        assert!(input.suggestion_up());
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (11, 2));
    }

    /// The wheel moves the window and **not** the selection, and clamps at both
    /// ends — `mouseScrolled` clamps `scroll` to `-1..=1` and `offset` to
    /// `0..=max_offset`, and never touches `current`.
    ///
    /// A implementation that moved the selection too would pass a
    /// "scrolling changes what is on screen" assertion; the pinned `current` is
    /// what separates them.
    #[test]
    fn the_wheel_scrolls_the_window_without_moving_the_selection() {
        let names: Vec<String> = (0..12).map(|i| format!("Player{i:02}")).collect();
        let mut input = ChatInput::new();
        input.set_online_players(names);
        typed(&mut input, None, "Player");
        // A wheel notch *down* is a negative scroll in vanilla's sign
        // convention, which increases `offset`.
        assert!(input.suggestion_scroll(-1));
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (0, 1));
        // Several notches in one event are clamped to one row of movement.
        assert!(input.suggestion_scroll(-7));
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (0, 2));
        // …and 2 is the ceiling: 12 candidates, 10 rows.
        assert!(input.suggestion_scroll(-1));
        assert_eq!(input.suggestion_list().map(SuggestionsList::offset), Some(2));
        // Back up, clamping at 0.
        for _ in 0..4 {
            assert!(input.suggestion_scroll(1));
        }
        let list = input.suggestion_list().expect("popup");
        assert_eq!((list.current(), list.offset()), (0, 0));
    }

    /// Committing a candidate replaces the previous commit, not appends to it.
    ///
    /// This is the one `original` exists for. Every commit is
    /// `original[..start] + text`, so cycling `Player00 → Player01` rewrites the
    /// token; a list that recomputed from the line on screen would build
    /// `Player00Player01`, and with a *single*-candidate list the two are
    /// indistinguishable.
    #[test]
    fn cycling_replaces_the_previous_commit_rather_than_appending() {
        let mut input = ChatInput::new();
        input.set_online_players(vec![
            "Player00".to_owned(),
            "Player01".to_owned(),
            "Player02".to_owned(),
        ]);
        typed(&mut input, None, "hi Play");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "hi Player00");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "hi Player01");
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "hi Player02");
    }

    /// With the popup down, Tab **shows** it and edits nothing — vanilla's
    /// `autoSuggestions`-off ordering, reachable here because
    /// `ChatScreen.init` calls `setAllowHiding(false)`.
    #[test]
    fn tab_with_no_popup_shows_it_without_editing() {
        let mut input = ChatInput::new();
        input.set_online_players(vec!["Steve".to_owned(), "steven".to_owned()]);
        typed(&mut input, None, "Ste");
        assert!(input.suggestion_escape());
        assert!(input.suggestion_list().is_none());
        assert!(input.tab(None, false).is_none());
        // Shown, not committed.
        assert_eq!(input.as_str(), "Ste");
        assert_eq!(input.suggestion_list().map(SuggestionsList::current), Some(0));
        // The *next* Tab commits.
        assert!(input.tab(None, false).is_none());
        assert_eq!(input.as_str(), "Steve");
    }

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
        /// sourced from `GameType.java`'s declaration order
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
                tooltip: Some(Text::literal("all players")),
            }];
            let received = requests.receive(id, entries).expect("current id honoured");
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].text, "@a");
            assert_eq!(received[0].tooltip, Some(Text::literal("all players")));
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
