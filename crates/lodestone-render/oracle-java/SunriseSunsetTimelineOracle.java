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
 * Ground-truth dump of the three {@code Timelines.OVERWORLD_DAY} tracks issue
 * #96 needs, sampled through the real {@code Timeline.createTrackSampler}
 * machinery — the same {@code KeyframeTrackSampler} + {@code AttributeModifier}
 * chain the live client uses — once per tick of the 24000-tick day.
 *
 * Sibling of {@link SkyLightTimelineOracle}, which does the same for the two
 * lighting tracks issue #49 needs. Split rather than merged so that the #49
 * dump stays byte-stable while this one is added.
 *
 * <h2>Why each column is sampled the way it is</h2>
 *
 * The three tracks do not share a modifier, so they cannot share a base value:
 *
 * <ul>
 *   <li>{@code visual/sunrise_sunset_color} declares no {@code modifier} in
 *       {@code data/minecraft/timeline/day.json}, so it gets
 *       {@code AttributeModifier.override()} (see
 *       {@code AttributeTrack.createCodec}'s {@code optionalFieldOf}). Its
 *       attribute default is {@code 0}; an override ignores the base entirely,
 *       so sampling from the default yields the raw interpolated ARGB keyframe
 *       value.</li>
 *   <li>{@code visual/sky_color} and {@code visual/fog_color} declare
 *       {@code "modifier": "multiply"}, i.e. {@code ColorModifier.MULTIPLY_RGB
 *       = ARGB::multiply}, applied as {@code multiply(base, keyframe)}. Their
 *       attribute default is {@code 0} (black), and black times anything is
 *       black — so sampling from the default would dump a column of zeroes and
 *       prove nothing. These are sampled from {@code 0xFFFFFFFF} instead:
 *       {@code ARGB.multiply} short-circuits {@code lhs == -1} to {@code rhs},
 *       so a white base extracts the track's own per-tick multiplier, which is
 *       the value a biome's real {@code sky_color} is then multiplied by.</li>
 * </ul>
 *
 * A fourth column re-samples {@code sky_color} from {@code plains}' real
 * {@code minecraft:visual/sky_color} ({@code #78a7ff}) so the dump also pins
 * the gamma-space, byte-truncating {@code red*red/255} shape of
 * {@code ARGB.multiply} itself, not just the multiplier.
 *
 * Output, one line per sampled tick:
 *   {@code <tick> <sunriseSunsetArgb> <skyColorMultiplier> <fogColorMultiplier> <skyColorOverPlains>}
 *
 * every colour a raw 32-bit ARGB int, left-padded to 8 hex digits.
 */
public final class SunriseSunsetTimelineOracle {
    /** {@code plains}' {@code minecraft:visual/sky_color}, opaque. */
    private static final int PLAINS_SKY_COLOR = 0xFF78A7FF;

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

        var sunriseSampler =
                dayTimeline.value().createTrackSampler(EnvironmentAttributes.SUNRISE_SUNSET_COLOR, clockManager);
        var skySampler = dayTimeline.value().createTrackSampler(EnvironmentAttributes.SKY_COLOR, clockManager);
        var fogSampler = dayTimeline.value().createTrackSampler(EnvironmentAttributes.FOG_COLOR, clockManager);
        var skyOverPlainsSampler =
                dayTimeline.value().createTrackSampler(EnvironmentAttributes.SKY_COLOR, clockManager);

        StringBuilder sb = new StringBuilder();
        sb.append("# SunriseSunsetTimelineOracle: Timelines.OVERWORLD_DAY sampled every tick of the\n");
        sb.append("# 24000-tick day via the real Timeline/AttributeTrackSampler machinery.\n");
        sb.append("# <tick> <sunrise_sunset_argb> <sky_color_multiplier_argb> <fog_color_multiplier_argb>");
        sb.append(" <sky_color_over_plains_argb>\n");
        sb.append("# sunrise_sunset_color uses AttributeModifier.override() (no `modifier` in day.json),\n");
        sb.append("# sampled from its own default 0. sky_color/fog_color use MULTIPLY_RGB, sampled from\n");
        sb.append("# 0xffffffff to extract the multiplier (ARGB.multiply short-circuits lhs == -1).\n");
        sb.append("# The last column multiplies plains' real #78a7ff sky_color through the same track.\n");
        for (long tick = 0; tick < 24000; tick++) {
            tickBox[0] = tick;
            int sunrise = sunriseSampler.applyTimeBased(
                    EnvironmentAttributes.SUNRISE_SUNSET_COLOR.defaultValue(), (int) tick);
            int sky = skySampler.applyTimeBased(0xFFFFFFFF, (int) tick);
            int fog = fogSampler.applyTimeBased(0xFFFFFFFF, (int) tick);
            int skyOverPlains = skyOverPlainsSampler.applyTimeBased(PLAINS_SKY_COLOR, (int) tick);
            sb.append(tick)
                    .append(' ')
                    .append(String.format("%08x", sunrise))
                    .append(' ')
                    .append(String.format("%08x", sky))
                    .append(' ')
                    .append(String.format("%08x", fog))
                    .append(' ')
                    .append(String.format("%08x", skyOverPlains))
                    .append('\n');
        }
        System.out.print(sb);
    }
}
