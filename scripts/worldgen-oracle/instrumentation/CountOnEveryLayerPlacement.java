package net.minecraft.world.level.levelgen.placement;

import com.mojang.serialization.MapCodec;
import java.util.stream.Stream;
import java.util.stream.Stream.Builder;
import net.minecraft.core.BlockPos;
import net.minecraft.util.RandomSource;
import net.minecraft.util.valueproviders.ConstantInt;
import net.minecraft.util.valueproviders.IntProvider;
import net.minecraft.util.valueproviders.IntProviders;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.Heightmap;

@Deprecated
public class CountOnEveryLayerPlacement extends PlacementModifier {
    public static final MapCodec<CountOnEveryLayerPlacement> CODEC = IntProviders.codec(0, 256)
        .fieldOf("count")
        .xmap(CountOnEveryLayerPlacement::new, value -> value.count);
    private final IntProvider count;

    private CountOnEveryLayerPlacement(IntProvider count) { this.count = count; }
    public static CountOnEveryLayerPlacement of(IntProvider count) { return new CountOnEveryLayerPlacement(count); }
    public static CountOnEveryLayerPlacement of(int count) { return of(ConstantInt.of(count)); }

    @Override
    public Stream<BlockPos> getPositions(PlacementContext context, RandomSource random, BlockPos origin) {
        Builder<BlockPos> positions = Stream.builder();
        int layer = 0;
        boolean trace = origin.getX() == Integer.parseInt(System.getenv().getOrDefault("ORACLE_SOURCE_X", "999999")) * 16
            && origin.getZ() == Integer.parseInt(System.getenv().getOrDefault("ORACLE_SOURCE_Z", "999999")) * 16;
        boolean foundAny;
        do {
            foundAny = false;
            int attempts = this.count.sample(random);
            for (int i = 0; i < attempts; i++) {
                int x = random.nextInt(16) + origin.getX();
                int z = random.nextInt(16) + origin.getZ();
                int startY = context.getHeight(Heightmap.Types.MOTION_BLOCKING, x, z);
                int y = findOnGroundYPosition(context, x, startY, z, layer);
                if (trace) {
                    System.out.println("count-layer layer=" + layer + " attempt=" + i
                        + " x=" + x + " z=" + z + " startY=" + startY + " y=" + y);
                }
                if (y != Integer.MAX_VALUE) {
                    positions.add(new BlockPos(x, y, z));
                    foundAny = true;
                }
            }
            layer++;
        } while (foundAny);
        return positions.build();
    }

    @Override
    public PlacementModifierType<?> type() { return PlacementModifierType.COUNT_ON_EVERY_LAYER; }

    private static int findOnGroundYPosition(PlacementContext context, int x, int yStart, int z, int wantedLayer) {
        BlockPos.MutableBlockPos position = new BlockPos.MutableBlockPos(x, yStart, z);
        int layer = 0;
        BlockState current = context.getBlockState(position);
        for (int y = yStart; y >= context.getMinY() + 1; y--) {
            position.setY(y - 1);
            BlockState below = context.getBlockState(position);
            if (!isEmpty(below) && isEmpty(current) && !below.is(Blocks.BEDROCK)) {
                if (layer == wantedLayer) return position.getY() + 1;
                layer++;
            }
            current = below;
        }
        return Integer.MAX_VALUE;
    }

    private static boolean isEmpty(BlockState state) {
        return state.isAir() || state.is(Blocks.WATER) || state.is(Blocks.LAVA);
    }
}
