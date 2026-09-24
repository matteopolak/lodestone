import java.io.File;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;

/**
 * The classload-interception spike.
 *
 * <p><b>The claim under test.</b> The Java-plugin bridge's whole design rests on
 * one mechanism: that a target package can be redirected to
 * signature-identical shim classes <em>at classload time</em>, so that Paper's
 * already-compiled bytecode calls into us with no bytecode modification and no
 * redistribution of Paper. Everything else in the design is downstream of that
 * working. It had never been executed here, so it was an assumption.
 *
 * <p><b>The A/B.</b> {@code org.example.Caller} is compiled <em>once</em>,
 * against the real {@code World}, and never recompiled. It is then loaded twice
 * through two loaders that differ in exactly one element of their search path:
 *
 * <ul>
 *   <li><b>control arm</b> — {@code [real, app]}: must answer {@code REAL}. This
 *       is what proves the harness can distinguish the two at all; without it a
 *       shim that was never reached and a shim that was would look identical.
 *   <li><b>test arm</b> — {@code [shim, app]}: must answer {@code SHIM}.
 * </ul>
 *
 * <p>Both loaders take the <b>platform</b> class loader as parent, not the
 * system one. That is load-bearing and is the detail most likely to be got
 * wrong: with the system loader as parent, ordinary parent-first delegation
 * would find whichever {@code World} is on the application classpath and the
 * test arm would silently answer {@code REAL} — an interception that appears to
 * fail for a reason that has nothing to do with interception. The platform
 * loader sees JDK modules only, so the target package genuinely cannot
 * resolve above us.
 *
 * <p><b>The native seam.</b> The shim additionally declares a {@code native}
 * method with no library loaded. Reaching it must raise
 * {@link UnsatisfiedLinkError} — evidence that an intercepted class really can
 * carry the JNI entry point the Rust side will attach to, rather than a claim
 * that it could.
 */
public final class Spike {
    private Spike() {}

    private static URLClassLoader loader(File... dirs) throws Exception {
        URL[] urls = new URL[dirs.length];
        for (int i = 0; i < dirs.length; i++) {
            urls[i] = dirs[i].toURI().toURL();
        }
        // Platform parent, not system -- see the class doc.
        return new URLClassLoader(urls, ClassLoader.getPlatformClassLoader());
    }

    private static String call(URLClassLoader loader, String method) throws Exception {
        Class<?> caller = Class.forName("org.example.Caller", true, loader);
        // Sanity: the class must genuinely come from OUR loader, not from an
        // ancestor. A silently delegated load would make both arms agree.
        if (caller.getClassLoader() != loader) {
            return "HARNESS ERROR: Caller was loaded by " + caller.getClassLoader();
        }
        Method m = caller.getMethod(method);
        return String.valueOf(m.invoke(null));
    }

    public static void main(String[] args) throws Exception {
        File real = new File(args[0]);
        File shim = new File(args[1]);
        File app = new File(args[2]);

        String control = call(loader(real, app), "describe");
        String test = call(loader(shim, app), "describe");
        String nativeSeam = call(loader(shim, app), "describeNative");

        System.out.println("control arm [real, app]: " + control);
        System.out.println("test arm    [shim, app]: " + test);
        System.out.println("native seam (shim)     : " + nativeSeam);

        boolean controlOk = control.startsWith("REAL:");
        boolean testOk = test.startsWith("SHIM:");
        boolean seamOk = nativeSeam.startsWith("UnsatisfiedLinkError");

        System.out.println();
        System.out.println("control shows NO interception : " + (controlOk ? "PASS" : "FAIL"));
        System.out.println("test    shows interception    : " + (testOk ? "PASS" : "FAIL"));
        System.out.println("native seam reachable         : " + (seamOk ? "PASS" : "FAIL"));

        if (!controlOk || !testOk || !seamOk) {
            System.out.println();
            System.out.println("SPIKE FAILED");
            System.exit(1);
        }
        System.out.println();
        System.out.println("SPIKE PASSED");
    }
}
