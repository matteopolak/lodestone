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

Protocol 47's `block_place` is now also a hosted consumer boundary. Its packed
position, face byte, mandatory inline slot, and three cursor sixteenths lift to
`ServerBound::UseItemOn`, with main hand and sequence zero because neither
concept exists on this wire. Cursor bytes outside `0..=15` and unknown faces are
rejected rather than clamped into a plausible placement. The separate
`(-1, -1, -1), -1` in-air sentinel remains ignored: this packet does not carry
the click-time look direction required by the shared in-air-use consumer.

The protocol-47 `chat` packet now connects its one bounded string to the shared
text and command consumers. Ordinary text becomes unsigned `ServerBound::Chat`
with explicit zero timestamp and salt; a leading slash becomes
`ServerBound::ChatCommand` with the prefix removed. The shared server decorates
and broadcasts accepted text, and `V47ServerProtocol::encode_system_chat` sends
that reply as a JSON literal component with position byte `1` (normal system
chat). Literal input and reply bodies pin the bounded-string and JSON/position
shapes, including trailing-byte rejection; the in-memory protocol-47 control
selects the host through the registry, sends the real client action, and
observes the decorated reply through the adapter's `ClientEvent` stream.

The protocol-47 `arm_animation` request is an empty Play body, so it lifts to
the shared main-hand `ServerBound::Swing` consumer. The host broadcasts the
result through the legacy `animation` packet; its varint entity id and raw
animation byte are decoded by the family adapter into `ClientEvent::EntityAnimation`.
Literal empty/trailing-byte request controls and literal main/off-hand animation
codes cover both packet directions, while the registry-selected adapter test
proves the complete protocol-to-consumer-to-client-event path.

The protocol-47 `entity_action` frame now passes its leave-bed ordinal (`2`)
to the shared wake consumer as `ServerBound::PlayerCommand { action: 0 }`.
Its sender id is deliberately ignored because the connection determines the
player being woken; the riding-jump field is likewise not a wake input. A
literal three-VarInt control fixes the packet shape and rejects the adjacent
sneak ordinal, trailing bytes, and all non-Play states. The client adapter's
leave-bed action is then decoded through the registry-selected protocol, which
proves this version-family frame reaches the common consumer boundary.

The joining adapter translates four additional clientbound state packets.
`update_time` is two raw `i64` values and becomes `ClientEvent::TimeChanged`;
its second value remains signed so a frozen day-night cycle is not lost.
`game_state_change` reason `1` ends rain and reason `2` begins it; its
game-mode, rain-level, and thunder-level reasons map to their corresponding
canonical events. Other reasons
are consumed but ignored because they have no non-invented canonical carrier;
the game-mode value is accepted only for integral ids `0..=3`. The legacy
`explosion` body keeps its signed byte block offsets and always emits a
`Some` knockback vector, including an all-zero vector, because those three
motion fields are unconditional on this wire. Before emitting that event, the
adapter applies every offset to `floor(center) + offset` in the loaded world as
canonical air and synchronizes the block-entity tail with `None`, so removed
blocks stop contributing pixels and collision immediately. Finally, textual attribute
snapshots use a VarInt entity id, fixed-width property count, and VarInt
modifier count. The adapter maps the seven established legacy names to
canonical keys, represents UUID-only modifiers with stable private ids, and
skips unrecognized attribute names without dropping neighboring snapshots;
modifier operation bytes outside `0..=2` reject the packet.
`crates/versions/1.8/tests/misc_events.rs` uses literal packet bodies for
these paths so field-order or count-width regressions cannot self-confirm via
the version codec.

### External-client acceptance

The opt-in release-client gate covers hosted protocol **47** (1.8.9). Run it with
`just external-client-acceptance --protocol 47 --output /private/tmp/lodestone-v47` and an
external driver. The six-stage evidence records direct login-to-Play (`configuration.mode:
"login_to_play"`) and unbatched initial chunks (`chunk_batch_acknowledgement.mode: "unbatched",
batch_count: 0`), then requires world join, deliberate movement, one observed
`start_destroy_block` result, and a client-initiated clean disconnect. Provenance must identify the
exact 1.8.9 client build and retain non-empty capture and client-log artifacts. No client was
launched while this document was updated; protocol 47 remains unverified by a real release client
until its manual run produces a passing `report.json`.

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
