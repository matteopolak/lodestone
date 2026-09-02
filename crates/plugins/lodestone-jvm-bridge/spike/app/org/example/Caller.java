package org.example;

import net.minecraft.world.level.Level;

/**
 * Stands in for Paper's {@code org.bukkit.craftbukkit.CraftWorld}: a class
 * outside {@code net.minecraft} that is compiled once against the real NMS
 * signature and then never touched again.
 *
 * <p>The spike compiles this against {@code spike/real} and runs it in two
 * arms without recompiling. If the answer changes between the arms, classload
 * interception works on already-compiled third-party bytecode — which is the
 * one mechanism the whole bridge design rests on.
 */
public final class Caller {
    private Caller() {}

    /** Called reflectively by the harness so the harness itself is not bound to {@link Level}. */
    public static String describe() {
        return new Level().getBlockName(11, 1, 4);
    }

    /**
     * Reach the {@code native} method the shim declares. Reported as a string
     * rather than thrown so the harness can print one line per arm.
     */
    public static String describeNative() {
        try {
            java.lang.reflect.Method m =
                    Level.class.getMethod("nativeBlockName", int.class, int.class, int.class);
            return String.valueOf(m.invoke(new Level(), 11, 1, 4));
        } catch (NoSuchMethodException e) {
            return "NO-SUCH-METHOD (this class has no native seam)";
        } catch (java.lang.reflect.InvocationTargetException e) {
            return e.getCause().getClass().getSimpleName() + " (the JNI seam is reachable)";
        } catch (ReflectiveOperationException e) {
            return "UNEXPECTED: " + e;
        }
    }
}
