# Entity rendering

## What it is

The path from "the server says there is an entity at (x, y, z)" to a posed, textured, lit mob (or sprite, or nametag) on screen, plus two systems that ride on the same entity data: picking (what the crosshair targets) and pose-dependent collision dimensions (crouch/swim box sizing).

## How it works

### Type path → model → texture

The jar-derived dimensions table takes a validated
`lodestone_data::entity_type::EntityType`, never an unchecked registry integer.
The wire adapter and render-side type-path consumers resolve their external id or
path before that lookup; an unknown or plugin type remains a miss and follows
the caller's existing fallback instead of borrowing a built-in hitbox.

Display-entity billboard metadata crosses the version seam as
`lodestone_model::BillboardMode`, so the ECS and renderer cannot confuse its
four semantic modes with an arbitrary byte. The version adapter performs the
only ordinal conversion and maps unknown values to `Fixed`; the renderer
re-exports the model type for callers that already use
`lodestone_render::display::BillboardMode`.

`canonical_model_name(type_path)` maps a registry path to a `lodestone_assets::entity_models` corpus entry. **The corpus is the source of truth**: a type path that *is* a corpus entry name resolves directly; only a few need an explicit alias (`player`/`mannequin` → `player_wide`, `bogged` → `skeleton`, pending its own mesh). Gotcha: an alias onto a nearby mesh must be *written down* the moment the real mesh lands, or it silently survives as a wrong-but-plausible mob once the real one is ported.

`entity_texture_candidates(model_name)` returns in-jar paths in priority order, **derived from each corpus entry's own `EntityTexture`**, never hand-listed. A flat-hue fallback means the sheet wasn't found; the *wrong* mob means resolution picked the wrong entry — different bugs.

**Variant → texture** (wolf breed, pig climate) is a second axis, `EntityTexture::ByVariant` + `resolve(variant)`. Only wolf breed and pig/cow/chicken climate are wired; horse colour, llama, cat, parrot and mooshroom have corpus entries but no variant axis. Gotcha: a resolver can be fully implemented, tested and wire-reachable and still have **zero production callers**, because every call site asks for `default_path()` instead — grep for "what reads this", not "is every assignment the same constant".

### Pose

Arm-swing requests are decoded into `lodestone_model::Hand` at every hosted
protocol boundary. The integrated server's broadcast log retains that type
until its final animation-byte encoding, so an arbitrary integer cannot become
an arm selection inside gameplay code; protocols that predate the off hand
produce `Hand::Main` directly.

`AnimFamily::classify` picks a `setupAnim` from a model's **part names**, not its type name — a quadruped is whatever has `right_hind_leg`/`left_front_leg` — keeping a version-specific mob list out of a version-free crate. It cannot express a vanilla **subclass override** on an identical skeleton (a zombie's arms on a player's rig); `HumanoidArms` is a second table keyed on model name for that, never a branch inside the classifier.

**Creeper swell is a scale about the root part, not a pose** — vanilla wraps it around the whole model *before* the ground-lift translate, so it must be conjugated as `T(+1.501) ∘ S ∘ T(-1.501)` or the creeper sinks into the floor as it grows:

```text
wobble = 1 + sin(swell * 100) * swell * 0.01
s      = (1 + clamp(swell,0,1)^4 * 0.4) * wobble   // x and z
hs     = (1 + clamp(swell,0,1)^4 * 0.1) / wobble   // y
```

The quartic term dominates (+40% wide, +10% tall at full swell); the sine is a ±1% shudder. Gotcha: a render field whose correct rest value is `0.0` is trivially easy to leave wired to nothing — every unit test passes and no frame looks wrong, since the identity default is perfect camouflage for a missing caller. Gate the **caller**, not the formula.

Walk cycle samples the drawn (interpolated) position once per 20 Hz tick, not a fresh network snapshot, which opens a gap of `INTERP_STEPS` (3×) and over-swings the legs.

### Shading: light, colour space, fog

Final pixel is `texel × diffuse × light_term`, faded toward fog by distance.

