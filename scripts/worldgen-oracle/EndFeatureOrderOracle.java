// Independent End decoration-order oracle. It asks the bundled server's
// FeatureSorter for the five End biomes' flattened feature order and emits the
// raw per-step indices used to seed each feature. No Lodestone code participates.
import java.util.List;
import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.server.Bootstrap;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.FeatureSorter;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;

public final class EndFeatureOrderOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        HolderLookup.RegistryLookup<Biome> biomes = provider.lookupOrThrow(Registries.BIOME);
        List<Holder<Biome>> sources = List.of(
            biomes.getOrThrow(net.minecraft.world.level.biome.Biomes.THE_END),
            biomes.getOrThrow(net.minecraft.world.level.biome.Biomes.END_HIGHLANDS),
            biomes.getOrThrow(net.minecraft.world.level.biome.Biomes.END_MIDLANDS),
            biomes.getOrThrow(net.minecraft.world.level.biome.Biomes.SMALL_END_ISLANDS),
            biomes.getOrThrow(net.minecraft.world.level.biome.Biomes.END_BARRENS)
        );
        List<FeatureSorter.StepFeatureData> sorted = FeatureSorter.buildFeaturesPerStep(
            sources,
            biome -> biome.value().getGenerationSettings().features(),
            true
        );
        HolderLookup.RegistryLookup<PlacedFeature> registry = provider.lookupOrThrow(Registries.PLACED_FEATURE);
        for (int step = 0; step < sorted.size(); step++) {
            List<PlacedFeature> features = sorted.get(step).features();
            for (int index = 0; index < features.size(); index++) {
                PlacedFeature feature = features.get(index);
                String id = registry.listElements()
                    .filter(holder -> holder.value() == feature)
                    .findFirst()
                    .orElseThrow()
                    .key()
                    .identifier()
                    .toString();
                System.out.println("feature " + step + " " + index + " " + id);
            }
        }
    }
}
