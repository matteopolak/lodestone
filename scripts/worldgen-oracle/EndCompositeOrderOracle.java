import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.ServerLevelAccessor;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.ChorusFlowerBlock;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.levelgen.feature.EndIslandFeature;
import net.minecraft.world.level.levelgen.feature.EndPlatformFeature;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.configurations.NoneFeatureConfiguration;

public final class EndCompositeOrderOracle {
    static <T extends Comparable<T>> String propertyValue(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }

    static String canonical(BlockState state) {
        StringBuilder out = new StringBuilder(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        TreeMap<String, String> properties = new TreeMap<>();
        for (Property<?> property : state.getProperties()) properties.put(property.getName(), propertyValue(state, property));
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

    static InvocationHandler handler(Map<String, BlockState> states) {
        return (proxy, method, args) -> {
            String name = method.getName();
            if (name.equals("getBlockState")) {
                return states.getOrDefault(key((BlockPos) args[0]), Blocks.AIR.defaultBlockState());
            }
            if (name.equals("isEmptyBlock")) {
                return states.getOrDefault(key((BlockPos) args[0]), Blocks.AIR.defaultBlockState()).isAir();
            }
            if (name.equals("setBlock")) {
                states.put(key((BlockPos) args[0]), (BlockState) args[1]);
                return Boolean.TRUE;
            }
            if (name.equals("getBlockEntity")) return null;
            if (name.equals("getMinY") || name.equals("getMinBuildHeight")) return 0;
            if (name.equals("getMaxY")) return 255;
            if (name.equals("getMaxBuildHeight")) return 256;
            if (name.equals("isOutsideBuildHeight")) return Boolean.FALSE;
            if (name.equals("getSeed")) return 0L;
            Class<?> result = method.getReturnType();
            if (result == boolean.class) return Boolean.FALSE;
            if (result == int.class) return 0;
            if (result == long.class) return 0L;
            if (result.isPrimitive()) return 0;
            return null;
        };
    }

    static WorldGenLevel world(Map<String, BlockState> states) {
        return (WorldGenLevel) Proxy.newProxyInstance(
            EndCompositeOrderOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler(states));
    }

    static ServerLevelAccessor serverWorld(Map<String, BlockState> states) {
        return (ServerLevelAccessor) Proxy.newProxyInstance(
            EndCompositeOrderOracle.class.getClassLoader(), new Class[]{ServerLevelAccessor.class}, handler(states));
    }

    static <T extends net.minecraft.world.level.levelgen.feature.configurations.FeatureConfiguration>
    FeaturePlaceContext<T> context(WorldGenLevel world, RandomSource random, BlockPos origin, T config) {
        return new FeaturePlaceContext<>(Optional.empty(), world, null, random, origin, config);
    }

    static void structure(Map<String, BlockState> states) {
        for (int x = -1; x <= 1; x++) for (int z = -1; z <= 1; z++) {
            states.put(key(new BlockPos(x, 50, z)), Blocks.PURPUR_BLOCK.defaultBlockState());
        }
    }

    static void island(Map<String, BlockState> states) {
        WorldGenLevel world = world(states);
        new EndIslandFeature(NoneFeatureConfiguration.CODEC).place(context(
            world, RandomSource.create(918_273L), new BlockPos(0, 50, 0), NoneFeatureConfiguration.INSTANCE));
    }

    static void chorus(Map<String, BlockState> states) {
        WorldGenLevel world = world(states);
        ChorusFlowerBlock.generatePlant(world, new BlockPos(0, 65, 0), RandomSource.create(12_345L), 8);
    }

    static void platform(Map<String, BlockState> states) {
        EndPlatformFeature.createEndPlatform(serverWorld(states), new BlockPos(0, 65, 0), false);
    }

    static Map<String, BlockState> run(String... operations) {
        Map<String, BlockState> states = new TreeMap<>();
        states.put(key(new BlockPos(0, 49, 0)), Blocks.END_STONE.defaultBlockState());
        for (String operation : operations) {
            switch (operation) {
                case "outer_island" -> island(states);
                case "structure" -> structure(states);
                case "chorus" -> chorus(states);
                case "platform" -> platform(states);
                default -> throw new IllegalArgumentException(operation);
            }
        }
        return states;
    }

    static String digest(Map<String, BlockState> states) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        for (Map.Entry<String, BlockState> entry : states.entrySet()) {
            digest.update((entry.getKey() + "=" + canonical(entry.getValue()) + "\n").getBytes(StandardCharsets.UTF_8));
        }
        StringBuilder hex = new StringBuilder();
        for (byte value : digest.digest()) hex.append(String.format("%02x", value));
        return hex.toString();
    }

    static void emit(String id, long seed, String origin, String expectedOrder, String wrongOrder, String... probes) throws Exception {
        String[] expectedOperations = expectedOrder.split(",");
        String[] wrongOperations = wrongOrder.split(",");
        Map<String, BlockState> expected = run(expectedOperations);
        Map<String, BlockState> wrong = run(wrongOperations);
        System.out.println("case " + id + " seed=" + seed + " origin=" + origin + " expected=" + expectedOrder + " wrong=" + wrongOrder);
        System.out.println("expected_digest " + digest(expected));
        System.out.println("wrong_digest " + digest(wrong));
        for (String probe : probes) {
            BlockPos pos = parse(probe);
            String coordinate = key(pos);
            System.out.println("probe " + coordinate + " expected=" + canonical(expected.getOrDefault(coordinate, Blocks.AIR.defaultBlockState()))
                + " wrong=" + canonical(wrong.getOrDefault(coordinate, Blocks.AIR.defaultBlockState())));
        }
    }

    static BlockPos parse(String value) {
        String[] parts = value.split(",");
        if (parts.length != 3) throw new IllegalArgumentException(value);
        return new BlockPos(Integer.parseInt(parts[0]), Integer.parseInt(parts[1]), Integer.parseInt(parts[2]));
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        System.out.println("format end-composite-order-v1");
        emit("outer_island_structure", 918_273L, "0,50,0", "outer_island,structure", "structure,outer_island", "0,50,0", "1,49,0");
        emit("chorus_platform", 12_345L, "0,65,0", "chorus,platform", "platform,chorus", "0,64,0", "0,65,0", "0,66,0");
    }
}
