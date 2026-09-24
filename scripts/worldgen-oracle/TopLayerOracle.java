// Isolated TOP_LAYER_MODIFICATION oracle (issue #404's U2: `freeze_top_layer`
// parity). Boots the real 26.2 server, builds a `FixedBiomeSource` for one
// named biome, runs vanilla's own doFill + buildSurface + applyCarvers over a
// 3x3 chunk neighbourhood, replays the real UNDERGROUND_ORES and
// VEGETAL_DECORATION steps over all 9 sources (so the centre chunk this oracle
// measures is vanilla's real post-decoration terrain, not an approximation),
// and THEN runs the TOP_LAYER_MODIFICATION step (ordinal 10) for the CENTRE
// chunk only, dumping exactly what changed.
//
// Why centre-only is not a scope reduction here, unlike every other feature
// step: `SnowAndFreezeFeature.place` loops `dx`/`dz` over `0..16` from its own
// `origin` and writes only at `(x, y, z)` and `(x, y-1, z)`
// (`SnowAndFreezeFeature.java:26-45`). There is no radius, no offset placement
// modifier (the placed feature is just `[{"type":"minecraft:biome"}]`), and
// therefore no cross-chunk spill to drive. Centre-only IS vanilla's full
// behaviour for this step.
//
// This is deliberately built on `VegetationOracle.java`'s proven scaffolding
// (dynamic-proxy `WorldGenLevel`, memoised unclamped `chunkAt`, `canon`,
// `bindBlockTags`, RLE `base.` dump) with ONE structural change, which is the
// whole point of the file:
//
//   **The proxy's `default:` arm THROWS.**
//
// `VegetationOracle`'s `default:` arm force-returned `Boolean.FALSE` for every
// unrecognised boolean method, and because `LevelSimulatedReader
// ::isStateAtPosition` is abstract with no case for it, `TreeFeature
// ::validTreePos` evaluated `false` forever and **no tree ever placed a single
// block** while the harness cheerfully reported success. That defect is
// structurally available to this oracle too, and worse: `SnowAndFreezeFeature`
// reaches `LevelHeightAccessor::isInsideBuildHeight` (a *default* method, so a
// proxy still intercepts it) and `LevelReader::getSeaLevel`, and a `false`
// from the former or a `0` from the latter each independently produces an
// all-zero, entirely plausible-looking result:
//
//   * `isInsideBuildHeight == false`  -> `Biome.shouldFreeze`/`shouldSnow` both
//     short-circuit to `false` (`Biome.java:150`/`188`) -> zero writes.
//   * `getSeaLevel() == 0`            -> `getHeightAdjustedTemperature`'s
//     `snowLevel = seaLevel + 17` becomes 17, so every real surface y is
//     "above the snow line" and the noise-based lapse term is applied from the
//     wrong datum (`Biome.java:112-121`) -> wrong, silently.
//
// So `default:` throws `UnsupportedOperationException(<method>)`, only
// `hashCode`/`equals`/`toString` are exempt, every method the feature actually
// needs is implemented explicitly, and every intercepted name is recorded and
// emitted as `meta.proxyCall` so the exercised surface is auditable from the
// fixture itself rather than asserted in prose.
//
// No Mojang source is copied — this only drives compiled classes.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Path;
import java.util.EnumSet;
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
import net.minecraft.tags.BlockTags;
import net.minecraft.tags.FluidTags;
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
import net.minecraft.world.flag.FeatureFlags;
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
import net.minecraft.world.level.chunk.status.ChunkStatus;
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

