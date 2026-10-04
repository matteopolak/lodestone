#!/usr/bin/env python3
"""Lodestone arm of the 26.3 comparison: the same fixture world, dedicated server, rcon setup,
pose, framebuffer and graphics settings as run-java-263.py, with the native Lodestone client.

  run-lodestone-263.py --binary /Volumes/LodestoneScratch/lodestone-arm/bin/lodestone-<sha> --trial lodestone-a

Artifacts land in $LODESTONE_COMPARISON_ROOT/runs/<trial>/. No credentials: Lodestone
joins with an offline identity written into a fresh data directory."""
import argparse, csv, hashlib, importlib.util, json, math, os, re, shutil, signal, statistics, subprocess, sys, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--binary', type=Path, required=True)
ap.add_argument('--trial', required=True)
ap.add_argument('--fps', type=int, default=260, help='framerate limit; 260 is unlimited, as in vanilla')
ap.add_argument('--duration', type=int, choices=(3, 10, 30, 60), default=10)
ap.add_argument('--warmup', type=int, default=30, help='seconds after join before the measured window')
ap.add_argument('--resolution', default='1280x720', help='physical framebuffer WxH')
ap.add_argument('--pose', default='-472.5,69.0,-392.5,0,0', help='feet x,y,z,yaw,pitch')
ap.add_argument('--assets', type=Path, default=Path(__file__).resolve().parents[2] / '.cache/benchmarks/vanilla-26.3-assets')
ap.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[2])
args = ap.parse_args()
if not re.fullmatch(r'[a-z0-9-]+', args.trial): ap.error('trial: lowercase simple name')
pose = tuple(map(float, args.pose.split(',')))
assert len(pose) == 5 and all(map(math.isfinite, pose))
rw, rh = map(int, args.resolution.split('x'))

HERE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get('LODESTONE_COMPARISON_ROOT', '/Volumes/LodestoneScratch/java-26.3'))
JAVA = Path(os.environ.get('LODESTONE_COMPARISON_JAVA', str(Path.home() / 'Library/Application Support/minecraft/runtime/java-runtime-epsilon/mac-os-arm64/java-runtime-epsilon/jre.bundle/Contents/Home/bin/java')))
RUN = ROOT / 'runs' / args.trial
RUN.mkdir(parents=True, exist_ok=False)
DATA = RUN / 'data'; DATA.mkdir()
WORKLOAD = 'showcase'  # idle in every segment; with moving=0 the camera never turns
username = 'BenchJava26'  # same name as the Java arm, so both draw the same default skin
# The Java arm's options.txt, expressed as Lodestone's eleven declared graphics fields.
settings = {
    'framerate_limit': args.fps, 'enable_vsync': False, 'inactivity_fps_limit': 'minimized', 'fov': 70,
    'render_distance': 8, 'graphics_preset': 'custom', 'cloud_status': 'fancy', 'cutout_leaves': True,
    'biome_blend_radius': 2, 'entity_shadows': True, 'particles': 'all',
}
(DATA / 'options.json').write_text(json.dumps(settings, indent=2, sort_keys=True) + '\n')
(DATA / 'offline.json').write_text(json.dumps({'username': username}))
binary_sha = hashlib.sha256(args.binary.read_bytes()).hexdigest()
command = [str(args.binary), '--benchmark', WORKLOAD,
           '--benchmark-warmup', str(args.warmup), '--benchmark-stationary', str(args.duration), '--benchmark-moving', '0',
           '--benchmark-debug-overlay', 'closed', '--host', '127.0.0.1', '--port', '25580', '--protocol', '777',
           '--render-distance', '8', '--benchmark-window', 'windowed', '--benchmark-pacing', 'options',
           '--benchmark-resolution', f'{rw}x{rh}']
frames = RUN / 'frames.csv'; presentation = RUN / 'presentation.json'
env = os.environ.copy()
env.update({'LODESTONE_DATA_DIR': str(DATA), 'LODESTONE_FRAME_PROFILE_DUMP': str(frames),
            'LODESTONE_PRESENTATION_CAPTURE': str(presentation),
            'LODESTONE_PRESENTATION_CAPTURE_SEGMENT': f'{WORKLOAD}.stationary',
            'LODESTONE_ASSETS': str(args.assets), 'TMPDIR': str(ROOT / 'temp'),
            'LODESTONE_BENCHMARK_SCREENSHOT': '1', 'RUST_LOG': os.environ.get('RUST_LOG', 'frame_profile=info,frame_benchmark=info,warn')})
git = lambda *a: subprocess.run(['git', '-C', str(args.repo), *a], capture_output=True, text=True).stdout.strip()
launch = {'purpose': 'bounded graphical benchmark run (Lodestone 26.3 arm)', 'binary': str(args.binary), 'binary_sha256': binary_sha,
          'checkout_head': git('rev-parse', 'HEAD'), 'command': command, 'settings': settings, 'assets': str(args.assets),
          'world': {'seed': -4172144997902289642, 'fixture': str(ROOT / 'world-fixture/world'), 'server': '26.3 vanilla dedicated, offline, 127.0.0.1:25580'},
          'pose_feet_xyz_yaw_pitch': pose, 'framebuffer_requested': f'{rw}x{rh}', 'warmup_seconds': args.warmup,
          'measured_seconds': args.duration, 'credential_access': False}
