import com.mojang.serialization.JsonOps;
import net.minecraft.SharedConstants;
import net.minecraft.core.component.DataComponentPatch;
import net.minecraft.core.component.DataComponents;
import net.minecraft.nbt.NbtIo;
import net.minecraft.nbt.NbtOps;
import net.minecraft.nbt.Tag;
import net.minecraft.network.chat.ClickEvent;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.ComponentSerialization;
import net.minecraft.network.chat.HoverEvent;
import net.minecraft.network.chat.Style;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.entity.EntityTypes;
import net.minecraft.world.item.ItemStackTemplate;
import net.minecraft.world.item.Items;
import net.minecraft.world.item.component.ItemLore;

import java.io.DataOutputStream;
import java.io.FileOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.List;
import java.util.UUID;

/**
 * Authoritative capture of the interactive-style wire shapes this workspace's
 * text model has to parse: a {@code show_item} hover payload, a
 * {@code show_entity} one, and the two click actions whose argument is not a
 * plain string under {@code value}.
 *
 * <p>Boots registries only (no world, no data pack), the same way the data
 * crate's own extractors in {@code crates/lodestone-data/oracle-java} do. Item
 * <i>prototype</i> components are not bound by that boot, which is why the item
 * payload is built as a stack <i>template</i> from an explicit component patch
 * rather than from a live stack — a template needs only the item's registry
 * holder.
 *
 * <p><b>Why capture rather than hand-author the fixture.</b> Three of the
 * shapes here are counter-intuitive enough that hand-authoring them would have
 * encoded the same misunderstanding twice, once in the fixture and once in the
 * parser it is meant to check:
 *
 * <ul>
 *   <li>the style fields are spelled in snake case, so a parser looking only
 *       for the older camel-case names finds no interactivity at all;</li>
 *   <li>an item payload's fields sit directly beside {@code action} rather
 *       than nested under a payload key, and its count is omitted when it is
 *       one;</li>
 *   <li>a UUID is four signed 32-bit words, not text, and a
 *       {@code change_page} argument is a number under its own field name.</li>
 * </ul>
 *
 * <p>Writes two files, both committed and read by
 * {@code lodestone_model::tests}: the JSON forms as {@code KEY=<json>} lines,
 * and the NBT forms as length-prefixed network tags (a type byte then the
 * payload, no name — {@code NbtIo.writeAnyTag}'s output, which is what a
 * modern server puts on the wire).
 *
 * <p>Recreate the two fixtures by compiling and running this against the
 * cached 26.2 server jar, writing into the fixture directory — the same
 * registries-only boot the data crate's extractors use, so the same container
 * image works:
 *
 * <pre>
 * container run --rm --memory 3g \
 *   -v "$(cd .cache/mc/26.2 &amp;&amp; pwd)":/mc:ro \
 *   -v "$(cd crates/lodestone-model/oracle-java &amp;&amp; pwd)":/oracle:ro \
 *   -v "$(cd crates/lodestone-model/tests/data &amp;&amp; pwd)":/out \
 *   -w /work eclipse-temurin:25-jdk bash -c '
 *     set -e
 *     CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
 *     cp /oracle/HoverEventOracle.java /work/
 *     javac -cp "$CP" -d /work /work/HoverEventOracle.java
 *     java -cp "/work:$CP" HoverEventOracle /out
 *   '
 * </pre>
 */
public final class HoverEventOracle {
   /** The four component values the item payload carries, one per decoded field. */
   private static DataComponentPatch swordPatch() {
      return DataComponentPatch.builder()
         .set(DataComponents.CUSTOM_NAME, Component.literal("Widowmaker"))
         .set(DataComponents.LORE, new ItemLore(List.of(
            Component.literal("Forged in the deep"),
            Component.literal("Bane of spiders"))))
         .set(DataComponents.DAMAGE, 431)
         .set(DataComponents.MAX_DAMAGE, 1561)
         .build();
   }

   public static void main(final String[] args) throws Exception {
      SharedConstants.tryDetectVersion();
      Bootstrap.bootStrap();

      Path outDir = Path.of(args.length > 0 ? args[0] : ".");

      ItemStackTemplate sword =
         new ItemStackTemplate(Items.DIAMOND_SWORD.builtInRegistryHolder(), 1, swordPatch());

      Component showItem = Component.literal("was slain using ")
         .append(Component.literal("[Widowmaker]").withStyle(
            Style.EMPTY.withHoverEvent(new HoverEvent.ShowItem(sword))));

      HoverEvent.EntityTooltipInfo boris = new HoverEvent.EntityTooltipInfo(
         EntityTypes.SPIDER,
         UUID.fromString("6ba7b810-9dad-11d1-80b4-00c04fd430c8"),
         Component.literal("Boris"));
      Component showEntity = Component.literal("Boris").withStyle(
         Style.EMPTY.withHoverEvent(new HoverEvent.ShowEntity(boris)));

      Component changePage = Component.literal("Turn to page 3").withStyle(
         Style.EMPTY.withClickEvent(new ClickEvent.ChangePage(3)));

      Component runCommand = Component.literal("[Teleport]").withStyle(
         Style.EMPTY
            .withClickEvent(new ClickEvent.RunCommand("/tp @s 0 64 0"))
            .withInsertion("Notch"));

      String[] names = {"show_item", "show_entity", "change_page", "run_command"};
      Component[] components = {showItem, showEntity, changePage, runCommand};

      try (PrintStream json = new PrintStream(
            new FileOutputStream(outDir.resolve("hover_events_26_2.json").toFile()),
            false,
            StandardCharsets.UTF_8)) {
         for (int i = 0; i < names.length; i++) {
            json.println(names[i] + "="
               + ComponentSerialization.CODEC.encodeStart(JsonOps.INSTANCE, components[i]).getOrThrow());
         }
      }

      try (DataOutputStream nbt = new DataOutputStream(
            new FileOutputStream(outDir.resolve("hover_events_26_2_nbt.bin").toFile()))) {
         for (Component component : components) {
            Tag tag = ComponentSerialization.CODEC.encodeStart(NbtOps.INSTANCE, component).getOrThrow();
            java.io.ByteArrayOutputStream buffer = new java.io.ByteArrayOutputStream();
            NbtIo.writeAnyTag(tag, new DataOutputStream(buffer));
            // Length-prefixed so the reader can walk the four tags without
            // parsing them to find each one's end.
            nbt.writeInt(buffer.size());
            buffer.writeTo(nbt);
         }
      }

      // The three lines an entity tooltip shows, in order, as the jar composes
      // them — the expected layout the Rust side is checked against.
      System.out.println("entity_tooltip_lines=" + boris.getTooltipLines());
   }
}
