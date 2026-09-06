// Isolated vegetation oracle (issue #406's evidence gap, closed): runs
// vanilla's own doFill + buildSurface + applyCarvers to obtain the real
// post-carve chunk for a 3x3 neighbourhood (centre chunk plus its 8
// neighbours), replays the real UNDERGROUND_ORES step over all 9 sources
// (so the vegetal-decoration pass this oracle actually measures starts from
// the same post-ore terrain `OverworldGenerator::vegetation_stage` does on
// the Rust side, not a pre-ore approximation), then runs the
// VEGETAL_DECORATION step TWICE from that identical post-ore baseline:
//
//   * SINGLE  — only the centre chunk's own decoration pass (its own origin,
//     its own decorationSeed). This is the scope
//     `crate::feature::vegetation` actually implements today: single-chunk
//     only, no cross-chunk feature spill (see that module's own "Scope"
//     doc section).
//   * FULL3X3 — all 9 chunks in the driven neighbourhood, each with its own
//     origin/seed, writing into one shared world — vanilla's real
//     `blockStateWriteRadius(1)` at the FEATURES generation stage
//     (`ChunkPyramid.java:32-35`), the same limit `docs/worldgen-parity.md`
//     already documents for the ore 3x3 driver, applied here to trees/
//     grass/flowers instead.
//
// Both passes dump every changed cell over the WHOLE driven -16..32 region
// (not just the centre 16x16), so the Rust side can compute not only "does
// the centre's own pass match SINGLE" (validates the engine) but also
// "how much does centre's real vanilla content (FULL3X3, restricted to the
// centre 16x16) differ from SINGLE" (measures the cross-chunk-spill gap
// this engine's own module doc names but had never measured against a real
// vanilla dump).
//
// Scope, named plainly (the same discipline `FeatureOracle.java`'s header
// applies to the ore case):
//   * Every one of the 9 source chunks decorates with the SAME (single,
//     `FixedBiomeSource`) biome's feature list — no biome variety, matching
//     every other isolated (non-`ComposedChunkOracle`) oracle in this
//     directory.
//   * READS during placement (heightmap probes, block/tag/would-survive
//     checks) are NOT clamped to the 3x3 footprint — `getChunk`/`getHeight`
//     lazily generate (and memoise) whatever additional chunk a read
//     actually touches, the same unclamped design `FeatureOracle.java`
//     uses and the same reason: clamping caused a real JVM deadlock there
//     (see that file's header). Canopies here are small (2-3 blocks for
//     every species this engine implements), so this practically never
//     reaches beyond the driven 3x3 anyway, but the mechanism is identical
//     on purpose — one proven pattern, not two.
//   * Earlier decoration steps this session's scope doesn't reach
//     (LAKES, LOCAL_MODIFICATIONS, UNDERGROUND_STRUCTURES, SURFACE_STRUCTURES,
//     STRONGHOLDS, UNDERGROUND_DECORATION, FLUID_SPRINGS) are NOT run —
//     both the Rust engine and this oracle start from the real post-carve
//     field plus the real `UNDERGROUND_ORES` step only, matching
//     `OverworldGenerator::vegetation_stage`'s own documented input
//     ("the centre chunk's own post-ore terrain").
//   * SINGLE mode's *reads* still see the real, unclamped neighbourhood
//     (this oracle has no reason to approximate that) — this is NOT the
//     same approximation `crate::feature::vegetation::VegGrid` makes on the
//     Rust side (which clamps every read to the local chunk, per that
//     module's own "Approximations, named" section). That is a real,
//     narrower discrepancy between SINGLE and the Rust engine's own
//     behaviour beyond pure placement-footprint — named here rather than
//     assumed away; it practically only matters for a read within a few
//     blocks of the chunk edge.
//
// The WorldGenLevel is supplied via the same JDK dynamic proxy pattern
// `FeatureOracle.java` already proved (memoised, on-demand, unclamped
// `chunkAt`). No Mojang source is copied — this only drives compiled
// classes.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
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
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import com.mojang.serialization.Lifecycle;

public final class VegetationOracle {
    static final StringBuilder sb = new StringBuilder();

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
    static int chunkX, chunkZ;
    static String biomeName;

    static Map<Long, ProtoChunk> chunkCache = new HashMap<>();
    // The set is emitted with each fixture.  A production oracle must make its
    // level surface auditable: a missing method cannot be mistaken for a real
    // negative predicate result.
    static final Set<String> proxyCalls = java.util.Collections.synchronizedSet(new TreeSet<String>());
    static final Set<String> proxyFallbacks = java.util.Collections.synchronizedSet(new TreeSet<String>());

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

