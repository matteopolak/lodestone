//! The command block edit screen: vanilla's
//! `AbstractCommandBlockEditScreen`/`CommandBlockEditScreen` —
//! `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/
//! {AbstractCommandBlockEditScreen,CommandBlockEditScreen}.java`.
//!
//! ## What it is
//!
//! A command-text field with tab-completion, a read-only "Previous Output"
//! line, a Track Output toggle, and three mode/conditional/redstone toggles for
//! the block variants (Impulse/Chain/Repeat × Conditional × Needs-Redstone).
//! `docs/command-block-screen.md` covers the wiring end to end.
//!
//! ## Geometry
//!
//! Every rect below is transcribed from the two `init` methods, not
//! invented — `AbstractCommandBlockEditScreen.java` for the shared
//! widgets and `CommandBlockEditScreen.java` for the block-specific row.
//! All of it is anchored on `this.width / 2` (`render::Origin::ScreenTop`)
//! except the Done/Cancel row, which is `this.height / 4 + 120 + 12`
//! (`render::Origin::CommandBlockFooter`).
//!
//! ## What is deliberately simplified, named rather than hidden
//!
//! - **The "Previous Output" field never takes keyboard focus here.** Vanilla
//!   adds it as a real, tabbable, uneditable `EditBox` — which lets a player
//!   select and copy its text, a feature that needs a clipboard seam this
//!   shell does not have (see `edit_box.rs`'s own doc on why Ctrl+C/X/V are
//!   declined). Drawing it as a real (never-focused) [`EditBox`] still gets the
//!   border/caret-less rendering right; only the pointless tab stop is missing.
//! - **Completion is an island until the command tree actually arrives.**
//!   [`highlight`]/[`complete`] below are a thin offset-adapter over
//!   `crate::chat::{highlight, complete}` — the walker `bb81776`/`f33f18f`
//!   landed for the chat box — rather than a second copy of that logic. But
//!   `crate::chat`'s walker only recognises a line that starts with `/`
//!   (`chat.rs`'s `parse_line`), which every real command-block command does
//!   **not** (`commandsOnly = true` in vanilla's own
//!   `CommandSuggestions` constructor, `AbstractCommandBlockEditScreen.java`,
//!   means the whole line is a command with no leading slash). The adapter
//!   prepends a synthetic `/`, calls the chat walker, then shifts every byte
//!   offset back by one and drops the synthetic slash's own span — see
//!   [`with_slash`]. And since **`COMMANDS`/`COMMAND_SUGGESTIONS` (ids 16/15)
//!   still have no decode arm** (`crates/protocol/**` is off-limits to this
//!   crate), no real server's tree ever reaches a live client — every caller
//!   in this shell passes `None`, and [`complete`]/[`highlight`] degrade
//!   honestly to no candidates / no colouring rather than pretending, and that
//!   gap is tracked as a follow-up.
//! - **Nothing yet opens this screen from a real interaction.** There is no
//!   command-block-entity NBT decode anywhere in this workspace (`grep -rn
//!   "CommandBlock" crates/lodestone-model crates/lodestone-server` finds
//!   nothing but this file and the protocol test that encodes the outbound
//!   packet), so there is no data to open it *with* yet either. See
//!   [`CommandBlockOpen`] for the shape a future right-click handler would
//!   construct, and that fix for the second, separate island this leaves.
//!
//! ## Dependencies
//!
//! [`super::edit_box`] for the command field; [`crate::chat`] for the
//! completion/highlight walker (called, not duplicated — see above);
//! `lodestone_model::{action::CommandBlockMode, BlockPos, ClientAction}` for
//! the outbound packet shape, `lodestone_model::command_tree::CommandTree` for
//! the (currently always-absent) server tree.

use lodestone_model::command_tree::CommandTree;
use lodestone_model::{BlockPos, ClientAction, CommandBlockMode};

use super::edit_box::EditBox;
use super::focus::KeyEvent;
use crate::chat::{self, Candidate, Completion, HighlightSpan};

