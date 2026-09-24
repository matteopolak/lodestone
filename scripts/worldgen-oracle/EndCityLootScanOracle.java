// Independent End-city container fixture scanner. It reads generated chunk
// NBT through the bundled server's RegionFile and reports container payloads
// from chunks that carry an End-city structure start.
import java.io.DataInputStream;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;

public final class EndCityLootScanOracle {
    public static void main(String[] args) throws Exception {
        String rawArgs = System.getenv().getOrDefault("ORACLE_ARGS", "").trim();
        if (args.length == 0 && !rawArgs.isEmpty()) args = rawArgs.split("\\s+");
        if (args.length != 1 && args.length != 3) {
            throw new IllegalArgumentException("usage: EndCityLootScanOracle <region-dir> [<rx> <rz>]");
        }
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        Path regionDir = Paths.get(args[0]);
        List<int[]> regions = new ArrayList<>();
        if (args.length == 3) {
            regions.add(new int[] {Integer.parseInt(args[1]), Integer.parseInt(args[2])});
        } else {
            try (var files = Files.list(regionDir)) {
                files.map(Path::getFileName)
                    .map(Path::toString)
                    .filter(name -> name.startsWith("r.") && name.endsWith(".mca"))
                    .forEach(name -> {
                        String[] fields = name.substring(2, name.length() - 4).split("\\.");
                        if (fields.length == 2) regions.add(new int[] {Integer.parseInt(fields[0]), Integer.parseInt(fields[1])});
                    });
            }
        }
        Path levelDat = regionDir.resolve("../../../..").normalize().resolve("level.dat");
        if (Files.exists(levelDat)) {
            CompoundTag level = NbtIo.readCompressed(levelDat, NbtAccounter.unlimitedHeap());
            long seed = level.getCompoundOrEmpty("Data").getLongOr("RandomSeed", 0L);
            System.out.println("seed " + seed);
        }
        for (int[] regionCoords : regions) scanRegion(regionDir, regionCoords[0], regionCoords[1]);
    }

    private static void scanRegion(Path regionDir, int regionX, int regionZ) throws IOException {
        Path regionPath = regionDir.resolve("r." + regionX + "." + regionZ + ".mca");
        if (Files.size(regionPath) == 0) return;
        RegionStorageInfo info = new RegionStorageInfo("end-city-loot-fixture", Level.END, "chunk");
        try (RegionFile region = new RegionFile(info, regionPath, regionDir, true)) {
            for (int localX = 0; localX < 32; localX++) {
                for (int localZ = 0; localZ < 32; localZ++) {
                    int chunkX = regionX * 32 + localX;
                    int chunkZ = regionZ * 32 + localZ;
                    try (DataInputStream input = region.getChunkDataInputStream(new ChunkPos(chunkX, chunkZ))) {
                        if (input == null) continue;
                        CompoundTag chunk = NbtIo.read(input);
                        CompoundTag start = chunk.getCompoundOrEmpty("structures")
                            .getCompoundOrEmpty("starts")
                            .getCompoundOrEmpty("minecraft:end_city");
                        boolean city = !start.isEmpty();
                        if (city) System.out.println("city " + chunkX + " " + chunkZ + " " + blockEntitiesCount(chunk));
                        ListTag blockEntities = chunk.getListOrEmpty("block_entities");
                        for (int i = 0; i < blockEntities.size(); i++) {
                            CompoundTag entity = blockEntities.getCompoundOrEmpty(i);
                            String id = entity.getStringOr("id", "");
                            if (!id.equals("minecraft:chest")) continue;
                            String table = entity.getStringOr("LootTable", "");
                            long seed = entity.getLongOr("LootTableSeed", 0L);
                            int x = entity.getIntOr("x", 0);
                            int y = entity.getIntOr("y", 0);
                            int z = entity.getIntOr("z", 0);
                            System.out.println("chest " + chunkX + " " + chunkZ + " " + x + " " + y + " " + z + " " + table + " " + seed + " city=" + city);
                        }
                    }
                }
            }
        }
    }

    private static int blockEntitiesCount(CompoundTag chunk) {
        return chunk.getListOrEmpty("block_entities").size();
    }
}