public final class TopLayerOracle {
    static HolderLookup.Provider provider;
    static Holder<Biome> biomeHolder;
    static NoiseGeneratorSettings settings;
    static NoiseBasedChunkGenerator generator;
    static RandomState rs;
    static LevelHeightAccessor heightAccessor;
    static RegistryAccess paletteAccess;
    static PalettedContainerFactory factory;
    static Aquifer.FluidPicker fluidPicker;
    static int minY, height, seaLevel;
    static long seed;
    static int chunkX, chunkZ;
    static String biomeName;
    /// `WorldGenRegion.random`, reproduced exactly: the positional factory
    /// named `worldgen_region_random`, sampled at the centre chunk's world
    /// position (`WorldGenRegion.java:89`). See the `getRandom` proxy case for
    /// why this is not a detail.
    static net.minecraft.util.RandomSource regionRandom;
    /// `WorldGenRegion.subTickCount` (`WorldGenRegion.java:497-499`).
    static long subTickCount = 0L;

    static final int STEP = GenerationStep.Decoration.TOP_LAYER_MODIFICATION.ordinal();

    static Map<Long, ProtoChunk> chunkCache = new HashMap<>();

    /// Every `WorldGenLevel` method name the dynamic proxy actually
    /// intercepted, for the `meta.proxyCall` audit. Synchronised because
    /// `fillFromNoise` returns a `CompletableFuture` and may run off the main
    /// thread.
    static final Set<String> proxyCalls = java.util.Collections.synchronizedSet(new TreeSet<String>());

    static <T extends Comparable<T>> String propVal(BlockState s, Property<T> p) {
        return p.getName(s.getValue(p));
    }

    /// Identical to `VegetationOracle.canon` — block id, plus
    /// alphabetically-sorted `key=value` pairs in brackets, no brackets when
    /// propertyless.
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

    /// The tag bind above is load-bearing for THIS oracle in a way it was not
    /// for the vegetation one: `SnowLayerBlock.canSurvive` consults
    /// `CANNOT_SUPPORT_SNOW_LAYER` and `SUPPORT_OVERRIDE_SNOW_LAYER`
    /// (`SnowLayerBlock.java:77-86`), and an *empty* tag is not an error — it
    /// silently makes every `is(tag)` false, so snow would happily sit on ice.
    /// So verify the bind populated them, by membership rather than by "the
    /// call did not throw", and fail loudly if not.
    static void verifyBlockTags() {
        boolean iceCannotSupport = Blocks.ICE.defaultBlockState().is(BlockTags.CANNOT_SUPPORT_SNOW_LAYER);
        boolean packedIceCannotSupport = Blocks.PACKED_ICE.defaultBlockState().is(BlockTags.CANNOT_SUPPORT_SNOW_LAYER);
        boolean stoneCannotSupport = Blocks.STONE.defaultBlockState().is(BlockTags.CANNOT_SUPPORT_SNOW_LAYER);
        boolean mudOverride = Blocks.MUD.defaultBlockState().is(BlockTags.SUPPORT_OVERRIDE_SNOW_LAYER);
        boolean stoneOverride = Blocks.STONE.defaultBlockState().is(BlockTags.SUPPORT_OVERRIDE_SNOW_LAYER);
        System.err.println("[tags] ice in CANNOT_SUPPORT_SNOW_LAYER = " + iceCannotSupport);
        System.err.println("[tags] packed_ice in CANNOT_SUPPORT_SNOW_LAYER = " + packedIceCannotSupport);
        System.err.println("[tags] stone in CANNOT_SUPPORT_SNOW_LAYER = " + stoneCannotSupport + " (expect false)");
        System.err.println("[tags] mud in SUPPORT_OVERRIDE_SNOW_LAYER = " + mudOverride);
        System.err.println("[tags] stone in SUPPORT_OVERRIDE_SNOW_LAYER = " + stoneOverride + " (expect false)");
        if (!iceCannotSupport || !packedIceCannotSupport || !mudOverride) {
            throw new IllegalStateException(
                "block tag bind did not populate the snow-support tags: ice=" + iceCannotSupport
                + " packed_ice=" + packedIceCannotSupport + " mud=" + mudOverride);
        }
        // The negative half: an "everything matches" bind would be just as
        // broken as an empty one, and would also pass the assertions above.
        if (stoneCannotSupport || stoneOverride) {
            throw new IllegalStateException("snow-support tags match blocks they must not");
        }
    }

