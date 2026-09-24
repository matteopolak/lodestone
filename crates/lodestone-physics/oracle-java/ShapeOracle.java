import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Authoritative collision-shape extractor. Runs the real 26.2 server code:
 * bootstraps the registries, then asks every registered block state for its
 * collision shape via the game's own VoxelShape pipeline. Emits raw double bits
 * so the numbers are bit-exact, one line per state:
 *   <globalStateId> <blockName> <n> [x0 y0 z0 x1 y1 z1]...  (bits, hex)
 */
public final class ShapeOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder sb = new StringBuilder();
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            String name = net.minecraft.core.registries.BuiltInRegistries.BLOCK
                    .getKey(state.getBlock()).toString();
            VoxelShape shape = state.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
            List<AABB> boxes = shape.toAabbs();
            sb.setLength(0);
            sb.append(id).append(' ').append(name).append(' ').append(boxes.size());
            for (AABB b : boxes) {
                sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.minX)));
                sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.minY)));
                sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.minZ)));
                sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.maxX)));
                sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.maxY)));
                sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.maxZ)));
            }
            System.out.println(sb);
        }
    }
}
