// Captures resident End heightmaps at the raw packet lifecycle boundary.
// Values are read immediately after each FEATURES admission, independently
// from any light-free final-terrain representation.
import java.util.ArrayList;
import java.util.List;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;

final class EndP06LifecycleCapture {
    static final class LifecycleTransition {
        final ChunkPos resident;
        final int stage;
        final int[][] clientHeightmaps;

        LifecycleTransition(ChunkPos resident, int stage, int[][] clientHeightmaps) {
            this.resident = resident;
            this.stage = stage;
            this.clientHeightmaps = clientHeightmaps;
        }
    }

    static final class LifecycleEvent {
        final ChunkPos source;
        final long sequence;
        final List<LifecycleTransition> residentTransitions;

        LifecycleEvent(ChunkPos source, long sequence, List<LifecycleTransition> residentTransitions) {
            this.source = source;
            this.sequence = sequence;
            this.residentTransitions = List.copyOf(residentTransitions);
        }
    }

    private EndP06LifecycleCapture() {}

    static List<LifecycleEvent> capture(
        MinecraftServer server,
        ServerLevel level,
        ChunkPos target,
        long sequenceBase
    ) {
        server.submit(() -> level.getChunkSource().getChunk(
            target.x(), target.z(), ChunkStatus.CARVERS, true)).join();
        List<LifecycleEvent> events = new ArrayList<>(9);
        long sequence = sequenceBase;
        for (int x = target.x() - 1; x <= target.x() + 1; x++) {
            for (int z = target.z() - 1; z <= target.z() + 1; z++) {
                ChunkPos sourcePos = new ChunkPos(x, z);
                ChunkAccess source = server.submit(() -> level.getChunkSource().getChunk(
                    sourcePos.x(), sourcePos.z(), ChunkStatus.FEATURES, true)).join();
                ChunkAccess observedTarget = server.submit(() -> level.getChunkSource().getChunk(
                    target.x(), target.z(), ChunkStatus.CARVERS, true)).join();
                List<LifecycleTransition> transitions = new ArrayList<>(2);
                transitions.add(transition(source));
                if (!sourcePos.equals(target)) transitions.add(transition(observedTarget));
                events.add(new LifecycleEvent(sourcePos, sequence++, transitions));
            }
        }
        return events;
    }

    private static LifecycleTransition transition(ChunkAccess chunk) {
        Heightmap.Types[] types = {
            Heightmap.Types.WORLD_SURFACE,
            Heightmap.Types.MOTION_BLOCKING,
            Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
        };
        int[][] maps = null;
        boolean primed = true;
        for (Heightmap.Types type : types) {
            if (chunk.getHeightmaps().stream().noneMatch(entry -> entry.getKey() == type)) {
                primed = false;
                break;
            }
        }
        if (primed) {
            maps = new int[types.length][256];
            for (int map = 0; map < types.length; map++) {
                for (int z = 0; z < 16; z++) {
                    for (int x = 0; x < 16; x++) {
                        maps[map][x + z * 16] = chunk.getHeight(types[map], x, z) + 1;
                    }
                }
            }
        }
        return new LifecycleTransition(chunk.getPos(), stage(chunk), maps);
    }

    private static int stage(ChunkAccess chunk) {
        String status = chunk.getPersistedStatus().getName();
        return switch (status) {
            case "carvers" -> 0;
            case "features" -> 1;
            case "full", "initialize_light", "light" -> 2;
            default -> throw new IllegalStateException(
                "external End lifecycle reported unsupported resident status: " + status);
        };
    }
}
