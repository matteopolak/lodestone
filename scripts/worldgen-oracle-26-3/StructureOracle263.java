// The structure starts the real 26.3 server creates over a window of chunks, one line per start:
//   start <chunkX> <chunkZ> <structure> <minX> <minY> <minZ> <maxX> <maxY> <maxZ> <pieces>
//   or: <seed> chunks <cx> <cz> [<cx> <cz> ...]   (just those chunks, plus the stronghold ring list)
// Each chunk is created independently (starts depend on the seed, the biome source and base
// heights only), exactly as the server's own per-chunk structure creation runs.
//   args: <seed> <minChunkX> <minChunkZ> <maxChunkX> <maxChunkZ>
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.*;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.*;
import net.minecraft.network.chat.Component;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.structure.*;
import net.minecraft.world.level.levelgen.structure.placement.ConcentricRingsStructurePlacement;
import net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager;
import net.minecraft.util.datafix.DataFixers;

public class StructureOracle263 {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        WorldOracle263.bindTags();
        HolderLookup.Provider provider = WorldOracle263.loadWorldgenRegistries();
        long seed = Long.parseLong(args[0]);
        boolean detail = args[1].equals("chunksp");
        boolean listMode = args[1].equals("ringsynth") || args[1].equals("chunks") || args[1].equals("chunksp") || args[1].equals("heights");
        int x0 = 0, z0 = 0, x1 = -1, z1 = -1;
        if (!listMode) { x0 = Integer.parseInt(args[1]); z0 = Integer.parseInt(args[2]); x1 = Integer.parseInt(args[3]); z1 = Integer.parseInt(args[4]); }
        var settingsHolder = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace("overworld")));
        RandomState rs = RandomState.create(provider.lookupOrThrow(Registries.NOISE), seed, settingsHolder.value());
        var biomeLookup = provider.lookupOrThrow(Registries.BIOME);
        var preset = net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList.knownPresets().entrySet().stream()
            .filter(e -> e.getKey().id().getPath().equals("overworld")).findFirst().orElseThrow().getValue();
        List<com.mojang.datafixers.util.Pair<Climate.ParameterPoint, Holder<Biome>>> rows = new ArrayList<>();
        for (var p : preset.values()) rows.add(com.mojang.datafixers.util.Pair.of(p.getFirst(), biomeLookup.getOrThrow(p.getSecond())));
        BiomeSource source = MultiNoiseBiomeSource.createFromList(new Climate.ParameterList<>(rows));
        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(source, settingsHolder);
        ChunkGeneratorStructureState state = generator.createState(provider.lookupOrThrow(Registries.STRUCTURE_SET), rs, seed);

        PackLocationInfo info = new PackLocationInfo("builtin", Component.literal("builtin"), PackSource.BUILT_IN, Optional.empty());
        PackResources pack = new PathPackResources(info, java.nio.file.Path.of("/mc/src"));
        ResourceManager manager = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(pack));
        sun.misc.Unsafe unsafe;
        { var f = sun.misc.Unsafe.class.getDeclaredField("theUnsafe"); f.setAccessible(true); unsafe = (sun.misc.Unsafe) f.get(null); }
        var access0 = (net.minecraft.world.level.storage.LevelStorageSource.LevelStorageAccess)
            unsafe.allocateInstance(net.minecraft.world.level.storage.LevelStorageSource.LevelStorageAccess.class);
        var dirField = access0.getClass().getDeclaredField("levelDirectory");
        dirField.setAccessible(true);
        dirField.set(access0, new net.minecraft.world.level.storage.LevelStorageSource.LevelDirectory(java.nio.file.Path.of("/tmp/oracle-level")));
        var resField = access0.getClass().getDeclaredField("resources");
        resField.setAccessible(true);
        resField.set(access0, new java.util.HashMap<>());
        StructureTemplateManager templates = new StructureTemplateManager(manager, access0, DataFixers.getDataFixer(), BuiltInRegistries.BLOCK);
        StructureManager structures = new StructureManager(null, new WorldOptions(seed, true, false), null);

        var idMap = new net.minecraft.core.IdMapper<Holder<Biome>>();
        biomeLookup.listElements().forEach(idMap::add);
        var plains = biomeLookup.getOrThrow(Biomes.PLAINS);
        var pcf = new PalettedContainerFactory(
            net.minecraft.world.level.chunk.Strategy.createForBlockStates(Block.BLOCK_STATE_REGISTRY), Blocks.AIR.defaultBlockState(), null,
            net.minecraft.world.level.chunk.Strategy.createForBiomes(idMap), plains, null);
        LevelHeightAccessor access = LevelHeightAccessor.create(-64, 384);
        var staticAccess = RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY);
        var loaded = (RegistryAccess) provider;
        var registryAccess = new RegistryAccess.ImmutableRegistryAccess(java.util.stream.Stream.concat(
            staticAccess.registries().map(e -> e.value()), loaded.registries().map(e -> e.value())).toList());
        var structureReg = loaded.lookupOrThrow(Registries.STRUCTURE);
        if (args[1].equals("ringsynth")) {
            // args: ringsynth <distance> <spread> <count>: the real ring walk with every biome preferred.
            int distance = Integer.parseInt(args[2]), spread = Integer.parseInt(args[3]), count = Integer.parseInt(args[4]);
            net.minecraft.util.RandomSource random = net.minecraft.util.RandomSource.create();
            random.setSeed(seed);
            double angle = random.nextDouble() * Math.PI * 2.0;
            int positionInCircle = 0, circle = 0;
            for (int i = 0; i < count; i++) {
                double dist = 4 * distance + distance * circle * 6 + (random.nextDouble() - 0.5) * (distance * 2.5);
                int initialX = (int) Math.round(Math.cos(angle) * dist);
                int initialZ = (int) Math.round(Math.sin(angle) * dist);
                net.minecraft.util.RandomSource biomeSearchGenerator = random.fork();
                var found = source.findBiomeHorizontal(net.minecraft.core.SectionPos.sectionToBlockCoord(initialX, 8), 0,
                    net.minecraft.core.SectionPos.sectionToBlockCoord(initialZ, 8), 112, b -> true, biomeSearchGenerator, rs);
                int fx = initialX, fz = initialZ;
                if (found != null) { fx = net.minecraft.core.SectionPos.blockToSectionCoord(found.getFirst().getX()); fz = net.minecraft.core.SectionPos.blockToSectionCoord(found.getFirst().getZ()); }
                WorldOracle263.OUT.println("ringsynth " + i + " " + initialX + " " + initialZ + " " + fx + " " + fz);
                angle += (Math.PI * 2) / spread;
                if (++positionInCircle == spread) {
                    circle++;
                    positionInCircle = 0;
                    spread += 2 * spread / (circle + 1);
                    spread = Math.min(spread, count - i);
                    angle += random.nextDouble() * Math.PI * 2.0;
                }
            }
            WorldOracle263.OUT.flush();
            return;
        }
        if (args[1].equals("heights")) {
            for (int a = 2; a + 1 < args.length; a += 2) {
                int x = Integer.parseInt(args[a]), z = Integer.parseInt(args[a + 1]);
                WorldOracle263.OUT.println("height " + x + " " + z + " " + generator.getBaseHeight(x, z, Heightmap.Types.OCEAN_FLOOR_WG, access, rs)
                    + " " + generator.getBaseHeight(x, z, Heightmap.Types.WORLD_SURFACE_WG, access, rs));
            }
            WorldOracle263.OUT.flush();
            return;
        }
        List<int[]> targets = new ArrayList<>();
        if (listMode) {
            for (int a = 2; a + 1 < args.length; a += 2) targets.add(new int[]{Integer.parseInt(args[a]), Integer.parseInt(args[a + 1])});
            state.possibleStructureSets().forEach(set -> {
                if (set.value().placement() instanceof ConcentricRingsStructurePlacement rings)
                    for (ChunkPos rp : java.util.Objects.requireNonNull(state.getRingPositionsFor(rings))) WorldOracle263.OUT.println("ring " + rp.x() + " " + rp.z());
            });
        } else {
            for (int cz = z0; cz <= z1; cz++) for (int cx = x0; cx <= x1; cx++) targets.add(new int[]{cx, cz});
        }
        for (int[] t : targets) {
            int cx = t[0], cz = t[1];
            ProtoChunk chunk = new ProtoChunk(new ChunkPos(cx, cz), UpgradeData.EMPTY, access, pcf, null);
            generator.createStructures(registryAccess, state, structures, chunk, templates, Level.OVERWORLD);
            for (var e : chunk.getAllStarts().entrySet()) {
                StructureStart s = e.getValue();
                if (!s.isValid()) continue;
                var bb = s.getBoundingBox();
                WorldOracle263.OUT.println("start " + cx + " " + cz + " " + structureReg.getKey(e.getKey())
                    + " " + bb.minX() + " " + bb.minY() + " " + bb.minZ() + " " + bb.maxX() + " " + bb.maxY() + " " + bb.maxZ() + " " + s.getPieces().size());
                if (detail) {
                    int pi = 0;
                    for (var p : s.getPieces()) {
                        var pb = p.getBoundingBox();
                        String what = p instanceof net.minecraft.world.level.levelgen.structure.PoolElementStructurePiece pe
                            ? pe.getRotation() + " " + pe.getElement() : p.getClass().getSimpleName();
                        WorldOracle263.OUT.println("piece " + cx + " " + cz + " " + pi++ + " " + pb.minX() + " " + pb.minY() + " " + pb.minZ() + " " + pb.maxX() + " " + pb.maxY() + " " + pb.maxZ() + " " + what);
                    }
                }
            }
        }
        WorldOracle263.OUT.flush();
    }
}
