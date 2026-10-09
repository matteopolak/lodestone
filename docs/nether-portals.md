# Portal connection travel

## What it is

The shared connection lifecycle for Nether portals, End portal entry and End gateway contact. Native and browser connections use the same contact controller, destination preparation, dimension commit and return-home reset, keeping their own platform timers and native persistence.

## How it works

`server::connection_travel::TravelController` owns the active destination, portal exposure, gateway cooldown, pending preparation and transition epoch. `Destination::Home` is the primary source retained at join; `Destination::Dimension` owns a sibling source. Each event-loop pass promotes a staged destination and clones its source locally before dispatch, chunk admission, relight or feed drains; `dimension_scoped_handles` selects that source's block-entity registry and delayed-block-tick feed (with join-time fallbacks).

**Contact.** Both timer arms call `TravelController::tick` after readiness-gated vitals, using the cell at the player's feet. End gateway contact precedes ordinary portals, uses live or resident block-entity metadata, refuses mounted players, admits the arrival footprint first, and sends a same-dimension teleport recentering the view (40-tick cooldown). Nether and End portals share `PortalTracker`: zero delay triggers on the first contact tick, delay 80 on tick 81; exposure decays by four outside a portal; game rules are read each tick so a changed delay applies to existing exposure. A successful trip starts a ten-tick player portal cooldown that re-arms while standing in the arrival portal.

**Preparation.** It owns its source and immutable request data and sends no packets; each connection polls it as its own event-loop branch. Moving off the contact cancels the pending request before its next poll; a dimension reset advances the epoch and drops it (a native worker mid-answer may finish with no sender). Terrain prepared before cancellation stays reusable; world edits are not rolled back.

- Nether travel scales coordinates by the dimension ratio, then admits the destination's indexed candidates and bounded fallback footprint; a found portal gets a second admission for its rectangle. Creating one searches a 33-by-33 site, admitting its frame and side-slice reach `[-17, +18]` along the portal axis and `[-17, +17]` across. The return leg admits the home source, including a cold search away from the join view.
- The temporary query adapter uses resident point reads, records unavailable or busy data and rejects an incomplete result instead of accepting placeholder air; it holds no terrain cache or global cache lock across an await. Owned-source admission drains the bounded `ColumnPipeline` and releases each payload (no second full-footprint vector); the legacy borrowed-source path keeps its original admission.
- End preparation repairs the fixed arrival platform before destination chunks are sent. Writes use resident-only mutation: busy reads/writes yield and retry, missing columns re-admit in bounded groups, and a capable source declining resident mutation errors instead of falling through to cold generation (legacy sources keep ordinary mutation); long write sequences yield every 64 cells.
- An uninitialised End fight completes its deterministic arena block plan before consuming the one-time claim; only the final claim and entity allocation have no await between them. An initialised fight skips arena repair. Cancellation keeps completed edits but cannot consume the claim mid-write; concurrent preparations may repeat identical writes while the claim still gates allocation.

**Commit.** A dimension commit sends the dimension-change and placement pair, forgets the previous view, publishes the destination cache centre and rebuilds the entire join stream, clearing old encode futures, unsent reactive batches, relight and tick-update queues (required even when both dimensions share chunk coordinates; the platform loop drops its relight job first). `reset_player` clears movement and fall baselines, sets `client_loaded` false and publishes the destination `TickAnchor`; fresh `PlayerLoaded` is required before vitals or contacts resume. World ticks continue during travel. Death away from home uses the same reset and stages `Destination::Home`.

### Leaving the End

End portal contact inside the End sets a flag (`take_end_exit_contact`) that the loop turns into `connection_travel::begin_end_exit`; per-connection state is `connection_travel::EndExit`.

- **Credits not yet seen:** the server sends game event 4 (win game) with parameter `1.0`, records `seenCredits` as a byte in the player's preserved save fields (so it persists across rejoin) and waits. The client answers with the perform-respawn command when the credits end; `apply_client_command` records that (`EndExit::request_respawn`) only while an exit is pending (a living player's stray perform-respawn is still ignored).
- **Credits already seen:** no announcement; straight home like any portal traveller (picked up after the next client packet).

Either way `connection_travel::end_exit_respawn` resolves the player's bed (falling back to world spawn) against the *home* source, sends a dimension change to the Overworld with all player data kept (not a death) and hands the position to the `dimension_reset` rebuild. Non-player entities are not carried through.

## How to change it

Keep contact decisions and commit in the shared controller; platform loops own timer polling, native saves and their relight task lifetime, and must end a reset pass before using another outgoing-source handle (a long-lived browser relight owns its dimension source). Extend a destination's admission footprint whenever its resolver reads farther, and scan with the source's resident point capability (cloning `ChunkColumn` is a deep copy). Native queries run on the world-generation dispatcher; the browser performs the bounded search or creation scan synchronously after admission with no cooperative cursor, so a green compile gives no latency bound (and a single scalar generator call can occupy a long poll).

Tests: `connection_travel::tests` cover timing, decay, rule changes, cooldown, cancellation, fractional negative return coordinates, markers, home promotion, readiness and anchor publication; scripted resident contention distinguishes busy from missing terrain and rejects unsupported mutation without a scalar fallback; `join_scheduler::tests::dimension_reset_cancels_completed_and_pending_encodes_at_the_same_coordinate` includes a cancellation-disabled control exposing stale delivery. None proves the browser loading cover or rendered chunks changed (needs a real browser round trip). Known gaps: resource selection exposes only block entities and delayed block ticks (`PlayerTicketGuard` stays bound to its join-time ticket store; sibling mob and explosion handles are not returned, so ticket/mob/explosion parity is unestablished); bed respawn validates its saved position against the current source before the home reset; the loading cover is driven by the dimension-change consumer at commit, with no earlier cold-preparation cover.

## Configuration

`players_nether_portal_creative_delay` (default `0`), `players_nether_portal_default_delay` (default `80`), `allow_entering_nether_using_portals` (gates entry, not the return leg). Tracked view radius and hosting protocol decide streaming and dimension-change frame support. No environment variable.

## Dependencies

`ChunkSource` (sibling sources, resident terrain, portal indexes, registries, tick feeds); `portal` (contact timing, destination geometry); `dimension` (scaling, keys); `JoinChunkStream`, `OrderedJoinEncodes`, `ServerProtocol`; `WorldStateHandle` (rules, tick anchors). Native timers use Tokio, browser timers `BrowserInterval`.
