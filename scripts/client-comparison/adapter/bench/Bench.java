package bench;

import com.mojang.renderpearl.api.device.GpuSurface;
import java.io.BufferedWriter;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.time.Instant;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import net.fabricmc.loader.api.FabricLoader;
import net.fabricmc.api.ClientModInitializer;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Screenshot;
import net.minecraft.server.level.ChunkTrackingView;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.EmptyLevelChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;

public final class Bench implements ClientModInitializer {
    private static final long ORIGIN = System.nanoTime(), SECOND = 1_000_000_000L;
    private static final String SCREENSHOT_NAME = "bench-settled-" + ORIGIN + ".png";
    private static final long[] PRESENTS = new long[65536];
    private static final int[] PRESENT_OBSERVATION = new int[65536];
    private static final double[][] OBS = new double[2048][35];
    private static final StringBuilder MARKERS = new StringBuilder("[\n");
    private static final String HEADER = "elapsed_ns,phase,camera_x,camera_y,camera_z,yaw,pitch,feet_x,feet_y,feet_z,feet_yaw,feet_pitch,eye_height,loaded_chunks,coverage_chunks,rendered_sections,total_sections,scheduled_jobs,busy_workers,pending_uploads,surface_width,surface_height,target_width,target_height,logical_width,logical_height,effective_cap,requested_cap,focused,iconified,minimized,throttle_reason,present_mode,effective_distance,vsync";
    private static final String[] OBSERVATION_NAMES = HEADER.split(",");
    private static final boolean SODIUM = FabricLoader.getInstance().isModLoaded("sodium");
    private static Path output;
    private static volatile Path archive;
    private static volatile boolean screenshotReturned;
    private static int phase, presentCount, observationCount, minimumChunks, radius, stallMs;
    private static int expectedCoverage, expectedWidth, expectedHeight, expectedDistance, durationSeconds, deadlineSeconds;
    private static int faultOmitCoverageIndex;
    private static List<ChunkPos> trackingDomain;
    private static boolean[] trackingLoaded;
    private static int trackingCenterX, trackingCenterZ, trackingDistance;
    private static boolean trackingInventoryWritten, trackingLoadedWitnessWritten, coverageFaultMarked;
    private static double feetX, feetY, feetZ, yaw, pitch;
    private static double[] recordingBaseline;
    private static long joinedAt, stableAt, nextObservation, recordingStart, recordingEnd, recordingDeadline, recordingCompletedAt;
    private static long nextLoopReadiness, nextPresentReadiness, loopHookCalls, presentHookCalls;
    private static long nextRejectedReadiness;
    private static boolean profiler;
    private static boolean initialized, finished, screenshotRequested, archiveMarked, screenshotMarked, stalled;
    private static String failure = "", backend = "", screenshotFeedback = "";

