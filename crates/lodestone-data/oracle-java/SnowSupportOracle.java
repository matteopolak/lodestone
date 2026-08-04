import java.util.ArrayList;
import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.LiquidBlock;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.material.Fluids;

/**
 * Authoritative extractor for the four per-block-state facts
 * {@code SnowAndFreezeFeature} (vanilla's {@code freeze_top_layer},
 * {@code TOP_LAYER_MODIFICATION}) needs and that no other committed census in
 * this crate carries.
 *
 * <p>Boots the real 26.2 server (registries only, no world, no data pack) the
 * same way {@code ShadeBrightnessOracle} and {@code HardnessOracle} in this
 * directory do. No tag is read by any of the four predicates, so no
 * {@code TagLoader} bind is needed — the two snow-support <i>tags</i>
 * ({@code cannot_support_snow_layer}, {@code support_override_snow_layer}) reach
 * the world generator through its own datapack resolver, not through this table.
 *
 * <p><b>Why each column has to be dumped rather than derived.</b>
 *
 * <ul>
 *   <li>{@code U} — {@code Block.isFaceFull(collisionShape, UP)}
 *       ({@code Block.java:345-348}), the geometric half of
 *       {@code SnowLayerBlock.canSurvive} ({@code SnowLayerBlock.java:76-86}).
 *       The predicate is {@code !Shapes.joinIsNotEmpty(Shapes.block(),
 *       shape.getFaceShape(UP), NOT_SAME)}, evaluated on the <i>discretised</i>
 *       {@code DiscreteVoxelShape} grid, after {@code VoxelShape.calculateFace}'s
 *       three-way branch on {@code isCubeLikeAlong(Y)} / empty slice / cube-like
 *       slice ({@code VoxelShape.java:197-245}). Re-deriving that from the AABB
 *       list {@link lodestone_data.collision_shapes} carries means
 *       re-implementing {@code SliceShape}, {@code CubePointRange} and the
 *       1.0E-7 fuzzy comparisons — a hand-rolled geometry lexer, which this repo
 *       has already been burned by once. Asking the jar costs one column.</li>
 *   <li>{@code L} — {@code !state.getFluidState().isEmpty()}, the second half of
 *       the {@code MOTION_BLOCKING} heightmap predicate
 *       ({@code Heightmap.java:151}: {@code input.blocksMotion() ||
 *       !input.getFluidState().isEmpty()}). {@link lodestone_data.block_solidity}
 *       carries the first half only. Deriving this from a property scan
 *       ("water, lava, or {@code waterlogged=true}") misses
 *       {@code bubble_column} and every future fluid-bearing block, and a
 *       heightmap that is one block off puts every snow layer one block off.</li>
 *   <li>{@code W} — {@code state.getFluidState().is(Fluids.WATER) &&
 *       state.getBlock() instanceof LiquidBlock}, which is exactly the ice
 *       condition in {@code Biome.shouldFreeze} ({@code Biome.java:153}). Note
 *       this is <i>narrower</i> than {@code L}: a waterlogged stair holds water
 *       but is not a {@code LiquidBlock}, so it never freezes. That distinction
 *       is invisible to a property scan and is the whole reason vanilla writes
 *       the {@code instanceof} check.</li>
 *   <li>{@code Y} — {@code state.hasProperty(BlockStateProperties.SNOWY)}, the
 *       {@code snowy} flip {@code SnowAndFreezeFeature.place} applies to the
 *       block under a placed snow layer ({@code SnowAndFreezeFeature.java:40-43}).
 *       This one <i>is</i> derivable from
 *       {@link lodestone_data.block_states#properties}, and the committed test
 *       asserts exactly that agreement — the column exists so "derivable" is a
 *       measurement, not a claim.</li>
 * </ul>
 *
 * <p><b>Known scope of {@code U}.</b> The shape is read at
 * {@code EmptyBlockGetter.INSTANCE} / {@code BlockPos.ZERO}, so
 * neighbour-dependent geometry (fences, walls, panes, chorus plant) reports its
 * no-neighbour shape — the same convention the committed collision census uses.
 * No block that world generation places at a surface top has neighbour-dependent
 * collision, and the {@code N} column below dumps the candidate set so that is a
 * checkable claim rather than an assertion.
 *
 * <p>Output:
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   N &lt;blockName&gt;                       (block declares dynamicShape())
 *   K &lt;U|L|W|Y&gt; &lt;countOfTrueStates&gt;
 *   P &lt;U|L|W|Y&gt; &lt;startStateId&gt; &lt;bitstring, up to 256 chars, ascending&gt;
 * </pre>
 *
 * {@code K} is the population count per column, emitted so a consumer can prove
 * its decoded bitset is not silently all-zero or all-one without recounting —
 * the degenerate-table failure mode a bitset makes easy to ship.
 */
