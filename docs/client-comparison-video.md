# Client comparison video

## What it is

`scripts/compose-client-comparison.py` turns two recorded controlled runs into a top/bottom MP4:
optimized Java above Lodestone native or browser, with equal 16:9 panels. It preserves source
playback speed and saves an accompanying manifest with recording hashes, synchronization offsets,
settings, releases, mods, and explicitly supplied measurement provenance.

## How it works

Capture each arm separately on the same hardware after establishing identical imported world
data, camera, action sequence, resolution, render distance, simulation settings, and warmup.
Use the real vanilla resource pack for Lodestone and retain its version/hash with the captures.
The composer records these declarations; it cannot establish world or workload equivalence.
`timing_control` should identify the external action clock/synchronization cue, selected interval,
warmup exclusion, and relationship between the video interval and measured interval.

The tool resets each source's starting timestamp, trims at its explicit elapsed-time offset,
and retains the same duration from each. It fits each view into an identically sized 16:9 panel
with black letterboxing, stacks Java above Lodestone, and encodes H.264 MP4 without audio.
It never changes playback speed. Encoding at 30 or 60 FPS resamples the recorded images;
that rate is not a game performance measurement.

With `--overlay`, labels, releases, mods and any supplied measured mean FPS/p95/p99 frame time
appear in translucent static banners. Each summary requires a separate measurement source
file and a human description of its method. The tool hashes that file and retains the supplied
values; it does not compute FPS from video or guess a raw benchmark report schema. State which
clock the measurement observes, such as presented frame intervals or CPU frame work, and whether
the measurement covers exactly the selected recorded interval. Use comparable definitions in
both arms. A full-run static summary should not be described as instantaneous FPS.

Existing outputs/manifests and identical source recordings are rejected. Sources must contain
the entire requested interval; there is no freeze-frame extension for a short arm. The final
encoded size and duration are checked, and exclusive file publication prevents accidental overwrite.
The manifest retains the FFmpeg command/version, source and measurement hashes, encoding settings,
and all original comparison configuration. No winner or posting claim is generated.

## Configuration

Create a JSON file using actual capture information. All paths resolve relative to that file.
The following is a configuration shape, not measured evidence; replace every placeholder and
set the duration/offsets from the action synchronization cue:

```json
{
  "scenario": "<environment and action>",
  "world_identity": "<imported world hash and proof of equivalence>",
  "action_identity": "<camera/action trace hash>",
  "texture_pack_identity": "<vanilla pack release and hash>",
  "timing_control": "<external clock, cue, warmup exclusion and selected measurement window>",
  "duration_seconds": 10,
  "java": {
    "source": "java.mp4",
    "offset_seconds": 2,
    "label": "Minecraft Java, optimized",
    "release": "<Minecraft version>",
    "mods": ["Sodium <version>", "<other mod and version>"],
    "settings": {"resolution": "<actual>", "render_distance": "<actual>", "vsync": "<actual>"}
  },
  "lodestone": {
    "source": "lodestone.mp4",
    "offset_seconds": 3,
    "label": "Lodestone in browser",
    "release": "<commit and browser/version>",
    "mods": [],
    "settings": {"resolution": "<actual>", "render_distance": "<actual>", "vsync": "<actual>"}
  }
}
```

To show measured summaries, add `measured_summary` to either arm with `source` (measurement
file path), `method` (nonblank explanation), and at least one of `mean_fps`,
`p95_frame_time_ms`, `p99_frame_time_ms` (positive finite numbers from that measurement).
No metrics are invented when this object is absent. Settings can retain hardware, FOV, simulation
distance, capture overhead, FPS limits, browser/backend, power state, and measurement boundaries.

```bash
python3 scripts/compose-client-comparison.py \
  --config captures/comparison.json --output captures/comparison.mp4 \
  --overlay --font /System/Library/Fonts/Helvetica.ttc
```

The default output is 1280×1440 with two 1280×720 panels at 60 encoding FPS. `--width` and
`--panel-height` must be even and exactly 16:9; `--fps` accepts 30 or 60. The requested duration
must represent a whole number of output frames. `--ffmpeg` and `--ffprobe` select existing
executables. `--threads` defaults to two threads per video decoder/encoder;
`--filter-complex-threads` defaults to one for the filter graph. Both must be positive and are
retained in the manifest so long recordings have bounded parallelism on shared hardware.
Omit `--overlay` for a video without banners or Pillow. Keep the manifest beside the
video, and visually review gameplay, panel alignment and text before sharing the clip on X.

## How to change it

Extend `prepare` for new declared metadata/measurement validation and `make_overlay` for banners.
Keep measured game performance separate from encoder metadata. Avoid speeding one arm, cropping
different areas, omitting mod versions, or silently selecting a shorter interval. Additional
environments should each have their own recorded pair, configuration, and manifest.

Run `python3 scripts/test-compose-client-comparison.py`. The small synthetic FFmpeg control uses
colored transitions to check independent offsets, equal duration, output dimensions and panel order.
It never records or represents game performance and leaves its temporary files unpublished.

## Dependencies

Python 3 standard library, FFmpeg with `libx264`, `scale`, `pad`, `fps`, `vstack`, and `overlay`,
and FFprobe. Banners additionally require Pillow in the selected Python runtime and an explicitly
supplied readable TrueType/OpenType font. The tool installs nothing and does not require
FFmpeg's `drawtext` filter.
