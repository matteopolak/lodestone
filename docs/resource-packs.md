# Server resource packs

## What it is

The end-to-end flow for a server-pushed resource pack: the accept/decline
prompt (`Screen::ResourcePackPrompt`), the per-server policy that can skip
it (`menu::servers::ServerPackPolicy`), the download/verify/apply pipeline
(`net.rs`), and how a downloaded pack actually reaches the block atlas
(`resources.rs`). Landed against a player report that a server's pack never
prompted at all and the "Server Resource Packs" row was permanently greyed
out.

## How it works

### The wire side

`ClientboundResourcePackPushPacket`/`Pop` decode into
`lodestone_model::event::ClientEvent::ResourcePackPushed`/`Popped`
(`crates/lodestone-model/src/event.rs`) and are classified `Route::NOWHERE`
— they are answered directly inside `net.rs`'s own connection loop, not
through the `forward`/`Sim::poll_net` path every other `ClientEvent` takes.
`ClientAction::ResourcePackResponse` is the one reply packet, carrying an
8-value status enum (`lodestone_model::action::ResourcePackResponseKind`)
that mirrors vanilla's `ServerboundResourcePackPacket.Action` exactly —
`SuccessfullyLoaded`, `Declined`, `FailedDownload`, `Accepted`, `Downloaded`,
`InvalidUrl`, `FailedReload`, `Discarded`, in that ordinal order.

### The decision (`net.rs`)

`route_resource_pack_pushed` reproduces
`ClientCommonPacketListenerImpl.handleResourcePackPush`'s own condition:

1. A URL that is not `http`/`https` (`resource_pack_url_is_valid`) is
   `INVALID_URL`, unconditionally, before the per-server policy is even
   read.
2. Otherwise `decide_resource_pack_push(policy, required)` — a pure,
   unit-tested function — answers `AutoAccept`, `AutoDecline`, or `Prompt`.
   `Enabled` always auto-accepts (a real accept: `ACCEPTED` then the
   download starts, no dialog). `Disabled` auto-declines an *optional*
   pack but still prompts a *required* one — vanilla will not silently
   drop a player over a pack they never personally answered. `Prompt`
   always asks.

### The prompt (`Screen::ResourcePackPrompt`)

A second, independent `ConfirmScreen`-style overlay
(`menu::confirm::ResourcePackPromptNav`/`resource_pack_prompt_frame`),
reusing the geometry helpers `menu::confirm::ConfirmNav` (the world-delete
confirmation) already built, but **not** `Screen::Confirm` itself — the two
cannot share a screen because `render::owns_frame` answers per *variant*,
and this one has to be a live-session overlay (openable over
`Screen::Connecting`, since a pack can be pushed during Configuration,
before Play, as well as over a live world) while `Confirm` is
unconditionally a full-frame menu screen reached only from
`Screen::WorldSelect`.

`net.rs` never touches the UI directly: it writes the pending prompt into
`net::PackPromptCell` (`NetClient::pending_resource_pack_prompt`), and
`app/session.rs`'s `drive_ui_from_session` reconciles it into the screen
every frame, the same pattern already used for `Sim::is_dead`/`has_won`.
Answering the dialog produces `MenuAction::ResourcePackResponse { id,
accept }`, which `app/menus.rs` submits through
`NetClient::respond_to_resource_pack` — a dedicated channel on `NetClient`,
not a generic `ClientAction` queued through `send_action`, because Accept
has to *do* something (spawn the download) rather than merely put a byte on
the wire.

A **required** pack the player declines disconnects the session — vanilla's
own `PackConfirmScreen` self-disconnects rather than waiting for the server
to notice a client that will never load the pack
(`multiplayer.requiredTexturePrompt.disconnect`). The net thread does this
itself (`apply_pack_response` returning `true` breaks its own loop and sends
`NetUpdate::Disconnected`), not the UI layer.

