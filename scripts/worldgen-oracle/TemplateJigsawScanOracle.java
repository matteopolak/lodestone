import java.io.InputStream;
import java.util.Arrays;
import java.util.jar.JarFile;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;

public final class TemplateJigsawScanOracle {
    public static void main(String[] args) throws Exception {
        String rawArgs = System.getenv().getOrDefault("ORACLE_ARGS", "").trim();
        if (args.length == 0 && !rawArgs.isEmpty()) args = rawArgs.split("\\s+");
        if (args.length == 0) throw new IllegalArgumentException("template ids required");
        try (JarFile jar = new JarFile("/mc/versions/26.2/server-26.2.jar")) {
            for (String id : args) {
                String entry = "data/minecraft/structures/" + id + ".nbt";
                var resource = jar.getJarEntry(entry);
                if (resource == null) throw new IllegalArgumentException("missing " + entry);
                try (InputStream in = jar.getInputStream(resource)) {
                    CompoundTag root = NbtIo.readCompressed(in, NbtAccounter.unlimitedHeap());
                    ListTag palettes = root.getListOrEmpty("palette");
                    ListTag blocks = root.getListOrEmpty("blocks");
                    System.out.println("template " + id + " size=" + root.getListOrEmpty("size"));
                    for (int i = 0; i < blocks.size(); i++) {
                        CompoundTag block = blocks.getCompoundOrEmpty(i);
                        int stateIndex = block.getIntOr("state", -1);
                        CompoundTag state = palettes.getCompoundOrEmpty(stateIndex);
                        if (!"minecraft:jigsaw".equals(state.getStringOr("Name", ""))) continue;
                        int[] pos = block.getIntArray("pos").orElseThrow();
                        System.out.println(" jigsaw pos=" + Arrays.toString(pos)
                                + " state=" + state
                                + " nbt=" + block.getCompoundOrEmpty("nbt"));
                    }
                }
            }
        }
    }
}
