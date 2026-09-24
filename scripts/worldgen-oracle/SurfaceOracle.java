// Isolated surface-rule oracle: runs vanilla's own doFill + buildSurface over a
// whole chunk column, dumping the *pre-surface* (aquifer-filled) column and the
// *post-surface* column block-for-block, plus the WORLD_SURFACE_WG heightmap.
//
// Biome is pinned via FixedBiomeSource + a fixed BiomeManager so this test is
// decoupled from the (not-yet-built) multi-noise biome source: it exercises the
// surface-rule DSL for whichever single biome is passed as args[0]. Seed 42,
// overworld noise settings, real 26.2 registries. No Mojang source is copied;
// this only *drives* the compiled classes and reads their output.
import java.util.Map;
import java.util.TreeMap;
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
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.dimension.DimensionType;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.blending.Blender;
import com.mojang.serialization.Lifecycle;
import com.mojang.serialization.JsonOps;
import com.google.gson.JsonParser;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.io.FileReader;

public final class SurfaceOracle {
    static final StringBuilder sb = new StringBuilder();

    static <T extends Comparable<T>> String propVal(BlockState s, Property<T> p) {
        return p.getName(s.getValue(p));
    }

    static String canon(BlockState s) {
        StringBuilder b = new StringBuilder(BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString());
        Map<String, String> props = new TreeMap<>();
        for (Property<?> p : s.getProperties()) {
            props.put(p.getName(), propVal(s, p));
        }
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

    // The canonical string for a JSON result_state's *specified* properties only
    // (name + sorted specified [k=v]). This is the key both sides can compute
    // from the raw JSON; the value is the full canon (defaults filled) below.
    static String partialKey(JsonObject rs) {
        StringBuilder b = new StringBuilder(rs.get("Name").getAsString());
        if (rs.has("Properties")) {
            JsonObject props = rs.getAsJsonObject("Properties");
            Map<String, String> m = new TreeMap<>();
            for (String k : props.keySet()) m.put(k, props.get(k).getAsString());
            if (!m.isEmpty()) {
                b.append('[');
                boolean first = true;
                for (Map.Entry<String, String> e : m.entrySet()) {
                    if (!first) b.append(',');
                    first = false;
                    b.append(e.getKey()).append('=').append(e.getValue());
                }
                b.append(']');
            }
        }
        return b.toString();
    }

    static void collectResultStates(JsonElement node, Map<String, JsonObject> out) {
        if (node.isJsonObject()) {
            JsonObject o = node.getAsJsonObject();
            if (o.has("type") && "minecraft:block".equals(o.get("type").getAsString()) && o.has("result_state")) {
                JsonObject rs = o.getAsJsonObject("result_state");
                out.put(partialKey(rs), rs);
            }
            for (String k : o.keySet()) collectResultStates(o.get(k), out);
        } else if (node.isJsonArray()) {
            for (JsonElement e : node.getAsJsonArray()) collectResultStates(e, out);
        }
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
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

        // Minimal biome registry only for the ProtoChunk palette (biomes are never
        // stored: createBiomes is skipped, biomeManager is fixed).
        MappedRegistry<Biome> biomeReg = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        Registry.register(biomeReg, Biomes.PLAINS, provider.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS).value());
        biomeReg.freeze();
        RegistryAccess paletteAccess = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(biomeReg));
        PalettedContainerFactory factory = PalettedContainerFactory.create(paletteAccess);

        ChunkPos chunkPos = new ChunkPos(chunkX, chunkZ);
        ProtoChunk chunk = new ProtoChunk(chunkPos, UpgradeData.EMPTY, heightAccessor, factory, null);

        // Replicate NoiseBasedChunkGenerator.createFluidPicker so we can pre-seed
        // the NoiseChunk with Beardifier.EMPTY (no StructureManager needed).
        Aquifer.FluidStatus lavaStatus = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus seaStatus = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker fluidPicker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lavaStatus : seaStatus;

        chunk.getOrCreateNoiseChunk(c -> NoiseChunk.forChunk(
            c, rs, Beardifier.EMPTY, settings, fluidPicker, Blender.empty()));

        // Fill terrain (density + aquifers). structureManager unused (noiseChunk cached).
        generator.fillFromNoise(Blender.empty(), rs, null, chunk).join();

        BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        int yLo = minY;
        int yHi = minY + height; // exclusive

        // Pre-surface column.
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = yLo; y < yHi; y++) {
                    BlockState s = chunk.getBlockState(pos.set(x, y, z));
                    sb.append("pre.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(canon(s)).append('\n');
                }

        // WORLD_SURFACE_WG heightmap (as read by buildSurface + steep condition).
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++) {
                int h = chunk.getHeight(Heightmap.Types.WORLD_SURFACE_WG, x, z);
                sb.append("hm.").append(x).append(',').append(z).append(' ').append(h).append('\n');
            }

        // buildSurface with a fixed biome manager.
        WorldGenerationContext wgctx = new WorldGenerationContext(generator, heightAccessor);
        BiomeManager biomeManager = new BiomeManager(
            (qx, qy, qz) -> biomeHolder, BiomeManager.obfuscateSeed(seed));
        generator.buildSurface(chunk, wgctx, rs, null, biomeManager, Blender.empty(), null);

        // Post-surface column.
        for (int x = 0; x < 16; x++)
            for (int z = 0; z < 16; z++)
                for (int y = yLo; y < yHi; y++) {
                    BlockState s = chunk.getBlockState(pos.set(x, y, z));
                    sb.append("post.").append(x).append(',').append(y).append(',').append(z)
                      .append(' ').append(canon(s)).append('\n');
                }

        sb.append("meta.biome ").append(biomeName).append('\n');
        sb.append("meta.chunkX ").append(chunkX).append('\n');
        sb.append("meta.chunkZ ").append(chunkZ).append('\n');
        sb.append("meta.minY ").append(minY).append('\n');
        sb.append("meta.height ").append(height).append('\n');
        sb.append("meta.seaLevel ").append(seaLevel).append('\n');
        sb.append("meta.defaultBlock ").append(canon(settings.defaultBlock())).append('\n');
        sb.append("meta.wayBelowMinY ").append(DimensionType.WAY_BELOW_MIN_Y).append('\n');

        // Canonicalisation table: every distinct result_state in the overworld
        // surface rule, decoded via vanilla's own BlockState.CODEC (fills in the
        // default properties) then canon()'d. The Rust side reproduces the
        // partial key from the same JSON and looks up the full canonical form,
        // so it needs no block registry of its own.
        try {
            JsonElement root = JsonParser.parseReader(
                new FileReader("/mc/src/data/minecraft/worldgen/noise_settings/overworld.json"));
            JsonElement surfaceRule = root.getAsJsonObject().get("surface_rule");
            Map<String, JsonObject> results = new TreeMap<>();
            collectResultStates(surfaceRule, results);
            for (Map.Entry<String, JsonObject> e : results.entrySet()) {
                BlockState st = BlockState.CODEC
                    .parse(JsonOps.INSTANCE, e.getValue())
                    .getOrThrow();
                sb.append("canonmap.").append(e.getKey()).append(' ').append(canon(st)).append('\n');
            }
        } catch (Exception ex) {
            sb.append("meta.canonmapError ").append(ex).append('\n');
        }

        System.out.print(sb);
    }
}
