// Independent JVM oracle for the density-function / noise-router system.
//
// Bootstraps the real 26.2 vanilla registries, builds a RandomState for the
// overworld noise settings at fixed seeds, and dumps router outputs
// (final_density and the climate channels) at sample block positions. No Mojang
// source is copied; the Rust interpreter is written from the data grammar and
// diffed element-wise against these raw-bit dumps.
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.core.HolderLookup;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.NoiseRouter;
import net.minecraft.world.level.levelgen.RandomState;

public final class DensityOracle {
    static StringBuilder sb = new StringBuilder();
    static void pd(String k, double v){ sb.append(k).append(' ').append(Long.toHexString(Double.doubleToRawLongBits(v))).append('\n'); }

    static final int[] XS = {0, 1, 4, 7, 16, -13, 100, -400};
    static final int[] YS = {-64, -32, 0, 40, 63, 80, 120, 200, 319};
    static final int[] ZS = {0, 5, -20, 37, 200};

    static double c(DensityFunction f, int x, int y, int z){
        return f.compute(new DensityFunction.SinglePointContext(x, y, z));
    }

    public static void main(String[] args){
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        long[] seeds = { 0L, 42L, 1234567890123L, -8823894646L };

        for (long seed : seeds){
            RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
            NoiseRouter r = rs.router();
            String t = "ow["+seed+"]";
            // Climate channels: sample across x/z (and a mid y).
            for (int x : XS) for (int z : ZS){
                pd(t+".continents."+x+","+z, c(r.continents(), x, 0, z));
                pd(t+".erosion."+x+","+z,    c(r.erosion(), x, 0, z));
                pd(t+".ridges."+x+","+z,     c(r.ridges(), x, 0, z));
                pd(t+".temperature."+x+","+z, c(r.temperature(), x, 0, z));
                pd(t+".vegetation."+x+","+z,  c(r.vegetation(), x, 0, z));
            }
            // Depth + final_density: full 3D.
            for (int x : XS) for (int y : YS) for (int z : ZS){
                pd(t+".depth."+x+","+y+","+z, c(r.depth(), x, y, z));
                pd(t+".finalDensity."+x+","+y+","+z, c(r.finalDensity(), x, y, z));
                pd(t+".barrier."+x+","+y+","+z, c(r.barrierNoise(), x, y, z));
            }
        }
        System.out.print(sb);
    }
}
