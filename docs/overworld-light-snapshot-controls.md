# Overworld light snapshot controls

## What it is

The `overworld_light_snapshot` integration controls pin the distinction between
an initial chunk light fallback and a light snapshot restored from storage. They
focus on the `(-25,-25)` serving coordinate and make the wire representation of
high sky and zero block-light sections observable.

## How it works

The retained-snapshot control builds a complete `ColumnLight` containing
`Missing`, `Uniform(0)`, and `Values` entries, attaches it to a server column,
and decodes the resulting initial chunk packet. Equality with the original
snapshot proves that the initial encoder consumes retained light verbatim.

The generated-fallback control obtains the seed-42 Overworld column at
`(-25,-25)` from the normal source and confirms that it has no retained light.
It then checks the fallback's compact representation: the high sky section 14
and the corresponding zero block-light section are absent. This is a lifecycle
control, not a claim that a restored settled column should have the same tags.

## How to change it

Update the focused integration test when the initial-light lifecycle or packet
representation changes. Keep the retained and generated cases separate: a
failure in the first points at packet encoding or light decoding, while a
failure in the second points at fallback settlement or normalization. Run:

```text
cargo test -p lodestone-v26-2 --test overworld_light_snapshot -- --nocapture
```

The test uses the shared workspace target and should be run in the foreground.

## Configuration

There are no test-specific environment variables. The generated control uses
seed `42`, the protocol's Overworld shape, and serving coordinate `(-25,-25)`.

## Dependencies

The control uses `lodestone-v26-2` packet encoding/decoding, the version-free
`lodestone-server` chunk source and retained-column API, and
`lodestone-world` light representations.
