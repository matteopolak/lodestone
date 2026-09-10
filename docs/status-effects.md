# Status effects

## What it is

Status effects are the per-player effect instances that the integrated server stores and
ticks, protocol adapters transport, and the shell uses for movement, HUD state, and
effect-driven rendering. The 26.2 built-in registry contains 40 ids; recognizing an id
on the wire is separate from implementing its game rule.

## How it works

`lodestone_server::mob_effects::ActiveEffects` owns duration, stacking, expiry, and the
periodic poison, wither, regeneration, and hunger rules. Commands, potion drinking,
splash impacts, milk, and selected gameplay events write that store. The server encodes
updates and removals with ambient, particle, icon, and blend flags; the 26.2 adapter
validates the registry id and the shell retains each of them in
`lodestone_game::effect::ActiveEffects`. `lodestone_ecs::EntityStatusEffects` keeps a
second, entity-keyed view for continuous particle emission and entity-outline state,
while the local player's `HudEffects` remains the display and renderer source of truth.

The live server store is keyed by validated `lodestone_data::mob_effects::MobEffectId`,
so periodic rules, attribute folds, and presence checks do not compare canonical strings
on every tick. Canonical names are reconstructed only when a protocol encoder or
presentation-facing API explicitly needs text; command and packet input remains textual
at those boundaries and is validated before entering the store.

The client consumes the full effect list in independent ways. The physics/ECS path
uses movement-relevant amplifiers, while interaction prediction applies Haste, Conduit
Power, and Mining Fatigue to digging speed. The HUD and inventory use duration, ambient,
and icon state. The renderer polls a local effect-light source: Night Vision raises the lightmap's
ambient seed by a channelwise maximum with its fading `0x999999` floor, so terrain,
fluids, entities, held items, and world text use the same result. Nausea exposes a
separate screen-effect intensity with its 150-tick entry and 20-tick exit transition;
the wire blend flag selects immediate versus transitioned adoption. A missing or expired
effect returns no light floor or screen intensity and leaves ordinary rendering unchanged.

Blindness and Darkness also supply a frame-polled vision-obscuration strength. Blindness
is opaque until its final 19 ticks, which fade out linearly; Darkness uses a 22-tick
entry and exit transition. The current bounded implementation draws this as a
depth-tested black screen layer. The server also projects Invisibility into the shared
entity-flags byte for every remote player snapshot, so the existing entity-draw consumer
hides them and receives an explicit zero-byte update on expiry or removal.

The entity glow source polls the current effect map and the shared entity-flags byte. An
active `minecraft:glowing` effect or the wire glow bit selects an entity id; the GPU joins
those ids with the current interpolated entity draws and renders a padded, depth-tested
green box outline. Because the source carries no positions, movement interpolation and
effect expiry/removal are reflected on the next frame without stale geometry.

Each game tick the shell samples every entity with one or more visible effects. The
selection rate is reduced for invisible entities and all-ambient sets, then a chosen
effect's generated RGB colour and ambient alpha are emitted as an `entity_effect`
particle. Entity removal, expiry, despawn, and session teardown prune the corresponding
entry so a stale effect cannot keep emitting or outlining.

| surface | implemented effects |
| --- | --- |
| Registry, packet id validation, HUD/inventory icon state | all 40 built-in ids |
| Timed server lifecycle | all applied timed effects; instant health, damage, and saturation resolve through the effect tick |
| Periodic server rules | poison, wither, regeneration, hunger |
| Server combat and environment consumers | instant health/damage, saturation, Health Boost, resistance, absorption, fire resistance, strength, weakness, Water Breathing, Conduit Power, Breath of the Nautilus air handling, Luck/Unluck fishing rolls |
| Client physics consumers | speed, slowness, levitation, slow falling, dolphin's grace, jump boost |
| Client interaction consumers | haste, conduit power, mining fatigue digging-speed modifiers |
| Client render consumers | Night Vision light floor; Nausea screen transition; Blindness/Darkness screen layer; Invisibility entity flag; Glowing entity outline |
| Presentation flags | ambient, icon, visible-particle, and blend state; continuous particles use generated effect colour |

The registry inventory below distinguishes a rule that changes gameplay or pixels from
transport-and-HUD support alone. Every row is still retained, timed, shown when its icon
flag permits, and eligible for the effect-particle path.

