package net.minecraft.world.level;

/**
 * Stands in for the real {@code net.minecraft.world.level.Level} inside a Paper
 * jar. Its only job is to answer differently from the shim, so the spike can
 * tell which of the two a caller actually bound to.
 *
 * <p>The signature is what matters: the shim must be signature-identical, so
 * that {@code Caller} — compiled against <em>this</em> class — links against the
 * shim without recompilation. That is the whole premise of the bridge design.
 */
public class Level {
    public String getBlockName(int x, int y, int z) {
        return "REAL:" + x + "," + y + "," + z;
    }
}
