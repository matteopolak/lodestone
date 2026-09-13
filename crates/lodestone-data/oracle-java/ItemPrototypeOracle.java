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
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.equipment.Equippable;

/**
 * Authoritative extractor for the item **prototype** components a clientbound
 * stack never carries — mirrors {@code ToolOracle} and {@code HardnessOracle} in
 * this same directory, and shares {@code ToolOracle}'s bootstrap exactly.
 *
 * <p>A clientbound {@code ItemStack} is an item id plus a
 * {@code DataComponentPatch}: the <i>delta</i> from the item's built-in
 * prototype component map. Three components that gameplay needs live only in
 * that prototype, so they are never on the wire and cannot be captured from a
 * packet dump at all:
 *
 * <ol>
 *   <li>{@code minecraft:max_stack_size} — every item has one, from
 *       {@code DataComponents.COMMON_ITEM_COMPONENTS} ({@code DataComponents.java:419-430},
 *       which sets {@code 64}) or an item-specific override. Absent it,
 *       {@code ItemInstance.getMaxStackSize} falls back to <b>1</b>, not 64
 *       ({@code ItemInstance.java:14-16}), so a client that does not know the
 *       real value cannot guess either way.</li>
 *   <li>{@code minecraft:max_damage} — gates {@code ItemStack.isDamageableItem}
 *       ({@code ItemStack.java:416-418}) and therefore
 *       {@code ItemStack.isStackable} ({@code ItemStack.java:412-414}).</li>
 *   <li>{@code minecraft:equippable} — {@code ArmorSlot.mayPlace} is
 *       {@code owner.isEquippableInSlot(stack, slot)}
 *       ({@code ArmorSlot.java:43-46}), which is
 *       {@code slot == equippable.slot() && canUseSlot(...) && equippable.canBeEquippedBy(type)}
 *       ({@code LivingEntity.java:3886-3891}). With no component at all the only
 *       accepting slot is {@code MAINHAND}, i.e. no armour is placeable.</li>
 * </ol>
 *
 * <p>Output is line-oriented, one record per item in <b>registry id</b> order
 * (so a consumer can index by the id the wire carries, and the ids can be
 * cross-checked against Mojang's own {@code registries.json}):
 *
 * <pre>
 *   P &lt;registryId&gt; &lt;itemName&gt; &lt;maxStackSize&gt; &lt;maxDamage|-&gt; &lt;hasDamage 0|1&gt; &lt;equipSlot|-&gt; &lt;allowedEntities&gt;
 * </pre>
 *
 * where {@code maxStackSize} is the effective
 * {@code getOrDefault(MAX_STACK_SIZE, 1)}; {@code maxDamage} is {@code -} when
 * the prototype has no {@code minecraft:max_damage} at all (as opposed to one
 * whose value happens to be 0); {@code hasDamage} reports whether the prototype
 * also carries {@code minecraft:damage}, which {@code isDamageableItem}
 * separately requires; {@code equipSlot} is
 * {@code Equippable.slot().getSerializedName()} or {@code -}; and
 * {@code allowedEntities} is {@code -} for "any entity"
 * ({@code Equippable.allowedEntities} empty, {@code Equippable.java:175-177}),
 * else {@code #<tag>} or {@code =name,name,...}.
 */
public final class ItemPrototypeOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // Same two-step bootstrap ToolOracle needs, and for the same reasons:
        // tags are datapack content (some component initializers resolve them),
        // and in 26.2 an item's prototype component map is baked at datapack
        // reload rather than class-init — `Item.components()` throws
        // "Components not bound yet" until the registered initializers run.
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

        BuiltInRegistries.DATA_COMPONENT_INITIALIZERS
                .build(VanillaRegistries.createLookup())
                .forEach(DataComponentInitializers.PendingComponents::apply);

        StringBuilder sb = new StringBuilder();
        sb.append("# ItemPrototypeOracle dump from the real 26.2 server (protocol 776).\n");
        sb.append("# P <registryId> <itemName> <maxStackSize> <maxDamage|-> <hasDamage 0|1>"
                + " <equipSlot|-> <allowedEntities -|#tag|=a,b>\n");

        List<Item> items = new ArrayList<>();
        BuiltInRegistries.ITEM.forEach(items::add);
        items.sort((a, b) -> Integer.compare(
                BuiltInRegistries.ITEM.getId(a), BuiltInRegistries.ITEM.getId(b)));

        for (Item item : items) {
            sb.append("P ")
                    .append(BuiltInRegistries.ITEM.getId(item))
                    .append(' ')
                    .append(BuiltInRegistries.ITEM.getKey(item))
                    .append(' ')
                    // The effective value the game reads, not the raw component:
                    // ItemInstance.getMaxStackSize()'s own fallback is 1.
                    .append(item.components().getOrDefault(DataComponents.MAX_STACK_SIZE, 1))
                    .append(' ');

            Integer maxDamage = item.components().get(DataComponents.MAX_DAMAGE);
            sb.append(maxDamage == null ? "-" : maxDamage.toString()).append(' ');
            sb.append(item.components().has(DataComponents.DAMAGE) ? '1' : '0').append(' ');

            Equippable equippable = item.components().get(DataComponents.EQUIPPABLE);
            if (equippable == null) {
                sb.append("- -");
            } else {
                sb.append(equippable.slot().getSerializedName()).append(' ');
                Optional<HolderSet<EntityType<?>>> allowed = equippable.allowedEntities();
                if (allowed.isEmpty()) {
                    sb.append('-');
                } else {
                    Either<TagKey<EntityType<?>>, List<Holder<EntityType<?>>>> unwrapped =
                            allowed.get().unwrap();
                    Optional<TagKey<EntityType<?>>> tag = unwrapped.left();
                    if (tag.isPresent()) {
                        sb.append('#').append(tag.get().location());
                    } else {
                        sb.append('=');
                        List<Holder<EntityType<?>>> holders = unwrapped.right().orElseThrow();
                        for (int i = 0; i < holders.size(); i++) {
                            if (i > 0) {
                                sb.append(',');
                            }
                            sb.append(BuiltInRegistries.ENTITY_TYPE.getKey(holders.get(i).value()));
                        }
                    }
                }
            }
            sb.append('\n');
        }

        System.out.print(sb);
    }
}
