// Independent JVM oracle for 26.3 biome decoration. Builds the 3x3 chunk neighbourhood the real
// pipeline hands the features step (fill + surface over a synthetic quart-resolution biome
// source, final heightmaps primed, no carvers), then runs the real feature sorter, placer and
// features over the centre chunk, one placed feature at a time, with the real decoration seed.
//
//   args: <seed> <settings> <layout-file> <types|*> <chunkMinY> <chunkHeight> <cx> <cz> [<cx> <cz> ...]
//   `layout-file` lists the possible biomes (one per line, its order is the biome source's) and
//   the same hash as surface-biomes.txt picks one per quart cell. `types` is a comma list of
//   feature type names (e.g. `ore,scattered_ore`) restricting which features run; `*` runs all,
//   and `!name` entries exclude placed features by registry name (without namespace).
//   `+probe:x:y:z` prints block facts around a position before any feature runs (debugging aid).
//   `+dump:name` prints a `d x y z state` line per cell that placed feature changed, and a
//   `t bits value` line per raw random draw it made.
// Per chunk:
//   chunk <settings> <seed> <cx> <cz>
//   f <step> <index> <placed name> draws <n> changed <m> hash <h>      (every executed feature)
//   final <chunkX> <chunkZ> <hash>                                     (the nine chunks, z then x)
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

