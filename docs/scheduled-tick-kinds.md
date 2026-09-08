# Scheduled tick kinds

## What it is

Scheduled tick records use a typed key for the built-in update lanes while
retaining an exact open extension value for plugin-defined actions. The live
block queue is typed, while fluid processing retains an explicit string-keyed
boundary until that implementation is migrated.

## How it works

`lodestone_server::scheduled_tick::ScheduledTickKind` stores the fourteen
known built-in lanes as enum variants. `Extension(String)` preserves any other
discriminator without guessing its meaning. The live block queue and block
feed are typed; the fluid queue remains string-keyed behind an explicit fluid
feed method until fluid processing is migrated. `PersistedScheduledTick` and
`StagedTick` carry the typed key. `ScheduledTickHandle` converts only the
fluid lane at its queue boundary and converts legacy fluid records back when
taking a non-destructive snapshot.

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
`BlockTickFeed::request_scheduled_ticks`; fluid producers must call the
explicit `request_fluid_scheduled_ticks` boundary. Keep parameterized piston
records as `Extension` values until their payload has a dedicated typed model.
When a reaction needs to test for a pending kind, keep the detector next to the
reaction and accept `ScheduledTickQueueAccess<ScheduledTickKind>` rather than
reintroducing a string comparison.

## Configuration

There are no runtime flags. The `extension_kind` field is present in storage
schema version 1 and is meaningful only when the compact `kind` field is
`UNSPECIFIED`.

## Dependencies

The scheduler owns the key model. Native record encoding uses
`lodestone-storage-schema`; the integrated server's block producers use typed
keys while the fluid implementation and legacy fixture adapters continue to
use their string boundary.
