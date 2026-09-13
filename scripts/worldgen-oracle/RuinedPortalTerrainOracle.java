// External JVM probe for the target-chunk decoration stream used by a ruined
// portal's post-template terrain pass.
//
// The small in-memory world below deliberately has one asymmetric frame cell
// and a finite 64x64 horizontal window. It is only a detector for the random
// draw order: all accepted cells are ordinary stone, so no world implementation
// participates in the expected values recorded by the Rust fixture.
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.HashSet;
import java.util.Set;

import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;

public final class RuinedPortalTerrainOracle {
    private static final int STEP = 4;
    private static final int REGISTRY_INDEX = 8;
    private static final int MIN_X = 0;
    private static final int MIN_Y = 0;
    private static final int MIN_Z = 0;
    private static final int MAX_X = 63;
    private static final int MAX_Y = 64;
    private static final int MAX_Z = 63;
    private static final int BOX_MIN_X = 29;
    private static final int BOX_MIN_Y = 60;
    private static final int BOX_MIN_Z = 29;
    private static final int BOX_MAX_X = 33;
    private static final int BOX_MAX_Y = 63;
    private static final int BOX_MAX_Z = 33;

    private record Case(long worldSeed, int chunkX, int chunkZ) {}

    private static long key(int x, int y, int z) {
        return ((long)x & 0xFFFFFL) << 40 | ((long)y & 0xFFFFFL) << 20 | ((long)z & 0xFFFFFL);
    }

    private static boolean inBounds(int x, int y, int z) {
        return x >= MIN_X && x <= MAX_X && y >= MIN_Y && y <= MAX_Y && z >= MIN_Z && z <= MAX_Z;
    }

    private static boolean isAir(Set<Long> netherrack, int x, int y, int z) {
        return !inBounds(x, y, z) || (y > 59 && !netherrack.contains(key(x, y, z)));
    }

    private static int surfaceY(Set<Long> netherrack, int x, int z) {
        for (int y = MAX_Y; y >= MIN_Y; y--) {
            if (!isAir(netherrack, x, y, z)) {
                return y;
            }
        }
        return Integer.MIN_VALUE;
    }

    private static void place(Set<Long> netherrack, int x, int y, int z) {
        if (inBounds(x, y, z)) {
            netherrack.add(key(x, y, z));
        }
    }

    private static void drip(Set<Long> netherrack, WorldgenRandom random, int x, int y, int z) {
        place(netherrack, x, y, z);
        int remaining = 8;
        while (remaining > 0 && random.nextFloat() < 0.5F) {
            y--;
            remaining--;
            place(netherrack, x, y, z);
        }
    }

    private static String simulate(long worldSeed, int chunkX, int chunkZ, int[] distanceOut) {
        WorldgenRandom random = new WorldgenRandom(new XoroshiroRandomSource(0L));
        long decorationSeed = random.setDecorationSeed(worldSeed, chunkX * 16, chunkZ * 16);
        random.setFeatureSeed(decorationSeed, REGISTRY_INDEX, STEP);

        Set<Long> netherrack = new HashSet<>();
        netherrack.add(key(30, 60, 30));
        int centerX = BOX_MIN_X + (BOX_MAX_X - BOX_MIN_X + 1) / 2;
        int centerZ = BOX_MIN_Z + (BOX_MAX_Z - BOX_MIN_Z + 1) / 2;
        int averageWidth = ((BOX_MAX_X - BOX_MIN_X + 1) + (BOX_MAX_Z - BOX_MIN_Z + 1)) / 2;
        int distanceAdjustment = random.nextInt(Math.max(1, 8 - averageWidth / 2));
        distanceOut[0] = distanceAdjustment;
        float[] chanceByDistance = {1.0F, 1.0F, 1.0F, 1.0F, 1.0F, 1.0F, 1.0F, 0.9F, 0.9F, 0.8F, 0.7F, 0.6F, 0.4F, 0.2F};
        for (int x = centerX - chanceByDistance.length; x <= centerX + chanceByDistance.length; x++) {
            for (int z = centerZ - chanceByDistance.length; z <= centerZ + chanceByDistance.length; z++) {
                int distance = Math.abs(x - centerX) + Math.abs(z - centerZ);
                int adjusted = Math.max(0, distance + distanceAdjustment);
                if (adjusted >= chanceByDistance.length || random.nextDouble() >= chanceByDistance[adjusted]) {
                    continue;
                }
                int surface = surfaceY(netherrack, x, z);
                if (surface == Integer.MIN_VALUE) {
                    continue;
                }
                int y = surface;
                if (Math.abs(y - BOX_MIN_Y) > 3 || !inBounds(x, y, z)) {
                    continue;
                }
                place(netherrack, x, y, z);
                drip(netherrack, random, x, y - 1, z);
            }
        }

        for (int x = BOX_MIN_X + 1; x < BOX_MAX_X; x++) {
            for (int z = BOX_MIN_Z + 1; z < BOX_MAX_Z; z++) {
                if (netherrack.contains(key(x, BOX_MIN_Y, z))) {
                    drip(netherrack, random, x, BOX_MIN_Y - 1, z);
                }
            }
        }

        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            for (int y = MIN_Y; y <= MAX_Y; y++) {
                for (int x = MIN_X; x <= MAX_X; x++) {
                    for (int z = MIN_Z; z <= MAX_Z; z++) {
                        if (netherrack.contains(key(x, y, z))) {
                            digest.update((x + "," + y + "," + z + "=minecraft:netherrack\n")
                                .getBytes(StandardCharsets.UTF_8));
                        }
                    }
                }
            }
            byte[] bytes = digest.digest();
            StringBuilder hex = new StringBuilder(bytes.length * 2);
            for (byte b : bytes) {
                hex.append(String.format("%02x", b & 0xff));
            }
            return hex.toString();
        } catch (Exception e) {
            throw new AssertionError(e);
        }
    }

    public static void main(String[] args) {
        for (Case c : new Case[] {
            new Case(42L, 0, 0),
            new Case(42L, 1, 0),
            new Case(42L, 0, 1),
            new Case(-195764831L, 0, 0),
        }) {
            int[] distance = new int[1];
            String hash = simulate(c.worldSeed, c.chunkX, c.chunkZ, distance);
            System.out.printf("case %d %d %d %d %s%n", c.worldSeed, c.chunkX, c.chunkZ, distance[0], hash);
        }
    }
}
