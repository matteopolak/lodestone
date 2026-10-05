import java.lang.reflect.Field;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

import net.minecraft.SharedConstants;
import net.minecraft.client.model.HumanoidModel;
import net.minecraft.client.model.geom.LayerDefinitions;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.ModelLayers;
import net.minecraft.client.model.geom.ModelPart;
import net.minecraft.client.model.geom.builders.LayerDefinition;
import net.minecraft.client.model.monster.piglin.BabyPiglinModel;
import net.minecraft.client.model.monster.piglin.BabyZombifiedPiglinModel;
import net.minecraft.client.model.monster.zombie.BabyDrownedModel;
import net.minecraft.client.model.monster.zombie.BabyZombieModel;
import net.minecraft.client.model.monster.zombie.BabyZombieVillagerModel;
import net.minecraft.client.renderer.entity.ArmorModelSet;
import net.minecraft.client.renderer.entity.state.HumanoidRenderState;
import net.minecraft.client.renderer.entity.state.PiglinRenderState;
import net.minecraft.client.renderer.entity.state.ZombieRenderState;
import net.minecraft.client.renderer.entity.state.ZombieVillagerRenderState;
import net.minecraft.client.renderer.entity.state.ZombifiedPiglinRenderState;
import net.minecraft.server.Bootstrap;

/**
 * Ground truth for baby humanoid armour: for each baby wearer and slot, bakes the
 * real client's baby armour layer into the wearer's own baby model class (as the
 * armour layer does), runs that model's pose setup for a scenario, and prints every
 * part's local pose and every face's corner extents.
 *
 * Output:
 *   {@code input <scenario> <rig> <slot> key=value ...}
 *   {@code part <scenario> <part> x y z xRot yRot zRot visible}
 *   {@code face <scenario> <part> minX minY minZ maxX maxY maxZ minU minV maxU maxV}
 *   (positions in blocks, part-local; texture coordinates normalised)
 *
 * Read by crates/lodestone-render/tests/entities/baby_armour_oracle.rs. Regenerate
 * with {@code just oracle-baby-armour}.
 */
public final class BabyArmourOracle {
    private static final String[] SCENARIOS = {
        "walk pitch=10 yaw=20 pos=1.3 speed=0.6 age=17.25",
        "aggressive aggressive=1 pitch=-5 yaw=-15 pos=2.2 speed=0.9 age=33.5",
        "crouch crouching=1 pitch=12 yaw=8 pos=0.4 speed=0.2 age=5",
    };

    private static final String[] SLOTS = {"head", "chest", "legs", "feet"};