    static long chunkKey(int cx, int cz) {
        return (((long) cx) << 32) ^ (cz & 0xffffffffL);
    }

    /// Same memoised, unclamped, on-demand per-chunk generator
    /// `VegetationOracle`/`FeatureOracle` proved (fill + surface + carve only —
    /// features are applied separately by `runStep`).
    ///
    /// One addition: the finished chunk is promoted to `ChunkStatus.FEATURES`
    /// before any feature runs. That status's `heightmapsAfter()` is
    /// `FINAL_HEIGHTMAPS` (`ChunkStatus.java:17-28`), which is what makes
    /// `ProtoChunk.setBlockState` keep `MOTION_BLOCKING` up to date
    /// (`ProtoChunk.java:147-167`). A chunk left at `EMPTY` only maintains the
    /// `*_WG` pair, so a grass block placed by VEGETAL_DECORATION would not
    /// raise `MOTION_BLOCKING` and `freeze_top_layer` — whose entire input is
    /// `getHeight(MOTION_BLOCKING, x, z)` — would read a stale surface. That is
    /// exactly the class of silent, plausible wrongness this file exists to
    /// avoid, so it is fixed rather than documented. `FEATURES` is below
    /// `INITIALIZE_LIGHT`, so the null light engine is still never touched.
    static ProtoChunk chunkAt(int cx, int cz) {
        long k = chunkKey(cx, cz);
        ProtoChunk cached = chunkCache.get(k);
        if (cached != null) return cached;

        ChunkPos pos = new ChunkPos(cx, cz);
        ProtoChunk chunk = new ProtoChunk(pos, UpgradeData.EMPTY, heightAccessor, factory, null);
        // Memoise BEFORE generating: carving reads this chunk back through the
        // proxy, and a re-entrant chunkAt for the same key would otherwise
        // recurse. (VegetationOracle inserts at the end; its carve path never
        // re-enters for the same key, but the freeze step's reads make the
        // ordering worth not relying on.)
        chunkCache.put(k, chunk);

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

        chunk.setPersistedStatus(ChunkStatus.FEATURES);
        return chunk;
    }

    static WorldGenLevel level;

