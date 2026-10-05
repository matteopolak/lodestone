#!/usr/bin/env python3
"""Alternating, idle-gated client benchmark: Java (optimized, vanilla) vs Lodestone.

Each round runs every arm once, in a rotated order, after waiting for the host
to be idle. Per trial it reports frame rate and frame-time percentiles from the
arm's own present log, and CPU, RSS and system GPU utilisation over the
measured window only (not startup or loading).

    python3 bench-suite.py --prefix s1 --rounds 3 --lodestone /path/to/lodestone
"""
import argparse, shutil, datetime, json, os, re, statistics, subprocess, sys, threading, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get('LODESTONE_COMPARISON_ROOT', '/Volumes/LodestoneScratch/java-26.3'))
TOOLING = HERE
RUNS = ROOT / 'runs'
REPO_SCREENSHOTS = HERE.parents[1] / 'screenshots'
HEAVY = re.compile(r'(rustc|cargo|jaic|clang|ld64|swift-frontend|javac)$')

ap = argparse.ArgumentParser()
ap.add_argument('--prefix', required=True)
ap.add_argument('--rounds', type=int, default=3)
ap.add_argument('--arms', default='optimized,vanilla,lodestone')
ap.add_argument('--lodestone', type=Path, required=True)
ap.add_argument('--duration', type=int, default=10)
ap.add_argument('--retries', type=int, default=2, help='reruns of a trial that presented no frames')
ap.add_argument('--fps', type=int, default=260, help='frame cap for every arm; 260 is unlimited')
ap.add_argument('--max-load', type=float, default=2.5, help='1-minute load average that counts as idle')
ap.add_argument('--no-idle-wait', action='store_true', help='skip the idle gate (smoke tests only)')
ap.add_argument('--idle-timeout', type=int, default=3600, help='seconds to wait for idle before running anyway (flagged)')
args = ap.parse_args()
arms = args.arms.split(',')


def heavy_processes():
    out = subprocess.run(['ps', '-Ao', 'pcpu=,comm='], capture_output=True, text=True).stdout
    busy = []
    for line in out.splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and float(parts[0]) > 30 and HEAVY.search(parts[1]):
            busy.append(parts[1].rsplit('/', 1)[-1])
    return busy


def wait_for_idle():
    if args.no_idle_wait:
        return {'waited_s': 0, 'load1': os.getloadavg()[0], 'idle': False, 'skipped': True}
    start = time.time(); calm = 0
    while time.time() - start < args.idle_timeout:
        load = os.getloadavg()[0]; busy = heavy_processes()
        calm = calm + 1 if load < args.max_load and not busy else 0
        if calm >= 3:
            return {'waited_s': round(time.time() - start), 'load1': load, 'idle': True}
        time.sleep(10)
    return {'waited_s': round(time.time() - start), 'load1': os.getloadavg()[0], 'idle': False, 'busy': heavy_processes()}


class GpuSampler(threading.Thread):
    """System-wide GPU utilisation from the accelerator's performance statistics."""
    def __init__(self):
        super().__init__(daemon=True); self.samples = []; self.stop = threading.Event()

    def run(self):
        pat = re.compile(r'"Device Utilization %"=(\d+)')
        while not self.stop.is_set():
            out = subprocess.run(['ioreg', '-r', '-d', '1', '-w', '0', '-c', 'IOAccelerator'], capture_output=True, text=True).stdout
            m = pat.search(out)
            if m: self.samples.append((time.time(), int(m.group(1))))
            self.stop.wait(0.25)


