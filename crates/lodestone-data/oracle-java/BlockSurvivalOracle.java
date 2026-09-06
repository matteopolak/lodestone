import java.lang.reflect.Method;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.FireBlock;
import net.minecraft.world.level.block.SupportType;
import net.minecraft.world.level.block.state.BlockState;

/** Emits the exact per-state predicates required by simple-block survival. */
public final class BlockSurvivalOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        FireBlock fire = (FireBlock) Blocks.FIRE;
        Method canBurn = FireBlock.class.getDeclaredMethod("canBurn", BlockState.class);
        canBurn.setAccessible(true);

        StringBuilder out = new StringBuilder("C 32366\n");
        int expected = 0;
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            if (id != expected) {
                throw new IllegalStateException("state registry order: expected " + expected + ", got " + id);
            }
            boolean solidRender = state.isSolidRender();
            boolean sturdyUp = state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.UP);
            boolean centerSupportDown = state.isFaceSturdy(
                EmptyBlockGetter.INSTANCE, BlockPos.ZERO, Direction.DOWN, SupportType.CENTER
            );
            boolean fireFlammable = ((Boolean) canBurn.invoke(fire, state)).booleanValue();
            out.append("S ").append(id).append(' ')
                .append(solidRender ? '1' : '0').append(' ')
                .append(sturdyUp ? '1' : '0').append(' ')
                .append(centerSupportDown ? '1' : '0').append(' ')
                .append(fireFlammable ? '1' : '0').append('\n');
            expected++;
        }
        if (expected != 32366) {
            throw new IllegalStateException("state count: expected 32366, got " + expected);
        }
        System.out.print(out);
    }
}
