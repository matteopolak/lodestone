# Player skins

## What it is

The `textures` profile property — base64 → JSON → a URL plus a **wide/slim rig
declaration** — and the render half that draws the declared rig, for issue
[#62](https://github.com/matteopolak/lodestone/issues/62). The decode and the
render are landed; **the fetch is not**, and the three patches it needs all sit
outside the rendering cluster, so they are named below rather than half-built.

## How it works

Two halves, in two crates, plus a hole between them:

| half | lives in | state |
|---|---|---|
| **decode** | `lodestone-assets/src/skin.rs` | landed — `decode_textures_property` |
| **render** | `lodestone-shell/src/container/player_preview.rs` | landed — a runtime `PlayerModelType`, both rigs reachable |
| **fetch** | `lodestone-client`/`protocol/v770`/`lodestone-auth` | **not built** — see [What is missing](#what-is-missing) |

### The decode, and where its record definition comes from

`lodestone_assets::decode_textures_property` takes the property *value* (the
base64 blob) and returns `ProfileTextures { skin, cape, elytra, profile_name }`.

The payload is **authlib's, not the game's**, so it is not in
`.cache/mc/26.2/client-src` at all. The shape was read out of the real jar 26.2
resolves — `.cache/mc/26.2/libraries/com/mojang/authlib/9.0.75/authlib-9.0.75.jar`
— by walking three classes' constant pools:

| class | what it fixes |
|---|---|
| `yggdrasil/response/MinecraftTexturesPayload` | a record of `timestamp`, `profileId`, `profileName`, `isPublic`, `textures: Map` |
| `minecraft/MinecraftProfileTexture$Type` | the map's keys: exactly `SKIN`, `CAPE`, `ELYTRA` (GSON writes an enum key as its own name) |
| `minecraft/MinecraftProfileTexture` | each value: `url: String`, `metadata: Map<String, String>` |

The *consumer* is in the game's own source and is available:
`SkinManager.registerTextures`
(`client-src/net/minecraft/client/resources/SkinManager.java:109-116`) does
`PlayerModelType.byLegacyServicesName(skinInfo.getMetadata("model"))`, which
pins that the model lives on the **skin** entry's `metadata.model` and nowhere
else.

**The one trap, and it fails in the safe-looking direction.**
`PlayerModelType` carries two names per variant:

```text
SLIM("slim", "slim"),
WIDE("wide", "default");
//         ^ id     ^ legacyServicesId
```

`metadata.model` holds the **`legacyServicesId`**, so the wide spelling on the
wire is `"default"`, not `"wide"`. Match on `"wide"` and nothing ever matches —
and because `byLegacyServicesName` is `requireNonNullElse(…, WIDE)`, every skin
including every slim one still resolves *wide*, so the only symptom is Alex's
arms being a pixel too thick. `PlayerModelType::WIDE_LEGACY_SERVICES_ID` is
published so no caller restates it, and
`the_model_comes_from_the_legacy_services_name_not_the_id` asserts `"default"` is
wide **and** `"slim"` is slim, a pair the swapped implementation cannot satisfy.

Base64 is decoded in-module rather than by the `base64` crate: it is already a
workspace dependency (`lodestone-auth` uses it for PKCE), but adding it to
`lodestone-assets`' manifest rewrites `Cargo.lock`, which that cluster does not
own. The decoder is gated against **RFC 4648 §10's own vectors**, an outside
expectation in the strict sense.

### The render half

`PlayerPreview` used to carry `const SLIM: bool = false` with a note saying "when
skins land, this is the one line that changes". It has changed: the rig is now a
runtime `PlayerModelType`, and both rigs are reachable through

```text
ContainerRenderer::set_player_skin(device, queue, model, sheet: Option<&Image>)
  -> PlayerPreview::set_skin
```

`sheet: None` falls back to the pack's own sheet for that rig, so
`set_skin(.., Slim, None)` draws the jar's Alex. **The rig and the sheet swap
together on purpose**: a slim-authored sheet drawn on the wide rig puts the arm
UVs one pixel out, which reads as a texture bug rather than a model bug, and
that is exactly the failure a "just replace the texture" API invites.
`set_skin` returns `false` and changes nothing rather than leaving a slim mesh
with a wide sheet.

### The local override, which is what keeps the slim rig from being dead code

`player_preview.rs::local_skin_override` reads `<data_dir>/skin.png`, with an
optional sibling `<data_dir>/skin.model` naming the rig. `data_dir()` is
`lodestone_auth::paths::data_dir()` — the same directory as `servers.json` and
`profiles.json` (`LODESTONE_DATA_DIR` overrides it).

The marker file holds a **legacy services id** (`slim` or `default`), parsed by
the very same `PlayerModelType::by_legacy_services_name` the network path will
use — not a second bespoke parse that could disagree. Absent, unreadable or
unrecognised is wide, exactly as an absent `metadata.model` is.

This exists for one reason: without a producer, the slim rig and `set_skin`
would both be unreachable, which is this repo's dominant defect class. It is
also the natural cache location for the fetch to write into.

### The bug this found

Making the slim rig reachable immediately exposed a wrong number in the rig
itself. `player_model(slim)` in `lodestone-assets/src/entity.rs` narrowed both
arms from 4 texels to 3 but left the **right** arm's cube origin at `-3`, where
vanilla moves it to `-2`:

```text
slim  right_arm: addBox(-2, -2, -2, 3, 12, 4)   left_arm: addBox(-1, -2, -2, 3, 12, 4)
wide  right_arm: addBox(-3, -2, -2, 4, 12, 4)   left_arm: addBox(-1, -2, -2, 4, 12, 4)
```

(`client-src/net/minecraft/client/model/player/PlayerModel.java:43-71`, with the
wide right arm from `HumanoidModel.createMesh:101`.) The **left** arm keeps
origin `-1` in both, so "slim just narrows the arms" is true of the left and
false of the right — the invariant is that the right arm's inner edge stays at
`origin + width == +1`. The wrong version drew the slim right arm a texel
outboard with a gap at the shoulder, and nothing could see it because nothing in
the workspace had ever selected the slim rig.
`both_player_rigs_arms_match_the_vanilla_mesh_definition` transcribes the table
above from the jar and asserts the inner-edge invariant, with the legs, body and
head as a control on the branch's scope.

## What is missing

**The fetch, entirely.** Three separate discards, in three crates, none of them
in the rendering cluster:

1. **`crates/protocol/v770/src/packets/player_info.rs`** — `read_add_player`
   decodes each profile property and throws all three fields away
   (`let _prop_name`, `let _prop_value`, `let _signature`). This is where a
   remote player's skin would come from.
2. **`crates/lodestone-model/src/event.rs`** — `PlayerListEntry` has no
   properties carrier, so even a decoded property has nowhere to go.
   `lodestone-game/src/tablist.rs:220-223` already names this gap in a comment:
   *"the model's `PlayerListEntry` carries no profile properties, so a folded
   profile has no `textures` (skin) blob yet. When impl-model adds a properties
   carrier, seed it here"*.
3. **`crates/lodestone-auth/src/flow.rs`** — `fetch_profile` deserialises only
   `id` and `name` from `api.minecraftservices.com/minecraft/profile`, discarding
   its `skins` array (`{url, variant: "CLASSIC"|"SLIM", state}`). This is where
   **our own** skin would come from, and `ProfileMetadata::skin_url` already
   exists as a field with nothing ever writing it.

Plus the HTTP GET of the texture URL itself, which must apply vanilla's own host
restriction (`com/mojang/authlib/yggdrasil/TextureUrlChecker`) rather than
fetching an arbitrary URL a server hands us.

Also not built, each deliberately:

* **Capes and the elytra texture.** Decoded (`ProfileTextures::cape`/`elytra`)
  and unconsumed — there is no cape rig in the entity corpus.
* **The first-person arm and the third-person body** still draw `player_wide`
  from the shared entity texture cache; only the inventory avatar honours the
  override. The arm is the most-seen surface of a skin, so it is the obvious next
  step, and it is a different seam (`gpu/entities.rs`' texture cache, keyed by
  model name, not a per-instance sheet).
* **`DefaultPlayerSkin`'s UUID hash.** Vanilla picks one of eight built-in
  sheets from the profile UUID for a skinless account; this reports "no skin
  declared" and draws the pack's `steve`. `ProfileTextures::skin` is an `Option`
  precisely so a caller can tell that case from "a skin declared as wide".
* **The signature.** `MinecraftProfileTextures.signatureState()` feeds vanilla's
  `secure()` flag, which only gates the server-side "require secure profiles"
  option. The signature is a *sibling* of the property value, so this decode
  never sees it.

## Configuration

| knob | effect |
|---|---|
| `<data_dir>/skin.png` | 64×64 sheet drawn on the inventory avatar |
| `<data_dir>/skin.model` | `slim` or `default` (legacy services ids), default wide |
| `LODESTONE_DATA_DIR` | moves both of the above, per `lodestone_auth::paths::data_dir` |

No `client.jar` and no local override means `PlayerPreview::new` returns `None`
and the recess stays empty — the same deliberate no-synthetic-fallback the rest
of the container renderer takes.

## Dependencies

* `lodestone-assets` — `skin` (this feature's own module), `Image::decode_png`,
  `entity::player_model` (the two rigs).
* `lodestone-render` — `entity::player_model_name`, `EntityModelSet`,
  `EntityPipeline`, `GpuEntityModel`.
* `lodestone-auth` — `paths::data_dir` only, read-only. The fetch would live
  here.
* `crate::gpu::entities::entity_texture_from_image` — shared with the world
  entity pass, not copied.

## Related

* [`inventory-player-preview.md`](inventory-player-preview.md) — the avatar this
  draws into, and the pose half.
* [`accounts.md`](accounts.md) — `profiles.json`, whose `skin_url` field is the
  fetch's other landing site.
* [`entity-rendering.md`](entity-rendering.md) — the rig machinery and the shared
  entity texture cache the first-person arm resolves through.
