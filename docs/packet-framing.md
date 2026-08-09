# Packet framing, and the empty frame that ended sessions

## What it is

How `lodestone-net` turns a byte stream into packets — the length prefix, the compression
header, and the one shape of frame that carries no packet at all. `Codec` in
`crates/lodestone-net/src/codec.rs` does the framing and `Connection::read_packet` in
`connection.rs` splits a frame body into `(packet_id, fields)`.

The reason this has a doc is a single measured defect: an **empty** frame was a fatal
transport error here and is silently dropped by vanilla, which cost whole sessions on any
server behind a Velocity proxy.

## How it works

Uncompressed, a frame is `[VarInt length][packet id VarInt][fields…]`. After
`login_compression` sets a threshold it becomes
`[VarInt frame length][VarInt uncompressed length][data]`, where `uncompressed length == 0`
means "`data` is not compressed" and any positive value means "`data` is zlib and inflates to
exactly this many bytes".

`Codec::next_packet` reads the length (capped at `MAX_LENGTH_VARINT_BYTES = 3`, vanilla's
`Varint21FrameDecoder` limit), rejects a zero length as a malformed frame and a length over
`MAX_PACKET_LEN` as too large, then hands the frame to `decompress_frame` when compression is
on. `Connection::read_packet_raw` returns the resulting body; `read_packet` reads the id out
of it.

## The empty frame

A **one-byte frame of `0x00`** in compression mode declares "uncompressed, zero bytes of
packet data" — a legal frame length of 1, and no packet id inside it. Reading an id out of it
raises `lodestone_core::Error::UnexpectedEof`, which becomes `NetError::Codec` and therefore
`ClientError::Transport`. That is fatal: `Driver::run` fails **open** on
`AdapterError::Decode` and closes the session on a transport error, by design. One junk frame,
one lost session, reported as `protocol codec error: unexpected end of input`.

`read_packet` now skips an empty body and reads the next frame.

**Vanilla tolerates it, and how is worth knowing, because there is no guard to point at.**
`Varint21FrameDecoder` rejects only a zero *length*. `CompressionDecoder` turns
`uncompressedLength == 0` into `in.readBytes(in.readableBytes())`, i.e. an empty buffer.
`PacketDecoder` has no empty check and would throw if it ran. It never runs: netty's
`ByteToMessageDecoder` only calls `decode` while the buffer `isReadable()`, so the empty one
is dropped before `PacketDecoder` sees it. **The tolerance is a property of the pipeline, not
of the packet classes** — which is exactly why reading `PacketDecoder` alone suggests vanilla
would die here too, and why this was diagnosed as our bug only after a live capture.

Measured against a live Velocity proxy at protocol 776 with compression threshold 256: it
emits exactly this frame a few seconds into the play phase. In the session that prompted the
investigation it arrived one millisecond after three unrelated item-component warnings, which
is why the warnings were blamed. They were coincident, not causal — the components were a real
second bug, and fixing them did not keep the session alive.

## How to change it

The skip is a `continue` in `Connection::read_packet`'s loop, gated on `body.is_empty()`.
Two things to preserve if you touch it:

* **`read_packet_raw` still returns the empty body.** Only `read_packet` skips. Anything that
  reads raw frames must decide for itself; today only `tests/round_trip.rs` does.
* **The gate needs a real packet after the empty frame.**
  `connection::tests::an_empty_compressed_frame_is_skipped_not_fatal` writes the hand-built
  `[0x01, 0x00]` and then a normal packet, because a `read_packet` that returned `Ok(None)`
  and hung up would pass a test that only checked "no error" — and a clean end of stream loses
  the session just as surely as an error does.

The general lesson, which generalises past this frame: a wire shape our decoder rejects is not
automatically malformed. Check what vanilla's *pipeline* does with it, not only what its packet
code says, and prefer a live capture over a reading when the two disagree.

## Configuration

* `MAX_PACKET_LEN` (2 MiB), `MAX_DECOMPRESSED_LEN` (8 MiB) and `MAX_LENGTH_VARINT_BYTES` (3)
  in `codec.rs`, all matching vanilla's `CompressionDecoder`/`Varint21FrameDecoder` constants.
* The compression threshold is set at runtime by `Connection::set_compression` from
  `login_compression`. A negative threshold disables it.

## Dependencies

* `lodestone_core::{Reader, Writer}` for the VarInts, and `lodestone_core::Error` — whose
  variants reach the caller wrapped as `NetError::Codec`.
* `flate2` for zlib.
* `crates/protocol/v770/tests/live_plugin_server_join.rs` is the live reproducer: an
  `#[ignore]`d gate driven by `LODESTONE_LIVE_SERVER=host:port` that joins a real plugin
  server and requires the session to outlive its lobby inventory.
