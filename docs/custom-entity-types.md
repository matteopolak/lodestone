# Custom entity types

## What it is

A registry mapping a plugin's own entity kind (`myplugin:sentry`) to the vanilla entity type
it is **disguised as on the wire** (`minecraft:armor_stand`).

## Why disguises, and not a new wire type

`add_entity` carries the entity type as a network **registry index** into a 158-entry table.
There is no room in the protocol for a novel type, and *vanilla itself has no such
mechanism* — which is why real Paper servers implement custom mobs as a vanilla entity with
custom NBT, a custom name and custom AI, not as a new registry entry. That framing is
correct.

So a custom entity kind is a **logical identity** on the server plus a **vanilla type** on
the wire, and `lodestone_data::entity_disguise` is the mapping between them.

## The trap this closes

`crates/protocol/v770/src/server_protocol.rs`'s `encode_add_entity_body`:

```rust,ignore
let type_id = entity_type_id(&entity.entity_type.to_string()).unwrap_or(0);
```

**Network type id `0` is `minecraft:acacia_boat`** — asserted by
`network_entity_type_id_zero_is_the_acacia_boat` rather than taken on trust. So an entity
type the table does not know (a typo, or exactly the plugin-namespaced key this issue is
about) streams as an acacia boat with **no error, no warning and no log line anywhere**. The
client renders a boat, the server thinks it spawned a sentry, and neither process reports a
problem.

**The fix is not a smarter fallback. It is moving the failure off the wire and onto the call
that caused it:**

- `EntityDisguises::register` resolves the vanilla target to a wire id **immediately** and
  returns `UnknownVanillaType` if it cannot. A disguise that *would have* streamed as a boat
  cannot be registered at all. The error message names `acacia_boat` explicitly, so a plugin
  author does not need to know the table to understand what went wrong.
- `EntityDisguises::resolve_wire_id` is the safe replacement for that `unwrap_or(0)` line. It
  returns `Option<i32>` and **never** substitutes a default.

**Never add a fallback to `resolve_wire_id`.** Its whole value is that it has none. If a
caller needs a default, that caller states it, somewhere a reader can see the decision.

## How it works

Resolution order in both `resolve_wire_id` and `resolve_name`:

1. a real vanilla type resolves to itself;
2. a registered custom kind resolves to its disguise;
3. anything else is `None`.

Two namespace rules, both enforced at registration:

| rule | why |
|---|---|
| the custom kind must **not** be `minecraft:` | it would shadow a real vanilla type, and every render-side lookup keyed off the vanilla registry would then disagree with this one |
| the target **must** resolve in the 158-entry table | otherwise it is the acacia boat, silently |

A bare `"sentry"` with no namespace counts as `minecraft:` for the first rule, matching
vanilla's own default, so it cannot slip past the check.

`resolve_name` exists for the **client** half: a client-only cosmetic entity needs a vanilla
key to pick a mesh, texture and `setupAnim` for, because every render-side lookup
(`docs/entity-rendering.md`) is keyed off the closed vanilla set.

## Why it lives in `lodestone-data`

It is the **only** crate both the client and `lodestone-server` can reach.
`lodestone-server` deliberately does not depend on `lodestone-ecs` or `lodestone-game` — its
`Cargo.toml` says so and names the two costs (dragging the client vocabulary in, and the
browser bundle) — and the server is where a spawn actually needs a type id. A registry in
`lodestone-game` would have been unreachable from the half that needs it.

## How to change it

- **Entity metadata indices are not hand-countable.** A disguise that also sends metadata (an
  armour stand's `DATA_CLIENT_FLAGS`, a creeper's swell) must take its index from
  `EntityDataIndexOracle.java`'s dump. Index 15 is `Mob`'s flags **and**
  `ArmorStand.DATA_CLIENT_FLAGS`; index 8 is `LivingEntity.DATA_LIVING_ENTITY_FLAGS` **and**
  `AbstractArrow.ID_FLAGS`. Which guard separates the real claimants depends on which classes
  collide, so the census column has to be chosen per collision — assuming the previous
  collision's guard generalises is how the armour-stand bug would have shipped.
