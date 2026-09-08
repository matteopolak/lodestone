// Independent End-city template marker extractor. It reads the packaged
// structure template and prints DATA marker positions without Lodestone code.
import java.io.InputStream;
import net.minecraft.SharedConstants;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import net.minecraft.server.Bootstrap;

public final class EndCityMarkerOracle {
    public static void main(String[] args) throws Exception {
        String rawArgs = System.getenv().getOrDefault("ORACLE_ARGS", "ship").trim();
        String template = rawArgs.isEmpty() ? "ship" : rawArgs.split("\\s+")[0];
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        String[] templates = template.equals("all")
            ? new String[] {"base_floor", "base_roof", "bridge_end", "bridge_gentle_stairs", "bridge_piece", "bridge_steep_stairs", "fat_tower_base", "fat_tower_middle", "fat_tower_top", "second_floor_1", "second_floor_2", "second_roof", "ship", "third_floor_1", "third_floor_2", "third_roof", "tower_base", "tower_floor", "tower_piece", "tower_top"}
            : new String[] {template};
        for (String selected : templates) {
            String resource = "data/minecraft/structure/end_city/" + selected + ".nbt";
            try (InputStream input = EndCityMarkerOracle.class.getClassLoader().getResourceAsStream(resource)) {
            if (input == null) throw new IllegalArgumentException("missing template " + resource);
            CompoundTag root = NbtIo.readCompressed(input, NbtAccounter.unlimitedHeap());
            ListTag palette = root.getListOrEmpty("palette");
            boolean[] markerStates = new boolean[palette.size()];
            for (int i = 0; i < palette.size(); i++) {
                CompoundTag state = palette.getCompoundOrEmpty(i);
                markerStates[i] = state.getStringOr("Name", "").equals("minecraft:structure_block");
            }
            ListTag blocks = root.getListOrEmpty("blocks");
            for (int i = 0; i < blocks.size(); i++) {
                CompoundTag block = blocks.getCompoundOrEmpty(i);
                int state = block.getIntOr("state", -1);
                if (state < 0 || state >= markerStates.length || !markerStates[state]) continue;
                ListTag pos = block.getListOrEmpty("pos");
                CompoundTag nbt = block.getCompoundOrEmpty("nbt");
                System.out.println(
                    "marker " + selected + " " + pos.getIntOr(0, 0) + " " + pos.getIntOr(1, 0) + " " + pos.getIntOr(2, 0)
                    + " " + nbt.getStringOr("mode", "") + " " + nbt.getStringOr("metadata", "")
                );
            }
        }
        }
    }
}
