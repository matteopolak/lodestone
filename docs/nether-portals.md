# Portal connection travel

## What it is

The shared connection lifecycle for Nether portals, End portal entry, and End gateway contact. Native and browser connections use the same contact controller, destination preparation, dimension commit, and return-home reset while retaining their platform timers and native persistence.

## How it works

`server::connection_travel::TravelController` owns the active destination, portal exposure, gateway cooldown, pending preparation, and transition epoch. `Destination::Home` means the primary source retained at join; `Destination::Dimension` owns a sibling source. Each event-loop pass promotes a staged destination and clones its source locally before dispatch, chunk admission, relight, or feed drains. `dimension_scoped_handles` selects that source's block-entity registry and delayed-block-tick feed, with independent join-time fallbacks.

Both timer arms call `TravelController::tick` after readiness-gated player vitals. Contacts use the cell at the player's feet. End gateway contact precedes ordinary portals, uses live or resident block-entity metadata, refuses mounted players, and admits the arrival footprint before resolving it. It sends a same-dimension teleport and recenters the existing view, with a 40-tick contact cooldown.

Nether and End portals share `PortalTracker`. A zero delay triggers on the first contact tick; a delay of 80 triggers on tick 81. Exposure decays by four outside a portal. The controller reads game rules each tick, so a changed delay applies to existing exposure. A successful dimension trip starts the ten-tick player portal cooldown, which re-arms while standing in the arrival portal and prevents an immediate return. End portal contact inside the End is the exit: see [Leaving the End](#leaving-the-end).

Preparation owns its source and immutable request data and sends no packets. Each connection polls it as a separate event-loop branch, alongside transport, chunk work, and timers. Moving off the original contact cancels the pending request before its next poll; dimension reset advances the epoch and drops it. A native worker already computing an answer may finish, but that answer has no connection sender. Terrain prepared or written before cancellation remains reusable world state; cancellation does not roll back world edits.

Nether travel scales coordinates by the dimension ratio, then admits the destination's indexed candidates and bounded fallback footprint. A found portal gets a second admission for its rectangle measurement. If creation is necessary, the 33-by-33 site search admits its frame and side-slice reach: `[-17, +18]` along the portal axis and `[-17, +17]` across it. The return leg admits the home source, including a cold search away from the join view. The temporary query adapter uses resident point reads, records unavailable or busy data, and rejects an incomplete result instead of accepting placeholder air. It holds no terrain cache or global cache lock across an await.

Owned-source admission drains the existing bounded `ColumnPipeline` and releases each returned payload; it does not clone and retain a second full-footprint terrain vector. The resolver then queries the source's resident store. The legacy borrowed-source path retains its original scalar or batch admission behavior.

End preparation repairs the fixed arrival platform before destination chunks can be sent. Destination writes use resident-only mutation: busy reads or writes yield before retrying, missing columns are re-admitted in bounded groups, and a capable source declining resident mutation returns an error instead of falling through to cold generation. Legacy sources without that capability retain their ordinary mutation path. Long write sequences yield every 64 completed cells.

An uninitialized End fight admits and completes its deterministic arena block plan before consuming the one-time claim. Only the final claim and entity allocation have no intervening await. An initialized fight skips arena repair on later arrivals; a source without readiness reporting retains its legacy claim policy. Cancellation retains completed platform or arena edits but cannot consume the claim while writes are pending. Concurrent preparations may repeat the same deterministic writes, while the source's claim still gates entity allocation.

A dimension commit sends the dimension-change and placement pair, forgets the previous view, publishes the destination cache center, and rebuilds the entire join stream. It clears old encode futures, completed unsent reactive batches, relight queues, and tick-update queues. The platform loop drops its detached or cooperative relight job before commit. Clearing is required even when both dimensions contain the same chunk coordinates.

`reset_player` clears movement and fall baselines, sets `client_loaded` false, and immediately publishes the destination `TickAnchor`. Fresh `PlayerLoaded` is required before vitals or contacts resume. Shared world ticks continue throughout travel; the connection does not pause the world's clock. Death away from home uses the same stream/player reset and stages `Destination::Home`, ending the old-source pass before further publication.

## Leaving the End

Contact with an end portal while in the End sets a flag on `TravelController` (`take_end_exit_contact`), which the connection loop turns into `connection_travel::begin_end_exit`. Per-connection state lives in `connection_travel::EndExit`:

- **Credits not yet seen**: the server sends game event 4 (win game) with parameter `1.0`, records `seenCredits` as a byte in the player's preserved save fields (so it persists with player data and survives rejoin), and waits. The current client shows the credits whatever the parameter is; older clients show them for `1.0`. The client answers with the perform-respawn client command when the credits end. `apply_client_command` only records that answer (`EndExit::request_respawn`) while an exit is pending; a living player's stray perform-respawn is still ignored.
- **Credits already seen**: no announcement; the player goes straight home like any portal traveller.

Either way the connection loop then calls `connection_travel::end_exit_respawn`, which resolves the player's bed (falling back to the world spawn) against the *home* source, sends a dimension change to the overworld with all player data kept (inventory, XP, health: it is not a death), and hands the position to the existing `dimension_reset` rebuild of the home view. The straight-home case is picked up after the next client packet, which a live client sends every tick.

Non-player entities are not carried through the exit portal.

## How to change it

Keep contact decisions and dimension commit in the shared controller/helpers. Platform loops own timer polling, native saves, and the lifetime of their relight task; they must end a reset pass before using another outgoing-source handle. Any long-lived browser relight must own its dimension source rather than borrow a pass-local source.

Extend a destination's admission footprint whenever its resolver starts reading farther. Use the source's resident point capability for scans; cloning `ChunkColumn` is a deep copy. Native queries run on the world-generation dispatcher. The browser currently performs the bounded existing portal search or creation scan synchronously after admission, with a cache lookup and short lock per point read. It has no cooperative search cursor, so successful compilation does not establish a portal latency bound. Generation admissions yield, but an individual scalar generator call may also occupy a long poll.

The focused `connection_travel::tests` cover timing, decay, rule changes, cooldown, contact cancellation, fractional negative return coordinates, destination markers, home promotion, readiness, and anchor publication. Scripted resident contention checks distinguish busy from missing terrain and reject an unsupported mutation without a scalar fallback. An End preparation cancelled at its arena write gate retains the completed platform without taking the fight claim. `join_scheduler::tests::dimension_reset_cancels_completed_and_pending_encodes_at_the_same_coordinate` includes a cancellation-disabled control that exposes stale delivery. Resolver and controller tests are not evidence that the browser loading cover or rendered chunks changed; that requires a built browser session and a real round trip.

Resource selection currently exposes block entities and delayed block ticks. `PlayerTicketGuard` remains bound to its join-time ticket store; sibling mob and explosion handles are not returned by this selector. Full dimension ticket, mob, and explosion parity is therefore not established. Bed respawn still validates its saved position against the current source before the home reset. The loading cover is driven by the client's dimension-change consumer at commit; there is no client waiting-event consumer for an earlier cold-preparation cover.

## Configuration

`players_nether_portal_creative_delay` defaults to `0`; `players_nether_portal_default_delay` defaults to `80`. `allow_entering_nether_using_portals` controls entry into the Nether while allowing the return leg. The tracked view radius and hosting protocol control chunk streaming and whether a dimension-change frame is supported. No additional environment variable enables travel.

## Dependencies

`ChunkSource` provides sibling sources, resident terrain, portal indexes, registries, and tick feeds. `portal` owns contact timing and destination geometry; `dimension` owns scaling and dimension keys. `JoinChunkStream`, `OrderedJoinEncodes`, and `ServerProtocol` own generation ordering and wire output. `WorldStateHandle` supplies rules and shared tick anchors. Native timers use Tokio; browser timers use `BrowserInterval` and the host timer seam.
