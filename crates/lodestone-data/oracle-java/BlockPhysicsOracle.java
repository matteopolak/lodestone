import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackResources;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.BlockTags;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.InsideBlockEffectApplier;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Authoritative extractor for the block facts that are <b>not geometry</b>:
 * the five per-block movement constants a player's physics reads off
 * {@code BlockBehaviour.Properties}, and the per-block-state {@code legacySolid}
 * flag that {@code blocksMotion()} is built from. Boots the real 26.2 server
 * (registries only, no world) exactly like {@code HardnessOracle} and
 * {@code OutlineShapeOracle} in this directory.
 *
 * <p><b>Why two id spaces in one dump.</b> The two halves are keyed differently
 * and that difference is load-bearing:
 *
 * <ul>
 *   <li>{@code friction}/{@code speedFactor}/{@code jumpFactor}/
 *       {@code bounceRestitution} are {@code final} fields on {@code BlockBehaviour}
 *       ({@code BlockBehaviour.java:93-96}) copied once from
 *       {@code Properties} at construction ({@code BlockBehaviour.java:110-113}),
 *       and their accessors on {@code Block} ({@code Block.java:493-507}) take no
 *       state at all. So they are <b>per block</b>, not per block state. Likewise
 *       {@code BlockTags.CLIMBABLE} ({@code BlockTags.java:102}) is a block tag.</li>
 *   <li>{@code legacySolid} is computed <b>per state</b> in
 *       {@code initCache()} ({@code BlockBehaviour.java:513}) from
 *       {@code calculateSolid()} ({@code BlockBehaviour.java:484-504}), whose
 *       geometry branch reads that state's own {@code cache.collisionShape}. An
 *       open vs. closed trapdoor genuinely differ. So this half must be keyed by
 *       global block-state id, like the collision census.</li>
 * </ul>
 *
 * <p><b>{@code calculateSolid} is not derivable from a committed shape table.</b>
 * Its first two branches are {@code Properties.forceSolidOn} /
 * {@code forceSolidOff} ({@code BlockBehaviour.java:485-491}), set by
 * {@code Properties.forceSolidOn()}/{@code forceSolidOff()}
 * ({@code BlockBehaviour.java:1175-1184}). Neither flag is exposed by any getter,
 * appears in {@code blocks.json}, or is recoverable from the shape — which is the
 * whole reason this dump exists. Only when both are unset does it fall through to
 * {@code bounds.getSize() >= 0.7291666666666666 || bounds.getYsize() >= 1.0},
 * where {@code AABB.getSize()} is the mean of the three extents
 * ({@code AABB.java:267-272}).
 *
 * <p><b>What is deliberately <i>not</i> dumped: the stuck multiplier.</b>
 * {@code makeStuckInBlock}'s per-block vector is not a property — it is an
 * argument constructed inside {@code Block.entityInside} by the three blocks that
 * grab an entity ({@code WebBlock.java:30-35}, {@code PowderSnowBlock.java:66},
 * {@code SweetBerryBushBlock.java:86}), and two of the three are conditional on
 * the entity. There is nothing to read. What <i>can</i> be established
 * mechanically, and is emitted below as {@code E}, is the authoritative
 * <b>candidate set</b>: every block whose class overrides {@code entityInside}
 * at all ({@code BlockBehaviour.java:377-380} declares the empty base). A
 * consumer's hand-transcribed three-row table is then checkable for
 * <i>completeness</i> — no block outside {@code E} can possibly set a stuck
 * multiplier — rather than merely asserted to be complete.
 *
 * <p>Tags are datapack content, so {@code Bootstrap.bootStrap()} leaves every tag
 * set empty and {@code state.is(BlockTags.CLIMBABLE)} would answer {@code false}
 * for all 1000+ blocks — a silent, uniform wrong answer. The vanilla data pack is
 * loaded and bound first, the same way {@code PathTypeOracle} does it.
 *
 * <p>Output, all floats as raw hex {@code Float.floatToRawIntBits} so the text
 * round-trip is lossless:
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt;
 *   K &lt;blockName&gt; &lt;frictionBits&gt; &lt;speedFactorBits&gt; &lt;jumpFactorBits&gt;
 *     &lt;bounceRestitutionBits&gt; &lt;climbable&gt; &lt;suppressesBounce&gt;
 *   F &lt;blockName&gt; &lt;forceSolidOn&gt; &lt;forceSolidOff&gt; &lt;dynamicShape&gt;
 *   E &lt;blockName&gt; &lt;declaringClassOfEntityInside&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   P &lt;L|M|G&gt; &lt;startStateId&gt; &lt;bitstring, up to 256 chars, ascending&gt;
 * </pre>
 *
 * {@code L} is {@code BlockStateBase.isSolid()} (the raw {@code legacySolid}
 * flag, {@code BlockBehaviour.java:547-550}); {@code M} is
 * {@code BlockStateBase.blocksMotion()} ({@code BlockBehaviour.java:541-545}),
 * i.e. {@code legacySolid} already net of the two hard-coded block exclusions.
 * Both are emitted because they are different questions and a consumer that
 * derives one from the other should be able to check that it did so correctly.
 *
 * <p>{@code G} is the <b>geometry branch alone</b>:
 * {@code calculateSolid()} with the two {@code forceSolid*} early-returns
 * deleted, evaluated against the same
 * {@code getCollisionShape(state, EmptyBlockGetter.INSTANCE, BlockPos.ZERO, CollisionContext.empty())}
 * that {@code Cache} caches ({@code BlockBehaviour.java:925}) and that this
 * repo's collision census dumps. It exists so a consumer that <i>derives</i>
 * solidity from a committed shape table can measure exactly how far that
 * derivation is from the truth — {@code L != G} is the authoritative
 * override count, computed in the JVM rather than inferred.
 *
 * <p>{@code F} carries the two {@code Properties} flags themselves, read by
 * reflection from {@code BlockBehaviour.properties}
 * ({@code BlockBehaviour.java:99}) because neither has a getter, plus
 * {@code hasDynamicShape}. The dynamic-shape flag matters for {@code G}: when
 * it is set, {@code initCache} leaves {@code cache == null}
 * ({@code BlockBehaviour.java:509-511}) and {@code calculateSolid}'s third
 * branch returns {@code false} regardless of geometry
 * ({@code BlockBehaviour.java:493-495}), which is a case no shape table can
 * express.
 */
