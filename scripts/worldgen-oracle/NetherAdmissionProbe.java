import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;

public final class NetherAdmissionProbe {
    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        args.rawPacketV6 = true;
        args.loX = -50;
        args.hiX = 50;
        args.loZ = -50;
        args.hiZ = 50;
        LargeParityOracle.runServer(Path.of("/work/admission-probe"), false, args, (server, level) -> {
            List<ChunkPos> positions = new ArrayList<>();
            for (int z = -51; z <= -36; z++) {
                for (int x = -51; x <= -36; x++) {
                    positions.add(new ChunkPos(x, z));
                }
            }
            net.minecraft.core.BlockPos focus = new net.minecraft.core.BlockPos(-794, 14, -800);
            String previous = "";
            for (ChunkPos pos : positions) {
                LargeParityOracle.resetLevelRandom(level, pos);
                LargeParityOracle.resetBiomeSearchState(level);
                LargeParityOracle.resetBiomeSearchStateOnWorker(level);
                java.util.concurrent.CompletableFuture<?> future = server.submit(() ->
                    level.getChunkSource().addTicketAndLoadWithRadius(
                        net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 2)).join();
                net.minecraft.server.level.ChunkResult<?> result =
                    (net.minecraft.server.level.ChunkResult<?>) future.join();
                if (!result.isSuccess()) throw new IllegalStateException("admission failed at " + pos + ": " + result.getError());
                LargeParityOracle.settleMaterializedBatch(server, level, List.of(pos));
                String current = level.getBlockState(focus).toString();
                if (!current.equals(previous)) {
                    System.out.println("TARGET_TRANSITION source=" + pos + " " + previous + " -> " + current);
                    previous = current;
                }
            }
            server.submit(() -> level.getChunkSource().tick(() -> true, false)).join();
            server.submit(() -> System.out.println("TARGET_PRE_RELEASE " + level.getBlockState(focus))).join();
            server.submit(() -> {
                for (ChunkPos pos : positions) level.getChunkSource().removeTicketWithRadius(
                    net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0);
                level.getChunkSource().tick(() -> true, false);
            }).join();
            server.submit(() -> {
                System.out.println("TARGET_SERVER_THREAD " + level.getBlockState(focus));
                net.minecraft.world.level.chunk.LevelChunk chunk = level.getChunkSource().getChunkNow(-50, -50);
                System.out.println("TARGET_CHUNK " + chunk.getBlockState(focus));
                byte[] packet = LargeParityOracle.packetBody(server, chunk, level);
                System.out.println("TARGET_AFTER_PACKET " + chunk.getBlockState(focus) + " packet_bytes=" + packet.length);
            }).join();
        });
    }
}
