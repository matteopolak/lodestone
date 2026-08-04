// Isolated feature oracle: runs vanilla's own doFill + buildSurface + applyCarvers
// to obtain the real post-carve chunk for a 3x3 neighbourhood (centre chunk
// plus its 8 neighbours), then runs the UNDERGROUND_ORES decoration step (ore
// features only) for EACH of those 9 chunks — each with its OWN origin and
// OWN decorationSeed, exactly as vanilla's `ChunkGenerator.applyBiomeDecoration`
// does per chunk — and dumps every block that ANY of those 9 passes wrote
// inside the CENTRE 16x16 column, plus the decoration/feature seed parameters
// (centre's own) and a real `OCEAN_FLOOR_WG` heightmap spanning the whole 3x3
// region.
//
// This replaces an earlier, narrower version of this oracle that ran only the
// centre chunk's own ore features against an empty (all-air) neighbourhood and
// answered every `getHeight` probe by wrapping the requested column back into
// the centre chunk's own heightmap via `& 15`. That header used to say, quite
// honestly: "deliberately does NOT model ore spill from the 8 neighbouring
// chunks into the centre." This version models it — vanilla's real
// `blockStateWriteRadius(1)` at the FEATURES generation stage
// (`ChunkPyramid.java:32-35`) means a NEIGHBOUR chunk's own ore decoration
// (its own origin, its own seed) can legitimately spill blocks into the
// centre chunk, and a real neighbour heightmap (not a wraparound of centre's)
// is what a boundary-adjacent blob's "does this reach daylight" probe
// actually reads in vanilla.
//
// Scope (honest — see `docs/worldgen-parity.md`'s "known gap" section):
//   * WHICH CHUNKS PLACE ORES is exactly 3x3 (centre + 8 immediate
//     neighbours) — matching vanilla's `blockStateWriteRadius(1)`, which is
//     a real, enforced limit on how far a chunk's OWN decoration can
//     legitimately write, not an approximation on this oracle's part.
//   * READS during placement (heightmap probes, block-state/adjacency
//     checks) are NOT clamped to that 3x3 footprint — `getChunk`/`getHeight`
//     lazily generate (and memoise) whatever additional chunk a read
//     actually touches, bounded only by the ore blob's own geometry (at
//     most ~13 blocks beyond the chunk a candidate position falls in for the
//     largest `size=64` blobs), never by an artificial cap. This is
//     deliberate, not merely more-accurate-for-free: an EARLIER version of
//     this method clamped `getChunk`'s coordinate into the 3x3 footprint,
//     which aliases two distinct real chunk coordinates onto the SAME
//     memoised chunk whenever both clamp to the same edge — and vanilla's
//     own `BulkSectionAccess` (used by `OreFeature.doPlace`) does not know
//     about that aliasing, so it can try to acquire the same
//     `LevelChunkSection`'s (non-reentrant) semaphore twice within one
//     placement and deadlock the JVM forever. Measured, not hypothetical:
//     that clamp hung this method for 10+ minutes at ~0% CPU, `jstack`
//     showing the main thread parked in `ThreadingDetector.checkAndLock`
//     called from `OreFeature.doPlace`. The fixture's own `inrun.`/`ofh.`
//     dump remains bounded to the 3x3 region regardless (see below) — this
//     only affects what this oracle computes internally while deciding the
//     centre's own post-feature state, which is now MORE correct than a
//     clamped read would have been, not less.
//   * Every one of the 9 source chunks is decorated with the SAME (single,
//     `FixedBiomeSource`) biome's feature list — this oracle has no biome
//     variety at all, so there is no "union of neighbouring biomes' feature
//     lists" question to answer (see `ComposedChunkOracle.java`'s
//     `postfeatures` stage for the real-biome case, which resolves this
//     differently and documents its own narrower simplification).
//   * Earlier feature steps that precede ores (lakes/springs) are still not
//     modelled — both sides start from the same post-carve field and run
//     only `UNDERGROUND_ORES`.
//   * The DUMPED fixture (`inrun.`/`ofh.`, read by the Rust side) IS bounded
//     to the 3x3 region — a full unbounded terrain dump has no natural size
//     limit, so `crate::feature::OreInput::region_local` clamps on the Rust
//     side when reconstructing this computation from the fixture. That is a
//     real, understood, narrower gap than what this oracle itself computes —
//     see `docs/worldgen-parity.md`'s "known gap" section for the measured
//     scope of that specific residual.
//
// The WorldGenLevel the ore feature needs is supplied via a JDK dynamic proxy
// whose `getChunk`/`getHeight` route through a memoised, on-demand per-chunk
// generator (`chunkAt`) rather than a single fixed chunk plus throwaway
// scratch — so cross-chunk reads and writes during placement see REAL
// terrain everywhere they reach. No Mojang source is copied — this only
// drives compiled classes.
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

    // ---- Shared generation context (set once in main, read by chunkAt) ----
    static HolderLookup.Provider provider;
    static Holder<Biome> biomeHolder;
    static NoiseGeneratorSettings settings;
    static NoiseBasedChunkGenerator generator;
    static RandomState rs;
    static LevelHeightAccessor heightAccessor;
    static RegistryAccess paletteAccess;
    static PalettedContainerFactory factory;
    static Aquifer.FluidPicker fluidPicker;
    static int minY, height;
    static long seed;
    static int centerX, centerZ;

    // Memoised per-chunk-coordinate real ProtoChunks (post fillFromNoise +
    // buildSurface + applyCarvers), generated on demand for WHATEVER
    // coordinate a read actually touches (deliberately unclamped — see this
    // file's own header comment for why clamping here caused a JVM
    // deadlock). In practice this stays small: the 9 chunks in the driven
    // 3x3 neighbourhood, plus at most a handful more that a boundary-
    // adjacent large ore blob's own geometry reaches.
    static final Map<Long, ProtoChunk> chunkCache = new HashMap<>();

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

    // ---- Memoised real per-chunk generation (fill + surface + carve) ----

    static long chunkKey(int cx, int cz) {
        return (((long) cx) << 32) ^ (cz & 0xffffffffL);
    }

    static ProtoChunk chunkAt(int cx, int cz) {
        long k = chunkKey(cx, cz);
        ProtoChunk cached = chunkCache.get(k);
        if (cached != null) return cached;

        ChunkPos pos = new ChunkPos(cx, cz);
        ProtoChunk chunk = new ProtoChunk(pos, UpgradeData.EMPTY, heightAccessor, factory, null);
        NoiseChunk noiseChunk = chunk.getOrCreateNoiseChunk(c -> NoiseChunk.forChunk(
            c, rs, Beardifier.EMPTY, settings, fluidPicker, Blender.empty()));
        generator.fillFromNoise(Blender.empty(), rs, null, chunk).join();

        WorldGenerationContext wgctx = new WorldGenerationContext(generator, heightAccessor);
        BiomeManager biomeManager = new BiomeManager(
            (qx, qy, qz) -> biomeHolder, BiomeManager.obfuscateSeed(seed));
        generator.buildSurface(chunk, wgctx, rs, null, biomeManager, Blender.empty(), null);

        Aquifer aquifer = noiseChunk.aquifer();
        CarvingContext context = new CarvingContext(
            generator, paletteAccess, heightAccessor, noiseChunk, rs, settings.surfaceRule());
        CarvingMask mask = chunk.getOrCreateCarvingMask();
        java.util.function.Function<BlockPos, Holder<Biome>> biomeGetter = p -> biomeHolder;
        WorldgenRandom carveRandom = new WorldgenRandom(new LegacyRandomSource(RandomSupport.generateUniqueSeed()));
        for (int dx = -8; dx <= 8; dx++) {
            for (int dz = -8; dz <= 8; dz++) {
                ChunkPos sourcePos = new ChunkPos(pos.x() + dx, pos.z() + dz);
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

        chunkCache.put(k, chunk);
        return chunk;
    }


    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindBlockTags();
        provider = VanillaRegistries.createLookup();

        String rawArgs = System.getenv("ORACLE_ARGS");
        if (rawArgs == null) rawArgs = "";
        rawArgs = rawArgs.trim();
        String biomeName = "minecraft:plains";
        int chunkX = 0, chunkZ = 0;
        seed = 42L;
        if (!rawArgs.isBlank()) {
            String[] toks = rawArgs.split("\\s+");
            if (toks.length >= 1 && !toks[0].isBlank()) biomeName = toks[0].trim();
            if (toks.length >= 3) {
                chunkX = Integer.parseInt(toks[1].trim());
                chunkZ = Integer.parseInt(toks[2].trim());
            }
            if (toks.length >= 4) seed = Long.parseLong(toks[3].trim());
        }
        centerX = chunkX;
        centerZ = chunkZ;

        ResourceKey<Biome> biomeKey = ResourceKey.create(
            Registries.BIOME, net.minecraft.resources.Identifier.parse(biomeName));
        Holder.Reference<Biome> biomeHolderRef = provider.lookupOrThrow(Registries.BIOME).getOrThrow(biomeKey);
        biomeHolder = biomeHolderRef;

        Holder<NoiseGeneratorSettings> settingsHolder =
            provider.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        settings = settingsHolder.value();

        FixedBiomeSource biomeSource = new FixedBiomeSource(biomeHolder);
        generator = new NoiseBasedChunkGenerator(biomeSource, settingsHolder);
        rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);

        minY = -64;
        height = 384;
        heightAccessor = LevelHeightAccessor.create(minY, height);

        MappedRegistry<Biome> biomeReg = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        Registry.register(biomeReg, Biomes.PLAINS, provider.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS).value());
        biomeReg.freeze();
        paletteAccess = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(biomeReg));
        factory = PalettedContainerFactory.create(paletteAccess);

        Aquifer.FluidStatus lavaStatus = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus seaStatus = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        fluidPicker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lavaStatus : seaStatus;

        // ---- Warm up the real 3x3 neighbourhood (fill+surface+carve only,
        // no features yet) before taking any snapshot ----
        for (int dx = -1; dx <= 1; dx++) {
            for (int dz = -1; dz <= 1; dz++) {
                chunkAt(chunkX + dx, chunkZ + dz);
            }
        }

        ProtoChunk centre = chunkAt(chunkX, chunkZ);

        // ---- Post-carve, pre-feature snapshot over the whole 3x3 region,
        // run-length-encoded per column (vanilla terrain is mostly vertical
        // runs of one block; see docs/worldgen-parity.md's fixture-format
        // section for the measured compaction ratio this pattern gets). ----
        Map<Long, String> preSnapshot = new HashMap<>();
        BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
        for (int lx = -16; lx < 32; lx++) {
            for (int lz = -16; lz < 32; lz++) {
                int wx = chunkX * 16 + lx, wz = chunkZ * 16 + lz;
                int cx = Math.floorDiv(wx, 16), cz = Math.floorDiv(wz, 16);
                int llx = wx - cx * 16, llz = wz - cz * 16;
                ProtoChunk owner = chunkAt(cx, cz);
                java.util.List<Object[]> runs = new java.util.ArrayList<>();
                int y = minY;
                int end = minY + height;
                while (y < end) {
                    String s = canon(owner.getBlockState(p.set(cx * 16 + llx, y, cz * 16 + llz)));
                    int count = 1;
                    while (y + count < end
                        && canon(owner.getBlockState(p.set(cx * 16 + llx, y + count, cz * 16 + llz))).equals(s)) {
                        count++;
                    }
                    runs.add(new Object[]{y, count, s});
                    // Only the CENTRE 16x16 columns need a pre-feature lookup
                    // (used below to diff post-feature vs. pre-feature); the
                    // neighbour columns are only needed via the RLE dump
                    // below, which every (lx,lz) in this loop already emits.
                    if (lx >= 0 && lx < 16 && lz >= 0 && lz < 16) {
                        for (int dy = 0; dy < count; dy++) {
                            preSnapshot.put(snapKey(lx, y + dy, lz), s);
                        }
                    }
                    y += count;
                }
                boolean allAir = runs.size() == 1 && runs.get(0)[2].toString().split("\\[")[0].equals("minecraft:air");
                if (!allAir) {
                    for (Object[] run : runs) {
                        sb.append("inrun.").append(lx).append(',').append(lz).append(' ')
                          .append(run[0]).append(' ').append(run[1]).append(' ').append(run[2]).append('\n');
                    }
                }
            }
        }

        // ---- Real OCEAN_FLOOR_WG heightmap across the whole 3x3 region ----
        for (int lx = -16; lx < 32; lx++) {
            for (int lz = -16; lz < 32; lz++) {
                int wx = chunkX * 16 + lx, wz = chunkZ * 16 + lz;
                int cx = Math.floorDiv(wx, 16), cz = Math.floorDiv(wz, 16);
                int llx = wx - cx * 16, llz = wz - cz * 16;
                int h = chunkAt(cx, cz).getHeight(Heightmap.Types.OCEAN_FLOOR_WG, llx, llz) + 1;
                sb.append("ofh.").append(lx).append(',').append(lz).append(' ').append(h).append('\n');
            }
        }

        // ---- Feature list (fixed biome, so identical for all 9 sources) ----
        List<FeatureSorter.StepFeatureData> perStep = FeatureSorter.buildFeaturesPerStep(
            List.of(biomeHolder), b -> b.value().getGenerationSettings().features(), true);
        int STEP = GenerationStep.Decoration.UNDERGROUND_ORES.ordinal();

        HolderLookup.RegistryLookup<PlacedFeature> pfLookup = provider.lookupOrThrow(Registries.PLACED_FEATURE);
        Map<PlacedFeature, String> pfIds = new java.util.IdentityHashMap<>();
        pfLookup.listElements().forEach(ref -> pfIds.put(ref.value(), ref.key().identifier().toString()));

        FeatureSorter.StepFeatureData sfd = perStep.get(STEP);
        List<PlacedFeature> feats = sfd.features();

        WorldGenLevel level = makeLevel();

        // ---- The real 3x3 driver: each of the 9 source chunks gets its OWN
        // origin and OWN decorationSeed (vanilla's `applyBiomeDecoration` is
        // called once per chunk, using that chunk's own seed — the SPILL
        // into a neighbour is a side effect of `blockStateWriteRadius(1)`
        // letting the WRITE land there, not a shared seed). Loop order (dx
        // outer, dz inner, -1..=1) matches `applyCarvers`' own source-chunk
        // convention in this same file family (see chunkAt's carve loop and
        // ComposedChunkOracle.java) — a fixed, documented convention, not a
        // claim this matches real-world chunk *load* order (which vanilla
        // itself does not guarantee is deterministic at boundaries; see
        // docs/worldgen-parity.md). ----
        WorldgenRandom fRandom = new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
        long centreDecorationSeed = 0;
        int centreOreOrder = 0;
        for (int dx = -1; dx <= 1; dx++) {
            for (int dz = -1; dz <= 1; dz++) {
                ChunkPos sourcePos = new ChunkPos(chunkX + dx, chunkZ + dz);
                BlockPos origin = SectionPos.of(sourcePos, heightAccessor.getMinSectionY()).origin();
                long decorationSeed = fRandom.setDecorationSeed(seed, origin.getX(), origin.getZ());
                boolean isCentre = dx == 0 && dz == 0;
                if (isCentre) centreDecorationSeed = decorationSeed;

                int order = 0;
                for (int i = 0; i < feats.size(); i++) {
                    PlacedFeature pf = feats.get(i);
                    fRandom.setFeatureSeed(decorationSeed, i, STEP);
                    ConfiguredFeature<?, ?> cf = pf.feature().value();
                    if (cf.feature() instanceof OreFeature) {
                        if (isCentre) {
                            String pid = pfIds.getOrDefault(pf, "?");
                            sb.append("oredef.").append(order).append(' ').append(pid).append(' ').append(i).append('\n');
                            order++;
                        }
                        pf.placeWithBiomeCheck(level, generator, fRandom, origin);
                    }
                }
                if (isCentre) centreOreOrder = order;
            }
        }
        sb.append("meta.decorationSeed ").append(centreDecorationSeed).append('\n');
        sb.append("meta.originX ").append(chunkX * 16).append('\n');
        sb.append("meta.originZ ").append(chunkZ * 16).append('\n');
        sb.append("meta.step ").append(STEP).append('\n');
        sb.append("meta.oreFeatureCount ").append(centreOreOrder).append('\n');

        // ---- Diff: every centre-chunk block ANY of the 9 passes changed ----
        int changed = 0;
        Map<String, Integer> perBlock = new TreeMap<>();
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++) {
                    String c = canon(centre.getBlockState(p.set(chunkX * 16 + x, y, chunkZ * 16 + z)));
                    String before = preSnapshot.get(snapKey(x, y, z));
                    if (before == null || !before.equals(c)) {
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

    static long snapKey(int lx, int y, int lz) {
        // lx,lz in [-16,32), y in [minY, minY+height) — pack into one long.
        return (((long) (lx + 32)) << 40) ^ (((long) (lz + 32)) << 24) ^ (long) (y - minY);
    }

    // A minimal WorldGenLevel via dynamic proxy. `getChunk`/`getHeight` route
    // through the memoised, unclamped per-chunk generator (`chunkAt`)
    // instead of a single fixed chunk plus throwaway scratch, so cross-chunk
    // reads *and* writes during placement see real terrain everywhere they
    // reach — see this file's header comment for why the "clamped" version
    // deadlocked the JVM.
    static WorldGenLevel makeLevel() {
        WorldGenLevel[] self = new WorldGenLevel[1];
        InvocationHandler handler = (proxy, method, methodArgs) -> {
            String name = method.getName();
            Object[] a = methodArgs;
            switch (name) {
                case "getHeight":
                    if (a != null && a.length == 3) {
                        int hx = ((Number) a[1]).intValue();
                        int hz = ((Number) a[2]).intValue();
                        int cx = Math.floorDiv(hx, 16), cz = Math.floorDiv(hz, 16);
                        int lx = hx - cx * 16, lz = hz - cz * 16;
                        return chunkAt(cx, cz).getHeight((Heightmap.Types) a[0], lx, lz) + 1;
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
                    // No clamp here, deliberately: two different (cx,cz)
                    // both resolving to the SAME memoised chunk (aliasing)
                    // would let vanilla's own `BulkSectionAccess` try to
                    // acquire the same `LevelChunkSection`'s (non-reentrant)
                    // semaphore twice within one placement and deadlock the
                    // JVM forever — measured, not hypothetical: an earlier
                    // version of this method clamped here and hung for
                    // 10+ minutes at 0% CPU, `jstack` showing the main
                    // thread parked in `ThreadingDetector.checkAndLock`
                    // called from `OreFeature.doPlace`. `chunkAt` is a pure
                    // 1:1 memoised generator, so every distinct coordinate
                    // gets its own distinct chunk/section — the 3x3 SOURCE
                    // loop below is still exactly 3x3 (which chunks get to
                    // place ore features), this only affects how far a
                    // READ during placement (heightmap probes, block-state/
                    // adjacency checks) can reach, which is naturally
                    // bounded by the ore blob's own geometry, not by an
                    // artificial cap here.
                    return chunkAt(cx, cz);
                }
                case "getBiome":
                    return biomeHolder;
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