    static long chunkKey(int cx, int cz) {
        return (((long) cx) << 32) ^ (cz & 0xffffffffL);
    }

    // Same memoised, unclamped, on-demand per-chunk generator FeatureOracle.java
    // proved (fill + surface + carve only — features are applied separately,
    // by runStep below, directly mutating the ProtoChunk this returns).
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

    static WorldGenLevel level;

    /// Runs every feature in decoration step `stepOrdinal`, for every one of
    /// the 9 chunks in `chunkX/chunkZ ± 1` if `full3x3`, else only the centre
    /// — each with its OWN origin and OWN decorationSeed, matching vanilla's
    /// real per-chunk `applyBiomeDecoration`. No feature-type filter (unlike
    /// `FeatureOracle.java`'s `instanceof OreFeature` gate — that one exists
    /// only to label its own ore-specific fixture output; this method places
    /// literally everything a step's feature list names, matching real
    /// vanilla, since it also has to run UNDERGROUND_ORES faithfully as prep
    /// before the vegetal-decoration measurement even starts).
    static void runStep(int stepOrdinal, boolean full3x3) {
        List<FeatureSorter.StepFeatureData> perStep = FeatureSorter.buildFeaturesPerStep(
            List.of(biomeHolder), b -> b.value().getGenerationSettings().features(), true);
        List<PlacedFeature> feats = perStep.get(stepOrdinal).features();
        if (System.getenv("VEG_ORACLE_DEBUG") != null) {
            System.err.println("[debug] step=" + stepOrdinal + " feats.size()=" + feats.size());
        }
        WorldgenRandom fRandom = new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
        int loDx = full3x3 ? -1 : 0, hiDx = full3x3 ? 1 : 0;
        int loDz = full3x3 ? -1 : 0, hiDz = full3x3 ? 1 : 0;
        int placedCount = 0;
        for (int dx = loDx; dx <= hiDx; dx++) {
            for (int dz = loDz; dz <= hiDz; dz++) {
                ChunkPos sourcePos = new ChunkPos(chunkX + dx, chunkZ + dz);
                BlockPos origin = SectionPos.of(sourcePos, heightAccessor.getMinSectionY()).origin();
                long decorationSeed = fRandom.setDecorationSeed(seed, origin.getX(), origin.getZ());
                for (int i = 0; i < feats.size(); i++) {
                    PlacedFeature pf = feats.get(i);
                    fRandom.setFeatureSeed(decorationSeed, i, stepOrdinal);
                    boolean placed = pf.placeWithBiomeCheck(level, generator, fRandom, origin);
                    if (placed) placedCount++;
                }
            }
        }
        if (System.getenv("VEG_ORACLE_DEBUG") != null) {
            System.err.println("[debug] step=" + stepOrdinal + " placedCount(true)=" + placedCount);
        }
    }

    /// Snapshot every cell over the whole driven `[-16,32)^2 x [minY,minY+height)`
    /// region, canonicalised, keyed the same way `snapKey` packs it.
    static Map<Long, String> snapshotRegion() {
        Map<Long, String> snap = new HashMap<>();
        BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
        for (int lx = -16; lx < 32; lx++) {
            for (int lz = -16; lz < 32; lz++) {
                int wx = chunkX * 16 + lx, wz = chunkZ * 16 + lz;
                int cx = Math.floorDiv(wx, 16), cz = Math.floorDiv(wz, 16);
                int llx = wx - cx * 16, llz = wz - cz * 16;
                ProtoChunk owner = chunkAt(cx, cz);
                for (int y = minY; y < minY + height; y++) {
                    String c = canon(owner.getBlockState(p.set(cx * 16 + llx, y, cz * 16 + llz)));
                    snap.put(snapKey(lx, y, lz), c);
                }
            }
        }
        return snap;
    }

