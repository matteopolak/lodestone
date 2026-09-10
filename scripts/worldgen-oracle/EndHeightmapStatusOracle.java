// Bounded external discriminator for End feature status and client heightmaps.
// It drives the compiled 26.2 server only; no Lodestone generation code is
// involved in the observations.
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collection;
import java.util.EnumMap;
import java.util.List;
import java.util.Map;
import java.util.Map.Entry;
import java.util.TreeMap;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerLevel;

/**
 * Captures one target column before FEATURES, then after each explicitly
 * admitted source reaches FEATURES. The target starts at CARVERS so a source
 * neighbour can write into it before the target itself enters FEATURES.
 */
public final class EndHeightmapStatusOracle {
    private static final long SEED = 42L;
    private static final int DEFAULT_TARGET_X = 280;
    private static final int DEFAULT_TARGET_Z = 78;
    private static final int LOCAL_X = 2;
    private static final int LOCAL_Z = 0;
    private static final int FIRST_ROW = 55;
    private static final int LAST_ROW = 68;
    private static boolean firstY57WriteReported;

    /** One resident status/map observation attached to an external lifecycle event. */
    public static final class LifecycleTransition {
        public final ChunkPos resident;
        public final String stage;
        /** Values are in registry order 1, 4, 5 and local index x + z * 16. */
        public final int[][] clientHeightmaps;

        private LifecycleTransition(ChunkAccess resident, String stage, int[][] clientHeightmaps) {
            this.resident = resident.getPos();
            this.stage = stage;
            this.clientHeightmaps = clientHeightmaps;
        }
    }

    /** One external source completion and the resident observations it produced. */
    public static final class LifecycleEvent {
        public final ChunkPos source;
        public final String stage;
        public final long sequence;
        public final List<LifecycleTransition> residentTransitions;

        private LifecycleEvent(ChunkPos source, String stage, long sequence, List<LifecycleTransition> residentTransitions) {
            this.source = source;
            this.stage = stage;
            this.sequence = sequence;
            this.residentTransitions = List.copyOf(residentTransitions);
        }
    }

    private static final class BandSnapshot {
        private final String[][][] states = new String[LAST_ROW - FIRST_ROW + 1][16][16];

        private String get(int y, int x, int z) {
            return states[y - FIRST_ROW][x][z];
        }

        private void set(int y, int x, int z, String state) {
            states[y - FIRST_ROW][x][z] = state;
        }
    }

    private static int envInt(String name, int fallback) {
        String value = System.getenv(name);
        return value == null || value.isBlank() ? fallback : Integer.parseInt(value);
    }

    private static String mode() {
        String raw = System.getenv().getOrDefault("ORACLE_ARGS", "--mode canonical-5x5").trim();
        String[] args = raw.isEmpty() ? new String[0] : raw.split("\\s+");
        for (int i = 0; i < args.length; i++) {
            if ("--mode".equals(args[i]) && i + 1 < args.length) return args[i + 1];
        }
        return raw.isEmpty() ? "canonical-5x5" : raw;
    }

    private static List<ChunkPos> sources(String mode, ChunkPos target) {
        List<ChunkPos> result = new ArrayList<>();
        int radius;
        switch (mode) {
            case "target-only" -> {
                result.add(target);
                return result;
            }
            case "canonical-5x5" -> radius = 2;
            case "canonical-3x3" -> radius = 1;
            case "reverse-5x5" -> {
                for (int z = target.z() + 2; z >= target.z() - 2; z--) {
                    for (int x = target.x() + 2; x >= target.x() - 2; x--) {
                        result.add(new ChunkPos(x, z));
                    }
                }
                return result;
            }
            default -> throw new IllegalArgumentException(
                "mode must be target-only, canonical-5x5, reverse-5x5, or canonical-3x3: " + mode);
        }
        for (int z = target.z() - radius; z <= target.z() + radius; z++) {
            for (int x = target.x() - radius; x <= target.x() + radius; x++) {
                result.add(new ChunkPos(x, z));
            }
        }
        return result;
    }

    private static <T extends Comparable<T>> String propertyValue(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }

    private static String canonical(BlockState state) {
        StringBuilder out = new StringBuilder(
            BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        Map<String, String> properties = new TreeMap<>();
        for (Property<?> property : state.getProperties()) {
            properties.put(property.getName(), propertyValue(state, property));
        }
        if (!properties.isEmpty()) {
            out.append('[');
            boolean first = true;
            for (Entry<String, String> property : properties.entrySet()) {
                if (!first) out.append(',');
                first = false;
                out.append(property.getKey()).append('=').append(property.getValue());
            }
            out.append(']');
        }
        return out.toString();
    }

    private static String raw(long[] values) {
        StringBuilder out = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            if (i != 0) out.append(',');
            out.append(Long.toUnsignedString(values[i], 16));
        }
        return out.toString();
    }

