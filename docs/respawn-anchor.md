# Respawn anchor

## What it is

A respawn anchor holds up to four charges. Glowstone charges it, using a charged anchor in the Nether makes it the player's respawn point, using one anywhere else blows it up, and dying with it as the respawn point spends a charge and stands the player beside it. A bed and an anchor share one per-player respawn slot; see [respawn.md](respawn.md) for how it is resolved.

## How it works

Chain, click to pixels:

1. **Use.** `server::apply_use_item_on` runs an anchor arm after the bed arm. `respawn_anchor::decide_use` is the pure decision (inputs: charges, whether the dimension allows anchors, glowstone in the clicking hand and the off hand, sneaking with an item, whether it is already the point). Outcomes:
   - Charge: the `charges` block property goes up by one, the block is resent and published, the `charge` sound plays on the effect lane, and one glowstone is consumed (not in creative) with the hotbar or off-hand slot resent.
   - Defer: the main hand holds a non-glowstone item while the off hand holds glowstone and the anchor can still be charged, so the main-hand click does nothing and the off-hand click charges.
   - SetSpawn: the slot becomes `RespawnPoint::block(pos, dimension, 0.0)`, the system line "Respawn point set" is sent, and the `set_spawn` sound is published. A repeat click on the same anchor is silent.
   - Explode: the block is removed first, then `MobSim::queue_blast` queues a power-5 blast that sets fires (unless smothered, below).
   - Fall through: an empty anchor with no fuel, or a sneaking player holding anything, continues to ordinary placement.
2. **Blast.** `mobs::Detonation` carries a `fire` flag. `tick::run_tick_loop` drains it through `block_drops::drop_explosion_loot_in_blast`, which after the crater runs `ignite_blast_cells`: each emptied cell has a one in three chance of becoming fire when it is air above a solid-render block. The fires are appended to the published changes. Other detonation producers pass `fire: false`.
3. **Respawn.** Resolution, routing between dimensions, the missing-block game event and persistence are in [respawn.md](respawn.md). Anchor specifics: the block must still be an anchor with charge in the Nether, a stand-up cell must exist, and a death respawn then spends one charge (a forced point spends none) and sends the `deplete` sound to the respawning player only.
4. **Blast.** An anchor blast is smothered when water is directly above the cell or a source or spreading flow is beside it (`respawn_anchor::smothered_by_water`): the detonation then has `destroys_blocks` and `fire` cleared, so entities are still hurt and the explosion is still published but no block changes and no fire appears.
5. **Stand-up search** (`respawn_anchor::find_stand_up`): 25 cells in a fixed order (eight horizontal neighbours, the same eight one down, the same eight one up, then straight up), first rejecting hazards (fire, soul fire, lava, magma, lit campfires, lava cauldrons, wither roses, sweet berry bushes, cacti, powder snow), then accepting them. A cell is valid when its floor height is below one block (a climbable block or open trapdoor counts as empty), a 0.6 by 1.8 body at (x + 0.5, y + floor, z + 0.5) overlaps no collision shape, and neither it nor the cell above is an end portal or gateway.

## Other consumers of the charge

- **Comparator.** `redstone::state_analog_output` reports the charge as 0, 3, 7, 11 or 15 for 0 to 4 charges, and `redstone::comparator_input_signal` replaces a comparator's input with it when the comparator faces an anchor. Containers and item frames are still unread (see `redstone_diode`). Charging by hand, by dispenser and by a death respawn notify the neighbours so the comparator is scheduled.
- **Dispenser.** A dispenser holding glowstone charges the anchor it faces by one and consumes one item (`tick::run_tick_loop`'s dispenser arm); a full anchor refuses and keeps the item; anything else ahead gets the ordinary toss. Charging works in every dimension.

## How to change it

- Dimensions that allow an anchor: `respawn_anchor::works_in`.
- Hazard and climbable lists: `is_hazard` and `is_non_climbable_exempt` in `respawn_anchor.rs`.
- The smother test is `smothered_by_water`; it treats a flow of depth one beside the cell as unable to spread into it.
- Gotcha: the stand-up body check does not reject fluids. The reference game's anchor search only requires a body-sized space free of collision shapes, a floor below one block high and no end portal or gateway; only the world-spawn search rejects fluids.

## Not modelled

- Soul fire from the blast (over soul sand or soil) and the world-border test on the stand-up cell.
- The facing direction the client expects with the respawn point.

## Configuration

None. Constants (`MAX_CHARGES`, `BLAST_POWER`, the two messages) are in `respawn_anchor.rs`.

## Dependencies

`lodestone_data` (block states, collision shapes, sounds `block.respawn_anchor.*`), `world_spawn::RespawnPoint`, the block-tick feed (block changes and sounds), `MobSim` (blast queue), `redstone` (comparator input) and `respawn` / `connection_travel` for routing.