    /// Fresh fill+surface+carve for the 3x3 neighbourhood, then the real
    /// UNDERGROUND_ORES step over all 9 sources — the shared, deterministic
    /// starting point both VEGETAL_DECORATION passes below measure from.
    /// Clears `chunkCache` first so a caller can rebuild this baseline
    /// cleanly between the SINGLE and FULL3X3 passes (there is no cheap way
    /// to "unplace" a feature already written into a real `ProtoChunk`, so
    /// this regenerates instead — cheap relative to a JVM boot, and it is a
    /// deterministic replay, not a second, different world).
    static void resetToPostOreBaseline() {
        chunkCache = new HashMap<>();
        for (int dx = -1; dx <= 1; dx++) {
            for (int dz = -1; dz <= 1; dz++) {
                chunkAt(chunkX + dx, chunkZ + dz);
            }
        }
        runStep(GenerationStep.Decoration.UNDERGROUND_ORES.ordinal(), true);
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindBlockTags();
        provider = VanillaRegistries.createLookup();

        String rawArgs = System.getenv("ORACLE_ARGS");
        if (rawArgs == null) rawArgs = "";
        rawArgs = rawArgs.trim();
        biomeName = "minecraft:plains";
        chunkX = 0;
        chunkZ = 0;
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

        level = makeLevel();

        int STEP = GenerationStep.Decoration.VEGETAL_DECORATION.ordinal();

        // ---- Pass 1: SINGLE (centre-only) vegetal decoration ----
        resetToPostOreBaseline();
        Map<Long, String> postOre = snapshotRegion();
        dumpRegionBaseline(postOre);
        long singleDecorationSeed = decorationSeedFor(chunkX, chunkZ);
        runStep(STEP, false);
        Map<Long, String> postSingle = snapshotRegion();
        dumpDiff("single", postOre, postSingle);

        // ---- Pass 2: FULL3X3 (real vanilla spill) vegetal decoration,
        // from the SAME post-ore baseline, regenerated fresh ----
        resetToPostOreBaseline();
        Map<Long, String> postOre2 = snapshotRegion();
        int sanityMismatches = 0;
        for (Map.Entry<Long, String> e : postOre.entrySet()) {
            if (!e.getValue().equals(postOre2.get(e.getKey()))) sanityMismatches++;
        }
        sb.append("meta.postOreReplayMismatches ").append(sanityMismatches).append('\n');
        runStep(STEP, true);
        Map<Long, String> postFull = snapshotRegion();
        dumpDiff("full3x3", postOre2, postFull);

        sb.append("meta.decorationSeed ").append(singleDecorationSeed).append('\n');
        sb.append("meta.originX ").append(chunkX * 16).append('\n');
        sb.append("meta.originZ ").append(chunkZ * 16).append('\n');
        sb.append("meta.step ").append(STEP).append('\n');
        sb.append("meta.biome ").append(biomeName).append('\n');
        sb.append("meta.chunkX ").append(chunkX).append('\n');
        sb.append("meta.chunkZ ").append(chunkZ).append('\n');
        sb.append("meta.minY ").append(minY).append('\n');
        sb.append("meta.height ").append(height).append('\n');
        sb.append("meta.seed ").append(seed).append('\n');
        synchronized (proxyCalls) {
            for (String name : proxyCalls) sb.append("meta.proxyCall ").append(name).append('\n');
        }
        synchronized (proxyFallbacks) {
            for (String name : proxyFallbacks) sb.append("meta.proxyFallback ").append(name).append('\n');
        }

        System.out.print(sb);
    }

    static long decorationSeedFor(int cx, int cz) {
        ChunkPos pos = new ChunkPos(cx, cz);
        BlockPos origin = SectionPos.of(pos, heightAccessor.getMinSectionY()).origin();
        WorldgenRandom r = new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
        return r.setDecorationSeed(seed, origin.getX(), origin.getZ());
    }

    /// Dumps every `(lx,y,lz)` (centre-relative, over the WHOLE -16..32
    /// region) whose canonical state differs between `before` and `after`,
    /// prefixed `<label>.diff.`, plus `<label>.meta.centreChanged`/
    /// `<label>.meta.totalChanged` counts (centre 16x16 vs the whole
    /// region) so the Rust side never has to re-derive the centre/spill
    /// split itself.
    static void dumpDiff(String label, Map<Long, String> before, Map<Long, String> after) {
        int centreChanged = 0, totalChanged = 0;
        Map<String, Integer> perBlock = new TreeMap<>();
        for (int lx = -16; lx < 32; lx++) {
            for (int lz = -16; lz < 32; lz++) {
                for (int y = minY; y < minY + height; y++) {
                    long k = snapKey(lx, y, lz);
                    String a = after.get(k);
                    String b = before.get(k);
                    if (a == null) a = "minecraft:air";
                    if (b == null) b = "minecraft:air";
                    if (!a.equals(b)) {
                        totalChanged++;
                        boolean inCentre = lx >= 0 && lx < 16 && lz >= 0 && lz < 16;
                        if (inCentre) centreChanged++;
                        perBlock.merge(a, 1, Integer::sum);
                        sb.append(label).append(".diff.").append(lx).append(',').append(y).append(',').append(lz)
                          .append(' ').append(a).append('\n');
                    }
                }
            }
        }
        sb.append(label).append(".meta.centreChanged ").append(centreChanged).append('\n');
        sb.append(label).append(".meta.totalChanged ").append(totalChanged).append('\n');
        for (Map.Entry<String, Integer> e : perBlock.entrySet())
            sb.append(label).append(".count.").append(e.getKey()).append(' ').append(e.getValue()).append('\n');
    }

