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
import java.util.concurrent.atomic.AtomicReference;
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
import net.minecraft.util.RandomSource;
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
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.structure.PoolElementStructurePiece;
import net.minecraft.world.level.levelgen.structure.StructureStart;
import net.minecraft.world.level.levelgen.structure.TerrainAdjustment;
import net.minecraft.world.level.levelgen.structure.pools.JigsawPlacement;
import net.minecraft.world.level.levelgen.structure.pools.StructureTemplatePool;
import net.minecraft.world.level.levelgen.structure.pools.alias.PoolAliasLookup;
import net.minecraft.world.level.levelgen.structure.templatesystem.LiquidSettings;
import net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager;
import net.minecraft.world.level.levelgen.structure.templatesystem.StructurePlaceSettings;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.BooleanOp;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;
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
    static final byte[] MAGIC_V7 = "LWP26P07".getBytes(StandardCharsets.US_ASCII);
    static final byte[] LIGHT_FREE_AUDIT_MAGIC = "LWP26A07".getBytes(StandardCharsets.US_ASCII);
    static final int HEADER_BYTES = 256, FORMAT_VERSION = 3, SCHEMA_VERSION = 3, DIGEST_BYTES = 32;
    // Header bytes 70..72 identify the structure-terrain scope. Zero is the
    // production scope used by every authenticated full-world export; the
    // composed stage oracle records its empty scope in its own text schema.
    static final short STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL = 0;
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
    static final byte[] MANIFEST_DOMAIN_V7 = "lodestone.worldgen.large-parity.manifest/v7/light-free".getBytes(StandardCharsets.US_ASCII);
    static final byte[] PACKET_AUDIT_MAGIC = "LWP26A06".getBytes(StandardCharsets.US_ASCII);
    static final byte[] PACKET_AUDIT_DOMAIN = "lodestone.worldgen.large-parity.packet-audit/v6/raw-packet".getBytes(StandardCharsets.US_ASCII);
    /**
     * Live one-way oracle stream. The stream is separate from the frozen-world
     * manifests so a consumer can compare a packet while the JVM is still
     * admitting the next coordinate.
     */
    static final byte[] STREAM_MAGIC = "LWS26S01".getBytes(StandardCharsets.US_ASCII);
    static final byte[] STREAM_DOMAIN = "lodestone.worldgen.streaming-parity/v2/light-free".getBytes(StandardCharsets.US_ASCII);
    static final byte[] END_STREAM_DOMAIN = "lodestone.worldgen.streaming-parity/v3/end-p06-lifecycle".getBytes(StandardCharsets.US_ASCII);
    static final byte[] END_STREAM_EVENT_DOMAIN = "lodestone.worldgen.streaming-parity/end-p06-lifecycle-event/v1".getBytes(StandardCharsets.US_ASCII);
    static final int STREAM_HEADER_BYTES = 256;
    static final int STREAM_FORMAT_LIGHT_FREE = 7;
    static final int STREAM_FORMAT_END_P06_LIFECYCLE = 8;
    static final byte[] LIGHT_FREE_AUDIT_DOMAIN = "lodestone.worldgen.large-parity.audit/v7/light-free".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN = "lodestone.worldgen.large-parity.chunk/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V4 = "lodestone.worldgen.large-parity.chunk/v4/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V5 = "lodestone.worldgen.large-parity.chunk/v5/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V7 = "lodestone.worldgen.large-parity.chunk/v7/light-free".getBytes(StandardCharsets.US_ASCII);
    // This namespace describes the generation scheduling contract, not the
    // semantic manifest format. It changes whenever a root's construction
    // rules change, even if its exported records do not.
    static final String MATERIALIZATION_CONTRACT = "lodestone-large-parity-materialization-v2";
    static final String MATERIALIZATION_CONTRACT_V6 = "lodestone-large-parity-materialization-v6";
    static final String MATERIALIZATION_CONTRACT_V7 = "lodestone-large-parity-materialization-v7";
    static final int MATERIALIZE_TILE = 16;
    static final int MATERIALIZE_RADIUS = 1;
    static final String OVERWORLD = "overworld", NETHER = "nether", END = "end";
    static String diagnosticPacketOut, diagnosticRecordOut;

    /**
     * The compiled generator keeps the last nearest-biome leaf in a
     * thread-local accelerator. That cache is an optimization, but its
     * tie-breaking candidate is observable when independent generation tasks
     * reach the same worker in a different order. Clear it on the worker
     * which will execute the next generation task so the admission order is
     * the only ordering input retained by the oracle.
     */
    static void resetBiomeSearchState(ServerLevel level) {
        try {
            Object source = level.getChunkSource().getGenerator().getBiomeSource();
            Method parameters = source.getClass().getDeclaredMethod("parameters");
            parameters.setAccessible(true);
            Object parameterList = parameters.invoke(source);
            Object index = field(parameterList, "index");
            Object lastResult = field(index, "lastResult");
            if (!(lastResult instanceof ThreadLocal<?> cache)) throw new IllegalStateException("biome search cache is not thread-local");
            cache.remove();
        } catch (NoSuchMethodException ignored) {
            // Fixed and end-dimension sources do not use a parameter tree.
        } catch (ReflectiveOperationException e) {
            throw new IllegalStateException("cannot reset biome search state", e);
        }
    }

    static void resetBiomeSearchStateOnWorker(ServerLevel level) {
        CompletableFuture.runAsync(() -> resetBiomeSearchState(level), Util.backgroundExecutor()).join();
    }

    static void resetLevelRandom(ServerLevel level) {
        // Structure placement and neighbour updates may consult the level
        // random source. The source is normally seeded from process entropy,
        // so seed it at the oracle boundary before any admitted centre can use
        // it. Per-chunk generator randomness remains position-derived.
        level.getRandom().setSeed(SEED);
    }

    static void resetLevelRandom(ServerLevel level, ChunkPos pos) {
        long mixed = SEED + 0x9E3779B97F4A7C15L * pos.x() + 0xC2B2AE3D27D4EB4FL * pos.z();
        mixed ^= mixed >>> 30;
        mixed *= 0xBF58476D1CE4E5B9L;
        mixed ^= mixed >>> 27;
        mixed *= 0x94D049BB133111EBL;
        mixed ^= mixed >>> 31;
        level.getRandom().setSeed(mixed);
    }

    static void waitForEmptyServerPause() throws InterruptedException {
        String value = System.getenv("ORACLE_PAUSE_WHEN_EMPTY_SECONDS");
        if (value == null || value.isBlank()) return;
        int seconds;
        try { seconds = Integer.parseInt(value); }
        catch (NumberFormatException e) { throw new IllegalStateException("ORACLE_PAUSE_WHEN_EMPTY_SECONDS must be an integer: " + value, e); }
        if (seconds < 0) throw new IllegalStateException("ORACLE_PAUSE_WHEN_EMPTY_SECONDS must be non-negative: " + value);
        if (seconds > 0) Thread.sleep(Math.multiplyExact((long)seconds + 1L, 1000L));
    }

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
        String streamOut;
        long streamStartIndex;
        String diagnosticCoordinates;
        String diagnosticPacketDir;
        String diagnosticRecordDir;
        int loX = GRID_MIN, hiX = GRID_MAX, loZ = GRID_MIN, hiZ = GRID_MAX;
        boolean resume, help, provenanceSelftest, determinismSelftest, rawPacketV6, lightFreeV7;
        boolean explicitCx, explicitCz;
        String dimension = OVERWORLD;
        boolean explicitDimension;
        boolean dimensionFormat() { return explicitDimension || !OVERWORLD.equals(dimension); }
        boolean v5() { return END.equals(dimension); }
        boolean v6() { return rawPacketV6; }
        boolean v7() { return lightFreeV7; }
        int semanticVersion() { return v7() ? 7 : v6() ? 6 : v5() ? 5 : dimensionFormat() ? 4 : FORMAT_VERSION; }
        int gridMin() { return v6() || v7() ? RAW_GRID_MIN : GRID_MIN; }
        int gridMax() { return v6() || v7() ? RAW_GRID_MAX : GRID_MAX; }
        int recordWidth() { return v6() || v7() ? RAW_RECORD_BYTES : DIGEST_BYTES; }
        byte[] magic() { return v7() ? MAGIC_V7 : v6() ? MAGIC_V6 : v5() ? MAGIC_V5 : dimensionFormat() ? MAGIC_V4 : MAGIC; }
        byte[] manifestDomain() { return v7() ? MANIFEST_DOMAIN_V7 : v6() ? MANIFEST_DOMAIN_V6 : v5() ? MANIFEST_DOMAIN_V5 : dimensionFormat() ? MANIFEST_DOMAIN_V4 : MANIFEST_DOMAIN; }
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
            case "--light-free", "--format-v7" -> out.lightFreeV7 = true;
            case "--format" -> {
                String format = a[++i];
                if ("v6".equals(format)) out.rawPacketV6 = true;
                else if ("v7".equals(format)) out.lightFreeV7 = true;
                else throw new IllegalArgumentException("--format accepts only v6 (raw packet) or v7 (light-free)");
            }
            case "--mode" -> out.mode = a[++i];
            case "--out" -> out.out = a[++i];
            case "--cx" -> { out.loX = Integer.parseInt(a[++i]); out.hiX = Integer.parseInt(a[++i]); out.explicitCx = true; }
            case "--cz" -> { out.loZ = Integer.parseInt(a[++i]); out.hiZ = Integer.parseInt(a[++i]); out.explicitCz = true; }
            case "--resume" -> out.resume = true;
            case "--packet-out" -> out.packetOut = a[++i];
            case "--packet-audit-out" -> out.packetAuditOut = a[++i];
            case "--record-out" -> out.recordOut = a[++i];
            case "--stream-out" -> out.streamOut = a[++i];
            case "--start-index" -> out.streamStartIndex = Long.parseLong(a[++i]);
            case "--diagnostic-coordinates" -> out.diagnosticCoordinates = a[++i];
            case "--diagnostic-packet-dir" -> out.diagnosticPacketDir = a[++i];
            case "--diagnostic-record-dir" -> out.diagnosticRecordDir = a[++i];
            case "--dimension" -> { out.dimension = a[++i].toLowerCase(); out.explicitDimension = true; }
            default -> throw new IllegalArgumentException("unknown argument " + a[i]);
        }
        if (out.rawPacketV6 && out.lightFreeV7) throw new IllegalArgumentException("--raw-packet and --light-free select different explicit formats");
        if (!out.rawPacketV6 && !out.lightFreeV7 && (out.loX < GRID_MIN || out.hiX > GRID_MAX || out.loZ < GRID_MIN || out.hiZ > GRID_MAX)) out.rawPacketV6 = true;
        if (out.rawPacketV6 || out.lightFreeV7) {
            if (!out.explicitCx) { out.loX = RAW_GRID_MIN; out.hiX = RAW_GRID_MAX; }
            if (!out.explicitCz) { out.loZ = RAW_GRID_MIN; out.hiZ = RAW_GRID_MAX; }
        }
        if (out.help || out.provenanceSelftest || out.determinismSelftest) return out;
        if (!OVERWORLD.equals(out.dimension) && !NETHER.equals(out.dimension) && !END.equals(out.dimension)) throw new IllegalArgumentException("--dimension must be overworld, nether, or end");
        if (!"materialize".equals(out.mode) && !"export".equals(out.mode) && !"diagnostic".equals(out.mode) && !"stream".equals(out.mode)) throw new IllegalArgumentException("--mode must be materialize, export, diagnostic, or stream");
        if (out.loX > out.hiX || out.loZ > out.hiZ || out.loX < out.gridMin() || out.hiX > out.gridMax() || out.loZ < out.gridMin() || out.hiZ > out.gridMax()) throw new IllegalArgumentException("ranges must lie in " + out.gridMin() + "..=" + out.gridMax());
        if ("materialize".equals(out.mode) && out.out != null) throw new IllegalArgumentException("materialize has no --out; it seals the persistent world");
        if ("diagnostic".equals(out.mode)) {
            if (out.diagnosticCoordinates == null) throw new IllegalArgumentException("diagnostic mode requires --diagnostic-coordinates");
            if (out.out != null || out.resume || out.packetOut != null || out.packetAuditOut != null || out.recordOut != null) throw new IllegalArgumentException("diagnostic mode accepts only coordinate and diagnostic output options");
            if (out.v7()) {
                if (out.diagnosticRecordDir == null || out.diagnosticPacketDir != null) throw new IllegalArgumentException("light-free diagnostic mode requires only --diagnostic-record-dir");
            } else if (out.diagnosticPacketDir == null || out.diagnosticRecordDir != null) {
                throw new IllegalArgumentException("packet diagnostic mode requires only --diagnostic-packet-dir");
            }
        } else if (out.diagnosticCoordinates != null || out.diagnosticPacketDir != null || out.diagnosticRecordDir != null) {
            throw new IllegalArgumentException("diagnostic output options require --mode diagnostic");
        }
        if ("export".equals(out.mode) && out.out == null) throw new IllegalArgumentException("export requires --out");
        if ("stream".equals(out.mode)) {
            if (out.streamOut == null || out.streamOut.isBlank()) throw new IllegalArgumentException("stream requires --stream-out");
            if (!out.v6() && !out.v7()) throw new IllegalArgumentException("stream requires --raw-packet or --light-free");
            if (END.equals(out.dimension) && !out.v6()) throw new IllegalArgumentException("End lifecycle stream requires --raw-packet");
            if (out.streamStartIndex < 0) throw new IllegalArgumentException("--start-index must be non-negative");
            if (out.streamStartIndex != 0) throw new IllegalArgumentException("stream is ephemeral; --start-index is not supported");
            if (out.out != null || out.resume || out.packetOut != null || out.packetAuditOut != null || out.recordOut != null) throw new IllegalArgumentException("stream mode accepts only --stream-out and --start-index output options");
            long count = (long)(out.hiX - out.loX + 1) * (out.hiZ - out.loZ + 1);
            if (out.streamStartIndex > count) throw new IllegalArgumentException("--start-index exceeds stream count");
        } else if (out.streamOut != null || out.streamStartIndex != 0) {
            throw new IllegalArgumentException("--stream-out and --start-index require --mode stream");
        }
        if ((out.packetOut != null || out.recordOut != null) && (out.loX != out.hiX || out.loZ != out.hiZ)) throw new IllegalArgumentException("--packet-out and --record-out require exactly one chunk");
        if (out.v7() && out.packetOut != null) throw new IllegalArgumentException("--packet-out requires the full-packet v6 format; light-free v7 never encodes a packet");
        if (out.v7() && out.packetAuditOut != null) throw new IllegalArgumentException("--packet-audit-out requires the full-packet v6 format; light-free v7 has no packet audit");
        if (out.v7() && out.determinismSelftest) throw new IllegalArgumentException("--determinism-selftest requires a packet-bearing format; light-free v7 has no packet path");
        return out;
    }

    static void usage() {
        System.out.println("materialize: LargeParityOracle --mode materialize [--dimension overworld|nether|end]");
        System.out.println("export:      LargeParityOracle --mode export --out /oracle/shard.lwp --cx LO HI --cz LO HI [--raw-packet|--light-free] [--dimension overworld|nether|end] [--resume] [--packet-out /oracle/chunk.bin] [--packet-audit-out /oracle/shard.packet-audit] [--record-out /oracle/chunk.record]");
        System.out.println("stream:      LargeParityOracle --mode stream --stream-out /oracle-out/stream --cx LO HI --cz LO HI --light-free [--dimension overworld|nether]");
        System.out.println("stream:      LargeParityOracle --mode stream --stream-out /oracle-out/stream --cx LO HI --cz LO HI --raw-packet --dimension end");
        System.out.println("diagnostic:  LargeParityOracle --mode diagnostic --diagnostic-coordinates /oracle/coords.tsv --diagnostic-packet-dir /oracle/packets [--dimension overworld|nether|end]");
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
        b.putShort((short)a.recordWidth()).putShort(STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL).put(digest(domain)).put(frozenDigest).put(payloadDigest);
        if (a.v6() || a.v7() || a.dimensionFormat()) b.put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
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
            if (b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=width || b.getShort()!=STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL) throw new IllegalStateException("resume shard geometry or structure-beard scope differs: " + f);
            byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected);
            if (!Arrays.equals(domain, digest(a.manifestDomain())) || !Arrays.equals(recordedFrozen, frozenDigest)) throw new IllegalStateException("resume schema or frozen-world identity differs: " + f);
            if (a.v6() || a.v7() || a.dimensionFormat()) { byte[] recordedDimension = new byte[32]; b.get(recordedDimension); if (!Arrays.equals(recordedDimension, digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)))) throw new IllegalStateException("resume dimension identity differs: " + f); }
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
        b.putShort((short)DIGEST_BYTES).putShort(STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL).put(digest(PACKET_AUDIT_DOMAIN)).put(frozenDigest).put(payloadDigest).put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
        return b.array();
    }
    static long resumePacketAudits(File f, Args a, long count, byte[] frozenDigest) throws Exception {
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * DIGEST_BYTES || ((f.length() - HEADER_BYTES) % DIGEST_BYTES) != 0) throw new IllegalStateException("resume refuses malformed packet audit sidecar: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN); byte[] magic = new byte[8]; b.get(magic);
            if (!Arrays.equals(magic, PACKET_AUDIT_MAGIC) || b.getShort() != 6 || b.getShort() != HEADER_BYTES || b.getShort() != 3 || b.getShort() != 6 || b.getInt() != 776 || b.getLong() != SEED) throw new IllegalStateException("packet audit sidecar identity differs: " + f);
            b.position(28);
            if (b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=DIGEST_BYTES) throw new IllegalStateException("packet audit sidecar geometry differs: " + f);
            if (b.getShort() != STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL) throw new IllegalStateException("packet-audit structure-beard scope differs: " + f);
            byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected); byte[] recordedDimension = new byte[32]; b.get(recordedDimension);
            if (!Arrays.equals(domain, digest(PACKET_AUDIT_DOMAIN)) || !Arrays.equals(recordedFrozen, frozenDigest) || !Arrays.equals(recordedDimension, digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)))) throw new IllegalStateException("packet audit sidecar provenance differs: " + f);
            long records = (f.length() - HEADER_BYTES) / DIGEST_BYTES;
            if (records == count) { MessageDigest actual = sha256(); byte[] buf = new byte[8192]; int n; while ((n = in.read(buf)) != -1) actual.update(buf, 0, n); if (!Arrays.equals(expected, actual.digest())) throw new IllegalStateException("packet audit sidecar checksum differs: " + f); }
            else if (!Arrays.equals(expected, new byte[32])) throw new IllegalStateException("partial packet audit sidecar has a non-zero final checksum: " + f);
            return records;
        }
    }

    static byte[] lightFreeAuditHeader(Args a, long count, byte[] frozenDigest, byte[] payloadDigest) {
        ByteBuffer b = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        b.put(LIGHT_FREE_AUDIT_MAGIC).putShort((short)7).putShort((short)HEADER_BYTES).putShort((short)3).putShort((short)7).putInt(776).putLong(SEED);
        b.putInt(a.gridMin()).putInt(a.gridMax()).putInt(a.gridMin()).putInt(a.gridMax()).putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count);
        b.putShort((short)DIGEST_BYTES).putShort(STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL).put(digest(LIGHT_FREE_AUDIT_DOMAIN)).put(frozenDigest).put(payloadDigest).put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
        return b.array();
    }

    static long resumeLightFreeAudits(File f, Args a, long count, byte[] frozenDigest) throws Exception {
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * DIGEST_BYTES || ((f.length() - HEADER_BYTES) % DIGEST_BYTES) != 0) throw new IllegalStateException("resume refuses malformed light-free audit sidecar: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN); byte[] magic = new byte[8]; b.get(magic);
            if (!Arrays.equals(magic, LIGHT_FREE_AUDIT_MAGIC) || b.getShort() != 7 || b.getShort() != HEADER_BYTES || b.getShort() != 3 || b.getShort() != 7 || b.getInt() != 776 || b.getLong() != SEED) throw new IllegalStateException("light-free audit sidecar identity differs: " + f);
            b.position(28);
            if (b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.gridMin() || b.getInt()!=a.gridMax() || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=DIGEST_BYTES) throw new IllegalStateException("light-free audit sidecar geometry differs: " + f);
            if (b.getShort() != STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL) throw new IllegalStateException("light-free audit structure-beard scope differs: " + f);
            byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected); byte[] recordedDimension = new byte[32]; b.get(recordedDimension);
            if (!Arrays.equals(domain, digest(LIGHT_FREE_AUDIT_DOMAIN)) || !Arrays.equals(recordedFrozen, frozenDigest) || !Arrays.equals(recordedDimension, digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)))) throw new IllegalStateException("light-free audit sidecar provenance differs: " + f);
            long records = (f.length() - HEADER_BYTES) / DIGEST_BYTES;
            if (records == count) { MessageDigest actual = sha256(); byte[] buf = new byte[8192]; int n; while ((n = in.read(buf)) != -1) actual.update(buf, 0, n); if (!Arrays.equals(expected, actual.digest())) throw new IllegalStateException("light-free audit sidecar checksum differs: " + f); }
            else if (!Arrays.equals(expected, new byte[32])) throw new IllegalStateException("partial light-free audit sidecar has a non-zero final checksum: " + f);
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
    static String materializationContract(Args a) { return a.v7() ? MATERIALIZATION_CONTRACT_V7 : a.v6() ? MATERIALIZATION_CONTRACT_V6 : MATERIALIZATION_CONTRACT; }
    static String freezeStamp(Args a) { return materializationContract(a) + "-" + a.dimension + ".freeze.sha256"; }
    static List<String> acceptedFreezeStamps(Args a) {
        if (a.v7()) return List.of(freezeStamp(a), MATERIALIZATION_CONTRACT_V6 + "-" + a.dimension + ".freeze.sha256");
        return List.of(freezeStamp(a));
    }
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
        List<String> freezeStamps = acceptedFreezeStamps(a);
        try (var paths = Files.walk(root)) { for (Path path : paths.filter(Files::isRegularFile).filter(p -> !freezeStamps.contains(p.getFileName().toString())).sorted().toList()) {
            byte[] name = root.relativize(path).toString().replace(File.separatorChar, '/').getBytes(StandardCharsets.UTF_8);
            sha.update(ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN).putInt(name.length).array()); sha.update(name); sha.update(ByteBuffer.allocate(8).order(ByteOrder.BIG_ENDIAN).putLong(Files.size(path)).array());
            try (var input = Files.newInputStream(path)) { byte[] buf = new byte[8192]; for (int n; (n = input.read(buf)) != -1;) sha.update(buf, 0, n); }
        } }
        return sha.digest();
    }
    static byte[] frozenDigest(Path root, Args a) throws Exception {
        rejectLegacyProvenance(root);
        Path stamp = acceptedFreezeStamps(a).stream().map(root::resolve).filter(Files::isRegularFile).findFirst().orElse(root.resolve(freezeStamp(a))); if (!Files.isRegularFile(stamp)) throw new IllegalStateException("frozen world has no selected-dimension seal: " + stamp);
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

    /**
     * Emits the authenticated v7 content record without constructing a packet
     * or consulting the light engine. The field order is fixed by the schema:
     * domain/coordinates/dimension, the three client heightmaps, then section
     * state/biome cells, then canonical block entities. Dimension identity is always present in v7,
     * including for the overworld, so a record cannot cross vertical shapes.
     */
    static byte[] lightFreeRecord(ServerLevel level, LevelChunk chunk, Args a) throws Exception {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream(120_000); DataOutputStream out = new DataOutputStream(bytes);
        out.write(RECORD_DOMAIN_V7); out.writeInt(chunk.getPos().x()); out.writeInt(chunk.getPos().z()); out.write(a.dimensionKey().getBytes(StandardCharsets.UTF_8));

        Map<Integer, Heightmap> heightmaps = new HashMap<>();
        for (Map.Entry<Heightmap.Types, Heightmap> entry : chunk.getHeightmaps()) {
            int id = entry.getKey().ordinal();
            if (id == 1 || id == 4 || id == 5) heightmaps.put(id, entry.getValue());
        }
        out.writeInt(3);
        for (int id : new int[] {1, 4, 5}) {
            Heightmap map = heightmaps.get(id);
            if (map == null) throw new IllegalStateException("light-free chunk is missing client heightmap id " + id);
            out.writeInt(id);
            for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) out.writeInt(map.getFirstAvailable(x, z));
        }

        Registry<Biome> biomes = level.registryAccess().lookupOrThrow(Registries.BIOME);
        LevelChunkSection[] sections = chunk.getSections();
        out.writeInt(sections.length);
        for (LevelChunkSection section : sections) {
            for (int y = 0; y < 16; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) out.writeInt(Block.getId(section.getBlockState(x, y, z)));
            for (int y = 0; y < 4; y++) for (int z = 0; z < 4; z++) for (int x = 0; x < 4; x++) {
                Holder<Biome> biome = section.getNoiseBiome(x, y, z);
                out.writeInt(biomes.getId(biome.value()));
            }
        }

        Registry<net.minecraft.world.level.block.entity.BlockEntityType<?>> types = level.registryAccess().lookupOrThrow(Registries.BLOCK_ENTITY_TYPE);
        List<BlockEntity> entities = new ArrayList<>(chunk.getBlockEntities().values());
        entities.sort(Comparator.comparingInt((BlockEntity e) -> e.getBlockPos().getX() & 15).thenComparingInt(e -> e.getBlockPos().getY()).thenComparingInt(e -> e.getBlockPos().getZ() & 15).thenComparingInt(e -> types.getId(e.getType())));
        out.writeInt(entities.size());
        for (BlockEntity entity : entities) {
            out.writeByte(entity.getBlockPos().getX() & 15); out.writeShort(entity.getBlockPos().getY()); out.writeByte(entity.getBlockPos().getZ() & 15); out.writeInt(types.getId(entity.getType()));
            CompoundTag tag = entity.getUpdateTag(level.registryAccess());
            if (tag.isEmpty()) out.writeByte(Tag.TAG_END); else canonicalTag(out, tag);
        }
        out.flush();
        return bytes.toByteArray();
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

    static void materializeOne(MinecraftServer server, ServerLevel level, Args a, ChunkPos pos) {
        resetBiomeSearchState(level);
        resetBiomeSearchStateOnWorker(level);
        CompletableFuture<?> future = server.submit(() -> level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)).join();
        net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)future.join();
        if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + pos + ": " + result.getError());
        if (!a.v7()) settleMaterializedBatch(server, level, List.of(pos));
        server.submit(() -> {
            level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0);
            // Ticket removal only queues the chunk for unloading. Drain that
            // queue before the next admission so a later generation cannot
            // observe a prior centre at an implementation-dependent point in
            // its save/unload lifecycle.
            level.getChunkSource().tick(() -> true, false);
        }).join();
    }

    /**
     * Admit one centre at a time while retaining every earlier centre ticket
     * in the tile. A one-centre loop that releases immediately lets the server
     * evict a dependency between admissions; the next feature pass can then
     * observe that dependency at a different generation stage even with one
     * worldgen worker. The tile is the durable scheduling unit, so its
     * admission order and release are one fence.
     */
    static void materializeBatch(MinecraftServer server, ServerLevel level, Args a, List<ChunkPos> positions) {
        if (positions.isEmpty()) return;
        for (ChunkPos pos : positions) {
            resetLevelRandom(level, pos);
            resetBiomeSearchState(level);
            resetBiomeSearchStateOnWorker(level);
            CompletableFuture<?> future = server.submit(() -> level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, MATERIALIZE_RADIUS)).join();
            net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>) future.join();
            if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + pos + ": " + result.getError());
            // FULL completion can leave feature and light work queued behind
            // the returned future. Fence each centre before admitting the
            // next one; earlier tile tickets remain live, so this preserves
            // the tile's dependency closure without allowing deferred work to
            // race a later centre.
            settleMaterializedBatch(server, level, List.of(pos));
        }
        // The materialization contract does not capture packets here, but a
        // complete tile must still drain scheduler work before its tickets are
        // released. This is deliberately a server-thread fence: joining the
        // futures from that thread would deadlock the server executor.
        server.submit(() -> level.getChunkSource().tick(() -> true, false)).join();
        server.submit(() -> {
            for (ChunkPos pos : positions) level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, MATERIALIZE_RADIUS);
            level.getChunkSource().tick(() -> true, false);
        }).join();
    }

    static void loadBatch(MinecraftServer server, ServerLevel level, Args a, List<ChunkPos> positions, boolean capture, List<byte[]> out) { loadBatch(server, level, a, positions, capture, out, null); }
    static void loadBatch(MinecraftServer server, ServerLevel level, Args a, List<ChunkPos> positions, boolean capture, List<byte[]> out, List<byte[]> packetAudits) {
        if (!capture) {
            materializeBatch(server, level, a, positions);
            return;
        }

        // A range future only orders the requested FULL status.  It does not
        // order the deferred feature/light work that can still mutate a
        // neighbouring column before a later capture.  Capturing a centre as
        // soon as it settles therefore lets later admissions change the value
        // that the export is supposed to describe.  Admit every centre first,
        // retain the whole dependency closure, then settle and capture the
        // final set in the requested order.
        List<CompletableFuture<?>> futures = new ArrayList<>(positions.size());
        for (ChunkPos pos : positions) {
            resetLevelRandom(level, pos);
            resetBiomeSearchState(level);
            resetBiomeSearchStateOnWorker(level);
            futures.add(server.submit(() -> level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, MATERIALIZE_RADIUS)).join());
        }
        for (int i = 0; i < positions.size(); i++) {
            net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)futures.get(i).join();
            ChunkPos pos = positions.get(i);
            if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + pos + ": " + result.getError());
        }
        settleMaterializedBatch(server, level, positions);
        out.addAll(server.submit(() -> {
            try {
                List<byte[]> result = new ArrayList<>(positions.size());
                for (ChunkPos pos : positions) {
                    LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z());
                    if (chunk == null) throw new IllegalStateException("loaded chunk was evicted: " + pos);
                    byte[] packet = a.v6() || diagnosticPacketOut != null ? packetBody(server, chunk, level) : null;
                    if (diagnosticPacketOut != null) Files.write(Path.of(diagnosticPacketOut), packet);
                    if (a.v7()) {
                        byte[] record = lightFreeRecord(level, chunk, a);
                        byte[] full = digest(record);
                        if (packetAudits != null) packetAudits.add(full);
                        if (diagnosticRecordOut != null) Files.write(Path.of(diagnosticRecordOut), record);
                        result.add(Arrays.copyOf(full, RAW_RECORD_BYTES));
                    } else if (a.v6()) {
                        byte[] full = digest(packet);
                        if (packetAudits != null) packetAudits.add(full);
                        result.add(Arrays.copyOf(full, RAW_RECORD_BYTES));
                    } else {
                        byte[] record = semanticRecord(level, chunk, a);
                        if (diagnosticRecordOut != null) Files.write(Path.of(diagnosticRecordOut), record);
                        result.add(digest(record));
                    }
                }
                return result;
            } catch (Exception e) {
                throw new IllegalStateException("canonical chunk export failed", e);
            }
        }).join());
        /*
         * All centres remain ticketed until their complete final-set capture
         * above.  Releasing them as a group prevents an unload queue from
         * interleaving with the next export batch.
         */
        server.submit(() -> {
            for (ChunkPos pos : positions) level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, MATERIALIZE_RADIUS);
            level.getChunkSource().tick(() -> true, false);
        }).join();
    }

    /** Admit one centre and capture only the requested bounded content record. */
    static byte[] streamPacket(MinecraftServer server, ServerLevel level, Args a, ChunkPos pos) throws Exception {
        resetBiomeSearchState(level);
        resetBiomeSearchStateOnWorker(level);
        CompletableFuture<?> future = server.submit(() -> level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)).join();
        net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)future.join();
        if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + pos + ": " + result.getError());
        if (!a.v7()) settleMaterializedBatch(server, level, List.of(pos));
        LevelChunk chunk = server.submit(() -> level.getChunkSource().getChunkNow(pos.x(), pos.z())).join();
        if (chunk == null) throw new IllegalStateException("stream centre was evicted before packet capture: " + pos);
        byte[] packet = a.v7() ? lightFreeRecord(level, chunk, a) : packetBody(server, chunk, level);
        server.submit(() -> {
            level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0);
            level.getChunkSource().tick(() -> true, false);
        }).join();
        return packet;
    }

    static byte[] fileDigest(Path path) throws Exception {
        MessageDigest digest = sha256();
        try (var input = Files.newInputStream(path)) {
            byte[] buffer = new byte[8192];
            for (int n; (n = input.read(buffer)) != -1;) digest.update(buffer, 0, n);
        }
        return digest.digest();
    }

    /**
     * Fixed binary stream header.  The fields intentionally mirror the
     * provenance needed by the host ledger, while leaving the existing LWP
     * formats untouched.  Offsets are documented in the streaming parity doc.
     */
    static byte[] streamHeader(Args a, long count) throws Exception {
        ByteBuffer b = ByteBuffer.allocate(STREAM_HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        b.put(STREAM_MAGIC).putShort((short)1).putShort((short)STREAM_HEADER_BYTES).putShort((short)2).putShort((short)1).putInt(776).putLong(SEED);
        int format = END.equals(a.dimension) ? STREAM_FORMAT_END_P06_LIFECYCLE : (a.v7() ? STREAM_FORMAT_LIGHT_FREE : 6);
        byte[] domain = END.equals(a.dimension) ? END_STREAM_DOMAIN : STREAM_DOMAIN;
        b.putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count).putShort((short)DIGEST_BYTES).putShort((short)format);
        b.put(digest(domain)).put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
        b.put(fileDigest(Path.of("/mc/versions/26.2/server-26.2.jar")));
        b.put(fileDigest(Path.of("/oracle/LargeParityOracle.java")));
        b.putLong(a.streamStartIndex);
        return b.array();
    }

    /**
     * Serialize the authenticated P06 End resident observations that belong
     * to one raw packet frame.  The Rust side installs these values at the
     * matching lifecycle boundary; it never derives them from the packet's
     * final block field or from the light-free stream.
     */
    static byte[] endP06LifecyclePayload(List<EndP06LifecycleCapture.LifecycleEvent> events) throws Exception {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream(32 + events.size() * 32_000);
        DataOutputStream out = new DataOutputStream(bytes);
        out.write(END_STREAM_EVENT_DOMAIN);
        out.writeInt(events.size());
        for (EndP06LifecycleCapture.LifecycleEvent event : events) {
            out.writeLong(event.sequence);
            out.writeInt(event.source.x());
            out.writeInt(event.source.z());
            out.writeByte(1); // FEATURES
            out.writeInt(event.residentTransitions.size());
            for (EndP06LifecycleCapture.LifecycleTransition transition : event.residentTransitions) {
                out.writeInt(transition.resident.x());
                out.writeInt(transition.resident.z());
                out.writeByte(transition.stage);
                if (transition.clientHeightmaps == null) {
                    out.writeByte(0);
                    continue;
                }
                if (transition.clientHeightmaps.length != 3) throw new IllegalStateException("End P06 map count differs");
                out.writeByte(1);
                for (int map = 0; map < 3; map++) {
                    if (transition.clientHeightmaps[map].length != 256) throw new IllegalStateException("End P06 map width differs");
                    for (int value : transition.clientHeightmaps[map]) {
                        if (value < 0 || value > 0xffff) throw new IllegalStateException("End P06 map cell exceeds u16: " + value);
                        out.writeShort(value);
                    }
                }
            }
        }
        out.flush();
        return bytes.toByteArray();
    }

    /**
     * Produce a bounded frame stream.  Each frame carries the full packet only
     * in transit; the consumer keeps the packet only when its digest differs.
     * A broken FIFO is treated as cancellation, allowing fail-fast comparison
     * to stop the JVM without a second world export.
     */
    static void stream(Args a) throws Exception {
        long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1);
        if (a.streamStartIndex > count) throw new IllegalArgumentException("stream start exceeds count");
        Path root = Path.of("/work/stream-world");
        runServer(root, false, a, (server, level) -> {
            int width = a.hiX - a.loX + 1;
            Path output = Path.of(a.streamOut);
            if (output.getParent() != null) Files.createDirectories(output.getParent());
            // Publish readiness before opening the FIFO. The shell can now
            // distinguish a JVM that failed during boot from one that is
            // merely waiting for the Rust reader, and the reader receives the
            // provenance header before the potentially long resume replay.
            Files.writeString(Path.of(a.streamOut + ".ready"), "ready\n", StandardCharsets.US_ASCII);
            try (var raw = Files.newOutputStream(output, java.nio.file.StandardOpenOption.CREATE, java.nio.file.StandardOpenOption.TRUNCATE_EXISTING, java.nio.file.StandardOpenOption.WRITE); var out = new DataOutputStream(new java.io.BufferedOutputStream(raw, 64 * 1024))) {
                out.write(streamHeader(a, count));
                out.flush();
                // Rebuild the server's state prefix while the reader remains
                // attached to the FIFO. A failed replay therefore closes the
                // stream instead of leaving the comparator blocked before its
                // first frame.
                for (long index = 0; index < a.streamStartIndex; index++) {
                    int cx = a.loX + (int)(index % width), cz = a.loZ + (int)(index / width);
                    streamPacket(server, level, a, new ChunkPos(cx, cz));
                    if ((index + 1) % 256 == 0) System.err.printf("[stream %s] replayed-prefix=%d/%d%n", a.dimension, index + 1, a.streamStartIndex);
                }
                long eventSequence = 0;
                for (long index = a.streamStartIndex; index < count; index++) {
                    int cx = a.loX + (int)(index % width), cz = a.loZ + (int)(index / width);
                    List<EndP06LifecycleCapture.LifecycleEvent> events = END.equals(a.dimension)
                        ? EndP06LifecycleCapture.capture(server, level, new ChunkPos(cx, cz), eventSequence)
                        : List.of();
                    eventSequence += events.size();
                    byte[] packet = streamPacket(server, level, a, new ChunkPos(cx, cz));
                    byte[] eventPayload = END.equals(a.dimension) ? endP06LifecyclePayload(events) : null;
                    byte[] eventDigest = eventPayload == null ? null : digest(eventPayload);
                    byte[] full = digest(packet);
                    long bodyLength = eventPayload == null
                        ? 8L + 4L + 4L + 4L + DIGEST_BYTES + packet.length
                        : 8L + 4L + 4L + 4L + 4L + DIGEST_BYTES + DIGEST_BYTES + eventPayload.length + packet.length;
                    if (bodyLength > Integer.MAX_VALUE) throw new IllegalStateException("stream packet frame is too large at " + cx + "," + cz);
                    out.writeInt((int)bodyLength);
                    out.writeLong(index);
                    out.writeInt(cx);
                    out.writeInt(cz);
                    if (eventPayload == null) {
                        out.writeInt(packet.length);
                        out.write(full);
                        out.write(packet);
                    } else {
                        out.writeInt(eventPayload.length);
                        out.writeInt(packet.length);
                        out.write(eventDigest);
                        out.write(full);
                        out.write(eventPayload);
                        out.write(packet);
                    }
                    out.flush();
                    if (index == a.streamStartIndex) Files.writeString(Path.of(a.streamOut + ".frame-ready"), "ready\n", StandardCharsets.US_ASCII);
                    if ((index + 1) % 256 == 0 || index + 1 == count) System.err.printf("[stream %s] emitted=%d/%d coord=(%d,%d)%n", a.dimension, index + 1, count, cx, cz);
                }
                out.writeInt(0);
                out.flush();
            }
            Files.writeString(Path.of(a.streamOut + ".complete"), "complete\n", StandardCharsets.US_ASCII);
        });
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

    static boolean reverseMaterializationTraversal() {
        String value = System.getenv().getOrDefault("ORACLE_TRAVERSAL", "forward");
        if ("forward".equalsIgnoreCase(value)) return false;
        if ("reverse".equalsIgnoreCase(value)) return true;
        throw new IllegalStateException("ORACLE_TRAVERSAL must be forward or reverse: " + value);
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
        int haloMin = a.v6() || a.v7() ? RAW_HALO_MIN : HALO_MIN, haloMax = a.v6() || a.v7() ? RAW_HALO_MAX : HALO_MAX;
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
            int haloMin = a.v6() || a.v7() ? RAW_HALO_MIN : HALO_MIN, haloMax = a.v6() || a.v7() ? RAW_HALO_MAX : HALO_MAX;
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
        boolean reverse = reverseMaterializationTraversal();
        runServer(root, false, a, (server, level) -> {
            long start = System.nanoTime();
            for (int ordinal = current.nextTile; ordinal < end; ordinal++) {
                int tile = reverse ? current.totalTiles() - 1 - ordinal : ordinal;
                int x0 = current.minX + (tile % current.tilesX) * MATERIALIZE_TILE, z0 = current.minZ + (tile / current.tilesX) * MATERIALIZE_TILE;
                List<ChunkPos> positions = new ArrayList<>(MATERIALIZE_TILE * MATERIALIZE_TILE);
                if (reverse) {
                    for (int z = Math.min(current.maxZ, z0 + MATERIALIZE_TILE - 1); z >= z0; z--) for (int x = Math.min(current.maxX, x0 + MATERIALIZE_TILE - 1); x >= x0; x--) positions.add(new ChunkPos(x, z));
                } else {
                    for (int z = z0; z <= Math.min(current.maxZ, z0 + MATERIALIZE_TILE - 1); z++) for (int x = x0; x <= Math.min(current.maxX, x0 + MATERIALIZE_TILE - 1); x++) positions.add(new ChunkPos(x, z));
                }
                loadBatch(server, level, a, positions, false, new ArrayList<>());
                System.err.printf("[large-parity %s] materialized-tile=%d/%d traversal=%s epoch=%d..%d rate=%.1f tiles/s%n", formatLabel(a), ordinal + 1, current.totalTiles(), reverse ? "reverse" : "forward", current.nextTile + 1, end, (ordinal - current.nextTile + 1) / ((System.nanoTime() - start) / 1_000_000_000.0));
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
        boolean freshWorld = tag == null;
        PackRepository packs = ServerPacksSource.createPackRepository(access); WorldStem stem = loadWorld(settings.getProperties(), access, packs, tag); Services services = Services.create(new YggdrasilAuthenticationService(Proxy.NO_PROXY), root.toFile()); NotificationManager notifications = new NotificationManager(); ManagementServer management = JsonRpc.create(settings, notifications);
        // A new world normally performs an unordered 11-by-11 spawn search
        // before the oracle can establish its admission fence. That search
        // consumes the process-seeded level random source and can write
        // overlapping columns, so two empty roots can diverge before oracle
        // work begins. Mark only a genuinely new root initialized before the
        // server thread starts; its default spawn is the deterministic origin.
        // Existing roots, including authenticated roots, retain their saved
        // startup state byte-for-byte.
        AtomicReference<DedicatedServer> serverReference = new AtomicReference<>();
        Thread serverThread = new Thread(() -> {
            try {
                Method runServer = MinecraftServer.class.getDeclaredMethod("runServer");
                runServer.setAccessible(true);
                runServer.invoke(serverReference.get());
            } catch (ReflectiveOperationException e) {
                throw new IllegalStateException("server thread failed to start", e);
            }
        }, "Server thread");
        serverThread.setUncaughtExceptionHandler((thread, error) -> System.err.println("[large-parity] server thread failed: " + error));
        if (Runtime.getRuntime().availableProcessors() > 4) serverThread.setPriority(8);
        DedicatedServer server = new DedicatedServer(serverThread, access, packs, stem, Optional.empty(), settings, DataFixers.getDataFixer(), services, management, notifications);
        if (!requireExisting && freshWorld) server.getWorldData().overworldData().setInitialized(true);
        notifications.setServer(server); server.setPort(25565); serverReference.set(server); serverThread.start();
        try {
            while (server.overworld() == null) Thread.sleep(25);
            // The server publishes its level objects before initial spawn
            // preparation has drained the startup chunk work. Wait for the
            // first ready tick as well as the selected level so oracle
            // admissions cannot race that work.
            ServerLevel level = null;
            long deadline = System.nanoTime() + 60_000_000_000L;
            while ((level == null || !server.isReady()) && System.nanoTime() < deadline) {
                level = selectedLevel(server, a);
                if (level == null) Thread.sleep(25);
                else if (!server.isReady()) Thread.sleep(25);
            }
            if (level == null) throw new IllegalStateException("selected dimension is unavailable: " + a.dimensionKey() + "; loaded=" + server.levelKeys());
            if (!server.isReady()) throw new IllegalStateException("server did not reach ready state before oracle work");
            // The empty-server pause is asynchronous. Wait past its deadline
            // before reseeding the level source so no startup tick can race
            // the first deterministic admission.
            waitForEmptyServerPause();
            resetLevelRandom(level);
            resetBiomeSearchState(level);
            resetBiomeSearchStateOnWorker(level);
            work.run(server, level);
        } finally { server.halt(true); stem.close(); access.close(); }
    }

    /**
     * Read a validated mismatch-coordinate stream without accepting arbitrary
     * paths or duplicate work.  The Rust comparator authenticates the
     * inventory against the manifest before invoking this mode; this reader
     * still checks its small structural contract so a stale or hand-edited
     * coordinate list cannot silently select a different world location.
     */
    static List<ChunkPos> diagnosticPositions(Args a) throws Exception {
        List<String> lines = Files.readAllLines(Path.of(a.diagnosticCoordinates), StandardCharsets.US_ASCII);
        if (lines.isEmpty() || !"index\tcx\tcz".equals(lines.get(0))) throw new IllegalStateException("diagnostic coordinate file has an unexpected header");
        if (lines.size() - 1 > 4096) throw new IllegalStateException("diagnostic coordinate file exceeds the 4096-coordinate bound");
        List<ChunkPos> positions = new ArrayList<>(lines.size() - 1);
        long previous = -1;
        for (int line = 1; line < lines.size(); line++) {
            String[] fields = lines.get(line).split("\\t", -1);
            if (fields.length != 3) throw new IllegalStateException("diagnostic coordinate row " + line + " does not have index/cx/cz fields");
            long index = Long.parseLong(fields[0]);
            int cx = Integer.parseInt(fields[1]), cz = Integer.parseInt(fields[2]);
            if (index <= previous) throw new IllegalStateException("diagnostic coordinate indices are not strictly increasing at row " + line);
            if (cx < a.gridMin() || cx > a.gridMax() || cz < a.gridMin() || cz > a.gridMax()) throw new IllegalStateException("diagnostic coordinate is outside the authenticated grid: (" + cx + "," + cz + ")");
            long expected = (long)(cz - a.gridMin()) * (a.gridMax() - a.gridMin() + 1) + cx - a.gridMin();
            if (index != expected) throw new IllegalStateException("diagnostic coordinate index disagrees with its grid position at row " + line);
            positions.add(new ChunkPos(cx, cz));
            previous = index;
        }
        return positions;
    }

    static Path diagnosticOutputPath(String directory, ChunkPos pos, String suffix) {
        return Path.of(directory).resolve("x" + pos.x() + "_z" + pos.z() + suffix);
    }

    static void diagnostic(Args a, Path frozenRoot) throws Exception {
        byte[] frozen = frozenDigest(frozenRoot, a);
        Path copy = copyReadOnlyWorld();
        if (!Arrays.equals(frozen, frozenDigest(copy, a))) throw new IllegalStateException("prepared frozen-world clone differs from its sealed source");
        List<ChunkPos> positions = diagnosticPositions(a);
        Path output = Path.of(a.v7() ? a.diagnosticRecordDir : a.diagnosticPacketDir);
        Files.createDirectories(output);
        try {
            runServer(copy, true, a, (server, level) -> {
                int batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256")));
                for (int start = 0; start < positions.size(); start += batch) {
                    int end = Math.min(positions.size(), start + batch);
                    List<ChunkPos> selected = positions.subList(start, end);
                    List<CompletableFuture<?>> futures = server.submit(() -> {
                        List<CompletableFuture<?>> result = new ArrayList<>(selected.size());
                        for (ChunkPos pos : selected) result.add(level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0));
                        return result;
                    }).join();
                    for (int i = 0; i < selected.size(); i++) {
                        net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)futures.get(i).join();
                        if (!result.isSuccess()) throw new IllegalStateException("chunk diagnostic load failed at " + selected.get(i) + ": " + result.getError());
                    }
                    server.submit(() -> {
                        try {
                            for (ChunkPos pos : selected) {
                                LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z());
                                if (chunk == null) throw new IllegalStateException("diagnostic chunk was evicted: " + pos);
                                if (System.getenv("ORACLE_BEARD_TRACE") != null && pos.x() == -50 && pos.z() == -50) {
                                    Beardifier beard = Beardifier.forStructuresInChunk(level.structureManager(), pos);
                                    level.structureManager()
                                            .startsForStructure(pos, structure -> structure.terrainAdaptation() == TerrainAdjustment.ENCAPSULATE)
                                            .forEach(structureStart -> {
                                                System.err.println("structure-start " + structureStart.getChunkPos()
                                                        + " bbox=" + structureStart.getBoundingBox()
                                                        + " pieces=" + structureStart.getPieces().size());
                                                for (int i = 0; i < structureStart.getPieces().size(); i++) {
                                                    var piece = structureStart.getPieces().get(i);
                                                    System.err.println("structure-piece " + i + " class="
                                                            + piece.getClass().getSimpleName() + " box=" + piece.getBoundingBox());
                                                }
                                            });
                                    for (java.lang.reflect.Field field : beard.getClass().getDeclaredFields()) {
                                        field.setAccessible(true);
                                        System.err.println("beard-field " + field.getName() + " " + field.get(beard));
                                    }
                                    for (int y = -25; y <= 10; y++) {
                                        for (int x = pos.getMinBlockX(); x <= pos.getMaxBlockX(); x++) {
                                            for (int z = pos.getMinBlockZ(); z <= pos.getMaxBlockZ(); z++) {
                                                double value = beard.compute(new DensityFunction.SinglePointContext(x, y, z));
                                                System.err.println("beard-trace " + x + "," + y + "," + z + " bits=" + Long.toHexString(Double.doubleToRawLongBits(value)));
                                            }
                                        }
                                    }
                                }
                                if (System.getenv("ORACLE_JIGSAW_REPLAY") != null && pos.x() == -50 && pos.z() == -50) {
                                    replayJigsawRandom(level);
                                }
                                byte[] bytes = a.v7() ? lightFreeRecord(level, chunk, a) : packetBody(server, chunk, level);
                                Path target = output.resolve("x" + pos.x() + "_z" + pos.z() + (a.v7() ? ".record" : ".packet"));
                                if (Files.exists(target)) throw new IllegalStateException("diagnostic output already exists: " + target);
                                Files.write(target, bytes);
                            }
                        } catch (Exception error) {
                            throw new IllegalStateException("diagnostic packet export failed", error);
                        }
                    }).join();
                    server.submit(() -> {
                        for (ChunkPos pos : selected) level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0);
                    }).join();
                    System.err.println("[large-parity diagnostic] exported " + end + "/" + positions.size() + " mismatching chunks");
                }
            });
        } finally {
            if (!Arrays.equals(frozen, frozenDigest(frozenRoot, a))) throw new IllegalStateException("sealed frozen-world source changed during diagnostic export");
        }
    }

    static void export(Args a, Path frozenRoot) throws Exception {
        diagnosticPacketOut = a.packetOut; diagnosticRecordOut = a.recordOut; byte[] frozen = frozenDigest(frozenRoot, a); Path copy = copyReadOnlyWorld();
        if (!Arrays.equals(frozen, frozenDigest(copy, a))) throw new IllegalStateException("prepared frozen-world clone differs from its sealed source");
        long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1); File out = new File(a.out); long done = a.resume ? resumeRecords(out, a, count, frozen) : 0;
        File packetAudit = a.v6() ? new File(a.packetAuditOut == null ? a.out + ".packet-audit" : a.packetAuditOut) : null;
        File lightFreeAudit = a.v7() ? new File(a.out + ".light-free-audit") : null;
        if (a.v6() && a.resume && done > 0) { long auditDone = resumePacketAudits(packetAudit, a, count, frozen); if (auditDone != done) throw new IllegalStateException("packet audit sidecar is not aligned with manifest records: " + packetAudit); }
        if (a.v7() && a.resume && done > 0) { long auditDone = resumeLightFreeAudits(lightFreeAudit, a, count, frozen); if (auditDone != done) throw new IllegalStateException("light-free audit sidecar is not aligned with manifest records: " + lightFreeAudit); }
        if (done == count) {
            if (a.v6()) resumePacketAudits(packetAudit, a, count, frozen);
            if (a.v7()) resumeLightFreeAudits(lightFreeAudit, a, count, frozen);
            System.err.println("[large-parity " + formatLabel(a) + "] authenticated shard already complete: " + out); return;
        } if (out.getParentFile() != null) out.getParentFile().mkdirs();
        if (packetAudit != null && packetAudit.getParentFile() != null) packetAudit.getParentFile().mkdirs();
        if (lightFreeAudit != null && lightFreeAudit.getParentFile() != null) lightFreeAudit.getParentFile().mkdirs();
        try { runServer(copy, true, a, (server, level) -> { MessageDigest payload = sha256(), auditPayload = sha256(); try (RandomAccessFile file = new RandomAccessFile(out, "rw"); RandomAccessFile audit = packetAudit == null ? null : new RandomAccessFile(packetAudit, "rw"); RandomAccessFile lightAudit = lightFreeAudit == null ? null : new RandomAccessFile(lightFreeAudit, "rw")) {
            if (done == 0) {
                file.setLength(HEADER_BYTES); file.seek(0); file.write(header(a, count, frozen, new byte[32])); file.seek(HEADER_BYTES);
                if (audit != null) { audit.setLength(HEADER_BYTES); audit.seek(0); audit.write(packetAuditHeader(a, count, frozen, new byte[32])); audit.seek(HEADER_BYTES); }
                if (lightAudit != null) { lightAudit.setLength(HEADER_BYTES); lightAudit.seek(0); lightAudit.write(lightFreeAuditHeader(a, count, frozen, new byte[32])); lightAudit.seek(HEADER_BYTES); }
            } else {
                file.seek(HEADER_BYTES); byte[] prefix = new byte[8192]; long left = done * a.recordWidth(); while (left != 0) { int n = file.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial shard ended before prefix"); payload.update(prefix, 0, n); left -= n; } file.seek(HEADER_BYTES + done * a.recordWidth());
                if (audit != null) { audit.seek(HEADER_BYTES); left = done * DIGEST_BYTES; while (left != 0) { int n = audit.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial packet audit ended before prefix"); auditPayload.update(prefix, 0, n); left -= n; } audit.seek(HEADER_BYTES + done * DIGEST_BYTES); }
                if (lightAudit != null) { lightAudit.seek(HEADER_BYTES); left = done * DIGEST_BYTES; while (left != 0) { int n = lightAudit.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial light-free audit ended before prefix"); auditPayload.update(prefix, 0, n); left -= n; } lightAudit.seek(HEADER_BYTES + done * DIGEST_BYTES); }
            }
            int width = a.hiX - a.loX + 1, batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256"))); long start = System.nanoTime();
            for (long at = done; at < count; at += batch) { long end = Math.min(count, at + batch); List<ChunkPos> positions = new ArrayList<>(); for (long i = at; i < end; i++) positions.add(new ChunkPos(a.loX + (int)(i % width), a.loZ + (int)(i / width))); List<byte[]> hashes = new ArrayList<>(), audits = a.v6() || a.v7() ? new ArrayList<>() : null; loadBatch(server, level, a, positions, true, hashes, audits); for (int i = 0; i < hashes.size(); i++) { file.write(hashes.get(i)); payload.update(hashes.get(i)); if (audit != null) { audit.write(audits.get(i)); auditPayload.update(audits.get(i)); } if (lightAudit != null) { lightAudit.write(audits.get(i)); auditPayload.update(audits.get(i)); } } double rate = (end - done) / ((System.nanoTime() - start) / 1_000_000_000.0); ChunkPos last = positions.get(positions.size()-1); System.err.printf("[large-parity] %s chunks=%d/%d rate=%.1f chunks/s coord=(%d,%d)%n", a.dimension, end, count, rate, last.x(), last.z()); }
            file.seek(0); file.write(header(a, count, frozen, payload.digest())); if (audit != null) { audit.seek(0); audit.write(packetAuditHeader(a, count, frozen, auditPayload.digest())); } if (lightAudit != null) { lightAudit.seek(0); lightAudit.write(lightFreeAuditHeader(a, count, frozen, auditPayload.digest())); }
            } });
        } finally {
            if (!Arrays.equals(frozen, frozenDigest(frozenRoot, a))) throw new IllegalStateException("sealed frozen-world source changed during export");
        }
    }
    private static final class ReplayRandom extends WorldgenRandom {
        private boolean trace;
        private int calls;

        ReplayRandom() {
            super(new LegacyRandomSource(0L));
        }

        void enableTrace() {
            trace = true;
        }

        @Override
        public int nextInt(int bound) {
            int value = super.nextInt(bound);
            if (trace) System.err.println("java-jigsaw-rng " + calls++ + " bound=" + bound + " value=" + value);
            return value;
        }
    }

    private static void replayJigsawRandom(ServerLevel level) throws Exception {
        ChunkPos target = new ChunkPos(-50, -50);
        StructureStart selected = level.structureManager()
                .startsForStructure(target, structure -> structure.terrainAdaptation() == TerrainAdjustment.ENCAPSULATE)
                .stream().findFirst().orElseThrow(() -> new IllegalStateException("no encapsulating target start"));
        if (selected.getPieces().isEmpty() || !(selected.getPieces().get(0) instanceof PoolElementStructurePiece center)) {
            throw new IllegalStateException("target start has no pool center");
        }
        System.err.println("java-jigsaw-center position=" + center.getPosition() + " rotation=" + center.getRotation()
                + " box=" + center.getBoundingBox());
        ReplayRandom random = new ReplayRandom();
        random.setLargeFeatureSeed(SEED, selected.getChunkPos().x(), selected.getChunkPos().z());
        random.enableTrace();
        int startHeight = -40 + random.nextInt(21);
        random.nextInt(4);
        random.nextInt(2);
        System.err.println("java-jigsaw-initial-height=" + startHeight);
        var centerBox = center.getBoundingBox();
        int centerX = (centerBox.maxX() + centerBox.minX()) / 2;
        int centerZ = (centerBox.maxZ() + centerBox.minZ()) / 2;
        int centerY = center.getPosition().getY();
        AABB limit = new AABB(
                centerX - 116,
                Math.max(centerY - 116, level.getMinY() + 10),
                centerZ - 116,
                centerX + 117,
                Math.min(centerY + 117, level.getMaxY() + 1 - 10),
                centerZ + 117
        );
        VoxelShape shape = Shapes.join(Shapes.create(limit), Shapes.create(AABB.of(centerBox)), BooleanOp.ONLY_FIRST);
        List<PoolElementStructurePiece> pieces = new ArrayList<>();
        pieces.add(center);
        Registry<StructureTemplatePool> pools = level.registryAccess().lookupOrThrow(Registries.TEMPLATE_POOL);
        Method addPieces = JigsawPlacement.class.getDeclaredMethod(
                "addPieces",
                net.minecraft.world.level.levelgen.RandomState.class,
                int.class,
                boolean.class,
                net.minecraft.world.level.chunk.ChunkGenerator.class,
                StructureTemplateManager.class,
                net.minecraft.world.level.LevelHeightAccessor.class,
                RandomSource.class,
                Registry.class,
                PoolElementStructurePiece.class,
                List.class,
                VoxelShape.class,
                PoolAliasLookup.class,
                LiquidSettings.class
        );
        addPieces.setAccessible(true);
        addPieces.invoke(
                null,
                level.getChunkSource().randomState(),
                20,
                false,
                level.getChunkSource().getGenerator(),
                level.getStructureManager(),
                level,
                random,
                pools,
                center,
                pieces,
                shape,
                PoolAliasLookup.EMPTY,
                LiquidSettings.IGNORE_WATERLOGGING
        );
        for (int i = 0; i < Math.min(4, pieces.size()); i++) {
            PoolElementStructurePiece piece = pieces.get(i);
            System.err.println("java-jigsaw-piece " + i + " element=" + piece.getElement()
                    + " rotation=" + piece.getRotation() + " box=" + piece.getBoundingBox());
        }
    }

    public static void main(String[] ignored) throws Exception {
        verifySingleWorldgenWorker(); verifyCanonicalLightContract(); Args a = args(); if (a.help) { usage(); return; } if (a.provenanceSelftest) { provenanceSelftest(); return; }
        if (a.determinismSelftest) { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("determinism selftest requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); determinismSelftest(a, Path.of(root)); return; }
        if ("materialize".equals(a.mode)) { String root = System.getenv("ORACLE_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("materialize requires LODESTONE_ORACLE_WORLD_ROOT"); materialize(a, Path.of(root)); }
        else if ("diagnostic".equals(a.mode)) { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("diagnostic requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); diagnostic(a, Path.of(root)); }
        else if ("stream".equals(a.mode)) stream(a);
        else { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("export requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); export(a, Path.of(root)); }
    }
}
