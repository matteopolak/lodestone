// Independent JVM oracle for Minecraft worldgen NOISE primitives.
//
// Calls the real 26.2 game classes' public APIs (ImprovedNoise, PerlinNoise,
// NormalNoise) to dump ground-truth values. No Mojang source is copied; the
// Rust re-implementation is written from the documented algorithms and diffed
// element-wise against these dumps. Doubles are dumped as raw IEEE-754 bits.
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.synth.ImprovedNoise;
import net.minecraft.world.level.levelgen.synth.PerlinNoise;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

public final class NoiseOracle {
    static StringBuilder sb = new StringBuilder();
    static void pd(String k, double v){ sb.append(k).append(' ').append(Long.toHexString(Double.doubleToRawLongBits(v))).append('\n'); }

    // Sample coordinates covering origin, fractional, negative, and large values.
    static final double[][] PTS = {
        {0,0,0}, {1.5,2.5,3.5}, {-4.25,10.0,-7.75}, {123.456,-64.0,987.654},
        {0.1,0.2,0.3}, {-1000.5,32.0,2000.25}, {33554431.0,0.0,-33554433.0},
        {16.0,-48.0,16.0}, {-0.5,-0.5,-0.5}, {50000.5, 128.0, -50000.5},
    };

    static RandomSource src(boolean xoro, long seed){
        return xoro ? new XoroshiroRandomSource(seed) : new LegacyRandomSource(seed);
    }

    public static void main(String[] args){
        long[] seeds = { 0L, 42L, 1234567890123L, -8823894646L };

        // ImprovedNoise: single octave. Dump offsets + samples.
        for (boolean xoro : new boolean[]{true, false}){
            for (long seed : seeds){
                ImprovedNoise n = new ImprovedNoise(src(xoro, seed));
                String t = "improved["+(xoro?"xoro":"legacy")+","+seed+"]";
                pd(t+".xo", n.xo);
                pd(t+".yo", n.yo);
                pd(t+".zo", n.zo);
                for (int i=0;i<PTS.length;i++){
                    double[] p = PTS[i];
                    pd(t+".noise."+i, n.noise(p[0], p[1], p[2]));
                }
            }
        }

        // Parameter sets: (firstOctave, amplitudes...) drawn from real vanilla
        // noise definitions plus a couple of synthetic shapes.
        Object[][] params = {
            {"temperature", -10, new double[]{1.5, 0.0, 1.0, 0.0, 0.0, 0.0}},
            {"vegetation",  -8,  new double[]{1.0, 1.0, 0.0, 0.0, 0.0, 0.0}},
            {"continentalness", -9, new double[]{1.0,1.0,2.0,2.0,2.0,1.0,1.0,1.0,1.0}},
            {"erosion", -9, new double[]{1.0,1.0,0.0,1.0,1.0}},
            {"ridge", -7, new double[]{1.0,2.0,1.0,0.0,0.0,0.0}},
            {"single", -6, new double[]{1.0}},
            {"aquifer_barrier", -3, new double[]{1.0}},
        };

        // PerlinNoise.create(random, firstOctave, firstAmp, rest...)
        for (boolean xoro : new boolean[]{true, false}){
            for (long seed : seeds){
                for (Object[] pr : params){
                    String name = (String)pr[0];
                    int firstOctave = (Integer)pr[1];
                    double[] amps = (double[])pr[2];
                    double first = amps[0];
                    double[] rest = new double[amps.length-1];
                    System.arraycopy(amps, 1, rest, 0, rest.length);
                    PerlinNoise pn = PerlinNoise.create(src(xoro, seed), firstOctave, first, rest);
                    String t = "perlin["+(xoro?"xoro":"legacy")+","+seed+","+name+"]";
                    for (int i=0;i<PTS.length;i++){
                        double[] p = PTS[i];
                        pd(t+".val."+i, pn.getValue(p[0], p[1], p[2]));
                    }
                }
            }
        }

        // NormalNoise.create(random, firstOctave, amplitudes...)
        for (boolean xoro : new boolean[]{true, false}){
            for (long seed : seeds){
                for (Object[] pr : params){
                    String name = (String)pr[0];
                    int firstOctave = (Integer)pr[1];
                    double[] amps = (double[])pr[2];
                    NormalNoise nn = NormalNoise.create(src(xoro, seed), firstOctave, amps);
                    String t = "normal["+(xoro?"xoro":"legacy")+","+seed+","+name+"]";
                    for (int i=0;i<PTS.length;i++){
                        double[] p = PTS[i];
                        pd(t+".val."+i, nn.getValue(p[0], p[1], p[2]));
                    }
                }
            }
        }

        System.out.print(sb);
    }
}
