# Backlog

Open work, ordered by how much it changes the screen per unit of effort. Traps are
attached to each item because they are the expensive part — the code is usually easy
once you know what already exists and what will silently mislead you.

**Companion docs:** [`../CLAUDE.md`](../CLAUDE.md) for the durable rules,
[`../HANDOFF.md`](../HANDOFF.md) for the long-form record of beliefs that were
confidently held and turned out false, and [`README.md`](./README.md) for the
per-subsystem index.

Session task lists do not survive a restart.

**What is open now lives in [GitHub issues](https://github.com/matteopolak/lodestone/issues)**, as seven
Tier epics with sub-issues:
[Tier 1](https://github.com/matteopolak/lodestone/issues/1) ·
[Tier 1½](https://github.com/matteopolak/lodestone/issues/2) ·
[Tier 2](https://github.com/matteopolak/lodestone/issues/3) ·
[Tier 3](https://github.com/matteopolak/lodestone/issues/4) ·
[Tier 4](https://github.com/matteopolak/lodestone/issues/5) ·
[Infrastructure](https://github.com/matteopolak/lodestone/issues/6) ·
[Architecture](https://github.com/matteopolak/lodestone/issues/7).

**This file remains the record of the *traps*** — the tier definitions below, and the per-item
"what already exists and what will silently mislead you" notes, which are the expensive part and do
not belong in an issue title. Treat the tracker as the answer to *what is open* and this file as the
answer to *what will go wrong when you start*. When they disagree, the tracker is newer; fix this
file rather than working around it.

The `island`, `stale-record` and `vacuous-test` labels exist because those are this repo's three
recurring defect classes, not because they are generically useful — see
[`../CLAUDE.md`](../CLAUDE.md).

---

## In flight (as of 2026-07-28)

| item | state |
|---|---|
| `minecraft:tool` component + `block_type_name` bug | oracle, fixtures, generated tables present; one holder-id fix outstanding |
| crafting interaction | click machine written; live gate deliberately cut |
| vanilla font | metrics gates green; wiring outstanding, pixel gate cut |
| main menu + unfocused frame pacing | `FramePacer` written with vanilla's real cap |
| grass light-response gate + sprite-item drops | one red test file to resolve, then thin-slab extrusion |
| mob lighting re-diagnosis | previous fix shipped broken; cause unknown |
| smooth lighting / AO + Nether `SkyDefault` | new |

---

## Known-broken, player-observed

- **Nothing darkens for night — and this was the *whole* reason mobs looked full-bright.**
  Diagnosed live: at `clock=6000` and `clock=18000` the sampled byte is identically
  `0xF0` and `light_term` is `1.000` both times. **The server's sky-light array records
  how much sky *reaches* a block, not how bright the sky is**, so no sampling or plumbing
  fix could ever have darkened a mob at night. The entity sampler (`53850ce`/`52f109f`)
  was fine all along; `entity_light_pixels` already proved the shader reads location 8
  (ratio 0.203).

  `sky_darken_for_time_of_day` now ports `getSkyDarken` + `LightTexture`'s `*0.95+0.05`
  lift — 1.0 at noon, 0.24 at midnight, applied to the **sky half only** so torchlit
  interiors do not black out. Entity side pixel-verified 88.4 → 34.6, ratio 0.391 vs a
  predicted 0.392.

  **Two things remain, and they must land together.** `set_sky_darken_source` has zero
  production callers (needs ~4 lines in `app.rs`, via `net.shared_handle()` →
  `ClientHandle::world_time()` — note `world_time` is **not** on `NetClient`). And
  `model_pipeline.rs` plus the fluid shader still render at permanent noon, so wiring
  only entities makes mobs *darker than the blocks around them* at night — a new bug,
  not a partial fix.
- **~~Nether/End render full-bright.~~ Fixed** — `shell/mesher.rs` now reads the
  connected dimension off the shared handle's player snapshot and passes
  `SkyDefault::None` outside the overworld. **But it matches on the dimension
  *name*** (`minecraft:overworld`), where vanilla reads `hasSkyLight` off the
  **dimension type**. So a datapack dimension that does have sky light would be
  meshed dark. Correct for vanilla's three dimensions; worth replacing with the real
  `hasSkyLight` flag if the client ever models the dimension-type registry.
- **A pickaxe makes nothing faster.** `minecraft:tool` is unmodeled, so `tool_speed`
  stays at bare-hand defaults and obsidian is ~4m10s of unbroken holding.
- ~~**Flat sprite items draw nothing as drops.**~~ **Fixed** in `9980a96` /
  `a9f263f`, and this entry's wording is the single most expensive stale note this
  repo has produced. `collect_item_model_parts` collects `IconPart::Sprite` too
  (`block_models.rs:515`), `extruded_sprite_geometry` bakes it into vanilla's thin
  slab, and the result goes into **the same `BlockModels::items` map** as the 3-D
  models. `sprite_drop_pixels` proves the pixels.

  The stale sentence — "`collect_item_model_parts` keeps only `IconPart::Model`, so
  an `item/generated` icon never enters `BlockModels::items()`" — was copied
  verbatim into **four** issues (#33, #50, #54, #56) as their shared root cause, and
  #54 and #56 each said "#33 is a prerequisite, not a coincidence". It was a
  prerequisite for neither, because it was already done. Three of the four had
  entirely unrelated causes:

  | issue | real cause |
  |---|---|
  | #33 drops | already fixed |
  | #50 container block items | `app.rs:1273` calls `render_scaled`, which hardcodes `models: None` — an island; see [`container-screen.md`](./container-screen.md) |
  | #54 first-person hand | nothing told `RenderState` what the player was holding; see [`first-person-held-item.md`](./first-person-held-item.md) |
  | #56 projectiles | no projectile renderer existed at all; see [`thrown-projectiles.md`](./thrown-projectiles.md) |

  The lesson is the one `CLAUDE.md` rule 2 already states, with a number attached:
  **one stale note cost four misdirected diagnoses.** Grep for the producer across
  the whole tree before believing a note about a consumer.

---

## Tier 1 — needed before "a stranger could play survival for an hour"

1. ~~**Smooth lighting / AO on the model path.**~~ Landed. `1b8e46b` ported the
   four-corner blend, the occlusion count and `smoothBlend` into `quad_corner_sample`
   (`lodestone-render/src/models.rs`); `3fd10ea` added the `ambientocclusion` model-flag
   gate and the live shell override that took it off the island. AO rides the `ao` vertex
   slot into `model.wgsl`'s existing gamma round-trip, so `4e8f058`'s rule holds without a
   shader change. `model_ao_corner_gate` measures a single-occluder corner against a
   **predicted** byte of `round(255 * 0.8) = 204`, through `mesh_models` — with a
   no-occluder control and a flag-off control that both must go full-bright. See
   [`docs/model-smooth-lighting.md`](./model-smooth-lighting.md).

   **This entry stayed stale after the work landed and re-dispatched it once** — it still
   claimed the model path was "flat per-block light plus directional shade today" and
   pointed at `render/mesh.rs`'s `face_corner_lighting` as the only implementation. Rule 2,
   eighth instance.

   Still open, in descending order of visibility — details and vanilla citations in the doc:
   - **The AO occluder predicate diverges.** Ours is `BlockModels::occludes`; vanilla's is
     `BlockBehaviour.getShadeBrightness` (`isCollisionShapeFullBlock ? 0.2 : 1.0`,
     overridden to a flat `1.0` by exactly `TransparentBlock`, `Barrier`, `Light`, `Mud`,
     `SnowLayer`, `SoulSand`, `StructureVoid` — **not** `LeavesBlock`). The two agree on
     glass and ice by coincidence and disagree on every full-collision-cube block that
     does not occlude for culling: **leaves**, slime, honey, spawner, grates. Vanilla
     darkens the underside of a tree canopy; we do not. **Actionable now** —
     `lodestone_data::collision_shapes` is already O(1) by state id.
   - Vanilla's AO neighbourhood is centred on the neighbour cell only when the face is
     *cubic*; for a partial quad it centres on the block's own cell. Ours always uses the
     neighbour, so stair/slab interior faces sample one cell off.
   - `smoothBlend`'s sky-inherit branch and vanilla's sub-nibble (0..240) smooth-light
     precision are not ported; `getLightEmission() == 0` still has no data source; the
     Nether `CardinalLighting` shade table is still Overworld's.
2. **Block entity renderers.** Chest has landed end to end (`docs/block-entity-renderers.md`);
   still absent are beds, banners (layered patterns), item frames, shulkers,
   enchanting-table book, bells, conduits, end crystals and decorated pots.

   **This entry used to say "start with chest and sign — the two a player notices first", and
   the sign half was wrong.** In 26.2 signs *are* real block models — `oak_sign_rot_0..3` carry
   genuine geometry — and `StandingSignRenderer` declares **no** geometry of its own, only text
   transforms. The board already meshes through the ordinary block path, so porting "sign
   geometry" would draw a second board inside the real one. Sign is a **text pass**, a different
   subsystem sharing the `gpu/nametag.rs` substrate, and it does not belong beside chest.

   Chest was the right first pick for a reason worth keeping: `block/chest.json` in the real jar
   is `{"textures":{"particle":"block/oak_planks"}}` — **zero elements** — so a chest was not a
   slightly-wrong box, it was a hole in the world, and `sections_drawn`/`total_quads` were
   byte-identical with and without it drawing.
3. ~~**Sun, moon, stars, clouds.**~~ Landed — the sky is a real dome, not a flat
   colour: `crates/lodestone-render/src/{sky,sky_pipeline}.rs` plus
   [`sky-and-air-bubbles.md`](./sky-and-air-bubbles.md), proved by
   `crates/lodestone-shell/tests/sky_pixels.rs`. What remains is tracked on #96 —
   the sunrise/sunset horizon band, void fog, gradient banding quality and per-biome
   sky tint — and on #49, whose finding is that 26.2 deleted `getSkyDarken` in favour
   of an `EnvironmentAttribute` timeline, so our dusk/dawn ramp is still 1.21's.
4. **Weather.** No rain/snow/thunder state or rendering. Camera-relative angled quads,
   density from `rainLevel`, rain-vs-snow from biome temperature per column, sky and
   light darkened by `thunderLevel`, looping ambient audio, lightning flashes. Server
   sends state via `GAME_EVENT` — check those ids reach a consumer first.
5. **First-person hand: the special-cased item poses.** The arm, the swing and the
   **generic held item** all landed (`1ffbdee`, `22dc0ee`, and
   [`first-person-held-item.md`](./first-person-held-item.md)); `display.firstperson_righthand`
   *is* reachable — `ItemGeometry::display` carries all nine slots, and the claim
   that "`icon.rs` keeps only the `gui` slot" was stale for two sessions before
   being deleted from here. `RenderState::set_main_hand_source` is **now wired** —
   `app.rs` installs it every frame, and the shell draws the held item rather than
   the bare arm, with `first_person_item_drawn`/`first_person_arm_drawn` asserted
   mutually exclusive by real pixel gates. (This bullet claimed it was unwired for
   two sessions; that is the second stale claim in this one entry, which is why the
   first is preserved above rather than deleted.) What remains is the special-cased
   poses: bow/crossbow-while-drawing, shield, spyglass, map,
   trident, and the eating/drinking and brush animations. The off hand is not drawn
   either, though every function supports `Arm::Left`.

   **Third stale claim in this one entry, now corrected: "each needing use-item state
   the shell does not track" is no longer true.** Issue #57 decoded
   `LivingEntity`'s using-item bit and folded it through to a `ItemUse` component;
   mobs and remote players draw the bow and crossbow-charge arm poses from it
   ([`item-use-arm-poses.md`](./item-use-arm-poses.md)). Two things remain for the
   *first-person* pass specifically, and neither is the wire state: a session-scoped
   fold to reach the local player (which has no `EntityKind`, so `entity_view()`
   cannot carry it — the `Vitals::on_fire` shape, `7822a60`), and
   `ItemInHandRenderer`'s own item-pose transforms, which are distinct from the
   humanoid arm poses. Separately, `ArmPose::CrossbowHold` is blocked on decoding
   `minecraft:charged_projectiles`, not on render work.
6. **Full inventory interaction verbs.** Audit `click.rs` against
   `AbstractContainerMenu.doClick`: drag-split (left even / right one-each / middle fill
   in creative), double-click gather, number-key swap, offhand swap, drop and drop-stack,
   creative variants. Each is a distinct wire `ClickType`.
7. **Combat feel.** None of the 1.9+ feedback exists: attack-strength cooldown bar,
   cooldown-scaled damage, crit particles, sweep arc, knockback feedback, hurt tint and
   camera shake. `EntityDamaged`/`EntityHurtAnimation` decode — check for consumers.
8. **Riding.** `EntityPassengersChanged` decodes and reaches nothing. Needs passenger
   attachment offsets, camera on the vehicle, vehicle-specific input (boat paddles and
   horse jump charge are client-driven), the horse jump bar, dismount.
9. **The remaining ~32 clientbound packets.** **Use `cargo xtask connectedness`, never a
   hand count** — the hand-derived figure has been wrong four times in four different
   ways, and do not trust the numbers written here either: run it.

   **`CHUNK_BATCH_START` was never a defect, and this entry carried it as one.** It was
   listed as "1 decoded-but-stranded". It is decoded on purpose and correctly emits no
   `ClientEvent`: it is an empty marker that starts the batch rate timer
   (`begin_chunk_batch`), and the client's reply goes out from `CHUNK_BATCH_FINISHED` as
   `CHUNK_BATCH_RECEIVED` carrying the measured rate. That handshake is load-bearing —
   `PlayerChunkSender.MAX_UNACKNOWLEDGED_BATCHES` is 10 and `sendNextChunks` stops
   entirely above it, so a client that never acknowledged would lose chunk delivery
   after ten batches — and it is complete. There is simply nothing observable at the
   *start* edge.

   The tool now reports it under **protocol-internal**, with that reason printed, and
   `decoded-but-stranded` reads `0`. The exemption is an allowlist carrying a reason per
   entry rather than a silent subtraction, because a false positive in an island
   detector costs real work and a hidden exemption is where a real island would go to
   die. Adding an entry needs the same standard as any other claim here: say what
   consumes the packet and why no event is correct.

## Tier 1½ — smaller, player-requested

- **HUD animations**: hearts jump and flash on heal/damage, hunger shakes when
  depleting, XP bar flashes on level-up, hotbar selector pop.
- **Air-supply bubbles** underwater. The submerged flag exists (`69f66c2`).
- ~~**Item pickup fly-to-player.**~~ Landed, issue #365 — see
  [item-pickup-animation.md](./item-pickup-animation.md). The diagnosis here was right
  (`TakeItemEntity` decoded, `PickupFeed` folded with tests, nothing called it) and the
  consumer was one arm in `net.rs`'s `forward` plus a resource and two systems in
  `entities.rs`. One correction to the description this entry inherited from #29: vanilla
  does **not** retarget the item entity — it removes it immediately and animates a frozen
  copy of its render state (`ItemPickupParticle`).
- **Mob equipment.** `SET_EQUIPMENT → EntityEquipmentUpdated → EntityView.equipment →
  nothing`. Unblocked now that item geometry exists; needs `EntityDraw` widened and
  `display.thirdperson_righthand` plumbed.
- **Stack count on drops.** Vanilla draws up to five jittered copies as a stack passes
  1/16/32/48; we draw one. Decoded and dropped at the `EntitySnapshot` boundary for
  model-freedom — restoring it needs one `u32`, no model dependency.
- **`CollisionView::fluid_at`.** Unimplemented by both shell adapters, so a fluid cell is
  treated as full height rather than `amount/9.0`. Matters for surface bobbing and push.
- **Vanilla settings menu**, at minimum GUI scale. `GuiScaling` already models it.
- **Dimension travel.** Test portals before building — mostly server-authoritative. See
  the known-broken list for what is wrong on arrival.

## Tier 2 — expected by any real player

Container screens: furnace family (burn + progress arrows), anvil, enchanting table,
brewing, loom, smithing, stonecutter, grindstone, cartography, beacon, villager trading,
horse inventory. Each is a `MenuKind` with its own slot layout — **a constant offset
draws a plausible but transposed inventory that reads as an art bug**.

Also: advancements, statistics, recipe-book UI, maps, elytra, swimming, sleeping, sound
event breadth and subtitles, music, enchantment glint (plumbed but hardcoded `false`),
animated item textures (frame 0 only), Nether and End dimension rendering.

## Tier 3 — completeness

Secure chat signing (26.2 requires it; `chat_ack.rs` exists — audit what is missing).
Online-mode auth end to end — the crypto is **verified** (the server accepted our
RSA-wrapped secret and its AES-128-CFB8 reply round-tripped; only the session-server
ownership lookup failed, needing a real Microsoft account). Server-provided resource
packs. Options persistence, keybind rebinding, language switching, accessibility.
Realms. **Singleplayer as a real integrated server** — the shell calls the generator
directly today; the faithful destination is generate → loopback → client-consumes,
sharing the multiplayer path, and the generator itself does not have to change.

## Tier 4 — the game simulation (being a *server*, not a client)

Plausibly larger than Tiers 1–3 combined, and a different axis entirely: redstone, mob
AI and pathfinding, villager trading logic, farming and breeding, mob spawning rules,
block ticks, fluid flow, explosions, world persistence in Anvil format, command
execution.

Foundations that already exist and are bit-exact against JVM oracles: worldgen (noise
router, density, carvers, surface, aquifer, ore features), collision shapes (32,366
states), hardness, entity dimensions, and a generated `path_types.rs` dumped from
`WalkNodeEvaluator` — real groundwork for pathfinding. `lodestone-server` exists with
its tokio target-split done.

---

## Infrastructure

- **Split `HANDOFF.md`.** ~1800 lines and growing; several claims have gone stale mid-session.
  `CLAUDE.md` now carries the durable rules — the rest should become per-subsystem docs
  with `HANDOFF.md` reduced to a short "what is open" index pointing here.
- **`cargo fmt` is unsafe repo-wide.** `lodestone-v770` (~13 files including generated
  tables) and `lodestone-render` are already dirty against `--check` at HEAD.
- **Window-scoped screenshots do not work** in the agent environment: `osascript` lacks
  assistive access and there is no `Quartz` module. A full-screen grab catches the
  desktop and every population reads `R=G=B`. A `--screenshot` flag in the client would
  be more reliable than fighting the OS.
