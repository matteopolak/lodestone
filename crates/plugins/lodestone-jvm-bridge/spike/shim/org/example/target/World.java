package org.example.target;

/**
 * The bridge's replacement for the target class.
 *
 * <p>Signature-identical to the real class by construction — that is the
 * contract that lets Paper's already-compiled bytecode bind to it with no
 * recompilation and no bytecode modification of Paper itself.
 *
 * <p>Eight methods, deliberately:
 *
 * <ul>
 *   <li>{@link #getBlockName} answers from pure Java, so the spike can prove
 *       interception happened without needing a native library present.
 *   <li>{@link #nativeBlockName} is declared {@code native} and returns text
 *       from the Rust side through the real bridge. With no library loaded,
 *       calling it must raise {@link UnsatisfiedLinkError} — evidence that the
 *       JNI seam is genuinely reachable from an intercepted class.
 *   <li>{@link #nativeBlockStateId} is a second {@code native} member with an
 *       integer return descriptor, exercising multi-method registration and
 *       primitive return marshalling.
 *   <li>{@link #nativeAcquireBlockHandle}, {@link #nativeReadBlockHandle} and
 *       {@link #nativeReleaseBlockHandle} carry an opaque generational handle
 *       across separate callbacks and report invalidation without dangling.
 *   <li>{@link #nativeReentrantDepth} and {@link #invokeReentrantDepth} form a
 *       bounded native-to-Java-to-native recursion control.
 * </ul>
 */
public class World {
    public String getBlockName(int x, int y, int z) {
        return "SHIM:" + x + "," + y + "," + z;
    }

    public native String nativeBlockName(int x, int y, int z);

    public native int nativeBlockStateId(int x, int y, int z);

    public native long nativeAcquireBlockHandle(int x, int y, int z);

    public native String nativeReadBlockHandle(long handle);

    public native int nativeReleaseBlockHandle(long handle);

    public static native String nativeReentrantDepth(int remaining);

    public static String invokeReentrantDepth(int remaining) {
        return nativeReentrantDepth(remaining);
    }
}
