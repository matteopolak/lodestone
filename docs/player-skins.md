# Player skins

## What it is

The `textures` profile property — base64 → JSON → a URL plus a **wide/slim rig
declaration** — the host-restricted fetch that turns it into a sheet, and the
render halves that draw it on the inventory avatar, on other players' bodies in
the world, and — rig **and** texture — on the local player's own third-person
body and first-person arm. Resource-pack reload also rebinds the vanilla default
head sheet when an overlay omits it. What is left is capes and the Yggdrasil
signature, named under [What is missing](#what-is-missing).

## How it works

Two halves, in two crates, plus a hole between them:

| half | lives in | state |
|---|---|---|
| **decode** | `lodestone-assets/src/skin.rs` | landed — `decode_textures_property` |
| **render** | `lodestone-shell/src/container/player_preview.rs` | landed — a runtime `PlayerModelType`, both rigs reachable |
| **fetch (our own)** | `lodestone-auth/src/texture.rs` + `lodestone-shell/src/skin_fetch.rs` | landed — see [Fetching our own skin](#fetching-our-own-skin) |
| **fetch (remote players)** | `lodestone-shell/src/remote_skins.rs` | landed — see [Remote players](#remote-players) |

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

## Fetching our own skin

The signed-in account's own skin, end to end. Three pieces:

```text
lodestone_auth::flow::fetch_profile                 -- keeps the `skins` array
  -> Profile { skin: Some(ProfileSkin { url, variant }) }
menu::accounts worker (device-code and loopback, both)
  -> skin_fetch::fetch_own_skin
       -> lodestone_auth::texture::fetch_texture    -- TextureUrlChecker, then GET
       -> Image::decode_png
       -> <data_dir>/skin.png + skin.model          -- the *next* launch's path
       -> skin_fetch::publish                       -- *this* session's path
ContainerRenderer::render_geometry_scaled
  -> skin_fetch::take_pending -> set_player_skin
```

### The host restriction is the security-relevant part

A texture URL arrives over the network, so it is screened by
`lodestone_auth::texture::is_allowed_texture_domain` — authlib's
`TextureUrlChecker.isAllowedTextureDomain`, transcribed from the **bytecode** of
`com/mojang/authlib/yggdrasil/TextureUrlChecker.class` in the jar 26.2 resolves.
The full transcription is that module's doc; the rule is *scheme ∈ {http, https}*
and *host exactly `textures.minecraft.net`*, and the two clauses worth knowing
here are the ones the constant pool alone does not reveal:

* the host must **already be lower-case** (`lowerCaseDomain.equals(decodedDomain)`
  compares the lowered host against the *unlowered* one and returns `false` when
  they differ), and `java.net.URI.getScheme()` is likewise case-preserving — so
  `HTTPS://TEXTURES.MINECRAFT.NET/…` is *refused*, not folded;
* `ALLOWED_DOMAINS` is exact-match set membership on the whole host, so
  `sub.textures.minecraft.net` is not allowed either — a suffix rule would have
  been both laxer and wrong.

**This is why the check is not built on `Url`'s parsed values alone.** The `url`
crate normalises the scheme and the host to lower-case while parsing, which is
exactly the question vanilla asks case-sensitively, so a check written against
`host_str()` accepts all four upper-case spellings.
`an_unlowered_host_or_scheme_is_refused_not_folded` computes that wrong
hypothesis in the same run and asserts it *would* have passed, so the rejection
is evidence of the case rule rather than of some unrelated malformedness. The
structural parse is still `Url`'s, because that is what gets `userinfo` right —
`https://textures.minecraft.net@evil.example.invalid/x` has host
`evil.example.invalid` — and the raw-string layer on top can only ever *add* a
rejection.

Two deliberate divergences, both **stricter**: no `IDN.toUnicode` (any `xn--`
label or non-ASCII byte is refused outright, which cannot lose a legitimate URL
because the one allowed domain is pure lower-case ASCII), and a
`MAX_TEXTURE_BYTES` cap vanilla does not have.

### Where the response shape does and does not come from an outside record

The `textures` **property** payload is pinned by authlib (see above). The
services `/minecraft/profile` response's `skins` array is **not** — authlib never
calls that endpoint, the launcher does, so there is nothing in the jar to check
it against. That is why every field of `SkinResponse` is `Option` and the array
is `#[serde(default)]`: a shape change at Mojang's end must degrade to "no skin",
never to a failed sign-in over a cosmetic field. `active_skin` picks the `ACTIVE`
entry (an account keeps its previously-worn skins in the same array, so "take the
first" draws the wrong one) and falls back to the first entry with a URL.

`variant` is `CLASSIC`/`SLIM` — a **different vocabulary** from the
`default`/`slim` a `textures` property uses. `SkinVariant::legacy_services_id`
is the single bridge, so `PlayerModelType::by_legacy_services_name` stays the one
parse for all three sources, and `<data_dir>/skin.model` is written in the
*property* spelling because that is what `local_skin_override` reads. The
inverse, `PlayerModelType::legacy_services_id`, was added for that write: the
obvious `serialized_name()` produces `"wide"`, which the parse does not recognise
— and since its fallback *is* wide, a Steve round-trips correctly and only an
Alex is wrong.

### Why it lands on the frame and not at construction

`PlayerPreview` is built **once**, during `app::lifecycle`'s resume, and never
re-reads the cache. Sign-in happens in the main menu and the inventory is opened
later in the same run, so writing only the cache would have deferred the entire
visible effect of the fetch to the next launch — an island in all but name.
`ContainerRenderer::render_geometry_scaled` therefore drains
`skin_fetch::take_pending()` at the top of every container frame: one uncontended
`Mutex::lock`, `None` on all but the one frame after a fetch lands. The slot is a
slot rather than a queue on purpose — a second fetch replaces an undrained first,
because only the newest skin matters.

Every failure inside the fetch is a `warn!` and a `false`, including the refused
host: a dead texture CDN must not fail an otherwise-successful login.

### `current_model` versus `take_pending`

`skin_fetch::publish` now writes **two** statics, not one: `PENDING` (a
one-shot slot, unchanged) and `CURRENT` (a last-known cache that is never
drained). `PENDING` exists for a *sheet* consumer that only runs while a
container is open (`ContainerRenderer::render_geometry_scaled`); `CURRENT`
exists for `current_model()`, a *rig-only* reader for a consumer that has no
such gate — `sim/camera.rs::third_person_body_state` runs every third-person
frame, container open or not, and a container that had already drained
`PENDING` would otherwise leave it with nothing to read. `current_model()`
falls back to the on-disk `skin.model` marker (the same file
`local_skin_override` reads) when `CURRENT` is empty, so a rig fetched in an
*earlier* session is honoured before this session's sign-in — or a
signed-out launch — has published anything.

That URL-threading fix has since landed, and it did **not** take this shape:
the body's skin comes from the **tab list**, through
`entities::player_skin_for_uuid`, not from the signed-in profile's own fetch.
The tab-list profile is the one the *server* saw, so our own body resolves
through the identical ladder every other player's does, and it works on a
server we are not signed in to. `skin_fetch::current_model` remains the rig
fallback for a session with no tab-list entry for us yet.

## Remote players

`lodestone-shell/src/remote_skins.rs`. The properties already survived the wire
(`read_add_player` keeps them, `PlayerListEntry::properties` carries them, and
`None` there means "this update had no `ADD_PLAYER`, keep the existing skin",
distinct from `Some(vec![])`); this is the consumer.

```text
player_info ADD_PLAYER -> GameProfile::properties
  entities::resolve_entity_facts
    -> remote_skins::skin_for_profile        -- memoised base64+JSON decode
    -> EntityFacts::player_skin -> RenderPlayerSkin -> EntityDraw::player_skin
  app::redraw
    -> remote_skins::request_all             -- one fetch per URL, ever
    -> RenderState::install_pending_player_skins
  RenderState::prepare_entities
    -> group by (hurt, skin url); EntityDrawBatch::skin
  the draw (gpu/frame.rs)
    -> player_skins[url], falling back to the model's default sheet
```

### The blocker that was named, and what it actually took

#62's investigation named the render-side blocker correctly: `EntityBatch` keys a
texture by the `&'static str` model name, so texture identity *was* model
identity and every player in the world collapsed into one `player_wide` batch
sharing one bind group.

The fix it proposed — a `(model, texture)` composite key plus interning, so a
runtime name could still be `&'static str` — is not what landed, and the reason is
worth recording. The batch is the wrong place to make the name static: an
`EntityDrawBatch` lives for one frame, so `EntityDrawBatch::skin` is an owned
`Option<String>` and there is nothing to intern and nothing to leak. What has to
outlive the frame is the **bind group**, and `EntityRenderer::player_skins` is a
`HashMap<String, wgpu::BindGroup>` keyed by URL for exactly that. This is the
shape `ArmourTextureKey` already gives armour: the sheet is chosen at the draw,
and the batch key is what guarantees one texture per batch.

**Keyed by URL, not by player UUID**, so two accounts wearing the same skin share
one decode, one GET and one bind group, and the key survives a reconnect where an
entity id does not.

### Two halves that must agree: the rig and the sheet

`EntityDraw::player_skin` carries a whole `RemoteSkin`, not a bare URL, because
the rig and the sheet change **together**:

* the **rig** is `EntityDraw::model_type_path`, which returns `player_slim` for a
  slim declaration and the untouched `type_path` otherwise;
* the **sheet** is the group key in `prepare_entities`, which becomes
  `EntityDrawBatch::skin`.

A slim-authored sheet on the wide rig puts the arm UVs a texel out; the wide
sheet on the slim rig leaves a gap at the shoulder. Neither reads as a model bug.

**`model_type_path` is an accessor and not a rewrite of `type_path`, and that is
load-bearing.** `type_path` is also what `gpu/nametag.rs` hands to
`entity_dimensions::base_dimensions` to place a tag above the head, and
`"player_slim"` is **not** an entity-type registry path — it would miss, fall back
to `FALLBACK_HEIGHT`, and put every slim player's nametag at the wrong height.
`world_items.rs`, `debug_lines.rs` and the flame pass read it the same way.
`prepare_armour` *does* use `model_type_path`, so a slim player's chestplate is
posed off the slim body's own part matrices.

### The fallback is the normal path, not an error path

A batch with `skin: Some(url)` and no bind group installed resolves to the
model's default sheet, and so does `skin: None`. That covers three cases at once
and none of them is a failure: no skin declared, a fetch in flight, and a fetch
that failed. **Offline-mode servers send no `textures` property at all** (the
account UUID is derived from the username), so this is what every one of our own
oracles looks like — a gate here must never assert that a skin *arrives*.

Failures are remembered rather than retried: `FetchState::Failed` is why a dead
CDN or a refused host does not produce one GET per player per frame forever.

### The profile arrives after the entity

`ADD_PLAYER` and `ADD_ENTITY` are separate packets, so a remote player's first
folds legitimately see no tab-list entry and `RenderPlayerSkin` is `None`. It is
updated **outside** `update_track`'s motion gate for that reason: a player
standing perfectly still while their tab-list entry lands must still get their
skin.

### The fetch forks on `#[cfg(test)]`

`remote_skins::spawn_fetch` has two definitions. The real one spawns a
short-lived thread with its own current-thread runtime (the shape
`menu::status`'s one-shot ping uses) — one thread per *distinct skin*, not per
player and not per frame. The test one records the URL in `requested_urls`.

A `cfg!(test)` early return would have been a silent skip; a `#[cfg(test)]` fork
makes the routing assertable, and without it a unit test reaching `request` would
perform a real HTTP GET as a side effect of `cargo test` — a defect class no
health check in this repo can see. See `DESIGN.md` on the unit test that was
opening a browser on every `cargo test -p lodestone-shell`.

## Custom heads: the same skin at four surfaces

A `minecraft:player_head` carrying a `minecraft:profile` with a `textures`
property is a **custom head** — the decorative kind a server places, whose whole
appearance is that one property. It reaches four different draw sites, each with
its own resolver and its own bind-group cache:

| surface | producer | draw site | texture cache |
|---|---|---|---|
| placed in the world | `block_entities::skull_candidates` (NBT) | `gpu/frame.rs`'s block-entity loop | `EntityRenderer::player_skins` |
| an inventory / hotbar slot | `container::builder::icon_record`, `app::redraw`'s hotbar fold | `hud/item_icon.rs`'s `SpecialIcons` | `SpecialIcons::player_skins` |
| the first-person hand | `app::redraw`'s `held` (a clone of the hotbar record) | `RenderState::build_special_hand_draw` | `EntityRenderer::player_skins` |

The `model.type` nested in an item-definition `special` node is itself a resource
location. `lodestone_assets::item_model` canonicalizes an omitted namespace, so a
pack's bare `"head"` becomes `minecraft:head` before GUI, hand, and world-special
routes dispatch it. Keep that normalization at the parser boundary; adding aliases to
individual draw sites would let those surfaces disagree after a pack reload.

`minecraft:head` and `minecraft:player_head` both submit the raw, Y-down skull
mesh. Vanilla item definitions—not the special renderers—supply the canonical
wrapper `translation: [0.5, 0, 0.5]` plus a 180° X rotation. When this server
pack retargets a player head to an empty generic `minecraft:head` node, shared
special-item composition restores that complete wrapper once, after authored
node transforms and before GUI, hand, or world placement. A non-empty parsed
chain remains authoritative, preventing vanilla generic heads and legacy player
heads from being wrapped twice. This is head-only; do not infer corrections for
other custom special kinds.

Every special-item surface ends in the same substitution: `special_item_rig`/`skull_spawn` resolves
the head to `skull_texture_stem(SkullType::Player)` — the default sheet, which is
the right answer for a *plain* head — and the caller replaces that stem with a
`BlockEntityTexture::PlayerSkin(Arc<str>)` when the stack declares one. The enum
is the shared batch key, not three parallel spellings of one identity.

The fourth surface is a third-person held stack. It is deliberately a separate
route from the first-person hand: the local avatar is a synthetic draw while a
remote player starts in the tracked `SET_EQUIPMENT` entity. Both preserve the
same profile URL beside their intentionally narrow `(EquipmentSlot,
ResourceLocation)` visual-id list:

```text
local player menu ItemStack
  -> ThirdPersonBodyState::equipment_skin -> EntityDraw::equipment_skin
tracked SET_EQUIPMENT ItemStack
  -> EntityFacts::equipment_skin -> RenderEquipmentSkin -> EntityDraw::equipment_skin
both
  -> RenderState::held_special_item -> BlockEntityTexture::PlayerSkin
```

This follows 26.2's `PlayerHeadSpecialRenderer.extractArgument`: it reads
`DataComponents.PROFILE` from an underlying `minecraft:player_head` stack and
asks the player-skin cache for the render argument. `minecraft:item_model` may
select a different client item definition, but it does not change which stack
owns `PROFILE`. Lodestone therefore creates this side channel only for that
underlying item, starts the same deduplicated remote fetch, and applies the URL
at the held special-item draw boundary. That boundary does not branch on a
server-pack special-model name: this is data-component propagation, so a pack
that resolves the player head through its generic `minecraft:head` renderer
still receives the profile-selected sheet.

**The world surface landed first and that is exactly what hid the other two.**
The same head was correct once placed and plain in a slot and in your hand, which
reads as a GUI bug rather than as a missing field. The field was
`ItemIcon::skin`/`MainHandItem::skin`, and the loss was the fourth of the same
family: `dyed_color`, `potion_color`, `banner_patterns` and `base_color` had each
been threaded through by their own earlier fix, so the record was *almost*
complete and no assertion anywhere was wrong.

### One decode, one fetch, two ways of getting the sheet

`remote_skins::skin_for_textures_property` is the single decode — the placed head
reaches it through the block entity's NBT, the item surfaces through
`hud::item_icon::stack_skin_url`, which also calls `remote_skins::request`. **A
resolver that returns a URL nobody requested draws the default sheet forever**,
so the request lives inside that one function rather than at each producer.

Getting the decoded sheet to a bind group forks, and the fork is the interesting
part:

* the world and hand surfaces share `EntityRenderer::player_skins`, filled by
  `RenderState::install_pending_player_skins` by draining `remote_skins::READY`
  once per frame and, when needed, rehydrating from the retained cache;
* the GUI icon pass owns everything it draws with (`SpecialIcons`' own doc says
  why), so it needs its own bind group — and it cannot drain, because a drain has
  exactly one consumer and a second `drain_ready()` would *steal* the world's
  sheet rather than share it.

So `publish` also fills a retained by-URL store (`remote_skins::SHEETS`), and the
GUI pass **pulls** from it with `remote_skins::sheet(url)` for any head it has no
binding for, on every frame that draws one. Pulling is also what removes the
ordering problem: a record built on the frame the fetch *starts* resolves on
whichever later frame it lands, with no "the drain has to arrive on the right
frame" hazard to get wrong.

The world/hand pass uses the same retained store as a recovery path. This matters
when a renderer is rebuilt after `READY` was already drained: it enumerates
`remote_skins::cached_sheets()` and uploads only URLs absent from its bind-group
cache. The steady state performs no GPU uploads, and a rebuild does not refetch
or decode a skin that is already resident in memory.

### Nothing declines silently, because a plain head is a head

The failure mode here has no visible signature — a head that lost its texture
still draws a head, so nothing looks broken, nothing goes red, and the only
evidence is the wrong face. Every decline therefore logs:

| decline | where | shape |
|---|---|---|
| the profile's `textures` decodes to no usable URL | `hud::item_icon::stack_skin_url` | one `warn!` per process |
| the sheet is not fetched yet, or the fetch failed | `SpecialIcons::install_ready_player_skins` | one `warn!` per **episode**, with a paired recovery `info!` |
| the URL's host is outside the allow list | `remote_skins::request` | one `warn!`, and the URL is memoised `Failed` |

The draw still falls back to the default sheet in all three cases — the fallback
is right, the *silence* was the defect.

### Pack reload keeps the default-head fallback bound

`BlockEntityRenderer` owns the default player-head sheet as a static GPU binding.
When a resource-pack generation changes, `RenderState` reloads those bindings from
the merged resource manager as well as rebuilding the GUI and item atlases. A server
pack may define `items/player_head.json` without supplying a player sheet; normal
pack precedence then resolves the vanilla lower-priority sheet. If a custom-head skin
fetch fails, the already rebound default sheet remains drawable instead of becoming a
blank head. Extend this reload path whenever another static block-entity texture is
made pack-resolved.

## Landed since this doc first said otherwise

Both entries below were in *What is missing* for long enough to be quoted
forward as blockers. They are recorded here rather than deleted, because the
shape of each is worth keeping.

* **Our own third-person body's texture, and the first-person arm.** The local
  player has no tracked entity — `extract_entity_draws` excludes it
  deliberately — so it reaches none of the fold that resolves a skin, and its
  own producer had no resolution of its own: `ThirdPersonBodyState::into_draw`
  hardcoded `player_skin: None` and the arm hardcoded `player_wide` plus the
  pack's sheet. `entities::player_skin_for_uuid` is now the shared ladder, and
  `Sim::local_player_skin` asks it for **us** against the same tab list, so our
  own body and every other player's resolve through one path.

  The arm reads the result through `remote_skins::local()` rather than through
  `ThirdPersonBodyState`, and that is structural: the arm draws precisely on
  the frames `third_person_body_state` returns `None`, so it can never see that
  state. `local_player_skin` is therefore called **above** the camera-mode gate.
  Moving it below silently unwires the arm and nothing goes red.

* **`DefaultPlayerSkin`'s UUID hash.** The 18-entry table and the hash pick
  landed in `lodestone_assets::skin`, and then `DefaultSkin::texture` sat with
  **zero production readers**: the one caller took `.model` (the rig) and
  dropped `.texture`, so all eighteen identities collapsed onto the pack's two
  plain sheets and every skinless player was Steve or Alex. `RemoteSkin::default_sheet`
  carries the identity to `EntityDraw::variant_sheet` — the channel the draw
  already consults after a fetched url misses — and `load_entity_variant_textures`
  loads the eighteen sheets by exact path.

  It is stamped **outside** `skin_for_textures_property`'s memoised decode:
  that cache is keyed by the property value, so two accounts wearing one skin
  share an entry, while the identity is a function of the uuid and must differ
  between them.

## What is missing

Not built, each deliberately:

* **Capes and the elytra texture.** Decoded (`ProfileTextures::cape`/`elytra`)
  and unconsumed — there is no cape rig in the entity corpus. The local body's
  cape URL now *reaches* its draw (it rides on `player_skin`), but nothing ticks
  a `CapeLag` for the local player, so `cape_sway` stays `(0, 0, 0)` by
  construction rather than by omission.
* **The signature.** `MinecraftProfileTextures.signatureState()` feeds vanilla's
  `secure()` flag — see the entry below.
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

Both files are **written** by the fetch as well as read, so signing in overwrites
a hand-placed override. Nothing else is env-driven; the allow list and the size
cap are constants (`ALLOWED_TEXTURE_DOMAIN`, `MAX_TEXTURE_BYTES`) and are
deliberately not configurable — a knob that widened the allow list would be the
whole vulnerability back.

No `client.jar` and no local override means `PlayerPreview::new` returns `None`
and the recess stays empty — the same deliberate no-synthetic-fallback the rest
of the container renderer takes.

## Dependencies

* `lodestone-assets` — `skin` (this feature's own module), `Image::decode_png`,
  `entity::player_model` (the two rigs).
* `lodestone-render` — `entity::player_model_name`, `EntityModelSet`,
  `EntityPipeline`, `GpuEntityModel`.
* `lodestone-auth` — `paths::data_dir`, plus `texture::fetch_texture` (the host
  allow list and the GET) and `flow::{Profile, ProfileSkin, SkinVariant}`. Uses
  `reqwest::Url` for the structural parse, which needs no manifest change since
  `reqwest` is already a direct dependency.
* `lodestone-game` — `tablist::GameProfile::skin_texture`, the accessor the
  remote path starts from.
* `crate::gpu::entities::entity_texture_from_image` — shared with the world
  entity pass, not copied; `remote_skins`' bind groups go through it too.

## Related

* [`inventory-player-preview.md`](inventory-player-preview.md) — the avatar this
  draws into, and the pose half.
* [`accounts.md`](accounts.md) — `profiles.json`, whose `skin_url` field is the
  fetch's other landing site.
* [`entity-rendering.md`](entity-rendering.md) — the rig machinery and the shared
  entity texture cache the first-person arm resolves through.