| what | rule | gotcha |
|---|---|---|
| diffuse | vanilla's **two** lights, `min(1, (max(dot(n,L0),0)+max(dot(n,L1),0))*0.6+0.4)`, `L0=(0.2,1,-0.7)`, `L1=(-0.2,1,0.7)` normalised | a single `abs()`-folded light lights backfaces as brightly as forward ones, with a whole great-circle of normals pinned at the ambient floor |
| normal | derivatives of **model-local** (not world) position, negated | a world-space varying quantises at the `f32` ULP far from the origin, so its derivative is speckle; a sign error is invisible on axis-aligned faces (lights mirror there), so gate shading *by location*, not by "the set of shades matches" |
| world light | per-**instance** (`EntityInstanceRaw::light`) | vanilla samples the lightmap once per entity; can't live on the vertex buffer, shared across every instance of a model |
| light probe | the entity's **eye**, not feet | a tall mob with its head in a lit cell is lit *by its head* |
| fire | forces only the **block** half of light to 15 | forcing the whole byte gives a burning mob in a dark cave a daytime sky |
| night darkening | **client-side only** — a server's sky-light array is time-invariant; scales only the sky half (`1.0` noon, `0.24` midnight) | scaling the whole `light_term` blackens every torch-lit interior at sunset |
| eye height | per registered type (102 of 158 override the `height*0.85` default) | most overrides floor into the default's block cell at integer `y`, so a wrong table still looks right; test with one that crosses a cell boundary (`elder_guardian`, `ghast`) |
| colour space | tint/shade multiply in **gamma** space | linear-space multiply pulls every factor toward 1.0 and washes the image out |
| texture format | sheet must be `_srgb` | plain `Rgba8Unorm` plus the sRGB swapchain double-encodes, roughly doubling brightness — invisible on a non-sRGB test target |
| fog | shared camera uniform, byte-compatible with the block shader's | keeps the pass inside the model shader's 4-bind-group floor |

Pose (crouch/swim eye height) and baby dimensions aren't modelled in the eye-height table — the age-scale approximation lands in the right block cell but not the exact number.

Entities draw **after opaque terrain, before the translucent water pass** (the fluid pipeline doesn't write depth, so a mob drawn after water passes the depth test against the sea floor and paints opaque colour over the water surface at any depth, if the order is wrong).

### Render layers: a second mesh posed off the wearer's own parts

Shared pattern with humanoid armour: a second, independently-baked mesh posed off the wearer's own already-animated part matrices, matched by part *name*, never a second skeleton. Sheep wool is the worked example: `WoolMesh::attach` gates on the wearer's **resolved model name**, not `AnimFamily` — every quadruped (pig, cow, wolf) shares sheep's exact part names, so gating on family alone would draw a fleece on a pig. Skipped when sheared, at the draw site, so decoded data stays honest about what the wire reported.

Gotcha for this class of field: vanilla's `SynchedEntityData` only puts a metadata field on the wire when it differs from the accessor's default, so an ordinary white unsheared sheep's wool byte (default `0`) **never appears on the wire at all**. The fix belongs at spawn (synthesizing vanilla's idle default once), never inside the raw decoder, which must stay a pure function of "what did this packet say" or it will reset an already-dyed sheep to white on every unrelated later packet.

Other vanilla layers of the same shape (wolf collar, charged-creeper aura, iron golem cracks, llama decor, horse markings/armour, mooshroom mushrooms, glowing eyes on enderman/spider/blaze) are surveyed but not landed.

### Sprite-rendered entities

Types with no cuboid rig — must stay absent from the model corpus:

| type | draws |
|---|---|
| `dragon_fireball` | one camera-facing quad, 2× scale, full-bright |
| `fishing_bobber` | one camera-facing quad, 0.5× scale, plus a sagging line to the caster's hand |
| `ominous_item_spawner` | the carried item, grown in over 50 ticks, spinning 40°/tick |

Both quads share one baked mesh and the base entity pipeline. **Never recover a row's index by pointer identity** — the sprite table is a `const`, inlined at every use site, so `std::ptr::eq` against a returned reference can match nothing even though everything else is correct; index by value.

