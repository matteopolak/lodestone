// Independent JVM oracle for the 26.3 Overworld a server serves: for each target chunk it
// generates the 5x5 chunks around it with the real fill, surface rules and carvers over the real
// multi-noise biome source, then runs the real placed-feature decoration of the nine chunks
// around the target in the production source order (each decoration reading what the earlier ones
// left), and prints the target's final blocks.
//
//   args: <seed> <cx> <cz> [<cx> <cz> ...] [dump]  (dump lists every non-air block of each target and of the target after each source; trace, with the first source, hashes its centre chunk after every placed feature and logs the writes of one feature; order lists the placed features per step)
// Per target:
//   target <seed> <cx> <cz>
//   sec <k> <hash>      (each 16-row section from the bottom: rows y, z, x of the target)
//   full <hash>         (every row of the target)
// The hash folds the fnv of each block state's full key (name[k=v,...], properties sorted).
import java.lang.reflect.*;
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.tags.TagKey;
import net.minecraft.tags.TagLoader;
import net.minecraft.network.chat.Component;
import net.minecraft.server.packs.*;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.level.WorldGenRegion;
import net.minecraft.util.RandomSource;
import net.minecraft.util.StaticCache2D;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LightLayer;
import net.minecraft.world.level.material.Fluid;
import net.minecraft.world.ticks.TickPriority;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkPyramid;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.chunk.status.ChunkStep;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.feature.Feature;
import net.minecraft.world.level.levelgen.placement.*;

