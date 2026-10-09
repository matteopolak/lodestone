# Respawn anchor

## What it is

A respawn anchor holds up to four charges. Glowstone charges it; using a charged anchor in the Nether sets the player's respawn point, using one elsewhere blows it up, and dying with it as the respawn point spends a charge and stands the player beside it. Bed and anchor share one per-player respawn slot ([respawn](respawn.md)).

## How it works

1. **Use.** `server::apply_use_item_on` runs an anchor arm after the bed arm. `respawn_anchor::decide_use` is the pure decision (charges, whether the dimension allows anchors, glowstone in main and off hand, sneaking with an item, already the point):
   - Charge: `charges` +1, block resent and published, `charge` sound on the effect lane, one glowstone consumed (not in creative) and the slot resent.
   - Defer: main hand holds a non-glowstone item while the off hand holds glowstone and the anchor can still charge; the main-hand click does nothing and the off-hand click charges.
   - SetSpawn: slot becomes `RespawnPoint::block(pos, dimension, 0.0)`, "Respawn point set" is sent and `set_spawn` plays; a repeat click is silent.
   - Explode: the block is removed first, then `MobSim::queue_blast` queues a power-5 blast that sets fires (unless smothered).
   - Fall through: an empty anchor with no fuel, or a sneaking player holding anything, continues to ordinary placement.
2. **Blast.** `mobs::Detonation` carries a `fire` flag. `tick::run_tick_loop` drains it through `block_drops::drop_explosion_loot_in_blast`, which after the crater runs `ignite_blast_cells`: each emptied cell with air above a solid-render block has a one-in-three chance of fire, appended to the published changes. Other producers pass `fire: false`. A blast is smothered (`respawn_anchor::smothered_by_water`) when water is directly above or a source or spreading flow is beside the cell: `destroys_blocks` and `fire` are cleared, so entities are hurt and the explosion published but no block changes.
3. **Respawn.** Resolution, dimension routing, the missing-block game event and persistence are in [respawn](respawn.md). Anchor specifics: the block must still be a charged anchor in the Nether, a stand-up cell must exist, and a death respawn spends one charge (a forced point spends none) and sends `deplete` to the respawning player only.
4. **Stand-up search** (`respawn_anchor::find_stand_up`): 25 cells in fixed order (eight horizontal neighbours, the same eight one down, the same eight one up, then straight up), first rejecting hazards (fire, soul fire, lava, magma, lit campfires, lava cauldrons, wither roses, sweet berry bushes, cacti, powder snow), then accepting them. A cell is valid when its floor height is below one block (climbable blocks and open trapdoors count as empty), a 0.6 by 1.8 body at (x + 0.5, y + floor, z + 0.5) overlaps no collision shape, and neither it nor the cell above is an end portal or gateway.

Other consumers of the charge:
- **Comparator.** `redstone::state_analog_output` reports 0, 3, 7, 11 or 15 for 0-4 charges and `redstone::comparator_input_signal` uses it when the comparator faces an anchor. Hand, dispenser and death charging notify neighbours so the comparator is scheduled.
- **Dispenser.** A dispenser holding glowstone charges the anchor it faces and consumes one item (in `tick::run_tick_loop`); a full anchor refuses and keeps the item; anything else ahead gets the ordinary toss. Works in every dimension.

## How to change it

- Dimensions allowing an anchor: `respawn_anchor::works_in`. Hazard and climbable lists: `is_hazard`, `is_non_climbable_exempt`. `smothered_by_water` treats a depth-one flow beside the cell as unable to spread into it.
- Gotcha: the stand-up body check does not reject fluids; the reference anchor search only needs a collision-free body-sized space, a floor under one block and no end portal or gateway. Only the world-spawn search rejects fluids.
- Not modelled: soul fire from the blast (over soul sand or soil), the world-border test on the stand-up cell, and the facing direction the client expects with the respawn point.

## Configuration

None. `MAX_CHARGES`, `BLAST_POWER` and the two messages are constants in `respawn_anchor.rs`.

## Dependencies

`lodestone_data` (block states, collision shapes, `block.respawn_anchor.*` sounds), `world_spawn::RespawnPoint`, the block-tick feed, `MobSim` (blast queue), `redstone`, `respawn` and `connection_travel`.
