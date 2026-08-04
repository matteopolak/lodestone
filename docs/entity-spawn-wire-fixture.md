# The `v340`/`v735` `tests/entity.rs` spawn-packet fixture

## What it is

`lodestone-testsupport::EntitySpawnWireFixture` is a small data table that replaces the
hand-written wire bytes in `crates/protocol/v340/tests/entity.rs` and
`crates/protocol/v735/tests/entity.rs`'s `spawn_entity`/`spawn_entity_living` tests. The two
test files are structurally identical hermetic seam tests through
`VersionAdapter::handle_packet`; comment-stripped, the only difference between them is six
literals, not a code-shape difference — so this is a fixture table, not a templated test body.

## How it works

```rust
pub struct EntitySpawnWireFixture {
    pub creeper_id: i32,
    pub arrow_id: i32,
    pub boat_id: i32,
    pub object_type_id_is_byte: bool,
    pub metadata_terminator: Option<u8>,
}
```

with three methods: `write_mob_type_id` (always a varint — `spawn_entity_living`'s mob-type
field is a varint in *both* families), `write_object_type_id` (a signed byte pre-1.13, a varint
from 1.13 on — this is the one place the width actually differs), and
`write_metadata_terminator` (writes the fixture's byte, or nothing).

Each test file defines its own six-literal `const FIXTURE`:

```rust
// v340 (1.12.2)
const FIXTURE: EntitySpawnWireFixture = EntitySpawnWireFixture {
    creeper_id: 50, arrow_id: 60, boat_id: 1,
    object_type_id_is_byte: true, metadata_terminator: Some(0xFF),
};

// v735 (1.16.5, speaks protocol 754)
const FIXTURE: EntitySpawnWireFixture = EntitySpawnWireFixture {
    creeper_id: 12, arrow_id: 2, boat_id: 6,
    object_type_id_is_byte: false, metadata_terminator: None,
};
```

The 1.13 metadata rewrite dropped the `0xFF` terminator sentinel, and the same release widened
the object-type field from a byte to a varint — both captured directly in the fixture rather
than as a magic version-number branch in the test body. The test bodies themselves — dispatch,
assertions, error cases — are untouched; only the previously hand-written byte layout is now
expressed through the fixture's methods.

`v47`'s `tests/entity.rs` deliberately does not use this fixture (its own doc comment explains
why it stays independent of `v340`), and `v770`'s entity wire shape is unrelated to either
family, so neither participates here. Only `v340` and `v735` do.

## How to change it

Only extend `EntitySpawnWireFixture` for another literal-shaped or bool-shaped divergence. A
protocol-architecture survey measured the comment-stripped, version-normalized cross-family
diff for several `tests/*.rs` files (`v340` vs `v735`): `entity.rs` was 22 differing lines out
of 363 — the best ratio, and the one this fixture handles. `interaction.rs` was 94 differing
lines out of 403; forcing a fixture over a diff that size would be a worse trade than the
duplication it replaces, because at that point the difference is a genuine code-shape
divergence, not a handful of literals. Don't widen this fixture to cover a file where the diff
is that large — write an independent test file instead, the way `v47`'s `tests/entity.rs`
already does relative to `v340`.

## Configuration

None.

## Dependencies

`lodestone-testsupport` gained one new dependency, `lodestone-core` (for `Writer`), added to
`crates/lodestone-testsupport/Cargo.toml`. `lodestone-testsupport` was already a dev-dependency
of all four protocol version crates, so no version crate's `Cargo.toml` needed a new edge.
