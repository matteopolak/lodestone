// External JVM probe for the desert-pyramid world-seeded positional picks.
//
// It calls the bundled server's public random-source API. The output is copied
// into the committed structure fixture so the Rust gate does not derive its
// expected values from the implementation it exercises.
import java.util.ArrayList;
import java.util.List;

import net.minecraft.core.BlockPos;
import net.minecraft.core.Vec3i;
import net.minecraft.util.RandomSource;
import net.minecraft.util.Util;

public final class PyramidRoofOracle {
    private record RoofCase(long seed, int cornerX, int cornerY, int cornerZ) {}

    public static void main(String[] args) {
        RoofCase[] cases = {
            new RoofCase(42L, 14, 74, 11),
            new RoofCase(-195764831L, 14, 74, 11),
            new RoofCase(1234567890123L, 14, 74, 11),
            new RoofCase(-8823894646L, 14, 74, 11),
        };
        for (RoofCase c : cases) {
            RandomSource random = RandomSource.createThreadLocalInstance(c.seed)
                .forkPositional()
                .at(c.cornerX, c.cornerY, c.cornerZ);
            int localX = random.nextInt(5) + 14;
            int localZ = random.nextInt(5) + 11;
            System.out.printf("case %d %d %d %d %d %d %d%n",
                c.seed, c.cornerX, c.cornerY, c.cornerZ, localX, c.cornerY, localZ);

            List<BlockPos> candidates = new ArrayList<>();
            for (int y = 71; y <= 73; y++) {
                for (int x = 14; x <= 18; x++) {
                    for (int z = 11; z <= 15; z++) {
                        candidates.add(new BlockPos(x, y, z));
                    }
                }
            }
            candidates.add(new BlockPos(19, 71, 13));
            candidates.add(new BlockPos(19, 72, 13));
            candidates.add(new BlockPos(13, 71, 13));
            candidates.add(new BlockPos(13, 72, 13));
            candidates.add(new BlockPos(16, 71, 16));
            candidates.add(new BlockPos(16, 72, 16));
            candidates.add(new BlockPos(16, 71, 10));
            candidates.add(new BlockPos(16, 72, 10));
            candidates.sort(Vec3i::compareTo);
            RandomSource shuffleRandom = RandomSource.createThreadLocalInstance(c.seed)
                .forkPositional()
                .at(10, 81, 10);
            Util.shuffle(candidates, shuffleRandom);
            int suspicious = shuffleRandom.nextInt(3) + 5;
            System.out.print("shuffle " + c.seed + " " + suspicious);
            for (int i = 0; i < suspicious; i++) {
                BlockPos pos = candidates.get(i);
                System.out.print(" " + pos.getX() + " " + pos.getY() + " " + pos.getZ());
            }
            System.out.println();
        }

        // The production piece stream starts with orientation and sink draws,
        // then four chest roll seeds. These values guard that the roof fork did
        // not consume or replace the unrelated loot stream.
        RandomSource stream = RandomSource.createThreadLocalInstance(0L);
        stream.nextInt(4);
        stream.nextInt(3);
        System.out.print("loot");
        for (int i = 0; i < 4; i++) {
            System.out.print(" " + stream.nextLong());
        }
        System.out.println();
    }
}
