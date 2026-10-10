import java.util.ArrayList;
import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Extracts, for every built-in block state, whether the state conducts
 * redstone power (a bat roosts only under such a block).
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   K &lt;countOfTrueStates&gt;
 *   P &lt;startStateId&gt; &lt;bitstring, up to 256 chars&gt;
 * </pre>
 */
public final class RedstoneConductorOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder blocks = new StringBuilder();
        List<Boolean> bits = new ArrayList<>();
        Block previousBlock = null;
        int count = 0;
        int trues = 0;
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
            boolean conducts = state.isRedstoneConductor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
            bits.add(conducts);
            if (conducts) {
                trues++;
            }
            count++;
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# RedstoneConductorOracle dump from the real 26.2 server (protocol 776).\n");
        sb.append("# A bit is set when the state conducts redstone power.\n");
        sb.append("# C <stateCount> <blockCount>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# K <countOfTrueStates>\n");
        sb.append("# P <startStateId> <bitstring up to 256 chars>\n");
        sb.append("C ").append(count).append(' ').append(BuiltInRegistries.BLOCK.size()).append('\n');
        sb.append(blocks);
        sb.append("K ").append(trues).append('\n');
        for (int start = 0; start < bits.size(); start += 256) {
            int end = Math.min(start + 256, bits.size());
            sb.append("P ").append(start).append(' ');
            for (int state = start; state < end; state++) {
                sb.append(bits.get(state) ? '1' : '0');
            }
            sb.append('\n');
        }
        System.out.print(sb);
    }
}
