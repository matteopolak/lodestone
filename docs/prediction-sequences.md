# Prediction sequences

## What it is

`PredictionSequence` is the version-free identity attached to a client-side
block placement. It preserves the protocol's signed VarInt bit pattern at the
wire boundary while giving the placement ledger explicit wrapping order.

## How it works

`Placement` allocates the next typed sequence before constructing a
`ClientAction::UseItemOn`. Each adapter that exposes a signed VarInt packet
field converts the typed value with `as_wire()` at that packet boundary. The
integrated server converts that field back, applies the authoritative
interaction, emits its block updates, and then sends a `block_changed_ack`
carrying the same sequence. The client applies those block updates first and
retires predictions through serial-number ordering.

The counter wraps across all 32 bits. A signed wire value is not invalid merely
because it is negative; malformed or truncated packet bodies are rejected by
the protocol decoder before they can reach the server consumer. Distances of
exactly half the counter domain are intentionally ambiguous, so the client
must keep fewer than half the possible predictions outstanding.

## How to change it

Keep conversion at the version adapter and protocol decoder. Extend the
placement tests with an independently chosen wire value, a rollover case, and
a malformed-body control whenever the packet shape changes. Test each adapter
that carries the sequence, including the signed-bit-pattern boundary. Do not
compare raw signed values in the reconciliation ledger.

## Configuration

There are no runtime flags. `PredictionSequence::INITIAL` is zero, and each
predictive placement allocates the next value before it is emitted.

## Dependencies

The type lives in `lodestone-model`; `lodestone-game::placement` owns pending
predictions; `lodestone-server` owns authoritative interaction and
acknowledgement; and the version adapters perform packet encoding and
decoding.
