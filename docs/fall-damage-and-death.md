# Fall damage and death

## What it is

The server-side half of the damage→death→respawn loop: `FallTracker`
(`crates/lodestone-server/src/fall.rs`) accumulates fall distance from the
positions a client reports and turns a landing into damage, `PlayerVitals`
(`vitals.rs`) holds the health it comes off, and `server.rs` reports the result —
including the `player_combat_kill` packet that actually raises the client's death
screen, and the `respawn` packet that closes it again.

## How it works

Movement is client-authoritative here, so there is no server-side physics tick to
read `fallDistance` off. Everything is driven by inbound packets:

```text
ServerBound::PlayerMoved / PlayerRotated / PlayerStatusOnly
  └─ fall_sample(source, x, y, z, on_ground)   -- reads two world cells
       └─ FallTracker::on_player_moved(FallSample) -> Option<i32> damage
            └─ PlayerVitals::apply_fall_damage(raw) -> Option<f32> dealt
                 └─ publish_health(.., DeathCause::Fall)
                      ├─ encode_set_health(health)
                      └─ encode_player_combat_kill(id, msg)   -- only if health == 0
```

and the way back out:

```text
ServerBound::ClientCommand { action: 0 }   -- the Respawn button
  └─ apply_client_command, guarded on health <= 0.0
       ├─ PlayerVitals::respawn()
       ├─ encode_respawn(world_spawn)  -- respawn record + placement teleport
       ├─ encode_set_health / encode_air_supply_update
       └─ FallTracker::reset()
```

`FallSample` carries the three facts the tracker cannot derive itself, each named
for the vanilla expression it stands for: `in_water`, `fall_resetting`, and
`block_damage_modifier`. `fall_sample` in `server.rs` fills them from the terrain.

The damage formula is vanilla's exactly:
`floor((distance + 1e-6 - SAFE_FALL_DISTANCE) * blockModifier * fallDamageMultiplier)`,
applied only when positive.

## How to change it, and the gotchas

**`set_health(0.0)` does not raise a death screen.** Not here and not in vanilla:
`ClientPacketListener.handleSetHealth` calls only `hurtTo`/`setFoodLevel`/
`setSaturation`, and the screen comes from `handlePlayerCombatKill`. A server that
applies lethal damage and sends health alone leaves the player pinned at zero
hearts with no screen and no respawn button — which reads as the server having
hung. **Route every damage site through `publish_health`**; that is what makes the
death announcement impossible to forget.

**`encode_respawn` is not optional either, and omitting it fails worse.** The
client clears its `Dead` marker only on `ClientEvent::Respawned`, decoded from
`respawn` (id 82). Answering `perform_respawn` with reset vitals and
`set_health(20.0)` refills the hearts *behind* a screen that never closes.
Sending health without the respawn packet is strictly worse than sending neither,
because it looks like it worked — hence the gate asserts the **ordering**, not
just the presence.

**There is no death-announced latch, and that is a property of `PlayerVitals`'
guards rather than of this code.** Every `apply_*` returns `None` once
`health <= 0.0`, so a landed hit crosses zero exactly once per life. If you add a
damage path that does not carry that guard, a real client will rebuild its death
screen on every movement packet. `death_is_announced_exactly_once_per_life` is the
gate.

**`reset` and `cancel` are different operations. Do not use one for the other.**

| method | vanilla | zeroes distance | drops `last_y` | for |
|---|---|---|---|---|
| `cancel` | `resetFallDistance()` | yes | no | mid-flight cancellation (water, climbable) |
| `reset` | teleport (`Entity.java:2897`, `:2946`) | yes | **yes** | a position snap (respawn) |

Using `cancel` for a teleport keeps the y the player was snapped away from as the
reference, so a death at y=70 and a respawn at y=64 banks six blocks of fall
nobody fell. Using `reset` for a water cancellation under-counts by one tick,
which is harmless.

**Lava does not cancel a fall.** `checkFallDamage`'s guard is `isInWater()` and
`updateFluidInteraction` resets only `if (inWater)`. The tempting generalisation —
"any fluid cancels" — makes a lava dive a safe landing and passes every water
test. `fall_sample` uses `crate::chunk::is_water` for exactly this reason;
`lava_is_not_water_and_does_not_cancel_a_fall` is what stops it widening.

