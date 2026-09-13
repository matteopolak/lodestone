// Independent Java oracle for the Minecraft `Mth.SIN` lookup table.
//
// This is NOT decompiled/copied Mojang code. It is a from-scratch Java
// re-implementation of the *documented* table-construction formula
// (`SIN[i] = (float) Math.sin(i / 10430.378350470453)`), written purely to
// obtain ground-truth `float` bit patterns from the real JVM whose IEEE-754
// semantics we are claiming to reproduce in Rust.
//
// It dumps one raw `Float.floatToRawIntBits` value per line (decimal, unsigned),
// so a comparator can diff element-by-element against the checked-in Rust table.
public final class SinOracle {
    private static final double SIN_SCALE = 10430.378350470453;

    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder(65536 * 11);
        for (int i = 0; i < 65536; i++) {
            float v = (float) Math.sin(i / SIN_SCALE);
            int bits = Float.floatToRawIntBits(v);
            // Print as unsigned 32-bit decimal to match Rust's `u32` array.
            sb.append(Integer.toUnsignedLong(bits));
            sb.append('\n');
        }
        System.out.print(sb);
    }
}
