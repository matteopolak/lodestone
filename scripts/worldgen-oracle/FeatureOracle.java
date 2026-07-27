// Isolated feature oracle: runs vanilla's own doFill + buildSurface + applyCarvers
// to obtain the real post-carve chunk, then runs the UNDERGROUND_ORES decoration
// step (ore features only) for the CENTER chunk and dumps every block those
// features write inside the center 16x16 column, plus the decoration/feature seed
// parameters and the OCEAN_FLOOR_WG heightmap the ore feature reads.
//
// Scope (honest): this proves the ORE FEATURE + PLACEMENT + RNG-order subsystem
// against identical post-carve input. It deliberately does NOT model (a) ore spill
// from the 8 neighbouring chunks into the centre (a 3x3 driver, analogous to the
// carver 17x17 driver), nor (b) earlier feature steps (lakes/springs) that precede
// ores. Both sides start from the same post-carve field and run only the centre
// chunk's ore features, so the in-centre writes are an exact, falsifiable check of
// the feature engine.
//
// The WorldGenLevel the ore feature needs is supplied via a JDK dynamic proxy that
// answers only the handful of methods the ore/placement path actually calls, and
// routes neighbour-chunk writes to throwaway scratch chunks so they cannot corrupt
// the centre. No Mojang source is copied — this only drives compiled classes.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
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
import net.minecraft.world.level.block.Block;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.HolderSet;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.SectionPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.biome.FeatureSorter;
import net.minecraft.world.level.biome.FixedBiomeSource;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.chunk.CarvingMask;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.dimension.DimensionType;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.GenerationStep;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.carver.CarvingContext;
import net.minecraft.world.level.levelgen.carver.ConfiguredWorldCarver;
import net.minecraft.world.level.levelgen.feature.ConfiguredFeature;
import net.minecraft.world.level.levelgen.feature.OreFeature;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import com.mojang.serialization.Lifecycle;

public final class FeatureOracle {
    static final StringBuilder sb = new StringBuilder();

    static <T extends Comparable<T>> String propVal(BlockState s, Property<T> p) {
        return p.getName(s.getValue(p));
    }

    static String canon(BlockState s) {
        StringBuilder b = new StringBuilder(BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString());
        Map<String, String> props = new TreeMap<>();
        for (Property<?> p : s.getProperties()) props.put(p.getName(), propVal(s, p));
        if (!props.isEmpty()) {
            b.append('[');
            boolean first = true;
            for (Map.Entry<String, String> e : props.entrySet()) {
                if (!first) b.append(',');
                first = false;
                b.append(e.getKey()).append('=').append(e.getValue());
            }
            b.append(']');
        }
        return b.toString();
    }

