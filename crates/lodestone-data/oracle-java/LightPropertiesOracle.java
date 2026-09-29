import java.io.PrintStream;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;

/** Queries cached state light inputs; adjacent face occlusion is a separate rule. */
public final class LightPropertiesOracle {
    public static void main(String[] args) {
        if (args.length != 0) {
            throw new IllegalArgumentException("LightPropertiesOracle takes no arguments");
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
            if (!name.matches("[a-z0-9_.-]+:[a-z0-9/._-]+")) {
                throw new IllegalStateException("Invalid block resource name: " + name);
            }
            String properties = properties(state);
            if (!signatures.add(name + "[" + properties + "]")) {
                throw new IllegalStateException("Duplicate semantic state: " + name + "[" + properties + "]");
            }
            int dampening = state.getLightDampening();
            int emission = state.getLightEmission();
            if (dampening < 0 || dampening > 15 || emission < 0 || emission > 15) {
                throw new IllegalStateException("Out-of-range light inputs for state " + id);
            }
            rows.append(id).append(' ').append(name).append(' ')
                    .append(dampening).append(' ').append(emission).append(' ')
                    .append(properties).append('\n');
            populations.merge(state.getBlock(), 1, Integer::sum);
            count++;
        }

        int definedStates = 0;
        for (Block block : BuiltInRegistries.BLOCK) {
            int expected = block.getStateDefinition().getPossibleStates().size();
            if (expected == 0 || populations.getOrDefault(block, 0) != expected) {
                throw new IllegalStateException("Incomplete state census for " + BuiltInRegistries.BLOCK.getKey(block));
            }
            for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                int id = Block.BLOCK_STATE_REGISTRY.getId(state);
                if (id < 0 || Block.BLOCK_STATE_REGISTRY.byId(id) != state) {
                    throw new IllegalStateException("Unregistered state for " + BuiltInRegistries.BLOCK.getKey(block));
                }
            }
            definedStates = Math.addExact(definedStates, expected);
        }
        int blocks = BuiltInRegistries.BLOCK.size();
        if (count == 0 || count != definedStates || count != Block.BLOCK_STATE_REGISTRY.size()
                || populations.size() != blocks) {
            throw new IllegalStateException("Incomplete block-state registry census");
        }

        StringBuilder dump = new StringBuilder();
        dump.append("# LightPropertiesOracle ").append(SharedConstants.getCurrentVersion().name())
                .append(" protocol=").append(SharedConstants.getCurrentVersion().protocolVersion()).append('\n');
        dump.append("# id block-resource-name raw-dampening emission sorted-properties-or-dash\n");
        dump.append("C ").append(count).append(' ').append(blocks).append('\n');
        dump.append(rows);
        dump.append("E ").append(count).append(' ').append(blocks).append('\n');
        output.print(dump);
        if (output.checkError()) {
            throw new IllegalStateException("Could not write complete light-properties dump");
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
