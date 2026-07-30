import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.lang.reflect.ParameterizedType;
import java.lang.reflect.Type;
import java.lang.reflect.WildcardType;
import java.util.LinkedHashMap;
import java.util.Map;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.EntityTypes;
import net.minecraft.world.entity.LivingEntity;

/**
 * Authoritative per-entity-type census: the facts needed to decide, per type,
 * whether an entity of that type can shove the local player, plus its base
 * hitbox. Boots the real 26.2 server (registries only) and reads them off the
 * game's own classes, so nothing here is hand-transcribed.
 *
 * <h2>Why the implementation class, and how it is recovered</h2>
 *
 * The push question is answered by <em>which class declares</em> the two
 * methods vanilla's crowd pass runs through, and by whether the type is a
 * {@code LivingEntity} at all — because {@code LivingEntity.aiStep} is the only
 * caller of {@code pushEntities()} in the whole tree.
 *
 * {@code EntityType} does not expose its implementation class:
 * {@code EntityType.getBaseClass()} is the {@code EntityTypeTest} hook and
 * returns {@code Entity.class} unconditionally, and the {@code EntityFactory} is
 * a method reference whose target class cannot be read without invoking it (and
 * invoking it needs a {@code Level}). What <em>is</em> mechanically readable is
 * the generic signature of the {@code public static final EntityType<T>} fields
 * on {@code EntityTypes}: {@code T} is the implementation class, recorded in the
 * class file by javac. Every registered type has exactly one such field, which
 * this program asserts, so the mapping is total and unambiguous.
 *
 * <h2>What is dumped, and what is deliberately not</h2>
 *
 * Raw, mechanically-derived facts only — never a boolean this program decided.
 * The reduction from "which class declares {@code doPush}" to "can it push a
 * player" is a modelling step and belongs in reviewable Rust beside the vanilla
 * citations, not buried in an oracle; keeping it out also means the dump stays
 * usable if the model changes. In particular the mechanisms that are
 * <em>not</em> {@code LivingEntity.pushEntities} (a boat's own
 * {@code AbstractBoat.push(Entity)} pass, {@code NewMinecartBehavior
 * .pushEntities(AABB)}) are left to the consumer to model or refuse; the
 * implementation class name is dumped so that can be done later without
 * re-dumping.
 *
 * Dimensions are the base (unscaled) {@code EntityDimensions} — the same
 * {@code EntityType.getWidth()/getHeight()} {@code EntityDimensionsOracle}
 * reads. They are re-dumped here so the two dumps cross-check each other; the
 * {@code SCALE} and {@code STEP_HEIGHT} attribute folds are the consumer's, as
 * there.
 *
 * One line per type, ordered by network registry id, space-separated:
 *
 * <pre>
 *   &lt;id&gt; &lt;name&gt; &lt;implClass&gt; &lt;isLiving&gt; &lt;pushEntitiesDecl&gt; &lt;doPushDecl&gt; &lt;widthBits&gt; &lt;heightBits&gt;
 * </pre>
 *
 * where {@code implClass} and the two {@code *Decl} columns are simple class
 * names (nested classes as {@code Outer.Inner}), {@code isLiving} is
 * {@code true}/{@code false}, and the two bit columns are raw {@code f32} bits
 * in hex. A {@code *Decl} column is {@code -} when the class hierarchy declares
 * the method nowhere, which cannot happen for a {@code LivingEntity} subclass
 * and is the expected value for a plain {@code Entity} one.
 */
