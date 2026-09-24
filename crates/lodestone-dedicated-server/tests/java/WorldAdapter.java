package lodestone.fixture;

public final class WorldAdapter {
    private static native int blockStateId(int x, int y, int z);
    private static int calls;

    public static void onTick(long tick) {
        if (calls++ == 0) {
            int state = blockStateId(11, 7, 13);
            if (state != 0) throw new AssertionError("expected fixture air, got " + state);
            System.out.println("LIVE-WORLD:tick=" + tick + " state=" + state);
        } else {
            blockStateId(1000000, 7, 1000000);
            throw new AssertionError("unavailable world query returned normally");
        }
    }

    public static void onBlockStateChanged(int x, int y, int z, int stateId) {
        // This world-read fixture does not request a mutation. The required
        // host callback remains a no-op so the adapter contract is complete.
    }

    public static void onPlayerJoined(long handle) {}

    public static void onPlayerDisconnected(long handle) {}
}
