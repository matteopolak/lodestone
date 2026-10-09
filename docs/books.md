# Books

## What it is

Reading and writing in-game books: the writable-book editor with its signing flow, and the read-only screen for a signed book (plus a lectern's book display). All three share one texture, one word-wrap model and one overlay frame builder.

## How it works

### Writing and signing

The book-and-quill editor is one state machine with a `signing` flag, not two screens. It introduced the shell's first **multi-line, word-wrapping** text widget (`menu/text_area.rs`), approximating font-based wrap with a fixed character-count width (like the single-line box's horizontal scrolling); reuse it for any future multi-line text screen. Opening is client-side only: using a writable book forks into the screen with no server round trip (like the command-block screen). Saving a draft overwrites the page content in place; finalising (only with a non-blank title) transmutes the item into a signed book with the signer as author, copy generation zero and content marked resolved. Escape discards from either layout; only an explicit action saves.

Out of scope for the editor: per-pixel caret placement (a click in the page area is a no-op) and opening an already-signed book (that is the read-only screen).

### Reading a signed book

A signed book's title is its **item display name** (falling back from any custom name to the book title), so it also feeds the held-item tooltip and every hover-name consumer. The author line and copy-generation line ("Original", "Copy of original"...) are a separate tooltip addition and must not be conflated: a written book's title renders upright while an anvil-renamed item's name renders italic, since the italic rule keys on whether an explicit custom-name *component* is present. The generation-name table is shared by the tooltip and the screen.

The reader is an **overlay**, not a full-frame screen, and it and the editor go through one shared frame-builder (a book stack is writable or written, never both), so hit-testing and drawing come from the same source (a hand-rolled second overlay is where a future screen would forget to draw). Page content stays real styled text to the renderer (colours survive word-wrap, with each run's click, hover and insertion).

Page text is interactive: a page is a full chat component, so runs can carry click or hover (`change_page` exists almost solely for books). One function on the reader's state says where each run draws; the frame builder emits labels from it and the hit-test tests against it, so a click cannot land on a run seen elsewhere (rects are the shared menu slot type). A `run_command` closes the book first (the command may open a screen); other actions leave it open so a page of links stays usable. The pointer is recorded on the reader's state because the frame builder takes only state, and the tooltip is resolved there rather than at draw time because it needs the language table (a hover payload inside an already-resolved page is not resolved). Tooltips paint through the menu overlay's painter: text payloads keep legacy colours, item and entity payloads use a compact description, an unknown action shows an explicit unsupported-hover line.

All three layouts (reader, editor, signing form) use the real book texture, a loose 256x256 non-atlas sheet cropped to its top-left 192x192, registered as an extra on the menu atlas so it follows resource-pack reloads, the same page-turn sprites at the same derived positions, and book text draws without the usual per-glyph drop shadow (a frame-local rule, not a global change).

### Lecterns

A lectern's container carries only the book in its single slot; the current page is a container-data property. Page navigation (previous/next or page-jump) is sent immediately and corrected by the server's next container-data update, and reuses the editor's wrap constants and widget (both reference screens share one wrap width, so a second implementation could disagree about line breaks).

Not ported: a hex-coloured page hover tooltip (the painter is a legacy-colour string surface and reduces unsupported colour detail rather than dropping the tooltip).

## How to change it

- A new book tooltip line goes in the book-specific lore-line function, not the general tooltip assembly ([container screens](./container-screens.md) lists the general gotchas).
- A third book screen extends the shared overlay frame-builder so hit-testing and drawing cannot drift.
- Keep the book texture on the menu atlas (which already rebuilds on resource-pack reload), not the HUD atlas.
- Fix a book screen's shadow locally; changing the global text-shadow default flips unrelated screens.

## Configuration

None.

## Dependencies

`crates/lodestone-shell/src/menu/{book_edit,book_view,text_area}.rs`; `lodestone-model::Text` (styled pages); `lodestone-game::item::ItemStack` (book content fields); container-button-click and container-close encoding (lectern navigation); [container screens](./container-screens.md).
