package org.example;

/**
 * A stand-in Java plugin that crosses the dynamically registered JNI seam.
 *
 * <p>The Rust harness calls {@link #runAndReport} after loading this class in
 * the in-process VM. Keeping error reporting on the Java side proves that a
 * failed callback returns control to plugin bytecode instead of terminating or
 * wedging the native process.
 */
public final class InvocationPlugin {
    private InvocationPlugin() {}

    private static native int nativeScore(int x, int y, int z);

    /** Invoke the native method and turn any Java-visible failure into evidence. */
    public static String runAndReport(int mode) {
        int x = mode == 1 ? 19 : 11;
        try {
            return "RESULT:" + nativeScore(x, 7, -3);
        } catch (Throwable error) {
            return "ERROR:" + error.getClass().getSimpleName() + ":" + error.getMessage();
        }
    }
}
