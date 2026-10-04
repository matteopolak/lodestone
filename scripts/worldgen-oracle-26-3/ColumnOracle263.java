// Independent JVM oracle for 26.3 terrain-shape generation. It drives the real
// server's noise chunk exactly as chunk generation does before surface rules:
// build the chunk volume, create the aquifer, sample the final-density volume,
// then ask the aquifer for each cell's substance in z, x, descending-y order.
//
//   args: <seed> <settings> <blockX> <blockZ> [<blockX> <blockZ> ...]
// Runs the single-column fill used for base-height queries (a 1 x height x 1 volume).
// Output per column:
//   column <settings> <seed> <x> <z> density <fnv> <n> subst <fnv> D<n> A<n> W<n> L<n> X<n>
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
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.*;

public final class ColumnOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false);

    static long fnvByte(long h, int b) { h ^= b & 0xFF; return h * 0x100000001b3L; }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createWorldLookup();
        long seed = Long.parseLong(args[0]);
        String name = args[1];
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
        for (int a = 2; a + 1 < args.length; a += 2) {
            int bx = Integer.parseInt(args[a]);
            int bz = Integer.parseInt(args[a + 1]);
            DensityVolume volume = new DensityVolume(1, settings.noiseSettings().height(), 1, bx, settings.noiseSettings().minY(), bz);
            try (NoiseChunk nc = new NoiseChunk(rs, null, settings, picker, Blender.empty(), volume)) {
                Aquifer aquifer = nc.aquifer();
                DensitySampler.Bound fd = nc.cachingSamplers().get(settings.noiseRouter().finalDensity());
                try (ScopedDensityBuffer buf = fd.sampleVolume(volume)) {
                    long dh = 0xcbf29ce484222325L;
                    long sh = 0xcbf29ce484222325L;
                    int n = volume.size();
                    int[] counts = new int[5];
                    for (int i = 0; i < n; i++) {
                        int bits = Float.floatToRawIntBits(buf.get(i));
                        for (int k = 0; k < 4; k++) dh = fnvByte(dh, bits >>> (8 * k));
                    }
                    for (int y = volume.sizeY() - 1; y >= 0; y--) {
                        BlockState s = aquifer.computeSubstance(bx, volume.blockY(y), bz, buf.get(volume.indexUnchecked(0, y, 0)));
                        char c = s == null ? 'D' : s == air ? 'A' : s == water ? 'W' : s == lavaState ? 'L' : 'X';
                        counts["DAWLX".indexOf(c)]++;
                        sh = fnvByte(sh, c);
                    }
                    OUT.println("column " + name + " " + seed + " " + bx + " " + bz + " density " + Long.toHexString(dh) + " " + n
                        + " subst " + Long.toHexString(sh) + " D" + counts[0] + " A" + counts[1] + " W" + counts[2] + " L" + counts[3] + " X" + counts[4]);
                }
            }
        }
        OUT.flush();
    }
}
