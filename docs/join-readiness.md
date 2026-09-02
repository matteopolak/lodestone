# Join readiness

## What it is

The rule that decides when a join stops showing the loading screen and starts
showing the world. It is **two** conditions, not one: the terrain rule (the
player's own chunk column has arrived) *and* an asset rule (no server-pushed
resource pack is still downloading or waiting to be applied to the block atlas).
Before the asset half existed the screen cleared on the column alone, so a server
that pushes a pack dropped the player into a world wearing the *previous* pack's
textures, hitched for about a second while the atlas rebuilt, and popped.

The vocabulary and the decision live in `crate::menu::loading`; the observations
are gathered by `Sim::world_wait`; the single consumer is the loading-overlay
block in `WindowApp::redraw`.

## How it works

### The sequence a join actually goes through

1. `Sim::connect` → `Sim::attach_net`. `sim.net` is `Some` from here on, i.e.
   **before** the handshake completes. `ConnectPhase::Connecting`.
2. Handshake and login run inside `lodestone-client` behind one
   `ClientBuilder::connect().await`, so the shell observes no sub-steps.
   `NetUpdate::ConnectPhase(Joining)` when the client handle exists.
3. **Configuration.** `ClientEvent::ResourcePackPushed` can arrive here.
   `crate::net::route_resource_pack_pushed` applies the policy table and, on an
   accept, `begin_accept` sends `ACCEPTED`, takes a `PackApplyInFlight` guard and
   spawns a detached OS thread to download.
4. Play begins. `NetUpdate::ConnectPhase(LoadingTerrain)` sets
   `Sim::terrain_wait_started` — **the one clock both halves of the readiness
   rule are measured against**. `Screen::Playing`.
5. Every frame, `WindowApp::redraw` renders the world *and then* draws the
   loading frame over it as an opaque overlay while `Sim::world_wait` is
   `Some`. The world rendering underneath is deliberate: chunks must keep
   meshing and uploading, and remote-player skin fetches are started from that
   same path.
6. The download thread verifies the SHA-1, calls
   `crate::resources::set_server_pack` (which bumps
   `crate::resources::pack_generation`), reports `SUCCESSFULLY_LOADED`, and
   drops its guard.
7. On the next `redraw`, `Sim::reload_resource_pack_atlas` sees the new
   generation, reloads `BlockResources`, swaps the atlas and re-meshes every
   loaded column. **This is the visible second-long hitch.**
8. Now — and only now — `Sim::world_wait` returns `None`, the overlay stops, and
   the first world frame the player sees already wears the new pack.

### The readiness condition

`menu::loading::world_wait(TerrainWait, AssetWait) -> Option<WorldWait>`:

| half | function | holds while |
|---|---|---|
| assets | `assets_ready` | `packs_in_flight > 0`, or the block atlas is older than the current `pack_generation` |
| terrain | `is_level_ready` | the player's own column has not arrived (vanilla's `LevelLoadTracker.WaitingForPlayerChunk` rule) |

Assets are checked **first**, so when both are outstanding the screen names the
pack. That matches vanilla's own precedence: a resource reload is an `Overlay`
and the terrain wait is a `Screen`, and `Gui.update` paints the overlay in
preference to the screen.

`packs_in_flight` is `crate::net::packs_in_flight`, a process-wide count held up
by an RAII `PackApplyInFlight` guard taken in `begin_accept` and dropped when the
download thread ends — however it ends. It has to be process-wide because the
download thread holds no reference back to the session, the same reason the pack
bytes themselves land in a static in `crate::resources`.

`atlas_stale` compares `crate::resources::pack_generation` against the value
`Sim::reload_resource_pack_atlas` last consumed. It is only ever true for a
fraction of a frame, because that rebuild is synchronous. It exists for the race
the counter alone cannot cover: the download thread installs the bytes and only
*then* resolves, so a reader sampling between those two points sees a zero count
with a stale atlas and would dismiss one frame early. `packs_in_flight` is read
`Acquire` against the guard's `Release` decrement precisely so the generation
bump is guaranteed visible to a reader that sees the count reach zero.

### The timeout

30 seconds, shared with the terrain wait — the same `CLIENT_WAIT_TIMEOUT`
constant *and* the same clock, measured from the entry into
`ConnectPhase::LoadingTerrain`.

That is a port, not a convenience. Vanilla's `LevelLoadTracker` stamps
`Util.getMillis() + CLIENT_WAIT_TIMEOUT_MS` **once**, in `startClientLoad`, and
`WaitingForServer.loadingPacketsReceived` carries the same `timeoutAfter` into
`WaitingForPlayerChunk` unchanged: one deadline for the whole client load, not
one per sub-wait. Two waits sharing one deadline is the shape the record already
has.

Sharing the clock also settles the scope by construction. A pack pushed **during
a join** is inside the window and holds the screen; a pack pushed an hour into a
session is far past it and is never held. See the deviation below.

## How to change it, and the gotchas

