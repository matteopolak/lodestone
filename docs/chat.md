# Chat

## What it is

The chat box: the outbound input line, the received scrollback and the HUD draw for both, including in-line editing (caret, selection, history, word motion) and interactive text (links, hover tooltips, command suggestions). It is the client-side render and edit surface; composing an outbound line and the log live in `lodestone-game`.

## How it works

### Input line and caret

The input is a real `EditBox` (shared mechanics in [UI framework](ui-framework.md)), giving a caret, selection, word motion, Home/End and clipboard, all following the reference text-field behaviour. There is no leading `>` glyph.

The caret is a one-pixel bar when inserting mid-line (or at the length cap) and a drawn `_` when appending. Its position is measured from the width of the text before it, never the whole line or `{text}_` (a trailing underscore adds no width; measuring it was a shipped bug). It blinks on a fixed wall-clock interval, like other transient HUD render flags.

The grey autocomplete ghost (top suggestion) draws just before the caret: measured from the text before the caret alone, drawn in order text, ghost, caret (so the caret composites over the ghost's first glyph), and suppressed at the length cap or when the caret is not at the end.

### History and name completion

Up and Down recall sent lines, oldest to newest, capped and deduplicated against immediate repeats after whitespace normalisation. History belongs to the input component (it survives reopening chat), clamps at both ends, stashes and restores a partial line, and records only on a real send, not an Escape.

Tab completion for ordinary lines is local, from the online player list (no server round trip), against every online player, not just the tab-list overlay's filtered projection (see [HUD](hud.md)). Matching is a prefix of a segment delimited by `._/`, not a substring. Word boundaries break only on whitespace, never punctuation (from the end of `say hi:there`, one backward skip lands after `hi`). Suggestions complete the text up to the caret, and accepting one mid-line replaces only the completed word. Chat's Tab and the player-list overlay's Tab cannot steal from each other because whichever context is open handles the key.

### Command-tree traversal

`CommandTree::new` validates flat wire node positions before the shell sees the tree. Chat starts at `CommandTree::root_id`, advances through `CommandNodeId` values from `effective_children_from`, and looks nodes up with `node_for`, so a node position is never confused with a byte offset or packet id. A handle is a position, not tree ownership: keep it only for one synchronous walk and never across a tree replacement.

### Word wrap and layout

Received lines are greedily wrapped against the same font metrics the draw uses, breaking at a space or hard-breaking an overlong word, with formatting codes carried onto the continuation. A message's rows stack with the last row nearest the bottom.

The scrollback's bottom edge and the input box's vertical position are independent constants; deriving one from the other collapses the intended gap of about 26 logical pixels between the newest message and the input. Both sit at a 4 logical-pixel horizontal text inset, separate from the shared HUD margin, and it must stay in sync in four places: the draw, the background plate (which pads differently), the suggestion popup anchor and the hover/click hit-test region.

Scrolling moves by whole logical entries, clamps, does not snap back on arrival while scrolled up, and resets on close. The scrollbar appears only when history exceeds the window and reads the scroll logic's own state.

### Display options

Persisted options mirror the reference: scale (the whole factor, no extra HUD multiplier; see [HUD](hud.md)), width and height focused and unfocused, line spacing, text and background opacity, and legacy colour-code stripping. At defaults the draw is byte-identical to having no options. Message-visibility filtering (needs a source tag the log discards) and arrival rate limiting are not modelled.

### Interactive text

Click and hover events and shift-click insertion are inherited span properties, like colour, including across a legacy-code split. Hit-testing reuses the draw's wrap and layout functions, so geometry cannot go stale.

**Field names differ by protocol era, silently.** Modern protocols use snake case for the event fields, older ones camel case, and a click action's argument is named for the action (`url`, `command`, `path`, numeric `page`) rather than a single `value`. Both spellings and shapes are read, newest first; a mismatch yields a message with no interactivity and no error.

**Hover payloads are typed per action.** `show_text` is a component, `show_item` an item stack (id, count, component patch), `show_entity` a type, UUID and optional name. A single component field is a lossy decode: a compound has no `text` or `translate`, parsing as an empty node, so an item hover paints nothing. The oldest `show_item` form, a component of serialised item data, is the one exception (kept when there is no readable item id).

An item hover body is gathered by the same function an inventory slot tooltip uses (and honours advanced tooltips). An entity hover shows name, type (through the type's description key) and UUID. The box paints from the untextured colour stream, so there is no bundle grid, icon or nine-slice frame.

Shift-click is a mode: with shift, the span's insertion goes in at the caret and the click action does not run; without it, the reverse. A shift-click on a span with no insertion is inert. Insertion uses the caret-respecting insert, and the text filter and length cap still apply.

Every server-supplied "open URL" click goes through an untrusted-link confirmation, with no silent path. "Run command" sends as typed; "suggest command" only fills the input; "copy to clipboard" is supported and "open file" surfaces a local message without touching the filesystem. Server links and `open_url` values become `ServerLinkUrl` (over `url::Url`) at ingress; malformed values and non-web schemes (`javascript:`, `file:`) never enter the confirmation, and a confirmed value stays typed until the platform handoff calls `ServerLinkUrl::as_str`. Confirmation stays necessary since a valid URL is not a trustworthy one.

## How to change it

- Add editing operations to the shared `EditBox`, not to the chat input.
- A key producer must distinguish consumed from edited: a caret-only key must not insert text or trigger a suggestion request.
- Resolve the platform edit-shortcut modifier (Cmd on macOS, Ctrl elsewhere) once, centrally; never hardcode Ctrl, and never fold it onto the generic control bit (that would make Ctrl+arrow a word skip on macOS).
- A horizontal inset change goes in all four places.
- Derive selection and caret pixels from the same width measurement as the glyph draw, clamped to the string and visible strip.
- Keep wire indices at the adapter boundary; only an encoder or diagnostic calls `CommandNodeId::index`.

## Configuration

Display options persist with other non-default settings and fall back to reference defaults when unset. The input cap is a fixed 256 characters.

## Dependencies

- `crates/lodestone-shell/src/chat.rs` (input, history, completion, suggestions) and `hud.rs` (layout, wrap, scrollback, caret, selection).
- `lodestone-game::chat` (message log), `lodestone-model::text` (styled tree with event fields).
- `crate::menu::edit_box::EditBox`, `crate::menu::focus` (shared with every text field; see [UI framework](ui-framework.md)).
- `crate::platform::clipboard` (degrades to empty read and fire-and-forget write without a synchronous API).
