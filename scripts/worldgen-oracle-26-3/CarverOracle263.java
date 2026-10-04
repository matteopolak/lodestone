// Independent JVM oracle for 26.3 carving: the real fill, surface and carver steps
// over the real biome source (multi-noise overworld/nether, the end's islands), driven
// reflectively like SurfaceOracle263.
//
//   args: <seed> <settings> <chunkMinY> <chunkHeight> <chunkX> <chunkZ> [<chunkX> <chunkZ> ...]
// Per chunk, two lines (hashes as in SurfaceOracle263), after surface and after carving:
//   <stage> <settings> <seed> <cx> <cz> blocks <hash> counts <k=v;...> heights <hash> post <hash> <n>
// The biome source keeps a last-result hint, so chunks run in argument order and the
// Rust side replays that order with one cursor.
import com.mojang.datafixers.util.Pair;
import java.lang.reflect.Method;
import java.util.*;
import java.util.stream.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;

public final class CarverOracle263 {
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

    static final Map<BlockState, Long> keyHash = new IdentityHashMap<>();
    static final Map<BlockState, String> keyName = new IdentityHashMap<>();

    static String report(String name, long seed, ProtoChunk chunk, int cx, int cz, int chunkMinY, int chunkHeight) {
        long h = 0xcbf29ce484222325L;
        TreeMap<String, Integer> counts = new TreeMap<>();
        BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) for (int y = chunkMinY; y < chunkMinY + chunkHeight; y++) {
            BlockState s = chunk.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z));
            Long kh = keyHash.get(s);
            if (kh == null) { String k = fullKey(s); kh = fnv(k); keyHash.put(s, kh); keyName.put(s, k); }
            h = (h ^ kh) * 0x100000001b3L;
            counts.merge(keyName.get(s), 1, Integer::sum);
        }
        long hh = 0xcbf29ce484222325L;
        for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
            int ht = chunk.getHeight(Heightmap.Types.WORLD_SURFACE_WG, x, z);
            for (int k = 0; k < 4; k++) { hh ^= (ht >>> (8 * k)) & 0xFF; hh *= 0x100000001b3L; }
        }
        long ph = 0xcbf29ce484222325L;
        int total = 0;
        var post = chunk.getPostProcessing();
        for (int i = 0; i < post.length; i++) {
            if (post[i] == null) continue;
            for (int k = 0; k < 4; k++) { ph ^= (i >>> (8 * k)) & 0xFF; ph *= 0x100000001b3L; }
            for (short v : post[i]) {
                ph ^= v & 0xFF; ph *= 0x100000001b3L;
                ph ^= (v >>> 8) & 0xFF; ph *= 0x100000001b3L;
                total++;
            }
        }
        return name + " " + seed + " " + cx + " " + cz + " blocks " + Long.toHexString(h)
            + " counts " + counts.entrySet().stream().map(e -> e.getKey() + "=" + e.getValue()).collect(Collectors.joining(";"))
            + " heights " + Long.toHexString(hh) + " post " + Long.toHexString(ph) + " " + total;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        // Bootstrapping alone leaves block tags unbound, so every tag test is false; the real
        // game binds them from data. Carving only consults one block tag, so bind that one.
        {
            Holder.Reference<Block> bedrock = Blocks.BEDROCK.builtInRegistryHolder();
            Method bind = Holder.Reference.class.getDeclaredMethod("bindTags", Collection.class);
            bind.setAccessible(true);
            bind.invoke(bedrock, List.of(net.minecraft.tags.BlockTags.UNCARVABLE));
        }
        HolderLookup.Provider provider = VanillaRegistries.createWorldLookup();
        long seed = Long.parseLong(args[0]);
        String name = args[1];
        int chunkMinY = Integer.parseInt(args[2]);
        int chunkHeight = Integer.parseInt(args[3]);
        Holder<NoiseGeneratorSettings> settingsHolder = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(name)));
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
        if (name.equals("end")) {
            source = TheEndBiomeSource.create(biomeLookup);
        } else {
            var preset = MultiNoiseBiomeSourceParameterList.knownPresets().entrySet().stream()
                .filter(e -> e.getKey().id().getPath().equals(name)).findFirst().orElseThrow().getValue();
            List<Pair<Climate.ParameterPoint, Holder<Biome>>> rows = new ArrayList<>();
            for (var p : preset.values()) rows.add(Pair.of(p.getFirst(), biomeLookup.getOrThrow(p.getSecond())));
            source = MultiNoiseBiomeSource.createFromList(new Climate.ParameterList<>(rows));
        }
        BiomeManager biomeManager = new BiomeManager(source.createUncachedResolver(rs), BiomeManager.obfuscateSeed(seed));

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

        for (int a = 4; a + 1 < args.length; a += 2) {
            int cx = Integer.parseInt(args[a]);
            int cz = Integer.parseInt(args[a + 1]);
            LevelHeightAccessor access = LevelHeightAccessor.create(chunkMinY, chunkHeight);
            ProtoChunk chunk = new ProtoChunk(new ChunkPos(cx, cz), UpgradeData.EMPTY, access, pcf, null);
            NoiseSettings ns = settings.noiseSettings().clampToHeightAccessor(access);
            DensityVolume volume = new DensityVolume(16, ns.height(), 16, cx * 16, ns.minY(), cz * 16);
            try (NoiseChunk nc = new NoiseChunk(rs, null, settings, picker, Blender.empty(), volume)) {
                doFill.invoke(generator, nc, chunk);
                WorldGenerationContext context = new WorldGenerationContext(generator, access);
                rs.surfaceSystem().buildSurface(rs, biomeManager, context, chunk, nc, settings.materialRule().value(), null);
                OUT.println("surface " + report(name, seed, chunk, cx, cz, chunkMinY, chunkHeight));
                genCarvers.invoke(generator, chunk, Blender.empty(), nc, rs, biomeManager, null, settings.materialRule().value());
                OUT.println("carved " + report(name, seed, chunk, cx, cz, chunkMinY, chunkHeight));
                OUT.flush();
            }
        }
        OUT.flush();
    }
}
