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
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
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
    static final byte[] MAGIC_V4 = "LWP26P04".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MAGIC_V5 = "LWP26P05".getBytes(StandardCharsets.US_ASCII);
    static final int HEADER_BYTES = 256, FORMAT_VERSION = 3, SCHEMA_VERSION = 3, DIGEST_BYTES = 32;
    static final int GRID_MIN = -250, GRID_MAX = 250;
    static final int GRID_SIDE = GRID_MAX - GRID_MIN + 1;
    static final long GRID_COUNT = (long) GRID_SIDE * GRID_SIDE;
    static final int HALO_MIN = GRID_MIN - 1, HALO_MAX = GRID_MAX + 1;
    static final long SEED = 42L;
    static final byte[] MANIFEST_DOMAIN = "lodestone.worldgen.large-parity.manifest/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MANIFEST_DOMAIN_V4 = "lodestone.worldgen.large-parity.manifest/v4/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] MANIFEST_DOMAIN_V5 = "lodestone.worldgen.large-parity.manifest/v5/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN = "lodestone.worldgen.large-parity.chunk/v3/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V4 = "lodestone.worldgen.large-parity.chunk/v4/semantic".getBytes(StandardCharsets.US_ASCII);
    static final byte[] RECORD_DOMAIN_V5 = "lodestone.worldgen.large-parity.chunk/v5/semantic".getBytes(StandardCharsets.US_ASCII);
    static final String FREEZE_STAMP = "lodestone-large-parity-v3.freeze.sha256";
    static final String MATERIALIZE_PROGRESS = "lodestone-large-parity-v3.materialize";
    static final int MATERIALIZE_TILE = 16;
    static final String OVERWORLD = "overworld", NETHER = "nether", END = "end";
    static String diagnosticPacketOut, diagnosticRecordOut;

    static final class Args {
        String out;
        String mode;
        String packetOut;
        String recordOut;
        int loX = GRID_MIN, hiX = GRID_MAX, loZ = GRID_MIN, hiZ = GRID_MAX;
        boolean resume, help;
        String dimension = OVERWORLD;
        boolean explicitDimension;
        boolean dimensionFormat() { return explicitDimension || !OVERWORLD.equals(dimension); }
        boolean v5() { return END.equals(dimension); }
        int semanticVersion() { return v5() ? 5 : dimensionFormat() ? 4 : FORMAT_VERSION; }
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
            case "--mode" -> out.mode = a[++i];
            case "--out" -> out.out = a[++i];
            case "--cx" -> { out.loX = Integer.parseInt(a[++i]); out.hiX = Integer.parseInt(a[++i]); }
            case "--cz" -> { out.loZ = Integer.parseInt(a[++i]); out.hiZ = Integer.parseInt(a[++i]); }
            case "--resume" -> out.resume = true;
            case "--packet-out" -> out.packetOut = a[++i];
            case "--record-out" -> out.recordOut = a[++i];
            case "--dimension" -> { out.dimension = a[++i].toLowerCase(); out.explicitDimension = true; }
            default -> throw new IllegalArgumentException("unknown argument " + a[i]);
        }
        if (out.help) return out;
        if (!OVERWORLD.equals(out.dimension) && !NETHER.equals(out.dimension) && !END.equals(out.dimension)) throw new IllegalArgumentException("--dimension must be overworld, nether, or end");
        if (!"materialize".equals(out.mode) && !"export".equals(out.mode)) throw new IllegalArgumentException("--mode must be materialize or export");
        if (out.loX > out.hiX || out.loZ > out.hiZ || out.loX < GRID_MIN || out.hiX > GRID_MAX || out.loZ < GRID_MIN || out.hiZ > GRID_MAX) throw new IllegalArgumentException("ranges must lie in -250..=250");
        if ("materialize".equals(out.mode) && out.out != null) throw new IllegalArgumentException("materialize has no --out; it seals the persistent world");
        if ("export".equals(out.mode) && out.out == null) throw new IllegalArgumentException("export requires --out");
        if ((out.packetOut != null || out.recordOut != null) && (out.loX != out.hiX || out.loZ != out.hiZ)) throw new IllegalArgumentException("--packet-out and --record-out require exactly one chunk");
        return out;
    }

    static void usage() {
        System.out.println("materialize: LargeParityOracle --mode materialize [--dimension overworld|nether|end]");
        System.out.println("export:      LargeParityOracle --mode export --out /oracle/shard.lwp --cx LO HI --cz LO HI [--dimension overworld|nether|end] [--resume] [--packet-out /oracle/chunk.bin] [--record-out /oracle/chunk.record]");
        System.out.println("materialize needs LODESTONE_ORACLE_WORLD_ROOT; export needs LODESTONE_ORACLE_FROZEN_WORLD_ROOT.");
    }

    static MessageDigest sha256() { try { return MessageDigest.getInstance("SHA-256"); } catch (Exception e) { throw new AssertionError(e); } }
    static byte[] digest(byte[] b) { return sha256().digest(b); }
    static String hex(byte[] b) { StringBuilder s = new StringBuilder(b.length * 2); for (byte v : b) s.append(String.format("%02x", v)); return s.toString(); }

    static byte[] header(Args a, long count, byte[] frozenDigest, byte[] payloadDigest) {
        ByteBuffer b = ByteBuffer.allocate(HEADER_BYTES).order(ByteOrder.BIG_ENDIAN);
        byte[] magic = a.v5() ? MAGIC_V5 : a.dimensionFormat() ? MAGIC_V4 : MAGIC;
        byte[] domain = a.v5() ? MANIFEST_DOMAIN_V5 : a.dimensionFormat() ? MANIFEST_DOMAIN_V4 : MANIFEST_DOMAIN;
        b.put(magic).putShort((short)a.semanticVersion()).putShort((short)HEADER_BYTES).putShort((short)2).putShort((short)a.semanticVersion()).putInt(776).putLong(SEED);
        b.putInt(GRID_MIN).putInt(GRID_MAX).putInt(GRID_MIN).putInt(GRID_MAX).putInt(a.loX).putInt(a.hiX).putInt(a.loZ).putInt(a.hiZ).putLong(count);
        b.putShort((short)DIGEST_BYTES).putShort((short)0).put(digest(domain)).put(frozenDigest).put(payloadDigest);
        if (a.dimensionFormat()) b.put(digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)));
        return b.array();
    }

    static long resumeRecords(File f, Args a, long count, byte[] frozenDigest) throws Exception {
        if (!f.exists()) return 0;
        if (!f.isFile() || f.length() < HEADER_BYTES || f.length() > HEADER_BYTES + count * DIGEST_BYTES || ((f.length() - HEADER_BYTES) % DIGEST_BYTES) != 0) throw new IllegalStateException("resume refuses malformed v3 shard: " + f);
        try (RandomAccessFile in = new RandomAccessFile(f, "r")) {
            byte[] h = new byte[HEADER_BYTES]; in.readFully(h); ByteBuffer b = ByteBuffer.wrap(h).order(ByteOrder.BIG_ENDIAN); byte[] magic = new byte[8]; b.get(magic);
            byte[] expectedMagic = a.v5() ? MAGIC_V5 : a.dimensionFormat() ? MAGIC_V4 : MAGIC; int expectedVersion = a.semanticVersion(), expectedSchema = a.semanticVersion();
            if (!Arrays.equals(magic, expectedMagic) || b.getShort() != expectedVersion || b.getShort() != HEADER_BYTES || b.getShort() != 2 || b.getShort() != expectedSchema || b.getInt() != 776 || b.getLong() != SEED) throw new IllegalStateException("manifest format differs; resume requires the selected parity format: " + f);
            b.position(28);
            if (b.getInt()!=GRID_MIN || b.getInt()!=GRID_MAX || b.getInt()!=GRID_MIN || b.getInt()!=GRID_MAX || b.getInt()!=a.loX || b.getInt()!=a.hiX || b.getInt()!=a.loZ || b.getInt()!=a.hiZ || b.getLong()!=count || b.getShort()!=DIGEST_BYTES) throw new IllegalStateException("resume shard geometry differs: " + f);
            b.getShort(); byte[] domain = new byte[32]; b.get(domain); byte[] recordedFrozen = new byte[32]; b.get(recordedFrozen); byte[] expected = new byte[32]; b.get(expected);
            byte[] expectedDomain = a.v5() ? MANIFEST_DOMAIN_V5 : a.dimensionFormat() ? MANIFEST_DOMAIN_V4 : MANIFEST_DOMAIN;
            if (!Arrays.equals(domain, digest(expectedDomain)) || !Arrays.equals(recordedFrozen, frozenDigest)) throw new IllegalStateException("resume schema or frozen-world identity differs: " + f);
            if (a.dimensionFormat()) { byte[] recordedDimension = new byte[32]; b.get(recordedDimension); if (!Arrays.equals(recordedDimension, digest(a.dimensionKey().getBytes(StandardCharsets.UTF_8)))) throw new IllegalStateException("resume dimension identity differs: " + f); }
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
    static String freezeStamp(Args a) { return a.dimensionFormat() ? "lodestone-large-parity-v4-" + a.dimension + ".freeze.sha256" : FREEZE_STAMP; }
    static String progressFile(Args a) { return a.dimensionFormat() ? "lodestone-large-parity-v4-" + a.dimension + ".materialize" : MATERIALIZE_PROGRESS; }
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
        Path stamp = root.resolve(freezeStamp(a)); if (!Files.isRegularFile(stamp)) throw new IllegalStateException("frozen world has no selected-dimension seal: " + stamp);
        byte[] actual = worldTreeDigest(root, a); String expected = Files.readString(stamp, StandardCharsets.US_ASCII).trim(); if (!hex(actual).equals(expected)) throw new IllegalStateException("frozen world differs from its seal; re-materialize before export"); return actual;
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
    static byte[] packetBody(MinecraftServer server, LevelChunk chunk, ServerLevel level) {
        ClientboundLevelChunkWithLightPacket packet = new ClientboundLevelChunkWithLightPacket(chunk, level.getLightEngine(), null, null);
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

    static List<ChunkPos> lightNeighbourhood(List<ChunkPos> positions) {
        java.util.LinkedHashSet<ChunkPos> result = new java.util.LinkedHashSet<>();
        for (ChunkPos center : positions) for (int z = -1; z <= 1; z++) for (int x = -1; x <= 1; x++) result.add(new ChunkPos(center.x() + x, center.z() + z));
        return new ArrayList<>(result);
    }

    static void awaitLight(MinecraftServer server, ServerLevel level, List<ChunkPos> positions) {
        List<CompletableFuture<?>> fences = server.submit(() -> {
            List<CompletableFuture<?>> result = new ArrayList<>(positions.size());
            for (ChunkPos pos : positions) result.add(level.getChunkSource().getLightEngine().waitForPendingTasks(pos.x(), pos.z()));
            return result;
        }).join();
        for (CompletableFuture<?> fence : fences) fence.join();
    }

    static void relightFromBlocks(MinecraftServer server, ServerLevel level, List<ChunkPos> positions) {
        server.submit(() -> {
            try {
                var engine = level.getChunkSource().getLightEngine();
                Method clear = net.minecraft.server.level.ThreadedLevelLightEngine.class.getDeclaredMethod("updateChunkStatus", ChunkPos.class);
                clear.setAccessible(true);
                for (ChunkPos pos : positions) clear.invoke(engine, pos);
                engine.tryScheduleUpdate();
            } catch (ReflectiveOperationException e) { throw new IllegalStateException("relight reset bridge failed", e); }
        }).join();
        awaitLight(server, level, positions);
        List<CompletableFuture<?>> initialized = server.submit(() -> {
            var engine = level.getChunkSource().getLightEngine(); List<CompletableFuture<?>> result = new ArrayList<>(positions.size());
            for (ChunkPos pos : positions) { LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z()); if (chunk == null) throw new IllegalStateException("relight lost chunk: " + pos); result.add(engine.initializeLight(chunk, false)); }
            engine.tryScheduleUpdate(); return result;
        }).join();
        for (CompletableFuture<?> future : initialized) future.join();
        List<CompletableFuture<?>> lit = server.submit(() -> {
            var engine = level.getChunkSource().getLightEngine(); List<CompletableFuture<?>> result = new ArrayList<>(positions.size());
            for (ChunkPos pos : positions) { LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z()); if (chunk == null) throw new IllegalStateException("relight lost chunk: " + pos); result.add(engine.lightChunk(chunk, false)); }
            engine.tryScheduleUpdate(); return result;
        }).join();
        for (CompletableFuture<?> future : lit) future.join();
        awaitLight(server, level, positions);
    }

    static void loadBatch(MinecraftServer server, ServerLevel level, Args a, List<ChunkPos> positions, boolean capture, List<byte[]> out) {
        List<ChunkPos> loaded = capture && a.v5() ? lightNeighbourhood(positions) : positions;
        List<CompletableFuture<?>> futures = server.submit(() -> { List<CompletableFuture<?>> result = new ArrayList<>(loaded.size()); for (ChunkPos pos : loaded) result.add(level.getChunkSource().addTicketAndLoadWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0)); return result; }).join();
        for (int i = 0; i < loaded.size(); i++) { net.minecraft.server.level.ChunkResult<?> result = (net.minecraft.server.level.ChunkResult<?>)futures.get(i).join(); if (!result.isSuccess()) throw new IllegalStateException("chunk generation failed at " + loaded.get(i) + ": " + result.getError()); }
        if (!capture) settleMaterializedBatch(server, level, positions);
        if (capture && a.v5()) relightFromBlocks(server, level, loaded);
        if (capture) out.addAll(server.submit(() -> { try { List<byte[]> result = new ArrayList<>(positions.size()); for (ChunkPos pos : positions) { LevelChunk chunk = level.getChunkSource().getChunkNow(pos.x(), pos.z()); if (chunk == null) throw new IllegalStateException("loaded chunk was evicted: " + pos); if (diagnosticPacketOut != null) Files.write(Path.of(diagnosticPacketOut), packetBody(server, chunk, level)); byte[] record = semanticRecord(level, chunk, a); if (diagnosticRecordOut != null) Files.write(Path.of(diagnosticRecordOut), record); result.add(digest(record)); } return result; } catch (Exception e) { throw new IllegalStateException("canonical chunk export failed", e); } }).join());
        if (!capture || !a.v5()) server.submit(() -> { for (ChunkPos pos : loaded) level.getChunkSource().removeTicketWithRadius(net.minecraft.server.level.TicketType.PLAYER_LOADING, pos, 0); }).join();
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
        String marker = a.dimensionFormat() ? "lodestone-large-parity-v4-" + a.dimension + "-materialize" : "lodestone-large-parity-v3-materialize";
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
        String marker = a.dimensionFormat() ? "lodestone-large-parity-v4-" + a.dimension + "-materialize" : "lodestone-large-parity-v3-materialize";
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
        int minX = Math.max(HALO_MIN, a.loX - 1), maxX = Math.min(HALO_MAX, a.hiX + 1), minZ = Math.max(HALO_MIN, a.loZ - 1), maxZ = Math.min(HALO_MAX, a.hiZ + 1);
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
        int epochTiles = materializeEpochTiles();
        Files.createDirectories(root);
        if (Files.exists(root.resolve(freezeStamp(a)))) throw new IllegalStateException("materialize refuses an already frozen world: " + root);
        MaterializeProgress progress;
        if (Files.exists(root.resolve(progressFile(a))) || Files.exists(root.resolve(progressFile(a) + ".tmp"))) {
            progress = readProgress(root, a); verifyProgress(a, progress, epochTiles);
        } else {
            try (var entries = Files.list(root)) {
                if (entries.findAny().isPresent()) throw new IllegalStateException("materialize requires an empty world root or its validated v3 progress journal: " + root);
            }
            int minX = Math.max(HALO_MIN, a.loX - 1), maxX = Math.min(HALO_MAX, a.hiX + 1), minZ = Math.max(HALO_MIN, a.loZ - 1), maxZ = Math.min(HALO_MAX, a.hiZ + 1);
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
            int batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256"))); long start = System.nanoTime();
            for (int tile = current.nextTile; tile < end; tile++) {
                int x0 = current.minX + (tile % current.tilesX) * MATERIALIZE_TILE, z0 = current.minZ + (tile / current.tilesX) * MATERIALIZE_TILE;
                List<ChunkPos> positions = new ArrayList<>(MATERIALIZE_TILE * MATERIALIZE_TILE); for (int z = z0; z <= Math.min(current.maxZ, z0 + MATERIALIZE_TILE - 1); z++) for (int x = x0; x <= Math.min(current.maxX, x0 + MATERIALIZE_TILE - 1); x++) positions.add(new ChunkPos(x, z));
                for (int off = 0; off < positions.size(); off += batch) loadBatch(server, level, a, positions.subList(off, Math.min(positions.size(), off + batch)), false, new ArrayList<>());
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
        diagnosticPacketOut = a.packetOut; diagnosticRecordOut = a.recordOut; byte[] frozen = frozenDigest(frozenRoot, a); Path copy = copyReadOnlyWorld(); long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1); File out = new File(a.out); long done = a.resume ? resumeRecords(out, a, count, frozen) : 0;
        if (done == count) { System.err.println("[large-parity " + formatLabel(a) + "] authenticated shard already complete: " + out); return; } if (out.getParentFile() != null) out.getParentFile().mkdirs();
        runServer(copy, true, a, (server, level) -> { MessageDigest payload = sha256(); try (RandomAccessFile file = new RandomAccessFile(out, "rw")) {
            if (done == 0) { file.setLength(HEADER_BYTES); file.seek(0); file.write(header(a, count, frozen, new byte[32])); file.seek(HEADER_BYTES); }
            else { file.seek(HEADER_BYTES); byte[] prefix = new byte[8192]; long left = done * DIGEST_BYTES; while (left != 0) { int n = file.read(prefix, 0, (int)Math.min(left, prefix.length)); if (n < 0) throw new IllegalStateException("partial shard ended before prefix"); payload.update(prefix, 0, n); left -= n; } file.seek(HEADER_BYTES + done * DIGEST_BYTES); }
            int width = a.hiX - a.loX + 1, batch = Math.max(1, Integer.parseInt(System.getenv().getOrDefault("LODESTONE_ORACLE_BATCH", "256"))); long start = System.nanoTime();
            for (long at = done; at < count; at += batch) { long end = Math.min(count, at + batch); List<ChunkPos> positions = new ArrayList<>(); for (long i = at; i < end; i++) positions.add(new ChunkPos(a.loX + (int)(i % width), a.loZ + (int)(i / width))); List<byte[]> hashes = new ArrayList<>(); loadBatch(server, level, a, positions, true, hashes); for (byte[] hash : hashes) { file.write(hash); payload.update(hash); } double rate = (end - done) / ((System.nanoTime() - start) / 1_000_000_000.0); ChunkPos last = positions.get(positions.size()-1); System.err.printf("[large-parity] %s chunks=%d/%d rate=%.1f chunks/s coord=(%d,%d)%n", a.dimension, end, count, rate, last.x(), last.z()); }
            file.seek(0); file.write(header(a, count, frozen, payload.digest()));
        } });
    }
    public static void main(String[] ignored) throws Exception {
        verifyCanonicalLightContract(); Args a = args(); if (a.help) { usage(); return; }
        if ("materialize".equals(a.mode)) { String root = System.getenv("ORACLE_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("materialize requires LODESTONE_ORACLE_WORLD_ROOT"); materialize(a, Path.of(root)); }
        else { String root = System.getenv("ORACLE_FROZEN_WORLD_ROOT"); if (root == null || root.isBlank()) throw new IllegalStateException("export requires LODESTONE_ORACLE_FROZEN_WORLD_ROOT"); export(a, Path.of(root)); }
    }
}