- The lookup is a `BTreeMap` keyed by the joined `namespace:path` string, matching
  `entity_types`' own table representation and avoiding a `ResourceKey` dependency.

## Configuration

None.

## What is verified, and the controls

8 tests plus a doctest. The gate asserts the **streamed type id**, not that an entity
arrived, exactly as the trap requires: `an_unregistered_custom_kind_resolves_to_none_and_never_to_the_boat`
checks both `== None` **and** `!= Some(0)` for four different unknown names.

Controls, run and observed:

| control | asserts |
|---|---|
| `control_a_vanilla_type_still_resolves_to_its_own_id` | the resolver is not a function that always returns `None` |
| `unregistering_makes_the_kind_unstreamable_again` | remove the registration, the effect vanishes — and does **not** degrade to a boat |
| `a_disguise_targeting_an_unknown_vanilla_type_is_refused_at_registration` | the failure really moved to registration time |
| `network_entity_type_id_zero_is_the_acacia_boat` | the premise every doc here rests on |

That last one is the one to keep: if the generated table's first entry ever changes, the
`unwrap_or(0)` fallback becomes a silent *something else* and every explanation here would be
wrong.

## Dependencies

`lodestone_data::entity_types` only. No protocol crate, no ECS.

## Not wired yet — the two brokered patches

**This registry has no production consumer.** By `CLAUDE.md`'s own rule that is a defect
report, not a status update, so it is stated plainly here. Two patches are needed and neither
file was this work's to touch (`crates/protocol/v770` and `crates/lodestone-server` both had
live concurrent authors).

### 1. `crates/protocol/v770/src/server_protocol.rs` — stop the silent fallback

`encode_add_entity_body` needs the disguise-aware resolution, and `ServerProtocol` needs a way
to see the registry. The minimal change that removes the silent failure *without* threading
state, as a first step:

```rust
// crates/protocol/v770/src/server_protocol.rs, in encode_add_entity_body
let Some(type_id) = entity_type_id(&entity.entity_type.to_string()) else {
    // Was `.unwrap_or(0)`, i.e. `minecraft:acacia_boat`, silently. An
    // unknown type must not be streamed as a different entity: refuse the
    // spawn and say so. `EntityDisguises::resolve_wire_id` is the seam a
    // caller uses to make a plugin type resolvable in the first place.
    tracing::warn!(
        entity_type = %entity.entity_type,
        entity_id = entity.id,
        "refusing to stream an entity whose type is not in the registry; \
         register a disguise (lodestone_data::entity_disguise) if this is a \
         plugin-defined kind"
    );
    return Vec::new();
};
```

**and** at the one production call site, `crates/lodestone-server/src/server.rs`'s
`EntityStreamer::sync`, an empty body must be treated as "send nothing" rather than as a
zero-length packet. `encode_add_entity` returning `ServerDirective::None` is the cleaner
shape:

```rust
fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
    match encode_add_entity_body(entity) {
        Some(payload) => ServerDirective::Send {
            packet_id: play::clientbound::ADD_ENTITY,
            payload,
        },
        None => ServerDirective::None,
    }
}
```

### 2. `crates/lodestone-server` — resolve before the snapshot leaves

`EntitySnapshot::entity_type` (`protocol.rs`) is the whole channel, and nothing between
`SimMob::snapshot` (`mobs/mod.rs`) and the encoder validates it. A disguise must be applied
**before** the snapshot leaves the server, in `MobSim::snapshots`, so the wire only ever sees
vanilla keys:

```rust
// crates/lodestone-server/src/mobs.rs, in MobSim::snapshots
for snapshot in &mut out {
    if let Some(vanilla) = self.disguises.resolve_name(&snapshot.entity_type.to_string()) {
        snapshot.entity_type = vanilla.parse().expect("a table key parses");
    }
}
```

with `MobSim` gaining a `disguises: EntityDisguises` field and a
`set_disguises`/`disguises_mut` accessor. `SimMob::set_entity_type` (`mobs/mod.rs`) already
exists, so a per-entity override is available today as a cruder alternative.

**Until both land, a plugin can define and validate a custom entity type but cannot spawn
one** — which is also blocked on the server-side spawn API not existing (there is no
`MobSim::remove_mob` and `IntegratedServer` hands out no `MobHandle`).