    private static Map<Heightmap.Types, long[]> rawHeightmaps(ChunkAccess chunk) {
        Map<Heightmap.Types, long[]> result = new EnumMap<>(Heightmap.Types.class);
        Collection<Entry<Heightmap.Types, Heightmap>> entries = chunk.getHeightmaps();
        for (Entry<Heightmap.Types, Heightmap> entry : entries) {
            result.put(entry.getKey(), entry.getValue().getRawData().clone());
        }
        return result;
    }

    private static LifecycleTransition lifecycleTransition(ChunkAccess chunk) {
        Map<Heightmap.Types, long[]> maps = rawHeightmaps(chunk);
        int[][] raw = null;
        if (maps.containsKey(Heightmap.Types.WORLD_SURFACE)
            && maps.containsKey(Heightmap.Types.MOTION_BLOCKING)
            && maps.containsKey(Heightmap.Types.MOTION_BLOCKING_NO_LEAVES)) {
            raw = new int[3][256];
            Heightmap.Types[] types = {
                Heightmap.Types.WORLD_SURFACE,
                Heightmap.Types.MOTION_BLOCKING,
                Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
            };
            for (int map = 0; map < types.length; map++) {
                for (int z = 0; z < 16; z++) {
                    for (int x = 0; x < 16; x++) {
                        // The Rust carrier stores the retained map in its
                        // local top+1 representation. The external status
                        // accessor reports the corresponding zero-based raw
                        // value, so translate at the oracle boundary once.
                        raw[map][x + z * 16] = chunk.getHeight(types[map], x, z) + 1;
                    }
                }
            }
        }
        return new LifecycleTransition(chunk, chunk.getPersistedStatus().getName(), raw);
    }

    /**
     * Capture the real End scheduler boundary used by the streaming format.
     * The target is first admitted through CARVERS, then the complete 3x3
     * source wavefront is requested through FEATURES in source-order. Every
     * event carries the source and the target's observed resident status/maps;
     * no map is reconstructed from a final block field.
     */
    public static List<LifecycleEvent> captureLifecycleEvents(
        MinecraftServer server,
        ServerLevel level,
        ChunkPos target,
        long sequenceBase
    ) {
        server.submit(() -> level.getChunkSource().getChunk(
            target.x(), target.z(), ChunkStatus.CARVERS, true)).join();
        List<ChunkPos> sources = new ArrayList<>();
        for (int x = target.x() - 1; x <= target.x() + 1; x++) {
            for (int z = target.z() - 1; z <= target.z() + 1; z++) {
                sources.add(new ChunkPos(x, z));
            }
        }
        List<LifecycleEvent> events = new ArrayList<>(sources.size());
        for (int index = 0; index < sources.size(); index++) {
            ChunkPos sourcePos = sources.get(index);
            ChunkAccess source = server.submit(() -> level.getChunkSource().getChunk(
                sourcePos.x(), sourcePos.z(), ChunkStatus.FEATURES, true)).join();
            ChunkAccess observedTarget = server.submit(() -> level.getChunkSource().getChunk(
                target.x(), target.z(), ChunkStatus.CARVERS, true)).join();
            List<LifecycleTransition> transitions = new ArrayList<>(2);
            transitions.add(lifecycleTransition(source));
            if (!sourcePos.equals(target)) transitions.add(lifecycleTransition(observedTarget));
            events.add(new LifecycleEvent(
                sourcePos,
                "features",
                sequenceBase + index,
                transitions
            ));
        }
        return events;
    }

    private static BandSnapshot captureBand(ChunkAccess target, int targetX, int targetZ) {
        BandSnapshot snapshot = new BandSnapshot();
        for (int y = FIRST_ROW; y <= LAST_ROW; y++) {
            for (int z = 0; z < 16; z++) {
                for (int x = 0; x < 16; x++) {
                    snapshot.set(y, x, z, canonical(target.getBlockState(
                        new BlockPos(targetX * 16 + x, y, targetZ * 16 + z))));
                }
            }
        }
        return snapshot;
    }

    private static void reportBandTransitions(
        BandSnapshot previous,
        BandSnapshot current,
        ChunkPos source,
        String targetStatus
    ) {
        if (previous == null) return;
        for (int y = FIRST_ROW; y <= LAST_ROW; y++) {
            for (int z = 0; z < 16; z++) {
                for (int x = 0; x < 16; x++) {
                    String before = previous.get(y, x, z);
                    String after = current.get(y, x, z);
                    if (before.equals(after)) continue;
                    if (y == 57) {
                        String prefix = firstY57WriteReported ? "Y57_WRITE" : "FIRST_Y57_WRITE";
                        System.out.println(prefix + " source=" + source + " local=" + x + "," + z
                            + " before=" + before + " after=" + after
                            + " target_status=" + targetStatus);
                        firstY57WriteReported = true;
                    } else {
                        System.out.println("BAND_TRANSITION source=" + source + " local=" + x + "," + z
                            + " y=" + y + " before=" + before + " after=" + after
                            + " target_status=" + targetStatus);
                    }
                }
            }
        }
    }

