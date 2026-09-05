# Protocol 47 hosting

## What it is

`crates/versions/1.8` can host protocol 47 alongside its existing joining adapter. The
registry resolves protocol 47 to `V47ServerProtocol`; neighboring protocol numbers remain
unhosted.

## How it works

The login flow switches directly from login success to Play because this protocol has no
configuration exchange. Its join packet uses a one-byte dimension field and its initial position
packet ends at the relative-coordinate flags byte. The client adapter answers that absolute
placement with an ordinary position-and-look packet; there is no separate confirmation-id packet.

Chunk encoding accepts a canonical column only when it covers y=0 through y=255. It projects that
window into non-empty 16-block sections, converts every canonical state through the exact inverse
table within protocol 47's contiguous numeric block range `0..=197`, and writes each legacy state
word little-endian in YZX order. The body then contains one
block-light and one sky-light array per emitted section, followed by the 256-byte biome footer.
The committed real-section fixture in `crates/versions/1.8/tests/support/` anchors that layout.
An unrepresentable state or a source that does not cover the full window is an encoding error;
neither can become air silently.

Block updates use the same exact state conversion. The serverbound break decoder accepts the three
break phases and six faces so an observed chunk block can produce an observed update. The in-memory
integration test connects the family adapter to a registry-selected server transport, verifies a
known dandelion appears, breaks it, and observes the replacement air state.

`V47ServerProtocol` also participates in the shared connection watchdog: it encodes a clientbound
keep-alive and consumes its serverbound echo as `ServerBound::KeepAlive`. Protocol 47 carries the
id as a signed VarInt, unlike protocol 5's fixed `i32` and later fixed `i64` forms. The hosted
wire control fixes the VarInt bytes directly and proves a trailing byte is rejected rather than
treated as an acknowledgement; an external 1.8.9 client session remains the compatibility check.

Serverbound `settings` now supplies its signed-byte view distance to the same shared view tracker
through `ServerBound::ClientInformationChanged`; resizing streams the added or removed columns to
the connected client, subject to the server ceiling. Locale, chat settings, and skin parts are
decoded only to establish the exact packet boundary. The in-memory host starts at radius zero,
sends a radius-one settings update, and observes column `(1, 0)` on the client; protocol 47's
settings body ends after the skin-parts byte, without a main-hand field.

The host also lifts all four movement forms: `position` and `position_look`
carry an absolute pose, while `look` and `flying` carry only rotation or the
grounded flag. Protocol 47 has no teleport id: a matching `position_look` is
the placement echo, so it must remain ordinary movement rather than be
invented as a separate acknowledgement. The in-memory movement control crosses
into chunk `(1, 0)` and observes that the shared view tracker streams it.

The protocol-47 `block_dig` decoder also preserves the non-breaking actions
that share its legacy body: statuses `3` and `4` drop the whole selected stack
or one selected item, and status `5` releases an item use. They lift to the
server's inventory-drop and release-use consumers instead of becoming ignored
after the adapter has successfully encoded the key input. Unlike later legacy
families, protocol 47 stops there: its pre-off-hand wire vocabulary has no
status-`6` hand swap.

## How to change it

Keep protocol-47 section encoding local to this family. Its raw state-word layout differs from the
palette-based legacy format in the later hosted family, so sharing an encoder would hide a material
wire distinction. Extend `V47ServerProtocol` only with packets whose fields have an independent
fixture or live-session proof, and add the matching client/server integration assertion when a new
packet has a visible consumer path.

## Configuration

Enable the `v1-8` feature on `lodestone-registry`. The default registry build does not expose this
family. The hermetic tests use the integrated in-memory transport; the remaining external check is
a live 1.8.9 client session.

## Dependencies

`lodestone-server` provides the version-free hosting trait and canonical chunk source,
`lodestone-canonical` supplies the exact legacy inverse mapping, and `lodestone-client` plus the
v1-8 adapter provide the in-memory consumer test.
