import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.Item;
import net.minecraft.world.level.block.Block;

/**
 * Authoritative item -> placed-block extractor. Boots the real 26.2 server
 * (registries only, no world) and walks {@code BuiltInRegistries.ITEM},
 * asking every {@code BlockItem} for its own {@code getBlock()} — mirrors
 * {@code HardnessOracle} and {@code ItemPrototypeOracle} in this same
 * directory.
 *
 * <h2>Why the jar and not a name match</h2>
 *
 * "An item places the block of the same name" is close but demonstrably
 * wrong in both directions, and the jar is the only thing that says so:
 *
 * <ul>
 *   <li><b>False negatives</b> — {@code Items.REDSTONE} is
 *       {@code createBlockItemWithCustomItemName(Blocks.REDSTONE_WIRE)}
 *       ({@code Items.java:753-755}), so the item is {@code minecraft:redstone}
 *       and the block is {@code minecraft:redstone_wire}. Same shape for
 *       {@code string}/{@code tripwire} ({@code Items.java:1044}),
 *       {@code wheat_seeds}/{@code wheat} ({@code :1047}),
 *       {@code cocoa_beans}/{@code cocoa} ({@code :1321}),
 *       {@code carrot}/{@code carrots} ({@code :1499}),
 *       {@code potato}/{@code potatoes} ({@code :1502}),
 *       {@code pumpkin_seeds}/{@code pumpkin_stem} ({@code :1346}),
 *       {@code melon_seeds}/{@code melon_stem} ({@code :1347}).</li>
 *   <li><b>A false positive</b> — {@code Items.WHEAT} is a plain
 *       {@code registerItem} ({@code Items.java:1048}) with no block at all,
 *       yet {@code minecraft:wheat} <em>is</em> a registered block (the crop).
 *       A name match would place a crop when the player holds wheat.</li>
 * </ul>
 *
 * <h2>Scope: {@code BlockItem} only</h2>
 *
 * Items that place something without being a {@code BlockItem} — buckets
 * ({@code BucketItem} placing a fluid), spawn eggs, {@code flint_and_steel}
 * lighting a fire, item frames and minecarts spawning entities — are
 * deliberately reported as non-block here. They are not "an item that places
 * its block", and each needs its own mechanism; conflating them into this
 * table would be a hand-written guess wearing generated clothes.
 *
 * Emits one line per item, ascending item registry id:
 *   {@code <itemRegistryId> <itemName> <blockName-or-dash>}
 *
 * Every item is emitted, including non-block ones (as {@code -}), so the dump
 * is positive evidence that an item was considered and found non-placeable,
 * rather than an absence that could equally mean the oracle skipped it.
 */
public final class BlockItemOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        StringBuilder sb = new StringBuilder();
        for (Item item : BuiltInRegistries.ITEM) {
            int id = BuiltInRegistries.ITEM.getId(item);
            String itemName = BuiltInRegistries.ITEM.getKey(item).toString();
            String blockName = "-";
            if (item instanceof BlockItem blockItem) {
                Block block = blockItem.getBlock();
                blockName = BuiltInRegistries.BLOCK.getKey(block).toString();
            }
            sb.append(id).append(' ').append(itemName).append(' ').append(blockName).append('\n');
        }
        System.out.print(sb);
    }
}
