// Independent compiled-server probe for the iceberg configured feature.
import java.lang.reflect.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.biome.FixedBiomeSource;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.IcebergFeature;
import net.minecraft.world.level.levelgen.feature.configurations.BlockStateConfiguration;

public final class IcebergOracle {
  static String key(BlockPos p) { return p.getX()+","+p.getY()+","+p.getZ(); }
  static BlockState state(Map<String,BlockState> blocks, BlockPos p) {
    BlockState changed = blocks.get(key(p));
    return changed != null ? changed : p.getY() <= 63 ? Blocks.WATER.defaultBlockState() : Blocks.AIR.defaultBlockState();
  }
  static String block(BlockState state) { return BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString(); }
  static WorldGenLevel level(Map<String,BlockState> blocks) {
    InvocationHandler h=(p,m,a)-> { String n=m.getName();
      if(n.equals("getBlockState")) return state(blocks, (BlockPos)a[0]);
      if(n.equals("setBlock")){ blocks.put(key((BlockPos)a[0]),(BlockState)a[1]); return true; }
      if(n.equals("ensureCanWrite")) return true;
      if(n.equals("isOutsideBuildHeight")) return false;
      if(n.equals("getMinY")||n.equals("getMinBuildHeight")) return -64;
      if(n.equals("getMaxY")||n.equals("getMaxBuildHeight")) return 319;
      if(n.equals("toString")) return "IcebergOracle";
      if(n.equals("hashCode")) return System.identityHashCode(p); if(n.equals("equals")) return p==a[0];
      throw new UnsupportedOperationException(n+"/"+(a==null?0:a.length)); };
    return (WorldGenLevel)Proxy.newProxyInstance(IcebergOracle.class.getClassLoader(),new Class[]{WorldGenLevel.class},h);
  }
  public static void main(String[] args) {
    SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
    HolderLookup.Provider registries=VanillaRegistries.createLookup();
    Holder<NoiseGeneratorSettings> settings=registries.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
    Holder<net.minecraft.world.level.biome.Biome> plains=registries.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS);
    ChunkGenerator generator=new NoiseBasedChunkGenerator(new FixedBiomeSource(plains),settings);
    Map<String,BlockState> blocks=new TreeMap<>();
    BlockStateConfiguration cfg=new BlockStateConfiguration(Blocks.PACKED_ICE.defaultBlockState());
    new IcebergFeature(BlockStateConfiguration.CODEC).place(new FeaturePlaceContext<>(Optional.empty(),level(blocks),generator,RandomSource.create(0),new BlockPos(0,0,0),cfg));
    for(var e:new TreeMap<>(blocks).entrySet()) {
      BlockPos p=parse(e.getKey());
      String before=p.getY() <= 63 ? "minecraft:water" : "minecraft:air";
      String after=block(e.getValue());
      if(!after.equals(before)) System.out.println(p.getX()+","+p.getY()+","+p.getZ()+" "+after);
    }
  }
  static BlockPos parse(String key) { String[] p=key.split(","); return new BlockPos(Integer.parseInt(p[0]),Integer.parseInt(p[1]),Integer.parseInt(p[2])); }
}
