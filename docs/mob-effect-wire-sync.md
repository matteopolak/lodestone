# Status-effect wire sync

## What it is

The clientbound `update_mob_effect`/`remove_mob_effect` encoders
(`ServerProtocol::encode_update_mob_effect`/`encode_remove_mob_effect`). Before this, the
integrated server's `ActiveEffects` (`crate::mob_effects`) changed real gameplay state —
`/effect give`, and now a beacon's periodic grant (see [`beacon.md`](./beacon.md)) — but no
client was ever told: the clientbound direction of both packets had zero references anywhere
in `crates/protocol/v770/src/server_protocol.rs`, even though the **decode** side
(`V770Adapter`'s `UPDATE_MOB_EFFECT`/`REMOVE_MOB_EFFECT` arms, `adapter/entity.rs`) already
existed and already emits `ClientEvent::MobEffectApplied`/`MobEffectRemoved`. So an applied
effect changed health/exhaustion/hunger correctly and put no icon on the HUD at all — the
"nothing consumes this" island CLAUDE.md's own evidence section calls out, on the *producer*
side this time rather than the consumer.

## How it works

Two trait methods on `ServerProtocol` (`crates/lodestone-server/src/protocol.rs`), both
defaulting to `ServerDirective::None` the way every optional-per-family encoder in that trait
does, implemented in `crates/protocol/v770/src/server_protocol.rs`:

- `encode_update_mob_effect(entity_id, effect, amplifier, duration_ticks, ambient, visible,
  show_icon, blend)` → `ClientboundUpdateMobEffectPacket`: entity id, the effect's
  `minecraft:mob_effect` registry id (`lodestone_data::mob_effects::mob_effect_id`, the
  reverse of the decode side's `mob_effect_name`), amplifier, duration, then one `u8` bitset
  (`ambient` `0x1`, `visible` `0x2`, `show_icon` `0x4`, `blend` `0x8`) — the exact mirror of
  `adapter/entity.rs`'s own decode arm.
- `encode_remove_mob_effect(entity_id, effect)` → `ClientboundRemoveMobEffectPacket`: entity
  id, registry id.

An effect this crate's registry table cannot resolve (`mob_effect_id` returns `None`)
degrades to `ServerDirective::None` — no packet — rather than writing a bogus id, the same
"an unresolvable id writes nothing rather than corrupting the rest of the packet" convention
`write_item_cost` already uses.

`crate::beacon`'s periodic sweep is the one production caller today; `/effect give`/`/effect
clear` (`server.rs`'s `ApplyEffect`/`ClearEffects` command arms) still only mutate
`ActiveEffects` and do not yet call either encoder — a real, separate gap this pass did not
close, since it needed only the beacon's own effect grant to reach the wire.

## How to change it

Both are ordinary trait methods with `#[allow(clippy::too_many_arguments)]` — if you add a
call site, pass the *entity's* id, not the local player's uuid; every current call site is
self-facing (`LOCAL_PLAYER_ENTITY_ID`), so a remote-entity caller is unverified territory.

## Configuration

None.

## Dependencies

`lodestone_data::mob_effects` (`mob_effect_id`/`mob_effect_name`, the registry census);
`crates/protocol/v770/src/adapter/entity.rs`'s existing decode arms, which this doc's own
round-trip tests (`crates/protocol/v770/tests/beacon_wiring.rs`) decode the encoder's output
through.