| Effect | Current consumer | Status |
| --- | --- | --- |
| Speed, Slowness | Local movement simulation | implemented |
| Haste, Mining Fatigue | Predicted digging speed | implemented; attack animation speed is separate |
| Strength, Weakness | Server player melee damage | implemented |
| Instant Health, Instant Damage, Saturation | Server application and authoritative food state | implemented |
| Jump Boost, Levitation, Slow Falling, Dolphin's Grace | Local movement simulation | implemented |
| Nausea | Screen transition | implemented |
| Regeneration, Hunger, Poison, Wither | Server periodic tick | implemented |
| Resistance, Absorption, Fire Resistance | Server damage and burning paths | implemented |
| Water Breathing, Conduit Power | Server underwater-air rule | implemented; no fog/vision modification |
| Breath of the Nautilus | Server underwater-air hold rule | implemented; preserves, but does not refill, air |
| Invisibility | Remote-player shared entity flags and body draw | implemented; teammate/spectator translucency is not modeled |
| Blindness, Darkness | Black screen layer | implemented with the bounded fog/lightmap limitation below |
| Night Vision | Shared renderer light floor | implemented |
| Glowing | Entity-outline source and draw pass | implemented |
| Bad Omen, Raid Omen | Server raid conversion and start | implemented |
| Hero of the Village | Server villager pricing | implemented |
| Health Boost | Dynamic max-health attribute, health clamp, and multi-row HUD | implemented |
| Luck, Unluck | Server fishing loot-quality roll at retrieval | implemented for fishing; other loot sources have no consumer |
| Trial Omen | No trial-event consumer | unsupported |
| Wind Charged | Server player-death transition, v26 particle packet, and shell small-gust emitter | implemented as a visible death burst; wind knockback and non-player holders remain unsupported |
| Weaving, Oozing | No death-trigger consumer | unsupported |
| Infested | No hurt-trigger consumer | unsupported |

Effects outside those consumers are still transported and displayed but do not yet change
their corresponding simulation or visual rule. Water Breathing and Conduit Power stop
underwater air loss and refill a depleted air bar; Breath of the Nautilus stops air loss
without refilling it. None of those effects currently changes underwater fog or the screen
overlay: no distinct water-vision rule is claimed here. Other notable gaps include the
trial-event effect and the newer death-trigger effects other than Wind Charged. Wind Charged's
player-death transition emits the small-gust particle at the player midpoint through the ordinary
world-effect wire path; it intentionally does not claim the separate nearby-entity wind impulse,
and the current server does not yet store status effects on simulated mobs. Luck and Unluck are folded at
fishing retrieval, but other loot sources do not yet consume that attribute. The bounded Darkness layer
does not yet alter fog distance or the lightmap, and attack-speed animation remains
separate from digging speed. Those effects retain their presentation state and can emit
particles, but their distinct game or render rules remain separate work.

## How to change it

Add a server rule to `lodestone_server::mob_effects` only when its consumer can run from
the shared `ActiveEffects` store; do not create a second effect timer. A new protocol family
must validate its wire id at the `lodestone_data::mob_effects::MobEffectId` boundary and
preserve every presentation flag. A new renderer effect should supply a local,
frame-polled input through `RenderState`'s effect-light seam instead of adding a separate
terrain, entity, or hand implementation. Entity outlines should pass ids only; positions
and dimensions belong to the current render-frame draw slice. Keep outline geometry
bounded and use the shared depth-tested line pipeline so walls occlude it consistently
with the entity.

When extending particle emission, read `StatusEffect::show_particles` at the source;
do not infer visibility from icon or ambient state. Preserve the entity-keyed state until
the entity is removed from the live index. A remote-player visual rule belongs in
`PlayerRegistry` shared flags and the version encoder, then needs a test covering both the
wire bytes and the real entity-stream directive. Keep a production-consumer test that
changes an observable movement, health, lightmap, screen transition, particle source, or
metadata stream rather than only asserting that an effect was stored.

Health Boost must update `PlayerVitals` and publish the folded
`minecraft:max_health` attribute together with the current-health frame whenever
the active instance changes or expires. The shell already ingests attribute updates;
keep the HUD row count sourced from that value, never from current health, because a
hurt boosted player can be below 20 health while still requiring the second row.

Luck and Unluck share one signed attribute fold. The fishing consumer samples that
value when a bite is retrieved, rather than when the bobber is cast, so expiry or a
replacement during a cast changes the eventual roll. Keep rod enchantment luck and
the player attribute separate until that retrieval point; other loot systems need an
equivalent explicit consumer before they can claim Luck support.

Death-trigger effects must be selected from `ActiveEffects` at
`lodestone_server::server::publish_health`, the zero-health transition shared by every
player damage source. Do not fire them from the periodic-effect timer: that would miss falls,
commands, starvation, and other non-effect deaths, or make expiry timing independent of the
stored instance. `lodestone_server::effects::wind_charged_death` is a normal simple-particle
world effect, so extending its visible path means preserving the version encoder's no-options
particle constraint and the shell particle emitter. A future wind-impulse implementation needs a
separate authoritative player/mob physics consumer; do not route it through damaging explosions.

## Configuration

There is no status-effect configuration. The fixed 26.2 registry census and effect colours
come from generated data. Night Vision uses the shipped default lightmap colour; custom
per-environment Night Vision colours are not yet synchronized into the renderer.

## Dependencies

This subsystem depends on `lodestone_data::mob_effects` for the registry boundary,
`lodestone_server::mob_effects` for server lifecycle, version adapters for packet mapping,
`lodestone_game::effect` for client state, `lodestone_ecs::EntityStatusEffects` for
entity-scoped effect state, `lodestone_render::light` for the shared lightmap rule, and
the shell GPU debug-line pipeline for bounded entity outline geometry.
