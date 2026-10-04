// Independent JVM oracle for the 26.3 density-function system. It drives the real
// server classes (RandomState, the density-function compiler and its samplers) and
// prints, for every function it is asked about, an FNV-1a hash over the raw float
// bits of its scalar and volume samples, plus the first few raw values so a
// mismatch can be localized. No Lodestone code participates.
//
//   args: <seed> <settings> [verbose <function> <shape>]
//   settings: overworld | amplified | large_biomes | nether | end | caves | floating_islands
//
// Output lines:
//   fn <settings> <seed> <name> scalar <hash> <n> <first raw bits...>
//   fn <settings> <seed> <name> vol<k> <hash> <n> <first raw bits...>
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.densityfunction.*;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

public final class DensityOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false);
    static final int[] XS = {0, 1, 4, 7, 16, -13, 100, -400, 5000, -77777};
    static final int[] YS = {-64, -33, 0, 40, 63, 80, 120, 200, 319};
    static final int[] ZS = {0, 5, -20, 37, 200, 12345};
    // size x/y/z, min x/y/z, step x/y/z
    static final int[][] SHAPES = {
        {3, 5, 3, -5, -3, 7, 1, 1, 1},
        {5, 13, 5, -16, -64, 32, 4, 8, 4},
        {1, 7, 1, 33, 10, -9, 1, 1, 1},
        {5, 1, 5, -20, 0, -20, 4, 1, 4},
        {16, 24, 16, 0, -64, 0, 1, 1, 1},
        {4, 5, 3, 3, -17, 5, 8, 4, 4},
        {17, 49, 17, 1000000, -64, -64000, 1, 1, 1},
    };

    static long fnv(long h, int bits) {
        for (int i = 0; i < 4; i++) { h ^= (bits >>> (8 * i)) & 0xFF; h *= 0x100000001b3L; }
        return h;
    }

    static String report(float[] values, int n) {
        long h = 0xcbf29ce484222325L;
        StringBuilder first = new StringBuilder();
        for (int i = 0; i < n; i++) {
            int b = Float.floatToRawIntBits(values[i]);
            h = fnv(h, b);
            if (i < 6) first.append(' ').append(Integer.toHexString(b));
        }
        return Long.toHexString(h) + " " + n + first;
    }

    static float[] scalar(RandomState rs, DensityFunction f) {
        DensitySampler s = rs.getSampler(f);
        float[] out = new float[XS.length * YS.length * ZS.length];
        int i = 0;
        for (int x : XS) for (int y : YS) for (int z : ZS)
            out[i++] = s.sampleValue(SamplerContext.EMPTY_UNCACHED, x, y, z);
        return out;
    }

    static float[] volume(RandomState rs, DensityFunction f, int[] sh) {
        DensitySampler s = rs.getSampler(f);
        DensityVolume v = new DensityVolume(sh[0], sh[1], sh[2], sh[3], sh[4], sh[5], sh[6], sh[7], sh[8]);
        DensityBuffer buf = DensityBuffer.createUnpooled(v.size());
        s.sampleVolume(SamplerContext.EMPTY_UNCACHED, buf, v);
        float[] out = new float[v.size()];
        for (int i = 0; i < out.length; i++) out[i] = buf.get(i);
        return out;
    }

    static void emit(String tag, RandomState rs, String name, DensityFunction f, String[] verbose) {
        try {
            float[] sc = scalar(rs, f);
            if (verbose == null) OUT.println("fn " + tag + " " + name + " scalar " + report(sc, sc.length));
            else if (verbose[1].equals("scalar")) for (float v : sc) OUT.println(Integer.toHexString(Float.floatToRawIntBits(v)));
        } catch (Throwable t) {
            OUT.println("fn " + tag + " " + name + " scalar ERR " + t.getClass().getSimpleName());
        }
        for (int k = 0; k < SHAPES.length; k++) {
            try {
                float[] vol = volume(rs, f, SHAPES[k]);
                if (verbose == null) OUT.println("fn " + tag + " " + name + " vol" + k + " " + report(vol, vol.length));
                else if (verbose[1].equals("vol" + k)) for (float v : vol) OUT.println(Integer.toHexString(Float.floatToRawIntBits(v)));
            } catch (Throwable t) {
                OUT.println("fn " + tag + " " + name + " vol" + k + " ERR " + t.getClass().getSimpleName());
            }
        }
    }

    static String prefixFor(String settings) {
        return switch (settings) {
            case "overworld" -> "overworld/";
            case "amplified" -> "overworld_amplified/";
            case "large_biomes" -> "overworld_large_biomes/";
            case "nether" -> "nether/";
            case "end" -> "end/";
            default -> "\0";
        };
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createWorldLookup();
        long seed = Long.parseLong(args[0]);
        String settingsName = args[1];
        String[] verbose = args.length > 2 && args[2].equals("verbose") ? new String[]{args[3], args[4]} : null;
        NoiseGeneratorSettings settings = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(settingsName))).value();
        RandomState rs = RandomState.create(provider.lookupOrThrow(Registries.NOISE), seed, settings);
        String tag = settingsName + " " + seed;
        var router = settings.noiseRouter();
        Map<String, DensityFunction> fns = new TreeMap<>();
        fns.put("router.temperature", router.temperature());
        fns.put("router.vegetation", router.vegetation());
        fns.put("router.continents", router.continents());
        fns.put("router.erosion", router.erosion());
        fns.put("router.depth", router.depth());
        fns.put("router.ridges", router.ridges());
        fns.put("router.chunk_surface_level", router.chunkSurfaceLevel());
        fns.put("router.final_density", router.finalDensity());
        settings.aquifers().ifPresent(a -> {
            fns.put("aquifer.barrier", a.barrierNoise());
            fns.put("aquifer.fluid_level_floodedness", a.fluidLevelFloodednessNoise());
            fns.put("aquifer.fluid_level_spread", a.fluidLevelSpreadNoise());
            fns.put("aquifer.lava", a.lavaNoise());
            fns.put("aquifer.exclusion", a.exclusion());
            fns.put("aquifer.surface_level", a.surfaceLevel());
        });
        String prefix = prefixFor(settingsName);
        provider.lookupOrThrow(Registries.DENSITY_FUNCTION).listElements().forEach(ref -> {
            String path = ref.key().identifier().getPath();
            if (path.startsWith(prefix) || !path.contains("/")) fns.put("fn." + path, ref.value());
        });
        for (var e : fns.entrySet()) {
            if (verbose != null && !verbose[0].equals(e.getKey())) continue;
            emit(tag, rs, e.getKey(), e.getValue(), verbose);
        }
        OUT.flush();
    }
}
