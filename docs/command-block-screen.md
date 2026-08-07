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
  `Option<&CommandTree>`. This used to read *"every current caller passes
  `None`"*, which was true when written and is not now — `MenuNav::
  command_tree` carries the tree the server sent, and both the draw path and
  the hit-test path pass it. `None` is still the honest pre-login state.
- **The three homes an overlay screen needs.** See "#474" below before adding
  another one: `frame_for` answering `None` is correct for this screen and is
  *not* the whole story.

## Where this screen is wired in (issue #474)

`render::frame_for` deliberately has **no arm** for `Screen::CommandBlockEdit`,
because this is an overlay — the world keeps rendering behind it, matching
vanilla's `isInGameUi() == true`
(`AbstractCommandBlockEditScreen.java:123-126`). That `None` is right. What it
does *not* do is excuse the screen from the other three homes, and the screen
was missing all three at once:

| home | file | what its absence looks like |
|---|---|---|
| draw | `app/redraw.rs`'s overlay block | the screen opens and renders **nothing** |
| hit-test frame | `nav::on_screen_frame` | clicks return `None` before reaching a row |
| input routing | `nav::routes_menu_input`, read by three guards in `app/lifecycle.rs` | clicks and keys never reach the body at all |

The draw and hit-test halves share **one expression**,
`nav::command_block_overlay_frame`: `redraw.rs` draws what it returns and
`on_screen_frame` hit-tests the same value. A second construction of the same
geometry is a click landing on a row the draw put elsewhere, which no
screenshot can show.

`routes_menu_input` is a function rather than an expression for the same
reason. It was written out four times — the `CursorMoved` guard, the
`MouseInput` guard, `KeyGate::menu`, and a hand-copy inside the very test that
was supposed to police the set. That last copy is why
`nav::tests::every_mouse_routable_screen_has_a_frame_to_hit_test` passed
throughout: it compared two things `nav.rs` controls and could not see the
driver. It now calls the production function, and
`app/tests.rs::clicking_a_command_block_row_at_its_own_coordinates_activates_
that_row` drives `WindowApp`'s own hit-test with coordinates computed from
`AbstractCommandBlockEditScreen.java`'s arithmetic rather than from our frame.

Adding a fourth overlay screen means all three rows of that table plus a case
in the nav gate. `Screen::Container` is the counter-example worth knowing: it
is an overlay with clickable rows and it is deliberately **not** in
`routes_menu_input`, because it has its own `hit_test_with_scale` path.

## What is missing (tracked on [#436](https://github.com/matteopolak/lodestone/issues/436))

**The island this section used to list first is closed.** It read: *"No command
tree ever reaches this client. `COMMANDS` (id 16) / `COMMAND_SUGGESTIONS` (id
15) have no decode arm anywhere in `crates/protocol/**`, so
`complete`/`highlight` always run with `tree: None` in production."* True when
written; #470 added the decode, #471 routed it to the shell, and #474's draw
half passes `MenuNav::command_tree` into `render::command_block_frame`. The
suggestion popup on this screen is now fed by the real server's tree.

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

### `mode_for_state` takes a block-**state** id, and getting that wrong was invisible

Read the accessor's parameter, not its name. `lodestone_data::block_states` has
two lookups whose names read almost identically and whose id spaces are
unrelated orders:

| accessor | id space | size | order |
|---|---|---|---|
| `block_name(id)` | block **state** | 32,366 | grouped by block, alphabetical |
| `block_type_name(id)` | `minecraft:block` **registry** | 1,196 | registration |

`mode_for_state` was written against `block_type_name` while being handed the
state ids the store deals in, and the symptom was **both directions at once**:

- real command blocks are states 9968 / 14817 / 14829, all past the registry's
  1,196 entries, so they answered `None` and **the edit screen could not open
  in the game at all** — the feature was dead the day it landed;
- the three registry ids reused as state ids answered `Some` — 407 is
  `minecraft:cherry_leaves` (Redstone), 668/669 are `minecraft:note_block`
  (Auto/Sequence) — so **right-clicking leaves or a note block opened the
  command-block editor**.

The audit that signed this path off verified the *wiring*
(`try_use` ← `KeyOutcome::Use` ← `lifecycle.rs`, resolving
`Sim::targeted_command_block`) and the wiring was real. A connected wire
carrying a wrong value is a separate question, and no `cargo check` and no
connectedness run can see it.

**What kept it green** is the part worth remembering: every test gating the
path picked its subject with the same wrong accessor, so the gate agreed with
the bug — a mirror. It only surfaced because state 407 owns no block entity, so
the harness panicked on `block_entity_type(...).expect(...)` several lines
before it could assert anything about a mode. The gate now takes each command
block's *registry* id — the number the broken call was really indexing — and
requires it, read as a state id, to answer `None`; that direction is one no
positive assertion can reach. Both fixed and reverted states were run and
observed.

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
