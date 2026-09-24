import java.util.ArrayList;
import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Extracts the effective six-direction face-occlusion result for every
 * built-in block state. The result is the predicate used by enclosed-floor
 * generation: a face occludes when the state's cached face shape is a full
 * unit face. A block that opts out of occlusion therefore contributes no
 * directions, even if its outline happens to be non-empty.
 *
 * <p>The registry walk is deliberately checked against ascending global state
 * ids. The dump's {@code B} rows independently retain the state-to-block
 * coverage, while {@code K} rows retain an outside population count for every
 * direction. {@code P} rows are the six 256-state bit-string chunks used by
 * the Rust generator.
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   K &lt;D|U|N|S|W|E&gt; &lt;countOfTrueStates&gt;
 *   P &lt;D|U|N|S|W|E&gt; &lt;startStateId&gt; &lt;bitstring, up to 256 chars&gt;
 * </pre>
 */
public final class FaceOcclusionOracle {
    private static final Direction[] DIRECTIONS = {
            Direction.DOWN,
            Direction.UP,
            Direction.NORTH,
            Direction.SOUTH,
            Direction.WEST,
            Direction.EAST,
    };

    private static final char[] NAMES = {'D', 'U', 'N', 'S', 'W', 'E'};

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder blocks = new StringBuilder();
        List<Integer> masks = new ArrayList<>();
        int[] counts = new int[DIRECTIONS.length];
        Block previousBlock = null;
        int count = 0;
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            if (id != count) {
                throw new IllegalStateException(
                        "BLOCK_STATE_REGISTRY is not iterating in ascending id order: expected "
                                + count + ", got " + id);
            }
            if (state.getBlock() != previousBlock) {
                previousBlock = state.getBlock();
                blocks.append("B ").append(id).append(' ')
                        .append(BuiltInRegistries.BLOCK.getKey(previousBlock)).append('\n');
            }

            int mask = 0;
            for (int direction = 0; direction < DIRECTIONS.length; direction++) {
                VoxelShape face = state.getFaceOcclusionShape(DIRECTIONS[direction]);
                if (Block.isShapeFullBlock(face)) {
                    mask |= 1 << direction;
                    counts[direction]++;
                }
            }
            masks.add(mask);
            count++;
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# FaceOcclusionOracle dump from the real 26.2 server (protocol 776).\n");
        sb.append("# A bit is set when the state fully occludes that face.\n");
        sb.append("# Direction bit order: D, U, N, S, W, E.\n");
        sb.append("# C <stateCount> <blockCount>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# K <D|U|N|S|W|E> <countOfTrueStates>\n");
        sb.append("# P <D|U|N|S|W|E> <startStateId> <bitstring up to 256 chars>\n");
        sb.append("C ").append(count).append(' ')
                .append(BuiltInRegistries.BLOCK.size()).append('\n');
        sb.append(blocks);
        for (int direction = 0; direction < DIRECTIONS.length; direction++) {
            sb.append("K ").append(NAMES[direction]).append(' ')
                    .append(counts[direction]).append('\n');
            for (int start = 0; start < masks.size(); start += 256) {
                int end = Math.min(start + 256, masks.size());
                sb.append("P ").append(NAMES[direction]).append(' ').append(start).append(' ');
                for (int state = start; state < end; state++) {
                    sb.append((masks.get(state) & (1 << direction)) == 0 ? '0' : '1');
                }
                sb.append('\n');
            }
        }

        System.out.print(sb);
    }
}
