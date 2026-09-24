import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import it.unimi.dsi.fastutil.shorts.ShortList;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;

public final class NetherPostProcessProbe {
    private static final BlockPos FOCUS = new BlockPos(-794, 14, -800);

    private static void dump(String label, ServerLevel level, ChunkAccess chunk) {
        System.out.println(label + " status=" + chunk.getPersistedStatus()
            + " target=" + chunk.getBlockState(FOCUS));
        int pending = 0;
        ShortList[] lists = chunk.getPostProcessing();
        if (lists != null) {
            for (int section = 0; section < lists.length; section++) {
                ShortList list = lists[section];
                if (list == null) continue;
                pending += list.size();
                for (short packed : list) {
                    BlockPos pos = ProtoChunk.unpackOffsetCoordinates(packed,
                        chunk.getSectionYFromSectionIndex(section), chunk.getPos());
                    if (Math.abs(pos.getX() - FOCUS.getX()) <= 2
                        && Math.abs(pos.getY() - FOCUS.getY()) <= 2
                        && Math.abs(pos.getZ() - FOCUS.getZ()) <= 2) {
                        System.out.println(label + " pending=" + pos + " state=" + chunk.getBlockState(pos)
                            + " below=" + chunk.getBlockState(pos.below())
                            + " west=" + level.getBlockState(pos.west())
                            + " east=" + level.getBlockState(pos.east())
                            + " north=" + level.getBlockState(pos.north())
                            + " south=" + level.getBlockState(pos.south()));
                    }
                }
            }
        }
        System.out.println(label + " pending_count=" + pending);
    }

    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        LargeParityOracle.runServer(Path.of("/work/post-process-world"), false, args, (server, level) -> {
            ChunkAccess sourceFeatures = level.getChunkSource().getChunk(-51, -51, ChunkStatus.FEATURES, true);
            dump("after_source_features", level, sourceFeatures);
            ChunkAccess targetFeatures = level.getChunkSource().getChunk(-50, -50, ChunkStatus.FEATURES, true);
            dump("target_features", level, targetFeatures);
            System.out.println("target_neighbours_features");
            for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) {
                ChunkAccess chunk = level.getChunkSource().getChunk(-50 + dx, -50 + dz, ChunkStatus.FEATURES, true);
                System.out.println("  " + chunk.getPos() + " " + chunk.getBlockState(FOCUS));
            }
            ChunkAccess sourceFull = level.getChunkSource().getChunk(-51, -51, ChunkStatus.FULL, true);
            dump("after_source_full", level, sourceFull);
            ChunkAccess targetFull = level.getChunkSource().getChunk(-50, -50, ChunkStatus.FULL, true);
            dump("target_full", level, targetFull);
            System.out.println("target_neighbours_full");
            for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) {
                ChunkAccess chunk = level.getChunkSource().getChunk(-50 + dx, -50 + dz, ChunkStatus.FULL, true);
                System.out.println("  " + chunk.getPos() + " " + chunk.getBlockState(FOCUS));
            }
        });
    }
}