**The answer is queued, not applied, by `respond_to_resource_pack`.** It
only sends `(id, accept)` on a channel the net thread's own loop drains —
up to 15 ms later on native, since that loop also has an outbound-action
flush and an `events.recv()` wait to get through first — and only that
drain, `apply_pack_response`, actually clears `PackPromptCell`. A player
report ("accepting did nothing, it kept the choice menu open") traced to
exactly this gap: `MenuNav::apply_resource_pack_prompt` closes
`Screen::ResourcePackPrompt` the instant the click is handled, but
`drive_ui_from_session`'s reconcile can run again before the drain catches
up (the click handler and a frame's reconcile share one winit dispatch), so
it read the still-`Some` cell, saw the screen closed, and reopened the exact
prompt just answered. `MenuNav::resource_pack_answered_id` is the fix: it
remembers the id this side already answered and the reconcile skips
reopening for it, forgetting it again once `pending_resource_pack_prompt()`
itself reports `None`. See `accepting_a_resource_pack_prompt_does_not_reopen_it_before_the_net_thread_catches_up`
(`app/tests.rs`) for the reproduction, driven against a **loopback**
`NetClient` — whose `pack_response_tx` has no receiver at all, so the cell
never clears, the permanent worst case of the real lag.

### The download (`net.rs`, native only)

`spawn_pack_download` runs on its own OS thread with its own
`current_thread` tokio runtime — the same shape `remote_skins.rs`'s
`spawn_fetch` uses for an identical reason: the connection loop also drives
movement and keep-alives, and a slow or hostile download must not stall it.
`download_pack_bytes` streams the HTTP(S) response, aborting the instant
either the declared `Content-Length` or the running total exceeds
`MAX_PACK_SIZE_BYTES` (262,144,000 — vanilla's own
`PackDownloader.MAX_PACK_SIZE_BYTES`, not an invented number).
`verify_pack_hash` checks the downloaded bytes' SHA-1 against the hash the
push carried, when it is a well-formed 40-hex-character string — vanilla's
own leniency (`DownloadedPackSource.tryParseSha1Hash`): an absent or
malformed hash skips verification rather than failing it. A hash that *is*
well-formed and does not match is always rejected.

### Applying it (`resources.rs`)

Nothing is ever extracted to disk. `crate::resources::set_server_pack`
hands the verified bytes straight to `lodestone_assets::ZipSource::from_bytes`
— the same version-free zip reader a local third-party `.zip` pack already
goes through (`open_pack_source`) — and, once it opens successfully, stores
them in a process-wide cell and bumps `pack_generation`. That is the entire
live-reload signal `Sim::reload_resource_pack_atlas` already polls every
frame for the local Resource Packs screen, so a server pack reaches the
block atlas through the exact pipeline a local one does, with no second
wiring path to drift out of sync with the first. `selected_pack_sources`
prepends the server pack ahead of the local selection — vanilla's own
`Pack.Position.TOP` for a downloaded pack — and it never appears in the
local Resource Packs screen's own list, matching vanilla keeping downloaded
packs out of `PackRepository`.

**The default font observes it too, and until recently did not.** The owner
reported *"custom fonts arent fully used. it didnt apply them to the chat"* while
confirming the same pack's fonts *did* change other text — which is the outside
view of a very specific defect. `VanillaFont::shared` cached
`minecraft:default` in a `OnceLock`, so **the first caller in the process decided
the default font for the whole session**: a pack applied afterwards, whether
server-pushed or selected on the Resource Packs screen, could never replace it.
Text drawn with a *custom* font id was unaffected, because the cache described in
the next paragraph already keys on the generation — hence "some places, but not
chat", chat being default-font text.

The cache is now keyed on the generation like its custom-font sibling, and the
three renderers that *hold* a resolved font (`HudRenderer`, `MenuRenderer`,
`ContainerRenderer`) each carry the generation they resolved against and re-ask
at the top of their draw, through the single shared
`hud::vanilla_font::refresh_shared_font`. One implementation rather than three
copies, deliberately: this repo has twice found the same fix discovered
independently at two call sites because nothing mechanical connected them. An
unchanged generation costs a relaxed atomic load and an integer compare.

Two things worth knowing before changing it. **There is no notification** — a
server pack installs on the network thread and the Resource Packs screen writes
the selection directly, so a per-draw compare is the seam, not a callback. And a
new stack resolving to *no* font is assigned rather than ignored: a pack that
breaks the font must fall back to the fixed-advance debug font, not keep drawing
the previous pack's glyphs.

The HUD's lazy custom-font cache (`hud::vanilla_font::VanillaFont`) also
observes this generation. It retains both successful and failed
`"font": "namespace:name"` lookups while the stack is unchanged, avoiding a
resource load for every rendered nameplate; a changed generation clears those
entries before the next lookup. This matters for a font first requested while
an accepted server pack is still downloading: once `set_server_pack` installs
the bytes, the next nameplate can resolve it instead of keeping the old
fallback forever. A warning for a name that remains missing after a generation
change still means the pack did not provide a usable font (or was never
installed); it is not suppressed by this cache policy.

### What a live reload re-attaches, and the trap in the one that borrows

`WindowApp::redraw`'s reload block is the GPU half. `Sim::reload_resource_pack_atlas`
has already rebuilt the classifier, the `BlockAtlas` and its `BlockModels` and
re-meshed every loaded column by the time it returns the new atlas; the redraw
block then catches up every GPU surface that is a **separate stitch with its own
owner** — `RenderState::reload_block_atlas` for the terrain/model atlas, the HUD's
and menu's GUI atlases, the flat `ItemAtlas` plus its glint sheet, and the 3-D
block-item pass on both `HudRenderer` and `ContainerRenderer`.

That last one is the one to be careful with, and it is why the list has to be kept
complete rather than trimmed. The GUI item-model pass does not *own* its atlas: it
borrows `RenderState`'s model atlas view, tint palette buffer and animation buffer
(that sharing is the point — a second copy of the block atlas to draw nine 16 px
icons would cost tens of megabytes). `reload_block_atlas` replaces all three with
new objects. wgpu resources are `Arc`-backed and a bind group holds a strong
reference, so a pass left un-reattached **does not error** — it goes on sampling the
dropped atlas while the geometry it draws has been re-baked against the *new* one's
sprite packing, reads a palette nothing updates any more, and reads an animation
buffer `update_animation` no longer writes (so an animated block icon freezes). The
flat sprite stream cannot show this, because its atlas and its UVs come from the
same `ItemAtlas` object and are replaced together — which makes the symptom look
like "3-D block icons in menus broke and flat item sprites are fine".

So: **whenever a GPU pass borrows another renderer's atlas/buffer rather than
owning it, its re-attach belongs in this block**, and a new borrower must be added
here in the same commit that adds the borrow. Nothing is red when it is not.

### Owning a resource is not an exemption — it is the *other* failure

The rule above is about borrowers, and reading it as "an owner is fine" is wrong.
It was: the **special-renderer** icon pass (chest, shulker box, banner, shield,
skull, player head — `IconPart::Special`, the third GUI icon stream) owns
everything it draws with, its own struct doc says so, and it was still wrong
across a reload. It decodes its block-entity sheets through
`crate::resources::load_block_entity_textures`, which reads the pack stack **live
at the moment of the call** — and that call happens exactly once, from a lazy
build on the first frame carrying a special icon.

So the two failures are mirror images, and only one of them is about dropped
objects:

| the pass | what a bump does to it | the fix |
|---|---|---|
| **borrows** (flat sprites, 3-D block items) | keeps sampling a **dropped** atlas with newly-repacked UVs; blank wherever a UV lands on padding | re-attach |
| **owns** (special-renderer icons) | keeps sampling a perfectly **valid** sheet belonging to the *previous* pack | drop it and let the lazy build re-run |

The owning case has a second, worse mode that the borrowing case does not: the
lazy build is **latched**. `IconRenderer`'s `special_tried` was set on that one
attempt and nothing ever cleared it, so if the build returned `None` that once —
no pack stack, or no sheet decoded — the whole stream stayed dark for the rest of
the process and no later reload could recover it. `IconRenderer::reload_special`
clears the latch, which is what makes a failed first build recoverable at all.

**A pass built lazily belongs in the reload block just as much as one built at
bring-up**, and it will not look like it does: there is no `attach_*` call for it
to sit beside, which is precisely why it was missed.

`gpu/block_entities.rs`'s **world** block-entity pass reads the same loader and is
built once in `RenderState::new`, which `reload_block_atlas` does not rebuild — so
it carries the owning-side defect too. Not yet fixed: it needs its sheets rebound
rather than a latch cleared.

No hermetic gate can see any of this — every gate builds its renderer once and
never reloads. `container_item_pixels.rs`'s
`a_resource_pack_reload_does_not_blank_a_container_special_icon` is the one that
does, and it carries a control arm that omits the drop so the defect is exhibited
rather than argued (sheet count `84 -> 84` un-reloaded, against `84 -> 0 -> 84`
with the fix).

### Single winner vs. merged stack: which resource types get which

`ResourceManager` offers two lookups (`crates/lodestone-assets/src/manager.rs`):
`read` (highest-priority pack wins outright, the rest are never even opened)
and `read_stack`/the pack-aware loaders built on it (every layer that carries
the path is opened, lowest priority first, and merged). Getting this wrong in
either direction is silent — nothing errors, a pack just does not do what it
looks like it should. The rule is per resource type, from vanilla's own
record definitions, not a blanket policy:

- **Single winner** (`read`) — a whole-document override with no merge
  concept in vanilla: a texture PNG, a `.mcmeta`, a blockstate JSON, a model
  JSON, an item definition JSON, a particle definition JSON
  (`ParticleResources.reload` uses `listMatchingResources`, not a stack). A
  server pack replacing one of these replaces the whole file, which is
  correct — changing these to merge would be the regression.
- **Merged across the stack** — a *list* or *registry* packs are meant to
  extend, not replace:
  - Language files (`lang.rs`'s `Language::merged_from_stack`,
    `ClientLanguage.loadFrom`/`getResourceStack`) — landed first; see its own
    module doc for the `container.crafting`-on-screen symptom a single-winner
    read produced.
  - Fonts (`font.rs`, `read_stack`) — a pack's own `font/<id>.json` adds a
    provider layer under/over the jar's chain rather than replacing it
    (`FontManager.reload`/`getResourceStack`).
  - Atlas source lists — `assets/<ns>/atlases/<id>.json`
    (`atlas_source.rs`'s `AtlasDefinition::load_stacked`,
    `SpriteSourceList.load`/`getResourceStack`). Covers `armor_trims.json`,
    `banner_patterns.json`, `shield_patterns.json` (`trim.rs`,
    `banner_pattern_atlas.rs`). A pack shipping its own `armor_trims.json`
    extends the jar's `paletted_permutations` source instead of discarding
    it. (The block/item atlases this client builds do not go through
    `atlases/*.json` at all — they enumerate textures directly via
    `ResourceManager::list`, which already unions paths across the stack, so
    this class of bug does not apply to them.)
  - Item tags — `data/<ns>/tags/item/<id>.json`
    (`resources.rs`'s `merged_tag_json`, `TagLoader.load`/
    `listMatchingResourceStacks`). Honours each layer's own `"replace"` flag
    (default `false`: append to the layers below; `true`: reset first) —
    the same shape `sound.rs`'s `SoundRegistry::merge_from` already models
    for `sounds.json` events, though that registry is currently fed from the
    external asset-index object store rather than a live `ResourceManager`
    stack, so no pack can extend it today.

When adding a new pack-loaded resource type, check the vanilla loader's own
method name first: `getResource`/`listMatchingResources` is single-winner,
`getResourceStack`/`listMatchingResourceStacks` merges.

When a span names a custom font, selection happens per codepoint: a glyph the
custom font declares uses that font's metrics and pixels, while an uncovered
codepoint falls back to `minecraft:default`. `spans_width` performs the same
selection as drawing, so a centred component cannot measure with default
advances and then draw with wider or narrower custom glyphs.

Bitmap font sheets retain native RGBA rather than being reduced to a white ink
mask. The HUD multiplies each bitmap texel's RGB and alpha by the component's
text colour/pass alpha, emits no geometry for zero final alpha, and breaks a
horizontal merged run whenever adjacent final RGBA differs. This permits pack
authors to use coloured or translucent font pixels. Unihex remains binary
white/transparent and TTF keeps its existing thresholded binary rasterisation.

### Diagnosing a pack font's spacing (`LODESTONE_FONT_METRICS`)

`FontLoader::load_bitmap` (`lodestone_assets::font`) can dump, to stderr, every
bitmap glyph it loads — declared `height`/`ascent`, the sheet's grid and
per-cell pixel size, the derived `pixel_scale`, each codepoint's measured ink
width (`actual_glyph_width`'s result, in the sheet's own physical texels), the
resulting `advance`, and the *drawn* extent (`ink_w * pixel_scale`, the
logical-pixel width `draw_ink` actually paints). Set `LODESTONE_FONT_METRICS`
to any value and run the client; this fires for **every** font loaded through
`FontLoader::load`/`load_raster`, `minecraft:default` included, so grep the
output for the pack's own bitmap `file` (a custom font's sheet is always
under its own namespace, never `minecraft:font/`) to isolate it.

