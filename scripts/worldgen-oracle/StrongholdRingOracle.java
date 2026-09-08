// Independent concentric-ring fixture extractor. It calls the bundled server's
// public random-source API and emits the first candidate after the preferred
// biome reservoir walk. The Rust test keeps the resulting coordinates as an
// external fixture rather than deriving expectations from its own RNG port.
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;

public final class StrongholdRingOracle {
    public static void main(String[] args) {
        long seed = args.length > 0 ? Long.parseLong(args[0]) : 42L;
        int distance = args.length > 1 ? Integer.parseInt(args[1]) : 1;
        int spread = args.length > 2 ? Integer.parseInt(args[2]) : 1;
        int count = args.length > 3 ? Integer.parseInt(args[3]) : 1;
        RandomSource random = new XoroshiroRandomSource(seed);
        double angle = random.nextDouble() * Math.PI * 2.0;
        int positionInCircle = 0;
        int circle = 0;
        for (int i = 0; i < count; i++) {
            double dist = 4.0 * distance
                + (double) distance * circle * 6.0
                + (random.nextDouble() - 0.5) * distance * 2.5;
            int initialX = (int) Math.round(Math.cos(angle) * dist);
            int initialZ = (int) Math.round(Math.sin(angle) * dist);
            RandomSource forked = random.fork();
            int centerX = initialX * 4 + 2;
            int centerZ = initialZ * 4 + 2;
            int found = 0;
            int selectedX = 0;
            int selectedZ = 0;
            // Every quart cell is preferred in this fixture. The loop still
            // performs the same reservoir draws as a real biome search.
            for (int offsetZ = -28; offsetZ <= 28; offsetZ++) {
                for (int offsetX = -28; offsetX <= 28; offsetX++) {
                    int quartX = centerX + offsetX;
                    int quartZ = centerZ + offsetZ;
                    if (forked.nextInt(found + 1) == 0) {
                        selectedX = Math.floorDiv(quartX, 4);
                        selectedZ = Math.floorDiv(quartZ, 4);
                    }
                    found++;
                }
            }
            System.out.println(i + " " + initialX + " " + initialZ + " " + selectedX + " " + selectedZ);
            angle += Math.PI * 2.0 / spread;
            positionInCircle++;
            if (positionInCircle == spread) {
                circle++;
                positionInCircle = 0;
                spread += 2 * spread / (circle + 1);
                spread = Math.min(spread, count - i);
                angle += random.nextDouble() * Math.PI * 2.0;
            }
        }
    }
}
