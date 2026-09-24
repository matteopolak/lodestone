import java.nio.file.Path;
import java.util.List;
import java.util.Optional;
import java.util.stream.Stream;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.placement.PlacementContext;
import net.minecraft.world.level.levelgen.placement.PlacementModifier;

/** Temporary placement-stream capture for one Nether basalt blob source. */
public final class NetherBlobPlacementOracle {
    private static void traceLazy(
        List<PlacementModifier> modifiers,
        int modifierIndex,
        BlockPos position,
        PlacementContext context,
        WorldgenRandom random,
        PlacedFeature feature,
        int[] rawIndex,
        int[] acceptedIndex
    ) {
        if (modifierIndex == modifiers.size()) {
            System.out.println("JAVA raw terminal=" + acceptedIndex[0]++ + " "
                + position.getX() + " " + position.getY() + " " + position.getZ());
            int before = random.getCount();
            BlockPos focus = new BlockPos(
                Integer.parseInt(System.getenv().getOrDefault("ORACLE_FOCUS_X", "-794")),
                Integer.parseInt(System.getenv().getOrDefault("ORACLE_FOCUS_Y", "14")),
                Integer.parseInt(System.getenv().getOrDefault("ORACLE_FOCUS_Z", "-800")));
            var beforeState = context.getLevel().getBlockState(focus);
            feature.feature().value().place(
                context.getLevel(), context.generator(), random, position);
            var afterState = context.getLevel().getBlockState(focus);
            System.out.println("JAVA raw body-draws=" + (random.getCount() - before)
                + " total=" + random.getCount() + " focus=" + beforeState + " -> " + afterState);
            return;
        }
        PlacementModifier modifier = modifiers.get(modifierIndex);
        modifier.getPositions(context, random, position).forEach(next -> {
            if (modifierIndex == 1) {
                System.out.println("JAVA raw in_square=" + rawIndex[0]++ + " "
                    + next.getX() + " " + next.getY() + " " + next.getZ());
            } else if (modifierIndex == 2) {
                System.out.println("JAVA raw height=" + rawIndex[0]++ + " "
                    + next.getX() + " " + next.getY() + " " + next.getZ());
            } else if (modifierIndex == 3) {
                System.out.println("JAVA raw final=" + rawIndex[0]++ + " "
                    + next.getX() + " " + next.getY() + " " + next.getZ()
                    + " biome=" + context.getLevel().getBiome(next).getRegisteredName());
            }
            traceLazy(modifiers, modifierIndex + 1, next, context, random, feature, rawIndex, acceptedIndex);
        });
    }