    @Override public void onInitializeClient() {
        output = Path.of(System.getProperty("bench.output", "bench-output")).toAbsolutePath();
        minimumChunks = integerProperty("bench.minimumChunks", 329);
        radius = integerProperty("bench.coverageRadius", 8);
        expectedCoverage = integerProperty("bench.expectedCoverage", 329);
        expectedWidth = integerProperty("bench.expectedWidth", 2560);
        expectedHeight = integerProperty("bench.expectedHeight", 1440);
        expectedDistance = integerProperty("bench.expectedRenderDistance", 8);
        durationSeconds = integerProperty("bench.durationSeconds", 10);
        deadlineSeconds = integerProperty("bench.deadlineSeconds", 120);
        String profilerProperty = System.getProperty("bench.profiler", "false");
        if (!profilerProperty.equals("true") && !profilerProperty.equals("false"))
            throw new IllegalArgumentException("bench.profiler must be true or false");
        profiler = Boolean.parseBoolean(profilerProperty);
        feetX = decimalProperty("bench.feetX", 0);
        feetY = decimalProperty("bench.feetY", 140);
        feetZ = decimalProperty("bench.feetZ", 0);
        yaw = decimalProperty("bench.yaw", 0);
        pitch = decimalProperty("bench.pitch", 10);
        stallMs = integerProperty("bench.stallMs", 0);
        faultOmitCoverageIndex = integerProperty("bench.faultOmitCoverageIndex", -1);
        if (radius != expectedDistance || radius < 1 || radius > 32 || expectedCoverage < 1
            || (expectedDistance == 8 && expectedCoverage != 329)
            || minimumChunks < expectedCoverage || expectedWidth < 1 || expectedHeight < 1 || expectedDistance < 1
            || (durationSeconds != 3 && durationSeconds != 10 && durationSeconds != 30 && durationSeconds != 60)
            || (profiler && durationSeconds != 10) || deadlineSeconds < 20 + durationSeconds + 10 || deadlineSeconds > 180
            || pitch < -90 || pitch > 90 || (stallMs != 0 && stallMs != 100)
            || faultOmitCoverageIndex < -1 || faultOmitCoverageIndex >= expectedCoverage)
            throw new IllegalArgumentException("Invalid benchmark parameters; full tracking domain at R8 is 329, radius must match expected distance, duration 3/10/30/60, profiler duration 10, deadline 20+duration+10..180, stallMs 0/100, fault index -1 or within domain");
        try {
            Files.createDirectories(output);
            for (String artifact : new String[]{"run.json", "presents.csv", "observations.csv", "markers.json", "observed-options.txt", "tracking-domain.json", "tracking-domain.csv", "tracking-loaded-witness.csv"})
                if (Files.exists(output.resolve(artifact))) throw new IOException("Benchmark output already contains " + artifact);
        } catch (IOException e) { throw new IllegalStateException(e); }
        initialized = true;
        marker("boot");
    }

    private static int integerProperty(String name, int fallback) {
        return Integer.parseInt(System.getProperty(name, Integer.toString(fallback)));
    }

    private static double decimalProperty(String name, double fallback) {
        double value = Double.parseDouble(System.getProperty(name, Double.toString(fallback)));
        if (!Double.isFinite(value)) throw new IllegalArgumentException(name + " must be finite");
        return value;
    }

    public static void beforeLoop(Minecraft mc) {
        if (!initialized || finished) return;
        try {
            long now = System.nanoTime();
            if (now - ORIGIN > deadlineSeconds * SECOND) { finish(mc, "deadline_exceeded"); return; }
            preJoinReadiness(mc, mc.windowSurface(), true, now);
            if (phase == 3) {
                if (mc.getMetricsRecorder().isRecording()) { finish(mc, "profiler_already_recording"); return; }
                if (profiler) {
                    marker("profiler_start_requested");
                    if (!mc.debugClientMetricsStart(feedback -> {})) { finish(mc, "profiler_start_failed"); return; }
                }
                recordingBaseline = OBS[observationCount - 1];
                recordingStart = System.nanoTime();
                recordingDeadline = recordingStart + durationSeconds * SECOND;
                phase = 4;
                marker("recording_start");
            } else if (phase == 4) {
                if (!profiler && mc.getMetricsRecorder().isRecording()) { finish(mc, "unexpected_profiler_recording"); return; }
                if ((profiler && !mc.getMetricsRecorder().isRecording()) || (!profiler && now >= recordingDeadline)) {
                    recordingEnd = profiler ? now : recordingDeadline;
                    recordingCompletedAt = now;
                    phase = 5;
                    marker("recording_end");
                }
            }
            if (phase == 5 && archive != null && !archiveMarked) { archiveMarked = true; marker("profile_zip_ready"); }
            if (phase == 5 && screenshotReturned && !screenshotMarked) { screenshotMarked = true; marker("screenshot_ready"); }
            if (phase == 5 && (!profiler || archiveMarked) && screenshotMarked) {
                if (profiler && (!Files.isRegularFile(archive) || Files.size(archive) == 0)) failure = "missing_profile_zip";
                Path png = mc.gameDirectory.toPath().resolve("screenshots").resolve(SCREENSHOT_NAME);
                if (!Files.isRegularFile(png) || Files.size(png) == 0) failure = "missing_screenshot";
                if (presentCount < 2) failure = "insufficient_presents";
                finish(mc, failure);
            }
        } catch (Exception e) { finish(mc, "loop_error: " + e); }
    }

