// W1.1 oracle: per-block interpolated final-density over a whole chunk.
//
// Where RegionOracle dumps the *raw* noise router sampled point-by-point (which
// proves the density-function tree), this dumps what the real server's
// NoiseChunk actually writes to blocks: final_density after 4x8x4 cell-corner
// sampling + trilinear interpolation. We drive the exact doFill loop from
// NoiseBasedChunkGenerator (all public NoiseChunk methods) and read
// getInterpolatedDensity() per block via reflection (it is `protected`).
//
// No Mojang source is copied: we *invoke* the running server's NoiseChunk, the
// same interrogation pattern as ShapeOracle/RegionOracle. Seed 42, overworld,
// real 26.2 registries. Output: "x,y,z <rawLongBitsHex>" per block.
import java.lang.reflect.Method;

import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.core.HolderLookup;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.dimension.DimensionType;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.DensityFunctions;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.NoiseSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.util.Mth;

public final class DensityChunkOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        long seed = 42L;

        Holder<NoiseGeneratorSettings> holder =
            provider.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        NoiseGeneratorSettings settings = holder.value();
        RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);

        LevelHeightAccessor accessor = new LevelHeightAccessor() {
            public int getHeight() { return 384; }
            public int getMinY() { return -64; }
        };
        NoiseSettings noiseSettings = settings.noiseSettings().clampToHeightAccessor(accessor);
        int cellWidth = noiseSettings.getCellWidth();
        int cellHeight = noiseSettings.getCellHeight();
        int cellCountXZ = 16 / cellWidth;
        int cellCountY = Mth.floorDiv(noiseSettings.height(), cellHeight);
        int cellMinY = Mth.floorDiv(noiseSettings.minY(), cellHeight);

        Aquifer.FluidPicker fluidPicker =
            (x, y, z) -> new Aquifer.FluidStatus(63, Blocks.WATER.defaultBlockState());

        int chunkMinBlockX = 0;
        int chunkMinBlockZ = 0;

        NoiseChunk nc = new NoiseChunk(
            cellCountXZ, rs, chunkMinBlockX, chunkMinBlockZ, noiseSettings,
            Beardifier.EMPTY, settings, fluidPicker, Blender.empty());

        Method getDensity = NoiseChunk.class.getDeclaredMethod("getInterpolatedDensity");
        getDensity.setAccessible(true);

        StringBuilder sb = new StringBuilder(1 << 22);
        int cellCountX = 16 / cellWidth;
        int cellCountZ = 16 / cellWidth;

        nc.initializeForFirstCellX();
        for (int cx = 0; cx < cellCountX; cx++) {
            nc.advanceCellX(cx);
            for (int cz = 0; cz < cellCountZ; cz++) {
                for (int cy = cellCountY - 1; cy >= 0; cy--) {
                    nc.selectCellYZ(cy, cz);
                    for (int yic = cellHeight - 1; yic >= 0; yic--) {
                        int posY = (cellMinY + cy) * cellHeight + yic;
                        double fy = (double) yic / cellHeight;
                        nc.updateForY(posY, fy);
                        for (int xic = 0; xic < cellWidth; xic++) {
                            int posX = chunkMinBlockX + cx * cellWidth + xic;
                            double fx = (double) xic / cellWidth;
                            nc.updateForX(posX, fx);
                            for (int zic = 0; zic < cellWidth; zic++) {
                                int posZ = chunkMinBlockZ + cz * cellWidth + zic;
                                double fz = (double) zic / cellWidth;
                                nc.updateForZ(posZ, fz);
                                double d = (Double) getDensity.invoke(nc);
                                sb.append(posX).append(',').append(posY).append(',').append(posZ)
                                  .append(' ')
                                  .append(Long.toHexString(Double.doubleToRawLongBits(d)))
                                  .append('\n');
                            }
                        }
                    }
                }
            }
            nc.swapSlices();
        }
        nc.stopInterpolation();

        System.out.print(sb);
    }
}
