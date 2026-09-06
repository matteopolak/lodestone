// Manifest framing for the compiled-26.2 large packet-parity oracle.
//
// This deliberately does not reconstruct terrain. v2 records are defined only
// from a fully scheduled server chunk's raw level_chunk_with_light payload.
// The packet-source adapter is fail-closed until it can own a real ServerLevel
// bulk scheduler and stream-codec invocation.
import java.io.File;
import java.io.RandomAccessFile;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Arrays;

public final class LargeParityOracle {
    static final byte[] MAGIC = "LWP26P02".getBytes(StandardCharsets.US_ASCII);
    static final int HEADER_BYTES = 160, FORMAT_VERSION = 2, SCHEMA_VERSION = 2, FINGERPRINT_BYTES = 2;
    static final long SEED = 42L;
    static final byte[] MANIFEST_DOMAIN = "lodestone.worldgen.large-parity.manifest/v2".getBytes(StandardCharsets.US_ASCII);

    static final class Args {
        String out;
        int loX = -500, hiX = 500, loZ = -500, hiZ = 500;
        boolean resume, help;
    }

    static void usage() {
        System.out.println("usage: LargeParityOracle --out /oracle/shard.lwp --cx LO HI --cz LO HI [--resume]");
        System.out.println("v2 is fail-closed until a real ServerLevel bulk packet source is installed.");
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
                default -> throw new IllegalArgumentException("unknown argument " + a[i]);
            }
        }
        if (!out.help && (out.out == null || out.loX > out.hiX || out.loZ > out.hiZ || out.loX < -500 || out.hiX > 500 || out.loZ < -500 || out.hiZ > 500))
            throw new IllegalArgumentException("ranges must lie in -500..=500; pass --help for usage");
        return out;
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

    /// Returns durable records after authenticating a complete v2 shard. A
    /// zero payload digest marks a record-aligned interrupted shard; its prefix
    /// will be re-hashed by the future packet-source writer before append.
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

    static void packetSourceRequired(Args a) throws Exception {
        long count = (long)(a.hiX - a.loX + 1) * (a.hiZ - a.loZ + 1);
        if (a.resume && resumeRecords(new File(a.out), a, count) == count) {
            System.err.println("[large-parity] resume: authenticated complete shard already present: " + a.out);
            return;
        }
        throw new UnsupportedOperationException(
            "v2 requires a real ServerLevel bulk chunk-status source and the compiled "
            + "level_chunk_with_light stream codec; refusing to write a non-packet manifest");
    }

    public static void main(String[] ignored) throws Exception {
        Args a = args();
        if (a.help) { usage(); return; }
        packetSourceRequired(a);
    }
}