    @SuppressWarnings("unchecked")
    static void bindBlockTags() {
        Path root = Path.of("/mc/src");
        PackLocationInfo loc = new PackLocationInfo(
            "vanilla", Component.literal("vanilla"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(loc, root);
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(pack));
        Map<TagKey<Block>, List<Holder<Block>>> tags = TagLoader.loadTagsForRegistry(
            manager, Registries.BLOCK,
            (TagLoader.ElementLookup<Holder<Block>>) TagLoader.ElementLookup.fromFrozenRegistry(BuiltInRegistries.BLOCK));
        TagLoader.LoadResult<Block> lr = new TagLoader.LoadResult<>(Registries.BLOCK, tags);
        Registry.PendingTags<Block> pending = BuiltInRegistries.BLOCK.prepareTagReload(lr);
        pending.apply();
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindBlockTags();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();

        String rawArgs = System.getenv("ORACLE_ARGS");
        if (rawArgs == null) rawArgs = "";
        rawArgs = rawArgs.trim();
        String biomeName = "minecraft:plains";
        int chunkX = 0, chunkZ = 0;
        long seed = 42L;
        if (!rawArgs.isBlank()) {
            String[] toks = rawArgs.split("\\s+");
            if (toks.length >= 1 && !toks[0].isBlank()) biomeName = toks[0].trim();
            if (toks.length >= 3) {
                chunkX = Integer.parseInt(toks[1].trim());
                chunkZ = Integer.parseInt(toks[2].trim());
            }
            if (toks.length >= 4) seed = Long.parseLong(toks[3].trim());
        }

        ResourceKey<Biome> biomeKey = ResourceKey.create(
            Registries.BIOME, net.minecraft.resources.Identifier.parse(biomeName));
        Holder.Reference<Biome> biomeHolder = provider.lookupOrThrow(Registries.BIOME).getOrThrow(biomeKey);

        Holder<NoiseGeneratorSettings> settingsHolder =
            provider.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        NoiseGeneratorSettings settings = settingsHolder.value();

        FixedBiomeSource biomeSource = new FixedBiomeSource(biomeHolder);
        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(biomeSource, settingsHolder);
        RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);

        int minY = -64;
        int height = 384;
        LevelHeightAccessor heightAccessor = LevelHeightAccessor.create(minY, height);

        MappedRegistry<Biome> biomeReg = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        Registry.register(biomeReg, Biomes.PLAINS, provider.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS).value());
        biomeReg.freeze();
        RegistryAccess paletteAccess = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(biomeReg));
        PalettedContainerFactory factory = PalettedContainerFactory.create(paletteAccess);

        ChunkPos chunkPos = new ChunkPos(chunkX, chunkZ);
        ProtoChunk chunk = new ProtoChunk(chunkPos, UpgradeData.EMPTY, heightAccessor, factory, null);

        Aquifer.FluidStatus lavaStatus = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus seaStatus = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker fluidPicker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lavaStatus : seaStatus;

        NoiseChunk noiseChunk = chunk.getOrCreateNoiseChunk(c -> NoiseChunk.forChunk(
            c, rs, Beardifier.EMPTY, settings, fluidPicker, Blender.empty()));

        generator.fillFromNoise(Blender.empty(), rs, null, chunk).join();

        WorldGenerationContext wgctx = new WorldGenerationContext(generator, heightAccessor);
        BiomeManager biomeManager = new BiomeManager(
            (qx, qy, qz) -> biomeHolder, BiomeManager.obfuscateSeed(seed));
        generator.buildSurface(chunk, wgctx, rs, null, biomeManager, Blender.empty(), null);

        // ---- applyCarvers (post-carve chunk is the ore-decoration input) ----
        Aquifer aquifer = noiseChunk.aquifer();
        CarvingContext context = new CarvingContext(
            generator, paletteAccess, heightAccessor, noiseChunk, rs, settings.surfaceRule());
        CarvingMask mask = chunk.getOrCreateCarvingMask();
        java.util.function.Function<BlockPos, Holder<Biome>> biomeGetter = p -> biomeHolder;
        WorldgenRandom carveRandom = new WorldgenRandom(new LegacyRandomSource(RandomSupport.generateUniqueSeed()));
        for (int dx = -8; dx <= 8; dx++) {
            for (int dz = -8; dz <= 8; dz++) {
                ChunkPos sourcePos = new ChunkPos(chunkPos.x() + dx, chunkPos.z() + dz);
                int index = 0;
                for (Holder<ConfiguredWorldCarver<?>> holder : biomeHolder.value().getGenerationSettings().getCarvers()) {
                    ConfiguredWorldCarver<?> carver = holder.value();
                    carveRandom.setLargeFeatureSeed(seed + index, sourcePos.x(), sourcePos.z());
                    if (carver.isStartChunk(carveRandom)) {
                        try {
                            carver.carve(context, chunk, biomeGetter, carveRandom, aquifer, sourcePos, mask);
                        } catch (Throwable t) { /* ignore, diagnostics live in CarverOracle */ }
                    }
                    index++;
                }
            }
        }

        // ---- Post-carve input snapshot (dumped so the Rust side is self-contained) ----
        BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
        String[] inSnap = new String[16 * 16 * height];
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++) {
                    String c = canon(chunk.getBlockState(p.set(x, y, z)));
                    inSnap[x + z * 16 + (y - minY) * 256] = c;
                    sb.append("in.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(c).append('\n');
                }

        // ---- OCEAN_FLOOR_WG heightmap the ore feature reads (as level.getHeight returns) ----
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                sb.append("ofh.").append(x).append(',').append(z).append(' ')
                  .append(chunk.getHeight(Heightmap.Types.OCEAN_FLOOR_WG, x, z) + 1).append('\n');

        // ---- Feature decoration: UNDERGROUND_ORES step, ore features only ----
        List<FeatureSorter.StepFeatureData> perStep = FeatureSorter.buildFeaturesPerStep(
            List.of(biomeHolder), b -> b.value().getGenerationSettings().features(), true);
        int STEP = GenerationStep.Decoration.UNDERGROUND_ORES.ordinal();

        HolderLookup.RegistryLookup<PlacedFeature> pfLookup = provider.lookupOrThrow(Registries.PLACED_FEATURE);
        Map<PlacedFeature, String> pfIds = new java.util.IdentityHashMap<>();
        pfLookup.listElements().forEach(ref -> pfIds.put(ref.value(), ref.key().identifier().toString()));

        BlockPos origin = SectionPos.of(chunkPos, heightAccessor.getMinSectionY()).origin();
        WorldgenRandom fRandom = new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
        long decorationSeed = fRandom.setDecorationSeed(seed, origin.getX(), origin.getZ());
        sb.append("meta.decorationSeed ").append(decorationSeed).append('\n');
        sb.append("meta.originX ").append(origin.getX()).append('\n');
        sb.append("meta.originZ ").append(origin.getZ()).append('\n');
        sb.append("meta.step ").append(STEP).append('\n');

        WorldGenLevel level = makeLevel(chunk, chunkPos, biomeHolder, heightAccessor, factory, minY, height, seed);

        FeatureSorter.StepFeatureData sfd = perStep.get(STEP);
        List<PlacedFeature> feats = sfd.features();
        int order = 0;
        for (int i = 0; i < feats.size(); i++) {
            PlacedFeature pf = feats.get(i);
            fRandom.setFeatureSeed(decorationSeed, i, STEP);
            ConfiguredFeature<?, ?> cf = pf.feature().value();
            if (cf.feature() instanceof OreFeature) {
                String pid = pfIds.getOrDefault(pf, "?");
                sb.append("oredef.").append(order).append(' ').append(pid).append(' ').append(i).append('\n');
                order++;
                pf.placeWithBiomeCheck(level, generator, fRandom, origin);
            }
        }
        sb.append("meta.oreFeatureCount ").append(order).append('\n');

        // ---- Diff: every in-centre block the ore features changed ----
        int changed = 0;
        Map<String, Integer> perBlock = new TreeMap<>();
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++) {
                    String c = canon(chunk.getBlockState(p.set(x, y, z)));
                    if (!c.equals(inSnap[x + z * 16 + (y - minY) * 256])) {
                        changed++;
                        perBlock.merge(c, 1, Integer::sum);
                        sb.append("ore.").append(x).append(',').append(y).append(',').append(z)
                          .append(' ').append(c).append('\n');
                    }
                }
        sb.append("meta.oreChanged ").append(changed).append('\n');
        for (Map.Entry<String, Integer> e : perBlock.entrySet())
            sb.append("count.").append(e.getKey()).append(' ').append(e.getValue()).append('\n');

        sb.append("meta.biome ").append(biomeName).append('\n');
        sb.append("meta.chunkX ").append(chunkX).append('\n');
        sb.append("meta.chunkZ ").append(chunkZ).append('\n');
        sb.append("meta.minY ").append(minY).append('\n');
        sb.append("meta.height ").append(height).append('\n');
        sb.append("meta.seed ").append(seed).append('\n');

        System.out.print(sb);
    }

    // A minimal WorldGenLevel via dynamic proxy. Only the methods the ore/placement
    // path actually calls are answered; neighbour-chunk writes go to throwaway
    // scratch chunks so they cannot corrupt the centre chunk we dump.
    static WorldGenLevel makeLevel(
        ProtoChunk center, ChunkPos centerPos, Holder<Biome> biome, LevelHeightAccessor ha,
        PalettedContainerFactory factory, int minY, int height, long seed
    ) {
        Map<Long, ProtoChunk> scratch = new HashMap<>();
        WorldGenLevel[] self = new WorldGenLevel[1];
        InvocationHandler handler = (proxy, method, methodArgs) -> {
            String name = method.getName();
            Object[] a = methodArgs;
            switch (name) {
                case "getHeight":
                    if (a != null && a.length == 3) {
                        int hx = ((Number) a[1]).intValue() & 15;
                        int hz = ((Number) a[2]).intValue() & 15;
                        return center.getHeight((Heightmap.Types) a[0], hx, hz) + 1;
                    }
                    return height;
                case "getMinY":
                    return minY;
                case "getMaxY":
                    return minY + height - 1;
                case "getSectionsCount":
                    return height >> 4;
                case "getSectionIndex":
                    return (((Number) a[0]).intValue() - minY) >> 4;
                case "getMinSectionY":
                    return minY >> 4;
                case "getMaxSectionY":
                    return (minY + height - 1) >> 4;
                case "isOutsideBuildHeight": {
                    int y = a.length == 1 && a[0] instanceof Number
                        ? ((Number) a[0]).intValue() : ((BlockPos) a[0]).getY();
                    return y < minY || y >= minY + height;
                }
                case "getChunk": {
                    int cx, cz;
                    if (a[0] instanceof Number) { cx = ((Number) a[0]).intValue(); cz = ((Number) a[1]).intValue(); }
                    else { BlockPos bp = (BlockPos) a[0]; cx = bp.getX() >> 4; cz = bp.getZ() >> 4; }
                    if (cx == centerPos.x() && cz == centerPos.z()) return center;
                    long key = (((long) cx) << 32) ^ (cz & 0xffffffffL);
                    return scratch.computeIfAbsent(key,
                        k -> new ProtoChunk(new ChunkPos(cx, cz), UpgradeData.EMPTY, ha, factory, null));
                }
                case "getBiome":
                    return biome;
                case "ensureCanWrite":
                    return Boolean.TRUE;
                case "getSeed":
                    return seed;
                case "getLevel":
                    return self[0];
                case "registryAccess":
                    return null;
                case "getMinBuildHeight":
                    return minY;
                case "dimensionType":
                    return null;
                case "hashCode":
                    return System.identityHashCode(proxy);
                case "equals":
                    return proxy == a[0];
                case "toString":
                    return "FeatureOracleLevel";
                default: {
                    Class<?> rt = method.getReturnType();
                    if (rt == boolean.class) return Boolean.FALSE;
                    if (rt == int.class) return 0;
                    if (rt == long.class) return 0L;
                    if (rt == float.class) return 0f;
                    if (rt == double.class) return 0d;
                    if (rt.isPrimitive()) return 0;
                    return null;
                }
            }
        };
        WorldGenLevel lvl = (WorldGenLevel) Proxy.newProxyInstance(
            FeatureOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler);
        self[0] = lvl;
        return lvl;
    }
}
