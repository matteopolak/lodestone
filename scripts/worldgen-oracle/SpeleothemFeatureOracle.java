// Direct compiled-server fixture for the sulfur speleothem configured feature.
// Every level query the feature uses is explicit: an unhandled query throws
// instead of turning an absent check into a plausible negative result.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderSet;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.network.chat.Component;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackLocationInfo;
import net.minecraft.server.packs.PackResources;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.PathPackResources;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.server.packs.resources.ResourceManager;
import net.minecraft.tags.TagKey;
import net.minecraft.tags.TagLoader;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.SpeleothemFeature;
import net.minecraft.world.level.levelgen.feature.configurations.SpeleothemConfiguration;

public final class SpeleothemFeatureOracle {
    @SuppressWarnings("unchecked")
    static void bindTags() {
        PackLocationInfo info = new PackLocationInfo("builtin", Component.literal("builtin"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(info, java.nio.file.Path.of("/mc/src"));
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, java.util.List.of(pack));
        Map<TagKey<Block>, java.util.List<Holder<Block>>> tags = TagLoader.loadTagsForRegistry(manager, Registries.BLOCK,
            (TagLoader.ElementLookup<Holder<Block>>)TagLoader.ElementLookup.fromFrozenRegistry(BuiltInRegistries.BLOCK));
        Registry.PendingTags<Block> pending = BuiltInRegistries.BLOCK.prepareTagReload(new TagLoader.LoadResult<>(Registries.BLOCK, tags));
        pending.apply();
    }
    static String key(BlockPos p) { return p.getX() + "," + p.getY() + "," + p.getZ(); }
    static <T extends Comparable<T>> String value(BlockState s, Property<T> p) { return p.getName(s.getValue(p)); }
    static String state(BlockState s) {
        TreeMap<String, String> props = new TreeMap<>();
        for (Property<?> p : s.getProperties()) props.put(p.getName(), value(s, p));
        String out = BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString();
        if (!props.isEmpty()) out += "[" + props.entrySet().stream().map(e -> e.getKey()+"="+e.getValue()).collect(java.util.stream.Collectors.joining(",")) + "]";
        return out;
    }
    static WorldGenLevel level(Map<String, BlockState> blocks) {
        InvocationHandler h = (proxy, method, a) -> {
            String n = method.getName();
            if (n.equals("getBlockState")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState());
            if (n.equals("getFluidState")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()).getFluidState();
            if (n.equals("isEmptyBlock")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()).isAir();
            if (n.equals("isWaterAt")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()).is(Blocks.WATER);
            if (n.equals("setBlock")) { blocks.put(key((BlockPos)a[0]), (BlockState)a[1]); return Boolean.TRUE; }
            if (n.equals("getMinY") || n.equals("getMinBuildHeight")) return 0;
            if (n.equals("getMaxY") || n.equals("getMaxBuildHeight")) return 128;
            if (n.equals("isOutsideBuildHeight")) return Boolean.FALSE;
            if (n.equals("getSeed")) return 0L;
            if (n.equals("toString")) return "SpeleothemFeatureOracle";
            if (n.equals("hashCode")) return System.identityHashCode(proxy);
            if (n.equals("equals")) return proxy == a[0];
            throw new UnsupportedOperationException("unhandled level method " + n + "/" + (a == null ? 0 : a.length));
        };
        return (WorldGenLevel)Proxy.newProxyInstance(SpeleothemFeatureOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, h);
    }
    static SpeleothemConfiguration config() {
        return new SpeleothemConfiguration(
            Blocks.SULFUR.defaultBlockState(), Blocks.SULFUR_SPIKE.defaultBlockState(),
            HolderSet.direct(Blocks.SULFUR.builtInRegistryHolder(), Blocks.CINNABAR.builtInRegistryHolder()),
            0.2F, 0.7F, 0.5F, 0.5F);
    }
    static boolean run(Map<String, BlockState> blocks, BlockPos pos, long seed) {
        WorldGenLevel level = level(blocks);
        FeaturePlaceContext<SpeleothemConfiguration> context = new FeaturePlaceContext<>(
            Optional.empty(), level, null, RandomSource.create(seed), pos, config());
        return new SpeleothemFeature(SpeleothemConfiguration.CODEC).place(context);
    }
    static void dump(String label, Map<String, BlockState> blocks, boolean placed) {
        System.out.println(label + " result=" + placed);
        for (Map.Entry<String, BlockState> e : new TreeMap<>(blocks).entrySet()) {
            System.out.println(label + " " + e.getKey() + " " + state(e.getValue()));
        }
    }
    static Map<String, BlockState> cinnabarFloor(int centerX) {
        Map<String, BlockState> out = new TreeMap<>();
        for (int x = centerX - 3; x <= centerX + 3; x++) {
            for (int z = -3; z <= 3; z++) {
                out.put(key(new BlockPos(x, 63, z)), Blocks.CINNABAR.defaultBlockState());
            }
        }
        return out;
    }
    static Map<String, BlockState> cinnabarCeiling(int centerX) {
        Map<String, BlockState> out = new TreeMap<>();
        for (int x = centerX - 3; x <= centerX + 3; x++) {
            for (int z = -3; z <= 3; z++) {
                out.put(key(new BlockPos(x, 65, z)), Blocks.CINNABAR.defaultBlockState());
            }
        }
        return out;
    }
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); bindTags();
        for (long seed : new long[]{0L, 1L, 2L, 3L, 4L, 5L, 6L, 7L}) {
            Map<String, BlockState> centre = new TreeMap<>();
            centre.put(key(new BlockPos(0, 63, 0)), Blocks.CINNABAR.defaultBlockState());
            dump("centre." + seed, centre, run(centre, new BlockPos(0, 64, 0), seed));
            Map<String, BlockState> edge = new TreeMap<>();
            edge.put(key(new BlockPos(16, 63, 0)), Blocks.CINNABAR.defaultBlockState());
            dump("edge." + seed, edge, run(edge, new BlockPos(16, 64, 0), seed));
            Map<String, BlockState> field = cinnabarFloor(0);
            dump("field." + seed, field, run(field, new BlockPos(0, 64, 0), seed));
            Map<String, BlockState> edgeField = cinnabarFloor(16);
            dump("edge_field." + seed, edgeField, run(edgeField, new BlockPos(16, 64, 0), seed));
            Map<String, BlockState> ceiling = cinnabarCeiling(0);
            dump("ceiling." + seed, ceiling, run(ceiling, new BlockPos(0, 64, 0), seed));
        }
        Map<String, BlockState> absent = new TreeMap<>();
        dump("control.absent", absent, run(absent, new BlockPos(0, 64, 0), 7L));
    }
}
