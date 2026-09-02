package net.minecraft.world.level;

/**
 * The bridge's replacement for {@code net.minecraft.world.level.Level}.
 *
 * <p>Signature-identical to the real class by construction — that is the
 * contract that lets Paper's already-compiled bytecode bind to it with no
 * recompilation and no bytecode modification of Paper itself.
 *
 * <p>Two methods, deliberately:
 *
 * <ul>
 *   <li>{@link #getBlockName} answers from pure Java, so the spike can prove
 *       interception happened without needing a native library present.
 *   <li>{@link #nativeBlockName} is declared {@code native} and is where the
 *       Rust side attaches in the real bridge. With no library loaded, calling
 *       it must raise {@link UnsatisfiedLinkError} — which is the evidence that
 *       the JNI seam is genuinely reachable from an intercepted class, rather
 *       than something merely asserted about it.
 * </ul>
 */
public class Level {
    public String getBlockName(int x, int y, int z) {
        return "SHIM:" + x + "," + y + "," + z;
    }

    public native String nativeBlockName(int x, int y, int z);
}
