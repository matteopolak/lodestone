package bench.mixin;

import bench.Bench;
import java.nio.file.Path;
import net.minecraft.client.Minecraft;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(value = Minecraft.class, remap = false)
abstract class ClientProbeMixin {
    @Inject(method = "run()V", at = @At(value = "INVOKE", target =
        "Lnet/minecraft/client/Minecraft;constructProfiler(ZLnet/minecraft/util/profiling/SingleTickProfiler;)Lnet/minecraft/util/profiling/ProfilerFiller;"), require = 1, allow = 1)
    private void bench$loop(CallbackInfo ci) { Bench.beforeLoop((Minecraft)(Object)this); }

    @Inject(method = "exitWorldAndClose()V", at = @At("HEAD"), require = 1, allow = 1)
    private void bench$normalExit(CallbackInfo ci) { Bench.normalExit((Minecraft)(Object)this); }

    @Inject(method = "archiveProfilingReport(Lnet/minecraft/SystemReport;Ljava/util/List;)Ljava/nio/file/Path;",
        at = @At("RETURN"), require = 1, allow = 1)
    private void bench$archive(CallbackInfoReturnable<Path> cir) { Bench.archiveReady(cir.getReturnValue()); }
}
