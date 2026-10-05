import java.lang.reflect.Field;
import java.util.LinkedHashMap;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.client.model.EntityModel;
import net.minecraft.client.model.animal.camel.AdultCamelModel;
import net.minecraft.client.model.animal.camel.BabyCamelModel;
import net.minecraft.client.model.animal.feline.AdultCatModel;
import net.minecraft.client.model.animal.feline.AdultOcelotModel;
import net.minecraft.client.model.animal.feline.BabyCatModel;
import net.minecraft.client.model.animal.feline.BabyOcelotModel;
import net.minecraft.client.model.animal.fox.AdultFoxModel;
import net.minecraft.client.model.animal.fox.BabyFoxModel;
import net.minecraft.client.model.animal.wolf.AdultWolfModel;
import net.minecraft.client.model.animal.wolf.BabyWolfModel;
import net.minecraft.client.model.geom.LayerDefinitions;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.ModelLayers;
import net.minecraft.client.model.geom.ModelPart;
import net.minecraft.client.model.geom.builders.LayerDefinition;
import net.minecraft.client.renderer.entity.state.CamelRenderState;
import net.minecraft.client.renderer.entity.state.CatRenderState;
import net.minecraft.client.renderer.entity.state.FelineRenderState;
import net.minecraft.client.renderer.entity.state.FoxRenderState;
import net.minecraft.client.renderer.entity.state.LivingEntityRenderState;
import net.minecraft.client.renderer.entity.state.WolfRenderState;
import net.minecraft.server.Bootstrap;

/**
 * Ground truth for the code-driven resting poses of the wolf, fox, cat and
 * ocelot (adult and baby rigs) and the camel's dash head nod: bakes each model from the real client's own
 * layer table, fills its render state for a scenario, runs the real pose setup,
 * and prints every part's local pose.
 *
 * Output:
 *   {@code input <scenario> <model> key=value ...}   the scenario's inputs
 *   {@code <scenario> <part> x y z xRot yRot zRot xScale yScale zScale visible}
 *
 * Read by crates/lodestone-render/tests/entities/posture_oracle.rs. Regenerate
 * with {@code just oracle-posture}.
 */
