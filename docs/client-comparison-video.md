# Client comparison video

## What it is

`scripts/compose-client-comparison.py` turns two recorded controlled runs into a top/bottom MP4: optimized Java above Lodestone (native or browser) in equal 16:9 panels, at source playback speed, with a manifest holding recording hashes, sync offsets, settings, releases, mods and explicitly supplied measurement provenance.

## How it works

- Capture each arm separately on the same hardware with identical imported world, camera, action sequence, resolution, render distance, simulation settings and warmup, using the real vanilla resource pack (retain its version and hash). The composer records these declarations but cannot verify equivalence. `timing_control` should name the external sync cue, selected interval, warmup exclusion and how the video interval relates to the measured one.
- Each source is trimmed at its own offset to the same duration, fitted into an identical 16:9 panel with black letterboxing, stacked and encoded as H.264 MP4 without audio. Playback speed never changes; the 30 or 60 FPS encode rate resamples images and is not a performance measurement.
- `--overlay` adds static banners with labels, releases, mods and supplied `measured_summary` values (mean FPS, p95, p99 frame time). The summary needs a separate measurement file and a human description of the method; the tool hashes the file and keeps the values, never computing FPS or guessing report schemas. State which clock was observed and whether it covers exactly the selected interval; a full-run summary is not instantaneous FPS.
- `resource_summary` adds CPU percent (100% per core), peak RSS and separately identified GPU residency, dedicated VRAM or allocations. Allocation counters are never relabelled residency; unknown metrics stay `null` with a reason under `unavailable.<metric>` and display as unavailable. Use the same interval for both arms; whole-launch samples must not be presented as the cost of a shorter segment. Collect with [client-resource-sampling](client-resource-sampling.md).
- Existing outputs and identical source recordings are rejected; a source must cover the whole interval (no freeze-frame extension); output size and duration are checked; publication is exclusive. The manifest keeps the FFmpeg command and version, source and measurement hashes, encode settings and all configuration. No winner or posting claim is generated.

## How to change it

- Extend `prepare` for new metadata or measurement validation and `make_overlay` for banners. Keep measured game performance separate from encoder metadata. Never speed up one arm, crop different areas, omit mod versions or silently shorten the interval. Each additional environment gets its own recorded pair, config and manifest.
- Test with `python3 scripts/test-compose-client-comparison.py` (a synthetic FFmpeg control with coloured transitions checks offsets, duration, dimensions and panel order; it represents no game performance).
- Review gameplay, alignment and text visually before sharing.

## Configuration

A JSON file (paths relative to it; every placeholder must be replaced with real capture data):

```json
{
  "scenario": "<environment and action>",
  "world_identity": "<imported world hash and proof of equivalence>",
  "action_identity": "<camera/action trace hash>",
  "texture_pack_identity": "<vanilla pack release and hash>",
  "timing_control": "<external clock, cue, warmup exclusion and selected measurement window>",
  "duration_seconds": 10,
  "java": {
    "source": "java.mp4", "offset_seconds": 2,
    "label": "Minecraft Java, optimized", "release": "<Minecraft version>",
    "mods": ["Sodium <version>"],
    "settings": {"resolution": "<actual>", "render_distance": "<actual>", "vsync": "<actual>"}
  },
  "lodestone": {
    "source": "lodestone.mp4", "offset_seconds": 3,
    "label": "Lodestone in browser", "release": "<commit and browser/version>",
    "mods": [], "settings": {"resolution": "<actual>"}
  }
}
```

- `measured_summary` (per arm): `source`, `method` (nonblank) and at least one positive finite `mean_fps`, `p95_frame_time_ms`, `p99_frame_time_ms`. Settings may also record hardware, FOV, simulation distance, capture overhead, FPS limits, backend, power state.
- `resource_summary`: `source`, `method`, `interval` and at least one of `mean_cpu_percent`, `peak_cpu_percent`, `peak_rss_bytes`, `peak_gpu_resident_bytes`, `peak_dedicated_vram_bytes`, `peak_gpu_allocated_bytes` (nonnegative finite; bytes shown in MiB). The source file is hashed and rechecked before publishing. An unavailable value looks like:

  ```json
  "resource_summary": {
    "source": "resources.json",
    "method": "Process-tree CPU intervals and summed RSS; shared pages can be counted twice",
    "interval": "<same selected action interval as the video>",
    "peak_dedicated_vram_bytes": null,
    "unavailable": {"peak_dedicated_vram_bytes": "Unified-memory host; no residency instrument"}
  }
  ```

```bash
python3 scripts/compose-client-comparison.py \
  --config captures/comparison.json --output captures/comparison.mp4 \
  --overlay --font /System/Library/Fonts/Helvetica.ttc
```

Default output is 1280x1440 (two 1280x720 panels, 60 FPS). `--width` and `--panel-height` must be even and exactly 16:9; `--fps` is 30 or 60; the duration must be a whole number of output frames. `--ffmpeg`, `--ffprobe` select executables; `--threads` (default 2) and `--filter-complex-threads` (default 1) are recorded in the manifest. Omit `--overlay` for no banners or Pillow.

## Dependencies

Python 3 standard library; FFmpeg with `libx264`, `scale`, `pad`, `fps`, `vstack`, `overlay`; FFprobe. Banners need Pillow and a readable TrueType or OpenType font (no `drawtext` required). Nothing is installed.
