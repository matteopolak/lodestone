// Export exact level_chunk_with_light bodies from a real compiled 26.2 server.
// The server is booted in-process so the chunk source, status pipeline, light
// engine, registry access, and packet stream codec are the production classes.
import com.mojang.authlib.yggdrasil.YggdrasilAuthenticationService;
import com.mojang.serialization.Dynamic;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import java.io.File;
import java.io.RandomAccessFile;
import java.lang.reflect.Method;
import java.net.Proxy;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Files;
import java.security.MessageDigest;
import java.util.Arrays;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.function.Function;
import net.minecraft.SharedConstants;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.protocol.game.ClientboundLevelChunkWithLightPacket;
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
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.util.Util;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.dimension.LevelStem;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.storage.LevelDataAndDimensions;
import net.minecraft.world.level.storage.LevelStorageSource;

public final class LargeParityOracle {
    static final byte[] MAGIC = "LWP26P02".getBytes(StandardCharsets.US_ASCII);
    static final int HEADER_BYTES = 160, FORMAT_VERSION = 2, SCHEMA_VERSION = 2, FINGERPRINT_BYTES = 2;
    static final long SEED = 42L;
    static final byte[] MANIFEST_DOMAIN = "lodestone.worldgen.large-parity.manifest/v2".getBytes(StandardCharsets.US_ASCII);

    static final class Args {
        String out;
        int loX = -500, hiX = 500, loZ = -500, hiZ = 500;
        boolean resume, help;
        String packetOut;
    }

    static Args args() {
        String raw = System.getenv().getOrDefault("ORACLE_ARGS", "").trim();
        String[] a = raw.isEmpty() ? new String[0] : raw.split("\\s+");
        Args out = new Args();
        for (int i = 0; i < a.length; i++) {
            switch (a[i]) {
                case "--help", "-h" -> out.help = true;
                case "--out" -> out.out = a[++i];
                case "--cx" -> { out.loX = Integer.parseInt(a[++i]); out.hiX = Integer.parseInt(a[++i]); }
                case "--cz" -> { out.loZ = Integer.parseInt(a[++i]); out.hiZ = Integer.parseInt(a[++i]); }
                case "--resume" -> out.resume = true;
                case "--packet-out" -> out.packetOut = a[++i];
                default -> throw new IllegalArgumentException("unknown argument " + a[i]);
            }
        }
        if (!out.help && (out.out == null || out.loX > out.hiX || out.loZ > out.hiZ || out.loX < -500 || out.hiX > 500 || out.loZ < -500 || out.hiZ > 500))
            throw new IllegalArgumentException("ranges must lie in -500..=500; pass --help for usage");
        if (out.packetOut != null && (out.loX != out.hiX || out.loZ != out.hiZ))
            throw new IllegalArgumentException("--packet-out requires exactly one chunk");
        return out;
    }

    static void usage() {
        System.out.println("usage: LargeParityOracle --out /out/shard.lwp --cx LO HI --cz LO HI [--resume] [--packet-out /oracle/chunk.bin]");
        System.out.println("each record is the first 16 bits of SHA-256(level_chunk_with_light body)");
    }

    static MessageDigest sha256() {
        try { return MessageDigest.getInstance("SHA-256"); }
        catch (Exception e) { throw new AssertionError(e); }
    }
    static byte[] digest(byte[] b) { return sha256().digest(b); }