/// `AbstractCommandBlockEditScreen`'s `commandEdit` width/height
/// (`:46`): `width/2 - 150, 50, 300, 20`.
pub const COMMAND_DX: f32 = -150.0;
/// See [`COMMAND_DX`].
pub const COMMAND_Y: f32 = 50.0;
/// See [`COMMAND_DX`].
pub const COMMAND_W: f32 = 300.0;
/// See [`COMMAND_DX`].
pub const COMMAND_H: f32 = 20.0;

/// `advMode.command`'s label position (`:156`): one pixel right of the field's
/// own left edge, `y = 40`.
pub const COMMAND_LABEL_DX: f32 = COMMAND_DX + 1.0;
/// See [`COMMAND_LABEL_DX`].
pub const COMMAND_LABEL_Y: f32 = 40.0;

/// `CommandBlockEditScreen.getPreviousY()` (`:28-30`): fixed `135` for the
/// block screen (the minecart variant's is `150` — not modelled here, see the
/// module doc).
pub const PREVIOUS_Y: f32 = 135.0;
/// `previousEdit`'s width (`:55`): `276`, narrower than [`COMMAND_W`] to leave
/// room for the Track Output toggle beside it.
pub const PREVIOUS_W: f32 = 276.0;
/// See [`PREVIOUS_W`].
pub const PREVIOUS_H: f32 = 20.0;
/// `previousEdit` shares [`COMMAND_DX`]'s x.
pub const PREVIOUS_DX: f32 = COMMAND_DX;

/// The "Previous Output" label's y (`extractRenderState`, `:158-162`):
/// `y = 75 + 5*9 + 1 + getPreviousY() - 135`, which for the block screen's
/// `getPreviousY() == 135` collapses to `75 + 46 = 121`, and the label itself
/// draws at `y + 4 == 125`.
pub const PREVIOUS_LABEL_Y: f32 = 125.0;
/// See [`PREVIOUS_LABEL_Y`]; shares [`COMMAND_LABEL_DX`]'s x.
pub const PREVIOUS_LABEL_DX: f32 = COMMAND_LABEL_DX;

/// `outputButton`'s rect (`:63`): `width/2 + 150 - 20, getPreviousY(), 20, 20`.
pub const OUTPUT_DX: f32 = 130.0;
/// See [`OUTPUT_DX`].
pub const OUTPUT_W: f32 = 20.0;
/// See [`OUTPUT_DX`].
pub const OUTPUT_H: f32 = 20.0;

/// The mode/conditional/autoexec row's shared y.
pub const EXTRA_ROW_Y: f32 = 165.0;
/// Each of the three extra-row buttons is `100` wide, `20` tall.
pub const EXTRA_ROW_W: f32 = 100.0;
/// See [`EXTRA_ROW_W`].
pub const EXTRA_ROW_H: f32 = 20.0;
/// `modeButton`'s x (`:50`): `width/2 - 50 - 100 - 4`.
pub const MODE_DX: f32 = -154.0;
/// `conditionalButton`'s x (`:55`): `width/2 - 50`.
pub const CONDITIONAL_DX: f32 = -50.0;
/// `autoexecButton`'s x (`:62`): `width/2 + 50 + 4`.
pub const AUTOEXEC_DX: f32 = 54.0;

/// Done/Cancel's shared width/height (`:71,74`): vanilla's standard `150x20`
/// button.
pub const FOOTER_W: f32 = 150.0;
/// See [`FOOTER_W`].
pub const FOOTER_H: f32 = 20.0;
/// `doneButton`'s x (`:71`): `width/2 - 4 - 150`.
pub const DONE_DX: f32 = -154.0;
/// `cancelButton`'s x (`:74`): `width/2 + 4`.
pub const CANCEL_DX: f32 = 4.0;

/// The title's y (`:155`): `SET_COMMAND_LABEL` centred at `width/2, 20`.
pub const TITLE_Y: f32 = 20.0;

/// `advMode.setCommand` (`en_us.json`).
pub const TITLE_TEXT: &str = "Set Console Command for Block";
/// `advMode.command`.
pub const COMMAND_LABEL_TEXT: &str = "Console Command";
/// `advMode.previousOutput`.
pub const PREVIOUS_LABEL_TEXT: &str = "Previous Output";

