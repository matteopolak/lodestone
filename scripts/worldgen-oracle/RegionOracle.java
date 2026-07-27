// Whole-chunk-region oracle for the noise router.
//
// Where DensityOracle samples scattered points across four seeds, this dumps a
// *contiguous* 16x16 chunk footprint so the Rust interpreter can be scored as a
// block-for-block percentage over a whole region (plan §12: whole-corpus
// coverage over spot checks). Seed 42, overworld noise settings, real 26.2
// registries. No Mojang source is copied.
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.core.HolderLookup;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.NoiseRouter;
import net.minecraft.world.level.levelgen.RandomState;

public final class RegionOracle {
    static StringBuilder sb = new StringBuilder();
    static void pd(String k, double v){ sb.append(k).append(' ').append(Long.toHexString(Double.doubleToRawLongBits(v))).append('\n'); }

    // A whole 16x16 chunk footprint (chunk 0,0) and a contiguous vertical band.
    static final int Y_LO = -32;
    static final int Y_HI = 32; // exclusive -> 64 contiguous levels

    static double c(DensityFunction f, int x, int y, int z){
        return f.compute(new DensityFunction.SinglePointContext(x, y, z));
    }

    public static void main(String[] args){
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        long seed = 42L;

        RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
        NoiseRouter r = rs.router();

        for (int x = 0; x < 16; x++) for (int z = 0; z < 16; z++){
            pd("continents."+x+","+z, c(r.continents(), x, 0, z));
            pd("erosion."+x+","+z,    c(r.erosion(), x, 0, z));
            pd("ridges."+x+","+z,     c(r.ridges(), x, 0, z));
            pd("temperature."+x+","+z, c(r.temperature(), x, 0, z));
            pd("vegetation."+x+","+z,  c(r.vegetation(), x, 0, z));
        }
        for (int x = 0; x < 16; x++) for (int y = Y_LO; y < Y_HI; y++) for (int z = 0; z < 16; z++){
            pd("depth."+x+","+y+","+z, c(r.depth(), x, y, z));
            pd("fd."+x+","+y+","+z, c(r.finalDensity(), x, y, z));
        }
        System.out.print(sb);
    }
}
