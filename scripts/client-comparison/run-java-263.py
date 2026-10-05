#!/usr/bin/env python3
"""Offline graphical benchmark run of Minecraft 26.3 (+Fabric, +modset) against a local dedicated server on the fixed-seed world fixture.
One command per arm:
  run-java-263.py --modset optimized --trial optimized-a
  run-java-263.py --modset vanilla   --trial vanilla-a
Artifacts land in $LODESTONE_COMPARISON_ROOT/runs/<trial>/ . Never touches credentials (--offlineDeveloperMode)."""
import argparse, hashlib, importlib.util, io, json, math, os, re, shutil, signal, subprocess, sys, time, zipfile
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--modset', choices=('optimized', 'vanilla'), required=True)
ap.add_argument('--trial', required=True)
ap.add_argument('--fps', type=int, default=260)
ap.add_argument('--vsync', action='store_true')
ap.add_argument('--duration', type=int, choices=(3, 10, 30, 60), default=10)
ap.add_argument('--deadline', type=int, default=120)
ap.add_argument('--window', default=None, help='launch --width/--height in logical points; default is the expected surface divided by the main display scale')
ap.add_argument('--expected-surface', default='1280x720', help='expected physical surface WxH')
ap.add_argument('--pose', default='-472.5,69.0,-392.5,0,0', help='feet x,y,z,yaw,pitch')
ap.add_argument('--prepare-only', action='store_true')
args = ap.parse_args()
if not re.fullmatch(r'[a-z0-9-]+', args.trial): ap.error('trial: lowercase simple name')
pose = tuple(map(float, args.pose.split(',')))
assert len(pose) == 5 and all(map(math.isfinite, pose))
ew, eh = map(int, args.expected_surface.split('x'))


def main_display_scale():
    """2 when the main display is Retina (logical points are half the pixels), else 1."""
    out = subprocess.run(['system_profiler', 'SPDisplaysDataType'], capture_output=True, text=True).stdout
    block = []
    for line in out.splitlines():
        if line.strip().startswith('Resolution:'):
            block = [line]
        elif block:
            block.append(line)
            if 'Main Display: Yes' in line:
                return 2 if 'Retina' in block[0] else 1
    return 2


if args.window is None:
    scale = main_display_scale()
    args.window = f'{ew // scale}x{eh // scale}'
ww, wh = map(int, args.window.split('x'))

HERE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get('LODESTONE_COMPARISON_ROOT', '/Volumes/LodestoneScratch/java-26.3'))
JAVA = Path(os.environ.get('LODESTONE_COMPARISON_JAVA', str(Path.home() / 'Library/Application Support/minecraft/runtime/java-runtime-epsilon/mac-os-arm64/java-runtime-epsilon/jre.bundle/Contents/Home/bin/java')))
BASE = json.loads((ROOT / 'dl/26.3.json').read_text())
FABRIC = json.loads((ROOT / 'fabric-profile.json').read_text())
MANIFEST = {f['path']: f for f in json.loads((ROOT / 'manifest.json').read_text())['files'] if 'path' in f}
RUN = ROOT / 'runs' / args.trial
RUN.mkdir(parents=True, exist_ok=False)
GAME = RUN / 'game'; (GAME / 'mods').mkdir(parents=True)
NAT = ROOT / 'natives'
for sub in ('java', 'jna', 'lwjgl', 'netty'): (NAT / sub).mkdir(parents=True, exist_ok=True)
(ROOT / 'temp').mkdir(exist_ok=True)

modset = ROOT / 'modsets' / args.modset
for jar in sorted(modset.glob('*.jar')): shutil.copy2(jar, GAME / 'mods' / jar.name)

def permitted(rules):
    if not rules: return True
    allowed = False
    for r in rules:
        o = r.get('os', {})
        if o.get('name', 'osx') != 'osx': continue
        if 'arch' in o and not re.fullmatch(o['arch'], 'aarch64'): continue
        if r.get('features'): continue
        allowed = r['action'] == 'allow'
    return allowed
def ident(name):
    p = name.split(':'); return p[0], p[1], p[3] if len(p) > 3 else None

libs = {}
for l in FABRIC['libraries']:
    g, a, v = l['name'].split(':')
    libs[ident(l['name'])] = ROOT / 'fabric-libraries' / g.replace('.', '/') / a / v / f'{a}-{v}.jar'
nbase = 0
for l in BASE['libraries']:
    if permitted(l.get('rules')):
        art = l['downloads']['artifact']; path = ROOT / 'libraries' / art['path']
        if hashlib.sha1(path.read_bytes()).hexdigest() != art['sha1']: raise ValueError('library digest ' + l['name'])
        nbase += 1; libs.setdefault(ident(l['name']), path)
