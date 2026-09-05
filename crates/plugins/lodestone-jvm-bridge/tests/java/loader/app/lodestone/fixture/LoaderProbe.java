package lodestone.fixture;

import lodestone.target.World;

/** Compiled once against the real target; runtime paths select its definition. */
public final class LoaderProbe {
    private LoaderProbe() {}

    public static String source() {
        return World.source();
    }
}
