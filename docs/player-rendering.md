# Player rendering

## What it is

Everything that turns a player's (or player-shaped entity's) identity and pose into pixels: skin and cape textures, the local player's synthetic third-person body, armour and trim layers, armour stand poses and elytra. All share one mechanism, a second mesh posed off the wearer's already-computed part matrices, and one recurring failure: attaching by part name instead of by the wearer's resolved animation family.

## How it works

### Skins

Identity comes from the `textures` profile property: base64, JSON, then structurally parsed URL plus model type. Invalid or relative entries are dropped independently; remote fetch still authorises scheme and host before opening a socket. The model bit is the skin entry's `metadata.model`, and the wide rig's wire value is `"default"`, not `"wide"` (matching `"wide"` always falls back to wide, so Alex's arms are a pixel thick with no crash).

The local and remote players resolve the same way, through the tab-list `ADD_PLAYER` entry for the UUID (`entities::player_skin_for_uuid`, `lodestone-shell/src/remote_skins.rs`), not the login profile fetch. A self-hosted or offline session sends no `textures`, so the default sheet is the normal fallback for no skin, fetch in flight and fetch failed. Fetches are keyed and cached by texture URL (shared skins share one decode and bind group; failures are remembered). The signed-in skin is cached at `<data_dir>/skin.png`, `skin.model`, `skin.uuid`.

The rig and sheet swap together: a slim sheet on the wide rig shifts every arm UV by a texel, so the retained-skin resolver applies the pair or neither. Custom player heads reuse the fetch at several draw sites (world, inventory slot, hand, third-person held item), each with its own resolver and cache.

### Capes

The model-layer byte's cape bit reaches `AnimInput::cape_visible`; an unreported byte keeps the default visible and an explicit clear skips submission before texture lookup. The mesh is posed off the wearer's `body` matrix and sways from a per-tick-lagged position that every tracked entity carries, chasing the true position at a fixed fraction per tick and snapping on a teleport-sized jump. Batched by texture URL. Elytra takes the chest slot and suppresses the cape; both gate on the identical "chest item is literally `elytra`" predicate, and divergence would lose the cape and give no wings.

### Third-person body

The local player has no tracked network entity, so `ThirdPersonBodyState` is bridged to an `EntityDraw` under a reserved id, supplying feet, body yaw, animation input, scale, rig and equipment (both hands, four armour slots). It is appended to the ordinary entity slice and takes the same resolve, cull, pose, upload and held-item path as mobs.

The camera mode (`F5`) has three states but the bridge asks only "is this first person" (asking "is the camera behind me" would bring back the first-person arm and screen overlays in front view). The third-person camera raycasts backward from the eye through real collision geometry. Player skins draw through the translucent entity pipeline with a `0.1` alpha cutout, which is why diamond armour has small shoulder gaps (the sheet is deliberately transparent there).

The body pose and first-person arm pose never share a function: the arm draws in a camera-space pass from an authored rest pose, the body uses the fully animated pose, and one `Option` source makes them exclusive. Check the producer of a per-entity animation field, not just its consumer; the rig-selection flag `slim` is hardcoded rather than read from the resolved skin today.

The swim body-pitch ramp applies to network player draws and the synthetic local draw, whose type path is `player_wide` or `player_slim` (gating on the network `player` path leaves the local avatar upright while the limbs stroke). Swimming and crawling share the prone rotation; the crawl-only nudge still needs an in-water signal the remote record lacks. The shared orientation helper serves body, armour, cape, elytra and held-item paths.

### Armour

An armour piece is a second mesh posed off the wearer's part matrices (`ArmourMesh::attach`, by part name), never a second animation pass and never written back. The attach gate is the wearer's resolved animation family (humanoid: both arms and both legs), never part names: a pig has `head` and `body` parts and would get a floating chestplate. The same applies to wool, capes and elytra.

| slot | parts | inflation |
|---|---|---|
| head | `head` (+`hat` shell) | 1.0 (+1.5 for `hat`, zero pixels on shipped sheets) |
| chest | `body`, both arms | 1.0 |
| legs | `body`, both legs | 0.5 / 0.4 (inner bake, legs 0.1 texel thinner) |
| feet | both legs | 0.9 |

Outer (1.0) and inner (0.5) bakes keep chestplate and leggings from z-fighting on the same torso cube. Sheets are 64x32. An item carries an asset id keying a per-layer texture list (`golden_helmet` maps to `gold`). Dye (leather only) multiplies in gamma space; a dye of exactly `0` reads as undyed, matching the protocol.

Trim is a texture overlay batched by sprite, drawn right after its slot's layers in an ordered list (coplanar depth test), untinted; sprites are baked at load from a greyscale index PNG plus an 8-colour palette strip per material. Slot order is fixed `chest, legs, feet, head`, with a `LessEqual` depth comparison so leather's base and overlay resolve correctly.

### Armour stands

A pose is six synced rotations (head, body, both arms, both legs) plus three derived stick parts. Every armour stand is posed whether or not a pose update arrived: the default (a small authored splay, not zero) applies as soon as the entity is recognised, or the stand animates like a walking humanoid. It sets rotations only, so crouch offset and attack-swing orbit translations survive. Updates mention only changed parts, so the fold merges per part in wire order. Extract gates on entity type (`armor_stand`), not on a pose component being present. The base plate takes the entity counter-rotation, culling uses the transformed vertices, and marker stands anchor names at their feet and are excluded from interaction targeting.

### Elytra

Two mirrored wings posed off `body` on a 64x32 sheet. The authored rotation is overwritten each frame by the pose branch, so the resting angle comes from that branch's target; the right wing negates two of the left's three angles. Draw gate and cape suppression are one predicate. Texture preference: custom elytra sheet, then visible cape sheet, then built-in. `AnimInput` carries fall-flying, crouching and movement state; `elytra_target_rotations` picks the branch (fall-flying beats crouching, descending changes the target). Missing state is the rest default.

## How to change it

- Gate wool, cape, armour and elytra attachment on the animation family, never shared part names.
- Never mutate the wearer's part transforms; read them and compute the layer's.
- Change a skin's rig and texture together; keep cape suppression and elytra draw one predicate.
- When a field looks unwired for the local player, check what supplies it for that caller.
- Never write same-typed field runs (six stand rotations, lean and flap angles) positionally; name them and keep fixtures pairwise distinct.
- Tint and dye multiply in gamma space.
- `#[ignore]`d pixel gates (`armour_pixels.rs`, `elytra_wings_pixels.rs`) need a real `client.jar`.

## Configuration

No feature flags; each surface draws when its data is present.

| knob | effect |
|---|---|
| `<data_dir>/skin.*` (`LODESTONE_DATA_DIR` relocates) | cached skin, model, ownership marker |
| `LODESTONE_ASSETS` or discovered `.cache/mc/<version>/` | must hold `client.jar` or armour, trim and elytra textures are empty |
| remote-texture host allow list and max size | fixed constants; widening reopens the vulnerability they close |

## Dependencies

- `lodestone-assets`: skin decode, equipment, trim and palette bakes, entity model bakes.
- `lodestone-render`: `entity`, `entity_anim`, `entity_pipeline`.
- `lodestone-auth`: account texture fetch, host allow list, data-dir paths.
- `lodestone-shell`: `remote_skins.rs`, `entities.rs`, `sim.rs`/`camera_rig.rs`, `gpu.rs`.
- `lodestone-ecs`: the armour stand pose component and per-accessor merge.
- The v26 families, the only ones decoding stand poses, dye and trim.
- [Entity rendering](entity-rendering.md), the resolve/cull/pose/upload pipeline underneath.
