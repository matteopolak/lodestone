#!/usr/bin/env python3
"""Create the deterministic 26.3 world fixture (seed -4172144997902289642): pre-generate chunks around the bench pose, save, stop.
Usage: make-fixture.py [probe x,z ...]   (probes print the ground y at each x,z)"""
import os, subprocess, time, sys, re, shutil
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from rcon import Rcon
R = Path(os.environ.get('LODESTONE_COMPARISON_ROOT', '/Volumes/LodestoneScratch/java-26.3'))
JAVA = os.environ.get('LODESTONE_COMPARISON_JAVA', str(Path.home() / 'Library/Application Support/minecraft/runtime/java-runtime-epsilon/mac-os-arm64/java-runtime-epsilon/jre.bundle/Contents/Home/bin/java'))
S = R / 'world-fixture'
CENTER_CHUNK = (-30, -25)   # chunk of the default pose (-472.5, -392.5)
SEED = '-4172144997902289642'
if (S / 'world').exists(): shutil.rmtree(S / 'world')
S.mkdir(exist_ok=True)
shutil.copy2(R / 'minecraft/26.3-server.jar', S / 'server.jar'); (S / 'eula.txt').write_text('eula=true\n')
(S / 'server.properties').write_text(f'server-ip=127.0.0.1\nserver-port=25580\nlevel-name=world\nlevel-seed={SEED}\nonline-mode=false\nenforce-secure-profile=false\nview-distance=10\nsimulation-distance=8\nenable-rcon=true\nrcon.port=25581\nrcon.password=lodestone\ngamemode=creative\nforce-gamemode=true\ndifficulty=peaceful\nallow-flight=true\nspawn-protection=0\npause-when-empty-seconds=0\nmax-players=2\nmax-tick-time=-1\nspawn-monsters=false\nspawn-animals=false\nwhite-list=false\n')
p = subprocess.Popen([JAVA, '-Xms256M', '-Xmx2G', '-jar', 'server.jar', 'nogui'], cwd=S, stdout=open(S / 'fixture-gen.log', 'w'), stderr=subprocess.STDOUT)
try:
    for _ in range(240):
        time.sleep(1)
        try: r = Rcon(); break
        except Exception: pass
    cx, cz = CENTER_CHUNK
    # 21x21 chunk square (view distance 10 around the pose chunk) as four <=256-chunk forceload rectangles
    spans = [(cx - 10, cx), (cx + 1, cx + 10)]
    zspans = [(cz - 10, cz), (cz + 1, cz + 10)]
    for a, b in spans:
        for c, d in zspans:
            print(r.command(f'forceload add {a*16} {c*16} {b*16+15} {d*16+15}'))
    time.sleep(45)
    for arg in sys.argv[1:]:
        x, z = arg.split(',')
        r.command('kill @e[type=marker]')
        r.command(f'summon marker {x} 319 {z} {{Tags:["m"]}}')
        r.command('execute as @e[tag=m] at @s positioned over motion_blocking_no_leaves run tp @s ~ ~ ~')
        print(arg, r.command('data get entity @e[tag=m,limit=1] Pos'))
        print('  block below:', r.command('execute as @e[tag=m] at @s run data get block ~ ~-1 ~ id') or '', r.command('execute as @e[tag=m] at @s if block ~ ~-1 ~ grass_block'))
        r.command('kill @e[type=marker]')
    print(r.command('forceload remove all'))
    time.sleep(2)
    print(r.command('save-all flush'))
finally:
    try: r.command('stop')
    except Exception: pass
    try: p.wait(60)
    except Exception: p.kill()