public final class PostureOracle {
    private static final String[] SCENARIOS = {
        "wolf_walk wolf pitch=10 yaw=20 pos=1.3 speed=0.6 age=17.25",
        "wolf_sit wolf sitting=1 pitch=10 yaw=20 pos=1.3 speed=0.6 age=17.25",
        "wolf_baby_walk wolf_baby pitch=10 yaw=20 pos=1.3 speed=0.6 age=17.25",
        "wolf_baby_sit wolf_baby sitting=1 pitch=10 yaw=20 pos=1.3 speed=0.6 age=17.25",
        "fox_walk fox pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5 roll=0.21",
        "fox_sit fox sitting=1 pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5",
        "fox_sleep fox sleeping=1 pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5 roll=0.21",
        "fox_sleep_sit fox sleeping=1 sitting=1 pitch=-8 yaw=15 age=33.5",
        "fox_crouch fox crouching=1 crouch=2.4 pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5",
        "fox_pounce fox pouncing=1 crouch=3 pitch=-8 yaw=15 age=33.5",
        "fox_crouch_pounce fox crouching=1 pouncing=1 crouch=5 pitch=-8 yaw=15 age=12.75",
        "fox_baby_walk fox_baby pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5 roll=0.21",
        "fox_baby_sit fox_baby sitting=1 pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5",
        "fox_baby_sleep fox_baby sleeping=1 pitch=-8 yaw=15 age=33.5 roll=0.21",
        "fox_baby_crouch fox_baby crouching=1 crouch=2.4 pitch=-8 yaw=15 pos=2.1 speed=0.4 age=33.5",
        "fox_baby_pounce fox_baby pouncing=1 crouch=3 pitch=-8 yaw=15 age=33.5",
        "cat_walk cat pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_sprint cat sprinting=1 pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_crouch cat crouching=1 pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_sit cat sitting=1 pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_lie cat lie=0.6 lie_tail=0.3 pitch=12 yaw=-25 age=41",
        "cat_lie_full cat lie=1 lie_tail=1 pitch=12 yaw=-25 age=41",
        "cat_relax cat relax=0.5 pitch=12 yaw=-25 age=41",
        "cat_sit_lie cat sitting=1 lie=1 lie_tail=1 relax=1 pitch=12 yaw=-25 age=41",
        "cat_baby_walk cat_baby pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_baby_sprint cat_baby sprinting=1 pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_baby_crouch cat_baby crouching=1 pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_baby_sit cat_baby sitting=1 pitch=12 yaw=-25 pos=3.7 speed=0.7 age=41",
        "cat_baby_lie cat_baby lie=0.6 lie_tail=0.3 pitch=12 yaw=-25 age=41",
        "cat_baby_lie_full cat_baby lie=1 lie_tail=1 pitch=12 yaw=-25 age=41",
        "cat_baby_relax cat_baby relax=0.5 pitch=12 yaw=-25 age=41",
        "ocelot_walk ocelot pitch=5 yaw=30 pos=0.9 speed=0.8 age=8",
        "ocelot_sprint ocelot sprinting=1 pitch=5 yaw=30 pos=0.9 speed=0.8 age=8",
        "ocelot_crouch ocelot crouching=1 pitch=5 yaw=30 pos=0.9 speed=0.8 age=8",
        "ocelot_baby_crouch ocelot_baby crouching=1 pitch=5 yaw=30 pos=0.9 speed=0.8 age=8",
        "camel_still camel pitch=40 yaw=12 age=20",
        "camel_nod camel jump=27.5 pitch=40 yaw=12 age=20",
        "camel_nod_capped camel jump=50 pitch=40 yaw=12 age=20",
        "camel_nod_steep camel jump=11 pitch=60 yaw=-40 age=20",
        "camel_baby_nod camel_baby jump=33 pitch=-10 yaw=5 age=20",
    };

    private static Map<ModelLayerLocation, LayerDefinition> roots;

    private static ModelPart bake(ModelLayerLocation layer) {
        return roots.get(layer).bakeRoot();
    }

