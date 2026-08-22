# Book-and-quill editing

## What it is

`EDIT_BOOK` (issue #616's remainder): drafting and signing a `minecraft:writable_book`,
server-side, **and** the client-side screen that produces the packet (issue #613's
remainder). Before the client half, `ClientAction::EditBook` was encoded by every protocol
family with zero producers anywhere in `lodestone-shell` — the same outbound-island shape
`ClientAction::SetFlying` was caught in, except here the failure mode is silent (nothing
disconnects; a book simply cannot be written). Before either half, the packet decoded and
was discarded, and `ItemComponents` (`lodestone-model`) had no book-content fields at all —
a written or writable book anywhere in an inventory silently truncated the rest of whatever
packet carried it, the same decode-cliff class as `trim`/`map_id`/`pot_decorations`/`profile`.

## The client-side producer

`crates/lodestone-shell/src/menu/book_edit.rs`'s `BookEditState` is the port of vanilla's
`BookEditScreen`/`BookSignScreen`, folded into one state with a `signing` flag rather than
two `Screen` variants (see that module's own doc for why). Two new widgets carry it:

- `crates/lodestone-shell/src/menu/text_area.rs`'s `TextArea` — a **multi-line,
  word-wrapping** text field, the first of its kind in this shell (every prior text widget,
  `edit_box::EditBox`, is single-line). It approximates vanilla's real-font word-wrap
  (`Font.Splitter.splitLines`) with a fixed character-count wrap width, the same "no `Font`
  dependency" simplification `EditBox` already makes for horizontal scrolling — see that
  module's own doc. Reusable: any future text-heavy screen (a written-book viewer, a
  multi-line sign, an in-game notes feature) can reach for it instead of inventing another
  one.
- `edit_box::EditBox` for the signing title field — already existed, reused as-is.

**What sends `ClientAction::EditBook`**: `WindowApp::try_use`
(`crates/lodestone-shell/src/app/menus.rs`) forks on a `minecraft:writable_book` in either
hand — `Sim::writable_book_in_hand` (`crates/lodestone-shell/src/sim/session.rs`) resolves
the held stack via `Menu::player_native` and reads its
`lodestone_game::item::ItemStack::writable_book_content()` — and opens the screen
client-side, no server round trip, the same shape the command-block screen's own fork
already has. The screen's Done row calls `BookEditState::to_save_action` (title `None`);
Finalize (only reachable once `BookEditState::can_finalize()`, i.e. a non-blank title) calls
`to_sign_action` (title `Some(..)`). Both close through `MenuNav::close_book_edit`, and
`app::menus`'s `MenuAction::EditBook` arm is what actually calls `net.send_action(..)`.
Escape discards unconditionally, from either layout, matching vanilla: neither
`BookEditScreen` nor `BookSignScreen` overrides `onClose`, so the default `Screen.keyPressed`
Escape path (`onClose()` → `setScreen(null)`) never reaches `saveChanges()`.

**What is deliberately out of scope**, named rather than silently missing: per-pixel mouse
caret placement inside the page (a click anywhere in the page area is a no-op; keyboard
focus already always reaches it since neither layout has a second focusable field to compete
with it) and opening this screen for an already-signed `minecraft:written_book` (vanilla's
read-only `BookViewScreen`, which sends nothing on the wire and is therefore not part of this
producer at all).

**The client-side item-component gap this closed**: `lodestone-game`'s own `ItemComponents`
(`crates/lodestone-game/src/item.rs`) — a *different* type from the `lodestone-model` one
this doc's server section describes, despite the identical name (the class of duplication
issue #143 is about) — carried no slot for either book component at all, so a book's content
was dropped converting a decoded `lodestone_model::ItemStack` into the shell's own game-crate
shape, and the book editor had nothing to seed its pages from. Fixed the same way the dye and
trim components were: two new `ComponentValue` variants (`WritableBook`, `WrittenBook`), two
new well-known component keys, and both `From` conversions (`lodestone_model::ItemStack` →
`lodestone_game::item::ItemStack` and back) now carry the fields, so — unlike
`pot_decorations`/`profile`/`custom_data`, which this crate's component map still has no
slot for and remain named, one-way losses — a book's content round-trips exactly.

## How it works

`ServerboundEditBookPacket` carries no `ItemStack` — just a slot index, page strings, and an
optional title. `crate::server`'s `apply_edit_book` looks the carried book up in the tracked
`PlayerInventory` by that slot directly (`Inventory.isHotbarSlot(slot) || slot == 40`,
vanilla's own gate), the same way `handleEditBook`'s real handler does — reading
`this.player.getInventory().getItem(slot)` rather than decoding an item off the wire. So the
serverbound component-patch decode gap this crate has (`server_protocol.rs`'s
`read_optional_item_stack`/`read_hashed_stack` hard-reject any nonzero patch) does not block
this packet at all.

Two new `ItemComponents` fields (`crates/lodestone-model/src/item.rs`):
`writable_book_content: Option<Vec<String>>` and `written_book_content: Option<WrittenBookContent>`
(title/author/generation/pages/resolved). Decoded on the **clientbound** side
(`crates/protocol/v770/src/adapter/inventory.rs`'s `read_component_patch`) for the same
reason `trim`/`map_id`/`pot_decorations`/`profile` are: their stream codecs carry no length
prefix, so leaving them unmodeled truncates the rest of the packet from a book onward.

The gameplay logic (`apply_edit_book`): a draft save overwrites `writable_book_content` in
place; a signing submission transmutes the stack to `minecraft:written_book`, drops
`writable_book_content`, and sets `written_book_content` with the signer's plain-text name as
author, generation `0`, `resolved: true` — `signBook`'s own literal
`new WrittenBookContent(title, author, 0, pages, true)`. The item's own canonical key
(`minecraft:writable_book`) stands in for vanilla's `carried.has(DataComponents.
WRITABLE_BOOK_CONTENT)` gate, because that item registers the component as a **prototype
default** (`WritableBookContent.EMPTY`) rather than only after a first edit, and this crate
has no general item-prototype default-component census to reproduce that distinction.

Reaching the wire: `write_item_component_patch` (`server_protocol.rs`) teaches the
clientbound item-stack encoder (shared by `container_set_slot`/`container_set_content`/
`merchant_offers`) to write real `DataComponentPatch` entries for these two components when
present, so the edited/signed book actually appears in the client's inventory via
`CONTAINER_SET_SLOT`. **Scope**: only these two components are ever written this way — every
other modeled `ItemComponents` field still writes an empty patch, a real pre-existing gap
this pass did not close (before this change, *no* component of any kind ever reached the
wire outbound at all).

## How to change it

`DataComponentPatch.STREAM_CODEC` writes **both** counts (`added`, `removed`) up front,
before a single entry — not added-count/entries/removed-count. Getting this wrong is
invisible to `decode(encode(x)) == x` against your own first draft; it was caught here only
by round-tripping through the independently written client decoder
(`crates/protocol/v770/tests/book_content_wiring.rs`).

`written_book_page_nbt` is deliberately narrower than a general `Text` serializer would need
to be — it only handles `Literal`/`Translate` content with no style/click/hover, because
every page this crate itself signs is `Text::literal`. Do not widen it in place; a general
serializer belongs in `lodestone-model` next to `Text::from_nbt`, as its inverse
(`text_to_nbt`'s own doc comment in `server_protocol.rs` already says the same for the
disconnect-reason serializer beside it).

## Configuration

None.

## Dependencies

`crate::inventory::PlayerInventory` (the tracked slot the book lives in);
`lodestone_core::{Nbt, write_network_nbt}` for page serialization;
`crates/protocol/v770/src/adapter/inventory.rs`'s existing chat-component NBT reader
(`Text::from_nbt`) for the client decode side.