This exists because two glyphs can each measure correctly in isolation and
still overlap on screen: the discriminating case is a `pixel_scale` other
than `1.0` (a sheet cell physically larger or smaller than the font's
declared `height`), which every one of vanilla's own three bitmap sheets
happens to avoid (all `height: 8`/`ascent: 7`, cell `8×8` or `16×16` — always
an *integer* scale) but which a server-provided background-panel font
commonly is not. Read two consecutive glyphs' lines together: if one
codepoint's `drawn_w` is greater than or close to its own `advance`, that
glyph's ink extends into the next glyph's pen position — the "background
block that swallows the next glyph" shape. If `drawn_w` stays comfortably
under `advance` for every glyph and the overlap is still visible, the sheet's
own metrics are not the cause and the next place to look is which draw path
actually consumed this font (`hud::vanilla_font` for chat/HUD text,
`gpu::nametag`/`gpu::sign_text` for nameplates and sign text — each is a
distinct implementation of the same `pixel_scale`-aware texel walk, so a
regression in one does not imply a regression in the others).

A `space` provider prints its own header line too, one per provider, listing
**every** codepoint it declares and the advance each one gets —
`space provider font=<id> provider[<n>] <count> advances [U+F800:-1,U+F801:-2,...]`
— not just a count. This is the direct way to answer "does this pack's gap
table even contain the codepoint I think is the gap, and what did the pack
author actually ask for": a bitmap provider's own header only shows *that*
provider's glyphs, so a `space` provider silently losing every codepoint to
an earlier bitmap provider (see the precedence note below) used to be
invisible even when the metrics dump was otherwise complete.

