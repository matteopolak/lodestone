import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Optional;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.placement.PlacementContext;
import net.minecraft.world.level.levelgen.placement.PlacementModifier;

public final class NetherStageOracle {
    private static int envInt(String name, int fallback) {
        String value = System.getenv(name);
        return value == null ? fallback : Integer.parseInt(value);
    }

    private static void replayLazy(
        ServerLevel level, PlacedFeature placed, WorldgenRandom random, BlockPos origin,
        int modifierIndex, List<PlacementModifier> modifiers, PlacementContext context,
        BlockPos focus, String id, int[] terminalCount) {
        if (modifierIndex == modifiers.size()) {
            terminalCount[0]++;
            BlockState before = level.getBlockState(focus);
            boolean result = placed.feature().value().place(
                level, level.getChunkSource().getGenerator(), random, origin);
            BlockState after = level.getBlockState(focus);
            if (!before.equals(after) || (origin.getX() >= -820 && origin.getX() <= -780
                    && origin.getZ() >= -820 && origin.getZ() <= -780)) {
                String biome = level.getBiome(origin).unwrapKey()
                    .map(key -> key.identifier().toString()).orElse("<unregistered>");
                System.out.println("LAZY_BODY " + id + " pos=" + origin + " biome=" + biome
                    + " result=" + result + " focus=" + before + " -> " + after);
            }
            return;
        }
        PlacementModifier modifier = modifiers.get(modifierIndex);
        modifier.getPositions(context, random, origin)
            .forEach(next -> replayLazy(level, placed, random, next, modifierIndex + 1,
                                        modifiers, context, focus, id, terminalCount));
    }

