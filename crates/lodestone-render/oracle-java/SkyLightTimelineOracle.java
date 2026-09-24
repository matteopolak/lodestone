import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.attribute.EnvironmentAttributes;
import net.minecraft.world.clock.ClockManager;
import net.minecraft.world.clock.WorldClock;
import net.minecraft.world.clock.WorldClocks;
import net.minecraft.world.timeline.Timeline;
import net.minecraft.world.timeline.Timelines;

/**
 * Ground-truth dump of the real 26.2 {@code Timelines.OVERWORLD_DAY} timeline
 * for {@code EnvironmentAttributes.SKY_LIGHT_FACTOR} and
 * {@code EnvironmentAttributes.SKY_LIGHT_COLOR} — the timeline track that
 * replaced {@code Level.getSkyDarken} (issue #49). Boots the real registries
 * (the timeline registry is data-driven, bootstrapped by
 * {@code Timelines::bootstrap} via {@code VanillaRegistries}, exactly like the
 * worldgen density/noise registries in {@code scripts/worldgen-oracle}) and
 * samples every tick of the 24000-tick day directly through
 * {@code Timeline.createTrackSampler}, i.e. through the exact same
 * {@code KeyframeTrackSampler} + {@code AttributeModifier} machinery the real
 * client uses — not a hand re-derivation of the interpolation math.
 *
 * A trivial {@link ClockManager} stands in for the live clock: it hands back
 * whatever tick the caller asks for, since the overworld clock is the only
 * one this timeline reads and we drive it directly rather than through a
 * running world.
 *
 * Output, one line per sampled tick:
 *   {@code <tick> <skyLightFactorBits> <skyLightColorARGB>}
 *
 * `skyLightFactorBits` is the raw hex `Float.floatToRawIntBits` pattern (no
 * precision loss in the text round-trip); `skyLightColorARGB` is the raw
 * 32-bit ARGB int in hex, sign included via `Integer.toHexString`'s unsigned
 * semantics (always 8 hex digits after left-padding).
 */
public final class SkyLightTimelineOracle {
    public static void main(String[] args) {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider registries = VanillaRegistries.createLookup();
        HolderLookup<Timeline> timelines = registries.lookupOrThrow(Registries.TIMELINE);
        HolderLookup<WorldClock> clocks = registries.lookupOrThrow(Registries.WORLD_CLOCK);
        Holder<WorldClock> overworld = clocks.getOrThrow(WorldClocks.OVERWORLD);
        Holder<Timeline> dayTimeline = timelines.getOrThrow(Timelines.OVERWORLD_DAY);

        long[] tickBox = new long[1];
        ClockManager clockManager = definition -> tickBox[0];

        var factorSampler = dayTimeline.value().createTrackSampler(EnvironmentAttributes.SKY_LIGHT_FACTOR, clockManager);
        var colorSampler = dayTimeline.value().createTrackSampler(EnvironmentAttributes.SKY_LIGHT_COLOR, clockManager);

        StringBuilder sb = new StringBuilder();
        sb.append("# SkyLightTimelineOracle: Timelines.OVERWORLD_DAY sampled every tick of the\n");
        sb.append("# 24000-tick day via the real Timeline/AttributeTrackSampler machinery.\n");
        sb.append("# <tick> <sky_light_factor_f32_bits_hex> <sky_light_color_argb_hex>\n");
        for (long tick = 0; tick < 24000; tick++) {
            tickBox[0] = tick;
            float factor = factorSampler.applyTimeBased(EnvironmentAttributes.SKY_LIGHT_FACTOR.defaultValue(), (int) tick);
            int color = colorSampler.applyTimeBased(EnvironmentAttributes.SKY_LIGHT_COLOR.defaultValue(), (int) tick);
            sb.append(tick).append(' ')
                    .append(Integer.toHexString(Float.floatToRawIntBits(factor))).append(' ')
                    .append(String.format("%08x", color))
                    .append('\n');
        }
        System.out.print(sb);
    }
}
