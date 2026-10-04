package bench.mixin;

import bench.Bench;
import com.mojang.renderpearl.api.device.GpuSurface;
import com.mojang.renderpearl.frontend.FrontendGpuSurface;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(value = FrontendGpuSurface.class, remap = false)
abstract class SurfaceProbeMixin {
    @Inject(method = "present()V", at = @At("RETURN"), require = 1, allow = 1)
    private void bench$present(CallbackInfo ci) { Bench.afterPresent((GpuSurface)(Object)this); }
}
