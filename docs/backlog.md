# Backlog

Open work, ordered by how much it changes the screen per unit of effort. Traps are
attached to each item because they are the expensive part — the code is usually easy
once you know what already exists and what will silently mislead you.

**Companion docs:** [`../CLAUDE.md`](../CLAUDE.md) for the durable rules,
[`../HANDOFF.md`](../HANDOFF.md) for the long-form record of beliefs that were
confidently held and turned out false, and [`README.md`](./README.md) for the
per-subsystem index.

Session task lists do not survive a restart. **This file is the backlog of record.**

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
- **Flat sprite items draw nothing as drops.** `collect_item_model_parts` keeps only
  `IconPart::Model`, so an `item/generated` icon never enters `BlockModels::items()`.
  That is most items in the game.

---

## Tier 1 — needed before "a stranger could play survival for an hour"

1. **Smooth lighting / AO on the model path.** Flat per-block light plus directional
   shade today; the most recognisable "not Minecraft" tell now that geometry is right.
   Vanilla is `ModelBlockRenderer.AmbientOcclusionFace` — four-corner blend, occlusion
   count, and the `smoothBlend` substitution so a corner against a wall is not black.
   `render/mesh.rs` has `face_corner_lighting` for the *demo* mesher; the model path
   needs its own. **AO must multiply in gamma space** (§ `4e8f058`).
2. **Block entity renderers.** None exist. Chests (animated lid, double pairing), signs
   (text layout, dye, glow), beds, banners (layered patterns), item frames, shulkers,
   enchanting-table book, bells, conduits, end crystals, decorated pots. Start with
   chest and sign — the two a player notices first.
3. **Sun, moon, stars, clouds.** Sky is a flat colour. Moon has 8 phases from a 4×2
   atlas; stars are a seeded 1500-point field; clouds sit at y=192 and scroll. Shares
   the world-time seam with night darkening and sky colour.
4. **Weather.** No rain/snow/thunder state or rendering. Camera-relative angled quads,
   density from `rainLevel`, rain-vs-snow from biome temperature per column, sky and
   light darkened by `thunderLevel`, looping ambient audio, lightning flashes. Server
   sends state via `GAME_EVENT` — check those ids reach a consumer first.
5. **First-person hand, held item, arm swing.** No first-person renderer of any kind.
   We already *send* `SwingArm`, so other players see us swing; we have nothing to draw
   it on. Needs `display.firstperson_righthand`, which is **not reachable** — `icon.rs`
   keeps only the `gui` slot.
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
9. **The remaining ~33 clientbound packets.** Measured `108/141` decoded, `107/141`
   emitted, `53/69` serverbound encoded, 1 decoded-but-stranded (`CHUNK_BATCH_START`).
   **Use `cargo xtask connectedness`, never a hand count** — the hand-derived figure has
   been wrong four times in four different ways.

## Tier 1½ — smaller, player-requested

- **HUD animations**: hearts jump and flash on heal/damage, hunger shakes when
  depleting, XP bar flashes on level-up, hotbar selector pop.
- **Air-supply bubbles** underwater. The submerged flag exists (`69f66c2`).
- **Item pickup fly-to-player.** `TakeItemEntity` decodes and `PickupFeed` already folds
  it with tests; **nothing calls it**. Needs a consumer, not adapter work.
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
