# Server resource packs

## What it is

The flow for a server-pushed resource pack: accept/decline prompt, per-server policy, download, verify and apply, and how a pack reaches every GPU surface that draws from an atlas. Also the single-winner versus merged-stack rule for how pack-loaded resources compose (the vanilla jar is the lowest layer), and the font diagnostics for pack authors.

## How it works

### The wire and the decision

Push and pop packets decode into `ClientEvent::ResourcePackPushed`/`Popped` and are answered inside the net thread's connection loop, not through `forward`/`Sim::poll_net`, because responding may spawn a download. `route_resource_pack_pushed` follows the reference: a non-`http(s)` URL is always `INVALID_URL` before policy is read; otherwise the pure `decide_resource_pack_push(policy, required)` answers auto-accept, auto-decline or prompt. `Disabled` still prompts for a required pack.

Server-side, a response decodes to the version-free `ServerBound::ResourcePackResponse` and is recorded by `ResourcePackPushFeed`; hosts read it with `drain_responses`. Recording is policy-neutral (no accept, reject or disconnect), and unknown ordinals are ignored.

The server-side `ResourcePackPush` carries a parsed `ResourcePackUrl` (absolute `http`/`https` only), converted to text only when writing the packet.

### The prompt

A separate confirm-style overlay reusing shared geometry helpers but not the `Confirm` screen, since it must open over a connection screen (a pack can be pushed in Configuration) as well as a live world. The net thread never touches the UI: it writes the pending prompt into a cell reconciled each frame. Answering uses a dedicated channel drained by the net loop (up to about 15 ms later on native). The UI must remember the prompt id it answered until the net thread clears the cell, or a reconcile in that window reopens the closed screen and reads as "the choice menu did nothing". Declining a required pack disconnects immediately.

### Download and verification (native only)

Downloading runs on its own OS thread with a single-threaded runtime so a slow fetch cannot stall movement and keep-alives. It aborts when the declared `Content-Length` or the running total passes 250 MiB. A well-formed 40-hex SHA-1 is checked and a mismatch rejected; an absent or malformed hash skips verification.

### Applying

Nothing is written to disk. Verified bytes go into the version-free zip reader a local `.zip` pack uses, stored in a process-wide cell, with a generation counter bump: the same live-reload signal the Resource Packs screen polls, so there is one wiring path. The server pack is prepended ahead of the local selection and never listed in the selection screen.

The built-in archive's parsed directory is cached per asset installation (native identity: path, length, mtime; browser: immutable bundle storage). Each loader gets its own light stack wrapper sharing bytes, directory and normalised index, so renderers never rescan the jar. The built-in `version.json` becomes `VersionMeta` with a validated `VersionId` (release, snapshot and pre-release spellings kept; empty, whitespace or path-like values fail at the asset boundary), turned back to text only as the built-in pack's description.

### What a reload re-attaches: borrow versus own

`Sim::reload_resource_pack_atlas` rebuilds the classifier, block atlas and models, and re-meshes loaded columns. The rest is GPU catch-up, and not every pass owns what it draws with:

| shape | symptom of a missed re-attach | fix |
|---|---|---|
| borrows another renderer's atlas or buffer (the 3-D block-item pass in HUD and container icons; `wgpu` resources are `Arc`-backed, so it keeps sampling the dropped object) | blank where a new UV lands on padding, frozen animated icon, stale tint palette | re-attach in the reload block in the same commit that adds the borrow |
| owns its sheet but builds it lazily (special-renderer icons: chest, shulker, banner, shield, skull, player head) | keeps sampling the previous pack's sheet | drop it so the lazy build reruns, and clear any "already tried" latch |

The flat item-sprite stream is immune because its atlas and UVs are replaced together, which is why the symptom read as "3-D block icons broke, flat items fine". Hermetic gates build once and never reload; only a gate that reloads and compares before and after sheet counts sees this.