    /// Dumps the WHOLE driven `-16..32` region's post-ore terrain
    /// (`base.x,z y_start count state`, run-length-encoded per column,
    /// matching `FeatureOracle.java`'s `inrun.` pattern) — what the Rust
    /// side seeds its region-wide `VegGrid` from to run its own engine
    /// against the identical starting terrain this oracle's SINGLE and
    /// FULL3X3 passes both ran against.
    ///
    /// **Widened from centre-only (issue #427).** This used to dump only the
    /// centre 16x16, on the reasoning that `crate::feature::vegetation
    /// ::VegGrid` never read real neighbour terrain — true for the
    /// single-chunk engine that reasoning was written for, but false for
    /// `apply_vegetal_decoration_step_3x3_per_source`'s real 3x3 driver
    /// (issue #427): each of the 8 neighbours' own decoration pass reads and
    /// writes its OWN real terrain, so a Rust-side FULL3X3 replay needs all
    /// 9 chunks' baseline, not just the centre's — the same reason the ore
    /// engine's `RegionGrid`/`OreInput` cover a wide region rather than one
    /// chunk. `dumpDiff`'s own footprint (`-16..32` on both axes) was always
    /// this wide; this function now matches it.
    static void dumpRegionBaseline(Map<Long, String> postOre) {
        for (int lx = -16; lx < 32; lx++) {
            for (int lz = -16; lz < 32; lz++) {
                int y = minY;
                int end = minY + height;
                while (y < end) {
                    String s = postOre.get(snapKey(lx, y, lz));
                    if (s == null) s = "minecraft:air";
                    int count = 1;
                    while (y + count < end) {
                        String next = postOre.get(snapKey(lx, y + count, lz));
                        if (next == null) next = "minecraft:air";
                        if (!next.equals(s)) break;
                        count++;
                    }
                    sb.append("base.").append(lx).append(',').append(lz).append(' ')
                      .append(y).append(' ').append(count).append(' ').append(s).append('\n');
                    y += count;
                }
            }
        }
    }

    static long snapKey(int lx, int y, int lz) {
        return (((long) (lx + 32)) << 40) ^ (((long) (lz + 32)) << 24) ^ (long) (y - minY);
    }