    private static BandSnapshot dump(
        String label,
        ChunkAccess target,
        int targetX,
        int targetZ,
        ChunkPos source,
        String sourceStatus,
        BandSnapshot previousBand
    ) {
        System.out.println("SNAPSHOT label=" + label + " source=" + source
            + " source_status=" + sourceStatus
            + " target_status=" + target.getPersistedStatus().getName()
            + " target=" + target.getPos());
        Map<Heightmap.Types, long[]> maps = rawHeightmaps(target);
        for (Heightmap.Types type : List.of(
            Heightmap.Types.WORLD_SURFACE,
            Heightmap.Types.MOTION_BLOCKING,
            Heightmap.Types.MOTION_BLOCKING_NO_LEAVES)) {
            long[] values = maps.get(type);
            String id = Integer.toString(type.ordinal());
            if (values == null) {
                System.out.println("HEIGHTMAP type=" + id + " name=" + type.getSerializationKey()
                    + " primed=false");
            } else {
                System.out.println("HEIGHTMAP type=" + id + " name=" + type.getSerializationKey()
                    + " primed=true focus=" + target.getHeight(type, LOCAL_X, LOCAL_Z)
                    + " raw=" + raw(values));
            }
        }
        BandSnapshot currentBand = captureBand(target, targetX, targetZ);
        reportBandTransitions(previousBand, currentBand, source,
            target.getPersistedStatus().getName());
        for (int y = FIRST_ROW; y <= LAST_ROW; y++) {
            StringBuilder row = new StringBuilder("ROW y=").append(y).append(" states=");
            for (int x = 0; x < 16; x++) {
                if (x != 0) row.append('|');
                row.append(canonical(target.getBlockState(
                    new BlockPos(targetX * 16 + x, y, targetZ * 16 + LOCAL_Z))));
            }
            System.out.println(row);
            System.out.println("CELL y=" + y + " local=" + LOCAL_X + "," + LOCAL_Z
                + " state=" + canonical(target.getBlockState(
                new BlockPos(targetX * 16 + LOCAL_X, y, targetZ * 16 + LOCAL_Z))));
        }
        return currentBand;
    }

    private static void run(String selectedMode) throws Exception {
        int targetX = envInt("ORACLE_TARGET_X", DEFAULT_TARGET_X);
        int targetZ = envInt("ORACLE_TARGET_Z", DEFAULT_TARGET_Z);
        ChunkPos targetPos = new ChunkPos(targetX, targetZ);
        List<ChunkPos> admissions = sources(selectedMode, targetPos);
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.END;
        args.explicitDimension = true;
        Path root = Path.of("/work/end-heightmap-status-" + selectedMode);
        LargeParityOracle.runServer(root, false, args, (server, level) -> {
            firstY57WriteReported = false;
            ChunkAccess target = server.submit(() -> level.getChunkSource().getChunk(
                targetX, targetZ, ChunkStatus.CARVERS, true)).join();
            System.out.println("META seed=" + SEED + " mode=" + selectedMode
                + " target=" + targetX + "," + targetZ + " local=" + LOCAL_X + "," + LOCAL_Z
                + " rows=" + FIRST_ROW + ".." + LAST_ROW
                + " pre_status_request=carvers admissions=" + admissions.size());
            dump("pre-feature", target, targetX, targetZ,
                new ChunkPos(targetX, targetZ), "not-requested", null);

            BandSnapshot previousBand = captureBand(target, targetX, targetZ);
            int index = 0;
            for (ChunkPos sourcePos : admissions) {
                index++;
                ChunkAccess source = server.submit(() -> level.getChunkSource().getChunk(
                    sourcePos.x(), sourcePos.z(), ChunkStatus.FEATURES, true)).join();
                String sourceStatus = source.getPersistedStatus().getName();
                System.out.println("ADMITTED index=" + index + " source=" + sourcePos
                    + " requested=features source_status=" + sourceStatus);
                ChunkAccess observedTarget = server.submit(() -> level.getChunkSource().getChunk(
                    targetX, targetZ, ChunkStatus.CARVERS, true)).join();
                previousBand = dump("after-features", observedTarget, targetX, targetZ,
                    sourcePos, sourceStatus, previousBand);
                target = observedTarget;
            }
            System.out.println("Y57_RESULT first_write="
                + (firstY57WriteReported ? "observed" : "none-in-captured-band"));
        });
    }

    public static void main(String[] ignored) throws Exception {
        run(mode());
    }
}
