// Independent End feature oracle.  Each case invokes the bundled server's
// compiled feature implementation against a small in-memory WorldGenLevel.  It
// deliberately records the gateway's block footprint separately from its exit:
// the exit is block-entity data, not a block-state property.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.levelgen.feature.ChorusPlantFeature;
import net.minecraft.world.level.levelgen.feature.EndGatewayFeature;
import net.minecraft.world.level.levelgen.feature.EndIslandFeature;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.configurations.EndGatewayConfiguration;
import net.minecraft.world.level.levelgen.feature.configurations.NoneFeatureConfiguration;

public final class EndDecorationOracle {
    static <T extends Comparable<T>> String prop(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }

    static String canonical(BlockState state) {
        StringBuilder out = new StringBuilder(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        TreeMap<String, String> properties = new TreeMap<>();
        for (Property<?> property : state.getProperties()) properties.put(property.getName(), prop(state, property));
        if (!properties.isEmpty()) {
            out.append('[');
            boolean first = true;
            for (Map.Entry<String, String> property : properties.entrySet()) {
                if (!first) out.append(',');
                first = false;
                out.append(property.getKey()).append('=').append(property.getValue());
            }
            out.append(']');
        }
        return out.toString();
    }

    static String key(BlockPos pos) {
        return pos.getX() + "," + pos.getY() + "," + pos.getZ();
    }

    static WorldGenLevel level(Map<String, BlockState> states) {
        InvocationHandler handler = (proxy, method, args) -> {
            String name = method.getName();
            if (name.equals("getBlockState")) {
                return states.getOrDefault(key((BlockPos)args[0]), Blocks.AIR.defaultBlockState());
            }
            if (name.equals("isEmptyBlock")) {
                return states.getOrDefault(key((BlockPos)args[0]), Blocks.AIR.defaultBlockState()).isAir();
            }
            if (name.equals("setBlock")) {
                states.put(key((BlockPos)args[0]), (BlockState)args[1]);
                return Boolean.TRUE;
            }
            if (name.equals("getBlockEntity")) return null;
            if (name.equals("getMinY") || name.equals("getMinBuildHeight")) return 0;
            if (name.equals("getMaxY")) return 127;
            if (name.equals("getMaxBuildHeight")) return 128;
            if (name.equals("isOutsideBuildHeight")) return Boolean.FALSE;
            if (name.equals("getSeed")) return 0L;
            Class<?> result = method.getReturnType();
            if (result == boolean.class) return Boolean.FALSE;
            if (result == int.class) return 0;
            if (result == long.class) return 0L;
            if (result.isPrimitive()) return 0;
            return null;
        };
        return (WorldGenLevel)Proxy.newProxyInstance(
            EndDecorationOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler);
    }

    static void dump(String label, Map<String, BlockState> states) {
        TreeMap<String, String> ordered = new TreeMap<>();
        for (Map.Entry<String, BlockState> state : states.entrySet()) ordered.put(state.getKey(), canonical(state.getValue()));
        for (Map.Entry<String, String> state : ordered.entrySet()) {
            System.out.println(label + " " + state.getKey() + " " + state.getValue());
        }
    }

    static <C extends net.minecraft.world.level.levelgen.feature.configurations.FeatureConfiguration> FeaturePlaceContext<C> context(
        WorldGenLevel level, RandomSource random, BlockPos origin, C config) {
        return new FeaturePlaceContext<>(Optional.empty(), level, null, random, origin, config);
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Map<String, BlockState> island = new TreeMap<>();
        new EndIslandFeature(NoneFeatureConfiguration.CODEC).place(context(
            level(island), RandomSource.create(918_273L), new BlockPos(24, 70, 24), NoneFeatureConfiguration.INSTANCE));
        dump("island", island);

        Map<String, BlockState> chorus = new TreeMap<>();
        chorus.put(key(new BlockPos(0, 64, 0)), Blocks.END_STONE.defaultBlockState());
        // Generate through the native plant routine after supplying its required
        // End-stone support. This keeps its recursive random stream and block-state
        // connection logic in the server implementation.
        net.minecraft.world.level.block.ChorusFlowerBlock.generatePlant(
            level(chorus), new BlockPos(0, 65, 0), RandomSource.create(12_345L), 8);
        chorus.remove(key(new BlockPos(0, 64, 0)));
        dump("chorus", chorus);

        Map<String, BlockState> gateway = new TreeMap<>();
        new EndGatewayFeature(EndGatewayConfiguration.CODEC).place(context(
            level(gateway),
            RandomSource.create(77L),
            new BlockPos(50, 70, 50),
            EndGatewayConfiguration.knownExit(new BlockPos(100, 50, 0), true)));
        dump("gateway", gateway);
        System.out.println("gateway_exit 100,50,0 exact=true");
    }
}