**Water needs *two* rules, and only one of them is in `checkFallDamage`.** The
guard there suppresses accumulation while submerged; the reset that actually fixes
a banked fall is a separate site, `Entity.updateFluidInteraction:1658`'s
`if (inWater) resetFallDistance()`. An implementation with only the guard still
charges the next dry landing — which is the bug that was shipped. **When porting a
`fallDistance` rule, grep for `resetFallDistance()` across `Entity.java` and
`LivingEntity.java` rather than reasoning from `checkFallDamage` alone.**

**Powder snow is not a modifier.** `HayBlock`/`HoneyBlock` pass `0.2F` and
`SlimeBlock` passes `0.0F` to `causeFallDamage`, but `PowderSnowBlock.fallOn`
plays a sound and never calls it at all. Its drift gate greps for the *absence* of
the call; asserting a modifier for it would be asserting something the jar does
not say.

**`DeathCause::message_id` is not derivable from the variant or the file name.**
`outside_border.json` carries `"message_id": "outsideBorder"` and
`generic_kill.json` carries `genericKill`, camelCase in an otherwise snake_case
directory, while `fall`/`drown`/`generic` match their file names. A wrong key
renders as itself and is indistinguishable from the client's documented
untranslated-message gap. `death_cause_message_ids_match_the_jar_damage_type_records`
reads the JSON at test time.

**Unit tests here are a closed loop around a `FallSample` the test built.** They
cannot see `fall_sample` reading the wrong cell of the world, which is the
likeliest way to ship this broken — the eye cell instead of the feet (a
player-height too late), or `y - 1` instead of `y - 0.2` for the landing block.
`crates/protocol/v770/tests/server_fall_cancellation.rs` is the end-to-end gate,
over terrain containing real water, real hay and a real ladder.

## Configuration

None at runtime. The constants are jar-sourced and named:
`SAFE_FALL_DISTANCE` (`3.0`), `FALL_DAMAGE_MULTIPLIER` (`1.0`),
`DEFAULT_BLOCK_DAMAGE_MODIFIER` (`1.0`), `CUSHIONED_BLOCK_DAMAGE_MODIFIER`
(`0.2`).

## Known gaps

Named here rather than left unfindable; each is a missing *input*, not a missing
rule:

- **Boats and any vehicle** (`LivingEntity.rideTick:3294` resets every tick). No
  vehicle state exists for the player anywhere in this crate, and `ServerBound`
  carries no mount packet.
- **`SLOW_FALLING` / `LEVITATION`** (`LivingEntity.aiStep:3123`). No potion
  effects are tracked for the player.
- **Feather Falling and Resistance.** `apply_fall_damage` routes through
  `lodestone_entity::apply_reductions` with `Defenses::default()`; armour is
  correctly bypassed (fall is `bypasses_armor`-tagged) but no equipment is
  tracked. The pipeline is real and picks these up for free once it is.
- **Pointed dripstone.** Its `2.0F` modifier would fit `FallSample`; its `+2.5`
  additive needs a pre-landing hook this shape does not have. Deliberately omitted
  whole, because shipping only the modifier would make dripstone *safer* than
  plain ground.
- **Per-block friction** for a landed item or player: `block_friction` keeps its
  default, so ice behaves like stone.
- **A landing reported only via `move_player_rot`/`move_player_status_only`** with
  no y change is handled (`fall_status_sample` reuses the remembered y), but a
  landing whose sample carries no `on_ground` edge at all is not observable.

## Dependencies

- `lodestone-entity` — `apply_reductions`, `Defenses`, `DamageFlags`,
  `HurtCooldown`.
- `lodestone-server::chunk` — `is_water` and `ChunkSource::block_state`, the two
  single-cell reads `fall_sample` makes per movement packet.
- `ServerProtocol::encode_player_combat_kill` / `encode_respawn` /
  `encode_set_health`, implemented for protocol 776 in
  `crates/protocol/v770/src/server_protocol.rs`.
- The client half is `docs/death-screen.md`, which was already complete before any
  of this existed.
