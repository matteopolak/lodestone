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

## Dependencies

- `reqwest` (native only) — the HTTP(S) client, mirroring how
  `remote_skins.rs`/`skin_fetch.rs` already use it.
- `sha1` — pack hash verification, version-free.
- `lodestone_assets::ZipSource` — reads the verified bytes as a pack, the
  same reader a local `.zip` pack in the Resource Packs screen uses
  (`crate::resources::open_pack_source`).
- `menu::confirm` — the geometry (`row_slot`, `confirm_rects`,
  `ConfirmWidgets`) the prompt dialog is built from.