/// `advMode.mode.sequence` — `CommandBlockMode::Sequence`'s label.
pub const MODE_SEQUENCE_TEXT: &str = "Chain";
/// `advMode.mode.auto` — `CommandBlockMode::Auto`'s label.
pub const MODE_AUTO_TEXT: &str = "Repeat";
/// `advMode.mode.redstone` — `CommandBlockMode::Redstone`'s label (the
/// default, matching vanilla's `Mode mode = CommandBlockEntity.Mode.REDSTONE`
/// field initialiser, `CommandBlockEditScreen.java`).
pub const MODE_REDSTONE_TEXT: &str = "Impulse";

/// The mode label for `mode`, matching `CommandBlockEditScreen.addExtraControls`'s
/// `switch` (`:41-47`).
#[must_use]
pub fn mode_label(mode: CommandBlockMode) -> &'static str {
    match mode {
        CommandBlockMode::Sequence => MODE_SEQUENCE_TEXT,
        CommandBlockMode::Auto => MODE_AUTO_TEXT,
        CommandBlockMode::Redstone => MODE_REDSTONE_TEXT,
    }
}

/// `CommandBlockEntity.Mode.values()`'s declared order
/// (`CommandBlockEntity.java`: `SEQUENCE, AUTO, REDSTONE`), which is
/// the order `CycleButton` cycles through.
#[must_use]
pub fn next_mode(mode: CommandBlockMode) -> CommandBlockMode {
    match mode {
        CommandBlockMode::Sequence => CommandBlockMode::Auto,
        CommandBlockMode::Auto => CommandBlockMode::Redstone,
        CommandBlockMode::Redstone => CommandBlockMode::Sequence,
    }
}

/// `outputButton`'s label: `CycleButton.booleanBuilder(Component.literal("O"),
/// Component.literal("X"), trackOutput).displayOnlyValue()`
/// — `true` shows the *first*
/// argument (`CycleButton.booleanBuilder`'s own `b == TRUE ? trueText :
/// falseText`, `CycleButton.java`).
#[must_use]
pub fn track_output_label(track_output: bool) -> &'static str {
    if track_output { "O" } else { "X" }
}

/// `conditionalButton`'s label (`advMode.mode.conditional`/`advMode.mode.unconditional`,
/// `CommandBlockEditScreen.java`).
#[must_use]
pub fn conditional_label(conditional: bool) -> &'static str {
    if conditional { "Conditional" } else { "Unconditional" }
}

/// `autoexecButton`'s label (`advMode.mode.autoexec.bat`/`advMode.mode.redstoneTriggered`,
/// `CommandBlockEditScreen.java`). `true` is vanilla's `automatic` flag —
/// "Always Active", no redstone required; `false` is "Needs Redstone".
#[must_use]
pub fn automatic_label(automatic: bool) -> &'static str {
    if automatic { "Always Active" } else { "Needs Redstone" }
}

/// The seven interactive rows this screen draws, in the order
/// [`CommandBlockState::rows`] emits them — also the row index the mouse
/// hit-test reports, mirroring `nav::NAME_FIELD`/`ADDRESS_FIELD`'s "the id is
/// the row index" convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandBlockRow {
    /// The command text field. The screen's sole keyboard focus target — see
    /// the module doc on why "Previous Output" is not a second one.
    Command,
    /// `outputButton` — toggles [`CommandBlockState::track_output`].
    TrackOutput,
    /// `modeButton` — cycles [`CommandBlockState::mode`] via [`next_mode`].
    Mode,
    /// `conditionalButton` — toggles [`CommandBlockState::conditional`].
    Conditional,
    /// `autoexecButton` — toggles [`CommandBlockState::automatic`].
    Automatic,
    /// `doneButton` — sends [`CommandBlockState::to_action`] and closes.
    Done,
    /// `cancelButton` — closes without sending.
    Cancel,
}

/// [`CommandBlockRow`]'s declaration order, and the row indices
/// [`CommandBlockState::rows`] emits them at (`ROWS[i] as usize == i`, asserted
/// in this module's own tests).
pub const COMMAND_BLOCK_ROWS: [CommandBlockRow; 7] = [
    CommandBlockRow::Command,
    CommandBlockRow::TrackOutput,
    CommandBlockRow::Mode,
    CommandBlockRow::Conditional,
    CommandBlockRow::Automatic,
    CommandBlockRow::Done,
    CommandBlockRow::Cancel,
];

