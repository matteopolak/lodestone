package bench;

/** Vanilla-jar stand-in: never called because SODIUM is false when the sodium mod is absent. */
final class SodiumProbe {
    private SodiumProbe() {}
    static double[] counts() { return null; }
}