The fishing line reuses the debug-line renderer (a screen-space ribbon, not a raw `LineList`, nearly invisible at real resolution) with a quadratic sag (midpoint at 0.375 of the rise, not 0.5). Anchor resolution needs no local entity id: owner found by wire id → third-person branch on that entity; not found but a synthetic local-player draw exists → our own body; neither → first person, camera is the anchor — the lookup miss itself means "this is us".

The ominous spawner needed no protocol work — metadata routes `ITEM_STACK` fields by serializer, so `EntityDraw::item` was already populated; the whole feature was a missing draw arm with its own pose (no bob, no hover, a different spin rate than the dropped-item pose).

### Nametags

Two vanilla resolution rules, applied once:

- **A player's tag is always its tab-list display name.** UUID-keyed rows are looked up by the entity UUID. Protocol 5 instead preserves the profile name carried beside the UUID in `named_entity_spawn` as `PlayerProfileName`, then uses an exact match on that wire-authored name to find the name-keyed row. It never derives one identity from the other. A server can decorate or truncate the separate player-list name so the two names do not match; that case is unresolvable from protocol 5's wire data and draws no player tag rather than guessing. A resolved name must also survive a tab-list entry that's since been removed (a server-spawned fake-player NPC commonly adds then removes one), via a per-UUID last-known-name cache, the name-side twin of the skin cache.
- **Every other entity's tag is `CUSTOM_NAME`, gated on `CUSTOM_NAME_VISIBLE`.** No fallback to a translated type name.

Both resolve to one `NameTag { text, see_through }`, gated further by the target's team `name_tag_visibility` rule; `see_through` is sneaking, and suppresses the depth-testless pass.

Style (colour incl. hex, bold, italic, underline, strikethrough) walks a real `Text`/`TextSpan` tree, no legacy-string bridge, so hex colour survives. Bold redraws the glyph offset (not a font weight) and widens the advance. **No drop shadow** (vanilla passes `drawShadow = false` here). **`§k` (obfuscated) is not implemented** — needs per-frame resample state this renderer doesn't keep.

`wgpu` has no equivalent of "this pipeline ignores the pass's depth attachment" while sharing a pass that has one (found via a validation error, not reasoned out in advance), so the see-through pass substitutes `Always` + no depth write for vanilla's "no depth attachment at all".

The plate (background rect) has asymmetric one-pixel padding, left/top only — symmetric padding is a plausible-looking wrong port — and needs no z-offset (a billboard is planar to the view axis, so draw order, not depth, separates plate from glyph). It's black at `0.25` opacity in vanilla's *gamma* space: drawing into an sRGB swapchain view instead blends in linear light and reads too weak against a bright backdrop, fixed by drawing world-text passes into a raw non-sRGB view rather than tuning the constant. The normal and see-through passes each submit a different colour/background/plate combination, so both must be read together or the composite silently loses the plate or the sneaking-tag alpha.

Distance cutoff is 64 blocks squared, camera to the entity's **feet**, not the tag anchor; anchor height comes from the jar-derived per-type hitbox census. Per-type attachment overrides (sitting cat, sleeping villager) aren't ported — every entity uses the generic fallback.

### Entity picking

One ray per frame from the interpolated camera: blocks first, then entities capped by the block-hit distance, narrowed by four filters — a cheap distance pre-filter, the `CAN_BE_PICKED` predicate below, a hitbox lookup that drops any type the census can't size, then the exact ray-vs-AABB test capped at `ENTITY_REACH` (3.0) and by the block-hit distance. The local player is never a candidate by construction.

`CAN_BE_PICKED` exists because of a specific server kick: continuing to attack a mob that just died lands the next click on its dropped item or XP orb, and the server disconnects for *"Attempting to attack an invalid entity"* if the resolved target is an item, orb, the player, or a non-attackable arrow. Reduction by declaring class (default `false` — a denylist would risk a forgettable new type shipping pickable and a real kick):

| declaring class | rule |
|---|---|
| `LivingEntity` | pickable unless removed |
| boat/minecart/falling-block/TNT, hanging/end-crystal/interaction/shulker-bullet | always pickable |
| `Projectile` | only if tagged `redirectable_projectile` (fireball, wind charge — **no arrow type qualifies**) |
| `Player`, `ArmorStand` | treated as living (spectator/marker-stand state not modelled — a harmless server no-op, not a disconnect) |
| `EnderDragon`, default `Entity` | never pickable |

