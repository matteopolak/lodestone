// Direct compiled-server fixture for the sulfur cave cluster feature.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
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
import net.minecraft.util.valueproviders.ConstantFloat;
import net.minecraft.util.valueproviders.UniformFloat;
import net.minecraft.util.valueproviders.UniformInt;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.SpeleothemClusterFeature;
import net.minecraft.world.level.levelgen.feature.configurations.SpeleothemClusterConfiguration;

public final class SpeleothemClusterOracle {
    @SuppressWarnings("unchecked")
    static void bindTags() {
        PackLocationInfo info = new PackLocationInfo("builtin", Component.literal("builtin"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(info, java.nio.file.Path.of("/mc/src"));
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, java.util.List.of(pack));
        Map<TagKey<Block>, java.util.List<Holder<Block>>> tags = TagLoader.loadTagsForRegistry(manager, Registries.BLOCK,
            (TagLoader.ElementLookup<Holder<Block>>)TagLoader.ElementLookup.fromFrozenRegistry(BuiltInRegistries.BLOCK));
        BuiltInRegistries.BLOCK.prepareTagReload(new TagLoader.LoadResult<>(Registries.BLOCK, tags)).apply();
    }
    static String key(BlockPos p) { return p.getX()+","+p.getY()+","+p.getZ(); }
    static <T extends Comparable<T>> String value(BlockState s, Property<T> p) { return p.getName(s.getValue(p)); }
    static String state(BlockState s) {
        TreeMap<String,String> props = new TreeMap<>();
        for (Property<?> p : s.getProperties()) props.put(p.getName(), value(s, p));
        String out = BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString();
        if (!props.isEmpty()) out += "["+props.entrySet().stream().map(e -> e.getKey()+"="+e.getValue()).collect(java.util.stream.Collectors.joining(","))+"]";
        return out;
    }
    static WorldGenLevel level(Map<String,BlockState> blocks) {
        InvocationHandler h = (proxy, method, a) -> {
            String n = method.getName();
            if (n.equals("getBlockState")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState());
            if (n.equals("isStateAtPosition")) {
                @SuppressWarnings("unchecked") java.util.function.Predicate<BlockState> predicate = (java.util.function.Predicate<BlockState>)a[1];
                return predicate.test(blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()));
            }
            if (n.equals("getFluidState")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()).getFluidState();
            if (n.equals("isEmptyBlock")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()).isAir();
            if (n.equals("isWaterAt")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.AIR.defaultBlockState()).is(Blocks.WATER);
            if (n.equals("setBlock")) { blocks.put(key((BlockPos)a[0]), (BlockState)a[1]); return Boolean.TRUE; }
            if (n.equals("getMinY") || n.equals("getMinBuildHeight")) return 0;
            if (n.equals("getMaxY") || n.equals("getMaxBuildHeight")) return 128;
            if (n.equals("isOutsideBuildHeight")) return Boolean.FALSE;
            if (n.equals("getSeed")) return 0L;
            if (n.equals("hashCode")) return System.identityHashCode(proxy);
            if (n.equals("equals")) return proxy == a[0];
            throw new UnsupportedOperationException("unhandled level method "+n+"/"+(a == null ? 0 : a.length));
        };
        return (WorldGenLevel)Proxy.newProxyInstance(SpeleothemClusterOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, h);
    }
    static SpeleothemClusterConfiguration config() {
        return new SpeleothemClusterConfiguration(
            Blocks.SULFUR.defaultBlockState(), Blocks.SULFUR_SPIKE.defaultBlockState(),
            net.minecraft.core.HolderSet.direct(Blocks.SULFUR.builtInRegistryHolder(), Blocks.CINNABAR.builtInRegistryHolder()),
            12, UniformInt.of(1,4), UniformInt.of(2,8), 1, 3, UniformInt.of(2,4),
            UniformFloat.of(.3F,.7F), ConstantFloat.ZERO, .1F, 3, 8);
    }
    static boolean run(Map<String,BlockState> blocks, long seed) {
        return new SpeleothemClusterFeature(SpeleothemClusterConfiguration.CODEC).place(
            new FeaturePlaceContext<>(Optional.empty(), level(blocks), null, RandomSource.create(seed), new BlockPos(0,64,0), config()));
    }
    static Map<String,BlockState> cave() {
        Map<String,BlockState> out = new TreeMap<>();
        for (int x=-1; x<=1; x++) for (int z=-1; z<=1; z++) {
            out.put(key(new BlockPos(x,60,z)), Blocks.CINNABAR.defaultBlockState());
            out.put(key(new BlockPos(x,68,z)), Blocks.CINNABAR.defaultBlockState());
        }
        return out;
    }
    static void dump(long seed) {
        Map<String,BlockState> blocks = cave(); Map<String,BlockState> before = new TreeMap<>(blocks);
        System.out.println("cluster."+seed+" result="+run(blocks,seed));
        for (Map.Entry<String,BlockState> e : blocks.entrySet()) if (!e.getValue().equals(before.get(e.getKey())))
            System.out.println("cluster."+seed+" "+e.getKey()+" "+state(e.getValue()));
    }
    public static void main(String[] args) { SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); bindTags(); dump(0); }
}
