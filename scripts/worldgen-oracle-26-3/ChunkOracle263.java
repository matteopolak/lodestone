// Independent JVM oracle for 26.3 terrain-shape generation. It drives the real
// server's noise chunk exactly as chunk generation does before surface rules:
// build the chunk volume, create the aquifer, sample the final-density volume,
// then ask the aquifer for each cell's substance in z, x, descending-y order.
//
//   args: <seed> <settings> [beard=<scenario>] <chunkX> <chunkZ> [<chunkX> <chunkZ> ...] [dump]
// Output per chunk:
//   chunk <settings> <seed> <cx> <cz> density <fnv> <n> subst <fnv> D<n> A<n> W<n> L<n> X<n> sched <fnv> <count>
// With `dump` after the coordinates, also `cells <rle>` in volume index order.
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.TerrainAdjustment;
import net.minecraft.world.level.levelgen.structure.pools.JigsawJunction;
import net.minecraft.world.level.levelgen.structure.pools.StructureTemplatePool;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.*;

public final class ChunkOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false);

    static long fnvByte(long h, int b) { h ^= b & 0xFF; return h * 0x100000001b3L; }

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


    static Beardifier scenario(String file, String wanted) throws Exception {
        String name = null;
        List<String[]> cur = null;
        for (String line : java.nio.file.Files.readAllLines(java.nio.file.Path.of(file))) {
            if (line.startsWith("#") || line.isBlank()) continue;
            String[] f = line.trim().split("\\s+");
            if (f[0].equals("scenario")) { name = f[1]; cur = new ArrayList<>(); }
            else if (f[0].equals("end")) { if (name.equals(wanted)) return build(cur); }
            else cur.add(f);
        }
        throw new IllegalArgumentException("no scenario " + wanted);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createWorldLookup();
        long seed = Long.parseLong(args[0]);
        String name = args[1];
        boolean dump = args[args.length - 1].equals("dump");
        int end = dump ? args.length - 1 : args.length;
        int first = 2;
        Beardifier beardifier = null;
        if (args[2].startsWith("beard=")) {
            beardifier = scenario("/oracle/beard-scenarios.txt", args[2].substring(6));
            first = 3;
        }
        NoiseGeneratorSettings settings = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(name))).value();
        RandomState rs = RandomState.create(provider.lookupOrThrow(Registries.NOISE), seed, settings);
        int seaLevel = settings.seaLevel();
        Aquifer.FluidStatus lava = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
        Aquifer.FluidStatus sea = new Aquifer.FluidStatus(seaLevel, settings.defaultFluid());
        Aquifer.FluidPicker picker = (x, y, z) -> y < Math.min(-54, seaLevel) ? lava : sea;
        BlockState water = Blocks.WATER.defaultBlockState();
        BlockState lavaState = Blocks.LAVA.defaultBlockState();
        BlockState air = Blocks.AIR.defaultBlockState();
        for (int a = first; a + 1 < end; a += 2) {
            int cx = Integer.parseInt(args[a]);
            int cz = Integer.parseInt(args[a + 1]);
            DensityVolume volume = new DensityVolume(16, settings.noiseSettings().height(), 16, cx * 16, settings.noiseSettings().minY(), cz * 16);
            try (NoiseChunk nc = new NoiseChunk(rs, beardifier, settings, picker, Blender.empty(), volume)) {
                Aquifer aquifer = nc.aquifer();
                DensitySampler.Bound fd = nc.cachingSamplers().get(settings.noiseRouter().finalDensity());
                try (ScopedDensityBuffer buf = fd.sampleVolume(volume)) {
                    long dh = 0xcbf29ce484222325L;
                    long sh = 0xcbf29ce484222325L;
                    long fh = 0xcbf29ce484222325L;
                    int sched = 0;
                    int n = volume.size();
                    char[] cells = new char[n];
                    int[] counts = new int[5];
                    for (int i = 0; i < n; i++) {
                        int bits = Float.floatToRawIntBits(buf.get(i));
                        for (int k = 0; k < 4; k++) dh = fnvByte(dh, bits >>> (8 * k));
                    }
                    for (int z = 0; z < volume.sizeZ(); z++) for (int x = 0; x < volume.sizeX(); x++) for (int y = volume.sizeY() - 1; y >= 0; y--) {
                        int idx = volume.indexUnchecked(x, y, z);
                        BlockState s = aquifer.computeSubstance(volume.blockX(x), volume.blockY(y), volume.blockZ(z), buf.get(idx));
                        char c = s == null ? 'D' : s == air ? 'A' : s == water ? 'W' : s == lavaState ? 'L' : 'X';
                        cells[idx] = c;
                        counts["DAWLX".indexOf(c)]++;
                        sh = fnvByte(sh, c);
                        if (aquifer.shouldScheduleFluidUpdate()) {
                            sched++;
                            for (int k = 0; k < 4; k++) fh = fnvByte(fh, idx >>> (8 * k));
                        }
                    }
                    OUT.println("chunk " + name + " " + seed + " " + cx + " " + cz + " density " + Long.toHexString(dh) + " " + n
                        + " subst " + Long.toHexString(sh) + " D" + counts[0] + " A" + counts[1] + " W" + counts[2] + " L" + counts[3] + " X" + counts[4]
                        + " sched " + Long.toHexString(fh) + " " + sched);
                    if (dump) {
                        StringBuilder sb = new StringBuilder("cells ");
                        int run = 1;
                        for (int i = 1; i <= n; i++) {
                            if (i < n && cells[i] == cells[i - 1]) run++;
                            else { sb.append(cells[i - 1]).append(run).append(' '); run = 1; }
                        }
                        OUT.println(sb);
                    }
                }
            }
        }
        OUT.flush();
    }
}
