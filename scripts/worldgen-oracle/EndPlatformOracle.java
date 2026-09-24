// Independent fixed-platform oracle.  It invokes the bundled server's feature
// implementation through a minimal ServerLevelAccessor proxy; Lodestone code
// does not participate in the emitted block states.
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import java.util.Map;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ServerLevelAccessor;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.feature.EndPlatformFeature;

public final class EndPlatformOracle {
    static String canonical(BlockState state) {
        return BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
    }

    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        Map<String, String> blocks = new TreeMap<>();
        InvocationHandler handler = (proxy, method, a) -> {
            if (method.getName().equals("getBlockState")) {
                return net.minecraft.world.level.block.Blocks.END_STONE.defaultBlockState();
            }
            if (method.getName().equals("setBlock")) {
                BlockPos p = (BlockPos)a[0];
                BlockState state = (BlockState)a[1];
                blocks.put(p.getX() + "," + p.getY() + "," + p.getZ(), canonical(state));
                return Boolean.TRUE;
            }
            if (method.getName().equals("getMinY")) return 0;
            if (method.getName().equals("getMaxY")) return 127;
            if (method.getName().equals("getMinBuildHeight")) return 0;
            if (method.getName().equals("getMaxBuildHeight")) return 128;
            if (method.getName().equals("isOutsideBuildHeight")) return Boolean.FALSE;
            Class<?> result = method.getReturnType();
            if (result == boolean.class) return Boolean.FALSE;
            if (result == int.class) return 0;
            if (result == long.class) return 0L;
            if (result.isPrimitive()) return 0;
            return null;
        };
        ServerLevelAccessor level = (ServerLevelAccessor)Proxy.newProxyInstance(
            EndPlatformOracle.class.getClassLoader(), new Class[]{ServerLevelAccessor.class}, handler);
        EndPlatformFeature.createEndPlatform(level, new BlockPos(100, 49, 0), false);
        for (Map.Entry<String, String> entry : blocks.entrySet()) {
            System.out.println("block " + entry.getKey() + " " + entry.getValue());
        }
    }
}
