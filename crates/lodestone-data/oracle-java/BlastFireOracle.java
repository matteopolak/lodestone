import java.lang.reflect.Field;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.FireBlock;

/**
 * Authoritative per-block-<em>type</em> blast-resistance and flammability
 * extractor for the fire-spread and explosion-destruction rules. Boots the real 26.2 server (registries only, no world)
 * and reads four quantities straight off the game's own objects, mirroring
 * {@code HardnessOracle} in this same directory.
 *
 * <p><b>Why per-block-type and not per-block-state.</b> All four quantities are
 * fields on {@code Block}/{@code BlockBehaviour}, not on
 * {@code BlockBehaviour.BlockStateBase}, so a per-state table would be 32,366
 * rows of which at most 1,196 are distinct:
 *
 * <ul>
 *   <li>{@code Block.getExplosionResistance()} ({@code Block.java}) returns the
 *       {@code explosionResistance} field, set once from
 *       {@code Properties.explosionResistance} at registration. Nothing
 *       overrides it.</li>
 *   <li>{@code FireBlock}'s {@code igniteOdds}/{@code burnOdds} are
 *       {@code Object2IntMap<Block>}s populated by {@code FireBlock.bootStrap()}
 *       (which {@code Bootstrap.bootStrap()} above calls, so that boot is
 *       sufficient — the maps are empty before it runs). They are
 *       private, hence the reflection below; both implement
 *       {@code java.util.Map<Block, Integer>}, so no fastutil type is named
 *       here.</li>
 *   <li>{@code BlockBehaviour.BlockStateBase.ignitedByLava()}
 *       reads a boolean copied from {@code Properties.ignitedByLava} —
 *       identical for every state of a block, so it is read off the default
 *       state.</li>
 * </ul>
 *
 * <p>The two <em>state</em>-level rules that do exist are deliberately left to
 * the consumer rather than baked in here, because they are cheap string checks
 * on our side and baking them in would need the 32,366-row shape this avoids:
 * {@code FireBlock.getBurnOdds}/{@code getIgniteOdds} both return {@code 0} for
 * a state with {@code waterlogged=true}, and {@code ExplosionDamageCalculator.getBlockExplosionResistance}
 * takes {@code max(block resistance, fluid resistance)} so a waterlogged or
 * fluid-filled cell resists at the fluid's 100.0.
 *
 * <p>Emits one line per block, ascending {@code BuiltInRegistries.BLOCK}
 * registry id:
 * <pre>{@code <registryId> <blockName> <explosionResistanceBits> <igniteOdds> <burnOdds> <ignitedByLava>}</pre>
 *
 * {@code explosionResistanceBits} is the raw hex
 * {@code Float.floatToRawIntBits} pattern, so nothing is lost in the text round
 * trip — indestructible blocks carry vanilla's own {@code 3600000.0F} exactly.
 */
public final class BlastFireOracle {
    @SuppressWarnings("unchecked")
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        FireBlock fire = (FireBlock) Blocks.FIRE;
        Field igniteField = FireBlock.class.getDeclaredField("igniteOdds");
        Field burnField = FireBlock.class.getDeclaredField("burnOdds");
        igniteField.setAccessible(true);
        burnField.setAccessible(true);
        Map<Block, Integer> igniteOdds = (Map<Block, Integer>) igniteField.get(fire);
        Map<Block, Integer> burnOdds = (Map<Block, Integer>) burnField.get(fire);

        StringBuilder sb = new StringBuilder();
        for (Block block : BuiltInRegistries.BLOCK) {
            int id = BuiltInRegistries.BLOCK.getId(block);
            String name = BuiltInRegistries.BLOCK.getKey(block).toString();
            int bits = Float.floatToRawIntBits(block.getExplosionResistance());
            int ignite = igniteOdds.getOrDefault(block, 0);
            int burn = burnOdds.getOrDefault(block, 0);
            boolean lava = block.defaultBlockState().ignitedByLava();
            sb.append(id).append(' ').append(name).append(' ')
                    .append(Integer.toHexString(bits)).append(' ')
                    .append(ignite).append(' ')
                    .append(burn).append(' ')
                    .append(lava ? 1 : 0).append('\n');
        }
        System.out.print(sb);
    }
}
