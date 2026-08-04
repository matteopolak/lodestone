// Independent JVM oracle for overworld multi-noise biome assignment (issue #405).
//
// Two independent things this dumps, both load-bearing for the Rust gate:
//
//  1. "table" (default, no args): the resolved
//     `MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD` parameter table
//     (7594 rows, not the ~700 the plan estimated — measured, not assumed) —
//     `MultiNoiseBiomeSourceParameterList.knownPresets()` is public and returns
//     it with **zero bootstrap/registry dependency** (the biome "values" are
//     plain `ResourceKey<Biome>` identity-mapped, never resolved to a `Holder`),
//     confirming the plan's finding that `OverworldBiomeBuilder` (1124 lines)
//     never needs transliterating — only its *output* is needed, and that
//     output is reachable directly. Each row is the 7 quantized `long`
//     [min,max] spans (temperature/humidity/continentalness/erosion/depth/
//     weirdness) plus a single `offset` long, exactly as `Climate.Parameter`
//     stores them internally — dumped as the raw longs, not re-derived floats,
//     so the Rust side never round-trips through a second float parse.
//
//  2. "sample <seed> <x1> <y1> <z1> <x2> <y2> <z2> ...": real seeded climate
//     samples. Boots the actual 26.2 registries, builds `RandomState` for the
//     overworld noise settings at `seed` (identical to every other oracle in
//     this directory), and for each `(x, y, z)` triple evaluates the six
//     climate channels at that exact block position — `y` is caller-supplied
//     (not fixed) because an early y=0 probe showed `depth`'s
//     `y_clamped_gradient` term makes y=0 read as "deep underground" almost
//     everywhere (mostly cave/deep-ocean biomes came back), so this port
//     samples at each column's own **terrain surface height**, not a global
//     constant — see `lodestone-worldgen/src/biome.rs`'s module doc for the
//     resulting convention. Dumps the quantized `Climate.TargetPoint` plus the
//     biome vanilla resolves it to via **both** `findValueBruteForce` (the
//     un-optimized reference next to the RTree, `Climate.java:182`) and
//     `findValue` (the real indexed search) — printing both lets the Rust
//     test confirm they agree before trusting either as ground truth.
//
// No Mojang source is copied; this only drives the compiled classes and reads
// their output.
import java.util.List;
import java.util.Map;
import com.mojang.datafixers.util.Pair;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

public final class BiomeOracle {
    static final StringBuilder sb = new StringBuilder();

    static void dumpTable() {
        Map<MultiNoiseBiomeSourceParameterList.Preset, Climate.ParameterList<ResourceKey<Biome>>> presets =
            MultiNoiseBiomeSourceParameterList.knownPresets();
        Climate.ParameterList<ResourceKey<Biome>> table =
            presets.get(MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD);
        List<Pair<Climate.ParameterPoint, ResourceKey<Biome>>> values = table.values();
        sb.append("table.count ").append(values.size()).append('\n');
        int i = 0;
        for (Pair<Climate.ParameterPoint, ResourceKey<Biome>> pair : values) {
            Climate.ParameterPoint p = pair.getFirst();
            String biome = pair.getSecond().identifier().toString();
            sb.append("row.").append(i).append(' ')
              .append(biome).append(' ')
              .append(p.temperature().min()).append(',').append(p.temperature().max()).append(' ')
              .append(p.humidity().min()).append(',').append(p.humidity().max()).append(' ')
              .append(p.continentalness().min()).append(',').append(p.continentalness().max()).append(' ')
              .append(p.erosion().min()).append(',').append(p.erosion().max()).append(' ')
              .append(p.depth().min()).append(',').append(p.depth().max()).append(' ')
              .append(p.weirdness().min()).append(',').append(p.weirdness().max()).append(' ')
              .append(p.offset())
              .append('\n');
            i++;
        }
    }

    static void dumpSamples(String[] toks) {
        long seed = Long.parseLong(toks[1]);
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);

        Map<MultiNoiseBiomeSourceParameterList.Preset, Climate.ParameterList<ResourceKey<Biome>>> presets =
            MultiNoiseBiomeSourceParameterList.knownPresets();
        Climate.ParameterList<ResourceKey<Biome>> table =
            presets.get(MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD);

        Climate.Sampler sampler = rs.sampler();

        // Triples: x y z (y is a block Y, quart-aligned or not — sample() does
        // its own QuartPos.fromBlock conversion, matching whatever y the Rust
        // side picks as its per-column depth reference).
        for (int i = 2; i + 2 < toks.length; i += 3) {
            int x = Integer.parseInt(toks[i]);
            int y = Integer.parseInt(toks[i + 1]);
            int z = Integer.parseInt(toks[i + 2]);
            Climate.TargetPoint target = sampler.sample(x >> 2, y >> 2, z >> 2);
            ResourceKey<Biome> bruteForce = table.findValueBruteForce(target);
            ResourceKey<Biome> indexed = table.findValue(target);
            sb.append("sample.").append(x).append(',').append(y).append(',').append(z)
              .append(" seed=").append(seed)
              .append(" target=").append(target.temperature()).append(',')
              .append(target.humidity()).append(',')
              .append(target.continentalness()).append(',')
              .append(target.erosion()).append(',')
              .append(target.depth()).append(',')
              .append(target.weirdness())
              .append(" brute=").append(bruteForce.identifier())
              .append(" indexed=").append(indexed.identifier())
              .append('\n');
        }
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        String rawArgs = System.getenv("ORACLE_ARGS");
        if (rawArgs == null) rawArgs = "";
        rawArgs = rawArgs.trim();
        String[] toks = rawArgs.isBlank() ? new String[] {"table"} : rawArgs.split("\\s+");

        if (toks[0].equals("sample")) {
            dumpSamples(toks);
        } else {
            dumpTable();
        }

        System.out.print(sb);
    }
}
