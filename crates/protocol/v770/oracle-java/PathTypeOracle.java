import java.lang.reflect.Method;
import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.packs.PackResources;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.level.pathfinder.WalkNodeEvaluator;

/**
 * Authoritative node-evaluator path-type extractor. Boots the real 26.2 server
 * (registry bootstrap only, no world) and asks the game's own
 * {@code WalkNodeEvaluator.getPathTypeFromState(BlockGetter, BlockPos)} to
 * classify every registered block state. That method is the per-state base
 * classifier (no neighbour context); the neighbour/context layer
 * ({@code getPathTypeStatic} + {@code checkNeighbourBlocks}) is pathfinder
 * policy and deliberately excluded.
 *
 * Emits one line per state, ascending global state id:
 *   {@code <globalStateId> <blockName> <PATH_TYPE>}
 *
 * The classifier only consults the queried block state (plus its fluid state
 * and {@code isPathfindable(LAND)}), so a single-cell BlockGetter that returns
 * the state under test at every position is a faithful stand-in.
 */
public final class PathTypeOracle {
    /** A BlockGetter that reports one block state everywhere. */
    static final class SingleState implements BlockGetter {
        BlockState state;

        @Override
        public BlockEntity getBlockEntity(final BlockPos pos) {
            return null;
        }

        @Override
        public BlockState getBlockState(final BlockPos pos) {
            return state;
        }

        @Override
        public FluidState getFluidState(final BlockPos pos) {
            return state.getFluidState();
        }

        @Override
        public int getMinY() {
            return 0;
        }

        @Override
        public int getHeight() {
            return 0;
        }
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // Bootstrap initialises the registries but leaves every tag set empty:
        // tags are datapack content, not code. Without binding them,
        // BlockState.is(BlockTags.FENCES/WALLS/TRAPDOORS) and
        // FluidState.is(FluidTags.WATER/LAVA) all return false, which silently
        // mis-classifies fences/walls/trapdoors/water/lava. Load and bind the
        // built-in registry tags from the vanilla data pack before classifying.
        PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
        packs.reload();
        packs.setSelected(packs.getAvailableIds());
        List<PackResources> selected = packs.openAllSelected();
        MultiPackResourceManager resources =
                new MultiPackResourceManager(PackType.SERVER_DATA, selected);
        RegistryAccess.Frozen staticAccess =
                RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY);
        List<Registry.PendingTags<?>> pending =
                TagLoader.loadTagsForExistingRegistries(resources, staticAccess);
        pending.forEach(Registry.PendingTags::apply);

        // getPathTypeFromState is protected static; reach it by reflection so we
        // do not have to inject a class into a game package.
        Method classifier = WalkNodeEvaluator.class.getDeclaredMethod(
                "getPathTypeFromState", BlockGetter.class, BlockPos.class);
        classifier.setAccessible(true);

        SingleState getter = new SingleState();
        StringBuilder sb = new StringBuilder();
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            String name = BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
            getter.state = state;
            Object pathType = classifier.invoke(null, getter, BlockPos.ZERO);
            sb.setLength(0);
            sb.append(id).append(' ').append(name).append(' ').append(pathType);
            System.out.println(sb);
        }
    }
}
