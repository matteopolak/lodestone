import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.SoundType;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Authoritative extractor for the per-block-state <b>{@link SoundType}</b>:
 * {@code BlockStateBase.getSoundType()} ({@code BlockBehaviour.java:877-879}),
 * which is what {@code LevelEventHandler}'s {@code case 2001} reads to play a
 * block-break sound ({@code LevelEventHandler.java:283-291}) and what
 * {@code BlockItem.place} reads for a placement sound
 * ({@code BlockItem.java:87}).
 *
 * <p>Boots the real 26.2 server (registries only, no world) exactly like
 * {@code ShadeBrightnessOracle} and {@code HardnessOracle} in this directory. No
 * data pack and no {@code BlockGetter} are needed: unlike
 * {@code getShadeBrightness}, {@code getSoundType} takes only the state, reads no
 * tag, and touches no level.
 *
 * <p><b>Why this cannot be derived from any committed table.</b> A
 * {@code SoundType} is seven values — {@code volume}, {@code pitch} and five
 * {@code SoundEvent}s (break / step / place / hit / fall) — assigned per block in
 * {@code BlockBehaviour.Properties.sound(..)}, i.e. in code, with no
 * representation in {@code blocks.json} at all. The mapping is also not
 * one-static-per-block: {@code HARD_CROP} reuses {@code WOOD}'s break, step, hit
 * and fall sounds with {@code CROP_PLANTED} for placement, and
 * {@code GLOW_LICHEN} reuses {@code GRASS}'s four with {@code VINE_STEP}. Any
 * hand-written "block family -> sound" list would have to reproduce those
 * exactly, which is the hand-counted-census mistake this repo has shipped twice.
 *
 * <p><b>Why it is state-keyed rather than block-keyed.</b> {@code getSoundType}
 * is per-block for every block but one: {@code DecoratedPotBlock} overrides it
 * ({@code DECORATED_POT_CRACKED} swaps the break sound for
 * {@code DECORATED_POT_SHATTER} when {@code CRACKED} is set). The {@code O}
 * census below finds that mechanically instead of asserting it.
 *
 * <p>Output:
 *
 * <pre>
 *   C &lt;stateCount&gt; &lt;blockCount&gt; &lt;distinctValueCount&gt; &lt;distinctIdentityCount&gt;
 *   N &lt;soundEventRegistryId&gt; &lt;soundEventName&gt;
 *   T &lt;index&gt; &lt;volumeBitsHex&gt; &lt;pitchBitsHex&gt; &lt;break&gt; &lt;step&gt; &lt;place&gt; &lt;hit&gt; &lt;fall&gt;
 *   O &lt;blockName&gt; &lt;declaringClassOfGetSoundTypeOverride&gt;
 *   B &lt;firstStateIdOfBlock&gt; &lt;blockName&gt;
 *   R &lt;soundTypeIndex&gt; &lt;runLength&gt;
 * </pre>
 *
 * {@code T} rows are the <b>deduplicated-by-value</b> sound-type table, indexed
 * by order of first appearance in ascending state-id order; the five sound
 * columns are {@code BuiltInRegistries.SOUND_EVENT} ids, which is the same id
 * space the committed {@code sound_events} table is indexed by, so a consumer can
 * cross-check two independently generated tables against each other. {@code N}
 * rows name every sound event any {@code T} row references (and only those), so
 * the names never have to be typed by hand. {@code R} rows are a run-length
 * encoding of the per-state index in ascending id order; the run lengths sum to
 * {@code stateCount}.
 *
 * <p>{@code distinctIdentityCount} counts distinct {@code SoundType} <i>objects</i>
 * and {@code distinctValueCount} counts distinct seven-tuples. Emitting both lets
 * a consumer measure whether value-dedup collapses anything (two vanilla statics
 * with identical fields) rather than assuming it does not.
 */
