# Chat input editing

## What it is

Ordinary text editing in the chat prompt: a caret you can move, a selection,
copy/cut/paste, word-wise motion and deletion, and Home/End. The line is
`crate::chat::ChatInput`, and since it became an `EditBox` the editing itself is
vanilla's `EditBox` port rather than anything written for chat.

This doc covers the **input** half of the chat box. `docs/chat.md` covers the
rendering half — the scrollback, the word wrap, the suggestion popup's geometry
and the caret draw.

## How it works

### The line is an `EditBox`, because vanilla's is

`ChatScreen.init` builds one — `setMaxLength(256)`, `setBordered(false)`,
`setCanLoseFocus(false)`, focused on init — and `ChatInput::new_box`
reproduces those four settings.

Before this, `ChatInput` held a bare `String` appended to at its end. There was
no caret, so there was nothing for Left/Right, Home/End, Shift-selection or a
word skip to move; select-all had nothing to select; copy and cut had nothing to
read; and paste was a bespoke arm in `handle_chat_key` that could only
*append*, because there was no selection for it to replace. `EditBox` already
implemented all of it, ported line for line from the jar, with its own tests —
the missing piece was never the behaviour, it was a **producer**: a way to turn
a `winit` key press into the `menu::focus::KeyEvent` that port speaks.

`app::input::text_key_event` is that producer, and
`WindowApp::handle_chat_key_parts` is where the line is routed through it.

### `EditBox`'s geometry means nothing here, on purpose

Vanilla's chat box is `(4, height - 12, width - 4, 12)`. The only thing those
numbers feed is `EditBox::display_pos` — the horizontal scroll of a box narrower
than its contents — plus `text_x`/`text_y`, and the chat HUD reads none of them:
it draws the whole line itself at its own anchor. So `new_box` sizes the widget
so the full 256-character budget always fits, which pins `display_pos` at `0`.
A narrower box would leave that field scrolled to a window nothing consults,
i.e. state that disagrees with the screen.

### The platform modifier is resolved once, and not here

`text_key_event` maps each `winit` modifier to the GLFW bit that literally means
it: Shift→`MOD_SHIFT`, Ctrl→`MOD_CONTROL`, Alt→`MOD_ALT`, Cmd→`MOD_SUPER`. The
Mac-versus-elsewhere split lives in `menu::focus::EDIT_SHORTCUT_MODIFIER`, which
is what `has_control_down_with_quirk`, `is_copy`, `is_cut`, `is_paste` and
`is_select_all` test.

Folding Cmd onto `MOD_CONTROL` in the translation is the tempting shortcut and
it is wrong in the direction nobody checks: it makes the shortcuts work on a
Mac, and *also* makes a Mac user's plain Ctrl+Left perform a word skip, which
vanilla treats as an ordinary one-character step. The gate that pins this
(`the_edit_modifier_is_cmd_on_a_mac_and_ctrl_elsewhere`) asserts both
directions, and asserts they are never both true.

`modifiers` comes from `WindowApp::modifiers`, tracked off
`WindowEvent::ModifiersChanged`. A `winit` key event carries no modifier state
of its own, so reading it off the press gives zero for every chord — which is
how the menus once shipped unable to tell Cmd+A from `a`.

### Word boundaries break on a space and nothing else

`EditBox::word_position_from` is `getWordPosition(dir, from, stripSpaces)`, and
the rule is not the one most editors use:

- **Punctuation is not a boundary.** From the end of `say hi:there`, one
  backward skip lands on index `4`, the start of `hi:there` — not on `7`, past
  the colon.