    /// Runs every placed feature in decoration step `stepOrdinal`, for the 9
    /// chunks in `chunkX/chunkZ ± 1` if `full3x3`, else the centre only — each
    /// with its own origin and its own decoration seed, matching vanilla's real
    /// per-chunk `applyBiomeDecoration`. Returns the number of placed features
    /// in the step's list (the denominator `meta.stepFeatures` reports).
    static int runStep(int stepOrdinal, boolean full3x3) {
        List<FeatureSorter.StepFeatureData> perStep = FeatureSorter.buildFeaturesPerStep(
            List.of(biomeHolder), b -> b.value().getGenerationSettings().features(), true);
        if (stepOrdinal >= perStep.size()) {
            throw new IllegalStateException("biome " + biomeName + " has no step " + stepOrdinal
                + " (perStep.size()=" + perStep.size() + ")");
        }
        List<PlacedFeature> feats = perStep.get(stepOrdinal).features();
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
                    if (pf.placeWithBiomeCheck(level, generator, fRandom, origin)) placedCount++;
                }
            }
        }
        System.err.println("[step] ordinal=" + stepOrdinal + " full3x3=" + full3x3
            + " features=" + feats.size() + " placedTrue=" + placedCount);
        return feats.size();
    }

    /// Canonical snapshot of the CENTRE chunk only, indexed `[lx][lz][y-minY]`.
    static String[][][] snapshotCentre() {
        String[][][] snap = new String[16][16][height];
        ProtoChunk centre = chunkAt(chunkX, chunkZ);
        BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
        for (int lx = 0; lx < 16; lx++) {
            for (int lz = 0; lz < 16; lz++) {
                for (int y = minY; y < minY + height; y++) {
                    snap[lx][lz][y - minY] = canon(centre.getBlockState(p.set(chunkX * 16 + lx, y, chunkZ * 16 + lz)));
                }
            }
        }
        return snap;
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindBlockTags();
        verifyBlockTags();
        provider = VanillaRegistries.createLookup();

        String rawArgs = System.getenv("ORACLE_ARGS");
        if (rawArgs == null) rawArgs = "";
        rawArgs = rawArgs.trim();
        biomeName = "minecraft:snowy_plains";
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
        biomeHolder = provider.lookupOrThrow(Registries.BIOME).getOrThrow(biomeKey);

        Holder<NoiseGeneratorSettings> settingsHolder =
            provider.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        settings = settingsHolder.value();

        FixedBiomeSource biomeSource = new FixedBiomeSource(biomeHolder);
        generator = new NoiseBasedChunkGenerator(biomeSource, settingsHolder);
        rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);

        minY = -64;
        height = 384;
        heightAccessor = LevelHeightAccessor.create(minY, height);
        seaLevel = settings.seaLevel();

        MappedRegistry<Biome> biomeReg = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        Registry.register(biomeReg, Biomes.PLAINS, provider.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS).value());
        biomeReg.freeze();
        paletteAccess = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(biomeReg));
        factory = PalettedContainerFactory.create(paletteAccess);

        Aquifer.FluidStatus lavaStatus = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        Aquifer.FluidStatus seaStatus = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        fluidPicker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lavaStatus : seaStatus;

        regionRandom = rs.getOrCreateRandomFactory(
                net.minecraft.resources.Identifier.withDefaultNamespace("worldgen_region_random"))
            .at(new ChunkPos(chunkX, chunkZ).getWorldPosition());

        level = makeLevel();

        // ---- Build the real post-decoration neighbourhood ----
        for (int dx = -1; dx <= 1; dx++) {
            for (int dz = -1; dz <= 1; dz++) chunkAt(chunkX + dx, chunkZ + dz);
        }
        runStep(GenerationStep.Decoration.UNDERGROUND_ORES.ordinal(), true);
        runStep(GenerationStep.Decoration.VEGETAL_DECORATION.ordinal(), true);

        // Recompute the centre chunk's heightmaps from its actual blocks, so
        // `top.` and the freeze step both read a surface that is definitionally
        // current rather than one maintained incrementally. Idempotent: a
        // correctly-maintained map re-primes to itself.
        Heightmap.primeHeightmaps(chunkAt(chunkX, chunkZ), ChunkStatus.FINAL_HEIGHTMAPS);

        // ---- base. + top. : the centre chunk immediately before step 10 ----
        String[][][] before = snapshotCentre();

        StringBuilder topSb = new StringBuilder();
        int[][] topY = new int[16][16];
        for (int lx = 0; lx < 16; lx++) {
            for (int lz = 0; lz < 16; lz++) {
                int y = level.getHeight(Heightmap.Types.MOTION_BLOCKING, chunkX * 16 + lx, chunkZ * 16 + lz);
                topY[lx][lz] = y;
                topSb.append("top.").append(lx).append(',').append(lz).append(' ').append(y).append('\n');
            }
        }

        // ---- the measurement: TOP_LAYER_MODIFICATION, centre chunk only ----
        int stepFeatures = runStep(STEP, false);
        String[][][] after = snapshotCentre();

        StringBuilder freezeSb = new StringBuilder();
        int freezeSnow = 0, freezeIce = 0, freezeSnowy = 0, freezeTotal = 0;
        Map<String, Integer> perBlock = new TreeMap<>();
        for (int lx = 0; lx < 16; lx++) {
            for (int lz = 0; lz < 16; lz++) {
                for (int y = minY; y < minY + height; y++) {
                    String a = after[lx][lz][y - minY];
                    String b = before[lx][lz][y - minY];
                    if (a.equals(b)) continue;
                    freezeTotal++;
                    freezeSb.append("freeze.").append(lx).append(',').append(y).append(',').append(lz)
                            .append(' ').append(a).append('\n');
                    if (a.equals("minecraft:snow") || a.startsWith("minecraft:snow[")) freezeSnow++;
                    if (a.equals("minecraft:ice") || a.startsWith("minecraft:ice[")) freezeIce++;
                    if (a.contains("snowy=true")) freezeSnowy++;
                    perBlock.merge(a, 1, Integer::sum);
                }
            }
        }
        for (Map.Entry<String, Integer> e : perBlock.entrySet())
            System.err.println("[freeze] " + e.getKey() + " x" + e.getValue());
        System.err.println("[freeze] total=" + freezeTotal + " snow=" + freezeSnow
            + " ice=" + freezeIce + " snowy=" + freezeSnowy);

        StringBuilder out = new StringBuilder();
        out.append("meta.biome ").append(biomeName).append('\n');
        out.append("meta.seed ").append(seed).append('\n');
        out.append("meta.chunkX ").append(chunkX).append('\n');
        out.append("meta.chunkZ ").append(chunkZ).append('\n');
        out.append("meta.minY ").append(minY).append('\n');
        out.append("meta.height ").append(height).append('\n');
        out.append("meta.seaLevel ").append(seaLevel).append('\n');
        out.append("meta.stepIndex ").append(STEP).append('\n');
        out.append("meta.stepFeatures ").append(stepFeatures).append('\n');
        synchronized (proxyCalls) {
            for (String n : proxyCalls) out.append("meta.proxyCall ").append(n).append('\n');
        }
        appendBaseline(out, before);
        out.append(freezeSb);
        out.append(topSb);
        out.append("meta.freezeSnow ").append(freezeSnow).append('\n');
        out.append("meta.freezeIce ").append(freezeIce).append('\n');
        out.append("meta.freezeSnowy ").append(freezeSnowy).append('\n');
        out.append("meta.done ").append(chunkX).append(',').append(chunkZ).append('\n');

        System.out.print(out);
        System.out.flush();

        // A zero from a brand-new oracle is a defect report about the oracle,
        // not a fact about vanilla — so make the snowy case fail loudly rather
        // than write a plausible empty fixture. (Written AFTER the dump so the
        // partial output is still inspectable.)
        if (biomeName.endsWith("snowy_plains") && freezeSnow == 0) {
            throw new IllegalStateException(
                "snowy_plains produced ZERO snow writes at chunk " + chunkX + "," + chunkZ
                + " — the oracle is broken, not the biome (freezeTotal=" + freezeTotal + ")");
        }
    }

    /// RLE snapshot of the centre chunk, `base.<lx>,<lz> <startY> <runLength>
    /// <state>`, chunk-local `lx`/`lz` in `0..16`, ascending y from `minY` —
    /// the same encoding `VegetationOracle`'s `base.` uses, narrowed to the one
    /// chunk this step can touch.
    static void appendBaseline(StringBuilder out, String[][][] snap) {
        for (int lx = 0; lx < 16; lx++) {
            for (int lz = 0; lz < 16; lz++) {
                int y = minY;
                int end = minY + height;
                while (y < end) {
                    String s = snap[lx][lz][y - minY];
                    int count = 1;
                    while (y + count < end && snap[lx][lz][y + count - minY].equals(s)) count++;
                    out.append("base.").append(lx).append(',').append(lz).append(' ')
                       .append(y).append(' ').append(count).append(' ').append(s).append('\n');
                    y += count;
                }
            }
        }
    }

    static int yOf(Object arg) {
        return arg instanceof Number ? ((Number) arg).intValue() : ((BlockPos) arg).getY();
    }

    /// The dynamic-proxy `WorldGenLevel`. Unclamped, memoised, on-demand
    /// `chunkAt` for every read; every unrecognised method THROWS (see the
    /// file header for why that is the entire point).
    static WorldGenLevel makeLevel() {
        WorldGenLevel[] self = new WorldGenLevel[1];
        BiomeManager fixedBiomeManager = new BiomeManager((qx, qy, qz) -> biomeHolder, 0L);
        InvocationHandler handler = (proxy, method, methodArgs) -> {
            String name = method.getName();
            Object[] a = methodArgs;
            if (!name.equals("hashCode") && !name.equals("equals") && !name.equals("toString")) {
                proxyCalls.add(name);
            }
            switch (name) {
                // --- height / heightmaps ---
                case "getHeight": {
                    if (a == null || a.length == 0) return height;                 // LevelHeightAccessor.getHeight()
                    if (a.length == 2) {                                           // getHeight(Types, BlockPos)
                        BlockPos bp = (BlockPos) a[1];
                        return levelHeight((Heightmap.Types) a[0], bp.getX(), bp.getZ());
                    }
                    if (a.length == 3) {                                           // getHeight(Types, int, int)
                        return levelHeight((Heightmap.Types) a[0],
                            ((Number) a[1]).intValue(), ((Number) a[2]).intValue());
                    }
                    throw new UnsupportedOperationException("getHeight/" + a.length);
                }
                case "getMinY": return minY;
                case "getMaxY": return minY + height - 1;
                case "getMinBuildHeight": return minY;
                case "getSectionsCount": return height >> 4;
                case "getSectionIndex": return (((Number) a[0]).intValue() - minY) >> 4;
                case "getSectionIndexFromSectionY": return ((Number) a[0]).intValue() - (minY >> 4);
                case "getSectionYFromSectionIndex": return ((Number) a[0]).intValue() + (minY >> 4);
                case "getMinSectionY": return minY >> 4;
                case "getMaxSectionY": return (minY + height - 1) >> 4;
                // `isInsideBuildHeight` is a DEFAULT method on
                // LevelHeightAccessor (LevelHeightAccessor.java:27-33) and a
                // proxy still intercepts defaults, so without this case the
                // throwing default arm fires — and with the old
                // `return Boolean.FALSE` arm it would have silently gated
                // `Biome.shouldFreeze`/`shouldSnow` off entirely
                // (Biome.java:150/188) for an all-zero, plausible fixture.
                case "isInsideBuildHeight": {
                    int y = yOf(a[0]);
                    return y >= minY && y <= minY + height - 1;
                }
                case "isOutsideBuildHeight": {
                    int y = yOf(a[0]);
                    return y < minY || y > minY + height - 1;
                }

                // --- chunks ---
                case "getChunk": {
                    int cx, cz;
                    if (a[0] instanceof Number) { cx = ((Number) a[0]).intValue(); cz = ((Number) a[1]).intValue(); }
                    else if (a[0] instanceof ChunkPos) { cx = ((ChunkPos) a[0]).x(); cz = ((ChunkPos) a[0]).z(); }
                    else { BlockPos bp = (BlockPos) a[0]; cx = bp.getX() >> 4; cz = bp.getZ() >> 4; }
                    return chunkAt(cx, cz);
                }
                case "hasChunk": return Boolean.TRUE;   // chunkAt generates on demand

                // --- block / fluid reads ---
                case "getBlockState": {
                    BlockPos bp = (BlockPos) a[0];
                    return chunkAt(bp.getX() >> 4, bp.getZ() >> 4).getBlockState(bp);
                }
                case "getFluidState": {
                    BlockPos bp = (BlockPos) a[0];
                    return chunkAt(bp.getX() >> 4, bp.getZ() >> 4).getFluidState(bp);
                }
                case "isStateAtPosition": {
                    BlockPos bp = (BlockPos) a[0];
                    @SuppressWarnings("unchecked")
                    java.util.function.Predicate<BlockState> predicate =
                        (java.util.function.Predicate<BlockState>) a[1];
                    return predicate.test(chunkAt(bp.getX() >> 4, bp.getZ() >> 4).getBlockState(bp));
                }
                case "isFluidAtPosition": {
                    BlockPos bp = (BlockPos) a[0];
                    @SuppressWarnings("unchecked")
                    java.util.function.Predicate<net.minecraft.world.level.material.FluidState> predicate =
                        (java.util.function.Predicate<net.minecraft.world.level.material.FluidState>) a[1];
                    return predicate.test(chunkAt(bp.getX() >> 4, bp.getZ() >> 4).getFluidState(bp));
                }
                case "isEmptyBlock": {
                    BlockPos bp = (BlockPos) a[0];
                    return chunkAt(bp.getX() >> 4, bp.getZ() >> 4).getBlockState(bp).isAir();
                }
                // `Biome.shouldFreeze(level, pos, true)` uses this; the freeze
                // feature passes `checkNeighbors = false` so it short-circuits
                // before ever reaching it (Biome.java:154-156) — implemented
                // correctly anyway rather than left to a wrong default, since
                // "never called" is a claim `meta.proxyCall` can now settle.
                case "isWaterAt": {
                    BlockPos bp = (BlockPos) a[0];
                    return chunkAt(bp.getX() >> 4, bp.getZ() >> 4).getFluidState(bp).is(FluidTags.WATER);
                }

                // --- writes ---
                case "setBlock": {
                    BlockPos bp = (BlockPos) a[0];
                    BlockState state = (BlockState) a[1];
                    int flags = a.length >= 3 && a[2] instanceof Number ? ((Number) a[2]).intValue() : 3;
                    chunkAt(bp.getX() >> 4, bp.getZ() >> 4).setBlockState(bp, state, flags);
                    return Boolean.TRUE;
                }
                case "ensureCanWrite": return Boolean.TRUE;
                // Also VEGETAL_DECORATION-only, and also only visible because
                // the default arm throws: a *waterlogged* leaf block's
                // `updateShape` schedules a water tick
                // (`LeavesBlock.java:99-101`). `scheduleTick` is a DEFAULT on
                // `ScheduledTickAccess`, so the proxy intercepts it and
                // vanilla's own body — `getFluidTicks().schedule(createTick(
                // …))` — never runs; reimplementing it here to land in the
                // owning `ProtoChunk`'s real tick container is what vanilla's
                // `WorldGenTickAccess` does (`WorldGenRegion.java:71-72`).
                // Nothing in this oracle ever *drains* a tick, so a silent
                // no-op would have been indistinguishable in the output — which
                // is exactly the reason not to write one.
                case "scheduleTick": {
                    BlockPos bp = ((BlockPos) a[0]).immutable();
                    int delay = ((Number) a[2]).intValue();
                    net.minecraft.world.ticks.TickPriority prio = a.length >= 4
                        ? (net.minecraft.world.ticks.TickPriority) a[3]
                        : net.minecraft.world.ticks.TickPriority.NORMAL;
                    ProtoChunk owner = chunkAt(bp.getX() >> 4, bp.getZ() >> 4);
                    // Game time is 0 in this harness (there is no `LevelData`),
                    // so `triggerTick` is just the delay. Only the drain order
                    // would care, and nothing drains.
                    if (a[1] instanceof Block) {
                        owner.getBlockTicks().schedule(new net.minecraft.world.ticks.ScheduledTick<Block>(
                            (Block) a[1], bp, delay, prio, subTickCount++));
                    } else {
                        owner.getFluidTicks().schedule(new net.minecraft.world.ticks.ScheduledTick<net.minecraft.world.level.material.Fluid>(
                            (net.minecraft.world.level.material.Fluid) a[1], bp, delay, prio, subTickCount++));
                    }
                    return null;
                }
                case "getBlockTicks":
                    return new net.minecraft.world.ticks.WorldGenTickAccess<Block>(
                        p -> chunkAt(p.getX() >> 4, p.getZ() >> 4).getBlockTicks());
                case "getFluidTicks":
                    return new net.minecraft.world.ticks.WorldGenTickAccess<net.minecraft.world.level.material.Fluid>(
                        p -> chunkAt(p.getX() >> 4, p.getZ() >> 4).getFluidTicks());
                case "nextSubTickCount": return subTickCount++;
                case "getGameTime": return 0L;

                // --- climate inputs ---
                // Sea level is the datum for `getHeightAdjustedTemperature`'s
                // `snowLevel = seaLevel + 17` (Biome.java:114). A `0` here —
                // what the old default arm returned — moves the snow line to
                // y=17 and silently changes every temperature above it.
                case "getSeaLevel": return seaLevel;
                // Block light during worldgen is genuinely 0: nothing has been
                // lit yet, and `BlockAndLightGetter.getBrightness` would route
                // through a `getLightEngine()` this level does not have
                // (BlockAndLightGetter.java:9-11). Both `shouldFreeze` and
                // `shouldSnow` require `< 10` here, so 0 is the permissive —
                // and correct — value; it is asserted rather than defaulted so
                // that a future skylight query cannot silently reuse it.
                case "getBrightness": {
                    net.minecraft.world.level.LightLayer layer = (net.minecraft.world.level.LightLayer) a[0];
                    if (layer != net.minecraft.world.level.LightLayer.BLOCK) {
                        throw new UnsupportedOperationException("getBrightness/" + layer);
                    }
                    return 0;
                }
                // Reached by `MushroomBlock.canSurvive`'s `getRawBrightness(pos,
                // 0) < 13` gate (`MushroomBlock.java:86`), during
                // VEGETAL_DECORATION. Zero is the *real* worldgen value, not a
                // stub: `LevelLightEngine.getRawBrightness` is
                // `max(blockLight, skyLight - dampen)` over engines that have
                // not run for a sub-`INITIALIZE_LIGHT` chunk
                // (`LevelLightEngine.java:146-150`). It is also the permissive
                // side of every gate that reads it, so it cannot be hiding a
                // placement that vanilla would have made.
                case "getRawBrightness": return 0;
                case "getSkyDarken": return 0;
                // Reached by `TreeFeature.place` -> `StructureTemplate
                // ::updateShapeAtEdge` -> `state.updateShape(..., level
                // .getRandom())` (`StructureTemplate.java:426`/`432`), i.e. by
                // the VEGETAL_DECORATION prep, not by `freeze_top_layer` — and
                // ONLY discovered because this oracle's default arm throws.
                // `VegetationOracle`'s arm returns `null` for object returns,
                // so it has been handing every tree shape-update a null
                // `RandomSource` since it was written; harmless only for as
                // long as no `updateShape` implementation reads it. Reproduced
                // faithfully instead of stubbed.
                case "getRandom": return regionRandom;
                case "getBiome": return biomeHolder;
                case "getBiomeManager": return fixedBiomeManager;
                case "getNoiseBiome": return biomeHolder;
                case "getUncachedNoiseBiome": return biomeHolder;

                // --- misc plumbing ---
                case "getSeed": return seed;
                case "isClientSide": return Boolean.FALSE;
                case "enabledFeatures": return FeatureFlags.VANILLA_SET;
                case "registryAccess": return paletteAccess;

                case "hashCode": return System.identityHashCode(proxy);
                case "equals": return proxy == a[0];
                case "toString": return "TopLayerOracleLevel";

                // THROW, never default. See the file header: a zero/false/null
                // default here is how `VegetationOracle` placed zero trees for
                // months while reporting success, and how this oracle would
                // have written an all-zero snow fixture.
                default:
                    throw new UnsupportedOperationException(
                        name + "(" + method.getParameterCount() + " args) -> " + method.getReturnType().getName());
            }
        };
        WorldGenLevel lvl = (WorldGenLevel) Proxy.newProxyInstance(
            TopLayerOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler);
        self[0] = lvl;
        return lvl;
    }

    /// `LevelReader.getHeight(type, x, z)` semantics, transcribed from
    /// `WorldGenRegion.java:431-436`: the owning chunk's heightmap value **plus
    /// one** — i.e. the first free y above the surface, which is the `topPos`
    /// `SnowAndFreezeFeature` snows into.
    static int levelHeight(Heightmap.Types type, int wx, int wz) {
        int cx = Math.floorDiv(wx, 16), cz = Math.floorDiv(wz, 16);
        return chunkAt(cx, cz).getHeight(type, wx - cx * 16, wz - cz * 16) + 1;
    }
}
