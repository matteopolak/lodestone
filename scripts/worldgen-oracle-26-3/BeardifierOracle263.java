// Independent JVM oracle for the structure-terrain density term. It builds the real
// server beardifier from the scenario file and hashes its scalar and volume samples.
//   args: <scenario file>
// Output: beard <scenario> scalar|vol<k> <fnv> <n> <first 6 raw hex>
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.densityfunction.*;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.TerrainAdjustment;
import net.minecraft.world.level.levelgen.structure.pools.JigsawJunction;
import net.minecraft.world.level.levelgen.structure.pools.StructureTemplatePool;

public final class BeardifierOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false);
    static final int[] XS = {-60, -31, -13, -5, 0, 3, 7, 12, 19, 26, 33, 41, 70};
    static final int[] YS = {-64, 0, 21, 33, 45, 52, 58, 61, 63, 66, 70, 75, 91, 120, 300};
    static final int[] ZS = {-50, -21, -8, 0, 5, 11, 17, 24, 35, 50};
    static final int[][] SHAPES = {
        {16, 24, 16, 0, 56, 0, 1, 1, 1},
        {16, 24, 16, 16, 40, -16, 1, 1, 1},
        {8, 12, 8, -8, 20, -24, 4, 8, 4},
        {5, 7, 5, 3, 2, 3, 8, 16, 8},
        {16, 384, 16, -16, -64, 16, 1, 1, 1},
    };

    static long fnv(long h, int bits) {
        for (int i = 0; i < 4; i++) { h ^= (bits >>> (8 * i)) & 0xFF; h *= 0x100000001b3L; }
        return h;
    }

    static String report(float[] values) {
        long h = 0xcbf29ce484222325L;
        StringBuilder first = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            int b = Float.floatToRawIntBits(values[i]);
            h = fnv(h, b);
            if (i < 6) first.append(' ').append(Integer.toHexString(b));
        }
        return Long.toHexString(h) + " " + values.length + first;
    }

    static Beardifier build(List<String[]> lines) {
        List<Beardifier.Rigid> rigids = new ArrayList<>();
        List<JigsawJunction> junctions = new ArrayList<>();
        BoundingBox any = null;
        for (String[] f : lines) {
            if (f[0].equals("rigid")) {
                BoundingBox box = new BoundingBox(Integer.parseInt(f[1]), Integer.parseInt(f[2]), Integer.parseInt(f[3]),
                    Integer.parseInt(f[4]), Integer.parseInt(f[5]), Integer.parseInt(f[6]));
                TerrainAdjustment adj = TerrainAdjustment.valueOf(f[7].toUpperCase());
                rigids.add(new Beardifier.Rigid(box, adj, Integer.parseInt(f[8])));
                any = any == null ? box : BoundingBox.encapsulating(any, box);
            } else {
                int x = Integer.parseInt(f[1]), y = Integer.parseInt(f[2]), z = Integer.parseInt(f[3]);
                junctions.add(new JigsawJunction(x, y, z, 0, StructureTemplatePool.Projection.RIGID));
                BoundingBox box = new BoundingBox(x, y, z, x, y, z);
                any = any == null ? box : BoundingBox.encapsulating(any, box);
            }
        }
        return new Beardifier(List.copyOf(rigids), List.copyOf(junctions), any == null ? null : any.inflatedBy(24));
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        List<String> all = Files.readAllLines(Path.of(args[0]));
        String name = null;
        List<String[]> cur = null;
        for (String line : all) {
            if (line.startsWith("#") || line.isBlank()) continue;
            String[] f = line.trim().split("\\s+");
            if (f[0].equals("scenario")) { name = f[1]; cur = new ArrayList<>(); }
            else if (f[0].equals("end")) {
                Beardifier b = build(cur);
                float[] sc = new float[XS.length * YS.length * ZS.length];
                int i = 0;
                for (int x : XS) for (int y : YS) for (int z : ZS) sc[i++] = b.sampleValue(SamplerContext.EMPTY_UNCACHED, x, y, z);
                OUT.println("beard " + name + " scalar " + report(sc));
                for (int k = 0; k < SHAPES.length; k++) {
                    int[] sh = SHAPES[k];
                    DensityVolume v = new DensityVolume(sh[0], sh[1], sh[2], sh[3], sh[4], sh[5], sh[6], sh[7], sh[8]);
                    DensityBuffer buf = DensityBuffer.createUnpooled(v.size());
                    b.sampleVolume(SamplerContext.EMPTY_UNCACHED, buf, v);
                    float[] out = new float[v.size()];
                    for (int j = 0; j < out.length; j++) out[j] = buf.get(j);
                    OUT.println("beard " + name + " vol" + k + " " + report(out));
                }
            } else cur.add(f);
        }
        OUT.flush();
    }
}
