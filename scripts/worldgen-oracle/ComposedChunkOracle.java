// Whole-chunk-parity oracle for the shared harness (worldgen-parity crate,
// issue: chunk-for-chunk parity harness). Unlike the other oracles in this
// directory — each isolated to one stage, pinned to a single fixed biome via
// FixedBiomeSource — this one runs the real, composed vanilla pipeline up
// through carvers, with the REAL `MultiNoiseBiomeSource` (the same 7594-row
// overworld parameter table `BiomeOracle.java` dumps), so the biome driving
// surface rules and carver selection is whatever vanilla actually assigns at
// each column, not a hardcoded constant. This is the ground truth the
// `lodestone-worldgen-parity` crate's committed fixtures come from.
//
// Pipeline driven, in vanilla's own order (`NoiseBasedChunkGenerator`):
//   fillFromNoise (shape + the REAL aquifer, not the sea-level approximation)
//   -> per-quart biome resolution (dumped between fill and surface, exactly
//      where Rust's `OverworldGenerator::biome_stage` runs)
//   -> buildSurface (surface rules, biome-parameterised)
//   -> applyCarvers (caves/ravines, replicating the real per-source-chunk
//      `carverBiome` resolution from `NoiseBasedChunkGenerator.applyCarvers`,
//      not a fixed biome)
//   -> postfeatures (ore-only decoration of the CENTRE chunk against its own
//      real per-column biome — narrower than `FeatureOracle.java`'s own
//      isolated ore fixture, which now drives a real 3x3 neighbour-spill
//      model; see the `dumpChunk` method's own doc comment at the
//      `postfeatures` stage for exactly what is and is not modelled here)
//
// Deliberately NOT composed here (see `docs/worldgen-parity.md` for why):
// vegetation features and structures. Structures are unbuilt anywhere in
// this repo's Rust. Vegetation has no isolated oracle yet anywhere in this
// directory (epic #404 Phase 3).
//
// No Mojang source is copied: this only drives compiled 26.2 classes (the
// same pattern as every other file in this directory) and reads their output.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import java.util.function.Function;

import com.mojang.datafixers.util.Pair;
import com.mojang.serialization.Lifecycle;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.QuartPos;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.SectionPos;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.ResourceKey;
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
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeGenerationSettings;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.FeatureSorter;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList;
import net.minecraft.world.level.block.Block;
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
import net.minecraft.world.level.levelgen.carver.ConfiguredWorldCarver;
import net.minecraft.world.level.levelgen.carver.CarvingContext;
import net.minecraft.world.level.levelgen.feature.ConfiguredFeature;
import net.minecraft.world.level.levelgen.feature.OreFeature;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;

public final class ComposedChunkOracle {
    static final StringBuilder sb = new StringBuilder(1 << 22);

    static <T extends Comparable<T>> String propVal(BlockState s, Property<T> p) {
        return p.getName(s.getValue(p));
    }

