// Direct compiled-server oracle for the three coral feature forms.  The
// in-memory level deliberately throws on an unimplemented proxy method: a
// false default would turn a missing water/survival query into a vacuous pass.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.network.chat.Component;
import net.minecraft.server.packs.PackLocationInfo;
import net.minecraft.server.packs.PackResources;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.PathPackResources;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.server.packs.resources.ResourceManager;
import net.minecraft.tags.TagKey;
import net.minecraft.tags.TagLoader;
import net.minecraft.core.Holder;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.levelgen.feature.CoralClawFeature;
import net.minecraft.world.level.levelgen.feature.CoralMushroomFeature;
import net.minecraft.world.level.levelgen.feature.CoralTreeFeature;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.configurations.NoneFeatureConfiguration;

public final class CoralFeatureOracle {
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
            if (n.equals("getBlockState")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.WATER.defaultBlockState());
            if (n.equals("getFluidState")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.WATER.defaultBlockState()).getFluidState();
            if (n.equals("isEmptyBlock")) return blocks.getOrDefault(key((BlockPos)a[0]), Blocks.WATER.defaultBlockState()).isAir();
            if (n.equals("setBlock")) { blocks.put(key((BlockPos)a[0]), (BlockState)a[1]); return Boolean.TRUE; }
            if (n.equals("getMinY") || n.equals("getMinBuildHeight")) return 0;
            if (n.equals("getMaxY") || n.equals("getMaxBuildHeight")) return 128;
            if (n.equals("isOutsideBuildHeight")) return Boolean.FALSE;
            if (n.equals("getSeed")) return 0L;
            if (n.equals("toString")) return "CoralFeatureOracle";
            if (n.equals("hashCode")) return System.identityHashCode(proxy);
            if (n.equals("equals")) return proxy == a[0];
            throw new UnsupportedOperationException("unhandled level method " + n + "/" + (a == null ? 0 : a.length));
        };
        return (WorldGenLevel)Proxy.newProxyInstance(CoralFeatureOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, h);
    }
    static FeaturePlaceContext<NoneFeatureConfiguration> context(WorldGenLevel l, long seed) {
        return new FeaturePlaceContext<>(Optional.empty(), l, null, RandomSource.create(seed), new BlockPos(0, 64, 0), NoneFeatureConfiguration.INSTANCE);
    }
    static void dump(String label, Map<String, BlockState> blocks) {
        for (Map.Entry<String, BlockState> e : new TreeMap<>(blocks).entrySet()) {
            if (!e.getValue().is(Blocks.WATER)) System.out.println(label + " " + e.getKey() + " " + state(e.getValue()));
        }
    }
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); bindTags();
        for (long seed : new long[]{11L, 12L, 13L}) {
            Map<String, BlockState> tree = new TreeMap<>(); new CoralTreeFeature(NoneFeatureConfiguration.CODEC).place(context(level(tree), seed)); dump("tree."+seed, tree);
            Map<String, BlockState> claw = new TreeMap<>(); new CoralClawFeature(NoneFeatureConfiguration.CODEC).place(context(level(claw), seed)); dump("claw."+seed, claw);
            Map<String, BlockState> mushroom = new TreeMap<>(); new CoralMushroomFeature(NoneFeatureConfiguration.CODEC).place(context(level(mushroom), seed)); dump("mushroom."+seed, mushroom);
        }
        Map<String, BlockState> dryOrigin = new TreeMap<>();
        dryOrigin.put(key(new BlockPos(0, 64, 0)), Blocks.AIR.defaultBlockState());
        boolean dryOriginResult = new CoralClawFeature(NoneFeatureConfiguration.CODEC).place(context(level(dryOrigin), 77));
        System.out.println("control.dry_origin result=" + dryOriginResult + " writes=" + dryOrigin.size());
        Map<String, BlockState> dryAbove = new TreeMap<>();
        dryAbove.put(key(new BlockPos(0, 65, 0)), Blocks.AIR.defaultBlockState());
        boolean dryAboveResult = new CoralClawFeature(NoneFeatureConfiguration.CODEC).place(context(level(dryAbove), 77));
        System.out.println("control.dry_above result=" + dryAboveResult + " writes=" + dryAbove.size());
    }
}