(RUN / 'launch.json').write_text(json.dumps(launch, indent=2))

def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m); return m
sampler_mod = load(HERE.parent / 'client-resource-sampler.py', 'sampler')
rcon_mod = load(HERE / 'rcon.py', 'rcon')

srv = RUN / 'server'; srv.mkdir()
shutil.copytree(ROOT / 'world-fixture/world', srv / 'world', ignore=shutil.ignore_patterns('session.lock'))
shutil.copy2(ROOT / 'world-fixture/server.jar', srv / 'server.jar'); shutil.copy2(ROOT / 'world-fixture/server.properties', srv / 'server.properties')
(srv / 'eula.txt').write_text('eula=true\n')
fixture_cmds = []; markers = []
server = process = None
server_log = (RUN / 'server.log').open('x'); client_log = (RUN / 'client.log').open('x')
exit_code = None; stop_reason = 'unknown'
try:
    server = subprocess.Popen([str(JAVA), '-XX:ActiveProcessorCount=2', '-Xms256M', '-Xmx1536M', '-jar', 'server.jar', 'nogui'],
                              cwd=srv, stdout=server_log, stderr=subprocess.STDOUT, env=env)
    t0 = time.monotonic()
    markers.append({'monotonic': time.monotonic(), 'unix': time.time(), 'event': 'driver_start'})
    while True:
        if server.poll() is not None: raise RuntimeError('server exited during startup')
        if time.monotonic() - t0 > 120: raise RuntimeError('server startup deadline')
        try:
            with rcon_mod.Rcon() as r:
                for c in ('difficulty peaceful', 'time set 6000', 'weather clear', 'tick freeze'):
                    fixture_cmds.append({'command': c, 'response': r.command(c)})
            break
        except (OSError, RuntimeError): time.sleep(0.5)
    process = subprocess.Popen(command, cwd=args.repo, stdout=client_log, stderr=subprocess.STDOUT, env=env)
    print(f'client PID={process.pid} log={RUN / "client.log"}', flush=True)
    sampler = sampler_mod.ProcessTreeSampler([process.pid], 'Lodestone 26.3 client; bounded graphical run', max_samples=220,
                                             retired_reader=sampler_mod.mac_retired_counters)
    deadline = time.monotonic() + args.warmup + args.duration + 120
    configured = False; pose_s = ' '.join(map(str, pose))
    while process.poll() is None and time.monotonic() < deadline:
        sampler.sample_if_due()
        if not configured:
            try:
                with rcon_mod.Rcon() as r:
                    if username in r.command('list'):
                        for c in (f'gamemode creative {username}', f'effect clear {username}', f'attribute {username} minecraft:gravity base set 0', f'tp {username} {pose_s}'):
                            fixture_cmds.append({'command': c, 'response': r.command(c)})
                        configured = True; markers.append({'monotonic': time.monotonic(), 'event': 'pose_applied'})
            except (OSError, RuntimeError): pass
        time.sleep(0.2)
    stop_reason = 'client_exit' if process.poll() is not None else 'hard_deadline_killed'
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try: process.wait(10)
        except subprocess.TimeoutExpired: process.kill(); process.wait()
    sampler.sample_if_due()
    sampler.write_report(RUN / 'resources.json', stop_reason)
    exit_code = process.returncode
finally:
    if process is not None and process.poll() is None: process.kill()
    if server is not None and server.poll() is None:
        try:
            with rcon_mod.Rcon() as r: r.command('stop')
            server.wait(30)
        except Exception: server.kill()
    (RUN / 'host-markers.json').write_text(json.dumps({'markers': markers, 'fixture_commands': fixture_cmds, 'exit_code': exit_code, 'stop_reason': stop_reason}, indent=2))
    server_log.close(); client_log.close()

def pct(v, p):
    s = sorted(v); k = (len(s) - 1) * p / 100; lo = int(k); hi = min(lo + 1, len(s) - 1); return s[lo] + (s[hi] - s[lo]) * (k - lo)
summary = {'trial': args.trial, 'arm': 'lodestone', 'exit_code': exit_code, 'stop_reason': stop_reason, 'binary_sha256': binary_sha}
if presentation.is_file():
    cap = json.loads(presentation.read_text())
    cols = cap['columns']; rows = cap['rows']
    summary['presentation'] = {k: cap.get(k) for k in ('elapsedUs', 'attempts', 'submissions', 'skipped', 'droppedRows', 'skipReasons')}
    # intervalUs is the gap between consecutive successful submissions, measured by the client.
    iv = cols.index('intervalUs')
    dts = [r[iv] / 1e3 for r in rows if r[iv] is not None]
    if dts:
        summary['interval_ms'] = {'mean': statistics.fmean(dts), 'p50': pct(dts, 50), 'p95': pct(dts, 95), 'p99': pct(dts, 99), 'max': max(dts)}
        summary['presents'] = cap['submissions']; summary['window_seconds'] = cap['elapsedUs'] / 1e6
        summary['mean_present_hz'] = cap['submissions'] / summary['window_seconds']
(RUN / 'summary.json').write_text(json.dumps(summary, indent=2)); print(json.dumps(summary, indent=2))
