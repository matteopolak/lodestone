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

    /** Preserve the callback's bounded error message as test evidence. */
    public static String describeNativeMessage() {
        return describeNativeMessageAt(11);
    }

    /** Drive the callback's deliberate panic arm with its discriminating input. */
    public static String describeNativePanicMessage() {
        return describeNativeMessageAt(19);
    }

    private static String describeNativeMessageAt(int x) {
        try {
            java.lang.reflect.Method m =
                    Level.class.getMethod("nativeBlockName", int.class, int.class, int.class);
            return String.valueOf(m.invoke(new Level(), x, 1, 4));
        } catch (NoSuchMethodException e) {
            return "NO-SUCH-METHOD:" + e.getMessage();
        } catch (java.lang.reflect.InvocationTargetException e) {
            Throwable cause = e.getCause();
            return cause.getClass().getSimpleName() + ":" + cause.getMessage();
        } catch (ReflectiveOperationException e) {
            return "UNEXPECTED:" + e;
        }
    }

    /** Exercise a primitive-return native member on the same intercepted shim. */
    public static String describeNativeId() {
        try {
            java.lang.reflect.Method m =
                    Level.class.getMethod("nativeBlockStateId", int.class, int.class, int.class);
            return "NATIVE-ID:" + m.invoke(new Level(), 11, 1, 4);
        } catch (NoSuchMethodException e) {
            return "NO-SUCH-METHOD:" + e.getMessage();
        } catch (java.lang.reflect.InvocationTargetException e) {
            Throwable cause = e.getCause();
            return cause.getClass().getSimpleName() + ":" + cause.getMessage();
        } catch (ReflectiveOperationException e) {
            return "UNEXPECTED: " + e;
        }
    }

    /** Keep an opaque handle across callbacks, then report forged and released failures. */
    public static String describeHandleLifetime() {
        try {
            java.lang.reflect.Method acquire = Level.class.getMethod(
                    "nativeAcquireBlockHandle", int.class, int.class, int.class);
            java.lang.reflect.Method read = Level.class.getMethod(
                    "nativeReadBlockHandle", long.class);
            java.lang.reflect.Method release = Level.class.getMethod(
                    "nativeReleaseBlockHandle", long.class);
            Level level = new Level();
            long handle = ((Long) acquire.invoke(level, 11, 1, 4)).longValue();
            String live = (String) read.invoke(level, handle);
            String forged = (String) read.invoke(level, handle ^ 1L);
            int released = ((Integer) release.invoke(level, handle)).intValue();
            String after = (String) read.invoke(level, handle);
            return "HANDLE-LIFETIME:live=" + live
                    + " forged=" + forged
                    + " released=" + released
                    + " after=" + after;
        } catch (NoSuchMethodException e) {
            return "NO-SUCH-METHOD:" + e.getMessage();
        } catch (java.lang.reflect.InvocationTargetException e) {
            Throwable cause = e.getCause();
            return cause.getClass().getSimpleName() + ":" + cause.getMessage();
        } catch (ReflectiveOperationException e) {
            return "UNEXPECTED: " + e;
        }
    }

    /** Exercise the bounded Rust-to-Java-to-Rust callback recursion control. */
    public static String describeReentrantDepth(int remaining) {
        try {
            java.lang.reflect.Method m = Level.class.getMethod(
                    "nativeReentrantDepth", int.class);
            return (String) m.invoke(new Level(), remaining);
        } catch (NoSuchMethodException e) {
            return "NO-SUCH-METHOD:" + e.getMessage();
        } catch (java.lang.reflect.InvocationTargetException e) {
            Throwable cause = e.getCause();
            return cause.getClass().getSimpleName() + ":" + cause.getMessage();
        } catch (ReflectiveOperationException e) {
            return "UNEXPECTED: " + e;
        }
    }

    /** Below-limit and over-limit controls in one externally observable result. */
    public static String describeReentrantControls() {
        return "REENTRANT-CONTROL:below=" + describeReentrantDepth(2)
                + " over=" + describeReentrantDepth(4);
    }
}
