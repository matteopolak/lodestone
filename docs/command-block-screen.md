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

**One island left, and it is not the one this section used to list first.**

1. **No command tree ever reaches this client.** `COMMANDS` (id 16) /
   `COMMAND_SUGGESTIONS` (id 15) have no decode arm anywhere in
   `crates/protocol/**` (off-limits to this crate), so `complete`/`highlight`
   always run with `tree: None` in production and degrade to "no
   completions, no highlighting" rather than a fabricated list. Re-measured
   for #436: the two ids appear **only** in `v770/src/generated/packet_ids.rs`
   — the id table, which proves the id is known and nothing more.

### Closed: the screen is now reachable from a real right-click

This section used to read: *"Nothing opens this screen from a real
interaction. There is no command-block-entity NBT decode anywhere in this
workspace and no right-click-detects-a-command-block path in `interact.rs`."*

**The first sentence of that was a wrong conclusion drawn from a true
observation**, and it is worth keeping as a worked example. There is indeed no
*typed* command-block decode in `lodestone-model` or `crates/protocol` — the
grep that established it was correct — but none was ever needed:
`lodestone_world::BlockEntity` already carries the server's **raw NBT** for
every block entity in a loaded chunk, and `lodestone_data::block_states`
already answers which block sits at a position. `SignText` had been reading
sign lines that same way the whole time. The estimate of "a substantially
bigger lift, since the data to open the screen *with* does not exist yet
either" was therefore wrong in the expensive direction: it argued for
deferring work that was reachable that day.

The reader is `crates/lodestone-shell/src/command_block_source.rs`, and the
split it enforces is vanilla's own:

| field | source | vanilla |
|---|---|---|
| mode | **block state** | `CommandBlockEntity.getMode()` matches on the three block ids |
| conditional | **block state** | `isConditional()` reads `CommandBlock.CONDITIONAL` |
| command, `TrackOutput`, `auto`, `LastOutput` | **NBT** | `BaseCommandBlock.save` / `saveAdditional` |

There is no mode field on the wire at all, so a reader that consulted only the
NBT would show every chain block as Redstone — a plausible-looking wrong
answer, and the reason `mode_for_state` exists as its own gated function.

The trigger is `WindowApp::try_use`, **not** `interact.rs`, and that is
deliberate. `drive_placement` returns `PlaceRejection::NothingPlaceableHeld`
before it ever looks at the clicked block, so a right-click with an empty hand
— the normal way to open a command block — never reaches its body. It is also
the faithful place: vanilla resolves this screen client-side with no packet
(`CommandBlock.useWithoutItem` → `LocalPlayer.openCommandBlock`).

Two behaviours worth preserving if you touch it:

- **No permission gate.** Vanilla guards on `canUseGameMasterBlocks()` (op
  level 2 **and** creative); this client tracks neither, and the server rejects
  an unauthorised `SetCommandBlock` regardless. Refusing to open on a *guessed*
  permission would be a dead control.
- **Fail open, never fail blank.** `Nbt::End` — what a block the server sent no
  data for carries, the common case for one just placed — opens an empty
  editor, which is exactly what vanilla shows. Note `TrackOutput` defaults to
  **`true`**, the one field here whose default is not `false`/empty.

## Configuration

None — no env var, flag, or config file affects this screen.

## Dependencies

`super::edit_box::EditBox` for the command/previous-output text fields;
`crate::chat::{complete, highlight, Completion, Candidate, HighlightSpan}` for
the completion/highlight walker (called, not duplicated);
`lodestone_model::{command_tree::CommandTree, action::CommandBlockMode,
BlockPos, ClientAction}` for the tree type and the outbound packet shape.