public final class SoundTypeOracle {
    public static void main(final String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder overrides = new StringBuilder();
        int blockCount = 0;
        for (Block block : BuiltInRegistries.BLOCK) {
            String owner = soundTypeOwner(block.getClass());
            if (owner != null) {
                overrides.append("O ")
                        .append(BuiltInRegistries.BLOCK.getKey(block))
                        .append(' ').append(owner).append('\n');
            }
            blockCount++;
        }

        // Value-keyed dedup table, indexed by order of first appearance.
        Map<String, Integer> valueToIndex = new LinkedHashMap<>();
        List<String> table = new ArrayList<>();
        // Identity-keyed count, purely so the value dedup is measurable.
        Map<SoundType, Boolean> identities = new java.util.IdentityHashMap<>();
        // Every sound event id any table row references, in ascending id order.
        Map<Integer, String> referenced = new java.util.TreeMap<>();

        StringBuilder blocks = new StringBuilder();
        StringBuilder runs = new StringBuilder();
        Block previousBlock = null;
        int runIndex = -1;
        int runLength = 0;
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

            SoundType sound = state.getSoundType();
            identities.put(sound, Boolean.TRUE);
            int[] events = {
                soundId(sound.getBreakSound(), referenced),
                soundId(sound.getStepSound(), referenced),
                soundId(sound.getPlaceSound(), referenced),
                soundId(sound.getHitSound(), referenced),
                soundId(sound.getFallSound(), referenced),
            };
            StringBuilder key = new StringBuilder();
            key.append(Integer.toHexString(Float.floatToRawIntBits(sound.getVolume())))
                    .append(' ')
                    .append(Integer.toHexString(Float.floatToRawIntBits(sound.getPitch())));
            for (int event : events) {
                key.append(' ').append(event);
            }
            String row = key.toString();
            Integer existing = valueToIndex.get(row);
            int index;
            if (existing == null) {
                index = table.size();
                valueToIndex.put(row, index);
                table.add(row);
            } else {
                index = existing;
            }

            if (index != runIndex) {
                if (runLength > 0) {
                    runs.append("R ").append(runIndex).append(' ').append(runLength).append('\n');
                }
                runIndex = index;
                runLength = 0;
            }
            runLength++;
            count++;
        }
        if (runLength > 0) {
            runs.append("R ").append(runIndex).append(' ').append(runLength).append('\n');
        }

        StringBuilder sb = new StringBuilder();
        sb.append("# SoundTypeOracle dump from the real 26.2 server (protocol 776):\n");
        sb.append("# per-block-state BlockStateBase.getSoundType(), deduplicated by value into\n");
        sb.append("# a T table of (volume, pitch, break, step, place, hit, fall) with the five\n");
        sb.append("# sound columns as BuiltInRegistries.SOUND_EVENT ids, plus the getSoundType\n");
        sb.append("# override census (O) and a run-length encoding (R) of the per-state index.\n");
        sb.append("# C <stateCount> <blockCount> <distinctValueCount> <distinctIdentityCount>\n");
        sb.append("# N <soundEventRegistryId> <soundEventName>\n");
        sb.append("# T <index> <volumeBitsHex> <pitchBitsHex> <break> <step> <place> <hit> <fall>\n");
        sb.append("# O <blockName> <declaringClassOfGetSoundTypeOverride>\n");
        sb.append("# B <firstStateIdOfBlock> <blockName>\n");
        sb.append("# R <soundTypeIndex> <runLength>\n");
        sb.append("C ").append(count).append(' ').append(blockCount).append(' ')
                .append(table.size()).append(' ').append(identities.size()).append('\n');
        sb.append(overrides);
        for (Map.Entry<Integer, String> e : referenced.entrySet()) {
            sb.append("N ").append(e.getKey()).append(' ').append(e.getValue()).append('\n');
        }
        for (int i = 0; i < table.size(); i++) {
            sb.append("T ").append(i).append(' ').append(table.get(i)).append('\n');
        }
        sb.append(blocks);
        sb.append(runs);

        System.out.print(sb);
    }

    /**
     * Registry id of {@code event}, recording its name in {@code referenced}.
     * Throws rather than emitting a sentinel: an unregistered {@code SoundEvent}
     * reachable from a {@code SoundType} would mean the id column cannot address
     * the committed sound-event table, and a {@code -1} in the dump would look
     * like data.
     */
    private static int soundId(final SoundEvent event, final Map<Integer, String> referenced) {
        int id = BuiltInRegistries.SOUND_EVENT.getId(event);
        if (id < 0) {
            throw new IllegalStateException(
                    "SoundEvent " + event.location() + " is not in BuiltInRegistries.SOUND_EVENT");
        }
        referenced.put(id, BuiltInRegistries.SOUND_EVENT.getKey(event).toString());
        return id;
    }

    /**
     * The most-derived class that declares {@code getSoundType(BlockState)}, or
     * {@code null} when nothing between {@code blockClass} and
     * {@code BlockBehaviour}'s base declaration overrides it. Mirrors
     * {@code ShadeBrightnessOracle.shadeBrightnessOwner}.
     */
    private static String soundTypeOwner(final Class<?> blockClass) {
        for (Class<?> c = blockClass; c != null && c != BlockBehaviour.class; c = c.getSuperclass()) {
            for (Method m : c.getDeclaredMethods()) {
                if (!m.getName().equals("getSoundType")) {
                    continue;
                }
                Class<?>[] p = m.getParameterTypes();
                if (p.length == 1 && p[0] == BlockState.class) {
                    return c.getName();
                }
            }
        }
        return null;
    }
}