    @SuppressWarnings({"rawtypes", "unchecked"})
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        Map<ModelLayerLocation, LayerDefinition> roots = LayerDefinitions.createRoots();
        StringBuilder out = new StringBuilder();
        out.append("# BabyArmourOracle: baby armour parts after the real client's pose setup.\n");
        Object[][] wearers = {
            {"zombie_baby", ModelLayers.ZOMBIE_BABY_ARMOR, (Function<ModelPart, HumanoidModel>) BabyZombieModel::new, "zombie"},
            {"husk_baby", ModelLayers.HUSK_BABY_ARMOR, (Function<ModelPart, HumanoidModel>) BabyZombieModel::new, "zombie"},
            {"drowned_baby", ModelLayers.DROWNED_BABY_ARMOR, (Function<ModelPart, HumanoidModel>) BabyDrownedModel::new, "zombie"},
            {"zombie_villager_baby", ModelLayers.ZOMBIE_VILLAGER_BABY_ARMOR, (Function<ModelPart, HumanoidModel>) BabyZombieVillagerModel::new, "zombie_villager"},
            {"piglin_baby", ModelLayers.PIGLIN_BABY_ARMOR, (Function<ModelPart, HumanoidModel>) BabyPiglinModel::new, "piglin"},
            {"zombified_piglin_baby", ModelLayers.ZOMBIFIED_PIGLIN_BABY_ARMOR, (Function<ModelPart, HumanoidModel>) BabyZombifiedPiglinModel::new, "zombified_piglin"},
        };
        for (Object[] wearer : wearers) {
            String rig = (String) wearer[0];
            ArmorModelSet<ModelLayerLocation> set = (ArmorModelSet<ModelLayerLocation>) wearer[1];
            Function<ModelPart, HumanoidModel> make = (Function<ModelPart, HumanoidModel>) wearer[2];
            String stateKind = (String) wearer[3];
            ModelLayerLocation[] layers = {set.head(), set.chest(), set.legs(), set.feet()};
            for (int s = 0; s < 4; s++) {
                for (String line : SCENARIOS) {
                    String[] cols = line.split(" ");
                    String name = rig + "_" + SLOTS[s] + "_" + cols[0];
                    ModelPart root = roots.get(layers[s]).bakeRoot();
                    HumanoidModel model = make.apply(root);
                    HumanoidRenderState state = state(stateKind);
                    boolean aggressive = false;
                    for (int i = 1; i < cols.length; i++) {
                        String[] kv = cols[i].split("=");
                        float v = Float.parseFloat(kv[1]);
                        switch (kv[0]) {
                            case "pitch" -> state.xRot = v;
                            case "yaw" -> state.yRot = v;
                            case "pos" -> state.walkAnimationPos = v;
                            case "speed" -> state.walkAnimationSpeed = v;
                            case "age" -> state.ageInTicks = v;
                            case "crouching" -> state.isCrouching = v != 0.0F;
                            case "aggressive" -> aggressive = v != 0.0F;
                            default -> throw new IllegalArgumentException(kv[0]);
                        }
                    }
                    if (state instanceof ZombieRenderState z) {
                        z.isAggressive = aggressive;
                    } else if (state instanceof ZombifiedPiglinRenderState z) {
                        z.isAggressive = aggressive;
                    }
                    state.isBaby = true;
                    state.ageScale = 0.5F;
                    model.setupAnim(state);
                    out.append("input ").append(name).append(' ').append(rig).append(' ').append(SLOTS[s]);
                    for (int i = 1; i < cols.length; i++) {
                        out.append(' ').append(cols[i]);
                    }
                    out.append('\n');
                    dump(out, name, root);
                }
            }
        }
        System.out.print(out);
    }

    private static HumanoidRenderState state(String kind) {
        return switch (kind) {
            case "zombie" -> new ZombieRenderState();
            case "zombie_villager" -> new ZombieVillagerRenderState();
            case "piglin" -> new PiglinRenderState();
            case "zombified_piglin" -> new ZombifiedPiglinRenderState();
            default -> throw new IllegalArgumentException(kind);
        };
    }

    @SuppressWarnings("unchecked")
    private static void dump(StringBuilder out, String scenario, ModelPart part) throws Exception {
        Field childrenField = ModelPart.class.getDeclaredField("children");
        childrenField.setAccessible(true);
        Field cubesField = ModelPart.class.getDeclaredField("cubes");
        cubesField.setAccessible(true);
        Map<String, ModelPart> children = (Map<String, ModelPart>) childrenField.get(part);
        for (Map.Entry<String, ModelPart> child : children.entrySet()) {
            ModelPart c = child.getValue();
            out.append("part ").append(scenario).append(' ').append(child.getKey())
                .append(' ').append(c.x).append(' ').append(c.y).append(' ').append(c.z)
                .append(' ').append(c.xRot).append(' ').append(c.yRot).append(' ').append(c.zRot)
                .append(' ').append(c.visible ? 1 : 0).append('\n');
            for (ModelPart.Cube cube : (List<ModelPart.Cube>) cubesField.get(c)) {
                for (ModelPart.Polygon polygon : cube.polygons) {
                    float[] lo = {Float.MAX_VALUE, Float.MAX_VALUE, Float.MAX_VALUE, Float.MAX_VALUE, Float.MAX_VALUE};
                    float[] hi = {-Float.MAX_VALUE, -Float.MAX_VALUE, -Float.MAX_VALUE, -Float.MAX_VALUE, -Float.MAX_VALUE};
                    for (ModelPart.Vertex v : polygon.vertices()) {
                        float[] p = {v.worldX(), v.worldY(), v.worldZ(), v.u(), v.v()};
                        for (int i = 0; i < 5; i++) {
                            lo[i] = Math.min(lo[i], p[i]);
                            hi[i] = Math.max(hi[i], p[i]);
                        }
                    }
                    out.append("face ").append(scenario).append(' ').append(child.getKey())
                        .append(' ').append(lo[0]).append(' ').append(lo[1]).append(' ').append(lo[2])
                        .append(' ').append(hi[0]).append(' ').append(hi[1]).append(' ').append(hi[2])
                        .append(' ').append(lo[3]).append(' ').append(lo[4]).append(' ').append(hi[3]).append(' ').append(hi[4])
                        .append('\n');
                }
            }
            dump(out, scenario, c);
        }
    }
}