public final class DecorationOracle263 {
    /** While set, every raw random draw is printed as `t <bits> <value>` (the `+dump` debugging aid). */
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
        BiomeResolver resolver;
        Region() { super(null, null, null, null); }
        @Override public int getMinY() { return minY; }
        @Override public int getHeight() { return height; }
        @Override public int getSeaLevel() { return seaLevel; }
        @Override public Holder<Biome> getUncachedNoiseBiome(int qx, int qy, int qz) { return resolver.getNoiseBiome(qx, qy, qz); }
        // A column the light engine has not registered yet (every column still being generated) reads
        // zero block light and full sky light; ticks scheduled by features change no block here.
        @Override public int getBrightness(LightLayer layer, BlockPos pos) { return layer == LightLayer.SKY ? 15 : 0; }
        @Override public int getRawBrightness(BlockPos pos, int skyDampen) { return Math.max(0, 15 - skyDampen); }
        @Override public void scheduleTick(BlockPos pos, net.minecraft.world.level.block.Block type, int delay, TickPriority priority) { }
        @Override public void scheduleTick(BlockPos pos, Fluid type, int delay, TickPriority priority) { }
        @Override public void scheduleTick(BlockPos pos, net.minecraft.world.level.block.Block type, int delay) { }
        @Override public void scheduleTick(BlockPos pos, Fluid type, int delay) { }
        @Override public boolean setBlock(BlockPos pos, BlockState state, int flags, int limit) {
            if (!ensureCanWrite(pos)) return false;
            var chunk = getChunk(pos);
            chunk.setBlockState(pos, state, flags);
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

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindTags();
        HolderLookup.Provider provider = loadWorldgenRegistries();
        long seed = Long.parseLong(args[0]);
        String name = args[1];
        String layout = args[2];
        // Entries: feature type names, `*` (every type), `!name` (exclude a placed feature by name).
        Set<String> types = new HashSet<>(Arrays.asList(args[3].split(",")));
        boolean allTypes = types.contains("*");
        // `+dump:name` prints every changed cell of that placed feature (debugging aid, not for fixtures).
        Set<String> dumps = new HashSet<>();
        for (String t : types) if (t.startsWith("+dump:")) dumps.add(t.substring(6));
        int chunkMinY = Integer.parseInt(args[4]);
        int chunkHeight = Integer.parseInt(args[5]);
        Holder<NoiseGeneratorSettings> settingsHolder = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(name)));
        NoiseGeneratorSettings settings = settingsHolder.value();
        RandomState rs = RandomState.create(provider.lookupOrThrow(Registries.NOISE), seed, settings);

        var biomeLookup = provider.lookupOrThrow(Registries.BIOME);
        List<Holder<Biome>> biomes = new ArrayList<>();
        for (String line : Files.readAllLines(Path.of("/oracle/" + layout)))
            if (!line.isBlank()) biomes.add(biomeLookup.getOrThrow(ResourceKey.create(Registries.BIOME, Identifier.withDefaultNamespace(line.trim()))));
        IdMapper<Holder<Biome>> idMap = new IdMapper<>();
        biomeLookup.listElements().forEach(idMap::add);
        Holder<Biome> plains = biomeLookup.getOrThrow(Biomes.PLAINS);
        PalettedContainerFactory pcf = new PalettedContainerFactory(
            Strategy.createForBlockStates(Block.BLOCK_STATE_REGISTRY), Blocks.AIR.defaultBlockState(), null,
            Strategy.createForBiomes(idMap), plains, null);

        long zoomSeed = BiomeManager.obfuscateSeed(seed);
        BiomeResolver resolver = (qx, qy, qz) -> biomes.get(pick(qx, qy, qz, biomes.size()));
        BiomeManager biomeManager = new BiomeManager(resolver, zoomSeed);

        BiomeSource source = new BiomeSource() {
            @Override protected com.mojang.serialization.MapCodec<? extends BiomeSource> codec() { return null; }
            @Override protected java.util.stream.Stream<Holder<Biome>> collectPossibleBiomes() { return biomes.stream(); }
            @Override public BiomeResolver createResolver(Climate.Sampler sampler) { return resolver; }
        };
        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(source, settingsHolder);
        Method doFill = NoiseBasedChunkGenerator.class.getDeclaredMethod("doFill", NoiseChunk.class, ChunkAccess.class);
        doFill.setAccessible(true);

        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus lava = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        Aquifer.FluidStatus sea = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker picker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lava : sea;

        List<FeatureSorter.StepFeatureData> featureList = FeatureSorter.buildFeaturesPerStep(
            List.copyOf(source.possibleBiomes()), b -> b.value().getGenerationSettings().features(), true);
        var placedLookup = provider.lookupOrThrow(Registries.PLACED_FEATURE);
        Map<PlacedFeature, String> placedNames = new IdentityHashMap<>();
        placedLookup.listElements().forEach(e -> placedNames.put(e.value(), e.key().identifier().toString()));

        sun.misc.Unsafe unsafe;
        { Field f = sun.misc.Unsafe.class.getDeclaredField("theUnsafe"); f.setAccessible(true); unsafe = (sun.misc.Unsafe) f.get(null); }
        ChunkStep featuresStep = ChunkPyramid.GENERATION_PYRAMID.getStepTo(ChunkStatus.FEATURES);

        for (int a = 6; a + 1 < args.length; a += 2) {
            int cx = Integer.parseInt(args[a]);
            int cz = Integer.parseInt(args[a + 1]);
            LevelHeightAccessor access = LevelHeightAccessor.create(chunkMinY, chunkHeight);
            ProtoChunk[][] chunks = new ProtoChunk[3][3];
            for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) {
                int px = cx + dx, pz = cz + dz;
                ProtoChunk chunk = new ProtoChunk(new ChunkPos(px, pz), UpgradeData.EMPTY, access, pcf, null);
                NoiseSettings ns = settings.noiseSettings().clampToHeightAccessor(access);
                DensityVolume volume = new DensityVolume(16, ns.height(), 16, px * 16, ns.minY(), pz * 16);
                try (NoiseChunk nc = new NoiseChunk(rs, null, settings, picker, Blender.empty(), volume)) {
                    doFill.invoke(generator, nc, chunk);
                    WorldGenerationContext context = new WorldGenerationContext(generator, access);
                    rs.surfaceSystem().buildSurface(rs, biomeManager, context, chunk, nc, settings.materialRule().value(), null);
                }
                chunk.fillBiomesFromNoise(resolver);
                Heightmap.primeHeightmaps(chunk, EnumSet.of(Heightmap.Types.MOTION_BLOCKING, Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
                    Heightmap.Types.OCEAN_FLOOR, Heightmap.Types.WORLD_SURFACE));
                chunk.setPersistedStatus(ChunkStatus.TERRAIN);
                chunks[dz + 1][dx + 1] = chunk;
            }
            final int fcx = cx, fcz = cz;
            StaticCache2D<ChunkAccess> cache = StaticCache2D.create(cx, cz, 1, (x, z) -> chunks[z - fcz + 1][x - fcx + 1]);
            Region region = (Region) unsafe.allocateInstance(Region.class);
            Class<?> wgr = WorldGenRegion.class;
            setField(region, wgr, "cache", cache);
            setField(region, wgr, "center", chunks[1][1]);
            setField(region, wgr, "seed", seed);
            setField(region, wgr, "random", RandomSource.create(seed));
            setField(region, wgr, "biomeManager", new BiomeManager(region, zoomSeed));
            setField(region, wgr, "generatingStep", featuresStep);
            setField(region, wgr, "centerChunkX", cx);
            setField(region, wgr, "centerChunkZ", cz);
            setField(region, wgr, "writeRadius", 1);
            region.minY = chunkMinY; region.height = chunkHeight; region.seaLevel = seaLevel;
            region.resolver = resolver;

            OUT.println("chunk " + name + " " + seed + " " + cx + " " + cz);
            for (String t : types) if (t.startsWith("+probe:")) {
                String[] c = t.substring(7).split(":");
                BlockPos pp = new BlockPos(Integer.parseInt(c[0]), Integer.parseInt(c[1]), Integer.parseInt(c[2]));
                BlockState tall = Blocks.TALL_SEAGRASS.defaultBlockState().setValue(net.minecraft.world.level.block.DoublePlantBlock.HALF, net.minecraft.world.level.block.state.properties.DoubleBlockHalf.LOWER);
                OUT.println("# probe at " + pp + " block " + fullKey(region.getBlockState(pp)) + " below " + fullKey(region.getBlockState(pp.below()))
                    + " above " + fullKey(region.getBlockState(pp.above())) + " tallCanSurvive " + tall.canSurvive(region, pp)
                    + " fluid " + region.getFluidState(pp) + " amount " + region.getFluidState(pp).getAmount());
            }
            WorldgenRandom random = new WorldgenRandom(new XoroshiroRandomSource(0L)) {
                @Override public int next(int bits) {
                    int v = super.next(bits);
                    if (traceDraws) OUT.println("t " + bits + " " + v);
                    return v;
                }
            };
            int originX = cx * 16, originZ = cz * 16;
            long decorationSeed = random.setDecorationSeed(seed, originX, originZ);
            Set<Holder<Biome>> possible = new LinkedHashSet<>();
            for (int dz = 0; dz < 3; dz++) for (int dx = 0; dx < 3; dx++)
                for (LevelChunkSection section : chunks[dz][dx].getSections()) section.getBiomes().getAll(possible::add);
            possible.retainAll(source.possibleBiomes());
            BlockPos origin = new BlockPos(originX, chunkMinY, originZ);
            FeaturePlacer placer = new FeaturePlacer(region, generator);

            BlockState[][] before = new BlockState[9][16 * 16 * chunkHeight];
            for (int dz = 0; dz < 3; dz++) for (int dx = 0; dx < 3; dx++) {
                ProtoChunk pc = chunks[dz][dx];
                BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
                for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) for (int y = 0; y < chunkHeight; y++)
                    before[dz * 3 + dx][(x + z * 16) * chunkHeight + y] = pc.getBlockState(p.set(pc.getPos().getMinBlockX() + x, chunkMinY + y, pc.getPos().getMinBlockZ() + z));
            }

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
                    String placedName = placedNames.getOrDefault(feature, "?").replace("minecraft:", "");
                    if (types.contains("!" + placedName)) continue;
                    if (!allTypes && !types.contains(typeOf(feature.feature().value()))) continue;
                    random.setFeatureSeed(decorationSeed, gi, stepIndex);
                    int c0 = random.getCount();
                    traceDraws = dumps.contains(placedName);
                    try {
                        placer.placeWithBiomeCheck(feature, random, origin);
                    } catch (Throwable t) {
                        OUT.println("# exception in " + placedNames.getOrDefault(feature, "?") + ": " + t);
                        for (StackTraceElement el : t.getStackTrace()) OUT.println("#   " + el);
                        OUT.flush();
                        throw t;
                    }
                    traceDraws = false;
                    int draws = random.getCount() - c0;
                    long h = 0xcbf29ce484222325L;
                    int changed = 0;
                    for (int dz = 0; dz < 3; dz++) for (int dx = 0; dx < 3; dx++) {
                        ProtoChunk pc = chunks[dz][dx];
                        BlockState[] prev = before[dz * 3 + dx];
                        BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
                        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) for (int y = 0; y < chunkHeight; y++) {
                            int wx = (cx + dx - 1) * 16 + x, wz = (cz + dz - 1) * 16 + z;
                            BlockState now = pc.getBlockState(p.set(wx, chunkMinY + y, wz));
                            int idx = (x + z * 16) * chunkHeight + y;
                            if (now != prev[idx]) {
                                prev[idx] = now;
                                changed++;
                                if (dumps.contains(placedName)) OUT.println("d " + wx + " " + (chunkMinY + y) + " " + wz + " " + fullKey(now));
                                h = mix(h, wx); h = mix(h, chunkMinY + y); h = mix(h, wz);
                                h = (h ^ keyHash(now)) * 0x100000001b3L;
                            }
                        }
                    }
                    OUT.println("f " + stepIndex + " " + gi + " " + placedNames.getOrDefault(feature, "?")
                        + " draws " + draws + " changed " + changed + " hash " + Long.toHexString(h));
                }
            }
            for (int dz = 0; dz < 3; dz++) for (int dx = 0; dx < 3; dx++) {
                ProtoChunk pc = chunks[dz][dx];
                long h = 0xcbf29ce484222325L;
                BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
                for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) for (int y = chunkMinY; y < chunkMinY + chunkHeight; y++)
                    h = (h ^ keyHash(pc.getBlockState(p.set((cx + dx - 1) * 16 + x, y, (cz + dz - 1) * 16 + z)))) * 0x100000001b3L;
                OUT.println("final " + (cx + dx - 1) + " " + (cz + dz - 1) + " " + Long.toHexString(h));
            }
            OUT.flush();
        }
        OUT.flush();
    }
}
