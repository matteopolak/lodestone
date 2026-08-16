# Book-and-quill editing

## What it is

`EDIT_BOOK` (issue #616's remainder): drafting and signing a `minecraft:writable_book`,
server-side. Before this, the packet decoded and was discarded, and `ItemComponents`
(`lodestone-model`) had no book-content fields at all — a written or writable book anywhere
in an inventory silently truncated the rest of whatever packet carried it, the same
decode-cliff class as `trim`/`map_id`/`pot_decorations`/`profile`.

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
