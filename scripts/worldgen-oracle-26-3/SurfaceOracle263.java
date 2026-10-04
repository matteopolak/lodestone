// Independent JVM oracle for 26.3 surface building. It drives the real server's
// fill step (reflectively) and the real surface system over a proto chunk, with a
// synthetic biome source that both this class and the Rust test implement from
// surface-biomes.txt, so every biome-dependent rule is exercised.
//
//   args: states
//         <seed> <settings> <chunkMinY> <chunkHeight> <chunkX> <chunkZ> [<chunkX> <chunkZ> ...]
// `states` prints `state <dataKey> <fullKey>` for every block state the rules can write.
// Per chunk:
//   surface <settings> <seed> <cx> <cz> blocks <hash> counts <k=v;...> heights <hash> post <hash> <n>
// After the seed is known, one line `zoom <seed> <obfuscated>` is printed first.
import com.google.gson.*;
import com.mojang.serialization.JsonOps;
import java.lang.reflect.Method;
import java.nio.file.*;
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
import net.minecraft.world.level.chunk.UpgradeData;

public final class SurfaceOracle263 {
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

    static String dataKey(JsonElement e) {
        if (e.isJsonPrimitive()) {
            String n = e.getAsString();
            return n.contains(":") ? n : "minecraft:" + n;
        }
        JsonObject o = e.getAsJsonObject();
        String n = o.get("Name").getAsString();
        if (!n.contains(":")) n = "minecraft:" + n;
        if (!o.has("Properties")) return n;
        TreeMap<String, String> props = new TreeMap<>();
        for (var en : o.getAsJsonObject("Properties").entrySet()) props.put(en.getKey(), en.getValue().getAsString());
        if (props.isEmpty()) return n;
        return n + props.entrySet().stream().map(en -> en.getKey() + "=" + en.getValue()).collect(Collectors.joining(",", "[", "]"));
    }

    static void collectStates(JsonElement e, Map<String, JsonElement> out) {
        if (e.isJsonObject()) {
            for (var en : e.getAsJsonObject().entrySet()) {
                String k = en.getKey();
                if (k.equals("result_state") || k.equals("ore_block") || k.equals("raw_ore_block") || k.equals("filler_block")) out.put(dataKey(en.getValue()), en.getValue());
                else collectStates(en.getValue(), out);
            }
        } else if (e.isJsonArray()) for (JsonElement c : e.getAsJsonArray()) collectStates(c, out);
    }

    static void printStates() throws Exception {
        Map<String, JsonElement> states = new TreeMap<>();
        try (Stream<Path> files = Files.walk(Path.of("/mc/src/data/minecraft/worldgen/material_rule"))) {
            for (Path f : files.filter(p -> p.toString().endsWith(".json")).collect(Collectors.toList()))
                collectStates(JsonParser.parseString(Files.readString(f)), states);
        }
        for (String n : List.of("air", "water", "lava", "stone", "netherrack", "end_stone", "snow_block", "packed_ice", "terracotta",
                "white_terracotta", "orange_terracotta", "yellow_terracotta", "brown_terracotta", "red_terracotta", "light_gray_terracotta"))
            states.put("minecraft:" + n, new JsonPrimitive("minecraft:" + n));
        for (var e : states.entrySet()) {
            BlockState s = BlockState.CODEC.parse(JsonOps.INSTANCE, e.getValue()).getOrThrow();
            OUT.println("state " + e.getKey() + " " + fullKey(s));
        }
    }

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

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        if (args[0].equals("states")) { printStates(); OUT.flush(); return; }
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
        List<Holder<Biome>> biomes = new ArrayList<>();
        for (String line : Files.readAllLines(Path.of("/oracle/surface-biomes.txt")))
            if (!line.isBlank()) biomes.add(biomeLookup.getOrThrow(ResourceKey.create(Registries.BIOME, Identifier.withDefaultNamespace(line.trim()))));
        IdMapper<Holder<Biome>> idMap = new IdMapper<>();
        biomeLookup.listElements().forEach(idMap::add);
        Holder<Biome> plains = biomeLookup.getOrThrow(Biomes.PLAINS);
        PalettedContainerFactory pcf = new PalettedContainerFactory(
            Strategy.createForBlockStates(Block.BLOCK_STATE_REGISTRY), Blocks.AIR.defaultBlockState(), null,
            Strategy.createForBiomes(idMap), plains, null);

        long zoomSeed = BiomeManager.obfuscateSeed(seed);
        OUT.println("zoom " + seed + " " + zoomSeed);
        BiomeResolver resolver = (qx, qy, qz) -> biomes.get(pick(qx, qy, qz, biomes.size()));
        BiomeManager biomeManager = new BiomeManager(resolver, zoomSeed);

        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(new FixedBiomeSource(plains), settingsHolder);
        Method doFill = NoiseBasedChunkGenerator.class.getDeclaredMethod("doFill", NoiseChunk.class, net.minecraft.world.level.chunk.ChunkAccess.class);
        doFill.setAccessible(true);

        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus lava = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        Aquifer.FluidStatus sea = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker picker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lava : sea;
        Map<BlockState, Long> keyHash = new IdentityHashMap<>();
        Map<BlockState, String> keyName = new IdentityHashMap<>();

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
            }
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
            OUT.println("surface " + name + " " + seed + " " + cx + " " + cz + " blocks " + Long.toHexString(h)
                + " counts " + counts.entrySet().stream().map(e -> e.getKey() + "=" + e.getValue()).collect(Collectors.joining(";"))
                + " heights " + Long.toHexString(hh) + " post " + Long.toHexString(ph) + " " + total);
            OUT.flush();
        }
        OUT.flush();
    }
}
