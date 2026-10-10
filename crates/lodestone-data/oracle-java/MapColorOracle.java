import java.io.PrintStream;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.level.material.MapColor;

/**
 * Per-block-state map inputs, from the real server's block-state registry:
 * the map colour id a state paints, whether it holds a fluid, and the colour id
 * of the block a map shows instead when the state holds a fluid and its top face
 * is not sturdy (the fluid's own legacy block). Also the 64-entry colour palette.
 */
public final class MapColorOracle {
    public static void main(String[] args) {
        if (args.length != 0) {
            throw new IllegalArgumentException("MapColorOracle takes no arguments");
        }
        PrintStream output = System.out;
        System.setOut(System.err);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Map<Block, Integer> populations = new HashMap<>();
        Set<String> signatures = new HashSet<>();
        StringBuilder rows = new StringBuilder();
        int count = 0;
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            if (id != count || Block.BLOCK_STATE_REGISTRY.byId(id) != state) {
                throw new IllegalStateException("Non-contiguous state registry at " + count);
            }
            String name = BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
            String properties = properties(state);
            if (!signatures.add(name + "[" + properties + "]")) {
                throw new IllegalStateException("Duplicate semantic state: " + name + "[" + properties + "]");
            }
            int color = state.getMapColor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO).id;
            FluidState fluid = state.getFluidState();
            int fluidFlag = fluid.isEmpty() ? 0 : 1;
            int surface = color;
            if (!fluid.isEmpty() && !state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.UP)) {
                surface = fluid.createLegacyBlock().getMapColor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO).id;
            }
            if (color < 0 || color > 63 || surface < 0 || surface > 63) {
                throw new IllegalStateException("Out-of-range map colour for state " + id);
            }
            rows.append(id).append(' ').append(name).append(' ')
                    .append(color).append(' ').append(fluidFlag).append(' ').append(surface).append(' ')
                    .append(properties).append('\n');
            populations.merge(state.getBlock(), 1, Integer::sum);
            count++;
        }
        int blocks = BuiltInRegistries.BLOCK.size();
        if (count == 0 || count != Block.BLOCK_STATE_REGISTRY.size() || populations.size() != blocks) {
            throw new IllegalStateException("Incomplete block-state registry census");
        }

        StringBuilder dump = new StringBuilder();
        dump.append("# MapColorOracle ").append(SharedConstants.getCurrentVersion().name())
                .append(" protocol=").append(SharedConstants.getCurrentVersion().protocolVersion()).append('\n');
        dump.append("# P <colour id> <rgb>: the map colour palette, defined ids only\n");
        dump.append("# id block-resource-name map-colour-id holds-fluid surface-colour-id sorted-properties-or-dash\n");
        dump.append("C ").append(count).append(' ').append(blocks).append('\n');
        for (int i = 0; i < 64; i++) {
            MapColor color = MapColor.byId(i);
            if (color.id == i) {
                dump.append("P ").append(i).append(' ').append(color.col).append('\n');
            }
        }
        dump.append(rows);
        dump.append("E ").append(count).append(' ').append(blocks).append('\n');
        output.print(dump);
        if (output.checkError()) {
            throw new IllegalStateException("Could not write complete map-colour dump");
        }
    }

    private static String properties(BlockState state) {
        Map<String, String> values = new TreeMap<>();
        for (Property<?> property : state.getProperties()) {
            String key = property.getName();
            String value = valueName(state, property);
            if (!key.matches("[a-z0-9_]+") || !value.matches("[a-z0-9_]+")
                    || values.put(key, value) != null) {
                throw new IllegalStateException("Invalid or duplicate state property: " + key + "=" + value);
            }
        }
        if (values.isEmpty()) {
            return "-";
        }
        return String.join(",", values.entrySet().stream().map(e -> e.getKey() + "=" + e.getValue()).toList());
    }

    private static <T extends Comparable<T>> String valueName(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }
}