public final class WorldOracle263 {
    static boolean traceDraws;
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false);

    static String fullKey(BlockState s) {
        StringBuilder sb = new StringBuilder(BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString());
        List<Property<?>> props = new ArrayList<>(s.getProperties());
        props.sort(Comparator.comparing(Property::getName));
        if (!props.isEmpty()) {
            sb.append('[');
            for (int i = 0; i < props.size(); i++) {
                if (i > 0) sb.append(',');
                sb.append(props.get(i).getName()).append('=').append(valueName(s, props.get(i)));
            }
            sb.append(']');
        }
        return sb.toString();
    }

    @SuppressWarnings({"unchecked", "rawtypes"})
    static String valueName(BlockState s, Property p) { return p.getName(s.getValue(p)); }

    static long fnv(String s) {
        long h = 0xcbf29ce484222325L;
        for (byte b : s.getBytes(java.nio.charset.StandardCharsets.UTF_8)) { h ^= b & 0xFF; h *= 0x100000001b3L; }
        return h;
    }

    static int pick(int qx, int qy, int qz, int n) {
        int h = (qx >> 1) * 73856093 ^ (qy >> 3) * 19349663 ^ (qz >> 1) * 83492791;
        h ^= h >>> 13;
        h *= 0x5bd1e995;
        h ^= h >>> 15;
        return Math.floorMod(h, n);
    }

    static final Map<BlockState, Long> KEY_HASH = new IdentityHashMap<>();

    static long keyHash(BlockState s) {
        Long k = KEY_HASH.get(s);
        if (k == null) { k = fnv(fullKey(s)); KEY_HASH.put(s, k); }
        return k;
    }

    /** The real region's reads over the 3x3 neighbourhood, with the server-level dependencies it
     *  does not need during features replaced. Allocated without running the constructor. */
    static final class Region extends WorldGenRegion {
        int minY, height, seaLevel;
        int skyLight = 15;
        BiomeResolver resolver;
        net.minecraft.server.level.ServerLevel fake;
        net.minecraft.core.RegistryAccess access;
        // A decorator that places a configured feature itself asks the region for the registries
        // and the level's generator; the fake level answers just those two questions.
        @Override public net.minecraft.server.level.ServerLevel getLevel() { return fake; }
        @Override public net.minecraft.core.RegistryAccess registryAccess() { return access; }
        Region() { super(null, null, null, null); }
        static boolean LOG;
        @Override public int getMinY() { return minY; }
        @Override public int getHeight() { return height; }
        @Override public int getSeaLevel() { return seaLevel; }
        @Override public Holder<Biome> getUncachedNoiseBiome(int qx, int qy, int qz) { return resolver.getNoiseBiome(qx, qy, qz); }
        // A column the light engine has not registered yet (every column still being generated) reads
        // zero block light and full sky light; ticks scheduled by features change no block here.
        @Override public int getBrightness(LightLayer layer, BlockPos pos) { return layer == LightLayer.SKY ? skyLight : 0; }
        @Override public int getRawBrightness(BlockPos pos, int skyDampen) { return Math.max(0, skyLight - skyDampen); }
        @Override public void scheduleTick(BlockPos pos, net.minecraft.world.level.block.Block type, int delay, TickPriority priority) { }
        @Override public void scheduleTick(BlockPos pos, Fluid type, int delay, TickPriority priority) { }
        @Override public void scheduleTick(BlockPos pos, net.minecraft.world.level.block.Block type, int delay) { }
        @Override public void scheduleTick(BlockPos pos, Fluid type, int delay) { }
        @Override public boolean setBlock(BlockPos pos, BlockState state, int flags, int limit) {
            if (!ensureCanWrite(pos)) return false;
            var chunk = getChunk(pos);
            chunk.setBlockState(pos, state, flags);
            if (LOG) OUT.println("w " + pos.getX() + " " + pos.getY() + " " + pos.getZ() + " " + fullKey(state));
            if (state.hasBlockEntity()) {
                // A chunk still being generated records a placeholder, which the region turns into a
                // fresh block entity on first access (the hive decorator then stores its bees).
                net.minecraft.nbt.CompoundTag tag = new net.minecraft.nbt.CompoundTag();
                tag.putInt("x", pos.getX());
                tag.putInt("y", pos.getY());
                tag.putInt("z", pos.getZ());
                tag.putString("id", "DUMMY");
                chunk.setBlockEntityNbt(tag);
            }
            return true;
        }
    }

    static net.minecraft.server.level.ServerLevel fakeLevel(sun.misc.Unsafe unsafe, net.minecraft.core.RegistryAccess access,
                                                            net.minecraft.world.level.chunk.ChunkGenerator generator) throws Exception {
        var level = (net.minecraft.server.level.ServerLevel) unsafe.allocateInstance(net.minecraft.server.level.ServerLevel.class);
        var cache = (net.minecraft.server.level.ServerChunkCache) unsafe.allocateInstance(net.minecraft.server.level.ServerChunkCache.class);
        var map = (net.minecraft.server.level.ChunkMap) unsafe.allocateInstance(net.minecraft.server.level.ChunkMap.class);
        setField(map, net.minecraft.server.level.ChunkMap.class, "worldGenContext",
            new net.minecraft.world.level.chunk.status.WorldGenContext(null, generator, null, null, null, null));
        // A new entity draws its id from the level, which asks the chunk map whether it is taken.
        setField(map, net.minecraft.server.level.ChunkMap.class, "entityMap", new it.unimi.dsi.fastutil.ints.Int2ObjectOpenHashMap<>());
        setField(cache, net.minecraft.server.level.ServerChunkCache.class, "chunkMap", map);
        setField(level, net.minecraft.server.level.ServerLevel.class, "chunkSource", cache);
        setField(level, net.minecraft.world.level.Level.class, "registryAccess", access);
        // Template features read their structure from the server's template manager, served here
        // from the jar's own data documents.
        PackLocationInfo info = new PackLocationInfo("builtin", Component.literal("builtin"), PackSource.BUILT_IN, Optional.empty());
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(new PathPackResources(info, Path.of("/mc/src"))));
        var tm = (net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager)
            unsafe.allocateInstance(net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager.class);
        var tmClass = net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager.class;
        var source = new net.minecraft.world.level.levelgen.structure.templatesystem.loader.ResourceManagerTemplateSource(
            net.minecraft.util.datafix.DataFixers.getDataFixer(), BuiltInRegistries.BLOCK, manager,
            new net.minecraft.resources.FileToIdConverter("structure", ".nbt"));
        setField(tm, tmClass, "structureRepository", new java.util.concurrent.ConcurrentHashMap<>());
        setField(tm, tmClass, "resourceManagerSource", source);
        setField(tm, tmClass, "sources", List.of(source));
        var server = (net.minecraft.server.MinecraftServer) unsafe.allocateInstance(net.minecraft.server.dedicated.DedicatedServer.class);
        setField(server, net.minecraft.server.MinecraftServer.class, "structureTemplateManager", tm);
        // End spikes create their crystal through the entity factory, which asks the world data
        // for the enabled feature flags; nothing else of the world data is read.
        var worldData = java.lang.reflect.Proxy.newProxyInstance(
            net.minecraft.world.level.storage.WorldData.class.getClassLoader(),
            new Class<?>[] { net.minecraft.world.level.storage.WorldData.class },
            (proxy, method, args) -> switch (method.getName()) {
                case "enabledFeatures" -> net.minecraft.world.flag.FeatureFlags.DEFAULT_FLAGS;
                case "getDataConfiguration" -> net.minecraft.world.level.WorldDataConfiguration.DEFAULT;
                default -> throw new UnsupportedOperationException("fake world data: " + method.getName());
            });
        setField(server, net.minecraft.server.MinecraftServer.class, "worldData", worldData);
        setField(level, net.minecraft.server.level.ServerLevel.class, "server", server);
        return level;
    }

    static void setField(Object o, Class<?> c, String name, Object v) throws Exception {
        Field f = c.getDeclaredField(name);
        f.setAccessible(true);
        f.set(o, v);
    }

    static long mix(long h, int v) {
        for (int k = 0; k < 4; k++) { h ^= (v >>> (8 * k)) & 0xFF; h *= 0x100000001b3L; }
        return h;
    }

    static String typeOf(Feature f) {
        var key = BuiltInRegistries.FEATURE_TYPE.getKey(f.codec());
        return key == null ? "?" : key.getPath();
    }

    /** The vanilla block tags, from the jar's data, bound on the built-in block registry. */
    @SuppressWarnings("unchecked")
    static void bindTags() {
        PackLocationInfo info = new PackLocationInfo("builtin", Component.literal("builtin"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(info, Path.of("/mc/src"));
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(pack));
        Map<TagKey<Block>, List<Holder<Block>>> tags = TagLoader.loadTagsForRegistry(manager, Registries.BLOCK,
            (TagLoader.ElementLookup<Holder<Block>>) TagLoader.ElementLookup.fromFrozenRegistry(BuiltInRegistries.BLOCK));
        Registry.PendingTags<Block> pending = BuiltInRegistries.BLOCK.prepareTagReload(new TagLoader.LoadResult<>(Registries.BLOCK, tags));
        pending.apply();
        Map<TagKey<net.minecraft.world.level.material.Fluid>, List<Holder<net.minecraft.world.level.material.Fluid>>> fluidTags =
            TagLoader.loadTagsForRegistry(manager, Registries.FLUID,
                (TagLoader.ElementLookup<Holder<net.minecraft.world.level.material.Fluid>>) TagLoader.ElementLookup.fromFrozenRegistry(BuiltInRegistries.FLUID));
        BuiltInRegistries.FLUID.prepareTagReload(new TagLoader.LoadResult<>(Registries.FLUID, fluidTags)).apply();
    }

    /** The worldgen registries read from the jar's own data documents, as a running server reads
     *  them (the code-built registries differ from the documents wherever a codec does not
     *  round-trip). */
    static HolderLookup.Provider loadWorldgenRegistries() {
        PackLocationInfo info = new PackLocationInfo("builtin", Component.literal("builtin"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(info, Path.of("/mc/src"));
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(pack));
        var staticAccess = net.minecraft.core.RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY);
        List<HolderLookup.RegistryLookup<?>> context = new ArrayList<>();
        staticAccess.listRegistries().forEach(r -> context.add(r));
        return net.minecraft.resources.RegistryDataLoader.load(manager, context, net.minecraft.resources.RegistryDataLoader.WORLD_REGISTRIES.stream().filter(d -> d.key().identifier().getPath().startsWith("worldgen/") && !Set.of("worldgen/world_preset", "worldgen/flat_level_generator_preset", "worldgen/multi_noise_biome_source_parameter_list").contains(d.key().identifier().getPath())).toList(), Runnable::run).join();
    }


    // The source order the production Overworld applies a target's nine decorations in.
    static final int[][] OFFSETS = {{-1, -1}, {-1, 0}, {-1, 1}, {0, -1}, {0, 0}, {0, 1}, {1, -1}, {1, 0}, {1, 1}};

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindTags();
        HolderLookup.Provider provider = loadWorldgenRegistries();
        // An optional leading dimension (`nether` or `end`) picks its noise settings, biome source,
        // build range and sky light; the default is the Overworld.
        final String dimension = args[0].equals("nether") || args[0].equals("end") ? args[0] : "overworld";
        if (!dimension.equals("overworld")) args = Arrays.copyOfRange(args, 1, args.length);
        long seed = Long.parseLong(args[0]);
        final int minY = dimension.equals("overworld") ? -64 : 0, height = dimension.equals("overworld") ? 384 : 256;
        Holder<NoiseGeneratorSettings> settingsHolder = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(dimension)));
        NoiseGeneratorSettings settings = settingsHolder.value();
        RandomState rs = RandomState.create(provider.lookupOrThrow(Registries.NOISE), seed, settings);

        var biomeLookup = provider.lookupOrThrow(Registries.BIOME);
        IdMapper<Holder<Biome>> idMap = new IdMapper<>();
        biomeLookup.listElements().forEach(idMap::add);
        Holder<Biome> plains = biomeLookup.getOrThrow(Biomes.PLAINS);
        PalettedContainerFactory pcf = new PalettedContainerFactory(
            Strategy.createForBlockStates(Block.BLOCK_STATE_REGISTRY), Blocks.AIR.defaultBlockState(), null,
            Strategy.createForBiomes(idMap), plains, null);
        BiomeSource source;
        if (dimension.equals("end")) {
            source = net.minecraft.world.level.biome.TheEndBiomeSource.create(biomeLookup);
        } else {
            var preset = MultiNoiseBiomeSourceParameterList.knownPresets().entrySet().stream()
                .filter(e -> e.getKey().id().getPath().equals(dimension)).findFirst().orElseThrow().getValue();
            List<com.mojang.datafixers.util.Pair<Climate.ParameterPoint, Holder<Biome>>> rows = new ArrayList<>();
            for (var p : preset.values()) rows.add(com.mojang.datafixers.util.Pair.of(p.getFirst(), biomeLookup.getOrThrow(p.getSecond())));
            source = MultiNoiseBiomeSource.createFromList(new Climate.ParameterList<>(rows));
        }
        BiomeResolver resolver = source.createUncachedResolver(rs);
        long zoomSeed = BiomeManager.obfuscateSeed(seed);
        BiomeManager biomeManager = new BiomeManager(resolver, zoomSeed);

        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(source, settingsHolder);
        Method doFill = NoiseBasedChunkGenerator.class.getDeclaredMethod("doFill", NoiseChunk.class, ChunkAccess.class);
        doFill.setAccessible(true);
        Method genCarvers = null;
        for (Method m : NoiseBasedChunkGenerator.class.getDeclaredMethods()) if (m.getName().equals("generateCarvers")) genCarvers = m;
        genCarvers.setAccessible(true);

        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus lava = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        Aquifer.FluidStatus sea = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker picker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lava : sea;

        List<FeatureSorter.StepFeatureData> featureList = FeatureSorter.buildFeaturesPerStep(
            List.copyOf(source.possibleBiomes()), b -> b.value().getGenerationSettings().features(), true);

        if (args[args.length - 1].equals("order")) {
            var placedReg = ((net.minecraft.core.RegistryAccess) provider).lookupOrThrow(net.minecraft.core.registries.Registries.PLACED_FEATURE);
            for (int st = 0; st < featureList.size(); st++) {
                var fs = featureList.get(st).features();
                for (int i = 0; i < fs.size(); i++) OUT.println("order " + st + " " + i + " " + placedReg.getKey(fs.get(i)));
            }
            OUT.flush();
            return;
        }
        sun.misc.Unsafe unsafe;
        { Field f = sun.misc.Unsafe.class.getDeclaredField("theUnsafe"); f.setAccessible(true); unsafe = (sun.misc.Unsafe) f.get(null); }
        ChunkStep featuresStep = ChunkPyramid.GENERATION_PYRAMID.getStepTo(ChunkStatus.FEATURES);
        LevelHeightAccessor access = LevelHeightAccessor.create(minY, height);

        for (int a = 1; a + 1 < args.length; a += 2) {
            final int cx = Integer.parseInt(args[a]);
            final int cz = Integer.parseInt(args[a + 1]);
            ProtoChunk[][] chunks = new ProtoChunk[5][5];
            for (int dz = -2; dz <= 2; dz++) for (int dx = -2; dx <= 2; dx++) {
                int px = cx + dx, pz = cz + dz;
                ProtoChunk chunk = new ProtoChunk(new ChunkPos(px, pz), UpgradeData.EMPTY, access, pcf, null);
                NoiseSettings ns = settings.noiseSettings().clampToHeightAccessor(access);
                DensityVolume volume = new DensityVolume(16, ns.height(), 16, px * 16, ns.minY(), pz * 16);
                try (NoiseChunk nc = new NoiseChunk(rs, null, settings, picker, Blender.empty(), volume)) {
                    doFill.invoke(generator, nc, chunk);
                    WorldGenerationContext context = new WorldGenerationContext(generator, access);
                    rs.surfaceSystem().buildSurface(rs, biomeManager, context, chunk, nc, settings.materialRule().value(), null);
                    chunk.fillBiomesFromNoise(resolver);
                    genCarvers.invoke(generator, chunk, Blender.empty(), nc, rs, biomeManager, null, settings.materialRule().value());
                }
                Heightmap.primeHeightmaps(chunk, EnumSet.of(Heightmap.Types.MOTION_BLOCKING, Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
                    Heightmap.Types.OCEAN_FLOOR, Heightmap.Types.WORLD_SURFACE));
                chunk.setPersistedStatus(ChunkStatus.TERRAIN);
                chunks[dz + 2][dx + 2] = chunk;
            }
            for (int[] off : OFFSETS) {
                final int sx = cx + off[0], sz = cz + off[1];
                ProtoChunk[][] window = new ProtoChunk[3][3];
                for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++)
                    window[dz + 1][dx + 1] = chunks[sz + dz - cz + 2][sx + dx - cx + 2];
                StaticCache2D<ChunkAccess> cache = StaticCache2D.create(sx, sz, 1, (x, z) -> window[z - sz + 1][x - sx + 1]);
                Region region = (Region) unsafe.allocateInstance(Region.class);
                Class<?> wgr = WorldGenRegion.class;
                setField(region, wgr, "cache", cache);
                setField(region, wgr, "center", window[1][1]);
                setField(region, wgr, "seed", seed);
                setField(region, wgr, "random", RandomSource.create(seed));
                setField(region, wgr, "biomeManager", new BiomeManager(region, zoomSeed));
                setField(region, wgr, "generatingStep", featuresStep);
                setField(region, wgr, "centerChunkX", sx);
                setField(region, wgr, "centerChunkZ", sz);
                setField(region, wgr, "writeRadius", 1);
                region.minY = minY; region.height = height; region.seaLevel = seaLevel;
                region.skyLight = dimension.equals("nether") ? 0 : 15;
                region.resolver = resolver;
                region.access = (net.minecraft.core.RegistryAccess) provider;
                region.fake = fakeLevel(unsafe, region.access, generator);

                WorldgenRandom random = new WorldgenRandom(new XoroshiroRandomSource(0L));
                int originX = sx * 16, originZ = sz * 16;
                long decorationSeed = random.setDecorationSeed(seed, originX, originZ);
                Set<Holder<Biome>> possible = new LinkedHashSet<>();
                for (int dz = 0; dz < 3; dz++) for (int dx = 0; dx < 3; dx++)
                    for (LevelChunkSection section : window[dz][dx].getSections()) section.getBiomes().getAll(possible::add);
                possible.retainAll(source.possibleBiomes());
                BlockPos origin = new BlockPos(originX, minY, originZ);
                FeaturePlacer placer = new FeaturePlacer(region, generator);
                for (int stepIndex = 0; stepIndex < featureList.size(); stepIndex++) {
                    it.unimi.dsi.fastutil.ints.IntSet indices = new it.unimi.dsi.fastutil.ints.IntArraySet();
                    FeatureSorter.StepFeatureData data = featureList.get(stepIndex);
                    for (Holder<Biome> biome : possible) {
                        var inBiome = generator.getBiomeGenerationSettings(biome).features();
                        if (stepIndex < inBiome.size())
                            inBiome.get(stepIndex).stream().map(Holder::value).forEach(f -> indices.add(data.indexMapping().applyAsInt(f)));
                    }
                    int[] arr = indices.toIntArray();
                    Arrays.sort(arr);
                    for (int gi : arr) {
                        PlacedFeature feature = data.features().get(gi);
                        random.setFeatureSeed(decorationSeed, gi, stepIndex);
                        Region.LOG = args[args.length - 1].equals("trace") && off[0] == -1 && off[1] == -1 && stepIndex == 9 && gi == 20;
                        placer.placeWithBiomeCheck(feature, random, origin);
                        Region.LOG = false;
                        if (args[args.length - 1].equals("trace") && off[0] == -1 && off[1] == -1) {
                            long th = 0xcbf29ce484222325L;
                            BlockPos.MutableBlockPos tq = new BlockPos.MutableBlockPos();
                            for (int y = minY; y < minY + height; y++)
                                for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                                    th = (th ^ keyHash(window[1][1].getBlockState(tq.set(sx * 16 + x, y, sz * 16 + z)))) * 0x100000001b3L;
                            OUT.println("trace " + stepIndex + " " + gi + " " + Long.toHexString(th));
                        }
                    }
                }
                if (args[args.length - 1].equals("dump")) {
                    BlockPos.MutableBlockPos q = new BlockPos.MutableBlockPos();
                    for (int y = minY; y < minY + height; y++)
                        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                            BlockState bs = chunks[2][2].getBlockState(q.set(cx * 16 + x, y, cz * 16 + z));
                            if (!bs.isAir()) OUT.println("s" + off[0] + "," + off[1] + " " + x + " " + y + " " + z + " " + fullKey(bs));
                        }
                    for (int y = 60; y < 110; y++)
                        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                            BlockState bs = chunks[1][2].getBlockState(q.set(cx * 16 + x, y, (cz - 1) * 16 + z));
                            if (!bs.isAir()) OUT.println("n" + off[0] + "," + off[1] + " " + x + " " + y + " " + z + " " + fullKey(bs));
                        }
                    for (int y = 60; y < 110; y++)
                        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                            BlockState bs = chunks[1][1].getBlockState(q.set((cx - 1) * 16 + x, y, (cz - 1) * 16 + z));
                            if (!bs.isAir()) OUT.println("m" + off[0] + "," + off[1] + " " + x + " " + y + " " + z + " " + fullKey(bs));
                        }
                }
            }
            ProtoChunk target = chunks[2][2];
            OUT.println("target " + seed + " " + cx + " " + cz);
            long full = 0xcbf29ce484222325L;
            BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
            for (int sec = 0; sec < height / 16; sec++) {
                long h = 0xcbf29ce484222325L;
                for (int y = minY + sec * 16; y < minY + sec * 16 + 16; y++)
                    for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                        long k = keyHash(target.getBlockState(p.set(cx * 16 + x, y, cz * 16 + z)));
                        h = (h ^ k) * 0x100000001b3L;
                        full = (full ^ k) * 0x100000001b3L;
                    }
                OUT.println("sec " + sec + " " + Long.toHexString(h));
            }
            OUT.println("full " + Long.toHexString(full));
            if (args[args.length - 1].equals("dump")) {
                for (int y = minY; y < minY + height; y++)
                    for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                        BlockState bs = target.getBlockState(p.set(cx * 16 + x, y, cz * 16 + z));
                        if (!bs.isAir()) OUT.println("blk " + x + " " + y + " " + z + " " + fullKey(bs));
                    }
            }
            OUT.flush();
        }
        OUT.flush();
    }
}
