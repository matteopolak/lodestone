// Independent JVM oracle for 26.3 biome sources (multi-noise overworld and nether,
// the end's island source) over the real router, parameter lists and search tree.
//
//   chunk  <seed> <settings> <cx> <cz> ...   chunk biomes via the chunk resolver
//          (bulk climate volumes), every section's quarts in generation order
//   scalar <seed> <settings> <qx> <qy> <qz> ...   one 16x16-column quart block per
//          origin (qx, qz) at the given quart y, via the point resolver
// Lines:
//   biomes <mode> <settings> <seed> <a> <b> targets <hash> biomes <hash> n <count> top <name=n;...>
// `targets` hashes the six quantized climate values per cell (multi-noise only; the
// end has no climate target), `biomes` the biome resource path per cell, in the order
// the cells were queried. The search tree keeps a last-result hint, so cells are
// queried in one fixed order and the Rust side must replay that order.
import com.mojang.datafixers.util.Pair;
import java.util.*;
import java.util.stream.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.densityfunction.*;

public final class BiomeSourceOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), true);

    static long mix(long h, long v) {
        for (int k = 0; k < 8; k++) { h ^= (v >>> (8 * k)) & 0xFF; h *= 0x100000001b3L; }
        return h;
    }

    static long mixStr(long h, String s) {
        for (byte b : s.getBytes(java.nio.charset.StandardCharsets.UTF_8)) { h ^= b & 0xFF; h *= 0x100000001b3L; }
        h ^= 0xFF; h *= 0x100000001b3L;
        return h;
    }

    static final class Acc {
        long targets = 0xcbf29ce484222325L, biomes = 0xcbf29ce484222325L;
        int n;
        final TreeMap<String, Integer> top = new TreeMap<>();
        void biome(String name) { biomes = mixStr(biomes, name); n++; top.merge(name, 1, Integer::sum); }
        void target(Climate.TargetPoint t) {
            for (long v : new long[]{t.temperature(), t.humidity(), t.continentalness(), t.erosion(), t.depth(), t.weirdness()}) targets = mix(targets, v);
        }
        String line() {
            return "targets " + Long.toHexString(targets) + " biomes " + Long.toHexString(biomes) + " n " + n + " top "
                + top.entrySet().stream().map(e -> e.getKey() + "=" + e.getValue()).collect(Collectors.joining(";"));
        }
    }

    static String name(Holder<Biome> h) { return h.unwrapKey().orElseThrow().identifier().getPath(); }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createWorldLookup();
        String mode = args[0];
        long seed = Long.parseLong(args[1]);
        String settingsName = args[2];
        NoiseGeneratorSettings settings = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(settingsName))).value();
        RandomState rs = RandomState.create(provider.lookupOrThrow(Registries.NOISE), seed, settings);
        var biomeLookup = provider.lookupOrThrow(Registries.BIOME);

        BiomeSource source;
        if (settingsName.equals("end")) {
            source = TheEndBiomeSource.create(biomeLookup);
        } else {
            var preset = MultiNoiseBiomeSourceParameterList.knownPresets().entrySet().stream()
                .filter(e -> e.getKey().id().getPath().equals(settingsName)).findFirst().orElseThrow().getValue();
            List<Pair<Climate.ParameterPoint, Holder<Biome>>> rows = new ArrayList<>();
            for (var p : preset.values()) rows.add(Pair.of(p.getFirst(), biomeLookup.getOrThrow(p.getSecond())));
            source = MultiNoiseBiomeSource.createFromList(new Climate.ParameterList<>(rows));
        }
        boolean multi = source instanceof MultiNoiseBiomeSource;

        if (mode.equals("chunk")) {
            int minY = settings.noiseSettings().minY(), height = settings.noiseSettings().height();
            for (int a = 3; a + 1 < args.length; a += 2) {
                int cx = Integer.parseInt(args[a]), cz = Integer.parseInt(args[a + 1]);
                int qx0 = cx * 4, qz0 = cz * 4, qy0 = minY >> 2, qh = height >> 2;
                Acc acc = new Acc();
                DensityBufferPool pool = rs.acquireDensityBufferPool();
                Climate.Sampler sampler = rs.createClimateSampler(SamplerContext.builder().enableCaches().useBufferArena(pool).build());
                if (multi) {
                    DensityVolume v = new DensityVolume(4, qh, 4, qx0 * 4, qy0 * 4, qz0 * 4, 4, 4, 4);
                    DensityBuffer[] b = new DensityBuffer[6];
                    for (int i = 0; i < 6; i++) b[i] = DensityBuffer.createUnpooled(v.size());
                    Climate.Sampler s2 = rs.createClimateSampler(SamplerContext.builder().enableCaches().useBufferArena(pool).build());
                    s2.temperature().sampleVolume(b[0], v);
                    s2.humidity().sampleVolume(b[1], v);
                    s2.continentalness().sampleVolume(b[2], v);
                    s2.erosion().sampleVolume(b[3], v);
                    s2.depth().sampleVolume(b[4], v);
                    s2.weirdness().sampleVolume(b[5], v);
                    for (int x = 0; x < 4; x++) for (int z = 0; z < 4; z++) for (int y = 0; y < qh; y++) {
                        int i = v.indexUnchecked(x, y, z);
                        acc.target(Climate.target(b[0].get(i), b[1].get(i), b[2].get(i), b[3].get(i), b[4].get(i), b[5].get(i)));
                    }
                }
                BiomeResolver r = source.createResolverForChunk(sampler, qx0, qy0, qz0, 4, qh, 4);
                for (int sec = 0; sec < qh / 4; sec++)
                    for (int x = 0; x < 4; x++) for (int y = 0; y < 4; y++) for (int z = 0; z < 4; z++)
                        acc.biome(name(r.getNoiseBiome(qx0 + x, qy0 + sec * 4 + y, qz0 + z)));
                rs.releaseDensityBufferPool(pool);
                OUT.println("biomes chunk " + settingsName + " " + seed + " " + cx + " " + cz + " " + acc.line());
            }
        } else {
            Climate.Sampler sampler = rs.createClimateSampler(SamplerContext.EMPTY_UNCACHED);
            BiomeResolver r = source.createResolver(sampler);
            for (int a = 3; a + 2 < args.length; a += 3) {
                int qx0 = Integer.parseInt(args[a]), qy = Integer.parseInt(args[a + 1]), qz0 = Integer.parseInt(args[a + 2]);
                Acc acc = new Acc();
                for (int x = 0; x < 16; x++) for (int z = 0; z < 16; z++) {
                    if (multi) acc.target(sampler.sample(qx0 + x, qy, qz0 + z));
                    acc.biome(name(r.getNoiseBiome(qx0 + x, qy, qz0 + z)));
                }
                OUT.println("biomes scalar " + settingsName + " " + seed + " " + qx0 + " " + qy + " " + qz0 + " " + acc.line());
            }
        }
    }
}
