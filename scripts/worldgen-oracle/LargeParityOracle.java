// Exports canonical semantic chunk digests from one frozen compiled-server world.
// Raw packet diagnostics use the same compiled codec after packet-visible ordering
// has been made explicit; semantic records remain the acceptance baseline.
import com.mojang.authlib.yggdrasil.YggdrasilAuthenticationService;
import com.mojang.serialization.Dynamic;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.RandomAccessFile;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.net.Proxy;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.ByteArrayTag;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.IntArrayTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.LongArrayTag;
import net.minecraft.nbt.NumericTag;
import net.minecraft.nbt.StringTag;
import net.minecraft.nbt.Tag;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.Main;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.Services;
import net.minecraft.server.WorldLoader;
import net.minecraft.server.WorldStem;
import net.minecraft.server.dedicated.DedicatedServer;
import net.minecraft.server.dedicated.DedicatedServerProperties;
import net.minecraft.server.dedicated.DedicatedServerSettings;
import net.minecraft.server.jsonrpc.JsonRpc;
import net.minecraft.server.jsonrpc.ManagementServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.notifications.NotificationManager;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.util.Util;
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.PalettedContainer;
import net.minecraft.world.level.chunk.Strategy;
import net.minecraft.world.level.dimension.LevelStem;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.storage.LevelDataAndDimensions;
import net.minecraft.world.level.storage.LevelStorageSource;
import net.minecraft.network.protocol.game.ClientboundLevelChunkWithLightPacket;
import net.minecraft.network.protocol.game.ClientboundLevelChunkPacketData;
import net.minecraft.network.FriendlyByteBuf;
import net.minecraft.network.RegistryFriendlyByteBuf;

public final class LargeParityOracle {
    static final byte[] MAGIC = "LWP26P03".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MAGIC_V4 = "LWP26P04".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MAGIC_V5 = "LWP26P05".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MAGIC_V6 = "LWP26P06".getBytes(StandardCharsets.US_ASCII);
    static final int HEADER_BYTES = 256, FORMAT_VERSION = 3, SCHEMA_VERSION = 3, DIGEST_BYTES = 32;
    static final int GRID_MIN = -250, GRID_MAX = 250;
    static final int RAW_GRID_MIN = -500, RAW_GRID_MAX = 500, RAW_RECORD_BYTES = 2;
    static final int GRID_SIDE = GRID_MAX - GRID_MIN + 1;
    static final long GRID_COUNT = (long) GRID_SIDE * GRID_SIDE;
    static final int HALO_MIN = GRID_MIN - 1, HALO_MAX = GRID_MAX + 1;
    static final int RAW_HALO_MIN = RAW_GRID_MIN - 1, RAW_HALO_MAX = RAW_GRID_MAX + 1;
    static final long SEED = 42L;
    static final byte[] MANIFEST_DOMAIN = "lodestone.worldgen.large-parity.manifest/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MANIFEST_DOMAIN_V4 = "lodestone.worldgen.large-parity.manifest/v4/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MANIFEST_DOMAIN_V5 = "lodestone.worldgen.large-parity.manifest/v5/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MANIFEST_DOMAIN_V6 = "lodestone.worldgen.large-parity.manifest/v6/raw-packet".getBytes(StandardCharsets.US_ASCII);
    static final byte[] PACKET_AUDIT_MAGIC = "LWP26A06".getBytes(StandardCharsets.US_ASCII);
    static final byte[] PACKET_AUDIT_DOMAIN = "lodestone.worldgen.large-parity.packet-audit/v6/raw-packet".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN = "lodestone.worldgen.large-parity.chunk/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V4 = "lodestone.worldgen.large-parity.chunk/v4/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V5 = "lodestone.worldgen.large-parity.chunk/v5/semantic".getBytes(StandardCharsets.US_ASCII);
    // This namespace describes the generation scheduling contract, not the
    // semantic manifest format. It changes whenever a root's construction
    // rules change, even if its exported records do not.
    static final String MATERIALIZATION_CONTRACT = "lodestone-large-parity-materialization-v2";
    static final String MATERIALIZATION_CONTRACT_V6 = "lodestone-large-parity-materialization-v6";
    static final int MATERIALIZE_TILE = 16;
    static final String OVERWORLD = "overworld", NETHER = "nether", END = "end";
    static String diagnosticPacketOut, diagnosticRecordOut;

    static void verifySingleWorldgenWorker() {
        if (!"1".equals(System.getProperty("max.bg.threads"))) {
            throw new IllegalStateException("large-parity materialization requires -Dmax.bg.threads=1");
        }
    }

    static final class Args {
        String out;
        String mode;
        String packetOut;
        String packetAuditOut;
        String recordOut;
        int loX = GRID_MIN, hiX = GRID_MAX, loZ = GRID_MIN, hiZ = GRID_MAX;
        boolean resume, help, provenanceSelftest, determinismSelftest, rawPacketV6;
        boolean explicitCx, explicitCz;
        String dimension = OVERWORLD;
        boolean explicitDimension;
        boolean dimensionFormat() { return explicitDimension || !OVERWORLD.equals(dimension); }
        boolean v5() { return END.equals(dimension); }
        boolean v6() { return rawPacketV6; }
        int semanticVersion() { return v6() ? 6 : v5() ? 5 : dimensionFormat() ? 4 : FORMAT_VERSION; }
        int gridMin() { return v6() ? RAW_GRID_MIN : GRID_MIN; }
        int gridMax() { return v6() ? RAW_GRID_MAX : GRID_MAX; }
        int recordWidth() { return v6() ? RAW_RECORD_BYTES : DIGEST_BYTES; }
        byte[] magic() { return v6() ? MAGIC_V6 : v5() ? MAGIC_V5 : dimensionFormat() ? MAGIC_V4 : MAGIC; }
        byte[] manifestDomain() { return v6() ? MANIFEST_DOMAIN_V6 : v5() ? MANIFEST_DOMAIN_V5 : dimensionFormat() ? MANIFEST_DOMAIN_V4 : MANIFEST_DOMAIN; }
        String dimensionKey() { return switch (dimension) { case NETHER -> "minecraft:the_nether"; case END -> "minecraft:the_end"; default -> "minecraft:overworld"; }; }
    }