    public static void afterPresent(GpuSurface surface) {
        long now = System.nanoTime();
        if (!initialized || finished) return;
        Minecraft mc = Minecraft.getInstance();
        try {
            preJoinReadiness(mc, surface, false, now);
            if (surface != mc.windowSurface()) return;
            if (phase == 4 && (profiler || now < recordingDeadline)) {
                if (presentCount == PRESENTS.length) { finish(mc, "present_buffer_overflow"); return; }
                PRESENTS[presentCount] = now - ORIGIN;
                PRESENT_OBSERVATION[presentCount++] = observationCount - 1;
            }
            if (mc.level == null || mc.player == null || mc.gui.screen() != null || mc.gui.overlay() != null) {
                rejectReadiness(now, ",\"condition\":\"world_and_gui_ready\",\"level_present\":"+(mc.level!=null)
                    +",\"player_present\":"+(mc.player!=null)+",\"screen_class\":"+quote(mc.gui.screen()==null?"":mc.gui.screen().getClass().getName())
                    +",\"overlay_class\":"+quote(mc.gui.overlay()==null?"":mc.gui.overlay().getClass().getName()));
                stableAt = 0;
                if (phase >= 4 && failure.isEmpty()) failure = "world_or_overlay_changed";
                return;
            }
            if (phase == 0) { phase = 1; joinedAt = now; backend = surface.getClass().getName(); marker("joined"); }
            if (now >= nextObservation) {
                nextObservation = now + SECOND / 10;
                boolean settled = observe(mc, surface, now);
                if (phase <= 2) {
                    phase = 2;
                    if (!settled) stableAt = 0;
                    else if (stableAt == 0) { stableAt = now; marker("pose_and_geometry_stable"); }
                    if (stableAt != 0 && now - stableAt >= 5 * SECOND && now - joinedAt >= 20 * SECOND) {
                        phase = 3;
                        marker("settled");
                    }
                } else if (!settled && phase == 4 && failure.isEmpty()) failure = "measurement_state_changed";
            }
            if (phase == 4 && stallMs != 0 && !stalled && now - recordingStart >= 2 * SECOND) {
                stalled = true;
                marker("synthetic_stall_start");
                Thread.sleep(stallMs);
                marker("synthetic_stall_end");
            }
            if (phase == 5 && !screenshotRequested) {
                screenshotRequested = true;
                marker("screenshot_requested");
                Screenshot.grab(mc.gameDirectory, SCREENSHOT_NAME, mc.gameRenderer.mainRenderTarget(), 1,
                    feedback -> { screenshotFeedback = feedback.getString(); screenshotReturned = true; });
            }
        } catch (Exception e) { finish(mc, "present_error: " + e); }
    }

    private static void preJoinReadiness(Minecraft mc, GpuSurface surface, boolean loopHook, long now) {
        if (phase != 0) return;
        if (loopHook) {
            loopHookCalls++;
            if (now < nextLoopReadiness) return;
            nextLoopReadiness = now + SECOND;
        } else {
            presentHookCalls++;
            if (now < nextPresentReadiness) return;
            nextPresentReadiness = now + SECOND;
        }
        var mainSurface = mc.windowSurface();
        var screen = mc.gui.screen();
        var overlay = mc.gui.overlay();
        marker("pre_join_readiness", ",\"hook\":"+quote(loopHook?"beforeLoop":"afterPresent")
            +",\"level_present\":"+(mc.level!=null)+",\"player_present\":"+(mc.player!=null)
            +",\"screen_class\":"+quote(screen==null?"":screen.getClass().getName())
            +",\"overlay_class\":"+quote(overlay==null?"":overlay.getClass().getName())
            +",\"surface_present\":"+(surface!=null)+",\"main_surface_present\":"+(mainSurface!=null)
            +",\"surface_is_main\":"+(surface!=null&&surface==mainSurface)
            +",\"surface_identity\":"+System.identityHashCode(surface)+",\"main_surface_identity\":"+System.identityHashCode(mainSurface)
            +",\"surface_config_available\":"+(surface!=null&&surface.currentConfiguration().isPresent())
            +",\"main_surface_config_available\":"+(mainSurface!=null&&mainSurface.currentConfiguration().isPresent())
            +",\"loop_hook_calls\":"+loopHookCalls+",\"present_hook_calls\":"+presentHookCalls);
    }