client = ROOT / 'minecraft/26.3-client.jar'
assert hashlib.sha1(client.read_bytes()).hexdigest() == BASE['downloads']['client']['sha1']
classpath = ':'.join(map(str, libs.values())) + ':' + str(client)
variables = {'natives_directory': str(NAT), 'launcher_name': 'isolated-local-comparison', 'launcher_version': '1', 'classpath': classpath}
jvm = []
for e in BASE['arguments']['default-user-jvm'] + BASE['arguments']['jvm'] + FABRIC['arguments']['jvm']:
    if isinstance(e, str): vals = [e]
    else:
        if not permitted(e.get('rules')): continue
        vals = e['value'] if isinstance(e['value'], list) else [e['value']]
    for v in vals:
        for k, rep in variables.items(): v = v.replace('${' + k + '}', rep)
        if '${' in v: raise ValueError('unresolved placeholder ' + v)
        jvm.append(v)
# Java 26 era launcher flag set is whatever the version JSON says; the 26.2 run also added these two:
for extra in ('--sun-misc-unsafe-memory-access=allow',):
    if extra not in jvm: jvm.insert(jvm.index('-XstartOnFirstThread') + 1, extra)

mods, mod_ids = [], {}
for p in sorted((GAME / 'mods').glob('*.jar')):
    mods.append({'file': p.name, 'bytes': p.stat().st_size, 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()})
    with zipfile.ZipFile(p) as z:
        m = json.loads(z.read('fabric.mod.json')); mod_ids[m['id']] = m['version']
        for n in m.get('jars', []):
            with zipfile.ZipFile(io.BytesIO(z.read(n['file']))) as inner:
                c = json.loads(inner.read('fabric.mod.json')); mod_ids[c['id']] = c['version']
with zipfile.ZipFile(client) as z: data_version = json.loads(z.read('version.json'))['world_version']
# Vanilla defaults except: vsync off, uncapped fps (260), render/simulation distance 8, windowed, inactivity limit "minimized" (default "afk" would throttle a hands-off run), and no pause on lost focus
# (a focus change would open the pause screen and fail readiness; Lodestone's benchmark mode ignores focus the same way).
# graphicsPreset must be "custom" or the FANCY preset overwrites the distances; every other option keeps its built-in default (clouds, particles, AO, shadows, blend all on).
options = (f'version:{data_version}\nlang:en_us\ngraphicsPreset:"custom"\nmaxFps:{args.fps}\nenableVsync:{str(args.vsync).lower()}\n'
           'inactivityFpsLimit:"minimized"\npauseOnLostFocus:false\nrenderDistance:8\nsimulationDistance:8\nfullscreen:false\nonboardAccessibility:false\njoinedFirstServer:true\ntutorialStep:none\n')
(GAME / 'options.txt').write_text(options)
props = [f'-Dbench.output={RUN}', '-Dbench.coverageRadius=8', '-Dbench.minimumChunks=329', '-Dbench.expectedCoverage=329',
         f'-Dbench.durationSeconds={args.duration}', f'-Dbench.expectedWidth={ew}', f'-Dbench.expectedHeight={eh}',
         f'-Dbench.deadlineSeconds={args.deadline}', '-Dbench.profiler=false']
props += [f'-Dbench.{k}={v}' for k, v in zip(('feetX', 'feetY', 'feetZ', 'yaw', 'pitch'), pose)]
username = 'BenchJava26'
command = [str(JAVA), *jvm, *props, FABRIC['mainClass'], '--offlineDeveloperMode', '--accessToken', 'offline-local-only',
           '--username', username, '--version', FABRIC['id'], '--versionType', 'release', '--gameDir', str(GAME),
           '--assetsDir', str(ROOT / 'assets'), '--assetIndex', BASE['assetIndex']['id'], '--graphicsBackend', 'OPENGL',
           '--width', str(ww), '--height', str(wh), '--quickPlayPath', str(RUN / 'quickplay.json'), '--quickPlayMultiplayer', '127.0.0.1:25580']
launch = {'purpose': 'bounded graphical benchmark run (Java 26.3 arm)', 'modset': args.modset, 'java_executable': str(JAVA),
          'minecraft': '26.3', 'fabric_profile': FABRIC['id'], 'client_sha1': BASE['downloads']['client']['sha1'],
          'base_libraries': nbase, 'classpath_entries': len(libs) + 1,
          'jvm_arguments': [v if v != classpath else '<classpath: see classpath.txt>' for v in jvm],
          'capture_properties': props, 'game_arguments': command[len(command) - 18:], 'mod_files': mods, 'mod_ids': mod_ids,
          'options_txt': options, 'world': {'seed': -4172144997902289642, 'fixture': str(ROOT / 'world-fixture/world'), 'server': '26.3 vanilla dedicated, offline, 127.0.0.1:25580'},
          'pose_feet_xyz_yaw_pitch': pose, 'window_requested': args.window, 'expected_surface': f'{ew}x{eh}', 'credential_access': False,
          'adapter_source': str(HERE / 'adapter')}
