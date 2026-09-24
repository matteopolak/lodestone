import java.nio.file.Path;
import java.util.HashMap;
import java.util.Map;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;

/**
 * Emits the target column's feature-stage writes, independently of the Rust
 * generator. The CARVERS snapshot is taken before the nine-source FEATURES
 * closure, so a captured block set is evidence about decoration rather than
 * terrain fill or carving.
 */
public final class NetherFeatureCellsOracle {
    private static int envInt(String name, int fallback) {
        String value = System.getenv(name);
        return value == null ? fallback : Integer.parseInt(value);
    }

    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        LargeParityOracle.runServer(Path.of("/work/nether-feature-cells"), false, args,
            (server, level) -> {
                int cx = envInt("ORACLE_TARGET_X", 0);
                int cz = envInt("ORACLE_TARGET_Z", 0);
                String wanted = System.getenv().getOrDefault("ORACLE_BLOCK", "minecraft:lava");
                ChunkAccess target = level.getChunkSource().getChunk(cx, cz, ChunkStatus.CARVERS, true);
                Map<String, String> before = new HashMap<>();
                for (int y = level.getMinY(); y <= level.getMaxY(); y++) {
                    for (int z = cz * 16; z < cz * 16 + 16; z++) {
                        for (int x = cx * 16; x < cx * 16 + 16; x++) {
                            BlockPos pos = new BlockPos(x, y, z);
                            before.put(key(x, y, z), target.getBlockState(pos).toString());
                        }
                    }
                }
                for (int dz = -1; dz <= 1; dz++) {
                    for (int dx = -1; dx <= 1; dx++) {
                        level.getChunkSource().getChunk(cx + dx, cz + dz, ChunkStatus.FEATURES, true);
                    }
                }
                int count = 0;
                for (int y = level.getMinY(); y <= level.getMaxY(); y++) {
                    for (int z = cz * 16; z < cz * 16 + 16; z++) {
                        for (int x = cx * 16; x < cx * 16 + 16; x++) {
                            BlockPos pos = new BlockPos(x, y, z);
                            String after = level.getBlockState(pos).toString();
                            if (after.contains(wanted) && !after.equals(before.get(key(x, y, z)))) {
                                count++;
                                System.out.println("cell " + (x - cx * 16) + " " + y + " "
                                    + (z - cz * 16) + " " + after);
                            }
                        }
                    }
                }
                System.out.println("seed " + level.getSeed());
                System.out.println("chunk " + cx + " " + cz);
                System.out.println("block " + wanted);
                System.out.println("count " + count);
            });
    }

    private static String key(int x, int y, int z) {
        return x + "," + y + "," + z;
    }
}
