# Player rendering

## What it is

Everything that turns a player's (or player-shaped entity's) identity and pose into pixels: skin
and cape texture resolution, the local player's synthetic third-person body, armour and trim
layers, armour stand poses, and elytra wings. All six share one mechanism — a second mesh posed
off a wearer's own already-computed part matrices — and one recurring failure mode: attaching by
part *name* instead of by the wearer's resolved model/animation family.

## How it works

### Skins

Skin identity comes from the `textures` profile property: base64 → JSON → structurally parsed URL
plus a model-type declaration. Invalid or relative URL entries are omitted independently, so
valid sibling entries remain independently represented and available at the asset parsing boundary
when another entry is malformed. This parse only establishes URL structure; the remote fetch path
still performs its existing scheme/host authorization before opening a socket. The model bit lives
on the *skin* entry's `metadata.model` and uses
authlib's `legacyServicesId` spelling, not its `id` — the wire value for the wide rig is
`"default"`, not `"wide"`. Matching on `"wide"` compiles, always falls back to wide (the enum's
own default), and the only symptom is Alex's arms being a pixel too thick — nothing crashes, so
get this match right rather than relying on a visible failure to catch it.

Both the local player's own skin and a remote player's resolve the same way: through the server's
tab-list `ADD_PLAYER` entry for that UUID (`entities::player_skin_for_uuid` for us,
`lodestone-shell/src/remote_skins.rs` for everyone else) — **not** through our own login profile
fetch. One ladder means a self-hosted or offline-mode session (which never sends a
`textures` property at all) looks exactly like "skin not fetched yet" — the model's default sheet
is the normal fallback for three cases at once (no skin declared, fetch in flight, fetch failed),
not an error path. Remote fetches are keyed and cached **by texture URL, not by player UUID**, so
two accounts sharing a skin share one decode/GET/bind-group, and a failed fetch is remembered
rather than retried every frame. The signed-in account's own skin is also cached locally at
`<data_dir>/skin.png` / `skin.model` / `skin.uuid`, decoded as an ordinary sRGB image through the
same upload path every other entity texture uses.

**The rig and the sheet swap together, on purpose.** A slim-authored sheet drawn on the wide rig
(or vice versa) shifts every arm UV by a texel, reading as a texture bug rather than a model bug —
so the skin-setting entry point takes rig and sheet as one pair, changing neither if either is
invalid, rather than offering "just replace the texture". Custom player heads
(`minecraft:player_head` with a profile) reuse this same fetch at several independent draw sites
(world, inventory slot, hand, third-person held item), each with its own resolver and cache.

### Capes