    static byte[] header(Args a, long count, byte[] payloadDigest) {
        ByteBuffer b = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        b.put(MAGIC).putShort((short)FORMAT_VERSION).putShort((short)HEADER_BYTES).putShort((short)1).putShort((short)SCHEMA_VERSION).putInt(776).putLong(SEED);
        b.putInt(-500).putInt(500).putInt(-500).putInt(500).putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count).put(digest(MANIFEST_DOMAIN)).put(payloadDigest);
        return b.array();
    }

    static long resumeRecords(File f, Args a, long count) throws Exception {
        if (!f.exists()) return 0;
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * FINGERPRINT_BYTES || ((f.length() - HEADER_BYTES) % FINGERPRINT_BYTES) != 0)
            throw new IllegalStateException("resume refuses malformed partial shard: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN);
            byte[] magic = new byte[8]; b.get(magic);
            if (!Arrays.equals(magic, MAGIC) || b.getShort() != FORMAT_VERSION || b.getShort() != HEADER_BYTES || b.getShort() != 1 || b.getShort() != SCHEMA_VERSION || b.getInt() != 776 || b.getLong() != SEED)
                throw new IllegalStateException("resume header differs: " + f);
            b.position(28);
            if (b.getInt()!=-500 || b.getInt()!=500 || b.getInt()!=-500 || b.getInt()!=500 || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count)
                throw new IllegalStateException("resume shard geometry differs: " + f);
            byte[] domain = new byte[32]; b.get(domain);
            if (!Arrays.equals(domain, digest(MANIFEST_DOMAIN))) throw new IllegalStateException("resume schema differs: " + f);
            byte[] expected = new byte[32]; b.get(expected); long records = (f.length() - HEADER_BYTES) / FINGERPRINT_BYTES;
            if (records == count) {
                MessageDigest actual = sha256(); byte[] buf = new byte[8192]; int n;
                while ((n = in.read(buf)) != -1) actual.update(buf, 0, n);
                if (!Arrays.equals(expected, actual.digest())) throw new IllegalStateException("resume payload checksum differs: " + f);
            } else if (!Arrays.equals(expected, new byte[32])) {
                throw new IllegalStateException("partial shard has a non-zero final checksum: " + f);
            }
            return records;
        }
    }

    static <T> T privateMain(String name, Class<?>[] types, Object... values) throws Exception {
        Method method = Main.class.getDeclaredMethod(name, types);
        method.setAccessible(true);
        @SuppressWarnings("unchecked") T result = (T) method.invoke(null, values);
        return result;
    }

    static WorldStem loadWorld(DedicatedServerProperties properties, LevelStorageSource.LevelStorageAccess access,
                               PackRepository packs, Dynamic<?> tag) throws Exception {
        WorldLoader.InitConfig config = privateMain("loadOrCreateConfig",
            new Class<?>[]{DedicatedServerProperties.class, Dynamic.class, boolean.class, PackRepository.class},
            properties, tag, false, packs);
        return Util.blockUntilDone(executor -> WorldLoader.load(config, context -> {
            Registry<LevelStem> dimensions = context.datapackDimensions().lookupOrThrow(Registries.LEVEL_STEM);
            if (tag != null) {
                LevelDataAndDimensions data = LevelStorageSource.getLevelDataAndDimensions(access, tag, context.dataConfiguration(), dimensions, context.datapackWorldgen());
                return new WorldLoader.DataLoadOutput<>(data.worldDataAndGenSettings(), data.dimensions().dimensionsRegistryAccess());
            }
            try {
                return privateMain("createNewWorldData",
                    new Class<?>[]{DedicatedServerSettings.class, WorldLoader.DataLoadContext.class, Registry.class, boolean.class, boolean.class},
                    new DedicatedServerSettings(Path.of("/work/server.properties")), context, dimensions, false, false);
            } catch (Exception e) {
                throw new IllegalStateException("creating new world data failed", e);
            }
        }, WorldStem::new, Util.backgroundExecutor(), executor)).get();
    }

    static byte[] packetBody(MinecraftServer server, ServerLevel level, LevelChunk chunk) {
        ClientboundLevelChunkWithLightPacket packet = new ClientboundLevelChunkWithLightPacket(chunk, level.getLightEngine(), null, null);
        ByteBuf bytes = Unpooled.buffer();
        RegistryFriendlyByteBuf out = new RegistryFriendlyByteBuf(bytes, server.registryAccess());
        ClientboundLevelChunkWithLightPacket.STREAM_CODEC.encode(out, packet);
        byte[] body = new byte[out.readableBytes()];
        out.getBytes(out.readerIndex(), body);
        out.release();
        return body;
    }

    static void export(Args a) throws Exception {
        long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1);
        File out = new File(a.out);
        long done = a.resume ? resumeRecords(out, a, count) : 0;
        if (done == count) { System.err.println("[large-parity] authenticated shard already complete: " + out); return; }
        if (out.getParentFile() != null) out.getParentFile().mkdirs();
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); Bootstrap.validate();
        Path root = Path.of("/work/universe"); Files.createDirectories(root);
        DedicatedServerSettings settings = new DedicatedServerSettings(Path.of("/work/server.properties"));
        LevelStorageSource storage = LevelStorageSource.createDefault(root);
        LevelStorageSource.LevelStorageAccess access = storage.validateAndCreateAccess(settings.getProperties().levelName);
        Dynamic<?> tag = access.hasWorldData() ? access.getUnfixedDataTagWithFallback() : null;
        PackRepository packs = ServerPacksSource.createPackRepository(access);
        WorldStem stem = loadWorld(settings.getProperties(), access, packs, tag);
        Services services = Services.create(new YggdrasilAuthenticationService(Proxy.NO_PROXY), root.toFile());
        NotificationManager notifications = new NotificationManager();
        ManagementServer management = JsonRpc.create(settings, notifications);
        DedicatedServer server = MinecraftServer.spin(thread -> {
            DedicatedServer s = new DedicatedServer(thread, access, packs, stem, Optional.empty(), settings, DataFixers.getDataFixer(), services, management, notifications);
            notifications.setServer(s); s.setPort(25565); return s;
        });
        while (server.overworld() == null) Thread.sleep(25);
        ServerLevel level = server.overworld();
        MessageDigest payloadDigest = sha256();
        try (RandomAccessFile file = new RandomAccessFile(out, "rw")) {
            if (done == 0) { file.setLength(HEADER_BYTES); file.seek(0); file.write(header(a, count, new byte[32])); file.seek(HEADER_BYTES); }
            else {
                file.seek(HEADER_BYTES);
                byte[] prefix = new byte[8192];
                long left = done * FINGERPRINT_BYTES;
                while (left != 0) {
                    int n = file.read(prefix, 0, (int)Math.min(left, prefix.length));
                    if (n < 0) throw new IllegalStateException("partial shard ended before its advertised prefix");
                    payloadDigest.update(prefix, 0, n); left -= n;
                }
                file.seek(HEADER_BYTES + done * FINGERPRINT_BYTES);
            }
            long start = System.nanoTime();
            final int batchSize = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256")));
            for (long batchStart = done; batchStart < count; batchStart += batchSize) {
                long batchEnd = Math.min(count, batchStart + batchSize);
                int width = a.hiX - a.loX + 1;
                List<ChunkPos> positions = new ArrayList<>((int)(batchEnd - batchStart));
                for (long index = batchStart; index < batchEnd; index++) {
                    positions.add(new ChunkPos(a.loX + (int)(index % width), a.loZ + (int)(index / width)));
                }
                // Keep a non-expiring loading ticket for every target until its packet
                // has been encoded. The ordinary getChunkFuture path uses a one-tick
                // ticket, so a later batch can otherwise unload chunks before encode.
                List<CompletableFuture<?>> regionFutures = server.submit(() -> {
                    List<CompletableFuture<?>> futures = new ArrayList<>(positions.size());
                    for (ChunkPos pos : positions) {
                        futures.add(level.getChunkSource().addTicketAndLoadWithRadius(
                            net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0));
                    }
                    return futures;
                }).join();
                for (int i = 0; i < positions.size(); i++) {
                    net.minecraft.server.level.ChunkResult<?> region = (net.minecraft.server.level.ChunkResult<?>) regionFutures.get(i).join();
                    if (!region.isSuccess()) throw new IllegalStateException("region generation failed at " + positions.get(i) + ": " + region.getError());
                }
                List<byte[]> hashes = server.submit(() -> {
                    List<byte[]> result = new ArrayList<>(positions.size());
                    for (ChunkPos pos : positions) {
                        LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z());
                        if (chunk == null) throw new IllegalStateException("region future did not retain FULL chunk at " + pos);
                        chunk.postProcessGeneration(level);
                        byte[] body = packetBody(server, level, chunk);
                        if (a.packetOut != null) {
                            try {
                                Files.write(Path.of(a.packetOut), body);
                            } catch (java.io.IOException e) {
                                throw new IllegalStateException("writing requested packet capture failed", e);
                            }
                        }
                        result.add(digest(body));
                    }
                    return result;
                }).join();
                for (byte[] hash : hashes) {
                    file.write(hash, 0, 2); payloadDigest.update(hash, 0, 2);
                }
                server.submit(() -> {
                    for (ChunkPos pos : positions) {
                        level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0);
                    }
                }).join();
                double rate = (batchEnd - done) / ((System.nanoTime() - start) / 1_000_000_000.0);
                System.err.printf("[large-parity] chunks=%d/%d rate=%.1f chunks/s coord=(%d,%d)%n", batchEnd, count, rate,
                    a.loX + (int)((batchEnd - 1) % width), a.loZ + (int)((batchEnd - 1) / width));
            }
            byte[] finalDigest = payloadDigest.digest(); file.seek(0); file.write(header(a, count, finalDigest));
        } finally { server.halt(true); stem.close(); access.close(); }
    }

    public static void main(String[] ignored) throws Exception {
        Args a = args();
        if (a.help) { usage(); return; }
        export(a);
    }
}