    static Args args() {
        String raw = System.getenv().getOrDefault("ORACLE_ARGS", "").trim();
        String[] a = raw.isEmpty() ? new String[0] : raw.split("\\s+");
        Args out = new Args();
        String environmentDimension = System.getenv("ORACLE_DIMENSION");
        if (environmentDimension != null && !environmentDimension.isBlank()) { out.dimension = environmentDimension.toLowerCase(); out.explicitDimension = true; }
        for (int i = 0; i < a.length; i++) switch (a[i]) {
            case "--help", "-h" -> out.help = true;
            case "--provenance-selftest" -> out.provenanceSelftest = true;
            case "--determinism-selftest" -> out.determinismSelftest = true;
            case "--raw-packet", "--format-v6" -> out.rawPacketV6 = true;
            case "--format" -> { if (!"v6".equals(a[++i])) throw new IllegalArgumentException("--format accepts only v6 for raw packet exports"); out.rawPacketV6 = true; }
            case "--mode" -> out.mode = a[++i];
            case "--out" -> out.out = a[++i];
            case "--cx" -> { out.loX = Integer.parseInt(a[++i]); out.hiX = Integer.parseInt(a[++i]); out.explicitCx = true; }
            case "--cz" -> { out.loZ = Integer.parseInt(a[++i]); out.hiZ = Integer.parseInt(a[++i]); out.explicitCz = true; }
            case "--resume" -> out.resume = true;
            case "--packet-out" -> out.packetOut = a[++i];
            case "--packet-audit-out" -> out.packetAuditOut = a[++i];
            case "--record-out" -> out.recordOut = a[++i];
            case "--dimension" -> { out.dimension = a[++i].toLowerCase(); out.explicitDimension = true; }
            default -> throw new IllegalArgumentException("unknown argument " + a[i]);
        }
        if (!out.rawPacketV6 && (out.loX < GRID_MIN || out.hiX > GRID_MAX || out.loZ < GRID_MIN || out.hiZ > GRID_MAX)) out.rawPacketV6 = true;
        if (out.rawPacketV6) {
            if (!out.explicitCx) { out.loX = RAW_GRID_MIN; out.hiX = RAW_GRID_MAX; }
            if (!out.explicitCz) { out.loZ = RAW_GRID_MIN; out.hiZ = RAW_GRID_MAX; }
        }
        if (out.help || out.provenanceSelftest || out.determinismSelftest) return out;
        if (!OVERWORLD.equals(out.dimension) && !NETHER.equals(out.dimension) && !END.equals(out.dimension)) throw new IllegalArgumentException("--dimension must be overworld, nether, or end");
        if (!"materialize".equals(out.mode) && !"export".equals(out.mode)) throw new IllegalArgumentException("--mode must be materialize or export");
        if (out.loX > out.hiX || out.loZ > out.hiZ || out.loX < out.gridMin() || out.hiX > out.gridMax() || out.loZ < out.gridMin() || out.hiZ > out.gridMax()) throw new IllegalArgumentException("ranges must lie in " + out.gridMin() + "..=" + out.gridMax());
        if ("materialize".equals(out.mode) && out.out != null) throw new IllegalArgumentException("materialize has no --out; it seals the persistent world");
        if ("export".equals(out.mode) && out.out == null) throw new IllegalArgumentException("export requires --out");
        if ((out.packetOut != null || out.recordOut != null) && (out.loX != out.hiX || out.loZ != out.hiZ)) throw new IllegalArgumentException("--packet-out and --record-out require exactly one chunk");
        return out;
    }

    static void usage() {
        System.out.println("materialize: LargeParityOracle --mode materialize [--dimension overworld|nether|end]");
        System.out.println("export:      LargeParityOracle --mode export --out /oracle/shard.lwp --cx LO HI --cz LO HI [--raw-packet] [--dimension overworld|nether|end] [--resume] [--packet-out /oracle/chunk.bin] [--packet-audit-out /oracle/shard.packet-audit] [--record-out /oracle/chunk.record]");
        System.out.println("control:     LargeParityOracle --provenance-selftest");
        System.out.println("control:     LargeParityOracle --determinism-selftest [--dimension overworld|nether|end]");
        System.out.println("materialize needs LODESTONE_ORACLE_WORLD_ROOT; export needs LODESTONE_ORACLE_FROZEN_WORLD_ROOT.");
    }

    static MessageDigest sha256() { try { return MessageDigest.getInstance("SHA-256"); } catch (Exception e) { throw new AssertionError(e); } }
    static byte[] digest(byte[] b) { return sha256().digest(b); }
    static String hex(byte[] b) { StringBuilder s = new StringBuilder(b.length * 2); for (byte v : b) s.append(String.format("%02x", v)); return s.toString(); }