    private static boolean rejectedReadinessDue(long now) {
        if (phase == 0 || phase > 4 || now < nextRejectedReadiness) return false;
        nextRejectedReadiness = now + SECOND;
        return true;
    }

    private static String readinessContext(long now) {
        return ",\"phase\":"+phase+",\"observation_count\":"+observationCount+",\"joined_age_ns\":"+(now-joinedAt)
            +",\"stable_age_ns\":"+(stableAt==0?0:now-stableAt);
    }

    private static void rejectReadiness(long now, String fields) {
        if (rejectedReadinessDue(now)) marker("readiness_rejected", readinessContext(now)+fields);
    }

    private static String observedState(double[] observed) {
        var values = new StringBuilder("{");
        for (int i = 0; i < observed.length; i++) {
            if (i > 0) values.append(',');
            values.append(quote(OBSERVATION_NAMES[i])).append(':').append(jsonNumber(observed[i]));
        }
        return values.append('}').toString();
    }

    private static String changedFields(double[] observed, double[] previous) {
        var changes = new StringBuilder("[");
        for (int i = 2; i < observed.length; i++) {
            if (Math.abs(observed[i] - previous[i]) <= 0.0001) continue;
            if (changes.length() > 1) changes.append(',');
            changes.append("{\"field\":").append(quote(OBSERVATION_NAMES[i])).append(",\"previous\":").append(jsonNumber(previous[i]))
                .append(",\"current\":").append(jsonNumber(observed[i])).append(",\"delta\":").append(jsonNumber(observed[i]-previous[i])).append('}');
        }
        return changes.append(']').toString();
    }

    private static void initializeTrackingDomain(int observedDistance) throws IOException {
        trackingCenterX = (int)Math.floor(feetX / 16);
        trackingCenterZ = (int)Math.floor(feetZ / 16);
        trackingDistance = observedDistance;
        var positions = new ArrayList<ChunkPos>();
        ChunkTrackingView.of(new ChunkPos(trackingCenterX, trackingCenterZ), observedDistance).forEach(positions::add);
        if (positions.size() != expectedCoverage || new HashSet<>(positions).size() != expectedCoverage)
            throw new IllegalStateException("tracking_domain_cardinality_mismatch: expected " + expectedCoverage + ", observed " + positions.size());
        trackingDomain = List.copyOf(positions);
        trackingLoaded = new boolean[trackingDomain.size()];
        try (var w = writer("tracking-domain.json")) {
            w.write("{\"source\":\"reference jar ChunkTrackingView.of(center, observed effective distance).forEach\",\"include_neighbors\":true"
                +",\"center_x\":"+trackingCenterX+",\"center_z\":"+trackingCenterZ+",\"observed_distance\":"+trackingDistance
                +",\"expected_coordinates\":"+expectedCoverage+",\"enumerated_coordinates\":"+trackingDomain.size()
                +",\"r8_independent_arithmetic_control_count\":329,\"fault_omit_coverage_index\":"+faultOmitCoverageIndex+"}\n");
        }
        marker("tracking_domain_ready");
    }

    private static void writeTrackingWitness(String name, long now) throws IOException {
        try (var w = writer(name)) {
            w.write("index,chunk_x,chunk_z,loaded,elapsed_ns\n");
            for (int i = 0; i < trackingDomain.size(); i++) {
                ChunkPos pos = trackingDomain.get(i);
                w.write(i+","+pos.x()+","+pos.z()+","+(trackingLoaded[i]?1:0)+","+(now-ORIGIN)+"\n");
            }
        }
    }