/// The extra, non-interactive row [`CommandBlockState::rows`] appends after
/// [`COMMAND_BLOCK_ROWS`] for the read-only "Previous Output" field — see the
/// module doc on why it carries no [`CommandBlockRow`] of its own.
pub const PREVIOUS_OUTPUT_ROW: usize = COMMAND_BLOCK_ROWS.len();

/// What opening this screen needs from the (currently nonexistent, see the
/// module doc) command-block-entity NBT a right-click would read.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandBlockOpen {
    /// The block's world position — `ServerboundSetCommandBlockPacket`'s
    /// target.
    pub pos: BlockPos,
    /// The command currently stored on the block, if any.
    pub command: String,
    /// Whether the block is currently tracking its output.
    pub track_output: bool,
    /// The block's last recorded output line, if [`Self::track_output`] and
    /// the block has run at least once. `None` draws `"-"`, matching
    /// vanilla's own `previousEdit.setValue("-")` default.
    pub previous_output: Option<String>,
    /// The block's current mode.
    pub mode: CommandBlockMode,
    /// Whether the block is conditional on the block behind it.
    pub conditional: bool,
    /// Whether the block runs automatically (`true`) or needs redstone
    /// (`false`).
    pub automatic: bool,
}

impl Default for CommandBlockOpen {
    /// A freshly placed command block: vanilla's own field initialisers
    /// (`CommandBlockEditScreen.java`, `BaseCommandBlock`'s defaults).
    fn default() -> Self {
        Self {
            pos: BlockPos::new(0, 0, 0),
            command: String::new(),
            track_output: false,
            previous_output: None,
            mode: CommandBlockMode::Redstone,
            conditional: false,
            automatic: false,
        }
    }
}

/// Prepends a synthetic `/` so `crate::chat`'s walker — which only recognises
/// a leading slash (`chat::parse_line`) — accepts a bare command-block line.
/// See the module doc's "What is deliberately simplified" section.
fn with_slash(line: &str) -> String {
    let mut s = String::with_capacity(line.len() + 1);
    s.push('/');
    s.push_str(line);
    s
}

/// [`chat::highlight`] over a slash-less line: shifts every span's offsets
/// back by one and drops the synthetic slash's own span (the one span whose
/// `start == 0`, which every real span in a slash-shifted line cannot be,
/// since `parse_line` always emits the slash span first at `{0,1}` and every
/// subsequent span starts at `>= 1`).
#[must_use]
pub fn highlight(tree: &CommandTree, line: &str) -> Vec<HighlightSpan> {
    chat::highlight(tree, &with_slash(line))
        .into_iter()
        .filter(|s| s.start > 0)
        .map(|s| HighlightSpan {
            start: s.start - 1,
            end: s.end - 1,
            kind: s.kind,
        })
        .collect()
}

/// [`chat::complete`] over a slash-less line, with the same offset shift as
/// [`highlight`]. Returns [`Completion::None`] whenever `tree` is absent —
/// the honest degrade the module doc names, not a special case here.
#[must_use]
pub fn complete(tree: Option<&CommandTree>, line: &str) -> Completion {
    let Some(tree) = tree else {
        return Completion::None;
    };
    match chat::complete(tree, &with_slash(line)) {
        Completion::Local { start, candidates } => Completion::Local {
            start: start.saturating_sub(1),
            candidates,
        },
        Completion::NeedsServer { start } => Completion::NeedsServer {
            start: start.saturating_sub(1),
        },
        Completion::None => Completion::None,
    }
}

