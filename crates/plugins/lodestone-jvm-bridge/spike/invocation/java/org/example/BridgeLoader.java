package org.example;

import java.io.File;
import java.net.URL;
import java.net.URLClassLoader;

/** Loads the already-compiled user plugin against either the real class or our shim. */
public final class BridgeLoader {
    private static URLClassLoader loader;
    private static Class<?> caller;

    private BridgeLoader() {}

    public static Class<?> load(String real, String shim, String app, boolean useShim)
            throws Exception {
        File first = new File(useShim ? shim : real);
        loader = new URLClassLoader(
                new URL[] {first.toURI().toURL(), new File(app).toURI().toURL()},
                ClassLoader.getPlatformClassLoader());
        caller = Class.forName("org.example.Caller", true, loader);
        return Class.forName("net.minecraft.world.level.Level", true, loader);
    }

    public static String invoke(String method) throws Exception {
        return String.valueOf(caller.getMethod(method).invoke(null));
    }
}
