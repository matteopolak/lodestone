// Independent JVM oracle for Minecraft worldgen RNG primitives.
//
// This does NOT copy Mojang source. It *calls the real game classes' public
// APIs* to obtain ground-truth outputs (the same approach as ShapeOracle), so
// the Rust re-implementations — written originally from the documented
// algorithms — can be diffed against the values the actual game produces.
//
// Output: one "key value..." line per probe. Doubles/floats are dumped as raw
// IEEE-754 bit patterns (hex) so comparison is exact.
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.PositionalRandomFactory;

public final class RngOracle {
    static StringBuilder sb = new StringBuilder();
    static void p(String k, long v){ sb.append(k).append(' ').append(v).append('\n'); }
    static void pd(String k, double v){ sb.append(k).append(' ').append(Long.toHexString(Double.doubleToRawLongBits(v))).append('\n'); }
    static void pf(String k, float v){ sb.append(k).append(' ').append(Integer.toHexString(Float.floatToRawIntBits(v))).append('\n'); }

    public static void main(String[] args){
        long[] seeds = { 0L, 1L, -1L, 42L, 1234567890123L, 0x0123456789ABCDEFL, -8823894646L };

        // java.util.Random-compatible LCG (LegacyRandomSource)
        for (long seed : seeds){
            RandomSource r = new LegacyRandomSource(seed);
            String t = "legacy["+seed+"]";
            for (int i=0;i<6;i++) p(t+".nextInt."+i, r.nextInt());
            for (int i=0;i<6;i++) p(t+".nextInt100."+i, r.nextInt(100));
            for (int i=0;i<4;i++) p(t+".nextIntPow2."+i, r.nextInt(256));
            for (int i=0;i<4;i++) p(t+".nextIntNonPow."+i, r.nextInt(17));
            for (int i=0;i<4;i++) p(t+".nextLong."+i, r.nextLong());
            for (int i=0;i<4;i++) pf(t+".nextFloat."+i, r.nextFloat());
            for (int i=0;i<4;i++) pd(t+".nextDouble."+i, r.nextDouble());
            for (int i=0;i<6;i++) pd(t+".nextGaussian."+i, r.nextGaussian());
            for (int i=0;i<4;i++) p(t+".nextBoolean."+i, r.nextBoolean()?1:0);
        }

        // RandomSupport.mixStafford13
        long[] zs = { 0L, 1L, -1L, 42L, 0x9E3779B97F4A7C15L, 1234567890123L, -8823894646L };
        for (long z : zs) p("mixStafford13["+z+"]", RandomSupport.mixStafford13(z));

        // XoroshiroRandomSource
        for (long seed : seeds){
            RandomSource r = new XoroshiroRandomSource(seed);
            String t = "xoro["+seed+"]";
            for (int i=0;i<6;i++) p(t+".nextInt."+i, r.nextInt());
            for (int i=0;i<6;i++) p(t+".nextInt100."+i, r.nextInt(100));
            for (int i=0;i<4;i++) p(t+".nextIntPow2."+i, r.nextInt(256));
            for (int i=0;i<4;i++) p(t+".nextIntNonPow."+i, r.nextInt(17));
            for (int i=0;i<4;i++) p(t+".nextLong."+i, r.nextLong());
            for (int i=0;i<4;i++) pf(t+".nextFloat."+i, r.nextFloat());
            for (int i=0;i<4;i++) pd(t+".nextDouble."+i, r.nextDouble());
            for (int i=0;i<6;i++) pd(t+".nextGaussian."+i, r.nextGaussian());
            for (int i=0;i<4;i++) p(t+".nextBoolean."+i, r.nextBoolean()?1:0);
        }

        // Positional factories: forkPositional().at(x,y,z) and fromHashOf(name)
        for (long seed : new long[]{42L, 1234567890123L}){
            for (boolean xoro : new boolean[]{true,false}){
                RandomSource base = xoro ? new XoroshiroRandomSource(seed) : new LegacyRandomSource(seed);
                PositionalRandomFactory pf = base.forkPositional();
                String t = "pos["+(xoro?"xoro":"legacy")+","+seed+"]";
                int[][] pts = {{0,0,0},{1,2,3},{-100,64,250},{16,-32,-16},{1000000,0,-1000000}};
                for (int[] pt : pts){
                    RandomSource r = pf.at(pt[0],pt[1],pt[2]);
                    p(t+".at("+pt[0]+","+pt[1]+","+pt[2]+").nextLong", r.nextLong());
                }
                String[] names = {"minecraft:ore_diamond", "minecraft:aquifer_barrier", "test"};
                for (String nm : names){
                    RandomSource r = pf.fromHashOf(nm);
                    p(t+".fromHashOf("+nm+").nextLong", r.nextLong());
                }
            }
        }

        // WorldgenRandom seed derivations
        for (long seed : new long[]{42L, 1234567890123L, -8823894646L}){
            for (boolean xoro : new boolean[]{true,false}){
                WorldgenRandom wr = new WorldgenRandom(xoro ? new XoroshiroRandomSource(seed) : new LegacyRandomSource(seed));
                String t = "wgr["+(xoro?"xoro":"legacy")+","+seed+"]";
                int[][] chunks = {{0,0},{1,1},{-3,7},{100,-100}};
                for (int[] c : chunks){
                    long ds = wr.setDecorationSeed(seed, c[0]*16, c[1]*16);
                    p(t+".setDecorationSeed("+c[0]*16+","+c[1]*16+")", ds);
                    p(t+".afterDecoration.nextLong", wr.nextLong());
                    wr.setLargeFeatureSeed(seed, c[0], c[1]);
                    p(t+".afterLargeFeature.nextLong", wr.nextLong());
                }
            }
        }

        System.out.print(sb);
    }
}
