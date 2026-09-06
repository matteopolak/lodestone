// Exports canonical semantic chunk digests from one frozen compiled-server world.
// Packet bytes are deliberately not a baseline: map iteration and palette choices
// may vary while describing the same delivered chunk.
import com.mojang.authlib.yggdrasil.YggdrasilAuthenticationService;
import com.mojang.serialization.Dynamic;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.RandomAccessFile;
import java.lang.reflect.Method;
import java.net.Proxy;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
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
import net.minecraft.world.level.dimension.LevelStem;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.storage.LevelDataAndDimensions;
import net.minecraft.world.level.storage.LevelStorageSource;
import net.minecraft.network.protocol.game.ClientboundLevelChunkWithLightPacket;
import net.minecraft.network.RegistryFriendlyByteBuf;

public final class LargeParityOracle {
    static final byte[] MAGIC = "LWP26P03".getBytes(StandardCharsets.US_ASCII);
    static final int HEADER_BYTES = 256, FORMAT_VERSION = 3, SCHEMA_VERSION = 3, DIGEST_BYTES = 32;
    static final long SEED = 42L;
    static final byte[] MANIFEST_DOMAIN = "lodestone.worldgen.large-parity.manifest/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN = "lodestone.worldgen.large-parity.chunk/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final String FREEZE_STAMP = "lodestone-large-parity-v3.freeze.sha256";
    static String diagnosticPacketOut, diagnosticRecordOut;

    static final class Args {
        String out;
        String mode;
        String packetOut;
        String recordOut;
        int loX = -500, hiX = 500, loZ = -500, hiZ = 500;
        boolean resume, help;
    }

    static Args args() {
        String raw = System.getenv().getOrDefault("ORACLE_ARGS", "").trim();
        String[] a = raw.isEmpty() ? new String[0] : raw.split("\\s+");
        Args out = new Args();
        for (int i = 0; i < a.length; i++) switch (a[i]) {
            case "--help", "-h" -> out.help = true;
            case "--mode" -> out.mode = a[++i];
            case "--out" -> out.out = a[++i];
            case "--cx" -> { out.loX = Integer.parseInt(a[++i]); out.hiX = Integer.parseInt(a[++i]); }
            case "--cz" -> { out.loZ = Integer.parseInt(a[++i]); out.hiZ = Integer.parseInt(a[++i]); }
            case "--resume" -> out.resume = true;
            case "--packet-out" -> out.packetOut = a[++i];
            case "--record-out" -> out.recordOut = a[++i];
            default -> throw new IllegalArgumentException("unknown argument " + a[i]);
        }
        if (out.help) return out;
        if (!"materialize".equals(out.mode) && !"export".equals(out.mode)) throw new IllegalArgumentException("--mode must be materialize or export");
        if (out.loX > out.hiX || out.loZ > out.hiZ || out.loX < -500 || out.hiX > 500 || out.loZ < -500 || out.hiZ > 500) throw new IllegalArgumentException("ranges must lie in -500..=500");
        if ("materialize".equals(out.mode) && out.out != null) throw new IllegalArgumentException("materialize has no --out; it seals the persistent world");
        if ("export".equals(out.mode) && out.out == null) throw new IllegalArgumentException("export requires --out");
        if ((out.packetOut != null || out.recordOut != null) && (out.loX != out.hiX || out.loZ != out.hiZ)) throw new IllegalArgumentException("--packet-out and --record-out require exactly one chunk");
        return out;
    }

    static void usage() {
        System.out.println("materialize: LargeParityOracle --mode materialize");
        System.out.println("export:      LargeParityOracle --mode export --out /oracle/shard.lwp --cx LO HI --cz LO HI [--resume] [--packet-out /oracle/chunk.bin] [--record-out /oracle/chunk.record]");
        System.out.println("materialize needs LODESTONE_ORACLE_WORLD_ROOT; export needs LODESTONE_ORACLE_FROZEN_WORLD_ROOT.");
    }

    static MessageDigest sha256() { try { return MessageDigest.getInstance("SHA-256"); } catch (Exception e) { throw new AssertionError(e); } }
    static byte[] digest(byte[] b) { return sha256().digest(b); }
    static String hex(byte[] b) { StringBuilder s = new StringBuilder(b.length * 2); for (byte v : b) s.append(String.format("%02x", v)); return s.toString(); }