The same env var also makes two previously-silent soft skips speak: a
`unihex`/`ttf` provider whose `hex_file`/`file` is not present in the active
pack stack now prints one line either way (see `FontLoader::load_unihex`/
`load_ttf`'s own doc for why the skip itself is intentional — an absent
optional file must degrade the font, not fail it outright). An unrecognised
`filter` condition key (or a non-boolean value) also always prints, since a
misgated provider silently landing as always-active is a correctness bug in
the pack, not something worth hiding behind the metrics flag. All of this is
plain `eprintln!`, not `tracing`: `lodestone-assets` carries no logging
dependency, and — unlike `tracing::warn!` elsewhere in this crate's callers —
none of it needs `RUST_LOG` set to be visible.

If every glyph's own `drawn_w` stays comfortably under its `advance`, the
sheet's own metrics are not the cause of an overlap. The next-most-likely
cause is a **precedence race**: two different providers (a pack's own bitmap
panel and, commonly, vanilla's own `unihex` CJK/Thai/Arabic fallback)
declaring the *same* codepoint, where whichever one is earlier in the
font's flattened, priority-ordered provider list wins outright — the other's
glyph is never even considered, however correct its own metrics are in
isolation. `LODESTONE_FONT_TRACE` (below) is the direct tool for that case;
`LODESTONE_FONT_METRICS` alone cannot show it, because it only ever prints
the *winning* provider's own numbers for a given codepoint.

### Which provider actually wins a codepoint (`LODESTONE_FONT_TRACE`)

Set `LODESTONE_FONT_TRACE` to a comma/whitespace-separated list of codepoints
(`0x7532`, `U+7532`, or a bare decimal all work) and every provider in the
font's flattened, priority-ordered list that declares one of those codepoints
prints one line — its own kind, source file, and (for `bitmap`/`space`/
`unihex`) the advance it would contribute — tagged `WINS` for the one
`Font::advance` actually returns and `shadowed` for every
other provider that also covers the codepoint but lost. This is the direct
answer to "which provider supplies this codepoint, and what did each one
compute", rather than inferring the race from a metrics dump that only shows
the winner. Example, tracing the space character where a `space` provider and
a blank ascii cell both declare it:

```text
lodestone-assets: TRACE font=minecraft:default U+0020 provider[0] space advance=4 -> WINS
lodestone-assets: TRACE font=minecraft:default U+0020 provider[1] bitmap file=minecraft:font/ascii.png cell_pos=(0,2) advance=1 -> shadowed (an earlier-declared provider already won this codepoint)
```

Every line names the font id being loaded (`font=`), because the tool fires
once per `FontLoader::load` call and a process typically loads several fonts
(`minecraft:default` for ordinary HUD text, plus one per distinct
`Style.font` a message actually uses) — three different `WINS` lines for one
codepoint is routine and expected when they belong to three different font
ids, and would be a real same-font contention bug if they ever shared one.
Read the `font=` field before drawing any conclusion from a multi-line
trace.

**Provider order across multiple packs is a merge, not an override.**
`FontLoader::flatten` reads every active pack's own copy of `font/<id>.json`
for a given id (`ResourceManager::read_stack`) and lays out the merged
provider list **highest-priority pack first, each pack's own JSON
declaration order preserved within its own segment** — matching vanilla's
`FontManager.prepare`, which reads via `FONT_DEFINITIONS.listMatchingResourceStacks`
rather than a single-winner `getResource`, for the same reason
`ResourceManager::read_stack`'s own doc already states for language files: a
pack that ships its own `font/<id>.json` to add a handful of custom bitmap
providers must not silently delete every lower-priority pack's (including the
jar's) own providers for that id. Before this was fixed, a pack overriding
`minecraft:default.json` (rather than declaring its own custom font id) lost
the jar's entire provider chain outright — every glyph the jar's `default.json`
would otherwise have supplied for that font, not just the ones the pack's own
file also names, silently vanished, which is a much bigger blast radius than
the codepoints the pack actually intended to add.

**Declaration order decides the winner, with no bias toward any provider
*type*.** A `space` provider does not implicitly outrank a `bitmap` one, or
vice versa — whichever provider is earlier in the flattened list wins,
exactly the same rule `duplicate_codepoint_within_one_providers_grid_uses_the_last_cell`'s
sibling tests already prove for a *single* provider's own grid. This was
re-verified by hand against vanilla's real `FontManager.loadResourceStack`/
`apply`'s double list-reversal (which exists to make cross-*pack* priority
work while preserving each pack's own JSON order, not to bias one provider
kind over another) and is pinned down by four tests in `tests/font.rs`:
space-before-bitmap and bitmap-before-space, each repeated through a
`reference` indirection, always with deliberately *different* advances on
each side so a coincidental match cannot hide a wrong winner. If a pack's
`space` provider seems to be losing a codepoint to a `bitmap` provider it
should win against, check which one is declared **earlier** in the pack's
own JSON (following any `reference` chain to its actual position) before
suspecting this crate's precedence logic — the four tests above are the
control that rules that logic in or out first.

