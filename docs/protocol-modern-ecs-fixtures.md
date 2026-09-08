# Modern protocol ECS fixtures

## What it is

The modern protocol fixture suites prove that entity metadata, equipment, and
attribute packets reach the production `NetIngest` schedule and become the ECS
components consumed by the client. They cover protocol 766, protocol 774 with
its `set_entity_data` name, and protocol 776.

## How it works

Each suite stores independent packet bodies as literal bytes. The version
adapter decodes those bodies into `ClientEvent` values, and the test queues the
events in an `App` with `IngestPlugin` before running `NetIngest`. Assertions
read `EntityFlags`, `Equipment`, and `Attributes` from the indexed spawned
entity, so a passing decoder-only test cannot hide a missing production fold.

The fixtures deliberately use non-default flags, a real stone stack, a base
movement-speed value, and a modifier. Each suite also has an unknown-attribute
control; an invalid registry id must be rejected or produce no event rather
than being coerced to a nearby entry.

## How to change it

Add a new packet body only when its field order is independently established,
and keep the body separate from the packet encoder under test. When a protocol
changes an id table, update that version's fixture and its expected canonical
key together. Keep the final assertion at the ECS component boundary and retain
the wrong-value control whenever a registry mapping changes.

The protocol-776 suite is included from `tests/entity.rs`, which keeps the
entity test binary consolidated. Protocol 766 and 774 each use a standalone
integration-test binary because their crates have no runtime dependency on the
ECS crate.

## Configuration

The fixtures require no environment variables, network access, JVM, or live
server. Run them with:

```text
cargo test -p lodestone-v1-20-6 --test metadata_attributes_ecs
cargo test -p lodestone-v1-21-11 --test metadata_attributes_ecs
cargo test -p lodestone-v26-2 --test entity modern_ecs
```

## Dependencies

The tests use the version-specific adapter, `lodestone-model` packet events,
`lodestone-world` as the adapter's packet-world sink, and `lodestone-ecs` for
the production ingest schedule and component types. They do not encode their
fixtures before decoding them.