**The ordering inside `WindowApp::redraw` is load-bearing and nothing in the type
system protects it.** `Sim::reload_resource_pack_atlas` must run *above* the
loading-overlay block. If it ran below, the frame on which a pack lands would
present one frame of the old atlas before the overlay went up, reinstating the
flash — and every unit gate would stay green, because the two blocks are
individually correct either way. `sim::tests::redraw_applies_a_pending_pack_before_it_asks_whether_to_stop_covering_the_world`
greps `redraw.rs` for exactly this, from a different file (a source-grep gate
placed inside the file it greps matches its own assertion string).

**The overlay must stay an overlay.** `loading_frame`'s backdrop is `Panorama`,
whose no-panorama fallback is a flat opaque fill, so nothing of the world shows
through — but the world is still *rendered* underneath, which is what keeps
chunks meshing and skin fetches flowing. Converting this to a full-frame
`owns_frame` screen would stop both and make the wait longer.

**Nothing can strand `atlas_stale` true.** `reload_resource_pack_atlas` records
the generation **before** its own three early returns (no session, no vanilla
atlas, a reload that fell back to the demo palette), so the latch always
advances even when the reload does nothing.

**Singleplayer pays nothing.** There is no pack push on the integrated server, so
`packs_in_flight` is 0 and the atlas is current: `assets_ready` is satisfied at
`Duration::ZERO`. It is an absence of work, not a delay — asserted at zero
elapsed by `menu::loading::tests::a_session_with_no_pack_is_ready_at_zero_elapsed`
so that a fixed wait of any length would fail.

**The browser never holds for this.** `spawn_pack_download`'s wasm32 arm reports
`FAILED_DOWNLOAD` immediately and drops its guard on return, because `reqwest` is
not in this crate's wasm32 dependency graph. No new clock, thread or
`Instant::now` was added anywhere; the elapsed time comes from the existing
`Sim::terrain_wait_started`, which is a `crate::platform::Instant`.

## Named deviations from vanilla

- **An in-play pack push does not cover the world here; in vanilla it does.**
  Vanilla's `LoadingOverlay` goes up for any reload, mid-session included, and
  paints over the level for its duration. Reproducing that needs a second clock
  and a screen reachable from mid-play, and the cost of getting it wrong is
  covering a live world, so it is left out rather than approximated. The
  mid-session hitch is therefore still visible; only the join is covered.
- **`SUCCESSFULLY_LOADED` is still sent when the bytes are installed, not when
  the atlas rebuild has run.** Vanilla sends it from
  `DownloadedPackSource.onReloadSuccess`, i.e. from the `LoadingOverlay`'s own
  completion callback, so it genuinely means "applied". A vanilla server holds
  the client in Configuration until it sees a terminal status (the
  `ServerResourcePackConfigurationTask` sits ahead of `JoinWorldTask` in the
  server's queue), so ours releases that gate one atlas-rebuild early. That is a
  protocol-fidelity gap, not a visible one — the client-side wait above covers
  the symptom either way — and closing it means making the response wait on a
  rebuild that only happens inside `redraw`, which risks deadlocking the
  Configuration phase on any path where the rebuild is a no-op. Left as is,
  deliberately.
- **Skins are not waited for, and should not be.** Vanilla resolves a skin
  asynchronously and never blocks on it: `SkinManager.createLookup` reads the
  future with `getNow` and falls back to `DefaultPlayerSkin.get(profile)`, and
  resolution is started lazily on the first `PlayerInfo.getSkin()` — at render
  time, not on packet receipt. `crate::remote_skins` has the same shape
  (`request_all` is driven from the world draw path and the draw falls back to
  the model's default sheet), so it is already vanilla-faithful and adding a wait
  would be inventing one. It does benefit incidentally: the world renders under
  the loading overlay, so holding the screen for a pack gives in-flight skin
  fetches strictly more time at no cost. The local player's own skin is fetched
  at sign-in by `crate::skin_fetch`, long before any join.

## Configuration

None of its own. It reads:

- `crate::resources::pack_generation` — bumped by `set_server_pack`,
  `set_selected_packs` and `set_mipmap_levels`.
- `crate::menu::servers::ServerPackPolicy` — the per-server Enabled/Prompt/
  Disabled setting that decides whether a push is auto-accepted at all. A pack
  that is declined or never accepted is never counted in flight.
- `menu::loading::CLIENT_WAIT_TIMEOUT` — 30 s, shared with the terrain wait.

## Dependencies

- `crate::menu::loading` — `TerrainWait`, `AssetWait`, `WorldWait`,
  `is_level_ready`, `assets_ready`, `world_wait`.
- `crate::net` — `packs_in_flight`, `PackApplyInFlight`, `begin_accept`,
  `spawn_pack_download`.
- `crate::resources` — `pack_generation`, `set_server_pack`.
- `crate::sim` — `Sim::terrain_wait`, `Sim::asset_wait`, `Sim::world_wait`,
  `Sim::reload_resource_pack_atlas`.
- `crate::app::redraw` — the single consumer.

See also [`resource-packs.md`](./resource-packs.md) for the pack stack itself and
[`player-skins.md`](./player-skins.md) for the skin pipeline.
