// Independent End terrain oracle. It drives the bundled 26.2 server classes
// and prints run-length encoded block columns plus the quart biome grid; no
// Lodestone generation code participates in the result.
import com.mojang.serialization.Lifecycle;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.QuartPos;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.Registries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.biome.TheEndBiomeSource;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.data.registries.VanillaRegistries;

public final class EndChunkOracle {
    static <T extends Comparable<T>> String propertyValue(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }

    static String canonical(BlockState state) {
        StringBuilder out = new StringBuilder(net.minecraft.core.registries.BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        Map<String, String> properties = new TreeMap<>();
        for (Property<?> property : state.getProperties()) properties.put(property.getName(), propertyValue(state, property));
        if (!properties.isEmpty()) {
            out.append('[');
            boolean first = true;
            for (Map.Entry<String, String> property : properties.entrySet()) {
                if (!first) out.append(',');
                first = false;
                out.append(property.getKey()).append('=').append(property.getValue());
            }
            out.append(']');
        }
        return out.toString();
    }

    static void dump(HolderLookup.Provider provider, long seed, int chunkX, int chunkZ) {
        HolderLookup.RegistryLookup<Biome> biomes = provider.lookupOrThrow(Registries.BIOME);
        TheEndBiomeSource biomeSource = TheEndBiomeSource.create(biomes);
        Holder<NoiseGeneratorSettings> settingsHolder = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(NoiseGeneratorSettings.END);
        NoiseGeneratorSettings settings = settingsHolder.value();
        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(biomeSource, settingsHolder);
        RandomState randomState = RandomState.create(provider, NoiseGeneratorSettings.END, seed);
        int minY = settings.noiseSettings().minY();
        int height = settings.noiseSettings().height();
        LevelHeightAccessor heightAccessor = LevelHeightAccessor.create(minY, height);

        MappedRegistry<Biome> biomeRegistry = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        for (var key : List.of(Biomes.PLAINS, Biomes.THE_END, Biomes.END_HIGHLANDS, Biomes.END_MIDLANDS, Biomes.SMALL_END_ISLANDS, Biomes.END_BARRENS)) {
            Holder.Reference<Biome> holder = biomes.getOrThrow(key);
            Registry.register(biomeRegistry, key, holder.value());
        }
        biomeRegistry.freeze();
        PalettedContainerFactory factory = PalettedContainerFactory.create(
            new RegistryAccess.ImmutableRegistryAccess(List.of(biomeRegistry)));
        ChunkPos chunkPos = new ChunkPos(chunkX, chunkZ);
        ProtoChunk chunk = new ProtoChunk(chunkPos, UpgradeData.EMPTY, heightAccessor, factory, null);
        Aquifer.FluidStatus seaStatus = new Aquifer.FluidStatus(settings.seaLevel(), settings.defaultFluid());
        Aquifer.FluidPicker fluidPicker = (x, y, z) -> seaStatus;
        chunk.getOrCreateNoiseChunk(c -> NoiseChunk.forChunk(
            c, randomState, Beardifier.EMPTY, settings, fluidPicker, Blender.empty()));
        generator.fillFromNoise(Blender.empty(), randomState, null, chunk).join();
        generator.buildSurface(
            chunk,
            new WorldGenerationContext(generator, heightAccessor),
            randomState,
            null,
            new BiomeManager((qx, qy, qz) -> biomeSource.getNoiseBiome(qx, qy, qz, randomState.sampler()), BiomeManager.obfuscateSeed(seed)),
            Blender.empty(),
            null);

        BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        System.out.println("case " + seed + " " + chunkX + " " + chunkZ + " " + minY + " " + height);
        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
            int y = minY;
            while (y < minY + height) {
                String state = canonical(chunk.getBlockState(pos.set(chunkPos.getMinBlockX() + x, y, chunkPos.getMinBlockZ() + z)));
                int count = 1;
                while (y + count < minY + height && state.equals(canonical(chunk.getBlockState(pos.set(chunkPos.getMinBlockX() + x, y + count, chunkPos.getMinBlockZ() + z))))) count++;
                System.out.println("run " + x + " " + z + " " + y + " " + count + " " + state);
                y += count;
            }
        }
        for (int qz = 0; qz < 4; qz++) for (int qx = 0; qx < 4; qx++) {
            Holder<Biome> biome = biomeSource.getNoiseBiome(
                QuartPos.fromBlock(chunkPos.getMinBlockX() + qx * 4), 0, QuartPos.fromBlock(chunkPos.getMinBlockZ() + qz * 4), randomState.sampler());
            System.out.println("biome " + qx + " " + qz + " " + biome.unwrapKey().orElseThrow().identifier());
        }
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        dump(provider, -195764831L, 0, 0);
        dump(provider, -195764831L, 65, 0);
        dump(provider, 42L, 400, 400);
    }
}
