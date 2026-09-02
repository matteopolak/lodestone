# Server resource packs

## What it is

The end-to-end flow for a server-pushed resource pack: the accept/decline
prompt, the per-server policy that can skip it, the download/verify/apply
pipeline, and how a downloaded pack actually reaches every GPU surface that
draws from an atlas (block/item/GUI/special-icon). Also covers the general
single-winner-vs-merged-stack rule for how *any* pack-loaded resource type
(vanilla's own jar included, as the lowest-priority layer) composes across a
pack stack, and the diagnostic env vars for a pack author's font.

## How it works

### The wire and the decision

`ClientboundResourcePackPushPacket`/`Pop` decode into `ClientEvent::ResourcePackPushed`/
`Popped` and are answered directly inside the net thread's own connection
loop, not through the ordinary `forward`/`Sim::poll_net` path every other
event takes — because responding may need to *do* something (spawn a
download) rather than just enqueue a reply byte.
`route_resource_pack_pushed` reproduces vanilla's own condition exactly: a
non-`http(s)` URL is always `INVALID_URL`, before the per-server policy is
even read; otherwise `decide_resource_pack_push(policy, required)` — a pure,
fully-tested function — answers auto-accept, auto-decline, or prompt.
`Disabled` still **prompts** a *required* pack rather than auto-declining it
— vanilla will not silently drop a player over a pack they never personally
answered.

### The prompt

A second, independent confirm-style overlay, reusing shared geometry helpers
but not the general `Confirm` screen itself — it must be able to open over an
in-progress connection screen as well as a live world (a pack can be pushed
during Configuration, before Play), which the generic confirm screen's
"reached only from World Select" contract does not allow. The net thread
never touches the UI directly: it writes the pending prompt into a cell the
UI reconciles into the screen every frame, the same pattern used for other
session-driven overlays. Answering sends the response over a **dedicated
channel**, not the generic outbound-action queue, and is drained
asynchronously by the net thread's own loop — up to ~15 ms later on native,
since that loop also flushes actions and waits on other events first. The UI
side must remember which prompt id it has already answered until the net
thread's own drain clears the pending-prompt cell, or a frame's reconcile can
run in that window, see the screen it just closed reopen, and read it as "the
choice menu did nothing" — a real player-reported symptom traced to exactly
this ordering gap.

A **required** pack the player declines disconnects the session immediately
(vanilla self-disconnects rather than waiting for the server to notice a
client that will never load the pack).

### Download and verification (native only)

Downloading runs on its own OS thread with its own single-threaded runtime,
mirroring the same pattern used elsewhere for a slow/hostile network fetch
that must not stall the connection loop (which also drives movement and
keep-alives). The stream aborts the instant either the declared
`Content-Length` or the running total exceeds vanilla's own 250 MiB cap.
Hash verification follows vanilla's own leniency exactly: a well-formed
40-hex-character SHA-1 is checked and any mismatch is rejected outright, but
an absent or malformed hash **skips** verification rather than failing it.

### Applying it

Nothing is ever extracted to disk. The verified bytes go straight into the
same version-free zip reader a local `.zip` pack already uses, are stored in
a process-wide cell, and a generation counter bumps — the identical
live-reload signal the local Resource Packs screen already polls every
frame, so a server pack reaches the block atlas through the same pipeline a
local one does, with no second wiring path to drift out of sync. The server
pack is prepended ahead of the local selection (vanilla's own "downloaded
pack goes on top") and never appears in the local pack-selection screen's own
list, matching vanilla keeping downloaded packs out of the user-visible
repository.

### What a reload has to re-attach — the borrow/own split

`Sim::reload_resource_pack_atlas` rebuilds the classifier, the block atlas
and its models, and re-meshes every loaded column. Everything past that is
GPU-side catch-up, and the entire difficulty here is that **not every GPU
pass owns what it draws with**:

| shape | symptom of a missed re-attach | fix |
|---|---|---|
| **borrows** another renderer's atlas/buffer (the 3-D block-item pass in HUD/container icons; `wgpu` resources are `Arc`-backed, so a borrowing pass does not error — it keeps sampling the dropped object, geometry re-baked against the new packing) | blank wherever a new UV lands on padding, a frozen animated icon, a stale tint palette | re-attach explicitly in the reload block, in the same commit that adds the borrow |
| **owns** its sheet outright but builds it **lazily** on first use (the special-renderer icon pass — chest/shulker/banner/shield/skull/player-head) | keeps sampling a perfectly valid sheet belonging to the **previous** pack | drop it and let the lazy build re-run; and clear any "already tried" latch, or a build that failed once (no pack stack yet) never recovers for the rest of the process |

The flat item-sprite stream is immune to the first failure mode purely
because its atlas and its UVs come from the same object and are replaced
together — which is why the symptom this shipped as read as "3-D block icons
in menus broke, flat items are fine" rather than pointing at the real cause.
No hermetic gate can see any of this, since every gate builds its renderer
once and never reloads; a gate that actually reloads and compares before/after
sheet counts is the only way to catch it.

A second, related trap: **the item atlas's own reload used to be coupled to
the block atlas's reload succeeding at all** — everything inside that one
`if let Some(atlas) = …` block (GUI atlases, the flat item atlas, the glint
sheet, the 3-D block-item pass, the special-icon latch) shared one guard, and
that guard's underlying generation counter had already **advanced** before
three of its own failure conditions (no net session, no vanilla atlas, or the
block-resource load itself falling back to the demo palette) could return
`None`. Any one of those consumed the generation permanently — no later
retry, because the comparison the next frame makes is already equal — and
stranded every icon surface on the previous pack **for the rest of the
process**. Fonts were immune to this specific bug because they re-resolve
**lazily** every frame (a pull, always checking the current generation)
rather than being **pushed** once from a consumed edge — a push keyed on a
counter that already advanced has no second chance, a pull keyed on the same
counter gets one every frame. The fix gives icon surfaces their own
independent latch compared against the pack generation directly, rather than
riding the block atlas's own optional return value.

The default font had a similar single-decision bug: it was resolved once
into a process-wide cache the first time anything asked, so a pack applied
afterwards — server-pushed or locally selected — could never replace it,
while text drawn with an explicitly-named custom font was unaffected (that
cache already keyed on generation). It is now keyed on generation like its
sibling, with the renderers that hold a resolved font each re-asking at the
top of their own draw through one shared refresh function, rather than three
independent copies of the same fix.

### Single winner vs. merged stack

Two lookups exist (`ResourceManager::read` and `read_stack`), and getting the
choice wrong for a given resource type is silent in both directions — a pack
simply does not do what it visibly should, with nothing erroring. The rule
comes from vanilla's own loader method name, not a blanket policy:
`getResource`/`listMatchingResources` is **single winner** (a texture PNG, a
`.mcmeta`, a blockstate/model/item-definition/particle-definition JSON — a
server pack replacing one of these replaces the whole file, correctly);
`getResourceStack`/`listMatchingResourceStacks` **merges every layer, lowest
priority first** (language files, fonts, atlas source lists like
`armor_trims.json`, and item tags — each honouring its own layer's
`"replace"` flag where vanilla defines one). Vanilla's own jar is simply the
lowest-priority layer in either case, which is what makes a pack able to
*extend* rather than only replace a merged resource. The block/item atlases
this client builds do not go through `atlases/*.json` at all — they enumerate
textures directly via a listing that already unions paths across the whole
stack, so this class of bug structurally cannot apply to them.

A pack author's JSON is parsed with **vanilla's own tolerance**, not
`serde_json`'s strict end-of-document check — Gson's adapter reads one value
and stops, silently ignoring trailing content a pack author's editor may have
appended. A real, widely-used pack was measured to have 23 of its
`.png.mcmeta` files carrying one extra closing brace; rejecting the metadata
(as strict parsing would) costs the **whole texture**, not just the
metadata, dropping 23 real items to empty wells while every other item in
the same pack re-textured correctly and nothing logged as an error. Any new
pack-facing JSON parser should use the lenient-trailing-content reader, and
keep the strict one only for documents this codebase produces itself.

### Diagnosing a pack font's spacing

Three env-gated diagnostics exist for the class of bug where a pack's custom
font measures correctly in isolation and still overlaps on screen:
`LODESTONE_FONT_METRICS` dumps every loaded bitmap glyph's declared
size/grid/measured-ink/advance to stderr (the discriminating case is a
non-integer `pixel_scale` — a sheet cell physically larger or smaller than
the font's declared height, which all three of vanilla's own bitmap sheets
happen to avoid but a pack-provided one commonly is not);
`LODESTONE_FONT_TRACE=<codepoints>` shows every provider that declares a
given codepoint and which one actually won (**declaration order decides,
with no bias toward any provider type** — a `space` provider does not
implicitly outrank a `bitmap` one); and `LODESTONE_TEXT_TRACE` traces a whole
drawn string's per-glyph pen position, the layer above both, for a *sequence*
problem (glyphs that should sit apart touching) or a codepoint that never
reached the draw layer at all. Provider order **across multiple packs is a
merge, not an override** — a pack overriding `minecraft:default.json` used
to silently delete the jar's entire provider chain rather than only adding
to it, which is a much larger blast radius than the pack author intended.

### The per-server policy

`Enabled`/`Disabled`/`Prompt`, matching vanilla's own tri-state field-for-field
(an optional boolean in the server-list JSON: true/false/absent). Set
immediately before the connect call that matters — it is a one-shot global
read once per connect, not a parameter threaded through every session kind,
since only a saved-server multiplayer join has a policy to carry at all.

## How to change it, and the gotchas

- **The live prompt never writes the answer back to the saved server entry.**
  Vanilla does; this client's per-server row is a manual, ahead-of-time
  setting only.
- **Browser build: the dialog shows, downloading does not work.** The HTTP
  client this uses is native-only, and there is no filesystem-backed cache to
  fall back to — a real gap enforced by the wasm confinement check, not a
  stub to quietly extend with a `#[cfg]`.
- **A pack is never written to disk** — the verified bytes live only in an
  in-memory cell for the session, so there is no cache directory to clean up
  or bound beyond the download size cap itself.
- Whenever a GPU pass **borrows** another renderer's atlas or buffer rather
  than owning it, its re-attach belongs in the reload block in the same
  commit that adds the borrow — nothing will be red if it is skipped.
- Whenever a GPU pass builds its own resource **lazily**, its reload also
  belongs in the reload block, and it needs no `attach_*` call to sit beside
  — which is exactly why it is easy to miss.

## Configuration

- `menu::servers::ServerPackPolicy` on each server entry, persisted in
  `servers.json`.
- The 250 MiB download cap — vanilla's own constant; change only with as good
  a reason as vanilla's.
- `LODESTONE_FONT_METRICS`, `LODESTONE_FONT_TRACE`, `LODESTONE_TEXT_TRACE` —
  stderr diagnostics, see above.

## Dependencies

`reqwest` (native only) for the HTTP(S) client; `sha1` for hash verification
(version-free); `lodestone_assets::ZipSource` for reading the verified bytes
as a pack, the same reader a local `.zip` pack uses; the shared confirm-menu
geometry helpers for the prompt dialog's layout.