    private static boolean observe(Minecraft mc, GpuSurface surface, long now) throws IOException {
        if (observationCount == OBS.length) throw new IllegalStateException("observation_buffer_overflow");
        double[] render = SODIUM ? SodiumProbe.counts() : vanillaCounts(mc);
        if (render == null) { rejectReadiness(now, ",\"condition\":\"renderer_present\",\"renderer_present\":false"); return false; }
        if (surface.currentConfiguration().isEmpty()) {
            rejectReadiness(now, ",\"condition\":\"manager_and_surface_ready\",\"surface_config_available\":false");
            return false;
        }
        var camera = mc.gameRenderer.gameRenderState().levelRenderState.cameraRenderState;
        var config = surface.currentConfiguration().orElseThrow();
        var target = mc.gameRenderer.mainRenderTarget();
        var window = mc.getWindow();
        int observedDistance = mc.options.getEffectiveRenderDistance();
        if (observedDistance != expectedDistance) {
            rejectReadiness(now, ",\"condition\":\"effective_distance\",\"observed_distance\":"+observedDistance+",\"expected_distance\":"+expectedDistance);
            return false;
        }
        if (trackingDomain == null) initializeTrackingDomain(observedDistance);
        int coverage = 0, actualCoverage = 0;
        boolean omitCoordinate = faultOmitCoverageIndex >= 0 && phase == 4 && now - recordingStart >= SECOND;
        if (omitCoordinate && !coverageFaultMarked) { coverageFaultMarked = true; marker("coverage_fault_activated"); }
        for (int i = 0; i < trackingDomain.size(); i++) {
            ChunkPos pos = trackingDomain.get(i);
            var chunk = mc.level.getChunkSource().getChunk(pos.x(), pos.z(), ChunkStatus.FULL, false);
            boolean loaded = chunk != null && !(chunk instanceof EmptyLevelChunk) && chunk.getPos().equals(pos);
            trackingLoaded[i] = loaded;
            if (loaded) {
                actualCoverage++;
                if (!omitCoordinate || i != faultOmitCoverageIndex) coverage++;
            }
        }
        if (!trackingInventoryWritten) { writeTrackingWitness("tracking-domain.csv", now); trackingInventoryWritten = true; }
        if (!trackingLoadedWitnessWritten && actualCoverage == expectedCoverage) {
            writeTrackingWitness("tracking-loaded-witness.csv", now);
            trackingLoadedWitnessWritten = true;
            marker("full_tracking_coverage_observed");
        }
        double[] a = OBS[observationCount];
        a[0]=now-ORIGIN; a[1]=phase; a[2]=camera.pos.x; a[3]=camera.pos.y; a[4]=camera.pos.z; a[5]=camera.yRot; a[6]=camera.xRot;
        a[7]=mc.player.getX(); a[8]=mc.player.getY(); a[9]=mc.player.getZ(); a[10]=mc.player.getYRot(); a[11]=mc.player.getXRot(); a[12]=mc.player.getEyeHeight();
        a[13]=mc.level.getChunkSource().getLoadedChunksCount(); a[14]=coverage; a[15]=render[0]; a[16]=render[1];
        a[17]=render[2]; a[18]=render[3]; a[19]=render[4];
        a[20]=config.width(); a[21]=config.height(); a[22]=target.width; a[23]=target.height; a[24]=window.getScreenWidth(); a[25]=window.getScreenHeight();
        a[26]=mc.gameRenderer.gameRenderState().framerateLimit; a[27]=mc.options.framerateLimit().get(); a[28]=window.isFocused()?1:0;
        a[29]=window.isIconified()?1:0; a[30]=0; // 26.3 Window has no minimized flag; iconified covers it a[31]=mc.getFramerateLimitTracker().getThrottleReason().ordinal();
        a[32]=config.presentMode().ordinal(); a[33]=observedDistance; a[34]=mc.options.enableVsync().get()?1:0;
        boolean same = observationCount > 0;
        double[] previous = phase == 4 ? recordingBaseline : OBS[Math.max(0, observationCount - 1)];
        for (int i = 2; i < a.length; i++) if (Math.abs(a[i] - previous[i]) > 0.0001) same = false;
        observationCount++;
        boolean cameraInitialized = camera.initialized, firstPerson = mc.options.getCameraType().isFirstPerson(), unpaused = !mc.isPaused();
        boolean feetPosition = Math.abs(a[7]-feetX)<0.0001 && Math.abs(a[8]-feetY)<0.001 && Math.abs(a[9]-feetZ)<0.0001;
        boolean cameraAngles = Math.abs(a[5]-yaw)<0.01 && Math.abs(a[6]-pitch)<0.01;
        boolean feetAngles = Math.abs(a[10]-yaw)<0.01 && Math.abs(a[11]-pitch)<0.01;
        boolean cameraPosition = Math.abs(a[2]-feetX)<0.0001 && Math.abs(a[3]-(feetY+a[12]))<0.001 && Math.abs(a[4]-feetZ)<0.0001;
        boolean loadedMinimum = a[13]>=minimumChunks, fullCoverage = coverage==expectedCoverage;
        boolean geometryCounts = a[15]>0 && a[16]>=a[15], queuesIdle = a[17]==0 && a[18]==0 && a[19]==0;
        boolean managerNeedsUpdate = render[5] != 0, graphClean = !managerNeedsUpdate;
        boolean physicalSurface = a[20]==expectedWidth && a[21]==expectedHeight, targetMatches = a[20]==a[22] && a[21]==a[23];
        boolean notIconified = a[29]==0, notMinimized = a[30]==0, unthrottled = a[31]==0, distanceMatches = a[33]==expectedDistance;
        boolean ready = same && cameraInitialized && firstPerson && unpaused && feetPosition && cameraAngles && feetAngles && cameraPosition
            && loadedMinimum && fullCoverage && geometryCounts && queuesIdle && graphClean && physicalSurface && targetMatches
            && notIconified && notMinimized && unthrottled && distanceMatches;
        if (!ready && rejectedReadinessDue(now)) {
            marker("readiness_rejected", readinessContext(now)+",\"condition\":\"observed_state\",\"predicates\":{\"same_fields\":"+same
                +",\"camera_initialized\":"+cameraInitialized+",\"first_person\":"+firstPerson+",\"unpaused\":"+unpaused
                +",\"feet_position\":"+feetPosition+",\"camera_angles\":"+cameraAngles+",\"feet_angles\":"+feetAngles+",\"camera_position\":"+cameraPosition
                +",\"loaded_minimum\":"+loadedMinimum+",\"full_coverage\":"+fullCoverage+",\"geometry_counts\":"+geometryCounts+",\"queues_idle\":"+queuesIdle
                +",\"graph_clean\":"+graphClean+",\"physical_surface\":"+physicalSurface+",\"target_matches\":"+targetMatches
                +",\"not_iconified\":"+notIconified+",\"not_minimized\":"+notMinimized+",\"unthrottled\":"+unthrottled+",\"distance_matches\":"+distanceMatches+"}"
                +",\"manager_needs_update\":"+managerNeedsUpdate+",\"changed_fields\":"+changedFields(a,previous)+",\"observed\":"+observedState(a));
        }
        return ready;
    }

