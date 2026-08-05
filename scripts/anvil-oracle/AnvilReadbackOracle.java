// The external oracle for issue #437's *write* path: the real Mojang 26.2
// server reading a region file **Lodestone wrote**.
//
// Everything else about persistence can be evidenced by reading files vanilla
// wrote. This is the other direction, and it is the one a round trip through
// our own code cannot establish at all: that what we produce is loadable by the
// program that defines the format.
//
// Three separate things we could have got wrong are each checked by Mojang's
// own code rather than by ours:
//
//   1. the container -- `RegionFile` itself opens our `.mca`, walks our sector
//      table and decompresses our chunk payload;
//   2. the palette entry -- `BlockState.CODEC` parses our `{Name, Properties}`
//      compounds, so a wrong property name, a wrong value, or an unsorted
//      reconstruction is a parse failure and not a silent mismatch;
//   3. the bit packing -- `SimpleBitStorage`, the exact class vanilla stores
//      section data in, unpacks our `data` long array. This is the one that
//      matters most: non-spanning versus dense packing is invisible for every
//      palette of 16 or fewer entries.
//
// Output is one `x,y,z=blockstate` line per probe on stdout, prefixed `RESULT
// `, for `tests/write_path_jvm_oracle.rs` to compare against what it asked for.
//
// Usage (via run.sh): ORACLE_ARGS="<regionDir> <rx> <rz> <x>,<y>,<z> ..."

import com.mojang.serialization.DataResult;
import java.io.DataInputStream;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;

import net.minecraft.SharedConstants;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtIo;
import net.minecraft.nbt.NbtOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.SimpleBitStorage;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;

public final class AnvilReadbackOracle {

    public static void main(String[] args) throws Exception {
        String raw = System.getenv("ORACLE_ARGS");
        if (raw == null || raw.isBlank()) {
            throw new IllegalArgumentException("ORACLE_ARGS is required");
        }
        String[] parts = raw.trim().split("\\s+");
        Path regionDir = Paths.get(parts[0]);
        int rx = Integer.parseInt(parts[1]);
        int rz = Integer.parseInt(parts[2]);

        // The registries `BlockState.CODEC` resolves names against. Without
        // this the codec parses nothing and every probe would "fail" for a
        // reason unrelated to our bytes -- a premise-false control.
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Path file = regionDir.resolve("r." + rx + "." + rz + ".mca");
        System.out.println("INFO opening " + file + " with Mojang's own RegionFile");

        // `Level.OVERWORLD` rather than a hand-built key: 26.2 renamed
        // `ResourceLocation` to `Identifier`, and reaching for the constant
        // avoids depending on which name this version uses.
        RegionStorageInfo info = new RegionStorageInfo("lodestone", Level.OVERWORLD, "chunk");

        List<String> results = new ArrayList<>();
        try (RegionFile region = new RegionFile(info, file, regionDir, true)) {
            for (int i = 3; i < parts.length; i++) {
                String[] xyz = parts[i].split(",");
                int x = Integer.parseInt(xyz[0]);
                int y = Integer.parseInt(xyz[1]);
                int z = Integer.parseInt(xyz[2]);
                results.add(x + "," + y + "," + z + "=" + probe(region, x, y, z));
            }
        }
        for (String line : results) {
            System.out.println("RESULT " + line);
        }
    }

    private static String probe(RegionFile region, int x, int y, int z) throws Exception {
        ChunkPos pos = new ChunkPos(Math.floorDiv(x, 16), Math.floorDiv(z, 16));
        if (!region.hasChunk(pos)) {
            return "<no-chunk>";
        }
        CompoundTag chunk;
        try (DataInputStream in = region.getChunkDataInputStream(pos)) {
            if (in == null) {
                return "<no-stream>";
            }
            chunk = NbtIo.read(in);
        }

        int sectionY = Math.floorDiv(y, 16);
        ListTag sections = chunk.getListOrEmpty("sections");
        for (int i = 0; i < sections.size(); i++) {
            CompoundTag section = sections.getCompound(i).orElse(null);
            if (section == null || section.getByteOr("Y", (byte) 0) != sectionY) {
                continue;
            }
            CompoundTag blockStates = section.getCompound("block_states").orElse(null);
            if (blockStates == null) {
                return "<no-block-states>";
            }

            // (2) Mojang's own palette-entry codec.
            ListTag palette = blockStates.getListOrEmpty("palette");
            List<BlockState> states = new ArrayList<>(palette.size());
            for (int p = 0; p < palette.size(); p++) {
                CompoundTag entry = palette.getCompound(p).orElse(null);
                if (entry == null) {
                    return "<palette-entry-not-compound>";
                }
                DataResult<BlockState> parsed = BlockState.CODEC.parse(NbtOps.INSTANCE, entry);
                if (parsed.isError()) {
                    return "<palette-parse-error:" + parsed.error().get().message() + ">";
                }
                states.add(parsed.getOrThrow());
            }
            if (states.isEmpty()) {
                return "<empty-palette>";
            }

            int cell = ((y & 15) << 8) | ((z & 15) << 4) | (x & 15);
            long[] data = blockStates.getLongArray("data").orElse(null);
            int index;
            if (data == null) {
                // Vanilla omits `data` for a single-valued container.
                index = 0;
            } else {
                int bits = Math.max(4, 32 - Integer.numberOfLeadingZeros(states.size() - 1));
                // (3) Mojang's own bit storage, not a reimplementation.
                SimpleBitStorage storage = new SimpleBitStorage(bits, 4096, data);
                index = storage.get(cell);
            }
            if (index >= states.size()) {
                return "<index-out-of-range:" + index + "/" + states.size() + ">";
            }
            return blockStateToString(states.get(index));
        }
        return "<no-section>";
    }

    /// Renders a resolved `BlockState` in the same canonical
    /// `namespace:name[a=1,b=2]` form Lodestone uses, with properties sorted by
    /// name so the comparison is on the state and not on field order.
    private static String blockStateToString(BlockState state) {
        StringBuilder out = new StringBuilder();
        out.append(net.minecraft.core.registries.BuiltInRegistries.BLOCK.getKey(state.getBlock()));
        List<String> props = new ArrayList<>();
        state.getProperties()
                .forEach(property -> props.add(
                        property.getName() + "=" + getName(state, property)));
        if (!props.isEmpty()) {
            props.sort(String::compareTo);
            out.append('[').append(String.join(",", props)).append(']');
        }
        return out.toString();
    }

    @SuppressWarnings("unchecked")
    private static <T extends Comparable<T>> String getName(
            BlockState state, net.minecraft.world.level.block.state.properties.Property<?> property) {
        net.minecraft.world.level.block.state.properties.Property<T> typed =
                (net.minecraft.world.level.block.state.properties.Property<T>) property;
        return typed.getName(state.getValue(typed));
    }

    private AnvilReadbackOracle() {}
}
