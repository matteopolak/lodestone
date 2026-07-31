import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Authoritative per-block-state <b>outline</b> and <b>interaction</b> shape
 * extractor. Boots the real 26.2 server (registries only, no world) and asks
 * every registered {@code BlockState} for its own
 * {@code BlockStateBase.getShape} and {@code BlockStateBase.getInteractionShape}
 * — mirrors {@code HardnessOracle} in this directory and
 * {@code lodestone-physics}'s {@code ShapeOracle}, which dumps the third,
 * genuinely different shape (collision).
 *
 * <p><b>Why this is not the collision census.</b> Three shape families diverge:
 *
 * <ul>
 *   <li>{@code getShape} (the <i>outline</i>) defaults to {@code Shapes.block()}
 *       ({@code BlockBehaviour.java:323-325}).</li>
 *   <li>{@code getCollisionShape} defaults to
 *       {@code this.hasCollision ? state.getShape(...) : Shapes.empty()}
 *       ({@code BlockBehaviour.java:327-329}) — so every {@code noCollission()}
 *       block (kelp, seagrass, torches, plants) has an <b>empty collision shape
 *       and a non-empty outline</b>.</li>
 *   <li>{@code getInteractionShape} defaults to {@code Shapes.empty()}
 *       ({@code BlockBehaviour.java:295-297}) and is an <i>additive</i> extra
 *       clip target for a handful of blocks, not a replacement.</li>
 * </ul>
 *
 * <p>The outline shape is what block selection uses:
 * {@code Entity.pick} clips with {@code ClipContext.Block.OUTLINE} and
 * {@code ClipContext.Fluid.NONE} ({@code Entity.java:2012-2017}), and
 * {@code ClipContext.Block.OUTLINE} is {@code BlockStateBase::getShape}
 * ({@code ClipContext.java:57}). Hence {@code LiquidBlock.getShape} being
 * {@code Shapes.empty()} ({@code LiquidBlock.java:145-147}) is what makes open
 * water untargetable, while {@code KelpBlock}'s {@code Block.column(16, 0, 9)}
 * ({@code KelpBlock.java:24}) and {@code SeagrassBlock}'s
 * {@code Block.column(12, 0, 12)} ({@code SeagrassBlock.java:29}) are what make
 * those targetable despite having no collision.
 *
 * <p><b>Level/context caveat, identical to the collision census.</b> Both
 * getters take a {@code BlockGetter}/{@code BlockPos} (and {@code getShape} a
 * {@code CollisionContext}); this passes {@code EmptyBlockGetter.INSTANCE},
 * {@code BlockPos.ZERO} and the implicit {@code CollisionContext.empty()}
 * (via the two-argument {@code getShape}, {@code BlockBehaviour.java:673-675}).
 * Vanilla's own shape cache does exactly this
 * ({@code getOcclusionShape} → {@code state.getShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)},
 * {@code BlockBehaviour.java:287-289}), so it is the game's own standing
 * assumption rather than ours. The handful of shapes that genuinely vary with
 * the entity ({@code ScaffoldingBlock}'s descending shape) or with a neighbour
 * resolve to their default/standing form here.
 *
 * <p>Output is deliberately <i>de-duplicated in the dumper</i> so the dump is
 * ~200 KB and can be committed as the external anchor rather than sitting in a
 * gitignored scratch file. De-duplication is by exact
 * {@code Double.doubleToRawLongBits} list identity, computed in the JVM, so it
 * introduces no encoding of its own:
 *
 * <pre>
 *   C &lt;stateCount&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   S &lt;O|X&gt; &lt;shapeIndex&gt; &lt;boxCount&gt; [minX minY minZ maxX maxY maxZ]...   (raw double bits, hex)
 *   P &lt;O|X&gt; &lt;startStateId&gt; &lt;shapeIndex&gt;...                              (256 per line, ascending)
 * </pre>
 *
 * {@code O} is the outline family, {@code X} the interaction family; each has
 * its own independent shape-index space. {@code B} lines let a consumer
 * cross-check the whole state→block mapping against a table built from a
 * different artifact ({@code blocks.json}) without carrying 32,366 names.
 */
public final class OutlineShapeOracle {
    /** Indices into a per-family distinct-shape list, one per block state. */
    private static final class Family {
        final Map<String, Integer> index = new LinkedHashMap<>();
        final List<String> distinct = new ArrayList<>();
        final List<Integer> perState = new ArrayList<>();

        void add(final VoxelShape shape) {
            String key = encode(shape);
            Integer existing = this.index.get(key);
            if (existing == null) {
                existing = this.distinct.size();
                this.index.put(key, existing);
                this.distinct.add(key);
            }
            this.perState.add(existing);
        }
    }

    /**
     * Encodes a shape as "boxCount coord coord ..." with every coordinate as the
     * raw hex bit pattern of its {@code double}, so nothing is lost in the text
     * round-trip and the string doubles as an exact de-duplication key.
     */
    private static String encode(final VoxelShape shape) {
        List<AABB> boxes = shape.toAabbs();
        StringBuilder sb = new StringBuilder();
        sb.append(boxes.size());
        for (AABB b : boxes) {
            sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.minX)));
            sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.minY)));
            sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.minZ)));
            sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.maxX)));
            sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.maxY)));
            sb.append(' ').append(Long.toHexString(Double.doubleToRawLongBits(b.maxZ)));
        }
        return sb.toString();
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Family outline = new Family();
        Family interaction = new Family();
        StringBuilder blocks = new StringBuilder();

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
            outline.add(state.getShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO));
            interaction.add(state.getInteractionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO));
            count++;
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# OutlineShapeOracle dump from the real 26.2 server (protocol 776):\n");
        sb.append("# BlockStateBase.getShape (O, the pick/outline shape) and\n");
        sb.append("# BlockStateBase.getInteractionShape (X), per block state.\n");
        sb.append("# C <stateCount>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# S <O|X> <shapeIndex> <boxCount> [minX minY minZ maxX maxY maxZ]... "
                + "(raw double bits, hex)\n");
        sb.append("# P <O|X> <startStateId> <shapeIndex>... (256 per line, ascending)\n");
        sb.append("C ").append(count).append('\n');
        sb.append(blocks);
        emitShapes(sb, 'O', outline);
        emitShapes(sb, 'X', interaction);
        emitPerState(sb, 'O', outline);
        emitPerState(sb, 'X', interaction);

        System.out.print(sb);
    }

    private static void emitShapes(final StringBuilder sb, final char kind, final Family family) {
        for (int i = 0; i < family.distinct.size(); i++) {
            sb.append("S ").append(kind).append(' ').append(i).append(' ')
                    .append(family.distinct.get(i)).append('\n');
        }
    }

    private static void emitPerState(final StringBuilder sb, final char kind, final Family family) {
        for (int start = 0; start < family.perState.size(); start += 256) {
            sb.append("P ").append(kind).append(' ').append(start);
            int end = Math.min(start + 256, family.perState.size());
            for (int i = start; i < end; i++) {
                sb.append(' ').append(family.perState.get(i));
            }
            sb.append('\n');
        }
    }
}
