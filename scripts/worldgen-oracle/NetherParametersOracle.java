// Independent JVM oracle for the NETHER multi-noise biome parameter list
// (worldgen-rewrite plan, phase NE-data).
//
// Why an oracle rather than a file copy: the jar's
// `data/minecraft/worldgen/multi_noise_biome_source_parameter_list/nether.json`
// is **37 bytes** — literally `{"preset": "minecraft:nether"}`. The real
// parameter table is Java-hardcoded in
// `MultiNoiseBiomeSourceParameterList.Preset.NETHER`
// (`MultiNoiseBiomeSourceParameterList.java:51-67`), and its DIRECT_CODEC only
// ever serialises the preset id (`:24-30`). So there is nothing to copy, and
// hand-transcribing the five `Climate.parameters(...)` calls would be exactly
// the sort of hand-written table that has been wrong five times in this repo
// recently. This dumps the *resolved* table instead.
//
// `MultiNoiseBiomeSourceParameterList.knownPresets()` is public and needs **no**
// registry resolution: the values are plain `ResourceKey<Biome>`, identity-
// mapped, never resolved to a `Holder`. Same property `BiomeOracle.java` relies
// on for the OVERWORLD table, and the reason neither oracle has to transliterate
// a biome builder — only read its output.
//
// Emits `biome_parameters/nether.json` on stdout in the exact 14-column shape
// `lodestone_worldgen::biome::parse_table` consumes (13 quantized longs + the
// biome id), byte-format-identical to the committed `overworld.json`: an opening
// `[`, one row per line, comma-separated, closing `]`. The quantized longs are
// dumped raw as `Climate.Parameter` stores them (factor 10000, `Climate.java:27`)
// so the Rust side never round-trips through a second float parse.
//
// No Mojang source is copied; this only drives the compiled classes.
//
//   usage: ./run.sh NetherParametersOracle > /tmp/nether_params.json
import java.util.List;
import java.util.Map;

import com.mojang.datafixers.util.Pair;

import net.minecraft.SharedConstants;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList;

public final class NetherParametersOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Map<MultiNoiseBiomeSourceParameterList.Preset, Climate.ParameterList<ResourceKey<Biome>>> presets =
            MultiNoiseBiomeSourceParameterList.knownPresets();

        // Fail loudly rather than emitting an empty array: an empty table would
        // parse fine downstream and silently assign one biome everywhere.
        Climate.ParameterList<ResourceKey<Biome>> table =
            presets.get(MultiNoiseBiomeSourceParameterList.Preset.NETHER);
        if (table == null) {
            throw new IllegalStateException("no NETHER preset in knownPresets(): " + presets.keySet());
        }
        List<Pair<Climate.ParameterPoint, ResourceKey<Biome>>> values = table.values();
        if (values.isEmpty()) {
            throw new IllegalStateException("NETHER parameter table is empty");
        }

        // Row shape, matching `parse_table`'s indices exactly:
        //   [t.min,t.max, h.min,h.max, c.min,c.max, e.min,e.max,
        //    d.min,d.max, w.min,w.max, offset, "<biome id>"]
        StringBuilder sb = new StringBuilder();
        sb.append("[\n");
        for (int i = 0; i < values.size(); i++) {
            Climate.ParameterPoint p = values.get(i).getFirst();
            String biome = values.get(i).getSecond().identifier().toString();
            sb.append('[')
              .append(p.temperature().min()).append(',').append(p.temperature().max()).append(',')
              .append(p.humidity().min()).append(',').append(p.humidity().max()).append(',')
              .append(p.continentalness().min()).append(',').append(p.continentalness().max()).append(',')
              .append(p.erosion().min()).append(',').append(p.erosion().max()).append(',')
              .append(p.depth().min()).append(',').append(p.depth().max()).append(',')
              .append(p.weirdness().min()).append(',').append(p.weirdness().max()).append(',')
              .append(p.offset()).append(',')
              .append('"').append(biome).append('"')
              .append(']');
            if (i + 1 < values.size()) {
                sb.append(',');
            }
            sb.append('\n');
        }
        sb.append("]\n");

        // stdout carries nothing but the JSON. Do NOT write the row count to
        // System.err as a "keeps stdout clean" trick: `Bootstrap.bootStrap()`
        // installs log4j over System.err, and the redirected line comes back out
        // on **stdout** as `[..] [main/INFO]: [STDERR]: ...`, corrupting the
        // document. Measured, not assumed — the first run of this oracle did
        // exactly that. The row count is asserted on the Rust side instead.
        System.out.print(sb);
    }
}
