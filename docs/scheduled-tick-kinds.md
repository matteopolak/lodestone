# Scheduled tick kinds

## What it is

Scheduled tick records use a typed key for the built-in update lanes while
retaining an exact open extension value for plugin-defined actions. Both live
queues are typed; text survives only at explicit feed, persistence, and
focused-fixture boundaries.

## How it works

`lodestone_server::scheduled_tick::ScheduledTickKind` stores the fourteen
known built-in lanes as enum variants. `Extension(String)` preserves any other
discriminator without guessing its meaning. The live block and fluid queues
are typed. Fluid spread and generated-column seeding request the `Fluid`
variant through `ScheduledTickSink`; focused legacy queues accept that typed
request through an explicit lossless adapter. `PersistedScheduledTick` and
`StagedTick` carry the typed key. The connection feed is still a textual
compatibility boundary and converts names when ticks enter the live queue.
Anvil saving converts typed names back to `chunk_nbt::SavedTick`, while native
snapshots retain the typed key.
Serde uses the canonical registry name, so built-ins and `Extension` keys
round-trip without exposing Rust variant names.

The native storage record keeps built-ins in its compact enum field. Extension
records leave that field unspecified and carry the original name in
`extension_kind`; the schema rejects an ambiguous record that supplies both.

Target projectile hits use `redstone_target::has_pending_decay` to query the
typed `TargetDecay` key. An extension containing the same spelling remains a
different key, so a plugin record cannot suppress a built-in target decay.
Pressed buttons likewise expose `hand_use::TICK_BUTTON` as the typed
`ButtonRelease` key; its string spelling is recovered only by an explicit
boundary conversion.

## How to change it

Add a built-in variant and its canonical name mapping in
`crates/lodestone-server/src/scheduled_tick.rs`, then add the matching compact
storage enum value and conversion arm. After changing the storage proto, run
`LODESTONE_STORAGE_SCHEMA_REGENERATE=1 cargo check -p lodestone-storage-schema`
to refresh the checked-in generated Rust and descriptor artifacts. Never
silently map an unknown name to a built-in: it must remain an `Extension` value.
Block producers should call
`BlockTickFeed::request_scheduled_ticks`; fluid producers should use the
explicit `ScheduledTickSink<ScheduledTickKind>` boundary. The
`request_fluid_scheduled_ticks` method remains the compatibility feed boundary;
the tick loop converts its names before entering the live queue. Keep parameterized piston records
as `Extension` values until their payload has a dedicated typed model. When a
reaction needs to test for a pending kind, keep the detector next to the
reaction and accept `ScheduledTickQueueAccess<ScheduledTickKind>` rather than
reintroducing a string comparison.

## Configuration

There are no runtime flags. The `extension_kind` field is present in storage
schema version 1 and is meaningful only when the compact `kind` field is
`UNSPECIFIED`.

## Dependencies

The scheduler owns the key model. Native record encoding uses
`lodestone-storage-schema`; the integrated server's block producers and fluid
implementation use typed keys, while the legacy feed, Anvil, and fixture
adapters retain explicit string boundaries.
