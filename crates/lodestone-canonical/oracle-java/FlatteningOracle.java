import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.Optional;

import com.mojang.datafixers.Dynamic;

/**
 * Authoritative {@code id:meta} (pre-Flattening, protocol 340 / Minecraft
 * 1.12.2) &rarr; modern block-state extractor. Unlike every other oracle in
 * this repository, this one does <em>not</em> boot a server and walk a live
 * registry (there is no server-bootstrap call at all here) — the 1.12.2/1.13.2
 * jars have no such bootstrap to call, and the mapping this program reads is
 * not a registry at all, it is a single {@code static} initializer inside one
 * class of the 1.13.2 server jar's own world-upgrade machinery (the exact
 * conversion vanilla itself runs on old worlds).
 *
 * <h2>Why the class is referenced by the single letter {@code "yp"}</h2>
 *
 * Mojang started shipping official (Mojang-mapped) obfuscation maps at
 * 1.14.4 (see {@code docs/version-table.md}: "the jar-{@code version.json}
 * boundary is empirically 1.13.2 &rarr; 1.14.4", the same boundary applies to
 * mappings). The 1.13.2 server jar predates that, so every game class
 * (everything except the open-source {@code com.mojang.datafixers} package
 * itself, which ships with real names because it is a separate open-source
 * library) is obfuscated to a short, meaningless, jar-build-specific name.
 * {@code "yp"} is simply what class-file extraction plus
 * {@code javap -p yp.class} shows for <em>this exact, SHA-256-pinned jar</em>
 * (see {@code docs/version-1-12-2-flattening-table.md}); it is stable for
 * this one artifact and would need to be rediscovered from scratch for any
 * other build. The discovery method (documented in full in the doc, summarized
 * here for anyone who has to repeat it):
 *
 * <ol>
 *   <li>Extract every {@code *.class} from {@code server.jar}.</li>
 *   <li>{@code grep} the raw class bytes (constant-pool strings are never
 *       obfuscated, only identifiers are) for distinctive pre-Flattening
 *       block names that only ever appear in the flattening table, e.g.
 *       {@code minecraft:log2} and {@code minecraft:double_plant}.</li>
 *   <li>Intersect the hits; decompile the surviving candidates (CFR was used
 *       here) and read the {@code static} initializer.</li>
 * </ol>
 *
 * <p>The class exposes (after decompilation) three static members relevant
 * here: a private {@code Dynamic<?>[] b} of length 4095 indexed by
 * {@code (oldBlockId << 4) | meta} holding the resolved modern state (or
 * {@code null} if vanilla's own table never assigned that slot — reflectively
 * inspected below <em>as null</em>, not coerced to any fallback value, which
 * is the whole reason this program reads the field directly instead of
 * calling the public {@code b(int)} accessor: {@code b(int)} silently
 * substitutes index 0 (air) for anything undefined, exactly the
 * ambiguity-hiding behaviour {@code CLAUDE.md} warns against, and this table
 * exists specifically to keep that decision visible instead of replicating
 * it).
 *
 * <h2>What this table is (and is not) authoritative for</h2>
 *
 * This is vanilla's own <em>first</em> flattening step. It resolves block
 * <em>identity</em> (which post-Flattening block family an old id:meta names)
 * reliably, but a handful of resolved names/property keys are the
 * <strong>intermediate</strong> 18w-snapshot-era spelling used inside this one
 * world-upgrade schema step, not always 1.13.2's final release
 * spelling — e.g. this table resolves leaves to properties
 * {@code decayable}/{@code check_decay} (confirmed live: {@code "persistent"}
 * and {@code "distance"}, 1.13.2's actual final leaf-property names, do not
 * appear anywhere in the 1.13.2 server jar's bytes at all), and several block
 * names it produces ({@code mob_spawner}, {@code melon_block}, {@code portal},
 * {@code oak_bark}/{@code spruce_bark}/...) are pre-rename spellings later
 * superseded by {@code spawner}, {@code melon}, {@code nether_portal},
 * {@code oak_wood}/{@code spruce_wood}/... — separate, unrelated rename fixes
 * chained later in the same world-upgrade pipeline that this program
 * does not chase down (out of scope for this task; see the doc's "What
 * wiring `v340` would need" section).
 *
 * <p>It is also, by vanilla's own design, unable to resolve some blocks at
 * all from id:meta alone — flower pots (contents live in the pot's
 * block entity, not the block's own metadata), skulls (type/rotation likewise
 * block-entity-only; several skull metas resolve to the literal Mojang
 * placeholder string {@code "%%FILTER_ME%%"} in this table, preserved as-is
 * rather than translated to something that looks like a real block name),
 * and the upper half of double plants (species is read from the paired lower
 * half at conversion time, not stored in the upper half's own meta at all).
 * These are reported, not silently resolved, in the generated table (see
 * {@code crates/lodestone-canonical/tests/flattening.rs}).
 *
 * <h2>Output format</h2>
 *
 * One line per array index {@code n} (0..4094 inclusive; the array itself is
 * only 4095 long, one short of the naive {@code 256 * 16 = 4096} id:meta
 * space — {@code id 255 (structure_block), meta 15} is structurally
 * unreachable through this table, a genuine vanilla off-by-one, not a bug in
 * this program):
 *
 * <pre>{@code
 * <n>                                   (no table entry — vanilla itself has no answer)
 * <n>\t<name>                           (no Properties)
 * <n>\t<name>\t<k1=v1,k2=v2,...>        (Properties, keys sorted for determinism)
 * }</pre>
 *
 * Properties are read via {@code Dynamic.get}/{@code getMapValues} and sorted
 * before printing rather than relying on {@code Dynamic.toString()}, because
 * the latter's key order follows the backing map's iteration order, which is
 * not guaranteed stable across JVM builds — sorting keeps the committed dump
 * byte-for-byte reproducible.
 */
public final class FlatteningOracle {
    public static void main(String[] args) throws Exception {
        Class<?> ypClass = Class.forName("yp");
        Field tableField = ypClass.getDeclaredField("b");
        tableField.setAccessible(true);
        Dynamic<?>[] table = (Dynamic<?>[]) tableField.get(null);

        StringBuilder sb = new StringBuilder();
        sb.append("# array length = ").append(table.length).append('\n');
        for (int n = 0; n < table.length; n++) {
            Dynamic<?> dyn = table[n];
            if (dyn == null) {
                sb.append(n).append('\n');
                continue;
            }
            String name = dyn.get("Name").flatMap(Dynamic::getStringValue).orElse("<no-name>");
            List<String> props = extractSortedProperties(dyn);
            sb.append(n).append('\t').append(name);
            if (!props.isEmpty()) {
                sb.append('\t').append(String.join(",", props));
            }
            sb.append('\n');
        }
        System.out.print(sb);
    }

    private static <T> List<String> extractSortedProperties(Dynamic<T> dyn) {
        List<String> props = new ArrayList<>();
        Optional<Dynamic<T>> propsDyn = dyn.get("Properties");
        if (propsDyn.isPresent()) {
            Map<Dynamic<T>, Dynamic<T>> mapValues = propsDyn.get().getMapValues().orElseThrow();
            for (Map.Entry<Dynamic<T>, Dynamic<T>> e : mapValues.entrySet()) {
                String k = e.getKey().getStringValue().orElseThrow();
                String v = e.getValue().getStringValue().orElseThrow();
                props.add(k + "=" + v);
            }
        }
        props.sort(Comparator.naturalOrder());
        return props;
    }
}
