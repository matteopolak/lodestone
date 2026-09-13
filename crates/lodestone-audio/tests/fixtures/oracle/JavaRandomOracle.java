// Golden-vector generator for java.util.Random, used to validate that
// lodestone_audio::JavaRandom reproduces vanilla's LegacyRandomSource (which is
// java.util.Random) bit-for-bit. The JVM is the external authority here on
// purpose: nothing lodestone wrote is in the oracle chain, so an agreement is
// real evidence rather than two ports sharing an author (plan §12.31).
//
// Regenerate (writes java_random_golden.txt next to this file):
//   docker run --rm -v "$PWD/crates/lodestone-audio/tests/fixtures/oracle:/w" \
//     -w /w eclipse-temurin:25-jdk java JavaRandomOracle.java > java_random_golden.txt
//
// Vanilla chain being modelled (26.2 client-src):
//   EntityBoundSoundInstance(..., seed) -> RandomSource.create(seed)
//     -> new LegacyRandomSource(seed) -> nextInt(totalWeight)   (variant pick)
import java.util.Random;

public class JavaRandomOracle {
    // Seeds: zero, small, negative, a big prime, and packet-seed-shaped values.
    static final long[] SEEDS = {
        0L, 1L, -1L, 42L, 1000000007L, -8675309L,
        1234567890123456789L, -1234567890123456789L
    };
    // Bounds exercised: degenerate 1; powers of two (fast path); small
    // non-powers where the rejection loop and modulo matter; a realistic
    // event-count (~1968 events measured); and Integer.MAX_VALUE, the worst
    // case for the overflow check in the rejection loop.
    static final int[] BOUNDS = {1, 2, 3, 5, 7, 16, 100, 1968, Integer.MAX_VALUE};

    public static void main(String[] args) {
        System.out.println("# java.util.Random golden vectors");
        System.out.println("# format: nextInt <seed> <v0..v15>");
        System.out.println("# format: nextIntBound <seed> <bound> <v0..v15>");
        System.out.println("# format: nextLong <seed> <v0..v7>");
        for (long seed : SEEDS) {
            Random r = new Random(seed);
            StringBuilder sb = new StringBuilder("nextInt ").append(seed);
            for (int i = 0; i < 16; i++) sb.append(' ').append(r.nextInt());
            System.out.println(sb);
        }
        for (long seed : SEEDS) {
            for (int bound : BOUNDS) {
                Random r = new Random(seed);
                StringBuilder sb = new StringBuilder("nextIntBound ")
                    .append(seed).append(' ').append(bound);
                for (int i = 0; i < 16; i++) sb.append(' ').append(r.nextInt(bound));
                System.out.println(sb);
            }
        }
        for (long seed : SEEDS) {
            Random r = new Random(seed);
            StringBuilder sb = new StringBuilder("nextLong ").append(seed);
            for (int i = 0; i < 8; i++) sb.append(' ').append(r.nextLong());
            System.out.println(sb);
        }
    }
}