    // Same dynamic-proxy WorldGenLevel FeatureOracle.java proved: unclamped,
    // memoised, on-demand chunkAt for getChunk/getHeight.
    static WorldGenLevel makeLevel() {
        WorldGenLevel[] self = new WorldGenLevel[1];
        InvocationHandler handler = (proxy, method, methodArgs) -> {
            String name = method.getName();
            proxyCalls.add(name);
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
                    return chunkAt(cx, cz);
                }
                // Vegetal decoration's own placement predicates
                // (`MatchingFluidsPredicate`/`WouldSurvivePredicate`, unlike
                // ore's `RuleTest`-based checks which route through
                // `BulkSectionAccess`/`getChunk` instead) call
                // `LevelReader.getBlockState`/`getFluidState` directly on the
                // level — routed through the same unclamped `chunkAt` every
                // other read here uses.
                case "getBlockState": {
                    BlockPos bp = (BlockPos) a[0];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    return chunkAt(cx, cz).getBlockState(bp);
                }
                case "getFluidState": {
                    BlockPos bp = (BlockPos) a[0];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    return chunkAt(cx, cz).getFluidState(bp);
                }
                case "isEmptyBlock": {
                    BlockPos bp = (BlockPos) a[0];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    return chunkAt(cx, cz).getBlockState(bp).isAir();
                }
                case "isWaterAt": {
                    BlockPos bp = (BlockPos) a[0];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    return chunkAt(cx, cz).getFluidState(bp).is(net.minecraft.tags.FluidTags.WATER);
                }
                // `LevelSimulatedReader.isStateAtPosition`/`isFluidAtPosition`
                // are ABSTRACT on that interface (`Level`'s own
                // implementation is just `predicate.test(this.getBlockState/
                // getFluidState(pos))` — Level.java:1053/1058), so a proxy
                // with no case for them fell through to the `default:`
                // branch below, which force-returns `Boolean.FALSE` for
                // every boolean-returning method it doesn't recognise.
                // **That silently broke every tree ever placed by this
                // oracle**: `TreeFeature.validTreePos` — the gate both
                // `TrunkPlacer.placeLog` (every log) and
                // `FoliagePlacer.tryPlaceLeaf` (every leaf) call before
                // writing anything — is defined as exactly
                // `level.isStateAtPosition(pos, state -> state.isAir() ||
                // state.is(BlockTags.REPLACEABLE_BY_TREES))`
                // (`TreeFeature.java:52-54`), so with this case missing it
                // always evaluated to `false` and no trunk placer of any
                // kind could ever place a single block — the committed
                // plains fixtures happening to show zero `oak_log` looked
                // like bad luck at plains' genuinely low ~5%-per-chunk tree
                // rate, until the SAME zero recurred at real, known savanna
                // coordinates (`(-2500,3200)`, `docs`'s own
                // `biome_matches_vanilla_at_known_coordinates_seed_42`
                // fixture) where `trees_savanna`'s outer count is `weighted_
                // list{1: 9, 2: 1}` — ALWAYS at least one attempt per
                // chunk, never zero — across 9 sources and two separate
                // real locations. That is not explainable by chance; it is
                // this proxy gap, discovered while adding issue #428's
                // acacia trunk/foliage placer support and needing a real
                // tree to actually land in a fixture to test against.
                case "isStateAtPosition": {
                    BlockPos bp = (BlockPos) a[0];
                    @SuppressWarnings("unchecked")
                    java.util.function.Predicate<BlockState> predicate = (java.util.function.Predicate<BlockState>) a[1];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    return predicate.test(chunkAt(cx, cz).getBlockState(bp));
                }
                case "isFluidAtPosition": {
                    BlockPos bp = (BlockPos) a[0];
                    @SuppressWarnings("unchecked")
                    java.util.function.Predicate<net.minecraft.world.level.material.FluidState> predicate =
                        (java.util.function.Predicate<net.minecraft.world.level.material.FluidState>) a[1];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    return predicate.test(chunkAt(cx, cz).getFluidState(bp));
                }
                // Vegetal decoration can ask whether a naturally generated
                // mushroom may survive at a candidate position. Lighting has
                // not been initialized during this stage, so the generation
                // level reports zero raw brightness, as the real worldgen
                // light engine does before INITIALIZE_LIGHT.
                case "getRawBrightness": return 0;
                // The actual write path for every non-ore feature this
                // oracle places (`SimpleBlockFeature`/`TreeFeature`/
                // `BlockColumnFeature` all call `WorldGenLevel.setBlock`
                // directly — `LevelWriter.setBlock(pos, state, updateFlags,
                // updateLimit)` is ABSTRACT, no default, so without this case
                // every placement silently no-ops: `Feature.place`'s own
                // return value is independent of what `setBlock` itself
                // returns, so `placeWithBiomeCheck` can report `true`
                // (placedCount > 0) while nothing was ever written to the
                // real `ProtoChunk`. Caught by adding a placedCount counter
                // and finding it nonzero while every `dumpDiff` output was
                // empty — see this oracle's own commit message for the
                // measured before/after. Unlike ore's `OreFeature.doPlace`,
                // which writes through `BulkSectionAccess` (itself backed by
                // `getChunk`'s real `LevelChunkSection`s, never touching this
                // method at all — which is exactly why `FeatureOracle.java`
                // never needed this case).
                case "setBlock": {
                    BlockPos bp = (BlockPos) a[0];
                    BlockState state = (BlockState) a[1];
                    int cx = bp.getX() >> 4, cz = bp.getZ() >> 4;
                    chunkAt(cx, cz).setBlockState(bp, state);
                    return Boolean.TRUE;
                }
                case "getBiome":
                    return biomeHolder;
                case "ensureCanWrite":
                    return Boolean.TRUE;
                case "getSeed":
                    return seed;
                case "getLevel":
                    return self[0];
                case "getRandom":
                    return new LegacyRandomSource(seed);
                case "scheduleTick":
                    return null;
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
                    return "VegetationOracleLevel";
                default: {
                    proxyFallbacks.add(name + "/" + (a == null ? 0 : a.length));
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
            VegetationOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler);
        self[0] = lvl;
        return lvl;
    }
}
