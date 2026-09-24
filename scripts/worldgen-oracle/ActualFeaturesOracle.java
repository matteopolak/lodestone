import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import com.mojang.datafixers.util.Pair;

import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.FeatureSorter;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.feature.ConfiguredFeature;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;

public final class ActualFeaturesOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        Map<MultiNoiseBiomeSourceParameterList.Preset, Climate.ParameterList<ResourceKey<Biome>>> presets =
            MultiNoiseBiomeSourceParameterList.knownPresets();
        Climate.ParameterList<ResourceKey<Biome>> keyTable =
            presets.get(MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD);
        HolderLookup.RegistryLookup<Biome> biomes = provider.lookupOrThrow(Registries.BIOME);
        List<Pair<Climate.ParameterPoint, Holder<Biome>>> resolved = new ArrayList<>();
        for (Pair<Climate.ParameterPoint, ResourceKey<Biome>> p : keyTable.values()) {
            resolved.add(Pair.of(p.getFirst(), biomes.getOrThrow(p.getSecond())));
        }
        MultiNoiseBiomeSource biomeSource = MultiNoiseBiomeSource.createFromList(
            new Climate.ParameterList<>(resolved));
        HolderLookup.RegistryLookup<NoiseGeneratorSettings> settings =
            provider.lookupOrThrow(Registries.NOISE_SETTINGS);
        NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(
            biomeSource, settings.getOrThrow(NoiseGeneratorSettings.OVERWORLD));

        List<Holder<Biome>> possibleBiomes = List.copyOf(generator.getBiomeSource().possibleBiomes());
        System.out.println("possible.count " + possibleBiomes.size());
        for (int i = 0; i < possibleBiomes.size(); i++) {
            System.out.println("possible." + i + " " + possibleBiomes.get(i).unwrapKey().orElseThrow().identifier());
        }
        List<FeatureSorter.StepFeatureData> perStep = FeatureSorter.buildFeaturesPerStep(
            possibleBiomes, b -> b.value().getGenerationSettings().features(), true);
        HolderLookup.RegistryLookup<PlacedFeature> placed = provider.lookupOrThrow(Registries.PLACED_FEATURE);
        Map<PlacedFeature, String> ids = new java.util.IdentityHashMap<>();
        placed.listElements().forEach(ref -> ids.put(ref.value(), ref.key().identifier().toString()));
        int step = 6;
        List<PlacedFeature> features = perStep.get(step).features();
        System.out.println("step.count " + features.size());
        for (int i = 0; i < features.size(); i++) {
            ConfiguredFeature<?, ?> configured = features.get(i).feature().value();
            System.out.println("step." + i + " " + ids.get(features.get(i)) + " "
                + configured.feature().getClass().getSimpleName());
        }
    }
}
