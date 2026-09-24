#!/usr/bin/env python3
"""Dump every entity type's vanilla `shadowRadius`/`shadowStrength`.

Regenerates the `SHADOW_RADII`/`SHADOW_STRENGTHS` tables in
`crates/lodestone-shell/src/gpu/entity_passes.rs`. Run it after bumping the
pinned Minecraft version and paste the output; the tables carry the values,
this carries the derivation.

    python3 scripts/dump-entity-shadows.py

# Why a source walk and not a JVM oracle

`EntityDataIndexOracle.java`'s trick is that `EntityDataAccessor`s are
`static` fields, so a bare `Bootstrap.bootStrap()` can read them.
`shadowRadius` is an **instance** field assigned in each renderer's
constructor, and constructing one needs an `EntityRendererProvider.Context`
-- a live `Minecraft`, a font, a baked model set. There is no headless way in.
The decompiled client under `.cache/mc/<version>/client-src` is the outside
source instead (data source 2 in CLAUDE.md), read mechanically rather than
transcribed by hand.

# How it resolves a value

Per `EntityRenderers.register(EntityTypes.X, YRenderer::new)`:

1. `this.shadowRadius = <literal>` anywhere in `YRenderer` wins outright.
2. Otherwise, if `YRenderer`'s superclass chain contains a class assigning
   `this.shadowRadius = <parameter>` (`LivingEntityRenderer` and friends),
   the last float literal in `YRenderer`'s own `super(...)`/`this(...)` call
   is that parameter.
3. Otherwise recurse into the superclass.
4. Otherwise `EntityRenderer`'s own field default, `0.0F` -- which is a real
   answer, not a fallback: 33 registered types genuinely cast no shadow.

Two traps this hit, both worth keeping in mind if you extend it:

* **`class Foo<S extends Bar> extends Baz` -- a naive `extends` regex finds
  `Bar`.** Superclass extraction has to skip `extends` inside the generic
  parameter list, or `ZombieRenderer` resolves through `ZombieRenderState`
  and reports 0.0. That misreading put 51 types at "no shadow", every one of
  them wrong, and it looks entirely plausible in the output.
* **A literal can arrive at the registration site**, not in the class
  (`new GiantMobRenderer(context, 6.0F)` against `super(..., 0.5F * scale)`).
  `OVERRIDES` carries those by hand, with the source line that settles each.
"""
import re, os, glob, sys
from collections import Counter

SRC = os.environ.get('LODESTONE_MC_SRC', '.cache/mc/26.2/client-src')
FLOAT = r'-?\d+(?:\.\d+)?F'

# Values the walk cannot reach, each with the source that settles it.
OVERRIDES = {
    # `new GiantMobRenderer(context, 6.0F)` at the registration site, against
    # `super(context, model, 0.5F * scale)` in the constructor.
    'giant': (3.0, 'GiantMobRenderer: 0.5F * 6.0F scale from the registration'),
    # `DisplayRenderer.getShadowRadius` reads the entity's own synced
    # `shadowRadius`, whose accessor default is 0.0F. Per-entity data, not a
    # renderer constant -- `DisplayDraw` does not carry it yet, so the default
    # is the honest answer and an explicitly reported one would be ignored.
    'block_display': (0.0, 'Display.DATA_SHADOW_RADIUS_ID default 0.0F (synced, not modelled)'),
    'item_display': (0.0, 'Display.DATA_SHADOW_RADIUS_ID default 0.0F (synced, not modelled)'),
    'text_display': (0.0, 'Display.DATA_SHADOW_RADIUS_ID default 0.0F (synced, not modelled)'),
    # Not in EntityRenderers.PROVIDERS -- the avatar renderer is registered
    # separately. `AvatarRenderer: super(context, new PlayerModel(...), 0.5F)`.
    'player': (0.5, 'AvatarRenderer: super(..., 0.5F)'),
}

files = {}
for p in glob.glob(os.path.join(SRC, '**', '*.java'), recursive=True):
    files.setdefault(os.path.basename(p)[:-5], []).append(p)


def read(cls):
    ps = files.get(cls)
    return open(ps[0]).read() if ps else None


