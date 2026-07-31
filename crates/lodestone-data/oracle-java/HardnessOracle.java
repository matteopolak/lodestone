import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Authoritative per-block-state hardness/correct-tool extractor. Boots the
 * real 26.2 server (registries only, no world) and asks every registered
 * {@code BlockState} for its own {@code destroySpeed} and
 * {@code requiresCorrectToolForDrops}, so the numbers are version-exact and
 * immune to third-party dataset lag — mirrors {@code EntityDimensionsOracle}
 * and {@code PathTypeOracle} in this same directory.
 *
 * {@code BlockBehaviour.BlockStateBase.getDestroySpeed(BlockGetter, BlockPos)}
 * (decompiled source: {@code BlockBehaviour.java:645-647}) ignores both
 * parameters in the base implementation and ships {@code this.destroySpeed}
 * (set from {@code Properties.destroyTime}, decompiled source:
 * {@code BlockBehaviour.java:439/470}); no block subclass overrides it
 * (verified by grepping the decompiled block package — only
 * {@code PistonBaseBlock} *calls* it, nothing overrides it). So a `null`
 * `BlockGetter` and `BlockPos.ZERO` are a faithful stand-in — there is no
 * neighbour/world dependence to fake.
 *
 * {@code requiresCorrectToolForDrops()} (decompiled source:
 * {@code BlockBehaviour.java:903-905}) is a plain boolean field set once at
 * block-registration time from {@code Properties.requiresCorrectToolForDrops}
 * (decompiled source: {@code BlockBehaviour.java:440/471}); nothing overrides
 * the method either, so no tag/world bootstrap is required (unlike
 * {@code PathTypeOracle}, which needs vanilla tags for fence/wall/fluid
 * classification).
 *
 * Emits one line per state, ascending global state id:
 *   {@code <globalStateId> <blockName> <destroySpeedBits> <requiresCorrectTool>}
 *
 * `destroySpeedBits` is the raw hex `Float.floatToRawIntBits` bit pattern (not
 * a decimal literal) so no precision is lost in the text round-trip;
 * `-1.0F` (bedrock, barrier, ...) round-trips exactly the same way.
 */
public final class HardnessOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder sb = new StringBuilder();
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            String name = BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
            float destroySpeed = state.getDestroySpeed(null, BlockPos.ZERO);
            boolean requiresCorrectTool = state.requiresCorrectToolForDrops();
            int bits = Float.floatToRawIntBits(destroySpeed);
            sb.append(id).append(' ').append(name).append(' ')
                    .append(Integer.toHexString(bits)).append(' ')
                    .append(requiresCorrectTool ? 1 : 0).append('\n');
        }
        System.out.print(sb);
    }
}
