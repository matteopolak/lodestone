import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Authoritative extractor for the <b>ambient-occlusion occluder predicate</b>:
 * per block state, {@code BlockStateBase.getShadeBrightness}
 * ({@code BlockBehaviour.java:617-619}), which is the value
 * {@code BlockModelLighter.prepareQuadAmbientOcclusion} averages into every
 * smooth-lit vertex ({@code BlockModelLighter.java:45-110}).
 *
 * <p>Boots the real 26.2 server (registries only, no world) exactly like
 * {@code BlockPhysicsOracle} and {@code HardnessOracle} in this directory. No
 * data pack is loaded because no branch of {@code getShadeBrightness} reads a
 * tag — unlike {@code BlockPhysicsOracle}, which needs {@code BlockTags.CLIMBABLE}
 * bound or it dumps a table of {@code false}s.
 *
 * <p><b>Why this cannot be derived from the committed collision census.</b> The
 * base implementation ({@code BlockBehaviour.java:315-317}) is
 *
 * <pre>
 *   return state.isCollisionShapeFullBlock(level, pos) ? 0.2F : 1.0F;
 * </pre>
 *
 * but seven classes override it, and the overrides go <i>both</i> ways:
 * {@code TransparentBlock}, {@code BarrierBlock}, {@code LightBlock} and
 * {@code StructureVoidBlock} return a flat {@code 1.0} where the shape says
 * {@code 0.2}; {@code MudBlock} and {@code SoulSandBlock} return a flat
 * {@code 0.2} where the shape (both sink an entity, so neither collision box is
 * a full cube) says {@code 1.0}; and {@code SnowLayerBlock} is per-state
 * ({@code LAYERS == 8 ? 0.2 : 1.0}). Emitting both columns lets a consumer
 * measure that divergence rather than assert it — the {@code L}/{@code G} trick
 * {@code BlockPhysicsOracle} uses for solidity.
 *
 * <p>The {@code O} census makes "exactly seven classes override this" a
 * <b>mechanical</b> fact rather than a {@code grep} someone transcribed:
 * {@code getShadeBrightness} is {@code protected}, so a hand-written list of
 * affected <i>blocks</i> would have to expand {@code TransparentBlock}'s family
 * by hand, which is precisely the hand-counted-census mistake this repo has
 * shipped twice.
 *
 * <p>Output:
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt;
 *   O &lt;blockName&gt; &lt;declaringClassOfGetShadeBrightnessOverride&gt;
 *   V &lt;floatBitsHex&gt; &lt;stateCount with that value&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   P &lt;S|F&gt; &lt;startStateId&gt; &lt;bitstring, up to 256 chars, ascending&gt;
 * </pre>
 *
 * {@code S} is {@code getShadeBrightness(...) == 0.2F} — the answer an AO
 * sampler wants, reduced to a bit. {@code F} is
 * {@code isCollisionShapeFullBlock(...)} alone, i.e. the base formula with every
 * override stripped. {@code V} is the full histogram of distinct
 * {@code getShadeBrightness} return values across all states, emitted so a
 * consumer can prove the boolean reduction in {@code S} is <b>lossless</b>: if
 * any state ever returned a third value, {@code V} would carry three rows and
 * the bit encoding would be silently wrong.
 */
public final class ShadeBrightnessOracle {
    /** Vanilla's occluded shade sample. Not 0.0 — see {@code AO_OCCLUDED}. */
    private static final float OCCLUDED = 0.2F;

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder overrides = new StringBuilder();
        int blockCount = 0;
        for (Block block : BuiltInRegistries.BLOCK) {
            String owner = shadeBrightnessOwner(block.getClass());
            if (owner != null) {
                overrides.append("O ")
                        .append(BuiltInRegistries.BLOCK.getKey(block))
                        .append(' ').append(owner).append('\n');
            }
            blockCount++;
        }

        BlockGetter level = EmptyBlockGetter.INSTANCE;
        BlockPos pos = BlockPos.ZERO;

        StringBuilder blocks = new StringBuilder();
        List<Boolean> shadeOccludes = new ArrayList<>();
        List<Boolean> fullCollisionCube = new ArrayList<>();
        Map<Integer, Integer> histogram = new LinkedHashMap<>();
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
            float shade = state.getShadeBrightness(level, pos);
            histogram.merge(Float.floatToRawIntBits(shade), 1, Integer::sum);
            shadeOccludes.add(shade == OCCLUDED);
            fullCollisionCube.add(state.isCollisionShapeFullBlock(level, pos));
            count++;
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# ShadeBrightnessOracle dump from the real 26.2 server (protocol 776):\n");
        sb.append("# per-block-state BlockStateBase.getShadeBrightness (S, reduced to\n");
        sb.append("# `== 0.2F`) and BlockStateBase.isCollisionShapeFullBlock (F, the base\n");
        sb.append("# formula alone), plus the getShadeBrightness override census (O) and the\n");
        sb.append("# histogram of distinct return values (V) that proves S is lossless.\n");
        sb.append("# C <stateCount> <blockCount>\n");
        sb.append("# O <blockName> <declaringClassOfGetShadeBrightnessOverride>\n");
        sb.append("# V <floatBitsHex> <stateCount with that value>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# P <S|F> <startStateId> <bitstring up to 256 chars, ascending>\n");
        sb.append("C ").append(count).append(' ').append(blockCount).append('\n');
        sb.append(overrides);
        for (Map.Entry<Integer, Integer> e : histogram.entrySet()) {
            sb.append("V ").append(Integer.toHexString(e.getKey()))
                    .append(' ').append(e.getValue()).append('\n');
        }
        sb.append(blocks);
        emitBits(sb, 'S', shadeOccludes);
        emitBits(sb, 'F', fullCollisionCube);

        System.out.print(sb);
    }

    /**
     * The most-derived class that declares
     * {@code getShadeBrightness(BlockState, BlockGetter, BlockPos)}, or
     * {@code null} when nothing between {@code blockClass} and
     * {@code BlockBehaviour}'s base declaration overrides it. Mirrors
     * {@code BlockPhysicsOracle.entityInsideOwner}.
     */
    private static String shadeBrightnessOwner(final Class<?> blockClass) {
        for (Class<?> c = blockClass; c != null && c != BlockBehaviour.class; c = c.getSuperclass()) {
            for (Method m : c.getDeclaredMethods()) {
                if (!m.getName().equals("getShadeBrightness")) {
                    continue;
                }
                Class<?>[] p = m.getParameterTypes();
                if (p.length == 3
                        && p[0] == BlockState.class
                        && p[1] == BlockGetter.class
                        && p[2] == BlockPos.class) {
                    return c.getName();
                }
            }
        }
        return null;
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
