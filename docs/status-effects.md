# Status effects

## What it is

Per-player effect instances that the integrated server stores and ticks, protocol adapters transport, and the shell uses for movement, HUD state and effect-driven rendering. The 26.2 built-in registry has 40 ids; recognising an id on the wire is separate from implementing its game rule.

## How it works

`lodestone_server::mob_effects::ActiveEffects` owns duration, stacking, expiry and the periodic poison, wither, regeneration and hunger rules, keyed by validated `lodestone_data::mob_effects::MobEffectId` (canonical names are rebuilt only for a protocol encoder or presentation API; command and packet input is validated at the boundary). Commands, potion drinking, splash impacts (typed through falloff and instant dispatch), milk and gameplay events write it. Updates and removals go out with ambient, particle, icon and blend flags; the 26.2 adapter validates the id and the shell retains them in `lodestone_game::effect::ActiveEffects`. `lodestone_ecs::EntityStatusEffects` is a second, entity-keyed view for continuous particles and outlines; the local player's `HudEffects` is the display source of truth.

Client consumers:

- Physics/ECS uses movement amplifiers; interaction prediction applies Haste, Conduit Power and Mining Fatigue to digging speed (attack animation speed is separate). HUD and inventory use duration, ambient and icon state.
- Night Vision raises the lightmap's ambient seed by a channelwise maximum with a fading `0x999999` floor, so terrain, fluids, entities, held items and world text share it. A missing or expired effect leaves rendering unchanged.
- Nausea exposes a screen intensity with a 150-tick entry and 20-tick exit; the wire blend flag selects immediate versus transitioned adoption.
- Blindness (opaque until its final 19 ticks, then linear fade) and Darkness (22-tick entry and exit) give a frame-polled obscuration, drawn as a depth-tested black screen layer; it does not alter fog distance or the lightmap.
- The server projects Invisibility into the shared entity-flags byte of every remote player snapshot (with an explicit zero byte on expiry); teammate/spectator translucency is not modelled.
- The glow source polls the effect map and the entity-flags glow bit; the GPU joins those ids with the current interpolated draws and renders a padded depth-tested green box outline. It carries no positions, so interpolation and expiry show next frame.
- Each game tick the shell samples entities with visible effects (rate reduced for invisible or all-ambient sets) and emits an `entity_effect` particle in the generated effect colour. Entity removal, expiry, despawn and teardown prune the entry.

Effect to consumer (every effect is retained, timed, shown when its icon flag permits and particle-eligible):

| Effect | Consumer |
|---|---|
| Speed, Slowness, Jump Boost, Levitation, Slow Falling, Dolphin's Grace | local movement simulation |
| Haste, Mining Fatigue | predicted digging speed |
| Strength, Weakness | server player melee damage |
| Instant Health/Damage, Saturation | server application, authoritative food state |
| Regeneration, Hunger, Poison, Wither | server periodic tick |
| Resistance, Absorption, Fire Resistance | server damage and burning paths |
| Water Breathing, Conduit Power | underwater air: stops loss and refills; no fog or vision change |
| Breath of the Nautilus | holds underwater air, does not refill |
| Health Boost | dynamic max-health attribute, health clamp, multi-row HUD |
| Luck, Unluck | fishing loot-quality roll at retrieval only |
| Bad Omen, Raid Omen | raid conversion and start |
| Hero of the Village | villager pricing |
| Nausea, Blindness, Darkness, Night Vision, Invisibility, Glowing | render consumers above |
| Wind Charged | player-death small-gust particle via the ordinary world-effect wire path; no wind knockback, and mobs hold no status effects yet |
| Trial Omen, Weaving, Oozing, Infested | none (transport and HUD only) |

## How to change it

- Add a server rule to `lodestone_server::mob_effects` only if its consumer runs from the shared `ActiveEffects` store; never a second timer. A new protocol family validates its wire id at the `MobEffectId` boundary and preserves every presentation flag.
- A new renderer effect supplies a local frame-polled input through `RenderState`'s effect-light seam rather than a separate terrain, entity or hand path. Outlines pass ids only (positions belong to the frame's draw slice), stay bounded and use the shared depth-tested line pipeline.
- Particle emission reads `StatusEffect::show_particles` at the source, not icon or ambient state; keep entity-keyed state until the entity leaves the live index.
- A remote-player visual rule belongs in `PlayerRegistry` shared flags and the version encoder, tested on both wire bytes and the real entity-stream directive. Add a production-consumer test that changes an observable (movement, health, lightmap, screen, particle, metadata), not only that an effect was stored.
- Health Boost must update `PlayerVitals` and publish the folded `minecraft:max_health` attribute with the current-health frame on every change or expiry; source the HUD row count from that value, never current health.
- Luck and Unluck share one signed attribute fold, sampled when a bite is retrieved (not cast); keep rod enchantment luck separate until then. Other loot systems need their own consumer.
- Death-trigger effects are chosen from `ActiveEffects` at `lodestone_server::server::health_sync::publish_health`, the zero-health transition shared by every player damage source, never the periodic timer (that would miss falls, commands and starvation). `lodestone_server::effects::wind_charged_death` is a simple-particle world effect, so keep the version encoder's no-options particle constraint. A wind impulse needs its own authoritative physics consumer, not a damaging explosion.

## Configuration

None. The registry census and effect colours come from generated data. Night Vision uses the shipped default lightmap colour; per-environment colours are not synchronised.

## Dependencies

`lodestone_data::mob_effects`, `lodestone_server::mob_effects`, version adapters, `lodestone_game::effect`, `lodestone_ecs::EntityStatusEffects`, `lodestone_render::light`, and the shell GPU debug-line pipeline.
