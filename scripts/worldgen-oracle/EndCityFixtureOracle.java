// Independent End-city fixture extractor. It reads a generated End region through
// the bundled server's RegionFile and reports the start compound plus non-terrain
// block states; Lodestone generation code does not participate in the output.
import java.io.DataInputStream;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtIo;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.SimpleBitStorage;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;

public final class EndCityFixtureOracle {
    public static void main(String[] args) throws Exception {
        if (args.length != 6) {
            throw new IllegalArgumentException("usage: EndCityFixtureOracle <region-dir> <rx> <rz> <chunk-x> <chunk-z> <structure-id>");
        }
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        Path regionDir = Paths.get(args[0]);
        int rx = Integer.parseInt(args[1]);
        int rz = Integer.parseInt(args[2]);
        int chunkX = Integer.parseInt(args[3]);
        int chunkZ = Integer.parseInt(args[4]);
        String structureId = args[5];
        Path regionPath = regionDir.resolve("r." + rx + "." + rz + ".mca");
        RegionStorageInfo info = new RegionStorageInfo("end-city-fixture", Level.END, "chunk");
        try (RegionFile region = new RegionFile(info, regionPath, regionDir, true);
             DataInputStream input = region.getChunkDataInputStream(new ChunkPos(chunkX, chunkZ))) {
            if (input == null) {
                throw new IllegalArgumentException("no generated chunk at " + chunkX + "," + chunkZ);
            }
            CompoundTag chunk = NbtIo.read(input);
            CompoundTag start = chunk.getCompoundOrEmpty("structures")
                .getCompoundOrEmpty("starts")
                .getCompoundOrEmpty(structureId);
            System.out.println("start " + start);
            dumpNonTerrainBlocks(chunk, chunkX, chunkZ);
        }
    }

    private static void dumpNonTerrainBlocks(CompoundTag chunk, int chunkX, int chunkZ) {
        ListTag sections = chunk.getListOrEmpty("sections");
        List<String> blocks = new ArrayList<>();
        for (int i = 0; i < sections.size(); i++) {
            CompoundTag section = sections.getCompound(i).orElse(null);
            if (section == null) continue;
            CompoundTag states = section.getCompound("block_states").orElse(null);
            if (states == null) continue;
            ListTag palette = states.getListOrEmpty("palette");
            List<BlockState> decoded = new ArrayList<>(palette.size());
            for (int p = 0; p < palette.size(); p++) {
                CompoundTag entry = palette.getCompound(p).orElse(null);
                if (entry == null) continue;
                decoded.add(BlockState.CODEC.parse(net.minecraft.nbt.NbtOps.INSTANCE, entry).getOrThrow());
            }
            if (decoded.isEmpty()) continue;
            long[] data = states.getLongArray("data").orElse(null);
            SimpleBitStorage storage = data == null ? null : new SimpleBitStorage(
                Math.max(4, 32 - Integer.numberOfLeadingZeros(decoded.size() - 1)), 4096, data);
            int sectionY = section.getByteOr("Y", (byte)0);
            for (int cell = 0; cell < 4096; cell++) {
                BlockState state = decoded.get(storage == null ? 0 : storage.get(cell));
                String id = net.minecraft.core.registries.BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
                if (id.equals("minecraft:air") || id.equals("minecraft:end_stone")) continue;
                int x = chunkX * 16 + (cell & 15);
                int y = sectionY * 16 + (cell >> 8);
                int z = chunkZ * 16 + ((cell >> 4) & 15);
                blocks.add(x + "," + y + "," + z + "=" + state);
            }
        }
        blocks.sort(String::compareTo);
        for (String block : blocks) System.out.println("block " + block);
    }
}
