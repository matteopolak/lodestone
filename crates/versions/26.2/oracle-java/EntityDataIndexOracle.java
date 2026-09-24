import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import java.lang.reflect.ParameterizedType;
import java.lang.reflect.Type;
import java.lang.reflect.WildcardType;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.syncher.EntityDataAccessor;
import net.minecraft.network.syncher.EntityDataSerializers;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.EntityTypes;

/**
 * Authoritative map of every synched-entity-data <em>index</em> in 26.2: which
 * class declared each {@code EntityDataAccessor}, the index
 * {@code SynchedEntityData.defineId} handed it, and the serializer id it is
 * encoded with on the wire.
 *
 * <h2>Why this exists</h2>
 *
 * {@code v26-2}'s {@code packets::metadata} carries roughly a dozen
 * {@code IDX_*} constants. Every one of them was <em>hand-counted</em>: the
 * indices are assigned by a per-class-hierarchy declaration-order counter inside
 * {@code SynchedEntityData.defineId}, so the number for
 * {@code Mob.DATA_MOB_FLAGS_ID} is "8 fields on {@code Entity}, then 7 on
 * {@code LivingEntity}, so 15" — a count over two source files that is correct
 * only until someone inserts a field. That is exactly the class of expected
 * value {@code CLAUDE.md} requires to originate outside the code under test.
 *
 * <h2>Why the sort order is by index</h2>
 *
 * The dump is ordered by index first, so <em>collisions are adjacent lines</em>.
 * Index ambiguity is the load-bearing hazard in this decoder: index 8 is
 * {@code LivingEntity}'s flags byte <em>and</em> {@code AbstractArrow}'s flags
 * byte, both {@code BYTE}, with bit-identical meanings for bit {@code 0x01}
 * ("using item" vs "critical"); index 15 is {@code Mob}'s flags byte
 * <em>and</em> {@code ArmorStand}'s client flags byte, both {@code BYTE}, with
 * bit {@code 0x04} meaning "aggressive" on one and "show arms" on the other.
 * Neither collision is visible in a per-class listing, and both are decoder
 * bugs that produce plausible output. Reading the dump top to bottom shows every
 * one of them.
 *
 * <h2>How the class set is chosen</h2>
 *
 * Every registered entity type's implementation class, plus every superclass up
 * to {@code Object} — recovered the same way {@code EntityCensusOracle} does it,
 * from the generic signature of the {@code public static final EntityType<T>}
 * fields on {@code EntityTypes}, because {@code EntityType} does not expose its
 * implementation class. So the set is exactly the classes that can appear on the
 * wire, and nothing is enumerated by hand.
 *
 * Reading a field's value forces its declaring class's static initialiser, which
 * is what runs {@code defineId} and therefore what assigns the index; walking
 * {@code getDeclaredFields()} alone would not.
 *
 * <h2>What is dumped</h2>
 *
 * One line per accessor, sorted by (index, class, field), space-separated:
 *
 * <pre>
 *   &lt;index&gt; &lt;declaringClass&gt;.&lt;FIELD&gt; &lt;serializerId&gt; &lt;serializerName&gt;
 * </pre>
 *
 * {@code serializerId} is the wire id ({@code EntityDataSerializers
 * .getSerializedId}), which is what a decoder actually branches on;
 * {@code serializerName} is the {@code EntityDataSerializers} constant's field
 * name, for human review only. An accessor whose serializer is not registered is
 * a hard error rather than a {@code -} column, because an unregistered
 * serializer cannot be transmitted and would mean the classpath is wrong.
 */
public final class EntityDataIndexOracle {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // Serializer wire id -> the EntityDataSerializers field name, for review.
        List<String> serializerNames = new ArrayList<>();
        for (Field field : EntityDataSerializers.class.getDeclaredFields()) {
            if (!Modifier.isStatic(field.getModifiers())) {
                continue;
            }
            field.setAccessible(true);
            Object value = field.get(null);
            if (!(value instanceof net.minecraft.network.syncher.EntityDataSerializer<?> serializer)) {
                continue;
            }
            int id = EntityDataSerializers.getSerializedId(serializer);
            if (id < 0) {
                throw new IllegalStateException(
                        "EntityDataSerializers." + field.getName() + " is not registered");
            }
            while (serializerNames.size() <= id) {
                serializerNames.add(null);
            }
            if (serializerNames.get(id) != null) {
                throw new IllegalStateException("two EntityDataSerializers fields share id " + id);
            }
            serializerNames.set(id, field.getName());
        }

        Set<Class<?>> classes = new LinkedHashSet<>();
        for (Field field : EntityTypes.class.getDeclaredFields()) {
            if (!Modifier.isStatic(field.getModifiers()) || field.getType() != EntityType.class) {
                continue;
            }
            field.setAccessible(true);
            EntityType<?> type = (EntityType<?>) field.get(null);
            if (type == null) {
                throw new IllegalStateException("EntityTypes." + field.getName() + " is null");
            }
            if (BuiltInRegistries.ENTITY_TYPE.getId(type) < 0) {
                throw new IllegalStateException(
                        "EntityTypes." + field.getName() + " is not in BuiltInRegistries.ENTITY_TYPE");
            }
            for (Class<?> clazz = implClassOf(field); clazz != null; clazz = clazz.getSuperclass()) {
                classes.add(clazz);
            }
        }

        List<Row> rows = new ArrayList<>();
        for (Class<?> clazz : classes) {
            for (Field field : clazz.getDeclaredFields()) {
                if (!Modifier.isStatic(field.getModifiers())
                        || field.getType() != EntityDataAccessor.class) {
                    continue;
                }
                field.setAccessible(true);
                // Reading the value is what forces the static initialiser, and
                // therefore what makes `defineId` assign the index at all.
                EntityDataAccessor<?> accessor = (EntityDataAccessor<?>) field.get(null);
                if (accessor == null) {
                    throw new IllegalStateException(
                            simpleName(clazz) + "." + field.getName() + " is null");
                }
                int serializer = EntityDataSerializers.getSerializedId(accessor.serializer());
                if (serializer < 0 || serializer >= serializerNames.size()
                        || serializerNames.get(serializer) == null) {
                    throw new IllegalStateException(simpleName(clazz) + "." + field.getName()
                            + " has an unregistered serializer");
                }
                rows.add(new Row(accessor.id(), simpleName(clazz), field.getName(), serializer,
                        serializerNames.get(serializer)));
            }
        }

        rows.sort(Comparator.<Row>comparingInt(row -> row.index)
                .thenComparing(row -> row.owner)
                .thenComparing(row -> row.field));

        StringBuilder sb = new StringBuilder();
        for (Row row : rows) {
            sb.append(row.index)
                    .append(' ').append(row.owner).append('.').append(row.field)
                    .append(' ').append(row.serializer)
                    .append(' ').append(row.serializerName)
                    .append('\n');
        }
        System.out.print(sb);
    }

    private record Row(int index, String owner, String field, int serializer, String serializerName) {}

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

    /** {@code Outer.Inner} for a nested class, the simple name otherwise. */
    private static String simpleName(Class<?> clazz) {
        Class<?> enclosing = clazz.getEnclosingClass();
        return enclosing == null
                ? clazz.getSimpleName()
                : simpleName(enclosing) + "." + clazz.getSimpleName();
    }
}