    public static void main(String[] ignored) throws Exception {
        LargeParityOracle.Args args = new LargeParityOracle.Args();
        args.dimension = LargeParityOracle.NETHER;
        args.explicitDimension = true;
        args.lightFreeV7 = true;
        LargeParityOracle.runServer(Path.of("/work/blob-placement"), false, args, (server, level) -> {
            int sourceX = Integer.parseInt(System.getenv().getOrDefault("ORACLE_SOURCE_X", "-50"));
            int sourceZ = Integer.parseInt(System.getenv().getOrDefault("ORACLE_SOURCE_Z", "-51"));
            var carvers = level.getChunkSource().getChunk(sourceX, sourceZ, ChunkStatus.CARVERS, true);
            var targetChunk = level.getChunkSource().getChunk(sourceX, sourceZ, ChunkStatus.CARVERS, true);
            Registry<PlacedFeature> registry = level.registryAccess().lookupOrThrow(Registries.PLACED_FEATURE);
            String featureId = System.getenv().getOrDefault("ORACLE_PLACED_FEATURE", "minecraft:basalt_blobs");
            int featureIndex = Integer.parseInt(System.getenv().getOrDefault("ORACLE_FEATURE_INDEX", "0"));
            int featureStep = Integer.parseInt(System.getenv().getOrDefault("ORACLE_FEATURE_STEP", "7"));
            PlacedFeature feature = registry.getValue(Identifier.parse(featureId));
            if (feature == null) throw new IllegalStateException("missing basalt feature");
            BlockPos origin = new BlockPos(sourceX * 16, level.getMinY(), sourceZ * 16);
            BlockPos focus = new BlockPos(
                Integer.parseInt(System.getenv().getOrDefault("ORACLE_FOCUS_X", "-794")),
                Integer.parseInt(System.getenv().getOrDefault("ORACLE_FOCUS_Y", "14")),
                Integer.parseInt(System.getenv().getOrDefault("ORACLE_FOCUS_Z", "-800")));
            System.out.println("JAVA initial focus=" + targetChunk.getBlockState(focus)
                + " carvers=" + carvers.getBlockState(focus)
                + " target=" + targetChunk.getBlockState(focus)
                + " status=" + carvers.getPersistedStatus());
            String[] preIds = {
                "minecraft:delta", "minecraft:small_basalt_columns", "minecraft:large_basalt_columns"
            };
            for (int preIndex = 0; preIndex < preIds.length; preIndex++) {
                PlacedFeature pre = registry.getValue(Identifier.parse(preIds[preIndex]));
                WorldgenRandom preRandom = new WorldgenRandom(
                    new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
                long preDecoration = preRandom.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
                preRandom.setFeatureSeed(preDecoration, preIndex, 4);
                boolean placed = pre.placeWithBiomeCheck(
                    level, level.getChunkSource().getGenerator(), preRandom, origin);
                System.out.println("JAVA pre=" + preIds[preIndex] + " placed=" + placed
                    + " focus=" + targetChunk.getBlockState(focus)
                    + " target=" + targetChunk.getBlockState(focus));
            }
            WorldgenRandom random = new WorldgenRandom(
                new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
            long decorationSeed = random.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
            random.setFeatureSeed(decorationSeed, featureIndex, featureStep);
            PlacementContext context = new PlacementContext(
                level, level.getChunkSource().getGenerator(), Optional.of(feature));
            List<BlockPos> positions = List.of(origin);
            System.out.println("JAVA seed=" + level.getSeed() + " source=" + sourceX + "," + sourceZ
                + " origin=" + origin + " decorationSeed=" + decorationSeed);
            for (int modifierIndex = 0; modifierIndex < feature.placement().size(); modifierIndex++) {
                PlacementModifier modifier = feature.placement().get(modifierIndex);
                positions = positions.stream()
                    .flatMap(pos -> modifier.getPositions(context, random, pos))
                    .toList();
                System.out.println("JAVA modifier=" + modifierIndex + " type="
                    + modifier.getClass().getSimpleName() + " count=" + positions.size());
                if (modifierIndex == 2) {
                    for (int i = 0; i < positions.size(); i++) {
                        BlockPos pos = positions.get(i);
                        System.out.println("JAVA height=" + i + " " + pos.getX() + " "
                            + pos.getY() + " " + pos.getZ());
                    }
                }
                if (modifierIndex == feature.placement().size() - 1) {
                    for (int i = 0; i < positions.size(); i++) {
                        BlockPos pos = positions.get(i);
                        System.out.println("JAVA terminal=" + i + " " + pos.getX() + " "
                            + pos.getY() + " " + pos.getZ());
                    }
                }
            }
            WorldgenRandom lazyRandom = new WorldgenRandom(
                new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
            long lazyDecorationSeed = lazyRandom.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
            lazyRandom.setFeatureSeed(lazyDecorationSeed, featureIndex, featureStep);
            Stream<BlockPos> lazyPositions = Stream.of(origin);
            for (PlacementModifier modifier : feature.placement()) {
                lazyPositions = lazyPositions.flatMap(pos -> modifier.getPositions(context, lazyRandom, pos));
            }
            final int[] lazyIndex = {0};
            lazyPositions.forEach(pos -> {
                System.out.println("JAVA lazy terminal=" + lazyIndex[0]++ + " " + pos.getX() + " "
                    + pos.getY() + " " + pos.getZ());
            });
            WorldgenRandom rawRandom = new WorldgenRandom(
                new XoroshiroRandomSource(RandomSupport.generateUniqueSeed()));
            long rawDecorationSeed = rawRandom.setDecorationSeed(level.getSeed(), origin.getX(), origin.getZ());
            rawRandom.setFeatureSeed(rawDecorationSeed, featureIndex, featureStep);
            System.out.println("JAVA raw traversal");
            traceLazy(feature.placement(), 0, origin, context, rawRandom, feature, new int[] {0}, new int[] {0});
        });
    }
}