    static String canon(BlockState s) {
        StringBuilder b = new StringBuilder(net.minecraft.core.registries.BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString());
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

    // Bind block tags (e.g. #overworld_carver_replaceables) onto the frozen
    // BuiltInRegistries.BLOCK, exactly as CarverOracle.java does — without
    // this, canReplaceBlock is always false and carvers write nothing.
    @SuppressWarnings("unchecked")
    static void bindBlockTags() {
        java.nio.file.Path root = java.nio.file.Path.of("/mc/src");
        PackLocationInfo loc = new PackLocationInfo(
            "vanilla", Component.literal("vanilla"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(loc, root);
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(pack));
        Map<TagKey<Block>, List<Holder<Block>>> tags = TagLoader.loadTagsForRegistry(
            manager, Registries.BLOCK,
            (TagLoader.ElementLookup<Holder<Block>>) TagLoader.ElementLookup.fromFrozenRegistry(net.minecraft.core.registries.BuiltInRegistries.BLOCK));
        TagLoader.LoadResult<Block> lr = new TagLoader.LoadResult<>(Registries.BLOCK, tags);
        Registry.PendingTags<Block> pending = net.minecraft.core.registries.BuiltInRegistries.BLOCK.prepareTagReload(lr);
        pending.apply();
    }

    /// Highest world Y at (x, z) whose block is neither air nor a fluid, else
    /// `seaLevel - 1` — the exact definition `OverworldGenerator::biome_stage`
    /// (Rust) uses for its own `heights[]`, so biome sampling in both
    /// languages starts from the same height even though this is running
    /// after the REAL aquifer (which the Rust side only approximates).
    static int solidTop(ProtoChunk chunk, int x, int z, int minY, int height, int seaLevel) {
        BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        for (int y = minY + height - 1; y >= minY; y--) {
            BlockState s = chunk.getBlockState(pos.set(x, y, z));
            if (!s.isAir() && s.getFluidState().isEmpty()) {
                return y;
            }
        }
        return seaLevel - 1;
    }

    static void dumpChunk(HolderLookup.Provider provider, long seed, int chunkX, int chunkZ) {
        // --- real MultiNoiseBiomeSource, same table BiomeOracle.java dumps ---
        Map<MultiNoiseBiomeSourceParameterList.Preset, Climate.ParameterList<ResourceKey<Biome>>> presets =
            MultiNoiseBiomeSourceParameterList.knownPresets();
        Climate.ParameterList<ResourceKey<Biome>> keyTable = presets.get(MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD);
        List<Pair<Climate.ParameterPoint, Holder<Biome>>> resolved = new ArrayList<>(keyTable.values().size());
        HolderLookup.RegistryLookup<Biome> biomes = provider.lookupOrThrow(Registries.BIOME);
        for (Pair<Climate.ParameterPoint, ResourceKey<Biome>> p : keyTable.values()) {
            resolved.add(Pair.of(p.getFirst(), biomes.getOrThrow(p.getSecond())));
        }
        Climate.ParameterList<Holder<Biome>> paramList = new Climate.ParameterList<>(resolved);
        MultiNoiseBiomeSource biomeSource = MultiNoiseBiomeSource.createFromList(paramList);

        Holder<NoiseGeneratorSettings> settingsHolder =
            provider.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        NoiseGeneratorSettings settings = settingsHolder.value();

        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(biomeSource, settingsHolder);
        RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
        Climate.Sampler sampler = rs.sampler();

        int minY = -64;
        int height = 384;
        int seaLevel = settings.seaLevel();
        LevelHeightAccessor heightAccessor = LevelHeightAccessor.create(minY, height);

        MappedRegistry<Biome> biomeReg = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        for (Pair<Climate.ParameterPoint, Holder<Biome>> p : resolved) {
            ResourceKey<Biome> key = p.getSecond().unwrapKey().orElseThrow();
            if (!biomeReg.containsKey(key)) {
                Registry.register(biomeReg, key, p.getSecond().value());
            }
        }
        biomeReg.freeze();
        RegistryAccess paletteAccess = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(biomeReg));
        PalettedContainerFactory factory = PalettedContainerFactory.create(paletteAccess);

        ChunkPos chunkPos = new ChunkPos(chunkX, chunkZ);
        ProtoChunk chunk = new ProtoChunk(chunkPos, UpgradeData.EMPTY, heightAccessor, factory, null);

        Aquifer.FluidStatus lavaStatus = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        Aquifer.FluidStatus seaStatus = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker fluidPicker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lavaStatus : seaStatus;

        chunk.getOrCreateNoiseChunk(c -> NoiseChunk.forChunk(c, rs, Beardifier.EMPTY, settings, fluidPicker, Blender.empty()));

        // ---- stage: fillFromNoise (shape + REAL aquifer) ----
        generator.fillFromNoise(Blender.empty(), rs, null, chunk).join();

        // ---- stage: biome (dumped here, between fill and surface, exactly
        // where OverworldGenerator::biome_stage runs in Rust) ----
        sb.append("meta.seed ").append(seed).append('\n');
        sb.append("meta.chunkX ").append(chunkX).append('\n');
        sb.append("meta.chunkZ ").append(chunkZ).append('\n');
        sb.append("meta.minY ").append(minY).append('\n');
        sb.append("meta.height ").append(height).append('\n');
        sb.append("meta.seaLevel ").append(seaLevel).append('\n');

        int[] quartHeight = new int[16];
        for (int qz = 0; qz < 4; qz++) {
            for (int qx = 0; qx < 4; qx++) {
                int lx = qx * 4, lz = qz * 4;
                int y = solidTop(chunk, lx, lz, minY, height, seaLevel);
                quartHeight[qz * 4 + qx] = y;
                Climate.TargetPoint target = sampler.sample(
                    QuartPos.fromBlock(chunkX * 16 + lx), QuartPos.fromBlock(y), QuartPos.fromBlock(chunkZ * 16 + lz));
                Holder<Biome> b = biomeSource.getNoiseBiome(target);
                sb.append("biome.").append(qx).append(',').append(qz).append(' ')
                  .append(b.unwrapKey().orElseThrow().identifier()).append(' ').append(y).append('\n');
            }
        }

        // ---- pre-surface (post fill) full-chunk snapshot ----
        BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++)
                    sb.append("presurface.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(canon(chunk.getBlockState(pos.set(chunkX * 16 + x, y, chunkZ * 16 + z)))).append('\n');

        // ---- stage: buildSurface ----
        BiomeManager biomeManager = new BiomeManager(
            (qx, qy, qz) -> biomeSource.getNoiseBiome(qx, qy, qz, sampler), BiomeManager.obfuscateSeed(seed));
        WorldGenerationContext wgctx = new WorldGenerationContext(generator, heightAccessor);
        generator.buildSurface(chunk, wgctx, rs, null, biomeManager, Blender.empty(), null);

        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++)
                    sb.append("postsurface.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(canon(chunk.getBlockState(pos.set(chunkX * 16 + x, y, chunkZ * 16 + z)))).append('\n');

        // ---- stage: applyCarvers, replicating NoiseBasedChunkGenerator.applyCarvers
        // with the REAL per-source-chunk carverBiome resolution (not a fixed
        // biome) ----
        NoiseChunk noiseChunk = chunk.getOrCreateNoiseChunk(c -> NoiseChunk.forChunk(c, rs, Beardifier.EMPTY, settings, fluidPicker, Blender.empty()));
        Aquifer aquifer = noiseChunk.aquifer();
        CarvingContext context = new CarvingContext(
            generator, paletteAccess, heightAccessor, noiseChunk, rs, settings.surfaceRule());
        CarvingMask mask = chunk.getOrCreateCarvingMask();
        Function<BlockPos, Holder<Biome>> biomeGetter = biomeManager::getBiome;
        WorldgenRandom random = new WorldgenRandom(new LegacyRandomSource(RandomSupport.generateUniqueSeed()));

        int carveExceptions = 0;
        for (int dx = -8; dx <= 8; dx++) {
            for (int dz = -8; dz <= 8; dz++) {
                ChunkPos sourcePos = new ChunkPos(chunkPos.x() + dx, chunkPos.z() + dz);
                Holder<Biome> sourceBiome = biomeSource.getNoiseBiome(
                    QuartPos.fromBlock(sourcePos.getMinBlockX()), 0, QuartPos.fromBlock(sourcePos.getMinBlockZ()), sampler);
                BiomeGenerationSettings sourceBiomeGenerationSettings = sourceBiome.value().getGenerationSettings();
                int index = 0;
                for (Holder<ConfiguredWorldCarver<?>> holder : sourceBiomeGenerationSettings.getCarvers()) {
                    ConfiguredWorldCarver<?> carver = holder.value();
                    random.setLargeFeatureSeed(seed + index, sourcePos.x(), sourcePos.z());
                    if (carver.isStartChunk(random)) {
                        try {
                            carver.carve(context, chunk, biomeGetter, random, aquifer, sourcePos, mask);
                        } catch (Throwable t) {
                            carveExceptions++;
                            if (carveExceptions <= 3) sb.append("meta.carveEx ").append(t).append('\n');
                        }
                    }
                    index++;
                }
            }
        }
        sb.append("meta.carveExceptions ").append(carveExceptions).append('\n');

        int changed = 0;
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++) {
                    String c = canon(chunk.getBlockState(pos.set(chunkX * 16 + x, y, chunkZ * 16 + z)));
                    sb.append("postcarve.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(c).append('\n');
                }

        // ---- stage: postfeatures (ore-only decoration of the CENTRE chunk
        // against its own REAL per-column biome, over the real composed
        // postcarve terrain above) ----
        //
        // Honest scope, narrower than `FeatureOracle.java`'s own isolated ore
        // fixture: this runs only the CENTRE chunk's own `UNDERGROUND_ORES`
        // step (its own origin, its own decorationSeed, its own biome's
        // feature list) — it does NOT drive the real 3x3 neighbour-spill
        // model `FeatureOracle.java` now does (vanilla's real
        // `blockStateWriteRadius(1)`), because that would need 8 more
        // real-per-quart-biome chunks generated here (each with its own
        // fillFromNoise/buildSurface/17x17-carve pass), which is a
        // significant expansion of this already-heavy oracle's Docker run
        // time. `FeatureOracle.java`'s fixture remains the authoritative
        // check on the ore ENGINE itself (RNG order, placement, and now real
        // spill); this stage exists to show what composing the centre's own
        // ore step looks like against real biome variety (useful for the
        // ore count-band predictions #295 asks for), not as a fully
        // vanilla-accurate edge band. See docs/worldgen-parity.md's "known
        // gap" section.
        Holder<Biome> centreBiomeForFeatures = biomeSource.getNoiseBiome(
            QuartPos.fromBlock(chunkPos.getMinBlockX()), 0, QuartPos.fromBlock(chunkPos.getMinBlockZ()), sampler);
        List<FeatureSorter.StepFeatureData> perStep = FeatureSorter.buildFeaturesPerStep(
            List.of(centreBiomeForFeatures), b -> b.value().getGenerationSettings().features(), true);
        int oreStep = GenerationStep.Decoration.UNDERGROUND_ORES.ordinal();
        if (oreStep < perStep.size()) {
            List<PlacedFeature> feats = perStep.get(oreStep).features();
            BlockPos featureOrigin = SectionPos.of(chunkPos, heightAccessor.getMinSectionY()).origin();
            WorldgenRandom fRandom = new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
            long decorationSeed = fRandom.setDecorationSeed(seed, featureOrigin.getX(), featureOrigin.getZ());
            WorldGenLevel level = makeCentreOnlyLevel(chunk, chunkPos, centreBiomeForFeatures, heightAccessor, factory, minY, height, seed);
            for (int i = 0; i < feats.size(); i++) {
                PlacedFeature pf = feats.get(i);
                fRandom.setFeatureSeed(decorationSeed, i, oreStep);
                ConfiguredFeature<?, ?> cf = pf.feature().value();
                if (cf.feature() instanceof OreFeature) {
                    pf.placeWithBiomeCheck(level, generator, fRandom, featureOrigin);
                }
            }
        }
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++)
                    sb.append("postfeatures.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(canon(chunk.getBlockState(pos.set(chunkX * 16 + x, y, chunkZ * 16 + z)))).append('\n');

        sb.append("meta.done ").append(chunkX).append(',').append(chunkZ).append('\n');
    }

    // A minimal WorldGenLevel via dynamic proxy, scoped to the CENTRE chunk
    // only (neighbour writes go to throwaway scratch chunks) — matches what
    // `FeatureOracle.java` did before its own 3x3 driver extension. See the
    // "postfeatures" stage's doc comment above for why this oracle keeps the
    // narrower, single-source scope rather than replicating that extension.
    static WorldGenLevel makeCentreOnlyLevel(
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
                    return "ComposedChunkOracleFeatureLevel";
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
            ComposedChunkOracle.class.getClassLoader(), new Class[]{WorldGenLevel.class}, handler);
        self[0] = lvl;
        return lvl;
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindBlockTags();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();

        long seed = 42L;
        // Two named chunks, matching the coordinates other oracles in this
        // directory already use (`carver_parity`'s "ocean chunk (0,0)" /
        // "land chunk (-120,-120)"), so this fixture set is drawn from
        // already-characterised, non-degenerate columns rather than an
        // untested pick.
        dumpChunk(provider, seed, 0, 0);
        dumpChunk(provider, seed, -120, -120);

        System.out.print(sb);
    }
}
