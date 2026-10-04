package bench.mixin;

import java.util.concurrent.ConcurrentLinkedDeque;
import net.caffeinemc.mods.sodium.client.render.chunk.RenderSectionManager;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;

@Mixin(value = RenderSectionManager.class, remap = false)
public interface ManagerProbeAccessor {
    @Accessor("buildResults") ConcurrentLinkedDeque<?> bench$buildResults();
}
