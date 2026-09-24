import java.io.DataInputStream;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.NbtIo;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;

public final class StructureScanOracle {
    public static void main(String[] args) throws Exception {
        String rawArgs = System.getenv().getOrDefault("ORACLE_ARGS", "").trim();
        if (args.length == 0 && !rawArgs.isEmpty()) args = rawArgs.split("\\s+");
        if (args.length > 5 && "--light-free".equals(args[args.length - 1])) {
            args = java.util.Arrays.copyOf(args, args.length - 1);
        }
        boolean compact = java.util.Arrays.asList(args).contains("--compact");
        if (compact) {
            args = java.util.Arrays.stream(args).filter(arg -> !"--compact".equals(arg)).toArray(String[]::new);
        }
        if (args.length != 5) throw new IllegalArgumentException("usage: StructureScanOracle <region-dir> <rx> <rz> <chunk-x> <chunk-z>");
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        Path regionDir = Paths.get(args[0]);
        int rx = Integer.parseInt(args[1]);
        int rz = Integer.parseInt(args[2]);
        int chunkX = Integer.parseInt(args[3]);
        int chunkZ = Integer.parseInt(args[4]);
        Path regionPath = regionDir.resolve("r." + rx + "." + rz + ".mca");
        RegionStorageInfo info = new RegionStorageInfo("structure-scan", Level.OVERWORLD, "chunk");
        try (RegionFile region = new RegionFile(info, regionPath, regionDir, true);
             DataInputStream input = region.getChunkDataInputStream(new ChunkPos(chunkX, chunkZ))) {
            if (input == null) throw new IllegalArgumentException("no generated chunk at " + chunkX + "," + chunkZ);
            CompoundTag chunk = NbtIo.read(input);
            CompoundTag structures = chunk.getCompoundOrEmpty("structures");
            System.out.println("structures=" + structures);
            CompoundTag starts = structures.getCompoundOrEmpty("starts");
            List<String> keys = new ArrayList<>(starts.keySet());
            keys.sort(Comparator.naturalOrder());
            for (String key : keys) {
                CompoundTag start = starts.getCompoundOrEmpty(key);
                if (compact) {
                    var children = start.getListOrEmpty("Children");
                    System.out.println("compact-start " + key + " children=" + children.size());
                    for (int i = 0; i < children.size(); i++) {
                        CompoundTag child = children.getCompoundOrEmpty(i);
                        int[] bb = child.getIntArray("BB").orElseThrow();
                        CompoundTag pool = child.getCompoundOrEmpty("pool_element");
                        StringBuilder line = new StringBuilder("piece ").append(i)
                                .append(" bb=").append(java.util.Arrays.toString(bb))
                                .append(" gd=").append(child.getIntOr("ground_level_delta", 0))
                                .append(" location=").append(pool.getStringOr("location", ""))
                                .append(" projection=").append(pool.getStringOr("projection", ""));
                        var junctions = child.getListOrEmpty("junctions");
                        for (int j = 0; j < junctions.size(); j++) {
                            CompoundTag junction = junctions.getCompoundOrEmpty(j);
                            line.append(" junction=")
                                    .append(junction.getIntOr("source_x", 0)).append(',')
                                    .append(junction.getIntOr("source_ground_y", 0)).append(',')
                                    .append(junction.getIntOr("source_z", 0));
                        }
                        System.out.println(line);
                    }
                    continue;
                }
                System.out.println("start " + key + "=" + start);
            }
            CompoundTag references = structures.getCompoundOrEmpty("References");
            System.out.println("references=" + references);
        }
    }
}
