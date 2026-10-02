# Canonical Entity Census

## What it is

The entity census supplies names, living and AI-mob membership, ordinary crowd-push capability,
hard-collision capability, and base hitboxes for every canonical built-in entity. Its 161 entries
preserve all 158 original 26.2 IDs and append the three identities introduced in 26.3.

## How it works

`EntityType` is the validated lookup boundary. Its discriminant is a canonical ID, and every
entity capability and dimension accessor accepts that enum and returns a total result. Protocol
adapters must translate a release's wire registry ID before constructing it. In 26.3, cushion's
wire ID is 33 but its canonical ID is 158; directly indexing the canonical table with 33 would
give it a boat's collision behavior.

The base census and independent dimension capture are committed under `lodestone-data`'s test
support directory. A complete latest-release capture records 161 wire IDs and resource names,
four capability flags, and the raw floating-point bits of each base dimension. A second program
captures the dimensions independently. Both live captures are checked against the official
registry report before the latest semantic rows are published.

The reduction grants ordinary crowd-push capability only to a living type whose crowd and
pairwise push steps can reach a player. Boats have a separate push mechanism, so they are hard
colliders without ordinary crowd-push capability. Hard collision records the maximum capability
of the type; alive and instance-state gates remain the caller's responsibility.

The measured new rows are:

| Canonical ID | Type | Base hitbox | Living / mob / crowd push | Hard collision |
|---|---|---|---|---|
| 158 | `minecraft:cushion` | 1 × 0.25 | false / false / false | false |
| 159 | `minecraft:poplar_boat` | 1.375 × 0.5625 | false / false / false | true |
| 160 | `minecraft:poplar_chest_boat` | 1.375 × 0.5625 | false / false / false | true |

Every shared type has matching capability flags and bit-exact dimensions across the two
releases. The generator checks that agreement before reusing a shared row; future disagreement
requires a generation-selected behavior accessor rather than a copied default.

## How to change it

Refresh both live captures against the target server release, validate complete dense wire IDs
and official resource-name membership, and retain capture hashes in the committed semantic
fixture. Inspect any newly declared interaction behavior before assigning its reduced flags.
Preserve the original canonical prefix and append identities in their first release's wire
order. A new living type requires an explicit crowd-pass model.

`tests/support/entity-union.rs` combines the canonical prefix with the latest resource names.
The `entity_census`, `entity_dimensions`, `entity_type_enum`, and `entity_types` integration tests
generate the corresponding runtime modules. Run their ignored drift checks after generation;
the ordinary tests check complete rows, independently captured dimensions, new entity witnesses,
and a control demonstrating the incorrect result of directly indexing by a shifted wire ID.

## Configuration

`LODESTONE_REGEN=1` enables table writes in the ignored integration-test generators. Without
that variable the same generators check exact committed bytes. Canonical lookups have no runtime
configuration and allocate no heap memory. Live extraction uses Java 25 and the release's own
server libraries. The census extractor's `--collision-declarer` option records the declaring
site used to review hard-collision behavior; `--semantic` emits the committed eight-column
capability format through a closed reduction that rejects unreviewed declaring sites.

## Dependencies

The runtime uses `lodestone-model`'s base-dimension value and generated static tables.
Generation relies on complete live server captures and official registry reports. The client
consumes dimensions through the version adapter and capabilities during metadata decoding and
movement candidate selection; protocol registry translation remains outside this census.
