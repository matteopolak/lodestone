# Mob-effect registry boundary

## What it is

`lodestone_data::mob_effects::MobEffectId` is the validated 26.2 built-in
`minecraft:mob_effect` registry id. It separates a known entry in the shipped
census from an arbitrary integer carried by a version-free item component or
an extension.

## How it works

The generated `MOB_EFFECT_NAMES` and colour tables are indexed only by
`MobEffectId`. The 26.2 entity-effect decoder validates its VarInt before it
creates a `ClientEvent`, and the 26.2 server and beacon encoders obtain an id
by canonical name before writing it. The HUD's natural effect ordering and the
beacon UI likewise resolve a validated id, so their table lookups are total.
The 1.17 family applies the same boundary to both legacy packet forms: it
converts their 1-based wire value to `MobEffectId` before producing an event,
and rejects an unknown value without attempting a table lookup.

`MobEffectInstance` intentionally retains its raw `i32` id. It can arrive in
an item component where the owning session or an extension has supplied a
value outside this built-in census. Tooltip and colour consumers validate that
raw value when they need built-in data; an unknown value is preserved by the
model and simply has no built-in name, tooltip, or colour.

## How to change it

Regenerate the mob-effect names and colours together when the canonical data
version changes, then update the literal boundary controls in
`lodestone_data::mob_effects`. New 26.2 packet paths should convert their raw
VarInt with `MobEffectId::from_registry_id` once at decode or encode entry,
then pass the typed value through table lookups. Do not change
`MobEffectInstance` to reject an extension value unless the owning session's
registry synchronization is also modeled.

## Configuration

There is no runtime configuration. The fixed census is generated from the
canonical 26.2 registry report and has 40 entries.

## Dependencies

This boundary depends on the generated mob-effect name and colour tables.
The 26.2 protocol adapter, integrated-server packet encoder, shell HUD, and
beacon controls consume it.
