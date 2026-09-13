import java.nio.file.Path;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;

public final class NetherFrozenDump {
    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        LargeParityOracle.runServer(Path.of("/work/copy"), true, args, (server, level) -> {
            BlockPos focus = new BlockPos(-794, 14, -800);
            ChunkAccess target = level.getChunkSource().getChunk(-50, -50, ChunkStatus.FULL, true);
            System.out.println("FOCUS " + target.getBlockState(focus));
            for (int y = 0; y <= 40; y++) {
                for (int z = -802; z <= -798; z++) for (int x = -796; x <= -792; x++) {
                    BlockPos p = new BlockPos(x, y, z);
                    String state = target.getBlockState(p).toString();
                    if (!state.contains("air") && !state.contains("netherrack"))
                        System.out.println("STATE " + p + " " + state);
                }
            }
            for (int y = 0; y <= 40; y++) for (int z = -816; z <= -784; z++) for (int x = -816; x <= -784; x++) {
                BlockPos p = new BlockPos(x, y, z);
                String state = level.getBlockState(p).toString();
                if (state.contains("lava") || state.contains("blue_ice") || state.contains("soul_soil"))
                    System.out.println("SPECIAL " + p + " " + state);
            }
        });
    }
}
