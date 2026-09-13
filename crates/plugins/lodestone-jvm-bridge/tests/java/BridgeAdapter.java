package lodestone.fixture;

/** Repository-owned adapter contract fixture; no Paper or game classes. */
public final class BridgeAdapter {
    private static native int blockStateId(int x, int y, int z);
    private static native int unregisteredBlockStateId(int x, int y, int z);
    private static native String playerHandleName(long handle);
    private static native String playerHandleUuid(long handle);
    private static native long playerHandleForUuid(String uuid);
    private static native long playerHandleForName(String name);
    private static native long playerHandleForNameIgnoringCase(String name);
    private static native long playerHandleForNamePrefix(String prefix);
    private static native long playerHandleForProfile(String name, String uuid);
    private static native long activePlayerHandleAt(int index);
    private static native int activePlayerCount();
    private static native boolean playerHandleIsActive(long handle);
    private static native boolean playerHandleIsRetained(long handle);
    private static native double playerHandleX(long handle);
    private static native double playerHandleY(long handle);
    private static native double playerHandleZ(long handle);
    private static native float playerHandleYaw(long handle);
    private static native float playerHandlePitch(long handle);
    private static native int playerHandleEntityId(long handle);
    private static native int playerHandleGameMode(long handle);
    private static native int playerHandleExperienceLevel(long handle);
    private static native int playerHandleExperiencePoints(long handle);
    private static native void playerHandleTeleport(long handle, double x, double y, double z);
    private static native void playerHandleDamage(long handle, float amount);
    private static native void playerHandleSendMessage(long handle, String message);
    private static native void playerHandleSetGameMode(long handle, int gameMode);
    private static native void playerHandleSetExperience(long handle, int level, int points);

    private static long playerHandle;
    private static boolean playerHandleSet;
    private static final String PLAYER_UUID = "07070707-0707-0707-0707-070707070707";

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
            if (activePlayerCount() != 1 || activePlayerHandleAt(0) != playerHandle) {
                throw new AssertionError("active player enumeration did not return the joined handle");
            }
            if (!"Alice".equals(playerHandleName(playerHandle))) {
                throw new AssertionError("unexpected player name");
            }
            if (!PLAYER_UUID.equals(playerHandleUuid(playerHandle))) {
                throw new AssertionError("unexpected player UUID");
            }
            if (playerHandleForUuid(PLAYER_UUID) != playerHandle
                    || playerHandleForName("Alice") != playerHandle
                    || playerHandleForNameIgnoringCase("alice") != playerHandle
                    || playerHandleForNamePrefix("Ali") != playerHandle
                    || playerHandleForProfile("Alice", PLAYER_UUID) != playerHandle) {
                throw new AssertionError("player resolver returned the wrong handle");
            }
            if (!playerHandleIsActive(playerHandle) || !playerHandleIsRetained(playerHandle)) {
                throw new AssertionError("joined player handle was not active and retained");
            }
            if (playerHandleX(playerHandle) != 12.5 || playerHandleY(playerHandle) != 64.0
                    || playerHandleZ(playerHandle) != -9.25 || playerHandleYaw(playerHandle) != 45.0f
                    || playerHandlePitch(playerHandle) != -12.0f || playerHandleEntityId(playerHandle) != 91
                    || playerHandleGameMode(playerHandle) != 2 || playerHandleExperienceLevel(playerHandle) != 7
                    || playerHandleExperiencePoints(playerHandle) != 23) {
                throw new AssertionError("player snapshot getter returned the wrong value");
            }
            playerHandleTeleport(playerHandle, 1.25, 65.5, -4.75);
            expectUnsupported("playerHandleDamage", () -> playerHandleDamage(playerHandle, 4.0f));
            expectUnsupported("playerHandleSendMessage", () -> playerHandleSendMessage(playerHandle, "hello"));
            expectUnsupported("playerHandleSetGameMode", () -> playerHandleSetGameMode(playerHandle, 1));
            expectUnsupported("playerHandleSetExperience", () -> playerHandleSetExperience(playerHandle, 8, 13));
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

    public static void onPlayerJoined(long handle) {
        if (playerHandleSet) {
            if (handle == playerHandle) {
                throw new AssertionError("reconnect reused the old player handle");
            }
            try {
                playerHandleName(playerHandle);
                throw new AssertionError("released player handle resolved after slot reuse");
            } catch (RuntimeException expected) {
                if (!expected.getMessage().contains("referenced object no longer exists")) {
                    throw new AssertionError("stale handle error did not name the lifetime failure", expected);
                }
            }
        }
        playerHandle = handle;
        playerHandleSet = true;
        if (!"Alice".equals(playerHandleName(handle)) || !PLAYER_UUID.equals(playerHandleUuid(handle))) {
            throw new AssertionError("join callback did not expose the copied player profile");
        }
    }

    public static void onPlayerDisconnected(long handle) {
        if (handle != playerHandle || !"Alice".equals(playerHandleName(handle))) {
            throw new AssertionError("disconnect callback did not retain the old player handle");
        }
    }

    private interface UnsupportedCall {
        void run();
    }

    private static void expectUnsupported(String member, UnsupportedCall call) {
        try {
            call.run();
            throw new AssertionError("unsupported player member resolved unexpectedly: " + member);
        } catch (UnsatisfiedLinkError expected) {
            if (expected.getMessage() == null || !expected.getMessage().contains(member)) {
                throw new AssertionError("unsupported member error did not name " + member, expected);
            }
        }
    }
}
