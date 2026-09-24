import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.entity.BlockEntityType;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Authoritative per-block-state {@code state_id -> block_entity_type} census.
 * Boots the real 26.2 server (registries only, no world) and walks
 * {@code Block.BLOCK_STATE_REGISTRY}, asking each state whether it owns a block
 * entity and, if so, which registered {@code BlockEntityType} claims it —
 * mirrors {@code HardnessOracle} and {@code PathTypeOracle} in this directory.
 *
 * <h2>Why the jar, and why this pairing is not in any report</h2>
 *
 * This is the fact vanilla's client uses to create a block entity from nothing
 * but a block-state write ({@code LevelChunk.setBlockState}, 26.2
 * {@code LevelChunk.java:341}: {@code blockEntity =
 * ((EntityBlock)newBlock).newBlockEntity(pos, state)}). Mojang's
 * {@code blocks.json} report carries block *properties* only — no
 * {@code hasBlockEntity} flag and no block-entity type — and
 * {@code registries.json} carries the {@code BLOCK_ENTITY_TYPE} registry's ids
 * but says nothing about which blocks each type covers. The pairing exists only
 * inside the jar.
 *
 * <h2>How the type is recovered, and why not via {@code newBlockEntity}</h2>
 *
 * {@code EntityBlock.newBlockEntity(pos, state)} is the call vanilla makes, but
 * running it here would construct a live {@code BlockEntity} per state (32k of
 * them), and several constructors touch a {@code Level} or a data-fixer. The
 * question it answers is available declaratively instead:
 * {@code BlockEntityType.isValid(BlockState)} is exactly
 * {@code this.validBlocks.contains(state.getBlock())} (26.2
 * {@code BlockEntityType.java:26-28}), and {@code validBlocks} is the very set
 * {@code newBlockEntity}'s owner was registered with. So scanning the
 * {@code BLOCK_ENTITY_TYPE} registry for the type that claims a state's block
 * yields the same answer with no construction.
 *
 * Two consistency facts are asserted here rather than trusted, because both
 * would silently corrupt the table:
 *
 * <ul>
 *   <li>a state with {@code hasBlockEntity()} must be claimed by exactly one
 *       registered type — zero would mean a block entity we cannot name, and
 *       more than one would make the mapping ambiguous; and</li>
 *   <li>a state <em>without</em> {@code hasBlockEntity()} must be claimed by no
 *       type at all.</li>
 * </ul>
 *
 * Emits one line per state, ascending global state id:
 * {@code <globalStateId> <blockName> <blockEntityTypeIdOr-1> <blockEntityTypeNameOr->}
 */
public final class BlockEntityTypeOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder sb = new StringBuilder();
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            String name = BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();

            int typeId = -1;
            String typeName = "-";
            int matches = 0;
            for (BlockEntityType<?> type : BuiltInRegistries.BLOCK_ENTITY_TYPE) {
                if (!type.isValid(state)) {
                    continue;
                }
                matches++;
                typeId = BuiltInRegistries.BLOCK_ENTITY_TYPE.getId(type);
                typeName = BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(type).toString();
            }

            if (state.hasBlockEntity()) {
                if (matches != 1) {
                    throw new IllegalStateException(
                            "state " + id + " (" + name + ") hasBlockEntity() but " + matches
                                    + " registered BlockEntityTypes claim it");
                }
            } else if (matches != 0) {
                throw new IllegalStateException(
                        "state " + id + " (" + name + ") has no block entity but " + matches
                                + " registered BlockEntityTypes claim it");
            }

            sb.append(id).append(' ').append(name).append(' ')
                    .append(typeId).append(' ').append(typeName).append('\n');
        }
        System.out.print(sb);
    }
}