Cape visibility is not unconditional: it is driven by a per-player option
(`DATA_PLAYER_MODE_CUSTOMISATION`'s "show cape" bit) that **this client does not yet decode for
remote players**, so every remote player currently draws as if they always allow it. The mesh is
posed off the wearer's own `body` part matrix — the same attach discipline armour uses (below) —
and its sway is a real per-tick-lagged position, not a fixed plane: every tracked entity carries the
lag state unconditionally (cheaper than gating it by render kind), chasing the entity's true
position at a fixed fraction per tick and snapping instantly on a teleport-sized jump. Batched by
cape **texture URL**, matching the skin key. **Elytra takes over the chest slot and suppresses the
cape**: the cape draw and the elytra draw both gate on the identical "is the chest item literally
`elytra`" check, and if the two predicates ever diverge a wearer can lose the cape and gain no wings.

### Third-person body

The local player has no tracked network entity (nothing sends a client its own movement packets),
so nothing built an `EntityDraw` for it. The bridge, `ThirdPersonBodyState → EntityDraw`, fills in
exactly what a tracked entity would otherwise supply — feet, body yaw, animation input (walk
cycle, head look, idle age), scale, rig choice, and equipment (both hands, all four armour slots) —
under a reserved id that can never collide with a server-assigned one. Once built it is appended to
the ordinary entity slice and goes through the *same* resolve → cull → pose → upload → held-item
path every mob uses, not a second copy.

The camera mode (`F5`) is a three-state enum (first person, third person back/front), but the
render bridge only asks the two-valued "is this first person" — asking "is the camera behind me"
instead is how the first-person arm and screen overlays (pumpkin head, underwater tint) would
wrongly reappear in the front view. The third-person camera does a collision-aware pullback,
raycasting backward from the eye through real collision geometry rather than a coarse
solid/not-solid test, so it does not clip through thin barriers. Player skins render through the
**translucent** entity pipeline (not opaque/cutout) with a `0.1` alpha cutout — why vanilla diamond
armour has small gaps at the shoulders: the sheet underneath is deliberately transparent there so
the skin shows through, not a depth bug to mask.

**The body pose and the first-person arm pose must never share a function.** The arm draws in a
camera-space pass with no world position, from an authored rest pose with one rotation swapped in;
the body uses the fully animated walk/head-look/attack pose every mob uses. They are mutually
exclusive by construction (one `Option` source, not two toggles) — pointing one function at the
other's job produces a plausible-looking wrong arm, not a crash. Separately: a per-entity animation
field can have a correct, tested consumer while its only producer for *this one caller* hands it a
hardcoded default — check the producer, not just the consumer. The rig-selection flag (`slim`) is
one such gap today, hardcoded rather than read from the resolved skin.

### Armour

An armour piece is a second mesh, posed off the wearer's own already-computed part matrices
(`ArmourMesh::attach`, matched by part name against the wearer's part transforms) — never a second
animation pass, never written back into the wearer's own transforms. Same mechanism the cape and
elytra use; describe it once and reference it, don't reimplement it. **The attach gate is the
wearer's resolved animation family, never part names.** A pig has both a
`head` and a `body` part; a lookup keyed on part name alone attaches a floating chestplate to a
farm animal. The real gate is "does this rig classify as humanoid (has both arms and both legs)",
the same predicate deciding whether a renderer owns a `HumanoidArmorLayer` in vanilla — the single
most-repeated gotcha in this cluster, applying identically to wool, capes, armour and elytra.

By slot:

| slot | parts | inflation |
|---|---|---|
| head | `head` (+`hat` shell) | 1.0 (+1.5 for `hat`, which draws zero pixels on every vanilla sheet but is kept for fidelity) |
| chest | `body`, both arms | 1.0 |
| legs | `body`, both legs | 0.5 / 0.4 — the **inner** mesh bake, legs an extra 0.1 texel thinner |
| feet | both legs | 0.9 |

Two bakes of the same mesh set exist (outer at 1.0, inner at 0.5) so the chestplate and leggings
don't draw the same torso cube at the same radius and resolve by z-fighting. Sheets are **64×32**,
not the 64×64 a modern skin uses. An item does not name its own texture: it carries an `assetId`
keying a registry mapping to a
per-layer texture list, and the asset name can differ from the item name (`golden_helmet` →
`gold`). Dye (leather only) multiplies in **gamma space** — doing it in linear light washes colour
toward white. A dye value of exactly `0` (including pure black) reads as *undyed*, matching
vanilla, not a bug to special-case away.

Trim is a texture overlay, not a tint: it batches as its own draw keyed by sprite rather than
riding the per-instance tint attribute, draws immediately after its slot's own armour layers
(insertion order matters under the coplanar depth test — an ordered list, never a hash map), and
stays untinted (the sprite is already palette-resolved to the material's colour). Trim sprites are
baked at load time from a greyscale "index" PNG plus an 8-colour palette strip per material, not
pre-baked in the jar. Draw order across slots is otherwise fixed (`chest → legs → feet → head`),
independent of wire order, and armour shares the body's (`LessEqual`) depth comparison, so coplanar
layers (leather's dyed base + its overlay) resolve correctly rather than by draw-order luck.

### Armour stands

A stand's pose is six synced part rotations — head, body, left/right arm, left/right leg — plus
three derived "body stick" parts. The critical rule: **every armour stand is posed, whether or not
a server ever sent a pose update.** The default pose (a small authored splay, not zero) applies the
moment an entity is recognised as a stand; treating "no pose reported" as "leave the walk cycle
running" is the actual, previously-shipped defect — an un-posed stand otherwise animates like a
walking humanoid, including swinging a held item off the same arm. The assignment covers
**rotations only** — a stand's crouch offset and attack-swing arm orbit are
translations and survive underneath it. A metadata update only mentions the parts that changed, so
the fold must merge per-part rather than replace the whole pose, in wire order (two updates to the
same stand in one batch must not both read the same stale base). The extract step gates on the
entity **type** (`armor_stand`), not the presence of a pose
component — reading the component alone leaves every default-posed stand animating, the same
island shape as any other "component absent ⇒ skip" mistake. Two small known gaps: the base plate
should cancel the stand's own body yaw to stay screen-aligned but currently rotates with it (only
head-relative yaw reaches that call site), and the rest-pose bounding box is a few degrees off
since the default splay isn't baked into the skeleton the way a zombie's fixed arm angle is.

### Elytra

Two mirrored wings, posed off the wearer's `body` matrix exactly like armour and the cape, on a
64×32 sheet (not 64×64). Unlike the cape, elytra's authored pose rotation is **not** cancelled by
composition — it is overwritten every frame by the pose branch below, so the standing angle comes
from that branch's resting target, not from anything baked into the mesh. The right wing's
rotation is the left wing's with two of its three angles negated (mirror symmetry), derivable from
the model's reflection rather than a fact to memorise per port.

The draw gate is the same "chest item is literally `elytra`" check the cape pass uses to suppress
itself — the two predicates must never diverge. The texture is the jar's default sheet, or the
wearer's own cape texture when they have one (vanilla prefers a player's cape sheet for elytra) —
unwired currently, since that field does not exist yet on the remote-skin record. **The pose is
presently always the "standing" resting triple** — correct while a wearer stands,
walks or runs, visibly wrong mid-glide or mid-crouch (the wings should fold/spread further), since
no per-tick lerped animation state exists yet and no fall-flying or crouch flag reaches the draw
call at all. A known, disclosed first cut, not a discovered bug.

## How to change it

- **Gate any wool/cape/armour/elytra attachment on the wearer's resolved animation family, never
  on shared part names** — a pig, a zombie and a player expose identically-named parts. Single
  most reusable gotcha in this cluster.
- **Never mutate a wearer's own part transforms to pose an attached layer** — compute the layer's
  transform and read the wearer's matrix; writing back would move the wearer's own visible limb.
- **A skin's rig and its texture change together, never independently**, and **the cape-suppression
  check and the elytra-draw check must stay one predicate** — if you touch either, touch both.
- **When a per-entity animation field looks unwired for the local player specifically, check what
  supplies it for that one caller**, not just its general (correct, tested) consumer.
- **Never write a same-typed run of fields (six armour-stand rotations, lean/flap angles)
  positionally** — a transposition survives every round trip silently; name them, keep test
  fixtures pairwise-distinct. **Tint and dye multiply in gamma space** — linear light washes
  colours toward white. Tests live beside each subsystem; the `#[ignore]`d pixel gates
  (`armour_pixels.rs`, `elytra_wings_pixels.rs`) need a real `client.jar`.

## Configuration

No feature flags gate any of this — each surface draws whenever its data is present.

| knob | effect |
|---|---|
| `<data_dir>/skin.png` / `skin.model` / `skin.uuid` (`LODESTONE_DATA_DIR` relocates) | the signed-in account's cached skin, model, and ownership marker |
| `LODESTONE_ASSETS` (or a discovered `.cache/mc/<version>/`) | must contain `client.jar` or armour/trim/elytra textures are empty |
| allowed remote-texture host and max size | fixed constants, not configurable — widening the allow list would reopen the vulnerability it exists to close |

## Dependencies

- `lodestone-assets` — skin decode, equipment/trim/palette-bake tables, entity model bakes (player,
  cape, elytra, armour meshes).
- `lodestone-render` — `entity`/`entity_anim`/`entity_pipeline`: skeletons, animation families,
  attach logic, and the armour/trim/player-skin/elytra pipelines.
- `lodestone-auth` — the account's own texture fetch, host allow-list, data-dir paths.
- `lodestone-shell` — `remote_skins.rs` (fetch/cache), `entities.rs` (cape lag,
  equipment/dye/trim carry), `sim.rs`/`camera_rig.rs` (third-person body and camera), `gpu.rs`
  (per-frame prepare/draw passes).
- `lodestone-ecs` — the armour stand pose component and its per-accessor merge.
- `lodestone-v26-2` — the only protocol family decoding armour-stand poses, dye and trim; legacy
  families render stands and armour without them.
- [`entity-rendering.md`](./entity-rendering.md) — the general resolve/cull/pose/upload pipeline
  every surface in this doc layers over.