    private static void replayLazyFeature(
        ServerLevel level, PlacedFeature feature, int featureIndex, int step,
        int cx, int cz, BlockPos focus, String id) {
        BlockPos origin = new BlockPos(cx * 16, level.getMinY(), cz * 16);
        WorldgenRandom random = new WorldgenRandom(
            new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
        long decorationSeed = random.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
        random.setFeatureSeed(decorationSeed, featureIndex, step);
        PlacementContext context = new PlacementContext(
            level, level.getChunkSource().getGenerator(), Optional.of(feature));
        int[] terminalCount = {0};
        replayLazy(level, feature, random, origin, 0, feature.placement(), context,
                   focus, id, terminalCount);
        System.out.println("LAZY_DONE " + id + " terminals=" + terminalCount[0]
            + " focus=" + level.getBlockState(focus));
    }

    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        String worldRoot = System.getenv().getOrDefault("ORACLE_WORLD_ROOT", "/work/stage-world");
        LargeParityOracle.runServer(Path.of(worldRoot), System.getenv("ORACLE_WORLD_ROOT") != null, args, (server, level) -> {
            int cx = envInt("ORACLE_TARGET_X", 50);
            int cz = envInt("ORACLE_TARGET_Z", -50);
            BlockPos focus = new BlockPos(
                envInt("ORACLE_FOCUS_X", 802),
                envInt("ORACLE_FOCUS_Y", 5),
                envInt("ORACLE_FOCUS_Z", -800));
            System.out.println("BIOME_FOCUS " + level.getBiome(focus));
            Registry<PlacedFeature> registry =
                level.registryAccess().lookupOrThrow(Registries.PLACED_FEATURE);
            PlacedFeature basalt = registry.getValue(Identifier.parse("minecraft:basalt_blobs"));
            BlockPos origin = new BlockPos(cx * 16, level.getMinY(), (cz - 1) * 16);
            WorldgenRandom random = new WorldgenRandom(
                new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
            long decorationSeed = random.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
            random.setFeatureSeed(decorationSeed, 0, 7);
            PlacementContext context = new PlacementContext(
                level, level.getChunkSource().getGenerator(), Optional.of(basalt));
            List<BlockPos> positions = List.of(origin);
            for (PlacementModifier modifier : basalt.placement()) {
                positions = positions.stream()
                    .flatMap(pos -> modifier.getPositions(context, random, pos))
                    .toList();
            }
            for (BlockPos position : positions) {
                String biome = level.getBiome(position).unwrapKey()
                    .map(key -> key.identifier().toString()).orElse("<unregistered>");
                if (Math.abs(position.getX() - focus.getX()) <= 12
                        && Math.abs(position.getZ() - focus.getZ()) <= 12) {
                    System.out.println("BASALT_POSITION " + position + " biome=" + biome);
                }
            }
            for (int dz = -1; dz <= 1; dz++) {
                for (int dx = -1; dx <= 1; dx++) {
                    ChunkAccess chunk = level.getChunkSource().getChunk(
                        cx + dx, cz + dz, ChunkStatus.CARVERS, true);
                    BlockState state = chunk.getBlockState(focus);
                    System.out.println("CARVERS " + (cx + dx) + " " + (cz + dz) + " " + state);
                }
            }
            ChunkAccess targetCarvers = level.getChunkSource().getChunk(cx, cz, ChunkStatus.CARVERS, true);
            System.out.println("TARGET_CARVERS " + targetCarvers.getBlockState(focus));
            for (int dz = -1; dz <= 1; dz++) {
                for (int dx = -1; dx <= 1; dx++) {
                    ChunkAccess source = level.getChunkSource().getChunk(cx + dx, cz + dz, ChunkStatus.FEATURES, true);
                    System.out.println("FEATURE_STEP " + (cx + dx) + " " + (cz + dz)
                        + " target=" + level.getBlockState(focus)
                        + " source=" + source.getBlockState(focus));
                }
            }
            java.util.Map<String, BlockState> beforeFeatures = new HashMap<>();
            for (int x = cx * 16; x < cx * 16 + 16; x++) {
                for (int z = cz * 16; z < cz * 16 + 16; z++) {
                    for (int y = level.getMinY(); y <= level.getMaxY(); y++) {
                        beforeFeatures.put(x + "," + y + "," + z,
                            level.getBlockState(new BlockPos(x, y, z)));
                    }
                }
            }
            System.out.println("BEFORE_FEATURES focus=" + level.getBlockState(focus));
            ChunkAccess first = level.getChunkSource().getChunk(cx, cz - 1, ChunkStatus.FEATURES, true);
            ChunkAccess targetFeatures = level.getChunkSource().getChunk(cx, cz, ChunkStatus.FEATURES, true);
            System.out.println("FIRST_FEATURES target=" + targetFeatures.getBlockState(focus)
                + " source_local=" + first.getBlockState(focus));
            int changedBasalt = 0;
            for (int x = cx * 16; x < cx * 16 + 16; x++) {
                for (int z = cz * 16; z < cz * 16 + 16; z++) {
                    for (int y = level.getMinY(); y <= level.getMaxY(); y++) {
                        BlockPos position = new BlockPos(x, y, z);
                        BlockState state = level.getBlockState(position);
                        if (state.toString().contains("minecraft:basalt")
                            && !beforeFeatures.get(x + "," + y + "," + z).equals(state)) {
                            if (changedBasalt++ < 160) System.out.println("FIRST_BASALT " + position + " " + state);
                        }
                    }
                }
            }
            System.out.println("FIRST_BASALT_COUNT " + changedBasalt);
            for (int dz = -1; dz <= 1; dz++) for (int dx = -1; dx <= 1; dx++) {
                ChunkAccess chunk = level.getChunkSource().getChunkNow(cx + dx, cz + dz);
                System.out.println("STATUS " + (cx + dx) + " " + (cz + dz) + " "
                    + (chunk == null ? "null" : chunk.getPersistedStatus()));
            }
            if (System.getenv("STOP_STAGE_PROBE") != null) return;
            String[][] featureIds = {
                {"minecraft:delta", "minecraft:small_basalt_columns", "minecraft:large_basalt_columns"},
                {"minecraft:basalt_blobs", "minecraft:blackstone_blobs", "minecraft:spring_delta",
                 "minecraft:spring_open", "minecraft:patch_fire", "minecraft:patch_soul_fire",
                 "minecraft:glowstone_extra", "minecraft:glowstone", "minecraft:brown_mushroom_nether",
                 "minecraft:red_mushroom_nether", "minecraft:spring_closed_double", "minecraft:spring_closed"},
            };
            for (int stepIndex = 0; stepIndex < featureIds.length; stepIndex++) {
                int step = stepIndex == 0 ? 4 : 7;
                for (int index = 0; index < featureIds[stepIndex].length; index++) {
                    String id = featureIds[stepIndex][index];
                    PlacedFeature feature = registry.getValue(Identifier.parse(id));
                    if (step == 4 && index == 0) {
                        level.setBlock(focus, net.minecraft.world.level.block.Blocks.NETHERRACK.defaultBlockState(), 3);
                        System.out.println("REPLAY_BEFORE " + level.getBlockState(focus) + " feature=" + feature);
                    }
                    replayLazyFeature(level, feature, index, step, cx, cz - 1, focus, id);
                    System.out.println("REPLAY " + step + " " + index + " " + id + " focus=" + level.getBlockState(focus));
                }
            }
        });
    }
}
