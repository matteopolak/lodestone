import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;

/**
 * Extracts, for every built-in block state of the running server, whether the
 * state conducts redstone power (a bat roosts only under such a block).
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt;
 *   S &lt;0|1&gt; &lt;namespaced state string, properties in registry order&gt;
 * </pre>
 *
 * States are named, not numbered, so the consumer joins them to its own ids.
 */
public final class RedstoneConductorOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder rows = new StringBuilder();
        int count = 0;
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            boolean conducts = state.isRedstoneConductor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
            rows.append("S ").append(conducts ? '1' : '0').append(' ')
                    .append(BuiltInRegistries.BLOCK.getKey(state.getBlock()));
            if (!state.getProperties().isEmpty()) {
                rows.append('[');
                boolean first = true;
                for (Property<?> property : state.getProperties()) {
                    if (!first) {
                        rows.append(',');
                    }
                    first = false;
                    rows.append(property.getName()).append('=').append(valueName(state, property));
                }
                rows.append(']');
            }
            rows.append('\n');
            count++;
        }
        System.out.print("# RedstoneConductorOracle dump from the real server.\n"
                + "# C <stateCount> <blockCount>\n# S <conducts 0|1> <state>\n"
                + "C " + count + " " + BuiltInRegistries.BLOCK.size() + "\n" + rows);
    }

    private static <T extends Comparable<T>> String valueName(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }
}