class PowerSampler(threading.Thread):
    """System CPU/GPU power and GPU active residency from powermetrics (needs a NOPASSWD rule)."""
    def __init__(self):
        super().__init__(daemon=True); self.samples = []; self.proc = None

    def run(self):
        try:
            self.proc = subprocess.Popen(['sudo', '-n', '/usr/bin/powermetrics', '-i', '500', '--samplers', 'gpu_power,cpu_power'],
                                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        except OSError:
            return
        cur = {}
        for line in self.proc.stdout:
            if line.startswith('*** Sampled'):
                if cur: self.samples.append(cur)
                cur = {'t': time.time()}
            elif m := re.match(r'(CPU|GPU) Power: (\d+) mW', line):
                cur[m.group(1).lower() + '_mw'] = int(m.group(2))
            elif m := re.match(r'GPU HW active residency:\s+([\d.]+)%', line):
                cur['gpu_active_percent'] = float(m.group(1))
            elif m := re.match(r'GPU HW active frequency: (\d+) MHz', line):
                cur['gpu_mhz'] = int(m.group(1))
        if cur: self.samples.append(cur)

    def finish(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try: self.proc.wait(5)
            except subprocess.TimeoutExpired: self.proc.kill()
        self.join(5)


def utc_to_unix(s):
    return datetime.datetime.fromisoformat(s.replace('Z', '+00:00')).timestamp()


def window_bounds(run_dir, arm):
    """The measured window in unix seconds, and monotonic->unix offset of the sampler clock."""
    hm = json.loads((run_dir / 'host-markers.json').read_text())['markers']
    pair = next(m for m in hm if 'unix' in m)
    offset = pair['unix'] - pair['monotonic']
    if arm == 'lodestone':
        log = (run_dir / 'client.log').read_text(errors='replace')
        clean = re.sub(r'\x1b\[[0-9;]*m', '', log)
        start = re.search(r'^(\S+Z)\s+INFO benchmark segment transition .*segment="showcase\.stationary"', clean, re.M)
        end = re.search(r'^(\S+Z)\s+INFO benchmark segment transition .*segment="showcase\.complete"', clean, re.M)
        return utc_to_unix(start.group(1)), utc_to_unix(end.group(1)), offset
    by = {}
    for m in hm:
        e = re.search(r'\\?"event\\?":\\?"([a-z_]+)', m.get('line', ''))
        if e: by.setdefault(e.group(1), m['unix'])
    return by['recording_start'], by['recording_end'], offset


def window_resources(run_dir, start, end, offset):
    samples = json.loads((run_dir / 'resources.json').read_text())['samples']
    cpu_s = span = 0.0; rss = []
    for s in samples:
        t1 = s['monotonic_seconds'] + offset; dt = s.get('interval_seconds')
        if not dt: continue
        t0 = t1 - dt
        if t0 >= start - 0.5 and t1 <= end + 0.5 and s.get('cpu_complete'):
            cpu_s += s['cpu_observed_delta_seconds']; span += dt
        if start <= t1 <= end + 0.5:
            rss.append(s['rss_bytes'])
    return (100 * cpu_s / span if span else None), (statistics.median(rss) if rss else None), round(span, 2)


def window_hidden(run_dir):
    """Lodestone skips every frame while macOS reports its window occluded (a hidden Space or a lock screen)."""
    cap = json.loads((run_dir / 'presentation.json').read_text())
    return cap.get('submissions') == 0 and cap.get('attempts', 0) > 0 and cap.get('skipReasons', {}).get('paced') == cap['attempts']


def run_trial(arm, trial):
    if arm == 'lodestone':
        cmd = [sys.executable, str(TOOLING / 'run-lodestone-263.py'), '--binary', str(args.lodestone), '--trial', trial, '--duration', str(args.duration), '--fps', str(args.fps)]
    else:
        cmd = [sys.executable, str(TOOLING / 'run-java-263.py'), '--modset', arm, '--trial', trial, '--duration', str(args.duration), '--fps', str(args.fps)]
    before = set(REPO_SCREENSHOTS.glob('*.png')) if REPO_SCREENSHOTS.exists() else set()
    gpu = GpuSampler(); gpu.start(); power = PowerSampler(); power.start()
    proc = subprocess.run(cmd, capture_output=True, text=True)
    gpu.stop.set(); gpu.join(); power.finish()
    run_dir = RUNS / trial
    (run_dir / 'suite-driver.log').write_text(proc.stdout + proc.stderr)
    for shot in (set(REPO_SCREENSHOTS.glob('*.png')) - before) if REPO_SCREENSHOTS.exists() else []:
        (run_dir / 'screenshots').mkdir(exist_ok=True); shutil.move(str(shot), run_dir / 'screenshots' / shot.name)
    summary = json.loads((run_dir / 'summary.json').read_text())
    start, end, offset = window_bounds(run_dir, arm)
    cpu, rss, span = window_resources(run_dir, start, end, offset)
    g = [u for t, u in gpu.samples if start <= t <= end]
    (run_dir / 'gpu-utilisation.json').write_text(json.dumps({'source': 'ioreg IOAccelerator Device Utilization % (system-wide)', 'samples': gpu.samples}))
    (run_dir / 'powermetrics.json').write_text(json.dumps({'source': 'powermetrics gpu_power,cpu_power, 500 ms, system-wide', 'samples': power.samples}))
    # A sample is stamped when its 500 ms interval ends.
    pw = [p for p in power.samples if start + 0.5 <= p['t'] <= end]
    mean = lambda k: statistics.mean(p[k] for p in pw if k in p) if any(k in p for p in pw) else None
    iv = summary.get('interval_ms', {})
    return {'arm': arm, 'trial': trial, 'exit_code': proc.returncode, 'fps': summary.get('mean_present_hz'),
            'p50_ms': iv.get('p50'), 'p95_ms': iv.get('p95'), 'p99_ms': iv.get('p99'), 'max_ms': iv.get('max'),
            'window_s': round(end - start, 2), 'cpu_percent': cpu, 'cpu_span_s': span, 'rss_mb': rss / 2**20 if rss else None,
            'gpu_util_percent': statistics.mean(g) if g else None, 'gpu_samples': len(g),
            'gpu_active_percent': mean('gpu_active_percent'), 'gpu_mhz': mean('gpu_mhz'),
            'gpu_mw': mean('gpu_mw'), 'cpu_mw': mean('cpu_mw'), 'power_samples': len(pw)}


rows = []
out = RUNS / f'{args.prefix}-suite.json'
for r in range(args.rounds):
    order = arms[r % len(arms):] + arms[:r % len(arms)]
    for arm in order:
        idle = wait_for_idle()
        trial = f'{args.prefix}-{arm}-{r + 1}'
        print(f'[{time.strftime("%H:%M:%S")}] {trial} (idle={idle["idle"]}, waited {idle["waited_s"]} s, load {idle["load1"]:.1f})', flush=True)
        for attempt in range(1 + args.retries):
            name = trial if attempt == 0 else f'{trial}-retry{attempt}'
            try:
                row = run_trial(arm, name)
            except Exception as e:  # keep the suite going; the row records why
                row = {'arm': arm, 'trial': name, 'error': repr(e)}
            # A covered (occluded) Lodestone window presents nothing, and a Java window that loses
            # focus mid-run never passes readiness: both mean the desktop interfered, not a result.
            if row.get('fps'):
                break
            if arm == 'lodestone' and not row.get('error') and window_hidden(RUNS / name):
                sys.exit(f'{name}: the Lodestone window was never visible (every frame skipped as paced). '
                         'Another app is full screen on the main display, or the screen is locked; '
                         'Java would keep drawing into its hidden window, so no arm is measurable. Clear the display and rerun.')
            print('    invalid, retrying:', row.get('error') or 'no presented frames', flush=True)
        row['idle'] = idle; rows.append(row)
        print('   ', {k: (round(v, 2) if isinstance(v, float) else v) for k, v in row.items() if k != 'idle'}, flush=True)
        out.write_text(json.dumps({'args': {k: str(v) for k, v in vars(args).items()}, 'rows': rows}, indent=2))
print('wrote', out)
