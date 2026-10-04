// Prints the real server's two multi-noise parameter presets (overworld, nether) as
// JSON: [[tmin,tmax,hmin,hmax,cmin,cmax,emin,emax,dmin,dmax,wmin,wmax,offset,"biome"],...],
// quantized values, in the list's own order (which seeds the search tree).
//   args: <preset>
import com.mojang.datafixers.util.Pair;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.biome.*;

public final class ClimateListOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), true);

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        for (var e : MultiNoiseBiomeSourceParameterList.knownPresets().entrySet()) {
            if (!e.getKey().id().getPath().equals(args[0])) continue;
            StringBuilder sb = new StringBuilder("[\n");
            boolean first = true;
            for (Pair<Climate.ParameterPoint, ResourceKey<Biome>> p : e.getValue().values()) {
                Climate.ParameterPoint pt = p.getFirst();
                if (!first) sb.append(",\n");
                first = false;
                sb.append('[');
                for (Climate.Parameter q : List.of(pt.temperature(), pt.humidity(), pt.continentalness(), pt.erosion(), pt.depth(), pt.weirdness()))
                    sb.append(q.min()).append(',').append(q.max()).append(',');
                sb.append(pt.offset()).append(",\"").append(p.getSecond().identifier()).append("\"]");
            }
            OUT.println(sb.append("\n]"));
        }
    }
}