public final class BlockPhysicsOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // Bind the vanilla data pack's tags: without this every
        // BlockTags.CLIMBABLE / SUPPRESSES_BOUNCE membership test answers false.
        // Same preamble as PathTypeOracle, and the same reason.
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

        // Fail loudly rather than dumping a table of `false`s: an empty tag is
        // indistinguishable from "no block is climbable" downstream.
        long climbableCount = BuiltInRegistries.BLOCK.stream()
                .filter(block -> block.defaultBlockState().is(BlockTags.CLIMBABLE))
                .count();
        if (climbableCount == 0) {
            throw new IllegalStateException(
                    "BlockTags.CLIMBABLE is empty: tag binding failed, refusing to dump");
        }

        Field propertiesField = BlockBehaviour.class.getDeclaredField("properties");
        propertiesField.setAccessible(true);
        Field forceSolidOnField =
                BlockBehaviour.Properties.class.getDeclaredField("forceSolidOn");
        forceSolidOnField.setAccessible(true);
        Field forceSolidOffField =
                BlockBehaviour.Properties.class.getDeclaredField("forceSolidOff");
        forceSolidOffField.setAccessible(true);

        StringBuilder constants = new StringBuilder();
        StringBuilder forceFlags = new StringBuilder();
        StringBuilder entityInside = new StringBuilder();
        int blockCount = 0;
        for (Block block : BuiltInRegistries.BLOCK) {
            String name = BuiltInRegistries.BLOCK.getKey(block).toString();
            BlockState def = block.defaultBlockState();
            Object props = propertiesField.get(block);
            forceFlags.append("F ").append(name)
                    .append(' ').append(forceSolidOnField.getBoolean(props) ? 1 : 0)
                    .append(' ').append(forceSolidOffField.getBoolean(props) ? 1 : 0)
                    .append(' ').append(block.hasDynamicShape() ? 1 : 0)
                    .append('\n');
            constants.append("K ").append(name)
                    .append(' ').append(hex(block.getFriction()))
                    .append(' ').append(hex(block.getSpeedFactor()))
                    .append(' ').append(hex(block.getJumpFactor()))
                    .append(' ').append(hex(block.getBounceRestitution()))
                    .append(' ').append(def.is(BlockTags.CLIMBABLE) ? 1 : 0)
                    .append(' ').append(def.is(BlockTags.SUPPRESSES_BOUNCE) ? 1 : 0)
                    .append('\n');
            String owner = entityInsideOwner(block.getClass());
            if (owner != null) {
                entityInside.append("E ").append(name).append(' ').append(owner).append('\n');
            }
            blockCount++;
        }

        StringBuilder blocks = new StringBuilder();
        List<Boolean> legacySolid = new ArrayList<>();
        List<Boolean> blocksMotion = new ArrayList<>();
        List<Boolean> geometrySolid = new ArrayList<>();
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
            legacySolid.add(state.isSolid());
            blocksMotion.add(state.blocksMotion());
            geometrySolid.add(geometryBranch(state));
            count++;
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# BlockPhysicsOracle dump from the real 26.2 server (protocol 776):\n");
        sb.append("# per-block movement constants + BlockTags.CLIMBABLE/SUPPRESSES_BOUNCE (K),\n");
        sb.append("# the entityInside override census (E), and per-block-state\n");
        sb.append("# BlockStateBase.isSolid() (L, the raw legacySolid flag) and\n");
        sb.append("# BlockStateBase.blocksMotion() (M).\n");
        sb.append("# C <stateCount> <blockCount>\n");
        sb.append("# K <blockName> <frictionBits> <speedFactorBits> <jumpFactorBits> "
                + "<bounceRestitutionBits> <climbable> <suppressesBounce>  (floats: raw hex bits)\n");
        sb.append("# F <blockName> <forceSolidOn> <forceSolidOff> <dynamicShape>\n");
        sb.append("# E <blockName> <declaringClassOfEntityInside>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# P <L|M|G> <startStateId> <bitstring up to 256 chars, ascending>\n");
        sb.append("C ").append(count).append(' ').append(blockCount).append('\n');
        sb.append(constants);
        sb.append(forceFlags);
        sb.append(entityInside);
        sb.append(blocks);
        emitBits(sb, 'L', legacySolid);
        emitBits(sb, 'M', blocksMotion);
        emitBits(sb, 'G', geometrySolid);

        System.out.print(sb);
    }

    /**
     * {@code calculateSolid()} ({@code BlockBehaviour.java:484-504}) with the two
     * {@code forceSolid*} early-returns removed — i.e. everything a consumer
     * holding only a collision-shape table could possibly compute.
     *
     * <p>The {@code cache == null} branch is reproduced via
     * {@code hasDynamicShape}: {@code initCache} only builds a {@code Cache} when
     * the block has no dynamic shape ({@code BlockBehaviour.java:509-511}).
     */
    private static boolean geometryBranch(final BlockState state) {
        if (state.getBlock().hasDynamicShape()) {
            return false;
        }
        // The two-argument BlockStateBase form returns `cache.collisionShape`
        // verbatim when a cache exists (BlockBehaviour.java:680-682), which is
        // precisely the field `calculateSolid` reads.
        VoxelShape shape = state.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
        if (shape.isEmpty()) {
            return false;
        }
        AABB bounds = shape.bounds();
        return bounds.getSize() >= 0.7291666666666666 || bounds.getYsize() >= 1.0;
    }

    private static String hex(final float value) {
        return Integer.toHexString(Float.floatToRawIntBits(value));
    }

    /**
     * The most-derived class that declares {@code entityInside}, or {@code null}
     * when nothing between {@code blockClass} and {@code BlockBehaviour}'s empty
     * base declaration overrides it.
     */
    private static String entityInsideOwner(final Class<?> blockClass) {
        for (Class<?> c = blockClass; c != null && c != BlockBehaviour.class; c = c.getSuperclass()) {
            for (Method m : c.getDeclaredMethods()) {
                if (!m.getName().equals("entityInside")) {
                    continue;
                }
                Class<?>[] p = m.getParameterTypes();
                if (p.length == 6
                        && p[0] == BlockState.class
                        && p[1] == Level.class
                        && p[2] == BlockPos.class
                        && p[3] == Entity.class
                        && p[4] == InsideBlockEffectApplier.class
                        && p[5] == boolean.class) {
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
