# Server-side advancements and statistics

## What it is

The version-free server-authoritative model for advancement and statistic
tracking in `crates/lodestone-server/src/advancements.rs` (issue #338): the
advancement tree, per-player criteria progress and completion, the every-tick
flush to the client, NBT persistence for the #437 world-save hook, and a
statistics counter answered on `REQUEST_STATS`. It is the "server plumbing"
half of the epic — the data model and rules, never the wire bytes.

## How it works

Vanilla tracks both on the server: `PlayerAdvancements.java` holds per-player
advancement progress, `ServerStatsCounter.java` holds statistics, both persist
to disk, and the server streams them with three packets:
`ClientboundUpdateAdvancementsPacket`, `ClientboundAwardStatsPacket`, and
`ClientboundSelectAdvancementsTabPacket`. This module owns the model; the three
`ServerProtocol` seams lower the payloads.

Two of those three seams had **no v770 override** until recently, so everything
this module computed reached the wire as `ServerDirective::None`. They now emit
real frames — see
[`map-and-advancement-wire.md`](./map-and-advancement-wire.md), which also records
why the advancement `DisplayInfo` optional is written absent.

### The tree

[`AdvancementManager::builtin`] ships a real vanilla tree — `story/root →
story/mine_stone → story/upgrade_tools → story/smelt_iron → story/obtain_armor`,
plus `nether/root` and `recipes/root` — with vanilla's exact criteria and
requirement shapes read from 26.2's `data/minecraft/advancement/`. `Advancement`
holds only what is version-free: id, parent, requirement groups, and the
`sendsTelemetryEvent` wire bit. **Display info (title, icon, frame) is absent on
purpose** — it is pure presentation and this crate has no component model; the
data-pack loader (the epic's next landing) supplies it, and a version crate's
encoder can carry display fields from its own registry.

### Completion

`AdvancementRequirements.test` is an **AND of ORs**: an advancement is done when
every requirement group has at least one obtained criterion. An `allOf`
advancement puts each criterion in its own group; an `anyOf` puts all in one
group. `AdvancementProgress::is_done` implements this, and the empty group list
is never done, matching vanilla. `grant_criterion` / `revoke_criterion` return a
`GrantOutcome` (`changed` = the criterion flipped, `completion_changed` = the
advancement's overall done state flipped) so a caller can react to a *first*
completion exactly where vanilla fires its toast/root logic.

### Visibility

Vanilla's `AdvancementVisibilityEvaluator` walks the tree in order with
`VISIBILITY_DEPTH = 2`: the window is the node's own rule plus its parent and
grandparent (`evaluateVisiblityForUnfinishedNode` scans `peek(0..=2)`). A node
without a display is hidden. [`visible_ids`] reimplements the walk — a node is
shown if it or any descendant is done, or if its parent or grandparent is done.
**A done ancestor further away than a grandparent does not re-show the node**;
`granting_a_criterion_flushes_exactly_that_progress` pins the exact shape where
a completed sibling is out of window.

### Lifecycle

1. **Join** — `server.rs`'s `ConfigurationFinished` arm calls
   `AdvancementManager::initial_update(player, true)` and sends it through
   `encode_update_advancements`. That is the first packet: `reset` true, the
   whole tree as `added`, every advancement's current progress, visibility
   pre-computed (vanilla's `isFirstPacket` path).
2. **Play** — each `serve_play` loop calls `advancements.flush_dirty(player,
   true)` after every packet dispatch. Vanilla does the same every tick
   (`ServerPlayer.tick()` → `advancements.flushDirty(player, true)`); here the
   flush rides the connection's own loop, and `flush_dirty` returns `None` on
   the no-op fast path (nothing dirty, first packet already sent) so the common
   tick costs one `BTreeSet::is_empty`.
3. **Triggers** — gameplay code calls `grant_criterion` / `revoke_criterion`
   (or `award_stat`) when a trigger fires. Unknown ids and unknown criteria are
   no-ops, and a repeated grant of an already-obtained criterion is not a
   change, so it is not a dirty flush.
4. **Persistence** — `save_advancements` / `save_statistics` hand back NBT for
   the #437 hook; `load_advancements` / `load_statistics` restore it. The NBT
   mirrors vanilla's `PlayerAdvancements.asData()` (a criteria map with obtained
   timestamps plus a `done` flag) rather than the JSON files on disk, because
   this crate persists NBT, not JSON. `nbt_round_trip_preserves_advancements_and_stats`
   covers the round trip.
5. **REQUEST_STATS** — `ClientCommand` action 1 (vanilla's
   `ServerGamePacketListenerImpl.java:1910` → `player.getStats().sendStats(player)`)
   replies via `stats_snapshot` lowered through `encode_award_stats`. Select
   tab (`action` select-tab / the tab-change packet) is a third seam,
   `encode_select_advancements_tab`, with the same no-op default.

## The protocol seam

Three new `ServerProtocol` methods, each defaulting to `ServerDirective::None`
so a protocol family with no support behaves exactly as before:

- `encode_update_advancements(&AdvancementUpdate)`
- `encode_award_stats(&[(StatKey, i32)])`
- `encode_select_advancements_tab(Option<&str>)`

Every method must be forwarded in `impl ServerProtocol for Box<P>`, covered in
the `Numbered` test protocol, and asserted in
`a_boxed_protocol_answers_exactly_as_the_concrete_one_does` — the boxed-assertion
control proves forwarding works and a forgotten forward cannot silently strand
the seam. Only `v770` implements `ServerProtocol`, so only 26.2 sends these
today.

## How to change it, and the gotchas

- **This crate is version-free.** The module may never name a protocol number,
  packet id, or NBT layout; the `encode_*` payloads travel the `ServerProtocol`
  seam. A new wire concern (e.g. the `SelectAdvancementsTab` packet) is a new
  default method plus a Box forward, never a hardcoded id here.
- **Adding a `ServerBound` variant changes directive sequences, not just
  types.** `apply_client_command`'s match is compiler-enforced, but a
  choreography test asserting an exact `vec![Directive…]` is a silent caller —
  grep the **packet id**, not the variant name.
- **The first packet is load-bearing.** `initial_update` must run once on join
  before gameplay triggers, or `flush_dirty` sends a `reset`-true whole-tree
  update mid-session (correct but wasteful). `serve_play`'s per-connection
  manager is threaded through both the native and the wasm32 `serve_play`
  variants — they share one target-agnostic call site, and a signature drift
  between them is invisible to native checks (verify with `cargo check --target
  wasm32-unknown-unknown`).
- **The depth-2 window is exact.** Do not "generalise" visibility to
  great-grandparents or deeper; the real vanilla evaluator does not, and the
  flush delta (`old visible` vs recomputed) is asserted against the vanilla
  shape.
- **Obtained timestamps are epoch-millis longs** in the NBT (vanilla stores
  strings in its JSON `asData()`); keep the two formats from blending in the
  #437 loader.

## Configuration

None. The builtin tree is fixed in [`AdvancementManager::builtin`], the flush
cadence rides the connection loop (no timer constant), and show-advancements is
a per-call flag defaulted to `true` at every call site. A world-seeded tree is
the data-pack loader's job.

## Dependencies

- `lodestone-core` — `Nbt` for persistence.
- `uuid` — player keys.
- `crate::server` — the sole production consumer (join, flush, `REQUEST_STATS`).
- `.cache/mc/26.2/src` — the decompiled reference (`AdvancementRequirements`,
  `AdvancementVisibilityEvaluator`, `PlayerAdvancements.asData`), and
  `data/minecraft/advancement/*.json` for the builtin tree's shapes.

## Related

- `docs/statistics-screen.md` — the client side: what the client does with the
  stats packet once the wire half of this epic lands.
- Issue #437 — world persistence: the NBT save/load hooks this module exposes.
