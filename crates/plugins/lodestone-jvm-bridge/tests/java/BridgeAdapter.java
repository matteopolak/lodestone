package lodestone.fixture;

/** Repository-owned adapter contract fixture; no Paper or game classes. */
public final class BridgeAdapter {
    private static native int blockStateId(int x, int y, int z);
    private static native int unregisteredBlockStateId(int x, int y, int z);

    public static void onTick(long tick) {
        if (tick == 37) {
            if (BridgeAdapter.class.getClassLoader() == ClassLoader.getSystemClassLoader()) {
                throw new AssertionError("adapter unexpectedly used the system class loader");
            }
            int state = blockStateId(11, 7, -3);
            if (state != 422) {
                throw new AssertionError("expected state 422, got " + state);
            }
            try {
                unregisteredBlockStateId(11, 7, -3);
                throw new AssertionError("unregistered method resolved unexpectedly");
            } catch (UnsatisfiedLinkError expected) {
                // The successful query above requires explicit registration.
            }
            final Throwable[] failure = new Throwable[1];
            Thread other = new Thread(() -> {
                try {
                    blockStateId(11, 7, -3);
                    failure[0] = new AssertionError("query escaped the worker-thread boundary");
                } catch (RuntimeException expected) {
                    if (!expected.getMessage().contains("requires the adapter worker thread")) {
                        failure[0] = expected;
                    }
                } catch (Throwable unexpected) {
                    failure[0] = unexpected;
                }
            });
            other.start();
            try {
                other.join(1000);
            } catch (InterruptedException interrupted) {
                throw new AssertionError(interrupted);
            }
            if (other.isAlive() || failure[0] != null) {
                throw new AssertionError("foreign-thread query did not fail correctly", failure[0]);
            }
        } else if (tick == 38) {
            blockStateId(-19, 5, 23);
            throw new AssertionError("unavailable block query returned normally");
        } else {
            throw new IllegalArgumentException("unexpected tick " + tick);
        }
    }

    public static void onBlockStateChanged(int x, int y, int z, int stateId) {
        if (x != -17 || y != 64 || z != 33 || stateId != 1234) {
            throw new AssertionError("unexpected host block-change callback");
        }
    }

    public static void onPlayerJoined(long handle) {}

    public static void onPlayerDisconnected(long handle) {}
}
