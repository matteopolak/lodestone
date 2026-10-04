package bench;

import bench.mixin.ManagerProbeAccessor;
import bench.mixin.RendererProbeAccessor;
import net.caffeinemc.mods.sodium.client.render.SodiumWorldRenderer;

/** Sodium-only readiness counters; never loaded unless the sodium mod is present. */
final class SodiumProbe {
    private SodiumProbe() {}

    static double[] counts() {
        var renderer = SodiumWorldRenderer.instanceNullable();
        if (renderer == null) return null;
        var manager = ((RendererProbeAccessor) renderer).bench$manager();
        if (manager == null) return null;
        return new double[]{manager.getVisibleChunkCount(), manager.getTotalSections(),
            manager.getBuilder().getScheduledJobCount(), manager.getBuilder().getBusyThreadCount(),
            ((ManagerProbeAccessor) manager).bench$buildResults().size(), manager.needsUpdate() ? 1 : 0};
    }
}
