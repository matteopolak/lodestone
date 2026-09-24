import net.minecraft.server.Bootstrap;
import net.minecraft.SharedConstants;
import net.minecraft.world.level.block.Blocks;

public final class BlockFactsOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        System.out.println("dark_oak_leaves.blocksMotion="
            + Blocks.DARK_OAK_LEAVES.defaultBlockState().blocksMotion());
        System.out.println("dark_oak_leaves.isSolid="
            + Blocks.DARK_OAK_LEAVES.defaultBlockState().isSolid());
        System.out.println("oak_leaves.blocksMotion="
            + Blocks.OAK_LEAVES.defaultBlockState().blocksMotion());
        System.out.println("stone.blocksMotion="
            + Blocks.STONE.defaultBlockState().blocksMotion());
    }
}
