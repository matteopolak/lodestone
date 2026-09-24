import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.ChunkPos;

public final class NetherColumnOracle {
    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        args.lightFreeV7 = true;
        LargeParityOracle.runServer(Path.of(System.getenv("ORACLE_FROZEN_WORK_ROOT")), true, args, (server, level) -> {
            List<ChunkPos> loaded = new ArrayList<>();
            for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) loaded.add(new ChunkPos(-3 + dx, -25 + dz));
            LargeParityOracle.loadBatch(server, level, args, loaded, false, new ArrayList<>());
            server.submit(() -> {
                int[][] columns = {
                    {-39, -399}, {-39, -389}, {-46, -391}, {-43, -400},
                    {-42, -398}, {-36, -398}, {-38, -395}, {-45, -393}, {-48, -396},
                    {-46, -394}, {-37, -399}, {-40, -387}, {-45, -392}, {-39, -385},
                    {-36, -397}, {-45, -389}
                };
                for (int[] column : columns) {
                    int x = column[0], z = column[1];
                    System.out.println("COLUMN " + x + " " + z + " height-before "
                        + level.getHeight(net.minecraft.world.level.levelgen.Heightmap.Types.MOTION_BLOCKING, x, z));
                    for (int y = 127; y >= 0; y--) {
                        var state = level.getBlockState(new BlockPos(x, y, z));
                        if (!state.isAir()) System.out.println("block " + x + " " + y + " " + z + " " + state);
                    }
                    System.out.println("height-after "
                        + level.getHeight(net.minecraft.world.level.levelgen.Heightmap.Types.MOTION_BLOCKING, x, z));
                }
            }).join();
        });
    }
}