### Watching a whole drawn string, not one codepoint at a time (`LODESTONE_TEXT_TRACE`)

`LODESTONE_FONT_METRICS` and `LODESTONE_FONT_TRACE` can only ever speak about
one codepoint's own numbers in isolation — neither can show a *sequence*
problem (glyphs that should sit apart touching, in a string with several
different fonts/providers in play), and neither can show a codepoint that
never reached this layer at all: a spacer character stripped or lost
somewhere between the packet and the `TextSpan` it should have become prints
nothing, because there is nothing to print for a glyph that was never in the
list. `LODESTONE_TEXT_TRACE` is the layer above both: it hooks
`VanillaFont::draw_resolved`, the one function every styled draw path
(`draw_legacy`, `draw_spans`, `draw_plain` — chat, tab list, boss bar
titles, container labels, everything) funnels through, so it needs no
per-screen wiring of its own.

Set it to `all` to trace every styled string drawn that frame, or to a
substring the drawn text must contain (checked against the codepoints
actually reaching this function, after component flattening and bidi
reordering) to isolate one. Each drawn string prints a `begin`/`end` pair
bracketing one line per glyph:

```text
lodestone-shell: TEXT_TRACE begin glyphs=3 x0=87.000 y0=12.000 scale=1.000
lodestone-shell: TEXT_TRACE cp=U+753C font=nameplates:default provider=bitmap:nameplates/font/backgrounds/b1.png advance=2.000 pen_x_before=87.000 pen_x_after=89.000 drawn_w=1.000
lodestone-shell: TEXT_TRACE cp=U+2007 font=nameplates:default provider=space advance=0.000 pen_x_before=89.000 pen_x_after=89.000 drawn_w=0.000
lodestone-shell: TEXT_TRACE cp=U+753D font=minecraft:default provider=unihex advance=9.000 pen_x_before=89.000 pen_x_after=98.000 drawn_w=4.500
lodestone-shell: TEXT_TRACE end total_width=11.000
```