def superclass(src, cls):
    """`cls`'s declared superclass, skipping `extends` inside `<...>`."""
    m = re.search(r'\bclass\s+' + re.escape(cls) + r'\b', src)
    if not m:
        return None
    i, depth = m.end(), 0
    while i < len(src):
        c = src[i]
        if c == '<':
            depth += 1
        elif c == '>':
            depth -= 1
        elif c == '{' and depth == 0:
            return None
        elif depth == 0 and src.startswith('extends', i) and not src[i - 1].isalnum():
            j = i + 7
            while j < len(src) and src[j].isspace():
                j += 1
            k = j
            while k < len(src) and (src[k].isalnum() or src[k] == '_'):
                k += 1
            return src[j:k]
        i += 1
    return None


def takes_shadow_param(cls, d=0):
    if d > 10:
        return False
    src = read(cls)
    if src is None:
        return False
    if re.search(r'this\.shadowRadius\s*=\s*[A-Za-z_]\w*\s*;', src):
        return True
    sup = superclass(src, cls)
    return takes_shadow_param(sup, d + 1) if sup else False


def resolve(cls, field='shadowRadius', d=0, chain=None):
    chain = (chain or []) + [cls]
    if d > 10:
        return None, 'too deep: ' + '->'.join(chain)
    src = read(cls)
    if src is None:
        return None, f'no source for {cls}'
    m = re.search(r'this\.' + field + r'\s*=\s*(' + FLOAT + r')\s*;', src)
    if m:
        return float(m.group(1)[:-1]), '->'.join(chain) + f': this.{field} = {m.group(1)}'
    sup = superclass(src, cls)
    if sup and field == 'shadowRadius' and takes_shadow_param(sup):
        for sm in re.finditer(r'\b(?:super|this)\(([^;]*?)\)\s*;', src, re.S):
            lits = re.findall(FLOAT, sm.group(1))
            if lits:
                return float(lits[-1][:-1]), '->'.join(chain) + f': (..., {lits[-1]}) into {sup}'
    if sup:
        return resolve(sup, field, d + 1, chain)
    default = 0.0 if field == 'shadowRadius' else 1.0
    return default, '->'.join(chain) + f': EntityRenderer default {default}'


def main():
    reg = read('EntityRenderers')
    if reg is None:
        sys.exit(f'no EntityRenderers.java under {SRC}')
    rows, strengths = [], []
    for m in re.finditer(r'register\(\s*EntityTypes\.([A-Z0-9_]+)\s*,\s*(.*?)\);', reg, re.S):
        ty, body = m.group(1).lower(), m.group(2)
        if ty in OVERRIDES:
            rows.append((ty, OVERRIDES[ty][0], OVERRIDES[ty][1]))
            continue
        rm = (re.search(r'new\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]', body)
              or re.search(r'([A-Za-z_][A-Za-z0-9_]*)::new', body))
        if not rm:
            rows.append((ty, None, 'no renderer parsed: ' + ' '.join(body.split())[:70]))
            continue
        v, p = resolve(rm.group(1))
        rows.append((ty, v, p))
        s, sp = resolve(rm.group(1), 'shadowStrength')
        if s is not None and s != 1.0:
            strengths.append((ty, s, sp))
    for ty, (v, p) in OVERRIDES.items():
        if not any(r[0] == ty for r in rows):
            rows.append((ty, v, p))
    rows.sort()
    strengths.sort()

    unresolved = [r for r in rows if r[1] is None]
    print(f'// {len(rows)} entity types, {len(unresolved)} unresolved')
    for r in unresolved:
        print(f'//   UNRESOLVED {r[0]}: {r[2]}')
    print('// radius histogram: '
          + ', '.join(f'{k}x{v}' for k, v in sorted(Counter(r[1] for r in rows if r[1] is not None).items())))
    print()
    print('const SHADOW_RADII: &[(&str, f32)] = &[')
    for ty, v, _ in rows:
        if v is not None:
            print(f'    ("{ty}", {v}),')
    print('];')
    print()
    print('const SHADOW_STRENGTHS: &[(&str, f32)] = &[')
    for ty, v, _ in strengths:
        print(f'    ("{ty}", {v}),')
    print('];')
    print()
    print('// ---- provenance ----')
    for ty, v, p in rows:
        print(f'// {ty}: {v} | {p}')


if __name__ == '__main__':
    main()