    // {visible, total, scheduled/queued jobs, busy workers, pending uploads, needsUpdate}; vanilla has no worker/upload counters.
    private static double[] vanillaCounts(Minecraft mc) {
        var lr = mc.levelRenderer;
        var dispatcher = lr.sectionRenderDispatcher();
        if (dispatcher == null) return null;
        int visible = lr.visibleSections().size();
        boolean idle = lr.hasRenderedAllSections() && dispatcher.isQueueEmpty();
        return new double[]{visible, visible, dispatcher.getCompileQueueSize(), 0, 0, idle ? 0 : 1};
    }

    public static void archiveReady(Path path) { archive = path.toAbsolutePath(); }

    private static void marker(String event) { marker(event, ""); }

    private static void marker(String event, String fields) {
        long now = System.nanoTime();
        String row = "{\"event\":"+quote(event)+",\"monotonic_ns\":"+now+",\"elapsed_ns\":"+(now-ORIGIN)+",\"utc\":"+quote(Instant.now().toString())+fields+"}";
        if (MARKERS.length() > 2) MARKERS.append(",\n");
        MARKERS.append(row);
        System.out.println("LODESTONE_BENCH " + row);
    }

    private static String quote(String value) { return "\""+value.replace("\\","\\\\").replace("\"","\\\"").replace("\n","\\n").replace("\r","\\r").replace("\t","\\t")+"\""; }

    private static String jsonNumber(double value) { return Double.isFinite(value)?Double.toString(value):"null"; }

