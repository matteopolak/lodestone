# Typed block properties

## What it is

The lodestone-data block_properties module represents built-in block-state
keys and values as generated enums and exposes complete states through the
Properties domain type. Its storage is private, so callers do not depend on
the current fixed-capacity representation.

## How it works

The ignored block_properties drift test reads the authoritative 26.2
block-state report, collects the 93 keys, 153 values, and 435 valid key/value
pairs, and emits generated enums plus compact property-set and state tables.
The generator asserts that all 32,366 state rows are present, that their owners
match the canonical identity table, and that the largest state has seven properties.

Properties::from_state_id uses those generated numeric tables directly. It
does not parse state or property text. Properties::parse is strict and is
intended for configuration or other text boundaries only. state_for_block
matches a typed property set only inside the canonical span for the supplied
block, so a key/value pair valid for one block cannot resolve to another. This
uses `block_states::state_span`, shared with text resolution: the report-only
identity emitter stores `(start, count)` and both consumers search the half-open
range `start..start + count`. The property table does not store duplicate spans.
with_builtin replaces or inserts one generated key/value pair without leaving
the typed representation; this is the operation used by moving block render
state synthesis.

Values supplied by extensions use ExtensionId, an opaque u32 handle owned by
the extension registry. The resident built-in Properties representation stores
one-byte keys and one-byte built-in values; extension values do not enlarge
every resident block state.

BlockStateValue uses this typed parser when a serialized state crosses into the
generated census. A partial list keeps the registered default for omitted keys,
while duplicate, unknown, malformed, and wrong-schema properties remain an
extension value. `as_str` and `into_string` are the explicit serialization
boundary APIs; runtime consumers should retain StateId or Properties instead of
re-parsing their spelling.

## How to change it

Add or change generation in
crates/lodestone-data/tests/block_properties.rs. Run the ignored drift test
with LODESTONE_REGEN=1; it writes a temporary file, syncs it, and renames it
into place atomically. Run the ordinary focused test afterward: it
round-trips every generated state and exercises malformed input, duplicate
keys, compact-layout assertions, wrong-block-schema rejection, and a neighboring
dirt-state control that catches treating an inclusive last ID as a count.

Keep the generated file present while changing the generator. Do not hand-edit
src/generated/block_property_tables.rs, and do not migrate worldgen call sites
until the typed API has an independently measured consumer.

## Configuration

The report path is
.cache/mc/26.2/generated/reports/blocks.json relative to the repository.
LODESTONE_REGEN=1 enables generation; without it the ignored test compares the
newly rendered source byte-for-byte with the committed file. For a safe
preview, set LODESTONE_TYPED_PROPERTIES_OUTPUT to a new temporary path.

## Dependencies

The module depends on lodestone-data block_states and its generated state and
canonical block identity tables. Span generation belongs to the
[Rust identity emitter](./data-identity-codegen.md), not the property generator.
The property generator uses the existing serde_json development dependency;
runtime code has no additional dependencies.