- **A run of spaces is stripped on the far side of the jump, and the word
  beyond it is crossed in the same press.** In `hi   there`, a backward skip
  from index `5` lands on `0`, not `2` (the run's near edge) and not `4`.
- **Trailing spaces are stripped first.** From the end of `"hi there   "`, one
  backward skip lands on `3`: the run goes and then `there` is crossed.
- Forwards with no further space runs to the end of the line — vanilla's
  `indexOf` returning `-1` becomes the length.

Each of those is a gate in `app::input::chat_editing`, and each fixture is
chosen so the naive answer is a *different* number; the assertion messages name
it. That matters more than usual here, because the wrong rule is not obviously
broken — it feels subtly off, which is the kind of thing nobody files.

### Suggestions complete the line up to the caret

`CommandSuggestions.updateCommandInfo` reads `input.getCursorPosition()` and
completes `value.substring(0, cursorPosition)`; `sortSuggestions` ranks against
the same prefix. While the caret was pinned to the end of the line the prefix
and the whole value were the same string, which is why `recompute_suggestions`
used to read the value. With a caret that moves they are not, and completing the
whole line offers candidates for a word the player is not standing in.

Two consequences carried through the rest of the popup:

- `SuggestionsList` gained an `end` field — the caret at the moment the list was
  built, which is where brigadier's `StringRange` ends. `applied()` splices
  `original[..start] + text + original[end..]` rather than truncating the tail,
  and `applied_cursor()` is `useSuggestion`'s `setCursorPosition(end)`: the
  caret lands just past what was spliced in, not at the end of the whole line.
- A `NeedsServer` request asks about the **prefix**, and `pending_line` records
  the prefix, because the reply's `start` is a byte offset into whatever text we
  asked about.

`SuggestionsList::ghost` is unchanged and is *deliberately* left literal:
vanilla's `calculateSuggestionSuffix` shows nothing when the applied line is not
an extension of what is typed, which is exactly what happens when the caret is
mid-line. The HUD draws the ghost at the end of the line, so that is also the
only place it could be correct.

## How to change it

- **Do not add an editing operation to `ChatInput`.** Add it to `EditBox`, where
  the vanilla port lives, and let `text_key_event` produce the key for it. A
  second implementation beside a working one is worse than none.
- **A new editing key needs an arm in `text_key_event`.** It returns `None` for
  anything no text field acts on, and `None` is the caller's signal to fall
  through to its own text-insertion path — so a key you forget silently types
  its letter instead.
- **`ChatInput::handle_key` returns "consumed" and "edited" separately** and the
  caller needs both. A consumed key that only moved the caret must not fall
  through to the text arm, and must not re-ask the server for suggestions
  either; that is `EditBox`'s own `onValueChange`-gated responder. Folding them
  into one bool gets one of the two wrong.
- **The other text fields share this translator too**, so a change to what a
  chord means changes both. They reach `EditBox` through the focus layer
  (`menu::focus::FocusTarget::key_pressed`) rather than through
  `ChatInput::handle_key`, and until the same day as this they had **no caret
  motion at all**: `menu_key_for` produced a `MenuKey`, whose vocabulary had
  no Left/Right/Home/End, so a sign, a book, a command block and the
  server-list name field got select-all/copy/cut/paste and no arrows while
  `EditBox` implemented all four and was unit-tested on them. `menu_key_for`
  now calls `text_key_event` for exactly those keys and wraps the result in
  `MenuKey::Edit`, which carries the whole `KeyEvent` — see that variant's own
  doc for why the modifiers cannot be abstracted away here as they are for
  every other menu key, and `app::input::menu_text_editing` for the gates.

### Selection highlighting and the caret draw

`HudFrame` now carries `chat_selection`, populated from `ChatInput::selection`
while the chat screen is open. The HUD converts the ordered character range to
prefix widths through the same `Builder::text_width` call used for glyphs, then
draws the blue selection rectangle after the input strip and before those
glyphs. Its endpoints are clamped to both the string and the input strip, so an
empty or stale range, or a line extending past the visible chat plate, cannot
emit an invalid or off-strip fill. This follows
`EditBox.extractWidgetRenderState`'s draw order. The colour stream has no
vanilla-equivalent glyph-inversion pipeline, so the rectangle remains behind
the white glyphs.

#### The caret follows the cursor, and switches shape

The caret used to be drawn as a trailing `_` at the width of the **whole** line,
with `HudFrame` carrying no cursor position at all — so Left/Right really did
move the insertion point while the indicator stayed pinned to the end. That was
two defects wearing one symptom, and both are fixed:

- `HudFrame::chat_cursor: Option<usize>` carries the caret as a `char` index,
  filled from `ChatInput::cursor_position` in `app::redraw`. `None` means "at the
  end", which is what every caller predating a movable caret meant and what
  `HudFrame::new` defaults to, so no existing frame changed.
- `cursor_x` is now vanilla's `cursorX`: `textX + width(text before the caret) + 1`,
  then one pixel back when `insert` holds. `EditBox.extractWidgetRenderState`
  reserves that pixel only when the visible slice is non-empty, so an empty line
  keeps its caret at the text origin. There is no `displayPos` term because the
  chat box is sized so the whole 256-character budget fits (see
  `ChatInput::new_box`), which pins it at `0`.
- The shape is chosen by vanilla's `insert = cursorPos < value.length() ||
  value.length() >= maxLength`. Both disjuncts are live now; the first used to be
  treated as permanently false. `insert` draws
  `TextCursorUtils.extractInsertCursor`'s bar — one pixel wide, spanning the glyph
  row plus a pixel either side, `fill(x, y - 1, x + 1, y + lineHeight, colour)`
  with `lineHeight` being the glyph height plus one. Otherwise it draws
  `extractAppendCursor`'s literal `_` character as text, which is why the caret at
  the end of a line still looks the way it always did.
- The suggestion ghost's `!insert` gate is therefore real rather than a
  constant-folded `true`, and the ghost still sits at `cursorX - 1`, drawn before
  the caret so the caret composites on top.

`hud::tests::chat_caret_follows_the_cursor_and_becomes_a_bar_mid_string` gates
both halves. Its fixture is `"abcd"` with the caret at 2 — deliberately neither
position 0 nor the end, the two inputs where the correct and the buggy answers
coincide — and each assertion carries the value the old code would have produced
alongside the right one. Neutering the two halves separately makes it fail on the
shape and on the x independently.

Everything else editable in this shell (search boxes, the anvil rename field, the
world-creation name fields, server addresses, the command block, sign and book
editors) goes through `menu::edit_box::EditBox` and `menu::render::draw`'s
`draw_edit_box`, which already carried the full rule. Only the chat HUD
hand-rolled its caret, and only it was wrong.

## Configuration

None of its own. The 256-character cap is `crate::chat::MAX_CHAT_LENGTH`, which
is `ChatScreen.init`'s `setMaxLength(256)`.

## Dependencies

- `crate::menu::edit_box::EditBox` — the editing behaviour, all of it.
- `crate::menu::focus` — `KeyEvent`, the GLFW key and modifier constants, and
  `EDIT_SHORTCUT_MODIFIER`.
- `crate::platform::clipboard` — `arboard` natively; on `wasm32` `get` returns
  an empty string (the browser clipboard read is async and permission-gated, so
  a synchronous read cannot be honoured) and `set` is a fire-and-forget
  `navigator.clipboard.writeText`. Neither traps, so paste degrades to inserting
  nothing rather than killing the tab.
