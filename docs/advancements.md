# Server advancement state

## What it is

The server advancement subsystem owns advancement completion, per-player progress, visibility, and
the version-free update payload consumed by protocol encoders. Registry-backed advancement IDs are
validated as `lodestone_model::ResourceKey` values at the manager's internal tree and progress seams.

## How it works

`AdvancementManager::new` parses each node ID and parent into typed keys before building its ordered
tree. Per-player progress, dirty IDs, and visible IDs use the same key type. Gameplay methods retain
string arguments at their public compatibility boundary and parse them before lookup; malformed or
unknown IDs are no-ops for gameplay triggers.

The manager lowers keys back to strings only when producing `AdvancementUpdate` values for the
protocol layer or NBT persistence. Packet DTOs and NBT field names remain strings because those are
serialization boundaries. The initial update and dirty flush are the production consumers: they read
the typed tree, compute visibility/progress, and emit the wire-facing payload.

## How to change it

Change `lodestone-server/src/advancements.rs` when adding completion rules or changing the internal
key model. Keep `AdvancementUpdate` string fields aligned with protocol encoders, and add a test that
uses a custom namespace plus a malformed-ID control whenever a new ID-bearing path is introduced.
When changing persistence, verify both the typed in-memory state and the string NBT round trip.

## Configuration

There is no runtime configuration. `AdvancementManager::builtin` supplies the built-in tree; callers
may construct a separate tree through `AdvancementManager::new`.

## Dependencies

The subsystem uses `lodestone-model::ResourceKey` for validation, `lodestone-core::Nbt` for player
save/load, and `ServerProtocol::encode_update_advancements` implementations to serialize updates.
