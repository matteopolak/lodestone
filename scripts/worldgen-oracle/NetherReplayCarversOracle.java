import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Optional;
import java.util.Set;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderSet;
import net.minecraft.core.registries.Registries;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.FeatureSorter;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.placement.PlacementContext;
import net.minecraft.world.level.levelgen.placement.PlacementModifier;

public final class NetherReplayCarversOracle {
    private static final int CX = 96;
    private static final int CZ = 96;
    private static final BlockPos FOCUS = new BlockPos(1536, 12, 1551);

    private static List<Holder<Biome>> sourceBiomes(ServerLevel level, int cx, int cz) {
        Set<Holder<Biome>> biomes = new LinkedHashSet<>();
        for (int dz = -1; dz <= 1; dz++) {
            for (int dx = -1; dx <= 1; dx++) {
                ChunkAccess chunk = level.getChunkSource().getChunk(cx + dx, cz + dz, ChunkStatus.CARVERS, true);
                for (var section : chunk.getSections()) section.getBiomes().getAll(biomes::add);
            }
        }
        return new ArrayList<>(biomes);
    }

    private static List<FeatureSorter.StepFeatureData> featureOrder(ChunkGenerator generator) {
        return FeatureSorter.buildFeaturesPerStep(
            List.copyOf(generator.getBiomeSource().possibleBiomes()),
            biome -> biome.value().getGenerationSettings().features(),
            true
        );
    }

    private static void replayBlob(
        ServerLevel level, ChunkGenerator generator, PlacedFeature feature,
        WorldgenRandom random, BlockPos origin, PlacementContext context,
        int modifierIndex, BlockPos position, int[] rawIndex, int[] bodyIndex
    ) {
        if (modifierIndex == feature.placement().size()) {
            int before = random.getCount();
            int cursor = position.getY();
            while (cursor > level.getMinY() + 1) {
                if (level.getBlockState(new BlockPos(position.getX(), cursor, position.getZ()))
                        .is(net.minecraft.world.level.block.Blocks.NETHERRACK)) break;
                cursor--;
            }
            System.out.println("BLOB_BODY index=" + bodyIndex[0]++ + " pos=" + position
                + " targetY=" + (cursor > level.getMinY() + 1 ? cursor : -1)
                + " before=" + before);
            boolean result = feature.feature().value().place(
                level, generator, random, position);
            System.out.println("BLOB_BODY_RESULT result=" + result
                + " draws=" + (random.getCount() - before) + " after=" + random.getCount());
            return;
        }
        PlacementModifier modifier = feature.placement().get(modifierIndex);
        modifier.getPositions(context, random, position).forEach(next -> {
            if (modifierIndex == 1) {
                System.out.println("BLOB_RAW_IN_SQUARE index=" + rawIndex[0]++ + " pos=" + next);
            } else if (modifierIndex == 2) {
                System.out.println("BLOB_RAW_HEIGHT index=" + rawIndex[0]++ + " pos=" + next);
            } else if (modifierIndex == 3) {
                System.out.println("BLOB_RAW_BIOME index=" + rawIndex[0]++ + " pos=" + next
                    + " biome=" + level.getBiome(next));
            }
            replayBlob(level, generator, feature, random, origin, context,
                modifierIndex + 1, next, rawIndex, bodyIndex);
        });
    }