Read it left to right: `font=` is which font's cell actually won this
codepoint — compare it against the `Style.font` a message intended, and
against a same-codepoint `LODESTONE_FONT_TRACE` run's `WINS` line, to catch
a font resolving differently at draw time than expected. `provider=` and
`drawn_w=` are read from *that* same font, so a measure/draw font mismatch
(a real hypothesis this tool exists to rule in or out, not just an example)
would show up as `drawn_w` disagreeing with `advance` for no reason a
fixture would explain. Most directly for a missing-gap report: read
`pen_x_after` of one line against `pen_x_before` of the next — they must be
equal, and a run where a glyph's own `drawn_w` reaches past the *next*
line's `pen_x_before` is the overlap, named exactly. And if the codepoint
you expected between two panels never appears as its own line at all, it
did not reach `draw_resolved` — the loss is upstream of this file entirely,
in component flattening or packet decode, not in anything font-shaped.

Only the main pass is traced, never the drop-shadow copy: the shadow's
positions are a fixed per-glyph offset from the main pass's own, so tracing
it would double every line without adding information.

### The per-server policy (`menu::servers::ServerPackPolicy`)

`Enabled`/`Disabled`/`Prompt`, matching vanilla's `ServerData.ServerPackStatus`
field-for-field, including the on-disk shape: an optional `acceptTextures`
boolean in the server-list JSON (`true`/`false`/absent), the same tri-state
`FIELD_CODEC` vanilla's NBT uses. The Add/Edit Server screen's "Server
Resource Packs" row (`RESOURCE_PACK_ROW`) cycles it — this used to be
permanently disabled because `ServerEntry` carried no field for it to
cycle. `app/menus.rs`'s `MenuAction::Connect` handler calls
`net::set_pending_server_pack_policy(entry.pack_status)` immediately before
dialing, and `NetClient::connect_impl` reads (and resets) it once per
connect.