public final class SnowSupportOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder blocks = new StringBuilder();
        StringBuilder dynamic = new StringBuilder();
        int blockCount = 0;
        for (Block block : BuiltInRegistries.BLOCK) {
            blockCount++;
            // `BlockBehaviour.Properties.dynamicShape` has no getter; the
            // observable consequence is that the state's shape cache is never
            // built (`BlockBehaviour.java:509-511`). `hasDynamicShape()` is the
            // public read of the same flag.
            if (block.hasDynamicShape()) {
                dynamic.append("N ").append(BuiltInRegistries.BLOCK.getKey(block)).append('\n');
            }
        }

        List<Boolean> faceFullUp = new ArrayList<>();
        List<Boolean> hasFluid = new ArrayList<>();
        List<Boolean> waterLiquidBlock = new ArrayList<>();
        List<Boolean> snowyProperty = new ArrayList<>();

        Block previousBlock = null;
        int count = 0;
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) {
            int id = Block.BLOCK_STATE_REGISTRY.getId(state);
            if (id != count) {
                throw new IllegalStateException(
                        "BLOCK_STATE_REGISTRY is not iterating in ascending id order: expected "
                                + count + ", got " + id);
            }
            if (state.getBlock() != previousBlock) {
                previousBlock = state.getBlock();
                blocks.append("B ").append(id).append(' ')
                        .append(BuiltInRegistries.BLOCK.getKey(previousBlock)).append('\n');
            }
            faceFullUp.add(Block.isFaceFull(
                    state.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO), Direction.UP));
            hasFluid.add(!state.getFluidState().isEmpty());
            waterLiquidBlock.add(
                    state.getFluidState().is(Fluids.WATER) && state.getBlock() instanceof LiquidBlock);
            snowyProperty.add(state.hasProperty(BlockStateProperties.SNOWY));
            count++;
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# SnowSupportOracle dump from the real 26.2 server (protocol 776): the four\n");
        sb.append("# per-block-state facts vanilla's freeze_top_layer needs.\n");
        sb.append("#   U = Block.isFaceFull(state.getCollisionShape(empty, ZERO), UP)\n");
        sb.append("#   L = !state.getFluidState().isEmpty()\n");
        sb.append("#   W = state.getFluidState().is(WATER) && block instanceof LiquidBlock\n");
        sb.append("#   Y = state.hasProperty(BlockStateProperties.SNOWY)\n");
        sb.append("# C <stateCount> <blockCount>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# N <blockName>   (block declares dynamicShape(); U reads its uncached shape)\n");
        sb.append("# K <U|L|W|Y> <countOfTrueStates>\n");
        sb.append("# P <U|L|W|Y> <startStateId> <bitstring up to 256 chars, ascending>\n");
        sb.append("C ").append(count).append(' ').append(blockCount).append('\n');
        sb.append(blocks);
        sb.append(dynamic);
        emitCount(sb, 'U', faceFullUp);
        emitCount(sb, 'L', hasFluid);
        emitCount(sb, 'W', waterLiquidBlock);
        emitCount(sb, 'Y', snowyProperty);
        emitBits(sb, 'U', faceFullUp);
        emitBits(sb, 'L', hasFluid);
        emitBits(sb, 'W', waterLiquidBlock);
        emitBits(sb, 'Y', snowyProperty);

        System.out.print(sb);
    }

    private static void emitCount(final StringBuilder sb, final char kind, final List<Boolean> bits) {
        int n = 0;
        for (Boolean b : bits) {
            if (b) {
                n++;
            }
        }
        sb.append("K ").append(kind).append(' ').append(n).append('\n');
    }

    private static void emitBits(final StringBuilder sb, final char kind, final List<Boolean> bits) {
        for (int start = 0; start < bits.size(); start += 256) {
            sb.append("P ").append(kind).append(' ').append(start).append(' ');
            int end = Math.min(start + 256, bits.size());
            for (int i = start; i < end; i++) {
                sb.append(bits.get(i) ? '1' : '0');
            }
            sb.append('\n');
        }
    }
}