/// The screen's live state: the command field, the block's toggles, and
/// whatever [`complete`] currently has to offer.
///
/// Owns exactly one focusable widget ([`Self::command`]) — see the module doc
/// on why "Previous Output" is rendered as a real, never-focused [`EditBox`]
/// (held as a plain `String` here; [`super::render::command_block_frame`]
/// wraps it for the draw) rather than a second focus target.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandBlockState {
    /// The target block. Carried so [`Self::to_action`] can name it without
    /// this struct needing a second "which block" parameter threaded through
    /// every caller.
    pub pos: BlockPos,
    /// The command text field. Always focused — this screen has exactly one
    /// keyboard focus target, so there is no focus set to arbitrate.
    pub command: EditBox,
    /// The read-only previous-output line, `None` drawing `"-"` (see
    /// [`CommandBlockOpen::previous_output`]).
    pub previous_output: Option<String>,
    /// `outputButton`'s value.
    pub track_output: bool,
    /// `modeButton`'s value.
    pub mode: CommandBlockMode,
    /// `conditionalButton`'s value.
    pub conditional: bool,
    /// `autoexecButton`'s value.
    pub automatic: bool,
    /// Which row the mouse is over, if any — for hover highlighting. Separate
    /// from keyboard focus, matching every other button-row screen in this
    /// shell (`EditForm::hovered`, `MenuNav::pause`/`death`'s own cursor).
    pub hovered: Option<usize>,
}

impl CommandBlockState {
    /// Builds the screen's state from what a (currently nonexistent, see the
    /// module doc) right-click handler would read off the block entity.
    #[must_use]
    pub fn new(open: CommandBlockOpen) -> Self {
        let mut command = EditBox::new(0.0, 0.0, COMMAND_W, COMMAND_H, "Console Command");
        // `EditBox.setMaxLength(32500)`.
        command.set_max_length(32_500);
        command.set_value(&open.command);
        command.widget.focused = true;
        Self {
            pos: open.pos,
            command,
            previous_output: open.previous_output,
            track_output: open.track_output,
            mode: open.mode,
            conditional: open.conditional,
            automatic: open.automatic,
            hovered: None,
        }
    }

    /// The previous-output line's displayed text: `"-"` when nothing has been
    /// recorded, matching vanilla's default.
    #[must_use]
    pub fn previous_output_text(&self) -> &str {
        self.previous_output.as_deref().unwrap_or("-")
    }

    /// One printable character into the command field.
    pub fn handle_char(&mut self, ch: char) -> bool {
        self.command.handle_char(ch)
    }

    /// One non-printable key into the command field (arrows, Home/End,
    /// Backspace/Delete, select-all — see [`EditBox::handle_key`]).
    pub fn handle_key(&mut self, event: KeyEvent) -> bool {
        self.command.handle_key(event)
    }

    /// `modeButton`'s click handler (`(button, value) -> this.mode = value`,
    /// `CommandBlockEditScreen.java`).
    pub fn cycle_mode(&mut self) {
        self.mode = next_mode(self.mode);
    }

    /// `conditionalButton`'s click handler.
    pub fn toggle_conditional(&mut self) {
        self.conditional = !self.conditional;
    }

    /// `autoexecButton`'s click handler.
    pub fn toggle_automatic(&mut self) {
        self.automatic = !self.automatic;
    }

    /// `outputButton`'s click handler
    /// (`(button, value) -> { commandBlock.setTrackOutput(value);
    /// this.updatePreviousOutput(value); }`, `:64-67`). Vanilla immediately
    /// reads the (possibly stale, until the next output) block's own
    /// `getLastOutput()` back; this shell has no live block behind the
    /// screen to re-read, so toggling off simply blanks the line — matching
    /// what `onDone` would do anyway (`commandBlock.setLastOutput(null)` when
    /// `!isTrackOutput()`, `:110-112`) and what a fresh, never-tracked block
    /// already shows.
    pub fn toggle_track_output(&mut self) {
        self.track_output = !self.track_output;
        if !self.track_output {
            self.previous_output = None;
        }
    }

    /// [`highlight`] against the command field's current text.
    #[must_use]
    pub fn highlight(&self, tree: &CommandTree) -> Vec<HighlightSpan> {
        highlight(tree, self.command.value())
    }

    /// [`complete`] against the command field's current text.
    #[must_use]
    pub fn completions(&self, tree: Option<&CommandTree>) -> Completion {
        complete(tree, self.command.value())
    }

