# Prediction sequences

## What it is

`PredictionSequence` is the version-free identity attached to client-side block
predictions. It keeps the protocol's signed VarInt representation at the wire
boundary while giving placement and acknowledgement code a wrapping counter
with explicit serial ordering.

## How it works

The model stores the counter as `u32`; `from_wire` and `as_wire` reinterpret the
same 32 bits as the protocol's `i32`. `next` wraps across the complete domain.
Placement records the typed value for every optimistic placement, and the shell
settles entries through `is_at_or_before`, which correctly handles a
max-to-zero rollover. Serial distances of half the domain are intentionally
ambiguous; the client cannot retain that many pending predictions.

## How to change it

Keep integer conversion at protocol adapters and use the domain predicate for
ledger comparisons. Do not restore signed `<=` comparisons or compare the raw
wire values after rollover. Add controls in `lodestone-model` for any new
boundary or ordering rule before changing the placement ledger.

## Configuration

There are no runtime flags. `PredictionSequence::INITIAL` is zero, and each
predictive action allocates its next value before being emitted.

## Dependencies

The type lives in `lodestone-model`; `lodestone-game::placement` owns pending
predictions, `lodestone-game::mining` owns action allocation, and the shell
settles the ledger after the model acknowledgement event reaches its network
update fold. Version adapters only convert the typed value to and from their
signed VarInt packet fields.