    @SuppressWarnings({"rawtypes", "unchecked"})
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        roots = LayerDefinitions.createRoots();
        StringBuilder out = new StringBuilder();
        out.append("# PostureOracle: local part poses after the real client's pose setup.\n");
        out.append("# input <scenario> <model> key=value ...\n");
        out.append("# <scenario> <part> x y z xRot yRot zRot xScale yScale zScale visible\n");
        for (String line : SCENARIOS) {
            String[] cols = line.split(" ");
            String name = cols[0];
            String model = cols[1];
            Map<String, Float> p = new LinkedHashMap<>();
            for (int i = 2; i < cols.length; i++) {
                String[] kv = cols[i].split("=");
                p.put(kv[0], Float.parseFloat(kv[1]));
            }
            boolean baby = model.endsWith("_baby");
            ModelPart root;
            EntityModel entityModel;
            LivingEntityRenderState state;
            switch (model) {
                case "wolf" -> { root = bake(ModelLayers.WOLF); entityModel = new AdultWolfModel(root); state = wolf(p); }
                case "wolf_baby" -> { root = bake(ModelLayers.WOLF_BABY); entityModel = new BabyWolfModel(root); state = wolf(p); }
                case "fox" -> { root = bake(ModelLayers.FOX); entityModel = new AdultFoxModel(root); state = fox(p); }
                case "fox_baby" -> { root = bake(ModelLayers.FOX_BABY); entityModel = new BabyFoxModel(root); state = fox(p); }
                case "cat" -> { root = bake(ModelLayers.CAT); entityModel = new AdultCatModel(root); state = feline(new CatRenderState(), p); }
                case "cat_baby" -> { root = bake(ModelLayers.CAT_BABY); entityModel = new BabyCatModel(root); state = feline(new CatRenderState(), p); }
                case "ocelot" -> { root = bake(ModelLayers.OCELOT); entityModel = new AdultOcelotModel(root); state = feline(new FelineRenderState(), p); }
                case "camel" -> { root = bake(ModelLayers.CAMEL); entityModel = new AdultCamelModel(root); state = camel(p); }
                case "camel_baby" -> { root = bake(ModelLayers.CAMEL_BABY); entityModel = new BabyCamelModel(root); state = camel(p); }
                case "ocelot_baby" -> { root = bake(ModelLayers.OCELOT_BABY); entityModel = new BabyOcelotModel(root); state = feline(new FelineRenderState(), p); }
                default -> throw new IllegalArgumentException(model);
            }
            state.xRot = p.getOrDefault("pitch", 0.0F);
            state.yRot = p.getOrDefault("yaw", 0.0F);
            state.walkAnimationPos = p.getOrDefault("pos", 0.0F);
            state.walkAnimationSpeed = p.getOrDefault("speed", 0.0F);
            state.ageInTicks = p.getOrDefault("age", 0.0F);
            state.isBaby = baby;
            state.ageScale = baby ? 0.5F : 1.0F;
            entityModel.setupAnim(state);
            out.append("input ").append(line).append('\n');
            dump(out, name, root);
        }
        System.out.print(out);
    }

    private static WolfRenderState wolf(Map<String, Float> p) {
        WolfRenderState s = new WolfRenderState();
        s.isSitting = p.getOrDefault("sitting", 0.0F) != 0.0F;
        return s;
    }

    private static CamelRenderState camel(Map<String, Float> p) {
        CamelRenderState s = new CamelRenderState();
        s.jumpCooldown = p.getOrDefault("jump", 0.0F);
        return s;
    }

    private static FoxRenderState fox(Map<String, Float> p) {
        FoxRenderState s = new FoxRenderState();
        s.isSitting = p.getOrDefault("sitting", 0.0F) != 0.0F;
        s.isSleeping = p.getOrDefault("sleeping", 0.0F) != 0.0F;
        s.isCrouching = p.getOrDefault("crouching", 0.0F) != 0.0F;
        s.isPouncing = p.getOrDefault("pouncing", 0.0F) != 0.0F;
        s.crouchAmount = p.getOrDefault("crouch", 0.0F);
        s.headRollAngle = p.getOrDefault("roll", 0.0F);
        return s;
    }

    private static <S extends FelineRenderState> S feline(S s, Map<String, Float> p) {
        s.isSitting = p.getOrDefault("sitting", 0.0F) != 0.0F;
        s.isCrouching = p.getOrDefault("crouching", 0.0F) != 0.0F;
        s.isSprinting = p.getOrDefault("sprinting", 0.0F) != 0.0F;
        s.lieDownAmount = p.getOrDefault("lie", 0.0F);
        s.lieDownAmountTail = p.getOrDefault("lie_tail", 0.0F);
        s.relaxStateOneAmount = p.getOrDefault("relax", 0.0F);
        return s;
    }

    @SuppressWarnings("unchecked")
    private static void dump(StringBuilder out, String scenario, ModelPart part) throws Exception {
        Field field = ModelPart.class.getDeclaredField("children");
        field.setAccessible(true);
        Map<String, ModelPart> children = (Map<String, ModelPart>) field.get(part);
        for (Map.Entry<String, ModelPart> child : children.entrySet()) {
            ModelPart c = child.getValue();
            out.append(scenario).append(' ').append(child.getKey())
                .append(' ').append(c.x).append(' ').append(c.y).append(' ').append(c.z)
                .append(' ').append(c.xRot).append(' ').append(c.yRot).append(' ').append(c.zRot)
                .append(' ').append(c.xScale).append(' ').append(c.yScale).append(' ').append(c.zScale)
                .append(' ').append(c.visible ? 1 : 0).append('\n');
            dump(out, scenario, c);
        }
    }
}