(RUN / 'classpath.txt').write_text(classpath)
(RUN / 'launch.json').write_text(json.dumps(launch, indent=2))
if args.prepare_only: print('prepared', RUN); sys.exit(0)

def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m); return m
sampler_mod = load(HERE.parent / 'client-resource-sampler.py', 'sampler')
rcon_mod = load(HERE / 'rcon.py', 'rcon')
env = os.environ.copy(); env['TMPDIR'] = str(ROOT / 'temp')

srv = RUN / 'server'; srv.mkdir()
shutil.copytree(ROOT / 'world-fixture/world', srv / 'world', ignore=shutil.ignore_patterns('session.lock'))
shutil.copy2(ROOT / 'world-fixture/server.jar', srv / 'server.jar'); shutil.copy2(ROOT / 'world-fixture/server.properties', srv / 'server.properties')
(srv / 'eula.txt').write_text('eula=true\n')
fixture_cmds, markers = [], []
server = process = None
server_log = (RUN / 'server.log').open('x'); client_log = (RUN / 'client.log').open('x')
exit_code = None; stop_reason = 'unknown'
try:
    server = subprocess.Popen([str(JAVA), '-XX:ActiveProcessorCount=2', '-Xms256M', '-Xmx1536M', '-jar', 'server.jar', 'nogui'],
                              cwd=srv, stdout=server_log, stderr=subprocess.STDOUT, env=env)
    t0 = time.monotonic()
    while True:
        if server.poll() is not None: raise RuntimeError('server exited during startup')
        if time.monotonic() - t0 > 120: raise RuntimeError('server startup deadline')
        try:
            with rcon_mod.Rcon() as r:
                for c in ('difficulty peaceful', 'time set 6000', 'weather clear', 'tick freeze'):
                    fixture_cmds.append({'command': c, 'response': r.command(c)})
            break
        except (OSError, RuntimeError): time.sleep(0.5)
    process = subprocess.Popen(command, cwd=GAME, stdout=client_log, stderr=subprocess.STDOUT, env=env)
    print(f'client PID={process.pid} log={RUN / "client.log"}', flush=True)
    sampler = sampler_mod.ProcessTreeSampler([process.pid], f'Java 26.3 {args.modset} client; bounded graphical run', max_samples=220,
                                             retired_reader=sampler_mod.mac_retired_counters)
    started = time.monotonic(); deadline = started + args.deadline + 60
    configured = False; offset = 0
    pose_s = ' '.join(map(str, pose))
    def drain():
        global offset
        with (RUN / 'client.log').open() as f:
            f.seek(offset); text = f.read(); offset = f.tell()
        for line in text.splitlines():
            if 'LODESTONE_BENCH' in line:
                markers.append({'monotonic': time.monotonic(), 'unix': time.time(), 'line': line}); print(line[:200], flush=True)
    while process.poll() is None and time.monotonic() < deadline:
        sampler.sample_if_due(); drain()
        if not configured:
            try:
                with rcon_mod.Rcon() as r:
                    if username in r.command('list'):
                        for c in (f'gamemode creative {username}', f'effect clear {username}', f'attribute {username} minecraft:gravity base set 0', f'tp {username} {pose_s}'):
                            fixture_cmds.append({'command': c, 'response': r.command(c)})
                        configured = True
            except (OSError, RuntimeError): pass
        time.sleep(0.2)
    stop_reason = 'client_exit' if process.poll() is not None else 'hard_deadline_killed'
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try: process.wait(10)
        except subprocess.TimeoutExpired: process.kill(); process.wait()
    sampler.sample_if_due(); drain()
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

# Summarise presents.csv within the recording window
import csv, statistics
run = json.loads((RUN / 'run.json').read_text())
rows = [int(r['elapsed_ns']) for r in csv.DictReader((RUN / 'presents.csv').open())]
dts = [(b - a) / 1e6 for a, b in zip(rows, rows[1:])]
def pct(v, p):
    s = sorted(v); k = (len(s) - 1) * p / 100; lo = int(k); hi = min(lo + 1, len(s) - 1); return s[lo] + (s[hi] - s[lo]) * (k - lo)
summary = {'trial': args.trial, 'modset': args.modset, 'status': run['status'], 'failure': run['failure'], 'exit_code': exit_code, 'stop_reason': stop_reason,
           'presents': len(rows), 'window_seconds': (rows[-1] - rows[0]) / 1e9 if rows else 0,
           'mean_present_hz': (len(rows) - 1) / ((rows[-1] - rows[0]) / 1e9) if len(rows) > 1 else 0,
           'interval_ms': {'mean': statistics.fmean(dts), 'p50': pct(dts, 50), 'p95': pct(dts, 95), 'p99': pct(dts, 99), 'max': max(dts)} if dts else None,
           'screenshot': run['screenshot'], 'backend': run['backend']}
(RUN / 'summary.json').write_text(json.dumps(summary, indent=2)); print(json.dumps(summary, indent=2))