public final class EntityCensusOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // Registry id -> implementation class, recovered from the generic
        // signature of the EntityTypes constant that registered it.
        Map<Integer, Class<?>> implByT = new LinkedHashMap<>();
        for (Field field : EntityTypes.class.getDeclaredFields()) {
            if (!Modifier.isStatic(field.getModifiers()) || field.getType() != EntityType.class) {
                continue;
            }
            field.setAccessible(true);
            EntityType<?> type = (EntityType<?>) field.get(null);
            if (type == null) {
                throw new IllegalStateException("EntityTypes." + field.getName() + " is null");
            }
            int id = BuiltInRegistries.ENTITY_TYPE.getId(type);
            if (id < 0) {
                throw new IllegalStateException(
                        "EntityTypes." + field.getName() + " is not in BuiltInRegistries.ENTITY_TYPE");
            }
            Class<?> impl = implClassOf(field);
            Class<?> previous = implByT.put(id, impl);
            if (previous != null) {
                throw new IllegalStateException("two EntityTypes fields map to registry id " + id);
            }
        }

        int count = BuiltInRegistries.ENTITY_TYPE.size();
        if (implByT.size() != count) {
            throw new IllegalStateException("recovered " + implByT.size()
                    + " implementation classes for " + count + " registered types");
        }

        String[] lines = new String[count];
        for (EntityType<?> type : BuiltInRegistries.ENTITY_TYPE) {
            int id = BuiltInRegistries.ENTITY_TYPE.getId(type);
            String name = BuiltInRegistries.ENTITY_TYPE.getKey(type).toString();
            Class<?> impl = implByT.get(id);
            boolean living = LivingEntity.class.isAssignableFrom(impl);
            lines[id] = id
                    + " " + name
                    + " " + simpleName(impl)
                    + " " + living
                    + " " + declarerOf(impl, "pushEntities")
                    + " " + declarerOf(impl, "doPush", Entity.class)
                    + " " + Integer.toHexString(Float.floatToRawIntBits(type.getWidth()))
                    + " " + Integer.toHexString(Float.floatToRawIntBits(type.getHeight()));
        }

        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < count; i++) {
            if (lines[i] == null) {
                throw new IllegalStateException("registry ids are not dense: nothing at " + i);
            }
            sb.append(lines[i]).append('\n');
        }
        System.out.print(sb);
    }

    /** The {@code T} of an {@code EntityType<T>} field, as a class. */
    private static Class<?> implClassOf(Field field) {
        Type generic = field.getGenericType();
        if (!(generic instanceof ParameterizedType parameterized)) {
            throw new IllegalStateException(
                    "EntityTypes." + field.getName() + " has no generic type argument");
        }
        Type argument = parameterized.getActualTypeArguments()[0];
        if (argument instanceof WildcardType wildcard) {
            argument = wildcard.getUpperBounds()[0];
        }
        if (argument instanceof Class<?> clazz) {
            return clazz;
        }
        throw new IllegalStateException("EntityTypes." + field.getName()
                + " has a non-class type argument " + argument);
    }

    /**
     * The class in {@code start}'s hierarchy that declares {@code name}, or
     * {@code -} if none does. {@code pushEntities} and {@code doPush} are
     * {@code protected}, so {@code getMethod} cannot see them and the chain must
     * be walked by hand.
     */
    private static String declarerOf(Class<?> start, String name, Class<?>... parameters) {
        for (Class<?> clazz = start; clazz != null; clazz = clazz.getSuperclass()) {
            for (Method method : clazz.getDeclaredMethods()) {
                if (method.getName().equals(name)
                        && method.getParameterCount() == parameters.length
                        && parametersMatch(method, parameters)) {
                    return simpleName(clazz);
                }
            }
        }
        return "-";
    }

    private static boolean parametersMatch(Method method, Class<?>[] parameters) {
        Class<?>[] actual = method.getParameterTypes();
        for (int i = 0; i < parameters.length; i++) {
            if (actual[i] != parameters[i]) {
                return false;
            }
        }
        return true;
    }

    /** {@code Outer.Inner} for a nested class, the simple name otherwise. */
    private static String simpleName(Class<?> clazz) {
        Class<?> enclosing = clazz.getEnclosingClass();
        return enclosing == null
                ? clazz.getSimpleName()
                : simpleName(enclosing) + "." + clazz.getSimpleName();
    }
}
