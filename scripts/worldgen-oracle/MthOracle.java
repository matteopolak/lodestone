// Independent JVM oracle for net.minecraft.util.Mth trig + value helpers used
// by the carvers. Does NOT copy Mojang source; it reads the game's own SIN
// lookup table (via reflection) and calls the real Mth.sin/cos/randomBetween so
// the Rust re-implementation can be diffed bit-for-bit.
//
// Output: one "key value" line per probe. Floats are raw IEEE-754 bit patterns
// (hex) so comparison is exact.
import java.lang.reflect.Field;
import net.minecraft.util.Mth;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;

public final class MthOracle {
    static StringBuilder sb = new StringBuilder();
    static void pf(String k, float v){ sb.append(k).append(' ').append(Integer.toHexString(Float.floatToRawIntBits(v))).append('\n'); }
    static void pi(String k, int v){ sb.append(k).append(' ').append(v).append('\n'); }

    public static void main(String[] args) throws Exception {
        // 1) The full 65536-entry SIN table, read straight from the field.
        Field f = Mth.class.getDeclaredField("SIN");
        f.setAccessible(true);
        float[] sin = (float[]) f.get(null);
        pi("sin.len", sin.length);
        for (int i = 0; i < sin.length; i++) {
            pf("sin." + i, sin[i]);
        }

        // 2) Mth.sin / Mth.cos over a dense, carver-relevant sweep of doubles:
        //    angles from tunnel rotation (nextFloat*2π), vertical rotation
        //    (small), step*π/dist, plus negatives and large magnitudes.
        double[] samples = new double[0];
        java.util.ArrayList<Double> ds = new java.util.ArrayList<>();
        for (int i = -2000; i <= 2000; i++) ds.add(i * 0.01);            // -20..20 step .01
        for (int i = 0; i <= 628; i++) ds.add(i * 0.01);                 // 0..2π fine
        double[] extra = { 0.0, -0.0, Math.PI, -Math.PI, Math.PI/2, -Math.PI/2,
                           2*Math.PI, 100.5, -100.5, 6.2831853, 1e-7, -1e-7,
                           0.12345678, 3.14159265358979, 12345.6789 };
        for (double d : extra) ds.add(d);
        for (int idx = 0; idx < ds.size(); idx++) {
            double d = ds.get(idx);
            pf("msin." + idx, Mth.sin(d));
            pf("mcos." + idx, Mth.cos(d));
        }
        pi("msamples.len", ds.size());
        // Echo the sample inputs as raw double bits so Rust feeds identical d.
        for (int idx = 0; idx < ds.size(); idx++) {
            sb.append("din." + idx).append(' ')
              .append(Long.toHexString(Double.doubleToRawLongBits(ds.get(idx)))).append('\n');
        }

        // 3) randomBetween / randomBetweenInclusive draw sequences.
        long[] seeds = { 0L, 42L, 1234567890123L, -8823894646L };
        for (long seed : seeds) {
            RandomSource r = new LegacyRandomSource(seed);
            for (int i = 0; i < 8; i++) pf("rb[" + seed + "]." + i, Mth.randomBetween(r, 0.75F, 1.4F));
            for (int i = 0; i < 8; i++) pi("rbi[" + seed + "]." + i, Mth.randomBetweenInclusive(r, 10, 67));
            for (int i = 0; i < 4; i++) pf("abs[" + seed + "]." + i, Mth.abs(r.nextFloat() - 0.5F));
        }

        System.out.print(sb);
    }
}
