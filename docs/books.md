# Books

## What it is

Reading and writing in-game books: the writable-book editor and its signing flow, and the read-only
screen for a signed book (plus a lectern's book display). All three share one texture, one word-wrap
model, and one overlay frame builder.

## How it works

### Writing and signing

The book-and-quill editor is one state machine with a `signing` flag rather than two separate screens,
matching vanilla's own edit/sign split. It introduced this shell's first **multi-line, word-wrapping**
text widget — every prior text field was single-line — which approximates real font-based word wrap
with a fixed character-count wrap width rather than measuring actual glyph widths, the same kind of
simplification the single-line edit box already makes for its own horizontal scrolling. This widget is
meant to be reused by any future multi-line text screen rather than reinvented.

Opening the editor is entirely client-side: holding a writable book and pressing use forks into the
screen immediately, with no server round trip, the same shape the command-block screen's own use-fork
already has. Saving a draft overwrites the book's page content in place; finalizing (only reachable
once the title is non-blank) transmutes the item into a signed book with the signer's name as author,
copy generation zero, and its content marked resolved. Escape always discards unconditionally from
either the draft or the signing layout, matching vanilla — neither of vanilla's own two screens saves on
close, only on an explicit affirmative action.

Deliberately out of scope, named rather than silently missing: per-pixel mouse caret placement inside a
page (a click in the page area is a no-op; keyboard focus already has nowhere else to go since neither
layout has a second focusable field), and opening this same screen for an *already-signed* book — that
is a distinct, read-only screen (below), not a mode of the editor.

### Reading a signed book

A signed book's title is its **item display name** (falling back from any explicit custom name to the
book's own title), not a tooltip line — which is why it also feeds the held-item name tooltip and every
other "hover name" consumer for free. The author line and copy-generation line ("Original", "Copy of
original", etc.) are a genuinely separate mechanism, a tooltip addition, and the two must not be
conflated: a written book's title itself renders upright, while an anvil-renamed item's name renders
italic, because vanilla's italic rule keys on whether an explicit custom-name *component* is present,
not on whichever string ultimately got resolved as the display name. The generation-name table is
shared between the tooltip and the reading screen so the two can't disagree with each other.

The reading screen is drawn as an **overlay**, not a full-frame screen, and both it and the editor route
through one shared frame-builder function precisely because the two are mutually exclusive (a book
stack is either writable or already-written, never both) and because that keeps hit-testing and drawing
constructed from the same source — a hand-rolled second overlay case is the natural place a future third
book screen would forget to draw at all despite hit-testing correctly. Page content stays as real styled
text all the way to the renderer (colors are preserved through word-wrap), though click/hover text
events on a page are inert — the interaction dispatcher for them doesn't exist yet.

All three book layouts (reader, editor, signing form) draw from the same real vanilla book texture — a
loose, non-atlas 256×256 sheet cropped to its top-left 192×192 window, registered as an extra on the
menu texture atlas so it participates in resource-pack reloads like everything else. All three also use
vanilla's own page-turn button sprites at the same derived positions, and all book text specifically
draws without vanilla's usual per-glyph drop shadow — a frame-local rendering rule, not a global change
to how text draws elsewhere in the menu or HUD.

### Lecterns

A lectern's container content is unusual: it carries only the book in its single slot, with the current
page communicated as a container-data property rather than as ordinary slot state. Page navigation is
optimistic (sent immediately, corrected by the server's next container-data update if it disagrees) and
reuses the editor's own word-wrap constants and widget rather than a second, independently-written wrap
implementation — since both vanilla screens actually share one wrap width, a second implementation would
be free to disagree with the first about exactly where a line breaks.

## What is not ported

Each of these is a real, known gap rather than a silent omission:

- **Page click/hover interactivity.** Pages retain their real click/hover text metadata through
  rendering, but nothing dispatches it yet.
- **Page Up / Page Down as real key bindings.** Arrow keys stand in for the dedicated pair vanilla
  binds.
- **Taking a book out of a lectern.** Page navigation and closing are implemented; the take-book
  control is not yet exposed.

## How to change it

- **A new tooltip line for a book goes into the book-specific lore-line function**, not the general
  tooltip assembly — see the container-screen tooltip docs for the general gotchas that still apply.
- **A third book-related screen should extend the shared overlay frame-builder**, not add a second,
  independent overlay case — the whole reason today's two screens share one is so hit-testing and
  drawing can never drift apart.
- **Keep the book texture registered on the menu atlas, not the HUD atlas** — the menu renderer owns
  this binding and already rebuilds it on every resource-pack reload; duplicating it onto the HUD atlas
  would just be a second, unnecessary copy.
- **Do not change the global default text-shadow behavior to fix a book screen** — book text's
  shadowless draw is a frame-local override; changing the global default instead would flip shadows on
  or off for unrelated screens.

## Configuration

None. Neither the editor, the reader, nor the lectern overlay has a feature flag or persisted setting —
both are unconditionally available.

## Dependencies

- `crates/lodestone-shell/src/menu/{book_edit,book_view,text_area}.rs` — the editor, reader, and shared
  multi-line text widget.
- `lodestone-model::Text` — the styled-text representation pages retain through rendering.
- `lodestone-game::item::ItemStack` — the writable/written book content fields both screens read from
  and (for the editor) write back to.
- Container-button-click and container-close action encoding — the wire mechanism lectern page
  navigation rides on.
- [`container-screens.md`](./container-screens.md) — the general container/overlay conventions books
  build on.
