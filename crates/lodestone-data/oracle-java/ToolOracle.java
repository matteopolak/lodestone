import java.util.ArrayList;
import java.util.List;
import java.util.Optional;

import com.mojang.datafixers.util.Either;

import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderSet;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.component.DataComponentInitializers;
import net.minecraft.core.component.DataComponents;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackResources;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagKey;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.component.Tool;
import net.minecraft.world.level.block.Block;

/**
 * Authoritative extractor for everything {@code Tool.getMiningSpeed} /
 * {@code Tool.isCorrectForDrops} need, dumped from the real 26.2 server —
 * mirrors {@code HardnessOracle} and {@code PathTypeOracle} in this directory.
 *
 * Three things have to come out of the game rather than out of memory:
 *
 * <ol>
 *   <li><b>The block registry order.</b> A {@code minecraft:tool} rule can name
 *       its blocks as an explicit {@code HolderSet.direct}, which the wire
 *       encodes as {@code minecraft:block} <i>registry</i> ids. That order is
 *       registration order ({@code air} is 0), not the alphabetical order
 *       {@code blocks.json} happens to be sorted in, so the two id spaces must
 *       be reconciled explicitly.</li>
 *   <li><b>Block tag membership.</b> The other way a rule names blocks is a tag
 *       ({@code #minecraft:mineable/pickaxe}). Tags are datapack content, so
 *       {@code Bootstrap.bootStrap()} alone leaves every one of them empty —
 *       the vanilla data pack has to be loaded and bound first, exactly as
 *       {@code PathTypeOracle} does.</li>
 *   <li><b>The per-item default {@code Tool} component.</b> A vanilla pickaxe
 *       carries its {@code minecraft:tool} in the item's <i>prototype</i>
 *       component map, so it is never present in the {@code DataComponentPatch}
 *       the server puts on the wire. The client is expected to already know it.
 *       </li>
 * </ol>
 *
 * Output is line-oriented; all floats are raw {@code Float.floatToRawIntBits}
 * hex so nothing is lost in the text round-trip.
 *
 * <pre>
 *   B &lt;blockRegistryId&gt; &lt;blockName&gt;
 *   T &lt;tagName&gt; &lt;blockName&gt; &lt;blockName&gt; ...
 *   I &lt;itemName&gt; &lt;defaultMiningSpeedBits&gt; &lt;damagePerBlock&gt; &lt;canDestroyBlocksInCreative&gt; &lt;ruleCount&gt;
 *   R &lt;blocks&gt; &lt;speedBits|-&gt; &lt;correctForDrops 1|0|-&gt;
 * </pre>
 *
 * where {@code <blocks>} is {@code #<tagName>} for a tag-backed rule, or
 * {@code =name,name,...} for an explicit list, and each {@code I} line is
 * immediately followed by its own {@code ruleCount} {@code R} lines, in rule
 * order — order is load-bearing, {@code getMiningSpeed} returns the first match.
 */
public final class ToolOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // Bootstrap initialises the registries but leaves every tag set empty:
        // tags are datapack content, not code. Without binding them,
        // Registry.getTags() yields nothing and a tool rule's HolderSet cannot
        // be resolved to blocks at all.
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

        // In 26.2 an item's *prototype* component map is baked at datapack
        // reload, not at class-init: `Item.components()` throws "Components not
        // bound yet" until someone runs the registered initializers. The server
        // does this in ReloadableServerResources.loadResources; do the same here
        // (with tags already bound above, since some initializers resolve tags).
        BuiltInRegistries.DATA_COMPONENT_INITIALIZERS
                .build(VanillaRegistries.createLookup())
                .forEach(DataComponentInitializers.PendingComponents::apply);

        StringBuilder sb = new StringBuilder();

        sb.append("# ToolOracle dump from the real 26.2 server (protocol 776).\n");
        sb.append("# B <blockRegistryId> <blockName>\n");
        sb.append("# T <tagName> <blockName>...\n");
        sb.append("# I <itemName> <defaultMiningSpeedBits> <damagePerBlock>"
                + " <canDestroyBlocksInCreative> <ruleCount>\n");
        sb.append("# R <#tag|=name,name,...> <speedBits|-> <correctForDrops 1|0|->\n");

        for (Block block : BuiltInRegistries.BLOCK) {
            sb.append("B ")
                    .append(BuiltInRegistries.BLOCK.getId(block))
                    .append(' ')
                    .append(BuiltInRegistries.BLOCK.getKey(block))
                    .append('\n');
        }

        List<HolderSet.Named<Block>> tags = new ArrayList<>();
        BuiltInRegistries.BLOCK.getTags().forEach(tags::add);
        tags.sort((a, b) -> a.key().location().toString().compareTo(b.key().location().toString()));
        for (HolderSet.Named<Block> tag : tags) {
            sb.append("T ").append(tag.key().location());
            List<String> names = new ArrayList<>();
            for (Holder<Block> holder : tag) {
                names.add(BuiltInRegistries.BLOCK.getKey(holder.value()).toString());
            }
            names.sort(null);
            for (String name : names) {
                sb.append(' ').append(name);
            }
            sb.append('\n');
        }

        List<Item> items = new ArrayList<>();
        BuiltInRegistries.ITEM.forEach(items::add);
        items.sort((a, b) -> BuiltInRegistries.ITEM.getKey(a).toString()
                .compareTo(BuiltInRegistries.ITEM.getKey(b).toString()));
        for (Item item : items) {
            Tool tool = item.components().get(DataComponents.TOOL);
            if (tool == null) {
                continue;
            }
            sb.append("I ")
                    .append(BuiltInRegistries.ITEM.getKey(item))
                    .append(' ')
                    .append(Integer.toHexString(Float.floatToRawIntBits(tool.defaultMiningSpeed())))
                    .append(' ')
                    .append(tool.damagePerBlock())
                    .append(' ')
                    .append(tool.canDestroyBlocksInCreative() ? 1 : 0)
                    .append(' ')
                    .append(tool.rules().size())
                    .append('\n');
            for (Tool.Rule rule : tool.rules()) {
                sb.append("R ");
                Either<TagKey<Block>, List<Holder<Block>>> blocks = rule.blocks().unwrap();
                Optional<TagKey<Block>> tagKey = blocks.left();
                if (tagKey.isPresent()) {
                    sb.append('#').append(tagKey.get().location());
                } else {
                    sb.append('=');
                    List<Holder<Block>> holders = blocks.right().orElseThrow();
                    for (int i = 0; i < holders.size(); i++) {
                        if (i > 0) {
                            sb.append(',');
                        }
                        sb.append(BuiltInRegistries.BLOCK.getKey(holders.get(i).value()));
                    }
                }
                sb.append(' ');
                Optional<Float> speed = rule.speed();
                if (speed.isPresent()) {
                    sb.append(Integer.toHexString(Float.floatToRawIntBits(speed.get())));
                } else {
                    sb.append('-');
                }
                sb.append(' ');
                Optional<Boolean> correct = rule.correctForDrops();
                if (correct.isPresent()) {
                    sb.append(correct.get() ? '1' : '0');
                } else {
                    sb.append('-');
                }
                sb.append('\n');
            }
        }

        System.out.print(sb);
    }
}
