// Dumps the 26.3 block-state table the feature engine runs on: every block with its
// property domains, and for every state a 64-bit fact word (see the layout below).
//
//   output: one `block <name> <firstStateId> <defaultOffset> <prop=v,v,...>...` line per block,
//   followed by one `facts` line holding that block's per-state words as hex with `*N`
//   run-lengths. The states of a block are the cartesian product of its properties sorted by
//   name, last property fastest; the oracle asserts that against the registry's own order.
//
// Fact word: bit0 air, 1 replaceable, 2 solid, 3 canOcclude, 4 solidRender, 5 collisionFullBlock,
// 6-7 fluid (0 empty, 1 water, 2 lava), 8 fluid source, 9 fluid falling, 10-13 fluid amount,
// 14-19 full-sturdy faces (DOWN, UP, NORTH, SOUTH, WEST, EAST), 20-25 center-sturdy, 26-31
// rigid-sturdy, 32-35 light emission, 36-39 light dampening, 40 propagatesSkylightDown,
// 41 has block entity, 42 ignitedByLava, 43 liquid (the block's own liquid flag), 44 the collision
// shape's up face is a full square.
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.material.*;

public final class BlockFactsOracle263 {
    static final java.io.PrintStream OUT = new java.io.PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.out), false);

    static long facts(BlockState s) {
        long w = 0;
        if (s.isAir()) w |= 1L;
        if (s.canBeReplaced()) w |= 1L << 1;
        if (s.isSolid()) w |= 1L << 2;
        if (s.canOcclude()) w |= 1L << 3;
        if (s.isSolidRender()) w |= 1L << 4;
        if (s.isCollisionShapeFullBlock(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)) w |= 1L << 5;
        FluidState f = s.getFluidState();
        if (!f.isEmpty()) {
            long kind = f.is(Fluids.WATER) || f.is(Fluids.FLOWING_WATER) ? 1 : f.is(Fluids.LAVA) || f.is(Fluids.FLOWING_LAVA) ? 2 : 3;
            w |= (kind & 3) << 6;
            if (f.isSource()) w |= 1L << 8;
            if (f.getValue(FlowingFluid.FALLING)) w |= 1L << 9;
            w |= ((long) f.getAmount() & 15) << 10;
        }
        Direction[] dirs = Direction.values();
        SupportType[] types = {SupportType.FULL, SupportType.CENTER, SupportType.RIGID};
        for (int t = 0; t < 3; t++)
            for (int d = 0; d < 6; d++)
                if (s.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, dirs[d], types[t])) w |= 1L << (14 + t * 6 + d);
        w |= ((long) s.getLightEmission() & 15) << 32;
        w |= ((long) s.getLightDampening() & 15) << 36;
        if (s.propagatesSkylightDown()) w |= 1L << 40;
        if (s.hasBlockEntity()) w |= 1L << 41;
        if (s.ignitedByLava()) w |= 1L << 42;
        if (s.liquid()) w |= 1L << 43;
        if (Block.isFaceFull(s.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO), Direction.UP)) w |= 1L << 44;
        return w;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        int expectedId = 0;
        for (Block block : BuiltInRegistries.BLOCK) {
            List<Property<?>> props = new ArrayList<>(block.getStateDefinition().getProperties());
            props.sort(Comparator.comparing(Property::getName));
            StringBuilder sb = new StringBuilder("block " + BuiltInRegistries.BLOCK.getKey(block));
            List<BlockState> states = block.getStateDefinition().getPossibleStates();
            int first = Block.BLOCK_STATE_REGISTRY.getId(states.get(0));
            if (first != expectedId) throw new IllegalStateException("non-contiguous state ids at " + block);
            expectedId += states.size();
            int def = states.indexOf(block.defaultBlockState());
            sb.append(' ').append(first).append(' ').append(def);
            for (Property<?> p : props) {
                sb.append(' ').append(p.getName()).append('=');
                StringJoiner j = new StringJoiner(",");
                for (Object v : p.getPossibleValues()) j.add(valueName(p, v));
                sb.append(j);
            }
            OUT.println(sb);
            // verify mixed-radix layout (last sorted property fastest)
            for (int i = 0; i < states.size(); i++) {
                int rem = i;
                for (int pi = props.size() - 1; pi >= 0; pi--) {
                    Property<?> p = props.get(pi);
                    List<?> vals = new ArrayList<>(p.getPossibleValues());
                    Object want = vals.get(rem % vals.size());
                    rem /= vals.size();
                    Object got = states.get(i).getValue(p);
                    if (!want.equals(got)) throw new IllegalStateException("layout mismatch " + block + " state " + i);
                }
            }
            StringBuilder fs = new StringBuilder("facts");
            long prev = 0; int run = 0;
            for (int i = 0; i < states.size(); i++) {
                long w = facts(states.get(i));
                if (run > 0 && w == prev) { run++; continue; }
                if (run > 0) fs.append(' ').append(Long.toHexString(prev)).append(run > 1 ? "*" + run : "");
                prev = w; run = 1;
            }
            fs.append(' ').append(Long.toHexString(prev)).append(run > 1 ? "*" + run : "");
            OUT.println(fs);
        }
        OUT.flush();
    }

    @SuppressWarnings({"unchecked", "rawtypes"})
    static String valueName(Property p, Object v) { return p.getName((Comparable) v); }
}