    static byte[] header(Args a, long count, byte[] frozenDigest, byte[] payloadDigest) {
        ByteBuffer b = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        byte[] magic = a.magic(), domain = a.manifestDomain();
        b.put(magic).putShort((short)a.semanticVersion()).putShort((short)HEADER_BYTES).putShort((short)2).putShort((short)a.semanticVersion()).putInt(776).putLong(SEED);
        b.putInt(a.gridMin()).putInt(a.gridMax()).putInt(a.gridMin()).putInt(a.gridMax()).putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count);
        b.putShort((short)a.recordWidth()).putShort((short)0).put(digest(domain)).put(frozenDigest).put(payloadDigest);
        if (a.v6() || a.dimensionFormat()) b.put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
        return b.array();
    }

    static long resumeRecords(File f, Args a, long count, byte[] frozenDigest) throws Exception {
        if (!f.exists()) return 0;
        int width = a.recordWidth();
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * width || ((f.length() - HEADER_BYTES) % width) != 0) throw new IllegalStateException("resume refuses malformed v" + a.semanticVersion() + " shard: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN); byte[] magic = new byte[8]; b.get(magic);
            byte[] expectedMagic = a.magic(); int expectedVersion = a.semanticVersion(), expectedSchema = a.semanticVersion();
            if (!Arrays.equals(magic, expectedMagic) || b.getShort() != expectedVersion || b.getShort() != HEADER_BYTES || b.getShort() != 2 || b.getShort() != expectedSchema || b.getInt() != 776 || b.getLong() != SEED) throw new IllegalStateException("manifest format differs; resume requires the selected parity format: " + f);
            b.position(28);
            if (b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=width) throw new IllegalStateException("resume shard geometry differs: " + f);
            b.getShort(); byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected);
            if (!Arrays.equals(domain, digest(a.manifestDomain())) || !Arrays.equals(recordedFrozen, frozenDigest)) throw new IllegalStateException("resume schema or frozen-world identity differs: " + f);
            if (a.v6() || a.dimensionFormat()) { byte[] recordedDimension = new byte[32]; b.get(recordedDimension); if (!Arrays.equals(recordedDimension, digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)))) throw new IllegalStateException("resume dimension identity differs: " + f); }
            long records = (f.length() - HEADER_BYTES) / width;
            if (records == count) { MessageDigest actual = sha256(); byte[] buf = new byte[8192]; int n; while ((n = in.read(buf)) != -1) actual.update(buf, 0, n); if (!Arrays.equals(expected, actual.digest())) throw new IllegalStateException("resume payload checksum differs: " + f); }
            else if (!Arrays.equals(expected, new byte[32])) throw new IllegalStateException("partial shard has a non-zero final checksum: " + f);
            return records;
        }
    }

    static byte[] packetAuditHeader(Args a, long count, byte[] frozenDigest, byte[] payloadDigest) {
        ByteBuffer b = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        b.put(PACKET_AUDIT_MAGIC).putShort((short)6).putShort((short)HEADER_BYTES).putShort((short)3).putShort((short)6).putInt(776).putLong(SEED);
        b.putInt(a.gridMin()).putInt(a.gridMax()).putInt(a.gridMin()).putInt(a.gridMax()).putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count);
        b.putShort((short)DIGEST_BYTES).putShort((short)0).put(digest(PACKET_AUDIT_DOMAIN)).put(frozenDigest).put(payloadDigest).put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
        return b.array();
    }
    static long resumePacketAudits(File f, Args a, long count, byte[] frozenDigest) throws Exception {
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * DIGEST_BYTES || ((f.length() - HEADER_BYTES) % DIGEST_BYTES) != 0) throw new IllegalStateException("resume refuses malformed packet audit sidecar: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN); byte[] magic = new byte[8]; b.get(magic);
            if (!Arrays.equals(magic, PACKET_AUDIT_MAGIC) || b.getShort() != 6 || b.getShort() != HEADER_BYTES || b.getShort() != 3 || b.getShort() != 6 || b.getInt() != 776 || b.getLong() != SEED) throw new IllegalStateException("packet audit sidecar identity differs: " + f);
            b.position(28);
            if (b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=DIGEST_BYTES) throw new IllegalStateException("packet audit sidecar geometry differs: " + f);
            b.getShort(); byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected); byte[] recordedDimension = new byte[32]; b.get(recordedDimension);
            if (!Arrays.equals(domain, digest(PACKET_AUDIT_DOMAIN)) || !Arrays.equals(recordedFrozen, frozenDigest) || !Arrays.equals(recordedDimension, digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)))) throw new IllegalStateException("packet audit sidecar provenance differs: " + f);
            long records = (f.length() - HEADER_BYTES) / DIGEST_BYTES;
            if (records == count) { MessageDigest actual = sha256(); byte[] buf = new byte[8192]; int n; while ((n = in.read(buf)) != -1) actual.update(buf, 0, n); if (!Arrays.equals(expected, actual.digest())) throw new IllegalStateException("packet audit sidecar checksum differs: " + f); }
            else if (!Arrays.equals(expected, new byte[32])) throw new IllegalStateException("partial packet audit sidecar has a non-zero final checksum: " + f);
            return records;
        }
    }

    static <T> T privateMain(String name, Class<?>[] types, Object... values) throws Exception { Method method = Main.class.getDeclaredMethod(name, types); method.setAccessible(true); @SuppressWarnings("unchecked") T result = (T) method.invoke(null, values); return result; }
    static WorldStem loadWorld(DedicatedServerProperties properties, LevelStorageSource.LevelStorageAccess access, PackRepository packs, Dynamic<?> tag) throws Exception {
        WorldLoader.InitConfig config = privateMain("loadOrCreateConfig", new Class<?>[]{DedicatedServerProperties.class, Dynamic.class, boolean.class, PackRepository.class}, properties, tag, false, packs);
        return Util.blockUntilDone(executor -> WorldLoader.load(config, context -> {
            Registry<LevelStem> dimensions = context.datapackDimensions().lookupOrThrow(Registries.LEVEL_STEM);
            if (tag != null) { LevelDataAndDimensions data = LevelStorageSource.getLevelDataAndDimensions(access, tag, context.dataConfiguration(), dimensions, context.datapackWorldgen()); return new WorldLoader.DataLoadOutput<>(data.worldDataAndGenSettings(), data.dimensions().dimensionsRegistryAccess()); }
            try { return privateMain("createNewWorldData", new Class<?>[]{DedicatedServerSettings.class, WorldLoader.DataLoadContext.class, Registry.class, boolean.class, boolean.class}, new DedicatedServerSettings(Path.of("/work/server.properties")), context, dimensions, false, false); } catch (Exception e) { throw new IllegalStateException("creating new world data failed", e); }
        }, WorldStem::new, Util.backgroundExecutor(), executor)).get();
    }

    static Path copyReadOnlyWorld() throws Exception {
        String raw = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (raw == null || raw.isBlank()) throw new IllegalStateException("export requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT; source must be mounted read-only");
        String prepared = System.getenv("ORACLE_FROZEN_WORK_ROOT");
        if (prepared != null && !prepared.isBlank()) {
            Path work = Path.of(prepared);
            if (!Files.isDirectory(work)) throw new IllegalStateException("prepared frozen-world clone is missing: " + work);
            return work;
        }
        Path source = Path.of(raw); Path copy = Path.of("/work/frozen-world-copy");
        try (var paths = Files.walk(source)) { for (Path from : paths.sorted().toList()) { Path to = copy.resolve(source.relativize(from).toString()); if (Files.isDirectory(from)) Files.createDirectories(to); else Files.copy(from, to, StandardCopyOption.COPY_ATTRIBUTES); } }
        return copy;
    }
    static String materializationContract(Args a) { return a.v6() ? MATERIALIZATION_CONTRACT_V6 : MATERIALIZATION_CONTRACT; }
    static String freezeStamp(Args a) { return materializationContract(a) + "-" + a.dimension + ".freeze.sha256"; }
    static String progressFile(Args a) { return materializationContract(a) + "-" + a.dimension + ".materialize"; }
    static String progressMarker(Args a) { return materializationContract(a) + "-" + a.dimension + "-progress"; }
    static List<Path> legacyProvenancePaths(Path root) {
        List<Path> result = new ArrayList<>();
        result.add(root.resolve("lodestone-large-parity-v3.freeze.sha256"));
        result.add(root.resolve("lodestone-large-parity-v3.materialize"));
        result.add(root.resolve("lodestone-large-parity-v3.materialize.tmp"));
        for (String dimension : List.of(OVERWORLD, NETHER, END)) {
            result.add(root.resolve("lodestone-large-parity-v4-" + dimension + ".freeze.sha256"));
            result.add(root.resolve("lodestone-large-parity-v4-" + dimension + ".materialize"));
            result.add(root.resolve("lodestone-large-parity-v4-" + dimension + ".materialize.tmp"));
        }
        return result;
    }
    static void rejectLegacyProvenance(Path root) {
        for (Path path : legacyProvenancePaths(root)) if (Files.exists(path)) {
            throw new IllegalStateException("obsolete concurrent materialization provenance is refused: " + path + "; create a new empty root under " + MATERIALIZATION_CONTRACT);
        }
    }
    static String formatLabel(Args a) { return "v" + a.semanticVersion() + "-" + a.dimension; }
    static byte[] worldTreeDigest(Path root, Args a) throws Exception {
        MessageDigest sha = sha256();
        try (var paths = Files.walk(root)) { for (Path path : paths.filter(Files::isRegularFile).filter(p -> !p.getFileName().toString().equals(freezeStamp(a))).sorted().toList()) {
            byte[] name = root.relativize(path).toString().replace(File.separatorChar, '/').getBytes(StandardCharsets.UTF_8);
            sha.update(ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN).putInt(name.length).array()); sha.update(name); sha.update(ByteBuffer.allocate(8).order(ByteOrder.BIG_ENDIAN).putLong(Files.size(path)).array());
            try (var input = Files.newInputStream(path)) { byte[] buf = new byte[8192]; for (int n; (n = input.read(buf)) != -1;) sha.update(buf, 0, n); }
        } }
        return sha.digest();
    }
    static byte[] frozenDigest(Path root, Args a) throws Exception {
        rejectLegacyProvenance(root);
        Path stamp = root.resolve(freezeStamp(a)); if (!Files.isRegularFile(stamp)) throw new IllegalStateException("frozen world has no selected-dimension seal: " + stamp);
        byte[] actual = worldTreeDigest(root, a); String expected = Files.readString(stamp, StandardCharsets.US_ASCII).trim(); if (!hex(actual).equals(expected)) throw new IllegalStateException("frozen world differs from its seal; re-materialize before export"); return actual;
    }
    interface CheckedWork { void run() throws Exception; }
    static void assertProvenanceRefusal(String arm, CheckedWork work) throws Exception {
        try {
            work.run();
        } catch (IllegalStateException expected) {
            if (expected.getMessage().contains("obsolete concurrent materialization provenance")) return;
            throw new AssertionError(arm + " failed for the wrong reason: " + expected.getMessage(), expected);
        }
        throw new AssertionError(arm + " accepted obsolete concurrent materialization provenance");
    }
    static void provenanceSelftest() throws Exception {
        Path root = Files.createTempDirectory("large-parity-provenance-");
        Args nether = new Args(); nether.dimension = NETHER; nether.explicitDimension = true;
        try {
            Files.writeString(root.resolve("lodestone-large-parity-v3.freeze.sha256"), "old\n", StandardCharsets.US_ASCII);
            assertProvenanceRefusal("export old v3 seal", () -> frozenDigest(root, nether));
            Files.delete(root.resolve("lodestone-large-parity-v3.freeze.sha256"));
            Files.writeString(root.resolve("lodestone-large-parity-v4-nether.materialize"), "old\n", StandardCharsets.US_ASCII);
            assertProvenanceRefusal("resume old v4 progress", () -> materialize(nether, root));
        } finally {
            try (var paths = Files.walk(root)) { for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) Files.deleteIfExists(path); }
        }
        System.out.println("provenance selftest ok: old v3 export seal and v4 resume progress refused");
    }

    static int determinismCoordinate(String name) {
        String value = System.getenv(name);
        if (value == null || value.isBlank()) return 0;
        try { return Integer.parseInt(value); } catch (NumberFormatException e) { throw new IllegalStateException(name + " must be an integer: " + value, e); }
    }
    static List<Integer> differingOffsets(byte[] left, byte[] right) {
        List<Integer> offsets = new ArrayList<>();
        int common = Math.min(left.length, right.length);
        for (int i = 0; i < common; i++) if (left[i] != right[i]) offsets.add(i);
        for (int i = common; i < Math.max(left.length, right.length); i++) offsets.add(i);
        return offsets;
    }
    static void determinismSelftest(Args a, Path frozenRoot) throws Exception {
        byte[] frozen = frozenDigest(frozenRoot, a);
        Path copy = copyReadOnlyWorld();
        if (!Arrays.equals(frozen, frozenDigest(copy, a))) throw new IllegalStateException("prepared frozen-world clone differs from its sealed source");
        try { runServer(copy, true, a, (server, level) -> {
            int cx = determinismCoordinate("ORACLE_DETERMINISM_X"), cz = determinismCoordinate("ORACLE_DETERMINISM_Z");
            ChunkPos pos = new ChunkPos(cx, cz);
            CompletableFuture<?> future = server.submit(() -> level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)).join();
            net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)future.join();
            if (!result.isSuccess()) throw new IllegalStateException("chunk load failed at " + pos + ": " + result.getError());
            settleMaterializedBatch(server, level, List.of(pos));
            try {
                LevelChunk chunk = server.submit(() -> level.getChunkSource().getChunkNow(cx, cz)).join();
                if (chunk == null) throw new IllegalStateException("loaded chunk was evicted: " + pos);
                byte[] stableA = packetBody(server, chunk, level, PacketOrder.STABLE);
                byte[] stableB = packetBody(server, chunk, level, PacketOrder.STABLE);
                byte[] reversed = packetBody(server, chunk, level, PacketOrder.REVERSED);
                List<Integer> positive = differingOffsets(stableA, stableB), negative = differingOffsets(stableA, reversed);
                if (!positive.isEmpty()) throw new AssertionError("stabilized packet exports differ at offsets " + positive);
                if (negative.isEmpty()) throw new AssertionError("reversed packet-order control unexpectedly matched stabilized bytes");
                System.out.println("determinism selftest: dimension=" + a.dimension + " chunk=" + pos + " negative differing-offsets=" + negative + " positive differing-offsets=" + positive);
            } finally {
                server.submit(() -> level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)).join();
            }
            });
        } finally {
            if (!Arrays.equals(frozen, frozenDigest(frozenRoot, a))) throw new IllegalStateException("sealed frozen-world source changed during determinism selftest");
        }
    }

    static void writeUtf8(DataOutputStream out, String value) throws Exception { byte[] b = value.getBytes(StandardCharsets.UTF_8); out.writeInt(b.length); out.write(b); }
    static void canonicalTag(DataOutputStream out, Tag tag) throws Exception {
        out.writeByte(tag.getId());
        switch (tag.getId()) {
            case Tag.TAG_END -> { }
            case Tag.TAG_BYTE -> out.writeByte(((NumericTag)tag).byteValue());
            case Tag.TAG_SHORT -> out.writeShort(((NumericTag)tag).shortValue());
            case Tag.TAG_INT -> out.writeInt(((NumericTag)tag).intValue());
            case Tag.TAG_LONG -> out.writeLong(((NumericTag)tag).longValue());
            case Tag.TAG_FLOAT -> out.writeInt(Float.floatToRawIntBits(((NumericTag)tag).floatValue()));
            case Tag.TAG_DOUBLE -> out.writeLong(Double.doubleToRawLongBits(((NumericTag)tag).doubleValue()));
            case Tag.TAG_BYTE_ARRAY -> { byte[] values = ((ByteArrayTag)tag).getAsByteArray(); out.writeInt(values.length); out.write(values); }
            case Tag.TAG_STRING -> writeUtf8(out, ((StringTag)tag).value());
            case Tag.TAG_LIST -> { ListTag values = (ListTag)tag; out.writeInt(values.size()); for (Tag value : values) canonicalTag(out, value); }
            case Tag.TAG_COMPOUND -> { CompoundTag values = (CompoundTag)tag; List<String> keys = new ArrayList<>(values.keySet()); keys.sort((left, right) -> Arrays.compareUnsigned(left.getBytes(StandardCharsets.UTF_8), right.getBytes(StandardCharsets.UTF_8))); out.writeInt(keys.size()); for (String key : keys) { writeUtf8(out, key); canonicalTag(out, values.get(key)); } }
            case Tag.TAG_INT_ARRAY -> { int[] values = ((IntArrayTag)tag).getAsIntArray(); out.writeInt(values.length); for (int value : values) out.writeInt(value); }
            case Tag.TAG_LONG_ARRAY -> { long[] values = ((LongArrayTag)tag).getAsLongArray(); out.writeInt(values.length); for (long value : values) out.writeLong(value); }
            default -> throw new IllegalStateException("unknown NBT tag " + tag.getId());
        }
    }
    static boolean fullSky(byte[] bytes) {
        for (byte value : bytes) if (value != (byte)0xff) return false;
        return true;
    }
    static void canonicalLight(DataOutputStream out, java.util.BitSet present, java.util.BitSet empty, List<byte[]> arrays, int sections, boolean sky, boolean normalizeFullSkyTail) throws Exception {
        byte[][] data = new byte[sections][];
        int cursor = 0;
        for (int section = 0; section < sections; section++) {
            if (present.get(section) && empty.get(section)) throw new IllegalStateException("light section has both present and empty bits");
            if (present.get(section)) { data[section] = arrays.get(cursor++); if (data[section].length != 2048) throw new IllegalStateException("light array length"); }
        }
        if (cursor != arrays.size()) throw new IllegalStateException("light mask/array count differs");
        // Initial chunks are enabled after their supplied layers are queued. A
        // trailing all-15 sky layer is therefore equivalent to omitted data;
        // retain every lower or mixed layer, where omission can change lookup.
        int lastRequired = sections - 1;
        if (sky && normalizeFullSkyTail) while (lastRequired >= 0 && (!present.get(lastRequired) || fullSky(data[lastRequired]))) lastRequired--;
        for (int section = 0; section < sections; section++) {
            if (sky && normalizeFullSkyTail && section > lastRequired && present.get(section) && fullSky(data[section])) out.writeByte(0);
            else if (present.get(section)) { out.writeByte(2); out.write(data[section]); }
            else if (empty.get(section)) out.writeByte(1); else out.writeByte(0);
        }
    }
    static byte[] canonicalLightBytes(java.util.BitSet present, java.util.BitSet empty, List<byte[]> arrays, int sections, boolean sky) throws Exception {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream(); DataOutputStream out = new DataOutputStream(bytes); canonicalLight(out, present, empty, arrays, sections, sky, true); out.flush(); return bytes.toByteArray();
    }
    static void verifyCanonicalLightContract() throws Exception {
        byte[] full = new byte[2048]; Arrays.fill(full, (byte)0xff); byte[] nearFull = full.clone(); nearFull[1487] = (byte)0xef;
        java.util.BitSet present = new java.util.BitSet(); present.set(0);
        byte[] omitted = canonicalLightBytes(new java.util.BitSet(), new java.util.BitSet(), List.of(), 1, true);
        byte[] explicitFull = canonicalLightBytes(present, new java.util.BitSet(), List.of(full), 1, true);
        if (!Arrays.equals(omitted, explicitFull) || explicitFull.length != 1) throw new IllegalStateException("full sky canonicalization lost compact default");
        byte[] nonDefault = canonicalLightBytes(present, new java.util.BitSet(), List.of(nearFull), 1, true);
        if (Arrays.equals(nonDefault, explicitFull)) throw new IllegalStateException("mixed sky layer was normalized away");
        java.util.BitSet empty = new java.util.BitSet(); empty.set(0);
        byte[] blockOmitted = canonicalLightBytes(new java.util.BitSet(), new java.util.BitSet(), List.of(), 1, false);
        byte[] blockEmpty = canonicalLightBytes(new java.util.BitSet(), empty, List.of(), 1, false);
        byte[] blockFull = canonicalLightBytes(present, new java.util.BitSet(), List.of(full), 1, false);
        if (Arrays.equals(blockOmitted, blockEmpty) || Arrays.equals(blockOmitted, blockFull)) throw new IllegalStateException("block-light representation was normalized");
    }
    enum PacketOrder { STABLE, REVERSED }
    static Field field(Class<?> type, String name) {
        for (Class<?> current = type; current != null; current = current.getSuperclass()) try { Field result = current.getDeclaredField(name); result.setAccessible(true); return result; } catch (NoSuchFieldException ignored) { }
        throw new IllegalStateException("compiled packet shape has no " + name + " field");
    }
    static Object field(Object target, String name) {
        try { return field(target.getClass(), name).get(target); } catch (ReflectiveOperationException e) { throw new IllegalStateException("cannot inspect compiled packet " + name, e); }
    }
    static void setField(Object target, String name, Object value) {
        try { field(target.getClass(), name).set(target, value); } catch (ReflectiveOperationException e) { throw new IllegalStateException("cannot stabilize compiled packet " + name, e); }
    }
    static void stabilizeCompound(CompoundTag tag) {
        List<Map.Entry<String, Tag>> entries = new ArrayList<>(tag.entrySet());
        entries.sort((left, right) -> Arrays.compareUnsigned(left.getKey().getBytes(StandardCharsets.UTF_8), right.getKey().getBytes(StandardCharsets.UTF_8)));
        for (Map.Entry<String, Tag> entry : entries) stabilizeTag(entry.getValue());
        Map<String, Tag> tags = new LinkedHashMap<>(); for (Map.Entry<String, Tag> entry : entries) tags.put(entry.getKey(), entry.getValue());
        setField(tag, "tags", tags);
    }
    static void stabilizeTag(Tag tag) {
        if (tag instanceof CompoundTag compound) stabilizeCompound(compound);
        else if (tag instanceof ListTag list) for (Tag value : list) stabilizeTag(value);
    }
    @SuppressWarnings({"rawtypes", "unchecked"})
    static PalettedContainer canonicalContainer(PalettedContainer source, int size, int depth) {
        Object strategy = field(source, "strategy");
        Object first = source.get(0, 0, 0);
        PalettedContainer result = new PalettedContainer(first, (Strategy)strategy);
        for (int y = 0; y < depth; y++) for (int z = 0; z < size; z++) for (int x = 0; x < size; x++) result.getAndSetUnchecked(x, y, z, source.get(x, y, z));
        return result;
    }
    static void canonicalizeSection(LevelChunkSection section) {
        PalettedContainer<?> states = section.getStates();
        PalettedContainer<?> canonicalStates = canonicalContainer(states, 16, 16);
        ByteBuf stateBytes = Unpooled.buffer(canonicalStates.getSerializedSize());
        try { canonicalStates.write(new FriendlyByteBuf(stateBytes)); states.read(new FriendlyByteBuf(stateBytes)); } finally { stateBytes.release(); }
        PalettedContainer<?> biomes = (PalettedContainer<?>)section.getBiomes();
        PalettedContainer<?> canonicalBiomes = canonicalContainer(biomes, 4, 4);
        ByteBuf biomeBytes = Unpooled.buffer(canonicalBiomes.getSerializedSize());
        try { canonicalBiomes.write(new FriendlyByteBuf(biomeBytes)); section.readBiomes(new FriendlyByteBuf(biomeBytes)); } finally { biomeBytes.release(); }
    }
    @SuppressWarnings({"rawtypes", "unchecked"})
    static void stabilizePacket(ClientboundLevelChunkWithLightPacket packet, ServerLevel level, PacketOrder order) {
        ClientboundLevelChunkPacketData data = packet.getChunkData();
        Map<Heightmap.Types, long[]> heightmaps = data.getHeightmaps();
        List<Map.Entry<Heightmap.Types, long[]>> maps = new ArrayList<>(heightmaps.entrySet());
        maps.sort(Comparator.comparingInt(entry -> entry.getKey().ordinal()));
        if (order == PacketOrder.REVERSED) maps.sort(Comparator.comparingInt((Map.Entry<Heightmap.Types, long[]> entry) -> entry.getKey().ordinal()).reversed());
        Map<Heightmap.Types, long[]> orderedHeightmaps = new LinkedHashMap<>(); for (Map.Entry<Heightmap.Types, long[]> entry : maps) orderedHeightmaps.put(entry.getKey(), entry.getValue());
        setField(data, "heightmaps", orderedHeightmaps);
        List entities = (List)field(data, "blockEntitiesData");
        Registry types = level.registryAccess().lookupOrThrow(Registries.BLOCK_ENTITY_TYPE);
        Comparator<Object> comparator = Comparator.comparingInt(value -> (int)field(value, "packedXZ"))
            .thenComparingInt(value -> (int)field(value, "y"))
            .thenComparingInt(value -> types.getId(field(value, "type")));
        if (order == PacketOrder.REVERSED) comparator = comparator.reversed();
        entities.sort(comparator);
        for (Object entity : entities) { Object tag = field(entity, "tag"); if (tag instanceof Tag value) stabilizeTag(value); }
    }
    static byte[] packetBody(MinecraftServer server, LevelChunk chunk, ServerLevel level) { return packetBody(server, chunk, level, PacketOrder.STABLE); }
    static byte[] packetBody(MinecraftServer server, LevelChunk chunk, ServerLevel level, PacketOrder order) {
        for (LevelChunkSection section : chunk.getSections()) canonicalizeSection(section);
        ClientboundLevelChunkWithLightPacket packet = new ClientboundLevelChunkWithLightPacket(chunk, level.getLightEngine(), null, null);
        stabilizePacket(packet, level, order);
        ByteBuf bytes = Unpooled.buffer(); RegistryFriendlyByteBuf out = new RegistryFriendlyByteBuf(bytes, server.registryAccess());
        ClientboundLevelChunkWithLightPacket.STREAM_CODEC.encode(out, packet);
        byte[] body = new byte[out.readableBytes()]; out.getBytes(out.readerIndex(), body); out.release(); return body;
    }
    static byte[] semanticRecord(ServerLevel level, LevelChunk chunk, Args a) throws Exception {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream(120_000); DataOutputStream out = new DataOutputStream(bytes);
        out.write(a.v5() ? RECORD_DOMAIN_V5 : a.dimensionFormat() ? RECORD_DOMAIN_V4 : RECORD_DOMAIN); out.writeInt(chunk.getPos().x()); out.writeInt(chunk.getPos().z());
        if (a.dimensionFormat()) out.write(a.dimensionKey().getBytes(StandardCharsets.UTF_8));
        ClientboundLevelChunkWithLightPacket packet = new ClientboundLevelChunkWithLightPacket(chunk, level.getLightEngine(), null, null);
        List<Map.Entry<Heightmap.Types, long[]>> maps = new ArrayList<>(packet.getChunkData().getHeightmaps().entrySet()); maps.sort(Comparator.comparingInt(entry -> entry.getKey().ordinal()));
        out.writeInt(maps.size()); for (Map.Entry<Heightmap.Types, long[]> entry : maps) { out.writeInt(entry.getKey().ordinal()); Heightmap map = null; for (Map.Entry<Heightmap.Types, Heightmap> candidate : chunk.getHeightmaps()) if (candidate.getKey() == entry.getKey()) { map = candidate.getValue(); break; } if (map == null) throw new IllegalStateException("packet heightmap missing from chunk"); for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) out.writeInt(map.getFirstAvailable(x, z)); }
        Registry<Biome> biomes = level.registryAccess().lookupOrThrow(Registries.BIOME);
        for (LevelChunkSection section : chunk.getSections()) {
            for (int y = 0; y < 16; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) out.writeInt(Block.getId(section.getBlockState(x, y, z)));
            for (int y = 0; y < 4; y++) for (int z = 0; z < 4; z++) for (int x = 0; x < 4; x++) { Holder<Biome> biome = section.getNoiseBiome(x, y, z); out.writeInt(biomes.getId(biome.value())); }
        }
        Registry<net.minecraft.world.level.block.entity.BlockEntityType<?>> types = level.registryAccess().lookupOrThrow(Registries.BLOCK_ENTITY_TYPE);
        List<BlockEntity> entities = new ArrayList<>(chunk.getBlockEntities().values()); entities.sort(Comparator.comparingInt((BlockEntity e) -> e.getBlockPos().getX() & 15).thenComparingInt(e -> e.getBlockPos().getY()).thenComparingInt(e -> e.getBlockPos().getZ() & 15).thenComparingInt(e -> types.getId(e.getType())));
        out.writeInt(entities.size()); for (BlockEntity entity : entities) { out.writeByte(entity.getBlockPos().getX() & 15); out.writeShort(entity.getBlockPos().getY()); out.writeByte(entity.getBlockPos().getZ() & 15); out.writeInt(types.getId(entity.getType())); CompoundTag tag = entity.getUpdateTag(level.registryAccess()); if (tag.isEmpty()) out.writeByte(Tag.TAG_END); else canonicalTag(out, tag); }
        var light = packet.getLightData(); int lightSections = chunk.getSections().length + 2; canonicalLight(out, light.getSkyYMask(), light.getEmptySkyYMask(), light.getSkyUpdates(), lightSections, true, a.v5()); canonicalLight(out, light.getBlockYMask(), light.getEmptyBlockYMask(), light.getBlockUpdates(), lightSections, false, false); out.flush(); return bytes.toByteArray();
    }

    static void settleMaterializedBatch(MinecraftServer server, ServerLevel level, List<ChunkPos> positions) {
        // A completed FULL future does not order deferred sky propagation before
        // its loading ticket is removed. Run only scheduler work (no resident
        // chunk ticks), then queue a post-update fence for every exact batch
        // column while all of its tickets still exist.
        server.submit(() -> level.getChunkSource().tick(() -> true, false)).join();
        List<CompletableFuture<?>> fences = server.submit(() -> {
            List<CompletableFuture<?>> result = new ArrayList<>(positions.size());
            for (ChunkPos pos : positions) result.add(level.getChunkSource().getLightEngine().waitForPendingTasks(pos.x(), pos.z()));
            return result;
        }).join();
        for (CompletableFuture<?> fence : fences) fence.join();
        server.submit(() -> {
            for (ChunkPos pos : positions) {
                LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z());
                if (chunk == null || !chunk.isLightCorrect()) {
                    throw new IllegalStateException("materialized chunk did not reach settled FULL light: " + pos);
                }
            }
        }).join();
    }

    static void materializeOne(MinecraftServer server, ServerLevel level, ChunkPos pos) {
        CompletableFuture<?> future = server.submit(() -> level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)).join();
        net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)future.join();
        if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + pos + ": " + result.getError());
        settleMaterializedBatch(server, level, List.of(pos));
        server.submit(() -> level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)).join();
    }

    static void loadBatch(MinecraftServer server, ServerLevel level, Args a, List<ChunkPos> positions, boolean capture, List<byte[]> out) { loadBatch(server, level, a, positions, capture, out, null); }
    static void loadBatch(MinecraftServer server, ServerLevel level, Args a, List<ChunkPos> positions, boolean capture, List<byte[]> out, List<byte[]> packetAudits) {
        if (!capture) {
            for (ChunkPos pos : positions) materializeOne(server, level, pos);
            return;
        }
        List<ChunkPos> loaded = positions;
        List<CompletableFuture<?>> futures = server.submit(() -> { List<CompletableFuture<?>> result = new ArrayList<>(loaded.size()); for (ChunkPos pos : loaded) result.add(level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)); return result; }).join();
        for (int i = 0; i < loaded.size(); i++) { net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)futures.get(i).join(); if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + loaded.get(i) + ": " + result.getError()); }
        if (capture) out.addAll(server.submit(() -> { try { List<byte[]> result = new ArrayList<>(positions.size()); for (ChunkPos pos : positions) { LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z()); if (chunk == null) throw new IllegalStateException("loaded chunk was evicted: " + pos); byte[] packet = a.v6() || diagnosticPacketOut != null ? packetBody(server, chunk, level) : null; if (diagnosticPacketOut != null) Files.write(Path.of(diagnosticPacketOut), packet); if (a.v6()) { byte[] full = digest(packet); if (packetAudits != null) packetAudits.add(full); result.add(Arrays.copyOf(full, RAW_RECORD_BYTES)); } else { byte[] record = semanticRecord(level, chunk, a); if (diagnosticRecordOut != null) Files.write(Path.of(diagnosticRecordOut), record); result.add(digest(record)); } } return result; } catch (Exception e) { throw new IllegalStateException("canonical chunk export failed", e); } }).join());
        server.submit(() -> { for (ChunkPos pos : loaded) level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0); }).join();
    }

    record MaterializeProgress(int minX, int maxX, int minZ, int maxZ, int tilesX, int tilesZ, int epochTiles, int nextTile, int inflightEnd) {
        int totalTiles() { return Math.multiplyExact(tilesX, tilesZ); }
        MaterializeProgress withInflight(int end) { return new MaterializeProgress(minX, maxX, minZ, maxZ, tilesX, tilesZ, epochTiles, nextTile, end); }
        MaterializeProgress withNext(int next) { return new MaterializeProgress(minX, maxX, minZ, maxZ, tilesX, tilesZ, epochTiles, next, -1); }
    }

    static int materializeEpochTiles() {
        String value = System.getenv("ORACLE_MATERIALIZE_EPOCH_TILES");
        if (value == null || value.isBlank()) throw new IllegalStateException("materialize requires ORACLE_MATERIALIZE_EPOCH_TILES; use large-parity.sh, which starts a fresh JVM for every epoch");
        try { int parsed = Integer.parseInt(value); if (parsed <= 0) throw new NumberFormatException(); return parsed; }
        catch (NumberFormatException e) { throw new IllegalStateException("ORACLE_MATERIALIZE_EPOCH_TILES must be a positive integer: " + value, e); }
    }

    static String progressText(Args a, MaterializeProgress progress) {
        String marker = progressMarker(a);
        return marker + "=1\n"
            + "seed=" + SEED + "\n"
            + "tile-size=" + MATERIALIZE_TILE + "\n"
            + "min-x=" + progress.minX + "\nmax-x=" + progress.maxX + "\nmin-z=" + progress.minZ + "\nmax-z=" + progress.maxZ + "\n"
            + "tiles-x=" + progress.tilesX + "\ntiles-z=" + progress.tilesZ + "\nepoch-tiles=" + progress.epochTiles + "\n"
            + "next-tile=" + progress.nextTile + "\ninflight-end=" + progress.inflightEnd + "\n";
    }

    static MaterializeProgress readProgress(Path root, Args a) throws Exception {
        String progressName = progressFile(a); Path progress = root.resolve(progressName), temporary = root.resolve(progressName + ".tmp");
        if (Files.exists(temporary)) throw new IllegalStateException("materialization progress has an unfinished atomic update; refusing to resume: " + temporary);
        if (!Files.isRegularFile(progress)) throw new IllegalStateException("materialization root has no validated progress: " + root);
        Map<String, String> values = new HashMap<>();
        for (String line : Files.readAllLines(progress, StandardCharsets.US_ASCII)) {
            int split = line.indexOf('=');
            if (split <= 0 || values.put(line.substring(0, split), line.substring(split + 1)) != null) throw new IllegalStateException("malformed materialization progress: " + progress);
        }
        String marker = progressMarker(a);
        if (values.size() != 12 || !"1".equals(values.get(marker)) || !Long.toString(SEED).equals(values.get("seed")) || !Integer.toString(MATERIALIZE_TILE).equals(values.get("tile-size"))) throw new IllegalStateException("materialization progress provenance differs: " + progress);
        try {
            MaterializeProgress result = new MaterializeProgress(Integer.parseInt(values.get("min-x")), Integer.parseInt(values.get("max-x")), Integer.parseInt(values.get("min-z")), Integer.parseInt(values.get("max-z")), Integer.parseInt(values.get("tiles-x")), Integer.parseInt(values.get("tiles-z")), Integer.parseInt(values.get("epoch-tiles")), Integer.parseInt(values.get("next-tile")), Integer.parseInt(values.get("inflight-end")));
            if (result.minX > result.maxX || result.minZ > result.maxZ || result.tilesX != (result.maxX - result.minX) / MATERIALIZE_TILE + 1 || result.tilesZ != (result.maxZ - result.minZ) / MATERIALIZE_TILE + 1 || result.epochTiles <= 0 || result.nextTile < 0 || result.nextTile > result.totalTiles() || result.inflightEnd < -1 || result.inflightEnd > result.totalTiles()) throw new IllegalStateException("materialization progress is out of range: " + progress);
            return result;
        } catch (NumberFormatException e) { throw new IllegalStateException("materialization progress contains a non-integer: " + progress, e); }
    }

    static void writeProgress(Path root, Args a, MaterializeProgress progress) throws Exception {
        String progressName = progressFile(a); Path destination = root.resolve(progressName), temporary = root.resolve(progressName + ".tmp");
        Files.writeString(temporary, progressText(a, progress), StandardCharsets.US_ASCII);
        try { Files.move(temporary, destination, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING); }
        catch (AtomicMoveNotSupportedException e) { Files.move(temporary, destination, StandardCopyOption.REPLACE_EXISTING); }
    }

    static void verifyProgress(Args a, MaterializeProgress progress, int epochTiles) {
        int haloMin = a.v6() ? RAW_HALO_MIN : HALO_MIN, haloMax = a.v6() ? RAW_HALO_MAX : HALO_MAX;
        int minX = Math.max(haloMin, a.loX - 1), maxX = Math.min(haloMax, a.hiX + 1), minZ = Math.max(haloMin, a.loZ - 1), maxZ = Math.min(haloMax, a.hiZ + 1);
        if (progress.minX != minX || progress.maxX != maxX || progress.minZ != minZ || progress.maxZ != maxZ || progress.epochTiles != epochTiles) throw new IllegalStateException("materialization geometry or epoch size differs from durable progress; refusing gap or reorder");
        if (progress.inflightEnd != -1) throw new IllegalStateException("previous materialization epoch did not exit cleanly; refusing to resume uncertain world state at tiles " + progress.nextTile + ".." + progress.inflightEnd);
    }

    static void materialize(Args a, Path root) throws Exception {
        // The halo is part of the frozen state: a feature may write one chunk past
        // the requested grid. This phase records no baseline bytes.
        // Each epoch is a distinct JVM because server shutdown closes shared work
        // executors. The journal is written before work starts and only advances
        // after runServer has closed and flushed, so an interrupted epoch fails
        // closed rather than silently reordering or repeating feature work.
        Files.createDirectories(root);
        rejectLegacyProvenance(root);
        int epochTiles = materializeEpochTiles();
        if (Files.exists(root.resolve(freezeStamp(a)))) throw new IllegalStateException("materialize refuses an already frozen world: " + root);
        MaterializeProgress progress;
        if (Files.exists(root.resolve(progressFile(a))) || Files.exists(root.resolve(progressFile(a) + ".tmp"))) {
            progress = readProgress(root, a); verifyProgress(a, progress, epochTiles);
        } else {
            try (var entries = Files.list(root)) {
                if (entries.findAny().isPresent()) throw new IllegalStateException("materialize requires an empty world root or its validated " + MATERIALIZATION_CONTRACT + " progress journal: " + root);
            }
            int haloMin = a.v6() ? RAW_HALO_MIN : HALO_MIN, haloMax = a.v6() ? RAW_HALO_MAX : HALO_MAX;
            int minX = Math.max(haloMin, a.loX - 1), maxX = Math.min(haloMax, a.hiX + 1), minZ = Math.max(haloMin, a.loZ - 1), maxZ = Math.min(haloMax, a.hiZ + 1);
            progress = new MaterializeProgress(minX, maxX, minZ, maxZ, (maxX - minX) / MATERIALIZE_TILE + 1, (maxZ - minZ) / MATERIALIZE_TILE + 1, epochTiles, 0, -1);
            writeProgress(root, a, progress);
        }
        if (progress.nextTile == progress.totalTiles()) {
            byte[] frozen = worldTreeDigest(root, a); Files.writeString(root.resolve(freezeStamp(a)), hex(frozen) + "\n", StandardCharsets.US_ASCII); System.err.println("[large-parity] sealed " + a.dimension + " frozen world " + hex(frozen)); return;
        }
        int end = (int)Math.min(progress.totalTiles(), (long)progress.nextTile + progress.epochTiles);
        writeProgress(root, a, progress.withInflight(end));
        MaterializeProgress current = progress;
        runServer(root, false, a, (server, level) -> {
            long start = System.nanoTime();
            for (int tile = current.nextTile; tile < end; tile++) {
                int x0 = current.minX + (tile % current.tilesX) * MATERIALIZE_TILE, z0 = current.minZ + (tile / current.tilesX) * MATERIALIZE_TILE;
                List<ChunkPos> positions = new ArrayList<>(MATERIALIZE_TILE * MATERIALIZE_TILE); for (int z = z0; z <= Math.min(current.maxZ, z0 + MATERIALIZE_TILE - 1); z++) for (int x = x0; x <= Math.min(current.maxX, x0 + MATERIALIZE_TILE - 1); x++) positions.add(new ChunkPos(x, z));
                loadBatch(server, level, a, positions, false, new ArrayList<>());
                System.err.printf("[large-parity %s] materialized-tile=%d/%d epoch=%d..%d rate=%.1f tiles/s%n", formatLabel(a), tile + 1, current.totalTiles(), current.nextTile + 1, end, (tile - current.nextTile + 1) / ((System.nanoTime() - start) / 1_000_000_000.0));
            }
        });
        progress = current.withNext(end); writeProgress(root, a, progress);
        if (end == progress.totalTiles()) { byte[] frozen = worldTreeDigest(root, a); Files.writeString(root.resolve(freezeStamp(a)), hex(frozen) + "\n", StandardCharsets.US_ASCII); System.err.println("[large-parity] sealed " + a.dimension + " frozen world " + hex(frozen)); }
        else System.err.printf("[large-parity %s] clean epoch complete; next tile %d/%d%n", formatLabel(a), end, progress.totalTiles());
    }

    interface ServerWork { void run(MinecraftServer server, ServerLevel level) throws Exception; }
    static ServerLevel selectedLevel(MinecraftServer server, Args a) {
        for (ServerLevel level : server.getAllLevels()) if (level.dimension().identifier().toString().equals(a.dimensionKey())) return level;
        return null;
    }
    static void runServer(Path root, boolean requireExisting, Args a, ServerWork work) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); Bootstrap.validate(); Files.createDirectories(root); DedicatedServerSettings settings = new DedicatedServerSettings(Path.of("/work/server.properties")); LevelStorageSource storage = LevelStorageSource.createDefault(root);
        LevelStorageSource.LevelStorageAccess access = storage.validateAndCreateAccess(settings.getProperties().levelName); Dynamic<?> tag = access.hasWorldData() ? access.getUnfixedDataTagWithFallback() : null; if (requireExisting && tag == null) throw new IllegalStateException("frozen world is missing level data");
        PackRepository packs = ServerPacksSource.createPackRepository(access); WorldStem stem = loadWorld(settings.getProperties(), access, packs, tag); Services services = Services.create(new YggdrasilAuthenticationService(Proxy.NO_PROXY), root.toFile()); NotificationManager notifications = new NotificationManager(); ManagementServer management = JsonRpc.create(settings, notifications);
        DedicatedServer server = MinecraftServer.spin(thread -> { DedicatedServer s = new DedicatedServer(thread, access, packs, stem, Optional.empty(), settings, DataFixers.getDataFixer(), services, management, notifications); notifications.setServer(s); s.setPort(25565); return s; });
        try {
            while (server.overworld() == null) Thread.sleep(25);
            // The server publishes the overworld before finishing its dimension
            // loop. Wait for the requested level so Nether/End work cannot race
            // startup and incorrectly report that a valid dimension is absent.
            ServerLevel level = null;
            long deadline = System.nanoTime() + 60_000_000_000L;
            while (level == null && System.nanoTime() < deadline) {
                level = selectedLevel(server, a);
                if (level == null) Thread.sleep(25);
            }
            if (level == null) throw new IllegalStateException("selected dimension is unavailable: " + a.dimensionKey() + "; loaded=" + server.levelKeys());
            work.run(server, level);
        } finally { server.halt(true); stem.close(); access.close(); }
    }
    static void export(Args a, Path frozenRoot) throws Exception {
        diagnosticPacketOut = a.packetOut; diagnosticRecordOut = a.recordOut; byte[] frozen = frozenDigest(frozenRoot, a); Path copy = copyReadOnlyWorld();
        if (!Arrays.equals(frozen, frozenDigest(copy, a))) throw new IllegalStateException("prepared frozen-world clone differs from its sealed source");
        long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1); File out = new File(a.out); long done = a.resume ? resumeRecords(out, a, count, frozen) : 0;
        File packetAudit = a.v6() ? new File(a.packetAuditOut == null ? a.out + ".packet-audit" : a.packetAuditOut) : null;
        if (a.v6() && a.resume && done > 0) { long auditDone = resumePacketAudits(packetAudit, a, count, frozen); if (auditDone != done) throw new IllegalStateException("packet audit sidecar is not aligned with manifest records: " + packetAudit); }
        if (done == count) { System.err.println("[large-parity " + formatLabel(a) + "] authenticated shard already complete: " + out); return; } if (out.getParentFile() != null) out.getParentFile().mkdirs();
        if (packetAudit != null && packetAudit.getParentFile() != null) packetAudit.getParentFile().mkdirs();
        try { runServer(copy, true, a, (server, level) -> { MessageDigest payload = sha256(), packetPayload = sha256(); try (RandomAccessFile file = new RandomAccessFile(out, "rw"); RandomAccessFile audit = packetAudit == null ? null : new RandomAccessFile(packetAudit, "rw")) {
            if (done == 0) { file.setLength(HEADER_BYTES); file.seek(0); file.write(header(a, count, frozen, new byte[32])); file.seek(HEADER_BYTES); if (audit != null) { audit.setLength(HEADER_BYTES); audit.seek(0); audit.write(packetAuditHeader(a, count, frozen, new byte[32])); audit.seek(HEADER_BYTES); } }
            else { file.seek(HEADER_BYTES); byte[] prefix = new byte[8192]; long left = done * a.recordWidth(); while (left != 0) { int n = file.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial shard ended before prefix"); payload.update(prefix, 0, n); left -= n; } file.seek(HEADER_BYTES + done * a.recordWidth()); if (audit != null) { audit.seek(HEADER_BYTES); left = done * DIGEST_BYTES; while (left != 0) { int n = audit.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial packet audit ended before prefix"); packetPayload.update(prefix, 0, n); left -= n; } audit.seek(HEADER_BYTES + done * DIGEST_BYTES); } }
            int width = a.hiX - a.loX + 1, batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256"))); long start = System.nanoTime();
            for (long at = done; at < count; at += batch) { long end = Math.min(count, at + batch); List<ChunkPos> positions = new ArrayList<>(); for (long i = at; i < end; i++) positions.add(new ChunkPos(a.loX + (int)(i % width), a.loZ + (int)(i / width))); List<byte[]> hashes = new ArrayList<>(), audits = a.v6() ? new ArrayList<>() : null; loadBatch(server, level, a, positions, true, hashes, audits); for (int i = 0; i < hashes.size(); i++) { file.write(hashes.get(i)); payload.update(hashes.get(i)); if (audit != null) { audit.write(audits.get(i)); packetPayload.update(audits.get(i)); } } double rate = (end - done) / ((System.nanoTime() - start) / 1_000_000_000.0); ChunkPos last = positions.get(positions.size()-1); System.err.printf("[large-parity] %s chunks=%d/%d rate=%.1f chunks/s coord=(%d,%d)%n", a.dimension, end, count, rate, last.x(), last.z()); }
            file.seek(0); file.write(header(a, count, frozen, payload.digest())); if (audit != null) { audit.seek(0); audit.write(packetAuditHeader(a, count, frozen, packetPayload.digest())); }
            } });
        } finally {
            if (!Arrays.equals(frozen, frozenDigest(frozenRoot, a))) throw new IllegalStateException("sealed frozen-world source changed during export");
        }
    }
    public static void main(String[] ignored) throws Exception {
        verifySingleWorldgenWorker(); verifyCanonicalLightContract(); Args a = args(); if (a.help) { usage(); return; } if (a.provenanceSelftest) { provenanceSelftest(); return; }
        if (a.determinismSelftest) { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("determinism selftest requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); determinismSelftest(a, Path.of(root)); return; }
        if ("materialize".equals(a.mode)) { String root = System.getenv("ORACLE_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("materialize requires LODESTONE_ORACLE_WORLD_ROOT"); materialize(a, Path.of(root)); }
        else { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("export requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); export(a, Path.of(root)); }
    }
}