### Pose dimensions (collision box)

The player's collision box (`0.6×1.8` standing, `0.6×1.5` crouching, `0.6×0.6` swimming/gliding) is a **fit-gated state machine**, not a lookup: the desired pose (priority `SLEEPING > SWIMMING > FALL_FLYING > SPIN_ATTACK > CROUCHING/STANDING`, off the raw shift key, not a derived flag) is **vetoed, never simply applied** — it must fit, else fall back to `CROUCHING`, else `SWIMMING`. If even the smallest (swimming) box doesn't fit, the pose is **sticky** — no write at all, keeping whatever it already was ("shrink to whatever fits" has it backwards). There is **no recovery** if a box later grows into a space it no longer fits, so the fit gate is the *only* thing stopping a surfacing swimmer clipping into a low ceiling.

A pose changes exactly two numbers, **box height and eye height**, anchored at the feet — one coupled record that must not be split (a standing eye height on a swimming box reports "not submerged" underwater, since the fluid sweep is bounded by the box). The pose is decided *after* the tick's movement (gates next tick, not this one); any entity-push step runs *before* the pose decision within one tick. The entity-collision half of the fit test is vacuously true for an ordinary living entity — only boats, shulkers and the happy ghast override `canBeCollidedWith`.

Gotchas: `(double)0.6F != 0.6` — pose heights are widened `f32` literals, build boxes from the pose table, never hand-typed decimals. A 1.5-block gap is a flush fit and is the real "crouch under a slab" case, not a rounding fluke. A new pose must respect `SLEEPING` being checked *first* in priority order — sleeping must be tested before crouching or a sleeping player will crouch instead. `eye_height` fields elsewhere in the stack are output mirrors of the pose, never an independent input.

## How to change it

* **New mob ported**: add the `EntityModelEntry` to the corpus; nothing else needs touching. Alias only for another mob's model *class*; extend `HumanoidArms` only for a subclass animation override on an identical skeleton, never a branch in `AnimFamily::classify`.
* **A mob looks too bright/dark**: check, in order, texture format (`_srgb`?), the shader's `light_term`, the sky-darken factor, and which side of the gamma curve the multiply happens on — independent, and indistinguishable on a non-sRGB render target, so measure on a real one.
* **Wiring real world light / sky darkening**: both ride a source function installed at connect time, on *every* connect path; until installed, mobs render full-bright / permanent-noon. Terrain does not yet read the sky-darken lane, though the shared uniform already carries it.
* **Adding a picking filter**: goes ahead of the hitbox lookup. Keep the table a default-deny allowlist, never a denylist.
* **Adding a pose**: extend the pose table with vanilla's real dimensions and check `getDesiredPose`'s priority order before wiring the input.
* **Entity metadata indices are reused across unrelated classes** — always run the metadata index oracle rather than hand-counting when adding a new decoded field; a class guard (not a bare index check) is what keeps two mobs' same-index fields from colliding.

## Configuration

None. `LODESTONE_ASSETS` (or a discovered `.cache/mc/<version>/`) is the pack root; absent, every mob falls back to a synthetic flat colour and every sprite/nametag/shadow pass draws nothing. `ENTITY_REACH` (3.0) and `REACH` (4.5) are constants matching vanilla's default interaction ranges. The `entityShadows` video option gates the shadow pass outright.

## Dependencies

`lodestone-assets` (model/texture corpus, font data), `lodestone-data` (jar-derived dimension/eye-height/collision census), `lodestone-physics` (pose dimensions, quantized `mth` sin/cos — never `f32::sin`/`cos`, which diverges from vanilla's table at cardinal angles), `lodestone-ecs` (per-entity metadata components), `lodestone-render`/`entity_pipeline.rs` (meshes, pipelines), `lodestone-shell`'s `gpu/entity_passes.rs`, `gpu/nametag.rs` and `sim/` (extraction, draw-site wiring, picking). See [`camera-and-view.md`](./camera-and-view.md) for the reversed-Z projection every depth-biased pass here assumes.