Related trap: the item atlas reload was coupled to the block atlas reload succeeding. GUI atlases, flat item atlas, glint sheet, 3-D pass and special-icon latch shared one `if let Some(atlas)` guard, and the generation counter had already advanced before three failures (no net session, no vanilla atlas, block load falling back to the demo palette) could return `None`, stranding icons on the previous pack for the process. Fonts were immune because they re-resolve lazily each frame (a pull), where a push on a consumed edge gets no second chance. Icon surfaces now have their own latch compared against the pack generation directly.

The default font had a similar single-decision bug: resolved once into a process-wide cache, so a later pack could not replace it (explicitly named custom fonts keyed on generation). It is now keyed on generation, and renderers holding a resolved font re-ask at the top of their draw through one shared refresh function.

### Single winner versus merged stack

`ResourceManager::read` and `read_stack` are the two lookups, and a wrong choice is silent. The rule follows the reference loader's semantics. Single winner: texture PNGs, `.mcmeta`, blockstate, model, item-definition and particle-definition JSON (a pack replaces the whole file). Merged, lowest priority first: language files, fonts, atlas source lists like `armor_trims.json` (26.2 only; 26.3 has no such descriptor and `TrimAtlas` palettes entity trim textures itself from `textures/palettes/trim_base.png` and `textures/palettes/trim/<suffix>.png`), and item tags, each honouring its layer's `"replace"` flag where defined. The vanilla jar is the lowest layer either way, which lets a pack extend a merged resource. Our block and item atlases enumerate textures through a listing that already unions the stack, so this bug class cannot apply.

Pack JSON is parsed with the reference's tolerance (read one value, ignore trailing content), not `serde_json`'s strict end check. A widely used pack had 23 `.png.mcmeta` files with one extra closing brace; strict parsing drops the whole texture, emptying 23 items with nothing logged. New pack-facing parsers use the lenient reader; strict is for documents we produce.

### Diagnosing pack font spacing

- `LODESTONE_FONT_METRICS` dumps every bitmap glyph's declared size, grid, measured ink and advance to stderr (the discriminating case is a non-integer `pixel_scale`, a sheet cell larger or smaller than the declared height, which the three vanilla sheets avoid and packs often do not).
- `LODESTONE_FONT_TRACE=<codepoints>` lists every provider declaring a codepoint and the winner (declaration order decides; a `space` provider does not outrank a `bitmap`).
- `LODESTONE_TEXT_TRACE` traces a drawn string's per-glyph pen positions, for sequence problems or codepoints that never reached drawing.

Provider order across packs is a merge: a pack overriding `minecraft:default.json` once deleted the jar's whole chain instead of adding to it.

### Per-server policy

`Enabled`/`Disabled`/`Prompt` mirrors the reference's tri-state (optional boolean in the server-list JSON). It is a one-shot global set right before the connect call and read once per connect, since only a saved-server join has a policy.

## How to change it

- The live prompt never writes the answer back to the saved server entry (the reference does); the per-server row is a manual setting.
- Browser build: the dialog shows but downloading does not work (the HTTP client is native-only, no filesystem cache); the wasm confinement check enforces this, so do not paper over it with `#[cfg]`.
- Packs are never written to disk, so there is no cache to bound beyond the size cap.
- A GPU pass that borrows another renderer's atlas or buffer gets its re-attach in the reload block in the same commit; a lazily built one needs its reload there too, with no `attach_*` to sit beside.

## Configuration

- `menu::servers::ServerPackPolicy` on each server entry (`servers.json`).
- The 250 MiB cap (the reference's own constant).
- `LODESTONE_FONT_METRICS`, `LODESTONE_FONT_TRACE`, `LODESTONE_TEXT_TRACE`.

## Dependencies

`reqwest` (native only), `sha1`, `lodestone_assets::ZipSource` (shared with local packs), and the shared confirm-menu geometry helpers.
