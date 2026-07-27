// Isolated carver oracle: runs vanilla's own doFill + buildSurface + applyCarvers
// over a whole chunk, dumping the post-carve column block-for-block, plus a
// per-carver draw-count probe (the outer WorldgenRandom's next nextLong() after
// each carver is handled — a single i64 that diverges the instant a carver
// consumes a different NUMBER of outer draws than vanilla).
//
// The carve INPUT (post-surface column) is NOT re-dumped here; the Rust test
// reuses the matching surface_*_jvm.txt fixture's post.* by name and asserts the
// chunk coords agree. Biome pinned to plains via FixedBiomeSource, seed 42,
// real 26.2 registries. No Mojang source is copied — this only drives compiled
// classes and reads their output.
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;
import java.util.function.Function;
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
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.Biomes;
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
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.carver.CarvingContext;
import net.minecraft.world.level.levelgen.carver.ConfiguredWorldCarver;
import com.mojang.serialization.Lifecycle;

public final class CarverOracle {
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

    // Bind block tags (e.g. #overworld_carver_replaceables) onto the frozen
    // BuiltInRegistries.BLOCK, exactly as a datapack reload does. Without this,
    // canReplaceBlock is always false and carvers write nothing.
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
        long seed = 42L;

        String rawArgs = System.getenv("ORACLE_ARGS");
        if (rawArgs == null) rawArgs = "";
        rawArgs = rawArgs.trim();
        String biomeName = "minecraft:plains";
        int chunkX = 0, chunkZ = 0;
        if (!rawArgs.isBlank()) {
            String[] toks = rawArgs.split("\\s+");
            if (toks.length >= 1 && !toks[0].isBlank()) biomeName = toks[0].trim();
            if (toks.length >= 3) {
                chunkX = Integer.parseInt(toks[1].trim());
                chunkZ = Integer.parseInt(toks[2].trim());
            }
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

        // ---- applyCarvers, replicating NoiseBasedChunkGenerator.applyCarvers ----
        Aquifer aquifer = noiseChunk.aquifer();
        CarvingContext context = new CarvingContext(
            generator, paletteAccess, heightAccessor, noiseChunk, rs, settings.surfaceRule());
        CarvingMask mask = chunk.getOrCreateCarvingMask();
        Function<BlockPos, Holder<Biome>> biomeGetter = p -> biomeHolder;
        WorldgenRandom random = new WorldgenRandom(new LegacyRandomSource(RandomSupport.generateUniqueSeed()));

        // Diagnostic: is the overworld_carver_replaceables block tag bound?
        sb.append("meta.stoneIsReplaceable ")
          .append(Blocks.STONE.defaultBlockState().is(net.minecraft.tags.BlockTags.OVERWORLD_CARVER_REPLACEABLES))
          .append('\n');

        // Pre-carve snapshot for a change count (diagnostic anti-vacuity).
        BlockPos.MutableBlockPos snapPos = new BlockPos.MutableBlockPos();
        String[] snap = new String[16 * 16 * height];
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++)
                    snap[x + z * 16 + (y - minY) * 256] = canon(chunk.getBlockState(snapPos.set(x, y, z)));

        int carveExceptions = 0;
        for (int dx = -8; dx <= 8; dx++) {
            for (int dz = -8; dz <= 8; dz++) {
                ChunkPos sourcePos = new ChunkPos(chunkPos.x() + dx, chunkPos.z() + dz);
                int index = 0;
                for (Holder<ConfiguredWorldCarver<?>> holder : biomeHolder.value().getGenerationSettings().getCarvers()) {
                    ConfiguredWorldCarver<?> carver = holder.value();
                    random.setLargeFeatureSeed(seed + index, sourcePos.x(), sourcePos.z());
                    boolean started = carver.isStartChunk(random);
                    if (started) {
                        try {
                            carver.carve(context, chunk, biomeGetter, random, aquifer, sourcePos, mask);
                        } catch (Throwable t) {
                            carveExceptions++;
                            if (carveExceptions <= 3) sb.append("meta.carveEx ").append(t).append('\n');
                        }
                    }
                    sb.append("start.").append(dx).append(',').append(dz).append(',').append(index)
                      .append(' ').append(started ? 1 : 0).append('\n');
                    sb.append("probe.").append(dx).append(',').append(dz).append(',').append(index)
                      .append(' ').append(random.nextLong()).append('\n');
                    index++;
                }
            }
        }

        BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        int changed = 0;
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = minY; y < minY + height; y++) {
                    BlockState s = chunk.getBlockState(pos.set(x, y, z));
                    String c = canon(s);
                    if (!c.equals(snap[x + z * 16 + (y - minY) * 256])) changed++;
                    sb.append("carve.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(c).append('\n');
                }
        sb.append("meta.changed ").append(changed).append('\n');
        sb.append("meta.carveExceptions ").append(carveExceptions).append('\n');

        sb.append("meta.biome ").append(biomeName).append('\n');
        sb.append("meta.chunkX ").append(chunkX).append('\n');
        sb.append("meta.chunkZ ").append(chunkZ).append('\n');
        sb.append("meta.minY ").append(minY).append('\n');
        sb.append("meta.height ").append(height).append('\n');
        sb.append("meta.seaLevel ").append(seaLevel).append('\n');
        sb.append("meta.seed ").append(seed).append('\n');
        sb.append("meta.wayBelowMinY ").append(DimensionType.WAY_BELOW_MIN_Y).append('\n');

        System.out.print(sb);
    }
}
