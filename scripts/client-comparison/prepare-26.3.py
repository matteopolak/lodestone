#!/usr/bin/env python3
"""Download + verify everything for the 26.3 Java arm into $LODESTONE_COMPARISON_ROOT (default /Volumes/LodestoneScratch/java-26.3) and write manifest.json."""
import hashlib, json, os, re, sys, urllib.request, zipfile, io, shutil
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
HERE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get('LODESTONE_COMPARISON_ROOT', '/Volumes/LodestoneScratch/java-26.3'))
UA = {'User-Agent': 'Lodestone-local-comparison/1'}
VER = '26.3'
records = []

def get(url):
    for i in range(4):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=60) as r:
                return r.read()
        except Exception as e:
            err = e
    raise err

def save(rel, url, size=None, algo=None, expected=None, kind='file'):
    p = ROOT / rel
    if p.exists():
        data = p.read_bytes()
    else:
        data = get(url)
    if size is not None and len(data) != size: raise ValueError(f'size {rel}')
    if algo and hashlib.new(algo, data).hexdigest() != expected: raise ValueError(f'hash {rel}')
    p.parent.mkdir(parents=True, exist_ok=True)
    if not p.exists(): p.write_bytes(data)
    return {'path': rel, 'url': url, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
            'verified': f'{algo}:{expected}' if algo else 'none', 'kind': kind}

manifest = json.load(open(ROOT / 'dl/version_manifest_v2.json'))
entry = next(v for v in manifest['versions'] if v['id'] == VER)
raw = get(entry['url'])
assert hashlib.sha1(raw).hexdigest() == entry['sha1']
(ROOT / 'dl/26.3.json').write_bytes(raw)
vj = json.loads(raw)
records.append({'path': 'dl/26.3.json', 'url': entry['url'], 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(), 'verified': 'sha1:' + entry['sha1'], 'kind': 'version-json'})
dl = vj['downloads']
for k in ('client', 'server'):
    records.append(save(f'minecraft/{VER}-{k}.jar', dl[k]['url'], dl[k]['size'], 'sha1', dl[k]['sha1'], k + '-jar'))
lg = vj['logging']['client']['file']
records.append(save('minecraft/' + lg['id'], lg['url'], lg['size'], 'sha1', lg['sha1'], 'logging'))

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
libs = [l['downloads']['artifact'] for l in vj['libraries'] if permitted(l.get('rules'))]
with ThreadPoolExecutor(8) as ex:
    records += list(ex.map(lambda a: save('libraries/' + a['path'], a['url'], a['size'], 'sha1', a['sha1'], 'library'), libs))
print('libraries', len(libs), flush=True)

ai = vj['assetIndex']
idx_raw = get(ai['url'])
assert hashlib.sha1(idx_raw).hexdigest() == ai['sha1']
(ROOT / f'assets/indexes/{ai["id"]}.json').parent.mkdir(parents=True, exist_ok=True)
(ROOT / f'assets/indexes/{ai["id"]}.json').write_bytes(idx_raw)
records.append({'path': f'assets/indexes/{ai["id"]}.json', 'url': ai['url'], 'bytes': len(idx_raw), 'sha256': hashlib.sha256(idx_raw).hexdigest(), 'verified': 'sha1:' + ai['sha1'], 'kind': 'asset-index'})
objs = json.loads(idx_raw)['objects']
uniq = {v['hash']: v['size'] for v in objs.values()}
def asset(item):
    h, s = item
    return save(f'assets/objects/{h[:2]}/{h}', f'https://resources.download.minecraft.net/{h[:2]}/{h}', s, 'sha1', h, 'asset')
with ThreadPoolExecutor(16) as ex:
    arecs = list(ex.map(asset, uniq.items()))
print('assets', len(arecs), sum(r['bytes'] for r in arecs), flush=True)
records.append({'kind': 'asset-objects-summary', 'count': len(arecs), 'bytes': sum(r['bytes'] for r in arecs), 'note': 'each object verified by sha1 == its name; per-object records omitted'})

# Fabric
loader = json.loads(get(f'https://meta.fabricmc.net/v2/versions/loader/{VER}'))
stable = next(e for e in loader if e['loader']['stable'])['loader']['version']
purl = f'https://meta.fabricmc.net/v2/versions/loader/{VER}/{stable}/profile/json'
praw = get(purl)
(ROOT / 'fabric-profile.json').write_bytes(praw)
records.append({'path': 'fabric-profile.json', 'url': purl, 'bytes': len(praw), 'sha256': hashlib.sha256(praw).hexdigest(), 'verified': 'none (meta api)', 'kind': 'fabric-profile'})
prof = json.loads(praw)
for l in prof['libraries']:
    g, a, v = l['name'].split(':')
    rel = f'{g.replace(".", "/")}/{a}/{v}/{a}-{v}.jar'
    url = l['url'].rstrip('/') + '/' + rel
    expected = l.get('sha256')
    if expected:
        rec = save('fabric-libraries/' + rel, url, l.get('size'), 'sha256', expected, 'fabric-library')
    else:
        sha1 = get(url + '.sha1').decode().split()[0]
        rec = save('fabric-libraries/' + rel, url, None, 'sha1', sha1, 'fabric-library')
    records.append(rec)

# Modrinth
MODS = {  # project, version id, sets
    'sodium': ('bAZQdGpg', ['optimized']),
    'lithium': ('xS0Q8LSi', ['optimized', 'chunkgen']),
    'ferritecore': ('d5ddUdiB', ['optimized', 'chunkgen']),
    'immediatelyfast': ('3MP9UR23', ['optimized']),
    'fabric-api': ('bNnaTiuM', ['optimized']),
    'entityculling': ('F4loCvYt', ['optimized']),
    'c2me': ('FXjQDzq7', ['chunkgen']),
}
for name, (vid, sets) in MODS.items():
    v = json.loads(get(f'https://api.modrinth.com/v2/version/{vid}'))
    assert VER in v['game_versions'] and 'fabric' in v['loaders'], name
    f = next(x for x in v['files'] if x['primary'])
    rec = save(f'mods/{f["filename"]}', f['url'], f['size'], 'sha512', f['hashes']['sha512'], 'mod')
    rec.update(project=name, modrinth_version_id=vid, version_number=v['version_number'], sets=sets, dependencies=v['dependencies'])
    records.append(rec)
    for s in sets:
        (ROOT / 'modsets' / s).mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / rec['path'], ROOT / 'modsets' / s / f['filename'])
(ROOT / 'manifest.json').write_text(json.dumps({'minecraft': VER, 'fabric_loader': stable, 'files': records}, indent=1))
print('done', len(records))