## How to change it, and the gotchas

- **The live prompt does not write the answer back to the saved
  `ServerEntry`.** Vanilla does (accepting sets the server's own
  `ServerPackStatus` to `Enabled`, declining an optional pack to
  `Disabled`). This client's per-server row is a manual, ahead-of-time
  setting only; wiring the reverse sync would mean reaching from the net
  thread back into the server-list file, which is a bigger seam than this
  fix needed.
- **`net::PENDING_PACK_POLICY` is a one-shot global, not a `connect`
  parameter.** `Sim::connect`'s signature is fixed and threaded through
  every session kind (direct connect, LAN, singleplayer), none of which
  but a saved-server multiplayer join has a policy to carry. Set it right
  before the `Sim::connect`/`connect_as` call that matters; anything else
  leaves the default (`Prompt`).
- **Browser build: the dialog shows, downloading does not work.**
  `spawn_pack_download`'s wasm32 arm reports `FailedDownload` immediately
  rather than attempting anything — `reqwest` is native-only in this
  crate's dependency graph (see `Cargo.toml`'s target-split section), and
  there is no filesystem-backed cache to fall back to that a browser could
  reach either. This is a real gap, not a stub: `cargo xtask wasm-check`
  is the enforcement (33/33 confinement rules pass, including
  `lodestone-shell`'s thread/instant/systemtime bans), so if a future
  change needs the download to work in a browser it needs its own design,
  not a `#[cfg]` flip.
- **A pack is never written to disk.** The verified bytes live only in
  `resources.rs`'s in-memory cell for the session. There is therefore no
  "resource pack cache directory" to document, clean up, or bound
  separately from `MAX_PACK_SIZE_BYTES` itself.
- **Font warnings distinguish timing from bad content.** A missing custom font
  is retried after every pack-generation change, not every frame. If its
  `load resource-pack font <name>: font not found` warning persists after the
  server reports `SuccessfullyLoaded`, inspect the pack's
  `assets/<namespace>/font/<name>.json` and its providers; a rejected,
  corrupt, or genuinely non-providing pack still correctly falls back to the
  default font.
- **A pack author's JSON is parsed with vanilla's tolerance, not
  `serde_json`'s.** `GsonHelper.parse` builds a `JsonReader` at
  `Strictness.STRICT` and then calls the type adapter's `read` on it — a Gson
  adapter consumes one value and stops, and this hand-rolled path never asserts
  end-of-document afterwards (`Gson.fromJson(Reader, Class)` would have). So the
  real client silently ignores anything after the root value, and
  `serde_json::from_slice` does not: it returns `trailing characters at line N
  column M`. `lodestone_assets::json::from_slice_lenient` is the port of that
  behaviour — lenient about *trailing* content only, still strict about the value
  itself, so a truncated or JSON5-flavoured document is still an error.

  This is not a hypothetical tolerance. Measured on a real, widely-used 16× pack
  the owner has installed: **23** of its `textures/item/*.png.mcmeta` files carry
  one closing brace too many. Rejecting the metadata cost the whole *texture*,
  because `AtlasBuilder::load` treats a metadata failure as a texture failure —
  so `ItemAtlas` stitched **692** sprites instead of **715**, and 23 items drew
  as empty wells while every other item in the same pack re-textured correctly.
  Nothing was red. If you add a parser for anything a pack author writes, reach
  for `from_slice_lenient`; keep `serde_json::from_slice` for documents we
  produced ourselves, where trailing bytes really are a bug.
- **`load_item_atlas` now says whether the pack actually reached it.** The log
  line carries `from_packs = ItemAtlasReport::layered_sprites` (stitched sprites
  served by a layer above the built-in pack) beside `pack_layers`, and warns when
  `pack_layers > 0 && from_packs == 0`. That pair is the only thing that
  separates "the pack's item art is in the atlas" from "the atlas was rebuilt and
  nothing changed" — every other counter reads identically in both cases, and on
  screen they are indistinguishable from a pack that simply overrides no items.
  A second warning names up to eight `missing_textures`, which was previously a
  bare count; with the jar alone it is always 0, so any non-zero value is
  something a pack introduced.
- **The item atlas's live refresh used to be coupled to the *block* atlas's, and
  that was a shipped bug: "the server's texture pack applied to fonts but not to
  my items — held, slot, or on the ground".** `app::redraw`'s reload block was one
  `if let Some(atlas) = sim.reload_resource_pack_atlas()`, and everything inside
  it — the GUI atlases, the flat `ItemAtlas`, the glint sheet, the 3-D block-item
  pass, the special-icon latch — only ran when that returned `Some`.
  `Sim::reload_resource_pack_atlas` advances `last_pack_generation` **before** its
  own guards and returns `None` on three further conditions (no `net`, no
  `vanilla_atlas`, or its own `BlockResources::load(true)` falling back to the
  demo palette). Each of those consumes the generation, so a single such frame
  stranded every icon surface on the previous pack **for the rest of the
  process** — no later retry, because the counter only moves forward and the next
  comparison is already equal.

  Fonts were immune, and that asymmetry is the diagnostic: `vanilla_font`
  re-resolves lazily per generation (a *pull*), while every icon surface was
  pushed exactly once from a consumed edge. **A push keyed on a consumed counter
  has no second chance; a lazy pull keyed on the same counter has one every
  frame.** When something updates on a pack change and something else does not,
  check which of the two shapes each uses before looking at either loader.

  Fixed by giving the icon surfaces their own latch —
  `WindowApp::last_icon_pack_generation`, compared against
  `resources::pack_generation()` directly. Only `reload_block_atlas` and
  `attach_item_models` remain inside the `if let`: the latter re-binds the very
  GPU objects the former replaces, so it is meaningless on a frame where that did
  not run. Nothing else in the block reads the block atlas at all, so gating them
  on it was never right on its own terms either.

- **`decide_resource_pack_push` is a pure function, tested against a full
  truth table.** If vanilla's own condition in
  `ClientCommonPacketListenerImpl.handleResourcePackPush` ever needs
  re-deriving, start there rather than re-deriving it from the auto-apply
  prose above — the prose is a summary, the Java is the source.

## Configuration

- `menu::servers::ServerPackPolicy` on each `ServerEntry` — set from the
  Add/Edit Server screen's "Server Resource Packs" row, persisted in
  `servers.json` under the `acceptTextures` key (see `data_dir`/
  `servers_path`).
- `net::MAX_PACK_SIZE_BYTES` (native only) — the 250 MiB download cap.
  Vanilla's own constant; change it only with a reason as good as vanilla's.
- `LODESTONE_FONT_METRICS` (any value) — per-glyph bitmap-font metrics dump
  to stderr; see "Diagnosing a pack font's spacing" above.
- `LODESTONE_FONT_TRACE` (comma/whitespace-separated codepoints) — per-
  codepoint provider-precedence trace to stderr; see "Which provider
  actually wins a codepoint" above.
- `LODESTONE_TEXT_TRACE` (`all`, or a substring the drawn text must
  contain) — per-glyph layout trace to stderr for a whole drawn styled
  string; see "Watching a whole drawn string, not one codepoint at a time"
  above.

## Dependencies

- `reqwest` (native only) — the HTTP(S) client, mirroring how
  `remote_skins.rs`/`skin_fetch.rs` already use it.
- `sha1` — pack hash verification, version-free.
- `lodestone_assets::ZipSource` — reads the verified bytes as a pack, the
  same reader a local `.zip` pack in the Resource Packs screen uses
  (`crate::resources::open_pack_source`).
- `menu::confirm` — the geometry (`row_slot`, `confirm_rects`,
  `ConfirmWidgets`) the prompt dialog is built from.
