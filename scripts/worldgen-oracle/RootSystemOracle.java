// Direct compiled-server root-system oracle.  It exercises the feature's own
// candidate scan and both scatter passes against a strict in-memory level.
import java.lang.reflect.*;
import java.util.*;
import net.minecraft.*;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.*;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.levelgen.blockpredicates.BlockPredicate;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.*;
import net.minecraft.world.level.levelgen.feature.stateproviders.BlockStateProvider;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;

public final class RootSystemOracle {
  static String key(BlockPos p) { return p.getX()+","+p.getY()+","+p.getZ(); }
  static BlockState state(Map<String,BlockState> blocks, BlockPos p) {
    return blocks.getOrDefault(key(p), p.getY() <= 64 ? Blocks.STONE.defaultBlockState() : Blocks.AIR.defaultBlockState());
  }
  static WorldGenLevel level(Map<String,BlockState> blocks) {
    InvocationHandler h=(p,m,a)-> { String n=m.getName();
      if(n.equals("getBlockState")) return state(blocks, (BlockPos)a[0]);
      if(n.equals("getFluidState")) return state(blocks, (BlockPos)a[0]).getFluidState();
      if(n.equals("isEmptyBlock")) return state(blocks, (BlockPos)a[0]).isAir();
      if(n.equals("setBlock")){ blocks.put(key((BlockPos)a[0]),(BlockState)a[1]); return true; }
      if(n.equals("ensureCanWrite")) return true;
      if(n.equals("getHeight")) return 80;
      if(n.equals("getMinY")||n.equals("getMinBuildHeight")) return 0;
      if(n.equals("getMaxY")||n.equals("getMaxBuildHeight")) return 128;
      if(n.equals("isOutsideBuildHeight")) return false;
      if(n.equals("toString")) return "RootSystemOracle";
      if(n.equals("hashCode")) return System.identityHashCode(p); if(n.equals("equals")) return p==a[0];
      throw new UnsupportedOperationException(n+"/"+(a==null?0:a.length)); };
    return (WorldGenLevel)Proxy.newProxyInstance(RootSystemOracle.class.getClassLoader(),new Class[]{WorldGenLevel.class},h);
  }
  static final class Inner extends Feature<NoneFeatureConfiguration> {
    Inner(){super(NoneFeatureConfiguration.CODEC);}
    public boolean place(FeaturePlaceContext<NoneFeatureConfiguration> c){c.level().setBlock(c.origin(),Blocks.OAK_LOG.defaultBlockState(),2);return true;}
  }
  static void dump(String label, Map<String,BlockState> m){ for(var e:new TreeMap<>(m).entrySet()) System.out.println(label+" "+e.getKey()+" "+BuiltInRegistries.BLOCK.getKey(e.getValue().getBlock())); }
  public static void main(String[] args){
    SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
    HolderLookup.Provider registries=VanillaRegistries.createLookup();
    Holder<NoiseGeneratorSettings> settings=registries.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
    Holder<Biome> plains=registries.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS);
    ChunkGenerator generator=new NoiseBasedChunkGenerator(new FixedBiomeSource(plains),settings);
    ConfiguredFeature<NoneFeatureConfiguration,Inner> configured=new ConfiguredFeature<>(new Inner(),NoneFeatureConfiguration.INSTANCE);
    PlacedFeature inner=new PlacedFeature(Holder.direct(configured),List.of());
    RootSystemConfiguration cfg=new RootSystemConfiguration(Holder.direct(inner),3,0,0,3,HolderSet.direct(Blocks.STONE.builtInRegistryHolder()),BlockStateProvider.simple(Blocks.ROOTED_DIRT),20,8,3,2,BlockStateProvider.simple(Blocks.HANGING_ROOTS),20,2,BlockPredicate.matchesBlocks(Blocks.AIR));
    Map<String,BlockState> normal=new TreeMap<>();
    for(int x=-3;x<=3;x++) for(int z=-3;z<=3;z++) normal.put(key(new BlockPos(x,62,z)),Blocks.AIR.defaultBlockState());
    normal.put(key(new BlockPos(0,63,0)),Blocks.AIR.defaultBlockState());
    Map<String,BlockState> before=new TreeMap<>(normal);
    new RootSystemFeature(RootSystemConfiguration.CODEC).place(new FeaturePlaceContext<>(Optional.empty(),level(normal),generator,RandomSource.create(19),new BlockPos(0,63,0),cfg));
    for(var e:normal.entrySet()) if(!Objects.equals(before.get(e.getKey()),e.getValue())) System.out.println("normal "+e.getKey()+" "+BuiltInRegistries.BLOCK.getKey(e.getValue().getBlock()));
    Map<String,BlockState> blocked=new TreeMap<>(); blocked.put(key(new BlockPos(0,63,0)),Blocks.STONE.defaultBlockState()); new RootSystemFeature(RootSystemConfiguration.CODEC).place(new FeaturePlaceContext<>(Optional.empty(),level(blocked),generator,RandomSource.create(19),new BlockPos(0,63,0),cfg)); dump("blocked",blocked);
  }
}