    private static void replaySource(ServerLevel level, ChunkGenerator generator,
                                     List<FeatureSorter.StepFeatureData> allFeatures,
                                     int cx, int cz) {
        List<Holder<Biome>> biomes = sourceBiomes(level, cx, cz);
        System.out.println("SOURCE " + cx + " " + cz + " biomes=" + biomes
            + " centerBiome=" + level.getBiome(new BlockPos(cx * 16 + 8, 14, cz * 16 + 8)));
        BlockPos origin = new BlockPos(cx * 16, level.getMinY(), cz * 16);
        WorldgenRandom random = new WorldgenRandom(new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
        long decorationSeed = random.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
        var registry = level.registryAccess().lookupOrThrow(Registries.PLACED_FEATURE);
        for (int step = 0; step < allFeatures.size(); step++) {
            Set<Integer> selected = new java.util.TreeSet<>();
            for (Holder<Biome> biome : biomes) {
                List<HolderSet<PlacedFeature>> settings = biome.value().getGenerationSettings().features();
                if (step >= settings.size()) continue;
                var mapping = allFeatures.get(step).indexMapping();
                settings.get(step).stream().map(Holder::value).forEach(feature -> selected.add(mapping.applyAsInt(feature)));
            }
            for (int index : selected) {
                PlacedFeature feature = allFeatures.get(step).features().get(index);
                String name = registry.getResourceKey(feature).map(Object::toString).orElse(feature.toString());
                BlockState before = level.getBlockState(FOCUS);
                random.setFeatureSeed(decorationSeed, index, step);
                if (name.contains("minecraft:basalt_blobs")) {
                    PlacementContext context = new PlacementContext(
                        level, generator, Optional.of(feature));
                    replayBlob(level, generator, feature, random, origin, context,
                        0, origin, new int[] {0}, new int[] {0});
                } else {
                    feature.placeWithBiomeCheck(level, generator, random, origin);
                }
                BlockState after = level.getBlockState(FOCUS);
                System.out.println("FEATURE " + cx + " " + cz + " step=" + step + " index=" + index
                    + " name=" + name + " focus=" + before + " -> " + after);
            }
        }
    }

    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        LargeParityOracle.runServer(Path.of("/work/replay-carvers"), false, args, (server, level) -> {
            if ("1".equals(System.getenv("REPLAY_FULL_ONLY"))) {
                LargeParityOracle.loadBatch(server, level, args, List.of(new ChunkPos(CX, CZ)), false, new ArrayList<>());
                System.out.println("FULL " + level.getBlockState(FOCUS) + " biome=" + level.getBiome(FOCUS));
                return;
            }
            if ("1".equals(System.getenv("REPLAY_ADMISSIONS"))) {
                for (int z = CZ; z <= CZ + 4; z++) {
                    ChunkPos pos = new ChunkPos(CX, z);
                    LargeParityOracle.loadBatch(server, level, args, List.of(pos), false, new ArrayList<>());
                    System.out.println("ADMISSION " + pos + " focus=" + level.getBlockState(FOCUS));
                }
                return;
            }
            if ("1".equals(System.getenv("REPLAY_FIRST_TILE"))) {
                for (int z = CZ - 1; z <= CZ; z++) {
                    for (int x = CX - 1; x <= CX; x++) {
                        ChunkPos pos = new ChunkPos(x, z);
                        LargeParityOracle.loadBatch(server, level, args, List.of(pos), false, new ArrayList<>());
                        System.out.println("TILE " + pos + " focus=" + level.getBlockState(FOCUS));
                    }
                }
                return;
            }
            List<ChunkPos> loaded = new ArrayList<>();
            for (int dz = -2; dz <= 2; dz++) {
                for (int dx = -2; dx <= 2; dx++) loaded.add(new ChunkPos(CX + dx, CZ + dz));
            }
            server.submit(() -> {
                for (ChunkPos pos : loaded) level.getChunkSource().getChunk(pos.x(), pos.z(), ChunkStatus.CARVERS, true);
            }).join();
            ChunkGenerator generator = level.getChunkSource().getGenerator();
            List<FeatureSorter.StepFeatureData> features = featureOrder(generator);
            if ("1".equals(System.getenv("REPLAY_SINGLE_SOURCE"))) {
                level.getChunkSource().getChunk(CX, CZ - 1, ChunkStatus.CARVERS, true);
                System.out.println("SINGLE_BASE " + level.getBlockState(FOCUS));
                replaySource(level, generator, features, CX, CZ - 1);
                System.out.println("SINGLE_FINAL " + level.getBlockState(FOCUS));
                return;
            }
            System.out.println("BASE " + level.getBlockState(FOCUS) + " biome=" + level.getBiome(FOCUS));
            if ("1".equals(System.getenv("REPLAY_BASE_ONLY"))) return;
            if ("1".equals(System.getenv("REPLAY_BIOMES_ONLY"))) {
                for (int dz = -1; dz <= 1; dz++) {
                    for (int dx = -1; dx <= 1; dx++) {
                        int sourceX = CX + dx;
                        int sourceZ = CZ + dz;
                        List<Holder<Biome>> biomes = sourceBiomes(level, sourceX, sourceZ);
                        System.out.println("BIOMES " + sourceX + " " + sourceZ + " " + biomes
                            + " centerBiome=" + level.getBiome(new BlockPos(sourceX * 16 + 8, 14, sourceZ * 16 + 8)));
                    }
                }
                return;
            }
            level.setBlock(FOCUS, Blocks.NETHERRACK.defaultBlockState(), 3);
            for (int dz = -1; dz <= 1; dz++) {
                for (int dx = -1; dx <= 1; dx++) {
                    replaySource(level, generator, features, CX + dx, CZ + dz);
                }
            }
            System.out.println("FINAL " + level.getBlockState(FOCUS));
        });
    }
}
