# Command block edit screen

## What it is

The command block edit screen (issue #47): vanilla's `CommandBlockEditScreen`
— an in-game overlay with a command text field, tab-completion, a read-only
"Previous Output" line, a Track Output toggle, and Mode/Conditional/Needs
Redstone toggles for the block variants.

## How it works

`Screen::CommandBlockEdit` (`menu.rs`) is an overlay screen, the same shape as
`Screen::Chat`/`Screen::Container`: the pointer is released and gameplay input
is frozen, but the world keeps rendering and ticking behind it, matching
vanilla's own `isInGameUi() == true`
(`AbstractCommandBlockEditScreen.java:123-126`).

All of the actual state — the command field (a real `EditBox`), the
mode/conditional/automatic/track-output flags, the target `BlockPos`, and the
previous-output text — lives in `menu/command_block.rs`'s `CommandBlockState`,
held by `MenuNav` the same way `EditForm` holds `Screen::ServerEdit`'s two
fields (`nav.rs`'s `command_block: Option<CommandBlockState>`). `UiState`
itself only tracks which screen is showing; opening/closing both the widget
state and the screen go through `MenuNav::open_command_block`/
`close_command_block`, which call the matching `UiState` methods in turn.

`render::command_block_frame` builds the `MenuFrame` — every row's rect is a
literal transcription of vanilla's `init()` methods (see the constants at the
top of `command_block.rs`, each citing a `.java:line`), using two new
`Origin` variants: `CommandBlockFooter` for the Done/Cancel row's
`height/4 + 132` y-anchor, and `CommandBlockSuggestion` for the tab-completion
popup's clamped x (the one rect on this screen that needs the canvas `width`
to resolve, since vanilla clamps it to the screen's absolute left/right edges,
not to an offset from the command box).

**Tab-completion reuses `chat.rs`'s walker rather than duplicating it.**
`chat::complete`/`chat::highlight` (landed in `bb81776`/`f33f18f` for the chat
box) only recognise a line starting with `/` — `chat::parse_line`'s own
invariant — but a command block's command text never has a leading slash
(`commandsOnly = true` in vanilla's `CommandSuggestions` constructor,
`AbstractCommandBlockEditScreen.java:76`). `command_block::highlight`/
`complete` prepend a synthetic `/`, call the chat walker, then shift every
byte offset back by one and drop the synthetic slash's own span. See
`command_block.rs`'s module doc for the full reasoning and
`with_slash`/`highlight`/`complete`'s own doc comments.

## How to change it

- **Geometry**: every rect is a named constant in `command_block.rs`
  (`COMMAND_DX`, `EXTRA_ROW_Y`, `DONE_DX`, …), each citing the vanilla source
  line it was transcribed from. Change the constant, not the call site in
  `render::command_block_frame`.
- **Row order / click routing**: `CommandBlockRow`'s declaration order in
  `command_block.rs` **is** the row index `nav.rs`'s `activate_command_block_row`
  and `render::command_block_frame` both key off — `COMMAND_BLOCK_ROWS.get(row)`
  and the frame builder's `.map()` over the same array. Adding a control means
  adding a variant there, an arm in `command_block_frame`'s `match`, and an arm
  in `activate_command_block_row`'s `match` — the same three-place coupling
  every other button-row screen in this shell has (see `nav.rs`'s own module
  doc).
- **Completion domain**: `command_block::complete`/`highlight` take
  `Option<&CommandTree>`. Every current caller passes `None` — see
  "What is missing" below.

## What is missing (tracked on [#436](https://github.com/matteopolak/lodestone/issues/436))

Two islands, named rather than hidden:

1. **No command tree ever reaches this client.** `COMMANDS` (id 16) /
   `COMMAND_SUGGESTIONS` (id 15) have no decode arm anywhere in
   `crates/protocol/**` (off-limits to this crate), so `complete`/`highlight`
   always run with `tree: None` in production and degrade to "no
   completions, no highlighting" rather than a fabricated list.
2. **Nothing opens this screen from a real interaction.** There is no
   command-block-entity NBT decode anywhere in this workspace and no
   right-click-detects-a-command-block path in `interact.rs`, so
   `UiState::open_command_block`/`MenuNav::open_command_block` have no
   producer. The screen, its layout, and its (tree-less) completion adapter
   are real and unit-tested regardless — see `command_block.rs`'s own tests
   for predicted rects and an exact completion ordering with a rejected
   hypothesis.

A third, smaller gap: `MenuAction` has no `SetCommandBlock` variant yet, and
`activate_command_block_row`'s Done arm computes `CommandBlockState::to_submit()`
but discards it (`let _submit = ...`) rather than sending it — landing the
variant now would break `app.rs`'s exhaustive `match action` with no
compiling counterpart there (a brokered file). `CommandBlockSubmit::into_action`
is ready for whichever agent adds both halves together.

## Configuration

None — no env var, flag, or config file affects this screen.

## Dependencies

`super::edit_box::EditBox` for the command/previous-output text fields;
`crate::chat::{complete, highlight, Completion, Candidate, HighlightSpan}` for
the completion/highlight walker (called, not duplicated);
`lodestone_model::{command_tree::CommandTree, action::CommandBlockMode,
BlockPos, ClientAction}` for the tree type and the outbound packet shape.
