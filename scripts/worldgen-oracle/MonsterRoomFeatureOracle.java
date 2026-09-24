// Direct compiled-server fixture for the underground room feature. The level
// proxy is deliberately strict: an accidental new query must fail instead of
// turning a missing world fact into a passing empty room.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import java.util.HashMap;
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
import net.minecraft.world.RandomizableContainer;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.ChestBlock;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.entity.ChestBlockEntity;
import net.minecraft.world.level.block.entity.SpawnerBlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.MonsterRoomFeature;
import net.minecraft.world.level.levelgen.feature.configurations.NoneFeatureConfiguration;

public final class MonsterRoomFeatureOracle {
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

    static <T extends Comparable<T>> String value(BlockState s, net.minecraft.world.level.block.state.properties.Property<T> p) {
        return p.getName(s.getValue(p));
    }

    static String state(BlockState s) {
        TreeMap<String, String> props = new TreeMap<>();
        for (var p : s.getProperties()) props.put(p.getName(), value(s, p));
        String out = BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString();
        if (!props.isEmpty()) {
            out += "[" + props.entrySet().stream()
                .map(e -> e.getKey() + "=" + e.getValue())
                .collect(java.util.stream.Collectors.joining(",")) + "]";
        }
        return out;
    }

    static final class FixtureLevel {
        final Map<String, BlockState> blocks = new HashMap<>();
        final Map<String, BlockState> writes = new TreeMap<>();
        final Map<String, BlockEntity> entities = new HashMap<>();
    }

    static BlockState state(FixtureLevel fixture, BlockPos pos) {
        return fixture.blocks.getOrDefault(key(pos), Blocks.STONE.defaultBlockState());
    }

    static WorldGenLevel level(FixtureLevel fixture) {
        InvocationHandler handler = (proxy, method, args) -> {
            String name = method.getName();
            if (name.equals("getBlockState")) return state(fixture, (BlockPos)args[0]);
            if (name.equals("isEmptyBlock")) return state(fixture, (BlockPos)args[0]).isAir();
            if (name.equals("getBlockEntity")) return fixture.entities.get(key((BlockPos)args[0]));
            if (name.equals("setBlock")) {
                BlockPos pos = (BlockPos)args[0];
                BlockState block = (BlockState)args[1];
                String k = key(pos);
                fixture.blocks.put(k, block);
                fixture.writes.put(k, block);
                if (block.is(Blocks.CHEST)) fixture.entities.put(k, new ChestBlockEntity(pos, block));
                if (block.is(Blocks.SPAWNER)) fixture.entities.put(k, new SpawnerBlockEntity(pos, block));
                return Boolean.TRUE;
            }
            if (name.equals("getMinY") || name.equals("getMinBuildHeight")) return 0;
            if (name.equals("getMaxY") || name.equals("getMaxBuildHeight")) return 128;
            if (name.equals("isOutsideBuildHeight")) return Boolean.FALSE;
            if (name.equals("toString")) return "MonsterRoomFeatureOracle";
            if (name.equals("hashCode")) return System.identityHashCode(proxy);
            if (name.equals("equals")) return proxy == args[0];
            throw new UnsupportedOperationException("unhandled level method " + name + "/" + (args == null ? 0 : args.length));
        };
        return (WorldGenLevel)Proxy.newProxyInstance(
            MonsterRoomFeatureOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler);
    }

    static FeaturePlaceContext<NoneFeatureConfiguration> context(WorldGenLevel level, long seed) {
        return new FeaturePlaceContext<>(Optional.empty(), level, null, RandomSource.create(seed),
            new BlockPos(0, 64, 0), NoneFeatureConfiguration.INSTANCE);
    }

    static FixtureLevel positive(long seed) {
        FixtureLevel fixture = new FixtureLevel();
        RandomSource preview = RandomSource.create(seed);
        int xr = preview.nextInt(2) + 2;
        preview.nextInt(2);
        int x = -xr - 1;
        fixture.blocks.put(x + ",64,0", Blocks.CAVE_AIR.defaultBlockState());
        fixture.blocks.put(x + ",65,0", Blocks.CAVE_AIR.defaultBlockState());
        return fixture;
    }

    static long digest(FixtureLevel fixture) {
        long hash = 1469598103934665603L;
        for (Map.Entry<String, BlockState> entry : fixture.writes.entrySet()) {
            String row = entry.getKey() + " " + state(entry.getValue()) + "\n";
            for (byte value : row.getBytes(java.nio.charset.StandardCharsets.UTF_8)) {
                hash ^= value & 0xffL;
                hash *= 1099511628211L;
            }
        }
        return hash;
    }

    static void dump(String label, FixtureLevel fixture) {
        Map<String, Integer> counts = new TreeMap<>();
        for (BlockState block : fixture.writes.values()) {
            counts.merge(state(block), 1, Integer::sum);
        }
        System.out.println(label + ".writes " + fixture.writes.size());
        System.out.println(label + ".digest " + Long.toUnsignedString(digest(fixture), 16));
        for (Map.Entry<String, Integer> entry : counts.entrySet()) {
            System.out.println(label + ".count " + entry.getKey() + " " + entry.getValue());
        }
        for (String position : new String[]{"0,64,0", "0,64,2", "3,64,1", "-4,65,0"}) {
            BlockState block = fixture.writes.get(position);
            if (block != null) System.out.println(label + ".cell " + position + " " + state(block));
        }
        for (Map.Entry<String, BlockEntity> entry : fixture.entities.entrySet()) {
            BlockEntity entity = entry.getValue();
            if (entity instanceof ChestBlockEntity chest) {
                String facing = chest.getBlockState().getValue(ChestBlock.FACING).getSerializedName();
                RandomizableContainer container = chest;
                System.out.println(label + ".entity.chest " + entry.getKey() + " " + facing + " "
                    + container.getLootTable() + " " + container.getLootTableSeed());
            } else if (entity instanceof SpawnerBlockEntity) {
                System.out.println(label + ".entity.spawner " + entry.getKey());
            }
        }
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindTags();
        long seed = 19L;
        FixtureLevel positive = positive(seed);
        boolean result = new MonsterRoomFeature(NoneFeatureConfiguration.CODEC)
            .place(context(level(positive), seed));
        System.out.println("positive.result " + result);
        dump("positive", positive);

        FixtureLevel sealed = new FixtureLevel();
        boolean sealedResult = new MonsterRoomFeature(NoneFeatureConfiguration.CODEC)
            .place(context(level(sealed), seed));
        System.out.println("control.sealed result=" + sealedResult + " writes=" + sealed.writes.size());
    }
}