    private static BufferedWriter writer(String name) throws IOException {
        return Files.newBufferedWriter(output.resolve(name), StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE);
    }

    public static void normalExit(Minecraft mc) {
        if (!initialized || finished) return;
        marker("normal_exit_before_completion", ",\"phase\":"+phase+",\"prior_failure\":"+quote(failure));
        finish(mc, "normal_exit_before_completion"+(failure.isEmpty()?"":": "+failure), false);
    }

    private static void finish(Minecraft mc, String reason) { finish(mc, reason, true); }

    private static void finish(Minecraft mc, String reason, boolean requestStop) {
        if (finished) return;
        finished = true;
        failure = reason;
        marker(requestStop?(reason.isEmpty()?"done":"failed"):"incomplete");
        try {
            try (var w = writer("presents.csv")) {
                w.write("frame_index,elapsed_ns,observation_index\n");
                for (int i=0; i<presentCount; i++) w.write(i+","+PRESENTS[i]+","+PRESENT_OBSERVATION[i]+"\n");
            }
            try (var w = writer("observations.csv")) {
                w.write(HEADER+"\n");
                for (int i=0; i<observationCount; i++) { for (int j=0; j<OBS[i].length; j++) { if (j>0) w.write(","); w.write(String.format(Locale.ROOT,"%.9f",OBS[i][j])); } w.write("\n"); }
            }
            try (var w=writer("markers.json")) { w.write(MARKERS+"]\n"); }
            try (var w=writer("observed-options.txt")) { w.write(mc.options.dumpOptionsForReport()); }
            try (var w=writer("run.json")) {
                w.write("{\"status\":"+quote(requestStop?(reason.isEmpty()?"complete":"failed"):"incomplete")+",\"failure\":"+quote(reason)+",\"backend\":"+quote(backend)+",\"final_phase\":"+phase
                    +",\"origin_ns\":"+ORIGIN+",\"recording_start_ns\":"+recordingStart+",\"recording_end_ns\":"+recordingEnd
                    +",\"recording_completed_at_ns\":"+recordingCompletedAt+",\"requested_duration_seconds\":"+durationSeconds+",\"profiler_enabled\":"+profiler
                    +",\"presents\":"+presentCount+",\"observations\":"+observationCount+",\"minimum_chunks\":"+minimumChunks+",\"coverage_radius\":"+radius
                    +",\"expected_coverage_chunks\":"+expectedCoverage+",\"expected_width\":"+expectedWidth+",\"expected_height\":"+expectedHeight+",\"expected_render_distance\":"+expectedDistance
                    +",\"coverage_domain\":\"reference jar buffered circular tracking view including neighbors\",\"tracking_domain_coordinates\":"+(trackingDomain==null?0:trackingDomain.size())
                    +",\"tracking_loaded_witness_written\":"+trackingLoadedWitnessWritten+",\"fault_omit_coverage_index\":"+faultOmitCoverageIndex+",\"coverage_fault_activated\":"+coverageFaultMarked
                    +",\"expected_feet_x\":"+feetX+",\"expected_feet_y\":"+feetY+",\"expected_feet_z\":"+feetZ+",\"expected_yaw\":"+yaw+",\"expected_pitch\":"+pitch
                    +",\"warmup_seconds\":20,\"plateau_seconds\":5,\"deadline_seconds\":"+deadlineSeconds+",\"synthetic_stall_ms\":"+stallMs
                    +",\"present_capacity\":"+PRESENTS.length+",\"observation_capacity\":"+OBS.length+",\"observation_hz\":10"
                    +",\"profile_zip\":"+quote(archive==null?"":archive.toString())+",\"screenshot\":"+quote(mc.gameDirectory.toPath().resolve("screenshots").resolve(SCREENSHOT_NAME).toAbsolutePath().toString())
                    +",\"screenshot_feedback\":"+quote(screenshotFeedback)+",\"timing_semantics\":\"successful OpenGL swap return; not compositor display time\"}\n");
            }
        } catch (Exception e) { System.err.println("LODESTONE_BENCH artifact_write_failed: " + e); }
        finally { if (requestStop) mc.stop(); }
    }
}
