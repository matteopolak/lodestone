# Issue batching: ~250 open issues as agent-sized, file-disjoint dispatch batches

## What it is

A dispatch plan that clusters the 255 open issues (surveyed 2026-08-04) into batches one agent can
close in one sitting, scheduled into waves whose file sets are disjoint. Verified against `git log`
and the current tree, not against issue bodies — roughly 28 open issues already have their substance
landed, and several "blocked" labels are stale.

## How this was verified, and what to re-verify

Every issue number was cross-checked against `git log --grep '#N\b'`; ~70 open issues already appear
in commit messages. Two caveats an agent dispatching from this doc must keep:

- **PR numbers share the issue number space.** `8cb8bfb` cites `(#106)` but is worldgen ore work,
  not the `#106` bench issue. A grep hit is a lead, not a verdict — the sweep batch reads the
  commit body before closing anything.
- **This doc is a sample of 2026-08-04.** The tree had ~6 agents mid-flight (HUD animations #30,
  savanna vegetation #404/#428, server creeper metadata/explode in `mobs.rs`, container cost
  screens, menu work, the gpu.rs split). Re-check `git status` before dispatching any batch that
  touches those files.

Seven plan docs already decompose their epics into units with file ownership; those batches
reference the plan instead of restating it: `server-ecs-migration.md` (#433),
`world-state.md` (#323–#330), `mob-ai-roster.md` (#225–#233), `chunk-lifecycle.md`
(#289/#292/#293/#297), `multi-version-protocol.md` (#343–#358), `paper-nms-bridge.md` (#341),
`worldgen-parity.md` (#404/#407/#428).

## The sweep: already done (or nearly), just needs verifying and closing

The cheapest closable work in the tracker. One read-only agent, one sitting: for each issue, read
the cited commits, run the named test or grep the producer, close with the evidence, or retitle to
the true residue. **Do not implement anything in this batch.**

| issue | evidence | residue, if any |
|---|---|---|
| #16 input verbs | `372ec67`, `ece2eae`, `1585e69`; all four in `keybinds.rs` | none found |
| #19 NEARBY_ENTITY_RADIUS | `799f1d0` | none |
| #21 Less→LessEqual | `a27230c` | none |
| #28 container family | `ef3d5dd`, `8e27372`, `f54c498` ("the whole family") | none |
| #30 HUD animations | `3c9f2f0`, `3e1a0af`, `60c0258`; `hud/anim.rs` in flight now | wait for in-flight work |
| #46 chat command UX | `fd98f55`, `bb81776`, `f33f18f` | none |
| #58 view bob/tilt/lag | `96f21c7`, `e321155` (consumers landed) | verify damage tilt third |
| #67 data_dir dedup | `515ebc4` | none |
| #71 crosshair question | `a0dfce0` (settled in docs) | none |
| #85, #87 worldgen benches | `c7f6f1c` ("five sub-issue closures"), `6969172` | check which of the five |
| #126 held-item tooltip | `0a0554d`, `3b2bcc5` | none |
| #144, #149 overlays | `a2e13c6`, `beb5194`, `2a89cd9` | "honest stopgap" inputs — verify |
| #154 spyglass | `6fe29db` ("the last hop") | none |
| #163 recipe book | `572e8ec` | none |
| #188 statistics screen | `79e0b94` | data stays zero until #26 decodes the stats packet |
| #206, #208 riptide/elytra | `98ac37b` cites both | verify firework boost specifically |
| #237 aging | `7bf2873` | none |
| #249–#252 furnace/hopper/composter/brewing | `fb23564`, `7f75055`, `c85fefa`, `814ef4c`, `1811b81` | none for the sims + placement |
| #311 gravity blocks | `4de7e84` | none |
| #314, #315, #317 redstone substrate | `466a694`, `ac5d2b7` | none — and this unblocks #316–#322 |
| #323, #327, #328 world state storage | per `docs/plans/world-state.md` | retitle to residues (per-connection `WorldAdminState` fix, tick-time island) |
| #262–#270 serverbound decode | decode ~60/69 done | retitle all five to the *connect* axis (13–15/69) |
| #182 particles | `09c9356`: 6/8 | retitle to 2/8 |
| #424 sliders | `9eba2bb`, `6497585` | retitle to remaining option groups |

## Batch table

Sizes: S = under one sitting, M = one sitting, L = 2–3 sittings, XL = a lane, not a batch.
Status: **now** = dispatchable immediately; **soft** = dispatchable but one shared file needs
brokering; **blocked(X)** = do not dispatch until X.

### Dispatchable now — client, protocol, and new crates (no server-core contention)

| id | issues | owns exclusively | status | size | substrate |
|---|---|---|---|---|---|
| SWEEP | table above (~28) | nothing (read-only + `gh`) | now | M | commit evidence already exists |
| BENCH | #90 #91 #92 #97 #99 #106 #128 #133 #151 #160 | bench harness crates, `xtask` bench code | now | L (3–5/sitting) | fifth-harness-crate pattern (`c7f6f1c`) |
| XTASK | #431 #432 #446 #447 | `xtask/`, CI config, `scripts/` | now | M | xtask scanner toolkit |
| DOCS-SMALL | #40 #43 #44 #448 | `docs/*.md`, doc comments | now | M | docs-only labels |
| PROTO-CLIENT | #26 #294 #421 #304 | `crates/protocol/v770/src/` client adapter (NOT `server_protocol.rs` — in flight) | soft | L (5–8 packets/sitting) | per-packet decode→event→fold pattern; `ebc3b7d` triage lists the 32 |
| LEGACY-V340 | #349 residue | `crates/protocol/v340/` | now | L | `multi-version-protocol.md`; canonical bridge exists |
| LEGACY-V47 | #345 residue | `crates/protocol/v47/` | now | L | same plan; needs flattening bridge |
| LEGACY-V735 | #353 residue | `crates/protocol/v735/` | now | L | same plan |
| MOBAI-1 | #226 hostile melee | `lodestone-entity/src/ai/` species goal files | now | M | perception spine landed (#441, `7630386`, `2a4da25`) |
| MOBAI-2 | #228 passive herd | disjoint species files, same crate | soft (shared `GoalSelector`, read-only) | M | same spine |
| PARTICLES | #178, #182 residue (2/8) | `lodestone-particle/`, `shell/src/particles.rs` | now | M (8–12 types/sitting) | transcription pattern from `09c9356` |
| MENU-SCROLL | #445 | `menu/{key_binds,social,stats,language,options,packs,world_select}.rs`, `menu/widget.rs` | now | M, mostly deletions | `ScrollList` landed; issue body has the per-screen audit |
| AUDIO | #135 #183 | `lodestone-audio`, `lodestone-sound`, `shell/src/audio.rs` | now | L | audio stack exists; needs event feeds |
| RENDER-ITEMS | #171 #130 (+#17 verify) | `lodestone-render` item paths, `lodestone-assets` tints | now | M | tint/glint both extend the item pipeline |
| ENTITYVIS | #29 (three islands) | `gpu/entities.rs`, `EntityDraw` widening | soft (gpu/ recently split) | M | all three decoded+folded already; consumers only |
| BLOCKENT | #23 residue | `gpu/block_entities.rs`, render BE paths | now | L | census in `8d94271`; bells/signs/chests done |
| CAMERA | #186 | `camera_rig.rs` | soft (gpu hookup) | S | rig composability proven by #154 |
| RENDER-MAP | #184 | map decode + texture + hand render | now | M | item-display plumbing exists |
| PHYS-CLIENT | #201 #218 #220 | `lodestone-physics`, `sim/step.rs` seam | soft | M | #220 is a live gate; #201 small |
| CMD-RECONCILE | #435 | `shell/src/chat.rs`, `lodestone-command` | now | M | both trees landed; delete `chat.rs`'s hand-rolled validators |
| WASM-PLUGIN | #172 #173 #175 #176 | new crates under `crates/plugins/` | now | L | greenfield; settle #173 ABI first |
| PLUGIN-DESIGN | #101 #141 #156 #165 #166 #168 #169 #177 #181 | `docs/plans/`, `docs/plugin-api.md` | now | L (3–4 docs/sitting) | design questions, docs-only; Fable-grade |
| PLUGIN-INTENTS | #422 | `crates/plugins/`, game intent types | now | S | `b435dea` PlaceIntent is the template |
| CMDBLOCK | #442 | BE NBT decode (v770 client side) + interact trigger + `menu/command_block.rs` | soft | M | screen already exists; two wiring gaps named in the issue |
| SKINS-A | #62 own-skin half | auth fetch, entity texture override | now | M | other-players half is blocked(#438) |
| WORLDGEN | #428 residue, #407, #404 residue | `lodestone-worldgen` per `worldgen-parity.md` | soft (savanna work in flight) | XL lane | plan has the unit split |

### Server-side — coordinate with the #433 ECS migration lane

`server.rs`, `tick.rs`, `mobs.rs` are the migration's own substrate (Phase 0 landed `cfdfec8`).
Any batch below that names them is **soft at best**; the safest sequencing is one server-core lane
at a time, brokered by the orchestrator, with per-phase mapping from `server-ecs-migration.md`.

| id | issues | owns | status | size | substrate |
|---|---|---|---|---|---|
| REDSTONE-1 | #318 #319 | new `redstone_rail/openable` modules; `scheduled_tick.rs` seam brokered | soft | M (1–2 blocks/sitting) | #314/#315/#317 landed — the `blocked` labels are **stale** |
| REDSTONE-2 | #320 #322 (+#321 residue) | dispenser/noteblock modules, same seam | after REDSTONE-1 | M | same spine |
| REDSTONE-3 | #316 pistons | piston module + update-order oracle | after R-1/R-2 | L | needs-oracle; hardest child |
| BLOCKBEH | #309 fluids; #312 fire; #313 residue (verify `281556f`) | `random_tick.rs`/flow modules | after redstone (shared tick seams) | L each | NeighborPropagator landed |
| ECONOMY | #253 #254 #255 (+#248 verify `growth_tick.rs`) | new server modules, `inventory.rs` | soft (container-cost client work in flight) | L | screens already exist client-side |
| VITALS-1 | #256 #258 #269 | `vitals.rs` + new modules | blocked-ish (#433 Adjudicate phase) | M | XP/hunger/burning are per-player scalars — natural ECS components |
| VITALS-2 | #257 #259 #260 #261-residue #272+#337 | entity/server split per issue | after VITALS-1 | L | loot tables (#337) pair with mob loot (#272) |
| SRV-FRONTDOOR | #277 #279 #280 | status/disconnect/keepalive in `protocol.rs` + `server.rs` seam | soft | M | three small, same handshake surface |
| SRV-SERVICES | #331 #332 | new RCON/query modules, `integrated.rs` seam | now-ish | M | mostly self-contained listeners |
| SRV-CONFIG | #275 #334 #335 | configuration-phase encode in `protocol.rs` | soft | L | registry data exists in `worldgen_data`/assets |
| SRV-ADMIN | #336 + shared `WorldAdminState` fix | admin state module | with WORLDSTATE-1 | M | world-state plan names the per-connection defect |
| WORLDSTATE-1 | #326 + `WorldAdminState`→shared | per `world-state.md` | soft | M | plan has unit split; #323/#327/#328 are residues |
| WORLDSTATE-2 | #324 #325 #329 #330 | per plan | #329 blocked(#437); #330 XL | L | plan |
| CHUNKLIFE | #289 #292 #297 #440 | `chunk.rs`, ticket modules per `chunk-lifecycle.md` | soft (#433 contention) | XL lane | #293 landed (`9699d84`); **#297's premise is false for 26.2 — rewrite, don't implement** |
| PERSIST | #437 first, then #302 #303 #305 | anvil wiring in `integrated.rs`, save modules | soft; #302/#303/#305 blocked(#437) | L | `lodestone-anvil` landed (#298/#300 closed) |
| CMD-SRV | #48 | server Brigadier dispatch | soft | L | unblocked by `lodestone-command` (`2f649d6`) |
| SPAWN | #221 #222 #223 #224 | spawn-rule tables, `mob_spawn.rs` driver | soft (`mobs.rs` in flight) | L | cap/despawn engine proven, driverless |
| MOBMECH | #235 #236 #238 #239 #240 #241 #246 #247 | entity + `mobs.rs` per species | soft; #241/#246 later | L | perception spine landed |
| CHAT-SIGN | #283 #286 #271 (+#301 residue) | client session keys; server verification; protocol | L lane, wave 3 | XL | #286's cache is built and waiting |
| MOBAI-3+ | #227 #229 #230 #231 #232 #233 #209 | per `mob-ai-roster.md` | #230–#233 after Brain driver (#209) | M each | plan sequences it |
| VILLAGERS | #242 #243 #244 #245 | Brain schedules + POI | blocked(#209/#231 Brain driver, #303 POI storage) | XL | plan epic |
| BOSS | #276 #278 (#274 parent) | fight state machines | blocked(projectiles #260, AI spine, #438 for multiplayer fights) | XL | — |
| P-438 | #438 player entities/broadcast | player registry per `server-ecs-migration.md` phase | the migration lane itself | XL | gates all real multiplayer, #62b, #271 |
| PLUGIN-FEAT | ~30 remaining #77 children (#107–#164 range, #129 #131 #134 #136 #138 #140 #143 #145 #147 #148 #150 #152 #153 #157 #161 #162 #164 #170 #179) | plugin API surface | blocked(#433 phases; #341 census found zero seams reachable) | XL | dispatch only per `paper-nms-bridge.md` sequencing |
| LEGACY-NEW | #344 #346–#348 #350–#352 #354–#358 | new family crates per era | not blocked, but XL each | XL | `multi-version-protocol.md` family-per-era |

## Parallelism schedule

**Wave 1 — dispatch these 12 now.** File sets are pairwise disjoint; none touches
`server.rs`/`tick.rs`/`mobs.rs`, leaving the whole server core free for the #433 migration lane:

1. SWEEP (no files)
2. BENCH (bench crates, xtask)
3. PROTO-CLIENT (v770 client adapter)
4. LEGACY-V340 (v340 crate)
5. MOBAI-1 (#226, entity AI hostile files)
6. MOBAI-2 (#228, entity AI passive files)
7. MENU-SCROLL (#445, menu/*)
8. PARTICLES (particle crate)
9. AUDIO (#135/#183, audio crates)
10. WASM-PLUGIN (#172/#173/#175, new crates)
11. PLUGIN-DESIGN (docs only)
12. RENDER-ITEMS (#171/#130, render+assets)

Alternates if one stalls: CMD-RECONCILE (#435), ENTITYVIS (#29), SRV-SERVICES (#331/#332),
BLOCKENT (#23), LEGACY-V47.

**Wave 2 — as wave-1 slots free, and as the ECS migration passes its early phases:**
REDSTONE-1 → REDSTONE-2, SRV-FRONTDOOR, WORLDSTATE-1 + SRV-ADMIN, PERSIST (#437), ECONOMY,
CHUNKLIFE unit 1, CMD-SRV (#48), MENU-SETTINGS (#443/#444 after MENU-SCROLL releases
`options.rs`), SCREENS (#158; #167 needs #26's decode first), HUD (#197/#198 after #30's
in-flight work lands), PHYS-CLIENT, SKINS-A, CAMERA, CMDBLOCK, LEGACY-V735.

**Wave 3 — after the migration's player registry (#438) and the plugin seams exist:**
VITALS-2, CHAT-SIGN, MOBAI-3+ Brain units, VILLAGERS, BOSS, PLUGIN-FEAT, WORLDSTATE-2 (#330),
LEGACY-NEW families, UNICODE (#187), RIDING (#11), TEXT-TYPE (#69), CLIENT-REFACTOR
(#20/#35/#37/#42).

## Close or merge as obsolete

- **#421 and #294 are instances of #26** — fold into PROTO-CLIENT's checklist, close as dupes on
  landing.
- **#321 (redstone hoppers)** — the transfer half landed as #250 (`7f75055`); the residue is
  redstone lock only. Merge into REDSTONE-2 or re-scope.
- **#297** — its stated premise is false for 26.2 (per `chunk-lifecycle.md`); rewrite before
  anyone implements the suggested gate, or close.
- **#43** — one of its two dedup candidates was #67's `data_dir`, now landed; re-scope or close.
- **#37 (WindowApp.ecs scaffold)** — either delete the inert field (S) or close as superseded by
  the bevy direction; verify which.
- **#436** is a process ledger, not schedulable work — but it holds an **unticked brokered patch**
  (`lodestone-render/src/target.rs` `COPY_SRC` + `AcquiredFrame::texture`) that should be applied
  or struck.

## Deliberately not classified

- **Epics and tier trackers** (#1–#7, #77, #78, #225, #242, #274, #314, #339, #340, #343, #404,
  #433, #438): they are lane-heads, not batches; their children are classified instead.
- **In-flight work** (per `git status` 2026-08-04): #30 HUD animations, savanna vegetation,
  server creeper metadata/explode, container cost screens, menu edits — dispatching over these
  clobbers.
- **#341** — design settled by `paper-nms-bridge.md` ("last plugin, not first"); nothing to
  dispatch until PLUGIN-FEAT exists.
- **#433's own phases** — `server-ecs-migration.md` is the authority; this doc only routes other
  batches around it.

## How to change it

Re-run the survey, don't edit numbers in place: `gh issue list --json number,labels,title` plus
`git log --grep '#N\b'` per candidate. Any batch here is a claim of 2026-08-04; the sweep table
especially decays fast. When a wave-1 batch lands, strike its row and promote from wave 2 — the
file-ownership column is the only part that must stay true at dispatch time.