    static byte[] header(Args a, long count, byte[] frozenDigest, byte[] payloadDigest) {
        ByteBuffer b = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        b.put(MAGIC).putShort((short)FORMAT_VERSION).putShort((short)HEADER_BYTES).putShort((short)2).putShort((short)SCHEMA_VERSION).putInt(776).putLong(SEED);
        b.putInt(-500).putInt(500).putInt(-500).putInt(500).putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count);
        b.putShort((short)DIGEST_BYTES).putShort((short)0).put(digest(MANIFEST_DOMAIN)).put(frozenDigest).put(payloadDigest);
        return b.array();
    }

    static long resumeRecords(File f, Args a, long count, byte[] frozenDigest) throws Exception {
        if (!f.exists()) return 0;
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * DIGEST_BYTES || ((f.length() - HEADER_BYTES) % DIGEST_BYTES) != 0) throw new IllegalStateException("resume refuses malformed v3 shard: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN); byte[] magic = new byte[8]; b.get(magic);
            if (!Arrays.equals(magic, MAGIC) || b.getShort() != FORMAT_VERSION || b.getShort() != HEADER_BYTES || b.getShort() != 2 || b.getShort() != SCHEMA_VERSION || b.getInt() != 776 || b.getLong() != SEED) throw new IllegalStateException("v2/raw manifests are rejected; resume requires v3: " + f);
            b.position(28);
            if (b.getInt()!=-500 || b.getInt()!=500 || b.getInt()!=-500 || b.getInt()!=500 || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=DIGEST_BYTES) throw new IllegalStateException("resume shard geometry differs: " + f);
            b.getShort(); byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected);
            if (!Arrays.equals(domain, digest(MANIFEST_DOMAIN)) || !Arrays.equals(recordedFrozen, frozenDigest)) throw new IllegalStateException("resume schema or frozen-world identity differs: " + f);
            long records = (f.length() - HEADER_BYTES) / DIGEST_BYTES;
            if (records == count) { MessageDigest actual = sha256(); byte[] buf = new byte[8192]; int n; while ((n = in.read(buf)) != -1) actual.update(buf, 0, n); if (!Arrays.equals(expected, actual.digest())) throw new IllegalStateException("resume payload checksum differs: " + f); }
            else if (!Arrays.equals(expected, new byte[32])) throw new IllegalStateException("partial shard has a non-zero final checksum: " + f);
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
        Path source = Path.of(raw); Path copy = Path.of("/work/frozen-world-copy");
        try (var paths = Files.walk(source)) { for (Path from : paths.sorted().toList()) { Path to = copy.resolve(source.relativize(from).toString()); if (Files.isDirectory(from)) Files.createDirectories(to); else Files.copy(from, to, StandardCopyOption.COPY_ATTRIBUTES); } }
        return copy;
    }
    static byte[] worldTreeDigest(Path root) throws Exception {
        MessageDigest sha = sha256();
        try (var paths = Files.walk(root)) { for (Path path : paths.filter(Files::isRegularFile).filter(p -> !p.getFileName().toString().equals(FREEZE_STAMP)).sorted().toList()) {
            byte[] name = root.relativize(path).toString().replace(File.separatorChar, '/').getBytes(StandardCharsets.UTF_8);
            sha.update(ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN).putInt(name.length).array()); sha.update(name); sha.update(ByteBuffer.allocate(8).order(ByteOrder.BIG_ENDIAN).putLong(Files.size(path)).array());
            try (var input = Files.newInputStream(path)) { byte[] buf = new byte[8192]; for (int n; (n = input.read(buf)) != -1;) sha.update(buf, 0, n); }
        } }
        return sha.digest();
    }
    static byte[] frozenDigest(Path root) throws Exception {
        Path stamp = root.resolve(FREEZE_STAMP); if (!Files.isRegularFile(stamp)) throw new IllegalStateException("frozen world has no v3 seal: " + stamp);
        byte[] actual = worldTreeDigest(root); String expected = Files.readString(stamp, StandardCharsets.US_ASCII).trim(); if (!hex(actual).equals(expected)) throw new IllegalStateException("frozen world differs from its seal; re-materialize before export"); return actual;
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
    static void canonicalLight(DataOutputStream out, java.util.BitSet present, java.util.BitSet empty, List<byte[]> arrays) throws Exception {
        int cursor = 0;
        for (int section = 0; section < 26; section++) {
            if (present.get(section) && empty.get(section)) throw new IllegalStateException("light section has both present and empty bits");
            if (present.get(section)) { byte[] bytes = arrays.get(cursor++); if (bytes.length != 2048) throw new IllegalStateException("light array length"); out.writeByte(2); out.write(bytes); }
            else if (empty.get(section)) out.writeByte(1); else out.writeByte(0);
        }
        if (cursor != arrays.size()) throw new IllegalStateException("light mask/array count differs");
    }
    static byte[] packetBody(MinecraftServer server, LevelChunk chunk, ServerLevel level) {
        ClientboundLevelChunkWithLightPacket packet = new ClientboundLevelChunkWithLightPacket(chunk, level.getLightEngine(), null, null);
        ByteBuf bytes = Unpooled.buffer(); RegistryFriendlyByteBuf out = new RegistryFriendlyByteBuf(bytes, server.registryAccess());
        ClientboundLevelChunkWithLightPacket.STREAM_CODEC.encode(out, packet);
        byte[] body = new byte[out.readableBytes()]; out.getBytes(out.readerIndex(), body); out.release(); return body;
    }
    static byte[] semanticRecord(ServerLevel level, LevelChunk chunk) throws Exception {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream(120_000); DataOutputStream out = new DataOutputStream(bytes);
        out.write(RECORD_DOMAIN); out.writeInt(chunk.getPos().x()); out.writeInt(chunk.getPos().z());
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
        var light = packet.getLightData(); canonicalLight(out, light.getSkyYMask(), light.getEmptySkyYMask(), light.getSkyUpdates()); canonicalLight(out, light.getBlockYMask(), light.getEmptyBlockYMask(), light.getBlockUpdates()); out.flush(); return bytes.toByteArray();
    }

    static void loadBatch(MinecraftServer server, ServerLevel level, List<ChunkPos> positions, boolean capture, List<byte[]> out) {
        List<CompletableFuture<?>> futures = server.submit(() -> { List<CompletableFuture<?>> result = new ArrayList<>(positions.size()); for (ChunkPos pos : positions) result.add(level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)); return result; }).join();
        for (int i = 0; i < positions.size(); i++) { net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)futures.get(i).join(); if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + positions.get(i) + ": " + result.getError()); }
        if (capture) out.addAll(server.submit(() -> { try { List<byte[]> result = new ArrayList<>(positions.size()); for (ChunkPos pos : positions) { LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z()); if (chunk == null) throw new IllegalStateException("loaded chunk was evicted: " + pos); if (diagnosticPacketOut != null) Files.write(Path.of(diagnosticPacketOut), packetBody(server, chunk, level)); byte[] record = semanticRecord(level, chunk); if (diagnosticRecordOut != null) Files.write(Path.of(diagnosticRecordOut), record); result.add(digest(record)); } return result; } catch (Exception e) { throw new IllegalStateException("canonical chunk export failed", e); } }).join());
        server.submit(() -> { for (ChunkPos pos : positions) level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0); }).join();
    }
    static void materialize(Args a, Path root) throws Exception {
        // The halo is part of the frozen state: a feature may write one chunk past
        // the requested grid. This phase records no baseline bytes.
        // A failed generation can leave valid-looking region files with no finished
        // feature pass. Never resume such a root: freeze provenance requires one
        // complete materialization from an empty persistent directory.
        if (Files.exists(root)) try (var entries = Files.list(root)) {
            if (entries.findAny().isPresent()) throw new IllegalStateException("materialize requires an empty world root; refusing an unsealed or previously frozen world: " + root);
        }
        runServer(root, false, (server, level) -> {
            int batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256"))); int minX = Math.max(-501, a.loX - 1), maxX = Math.min(501, a.hiX + 1), minZ = Math.max(-501, a.loZ - 1), maxZ = Math.min(501, a.hiZ + 1); long total = (long)(maxX - minX + 1) * (maxZ - minZ + 1), done = 0, start = System.nanoTime();
            for (int z0 = minZ; z0 <= maxZ; z0 += 16) for (int x0 = minX; x0 <= maxX; x0 += 16) {
                List<ChunkPos> positions = new ArrayList<>(256); for (int z = z0; z <= Math.min(maxZ, z0 + 15); z++) for (int x = x0; x <= Math.min(maxX, x0 + 15); x++) positions.add(new ChunkPos(x, z));
                for (int off = 0; off < positions.size(); off += batch) loadBatch(server, level, positions.subList(off, Math.min(positions.size(), off + batch)), false, new ArrayList<>());
                done += positions.size(); if (done % 4096 == 0 || done == total) System.err.printf("[large-parity v3] materialized=%d/%d rate=%.1f chunks/s%n", done, total, done / ((System.nanoTime() - start) / 1_000_000_000.0));
            }
        });
        byte[] frozen = worldTreeDigest(root); Files.writeString(root.resolve(FREEZE_STAMP), hex(frozen) + "\n", StandardCharsets.US_ASCII); System.err.println("[large-parity v3] sealed frozen world " + hex(frozen));
    }

    interface ServerWork { void run(MinecraftServer server, ServerLevel level) throws Exception; }
    static void runServer(Path root, boolean requireExisting, ServerWork work) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); Bootstrap.validate(); Files.createDirectories(root); DedicatedServerSettings settings = new DedicatedServerSettings(Path.of("/work/server.properties")); LevelStorageSource storage = LevelStorageSource.createDefault(root);
        LevelStorageSource.LevelStorageAccess access = storage.validateAndCreateAccess(settings.getProperties().levelName); Dynamic<?> tag = access.hasWorldData() ? access.getUnfixedDataTagWithFallback() : null; if (requireExisting && tag == null) throw new IllegalStateException("frozen world is missing level data");
        PackRepository packs = ServerPacksSource.createPackRepository(access); WorldStem stem = loadWorld(settings.getProperties(), access, packs, tag); Services services = Services.create(new YggdrasilAuthenticationService(Proxy.NO_PROXY), root.toFile()); NotificationManager notifications = new NotificationManager(); ManagementServer management = JsonRpc.create(settings, notifications);
        DedicatedServer server = MinecraftServer.spin(thread -> { DedicatedServer s = new DedicatedServer(thread, access, packs, stem, Optional.empty(), settings, DataFixers.getDataFixer(), services, management, notifications); notifications.setServer(s); s.setPort(25565); return s; });
        try { while (server.overworld() == null) Thread.sleep(25); work.run(server, server.overworld()); } finally { server.halt(true); stem.close(); access.close(); }
    }
    static void export(Args a, Path frozenRoot) throws Exception {
        diagnosticPacketOut = a.packetOut; diagnosticRecordOut = a.recordOut; byte[] frozen = frozenDigest(frozenRoot); Path copy = copyReadOnlyWorld(); long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1); File out = new File(a.out); long done = a.resume ? resumeRecords(out, a, count, frozen) : 0;
        if (done == count) { System.err.println("[large-parity v3] authenticated shard already complete: " + out); return; } if (out.getParentFile() != null) out.getParentFile().mkdirs();
        runServer(copy, true, (server, level) -> { MessageDigest payload = sha256(); try (RandomAccessFile file = new RandomAccessFile(out, "rw")) {
            if (done == 0) { file.setLength(HEADER_BYTES); file.seek(0); file.write(header(a, count, frozen, new byte[32])); file.seek(HEADER_BYTES); }
            else { file.seek(HEADER_BYTES); byte[] prefix = new byte[8192]; long left = done * DIGEST_BYTES; while (left != 0) { int n = file.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial shard ended before prefix"); payload.update(prefix, 0, n); left -= n; } file.seek(HEADER_BYTES + done * DIGEST_BYTES); }
            int width = a.hiX - a.loX + 1, batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256"))); long start = System.nanoTime();
            for (long at = done; at < count; at += batch) { long end = Math.min(count, at + batch); List<ChunkPos> positions = new ArrayList<>(); for (long i = at; i < end; i++) positions.add(new ChunkPos(a.loX + (int)(i % width), a.loZ + (int)(i / width))); List<byte[]> hashes = new ArrayList<>(); loadBatch(server, level, positions, true, hashes); for (byte[] hash : hashes) { file.write(hash); payload.update(hash); } double rate = (end - done) / ((System.nanoTime() - start) / 1_000_000_000.0); ChunkPos last = positions.get(positions.size()-1); System.err.printf("[large-parity v3] chunks=%d/%d rate=%.1f chunks/s coord=(%d,%d)%n", end, count, rate, last.x(), last.z()); }
            file.seek(0); file.write(header(a, count, frozen, payload.digest()));
        } });
    }
    public static void main(String[] ignored) throws Exception {
        Args a = args(); if (a.help) { usage(); return; }
        if ("materialize".equals(a.mode)) { String root = System.getenv("ORACLE_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("materialize requires LODESTONE_ORACLE_WORLD_ROOT"); materialize(a, Path.of(root)); }
        else { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("export requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); export(a, Path.of(root)); }
    }
}
