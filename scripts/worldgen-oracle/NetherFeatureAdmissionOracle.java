import java.nio.file.Path;
import java.util.List;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;

public final class NetherFeatureAdmissionOracle {
    private static int envInt(String name, int fallback) {
        String value = System.getenv(name);
        return value == null ? fallback : Integer.parseInt(value);
    }

    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        LargeParityOracle.runServer(Path.of("/work/admission-world"), false, args, (server, level) -> {
            int cx = envInt("ORACLE_TARGET_X", 14);
            int cz = envInt("ORACLE_TARGET_Z", -9);
            BlockPos focus = new BlockPos(
                envInt("ORACLE_FOCUS_X", 223), envInt("ORACLE_FOCUS_Y", 43),
                envInt("ORACLE_FOCUS_Z", -142));
            ChunkStatus status = System.getenv("ORACLE_ADMISSION") != null
                ? ChunkStatus.CARVERS : ChunkStatus.FEATURES;
            List<ChunkPos> positions = new java.util.ArrayList<>();
            String onlySource = System.getenv("ORACLE_SOURCE_X");
            if (System.getenv("ORACLE_STREAM_FULL") != null) {
                for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) {
                    positions.add(new ChunkPos(cx + dx, cz + dz));
                }
            } else if (onlySource != null) {
                positions.add(new ChunkPos(Integer.parseInt(onlySource), envInt("ORACLE_SOURCE_Z", cz)));
            } else {
            for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) {
                positions.add(new ChunkPos(cx + dx, cz + dz));
            }
            }
            for (ChunkPos pos : positions) {
                ChunkAccess chunk = level.getChunkSource().getChunk(pos.x(), pos.z(), status, true);
                System.out.println("ADMIT " + status + " " + pos + " focus="
                    + level.getBlockState(focus) + " returned=" + chunk.getBlockState(focus));
            }
            for (int y = focus.getY() - 3; y <= focus.getY() + 3; y++) {
                StringBuilder row = new StringBuilder("ROW " + y);
                for (int x = focus.getX() - 7; x <= focus.getX() + 7; x++) {
                    var state = level.getBlockState(new BlockPos(x, y, focus.getZ()));
                    char marker = state.isAir() ? '.' : state.toString().contains("nether_wart_block") ? 'W'
                        : state.toString().contains("crimson_stem") ? 'S'
                        : state.toString().contains("weeping_vines") ? 'V' : '#';
                    row.append(marker);
                }
                System.out.println(row);
            }
        });
    }
}
