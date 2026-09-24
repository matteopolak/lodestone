import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.entity.EntityType;

/**
 * Authoritative per-entity-type base dimension extractor. Boots the real 26.2
 * server (registries only) and asks every registered EntityType for its base
 * hitbox via the game's own EntityDimensions, so the numbers are version-exact
 * and immune to third-party dataset lag.
 *
 * These are the *base* (unscaled) width/height that vanilla keys on entity type
 * — EntityType.getWidth()/getHeight() read dimensions.width()/height() at
 * scale 1. The SCALE attribute fold and the STEP_HEIGHT attribute are applied
 * elsewhere (attribute map), so they are deliberately not dumped here.
 *
 * One line per type, ordered by network registry id:
 *   <networkId> <name> <widthBits> <heightBits>   (raw f32 bits, hex)
 */
public final class EntityDimensionsOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        int count = BuiltInRegistries.ENTITY_TYPE.size();
        String[] lines = new String[count];
        for (EntityType<?> type : BuiltInRegistries.ENTITY_TYPE) {
            int id = BuiltInRegistries.ENTITY_TYPE.getId(type);
            String name = BuiltInRegistries.ENTITY_TYPE.getKey(type).toString();
            int wBits = Float.floatToRawIntBits(type.getWidth());
            int hBits = Float.floatToRawIntBits(type.getHeight());
            lines[id] = id + " " + name + " "
                    + Integer.toHexString(wBits) + " " + Integer.toHexString(hBits);
        }
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < count; i++) {
            sb.append(lines[i]).append('\n');
        }
        System.out.print(sb);
    }
}