    /// The Tab key (step 2): splice the top locally-computed
    /// candidate into the command field, replacing everything from the
    /// completion's own `start`. Returns whether the field changed.
    ///
    /// Vanilla's Tab *cycles the popup's selection* and commits on Enter
    /// (`CommandSuggestions.SuggestionsList.cycle`/`useSuggestion`); this
    /// commits the top candidate directly, because no popup **selection**
    /// state is modelled here — the popup rows `super::render::
    /// command_block_frame` builds are derived from [`Self::completions`]
    /// rather than held. Pressing Tab again is then idempotent rather than a
    /// cycle: the completed token now matches only itself, so the same text is
    /// spliced back. Named here rather than hidden, and the gap is what a
    /// selection index would close.
    ///
    /// A [`Completion::NeedsServer`] position does nothing at all on this
    /// screen: a `command_suggestion` round trip needs an outbound action, and
    /// [`super::nav::MenuNav`] is pure — it returns a [`super::nav::MenuAction`]
    /// and holds no client handle. The chat box, which does have one, takes
    /// that path (`crate::chat::ChatInput::tab`).
    pub fn apply_completion(&mut self, tree: Option<&CommandTree>) -> bool {
        let Completion::Local { start, candidates } = self.completions(tree) else {
            return false;
        };
        let Some(first) = candidates.first() else {
            return false;
        };
        let value = self.command.value();
        if start > value.len() || !value.is_char_boundary(start) {
            return false;
        }
        let mut next = value[..start].to_string();
        next.push_str(&first.text);
        if next == value {
            return false;
        }
        self.command.set_value(next);
        true
    }

    /// `populateAndSendPacket`: the
    /// outbound packet this screen exists to produce.
    #[must_use]
    pub fn to_action(&self) -> ClientAction {
        self.to_submit().into_action()
    }

    /// [`Self::to_action`]'s `Eq`-able intermediate — see [`CommandBlockSubmit`]'s
    /// own doc for why `nav::MenuAction` carries this rather than a
    /// [`ClientAction`] directly.
    #[must_use]
    pub fn to_submit(&self) -> CommandBlockSubmit {
        CommandBlockSubmit {
            pos: self.pos,
            command: self.command.value().to_string(),
            mode: self.mode,
            track_output: self.track_output,
            conditional: self.conditional,
            automatic: self.automatic,
        }
    }
}

/// The `Eq`-able subset of [`CommandBlockState`] that reaches
/// `super::nav::MenuAction::SetCommandBlock`. [`ClientAction`] itself cannot
/// derive `Eq` (a sibling variant carries a float), and `MenuAction` derives
/// `Eq` for every one of its other variants, so a `MenuAction` variant
/// holding a `ClientAction` outright would break that derive for the whole
/// enum. This struct carries exactly [`ClientAction::SetCommandBlock`]'s own
/// fields, every one of which — a [`BlockPos`], a `String`, a
/// [`CommandBlockMode`] and three `bool`s — is already `Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBlockSubmit {
    /// `ClientAction::SetCommandBlock`'s `pos` field.
    pub pos: BlockPos,
    /// `ClientAction::SetCommandBlock`'s `command` field.
    pub command: String,
    /// `ClientAction::SetCommandBlock`'s `mode` field.
    pub mode: CommandBlockMode,
    /// `ClientAction::SetCommandBlock`'s `track_output` field.
    pub track_output: bool,
    /// `ClientAction::SetCommandBlock`'s `conditional` field.
    pub conditional: bool,
    /// `ClientAction::SetCommandBlock`'s `automatic` field.
    pub automatic: bool,
}

impl CommandBlockSubmit {
    /// Rebuilds the [`ClientAction`] `app.rs` actually sends — the one step
    /// [`nav::MenuAction`](super::nav::MenuAction)'s `Eq` derive cannot cross
    /// itself; see this struct's own doc.
    #[must_use]
    pub fn into_action(self) -> ClientAction {
        ClientAction::SetCommandBlock {
            pos: self.pos,
            command: self.command,
            mode: self.mode,
            track_output: self.track_output,
            conditional: self.conditional,
            automatic: self.automatic,
        }
    }
}

/// Applies a chosen [`Candidate`] at `start`, the way vanilla's
/// `Suggestion::apply` splices a replacement into `originalContents`
/// (`CommandSuggestions.java`, transcribed rather than called — this
/// module has no Brigadier `Suggestion` object, only the byte range and text
/// [`complete`] already computed).
#[must_use]
pub fn apply_candidate(line: &str, start: usize, candidate: &Candidate) -> String {
    let mut out = String::with_capacity(start + candidate.text.len());
    out.push_str(&line[..start.min(line.len())]);
    out.push_str(&candidate.text);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::command_tree::{NodeKind, RawCommandNode};

    fn open() -> CommandBlockOpen {
        CommandBlockOpen::default()
    }

    #[test]
    fn row_declaration_order_is_the_row_index() {
        for (i, row) in COMMAND_BLOCK_ROWS.iter().enumerate() {
            assert_eq!(
                COMMAND_BLOCK_ROWS.iter().position(|r| r == row),
                Some(i),
                "row {row:?} must appear exactly once, at its own declared index"
            );
        }
        assert_eq!(PREVIOUS_OUTPUT_ROW, 7);
    }

    #[test]
    fn mode_cycles_in_command_block_entitys_declared_order() {
        // `Mode.values()`: SEQUENCE, AUTO, REDSTONE, wrapping — and the
        // screen's own default field initialiser is REDSTONE.
        let mut state = CommandBlockState::new(open());
        assert_eq!(state.mode, CommandBlockMode::Redstone, "premise: default");
        state.cycle_mode();
        assert_eq!(state.mode, CommandBlockMode::Sequence);
        state.cycle_mode();
        assert_eq!(state.mode, CommandBlockMode::Auto);
        state.cycle_mode();
        assert_eq!(state.mode, CommandBlockMode::Redstone, "wraps");

        assert_eq!(mode_label(CommandBlockMode::Sequence), "Chain");
        assert_eq!(mode_label(CommandBlockMode::Auto), "Repeat");
        assert_eq!(mode_label(CommandBlockMode::Redstone), "Impulse");
    }

    #[test]
    fn the_three_boolean_toggles_flip_independently() {
        let mut state = CommandBlockState::new(open());
        assert!(!state.conditional && !state.automatic && !state.track_output);
        state.toggle_conditional();
        state.toggle_automatic();
        state.toggle_track_output();
        assert!(state.conditional && state.automatic && state.track_output);
        assert_eq!(conditional_label(state.conditional), "Conditional");
        assert_eq!(automatic_label(state.automatic), "Always Active");
        assert_eq!(track_output_label(state.track_output), "O");
        state.toggle_conditional();
        assert_eq!(conditional_label(state.conditional), "Unconditional");

        // Turning tracking off blanks whatever output was recorded — see
        // `toggle_track_output`'s own doc on why.
        let mut tracked = CommandBlockState::new(CommandBlockOpen {
            track_output: true,
            previous_output: Some("Set block to stone".to_string()),
            ..open()
        });
        assert_eq!(tracked.previous_output_text(), "Set block to stone");
        tracked.toggle_track_output();
        assert_eq!(tracked.previous_output_text(), "-");
    }

    #[test]
    fn to_action_carries_every_field_the_wire_needs() {
        let mut state = CommandBlockState::new(CommandBlockOpen {
            pos: BlockPos::new(1, 2, 3),
            command: "say hi".to_string(),
            ..open()
        });
        state.conditional = true;
        state.automatic = true;
        state.track_output = true;
        state.mode = CommandBlockMode::Sequence;
        assert_eq!(
            state.to_action(),
            ClientAction::SetCommandBlock {
                pos: BlockPos::new(1, 2, 3),
                command: "say hi".to_string(),
                mode: CommandBlockMode::Sequence,
                track_output: true,
                conditional: true,
                automatic: true,
            }
        );
    }

    /// A tiny two-literal tree: `/gamemode` with children `creative`,
    /// `survival` — enough to exercise ordering without pulling in the real
    /// 26.2 command graph. Mirrors `chat.rs`'s own test fixtures (same shape,
    /// independently built here since this module may not reach into
    /// `chat`'s private `#[cfg(test)]` helpers).
    fn gamemode_tree() -> CommandTree {
        let nodes = vec![
            RawCommandNode {
                kind: NodeKind::Root,
                children: vec![1],
                executable: false,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "gamemode".to_string(),
                },
                children: vec![2, 3],
                executable: false,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "creative".to_string(),
                },
                children: vec![],
                executable: true,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "survival".to_string(),
                },
                children: vec![],
                executable: true,
                restricted: false,
                redirect: None,
            },
        ];
        CommandTree::new(nodes, 0).unwrap()
    }

    #[test]
    fn no_tree_degrades_honestly_to_no_completions() {
        // The island named in the module doc: until `COMMANDS`/
        // `COMMAND_SUGGESTIONS` decode, every real caller passes `None`, and
        // this must never fabricate a candidate for it.
        let mut state = CommandBlockState::new(open());
        state.command.set_value("gamemode c");
        assert_eq!(state.completions(None), Completion::None);
    }

    #[test]
    fn completion_offsets_are_shifted_back_past_the_synthetic_slash() {
        let tree = gamemode_tree();
        let mut state = CommandBlockState::new(open());
        state.command.set_value("gamemode c");
        let Completion::Local { start, candidates } = state.completions(Some(&tree)) else {
            panic!("expected a local completion");
        };
        // The partial word "c" starts at byte 9 of "gamemode c" (no leading
        // slash) — *not* 10, which is what an un-shifted adapter would report
        // (the byte offset inside "/gamemode c").
        assert_eq!(start, 9, "must be relative to the slash-less line");
        assert_eq!(
            candidates,
            vec![Candidate {
                text: "creative".to_string(),
                tooltip: None,
            }],
            "only \"creative\" starts with \"c\"; \"survival\" must be excluded, \
             not merely ranked after it"
        );
        // Splicing the candidate in must reproduce the intended value.
        assert_eq!(
            apply_candidate(state.command.value(), start, &candidates[0]),
            "gamemode creative"
        );
    }

    #[test]
    fn completion_ordering_is_alphabetical_not_declaration_order() {
        // The literal children above are declared `creative` then `survival`
        // — alphabetical order and declaration order agree here, so this
        // tree alone cannot distinguish the two hypotheses. Add a third,
        // `adventure`, which sorts *before* both but is declared *last*, so a
        // declaration-order bug (the rejected hypothesis) would report
        // `[creative, survival, adventure]` while the real, alphabetical
        // rule (`chat::complete`'s `sort_by` — Brigadier's own
        // `SuggestionsBuilder::build`) reports `[adventure, creative,
        // survival]`.
        let nodes = vec![
            RawCommandNode {
                kind: NodeKind::Root,
                children: vec![1],
                executable: false,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "gamemode".to_string(),
                },
                children: vec![2, 3, 4],
                executable: false,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "creative".to_string(),
                },
                children: vec![],
                executable: true,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "survival".to_string(),
                },
                children: vec![],
                executable: true,
                restricted: false,
                redirect: None,
            },
            RawCommandNode {
                kind: NodeKind::Literal {
                    name: "adventure".to_string(),
                },
                children: vec![],
                executable: true,
                restricted: false,
                redirect: None,
            },
        ];
        let tree = CommandTree::new(nodes, 0).unwrap();
        let mut state = CommandBlockState::new(open());
        state.command.set_value("gamemode ");
        let Completion::Local { candidates, .. } = state.completions(Some(&tree)) else {
            panic!("expected a local completion");
        };
        let texts: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["adventure", "creative", "survival"],
            "rejected hypothesis: declaration order would give \
             [creative, survival, adventure]"
        );
    }

    #[test]
    fn highlight_colours_the_literal_without_the_synthetic_slashs_span() {
        let tree = gamemode_tree();
        let mut state = CommandBlockState::new(open());
        state.command.set_value("gamemode creative");
        let spans = state.highlight(&tree);
        // No span may start at the slash-less line's position 0 by
        // coincidence of the adapter forgetting to drop it — the first real
        // token, "gamemode", starts at byte 0 here.
        assert_eq!(
            spans.first().map(|s| (s.start, s.end)),
            Some((0, 8)),
            "\"gamemode\" must occupy [0, 8), not be preceded by a spurious \
             zero-width slash span"
        );
        assert_eq!(
            spans.get(1).map(|s| (s.start, s.end)),
            Some((9, 17)),
            "\"creative\" starts right after the space at byte 9"
        );
    }
}
