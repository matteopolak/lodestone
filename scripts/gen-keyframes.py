#!/usr/bin/env python3
"""Regenerate crates/lodestone-assets/src/keyframe/data.rs from the reference cache.

Reads the animation definition sources under `.cache/mc/<mc-version>/client-src/.../animation/definitions`
and emits each definition we drive as plain data: length, looping, and per-bone
channels of `(time, vector, interpolation)` keys. Vectors are stored exactly as
the client holds them after construction: rotations in radians (f32 product of
the degree value and the f32 degree-to-radian factor), positions with the y
component negated, scales as `value - 1`.

    python3 scripts/gen-keyframes.py
"""
import ctypes
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
VERSION = (ROOT / "mc-version").read_text().strip()
DEFS = ROOT / ".cache" / "mc" / VERSION / "client-src/net/minecraft/client/animation/definitions"
OUT = ROOT / "crates/lodestone-assets/src/keyframe/data.rs"

# (source file, source constant, rust variant)
WANTED = [
    ("RabbitAnimation", "HOP", "RabbitHop"),
    ("RabbitAnimation", "IDLE_HEAD_TILT", "RabbitIdleHeadTilt"),
    ("BabyRabbitAnimation", "HOP", "BabyRabbitHop"),
    ("BabyRabbitAnimation", "IDLE_HEAD_TILT", "BabyRabbitIdleHeadTilt"),
    ("BatAnimation", "BAT_FLYING", "BatFlying"),
    ("BatAnimation", "BAT_RESTING", "BatResting"),
    ("FrogAnimation", "FROG_JUMP", "FrogJump"),
    ("FrogAnimation", "FROG_CROAK", "FrogCroak"),
    ("FrogAnimation", "FROG_TONGUE", "FrogTongue"),
    ("FrogAnimation", "FROG_WALK", "FrogWalk"),
    ("FrogAnimation", "FROG_SWIM", "FrogSwim"),
    ("FrogAnimation", "FROG_IDLE_WATER", "FrogIdleWater"),
    ("CamelAnimation", "CAMEL_WALK", "CamelWalk"),
    ("CamelAnimation", "CAMEL_SIT", "CamelSit"),
    ("CamelAnimation", "CAMEL_SIT_POSE", "CamelSitPose"),
    ("CamelAnimation", "CAMEL_STANDUP", "CamelStandUp"),
    ("CamelAnimation", "CAMEL_IDLE", "CamelIdle"),
    ("CamelAnimation", "CAMEL_DASH", "CamelDash"),
    ("CamelBabyAnimation", "CAMEL_BABY_WALK", "BabyCamelWalk"),
    ("CamelBabyAnimation", "CAMEL_BABY_SIT", "BabyCamelSit"),
    ("CamelBabyAnimation", "CAMEL_BABY_SIT_POSE", "BabyCamelSitPose"),
    ("CamelBabyAnimation", "CAMEL_BABY_STANDUP", "BabyCamelStandUp"),
    ("CamelBabyAnimation", "CAMEL_BABY_IDLE", "BabyCamelIdle"),
    ("CamelBabyAnimation", "CAMEL_BABY_DASH", "BabyCamelDash"),
    ("ArmadilloAnimation", "ARMADILLO_WALK", "ArmadilloWalk"),
    ("ArmadilloAnimation", "ARMADILLO_ROLL_OUT", "ArmadilloRollOut"),
    ("ArmadilloAnimation", "ARMADILLO_ROLL_UP", "ArmadilloRollUp"),
    ("ArmadilloAnimation", "ARMADILLO_PEEK", "ArmadilloPeek"),
    ("BabyArmadilloAnimation", "ARMADILLO_BABY_WALK", "BabyArmadilloWalk"),
    ("BabyArmadilloAnimation", "ARMADILLO_BABY_ROLL_OUT", "BabyArmadilloRollOut"),
    ("BabyArmadilloAnimation", "ARMADILLO_BABY_ROLL_UP", "BabyArmadilloRollUp"),
    ("BabyArmadilloAnimation", "ARMADILLO_BABY_PEEK", "BabyArmadilloPeek"),
    ("SnifferAnimation", "SNIFFER_WALK", "SnifferWalk"),
    ("SnifferAnimation", "SNIFFER_SNIFF_SEARCH", "SnifferSniffSearch"),
    ("SnifferAnimation", "SNIFFER_DIG", "SnifferDig"),
    ("SnifferAnimation", "SNIFFER_LONGSNIFF", "SnifferLongSniff"),
    ("SnifferAnimation", "SNIFFER_STAND_UP", "SnifferStandUp"),
    ("SnifferAnimation", "SNIFFER_HAPPY", "SnifferHappy"),
    ("SnifferAnimation", "SNIFFER_SNIFFSNIFF", "SnifferSniffSniff"),
    ("FoxBabyAnimation", "FOX_BABY_WALK", "BabyFoxWalk"),
    ("BabyAxolotlAnimation", "BABY_AXOLOTL_SWIM", "BabyAxolotlSwim"),
    ("BabyAxolotlAnimation", "AXOLOTL_WALK_FLOOR", "BabyAxolotlWalkFloor"),
    ("BabyAxolotlAnimation", "WALK_FLOOR_UNDERWATER", "BabyAxolotlWalkUnderwater"),
    ("BabyAxolotlAnimation", "IDLE_UNDERWATER", "BabyAxolotlIdleUnderwater"),
    ("BabyAxolotlAnimation", "IDLE_FLOOR_UNDERWATER", "BabyAxolotlIdleFloorUnderwater"),
    ("BabyAxolotlAnimation", "BABY_AXOLOTL_IDLE_FLOOR", "BabyAxolotlIdleFloor"),
    ("BabyAxolotlAnimation", "BABY_AXOLOTL_PLAY_DEAD", "BabyAxolotlPlayDead"),
]

F32 = lambda v: ctypes.c_float(v).value
DEG = F32(3.141592653589793 / 180.0)


def num(text):
    return float(text.strip().rstrip("FfDd"))


def fmt(v):
    if v == 0:
        return "0.0"
    s = "%.9g" % v
    if "." not in s and "e" not in s:
        s += ".0"
    return s


def convert(kind, a, b, c):
    if kind == "degreeVec":
        return [F32(F32(a) * DEG), F32(F32(b) * DEG), F32(F32(c) * DEG)]
    if kind == "posVec":
        return [F32(a), F32(-b), F32(c)]
    return [F32(a - 1.0), F32(b - 1.0), F32(c - 1.0)]  # scaleVec computes in double


KEY = re.compile(
    r"new Keyframe\(\s*([-\d.]+)F,\s*KeyframeAnimations\.(degreeVec|posVec|scaleVec)\(([^)]*)\),\s*"
    r"AnimationChannel\.Interpolations\.(LINEAR|CATMULLROM)\)"
)
CHANNEL = re.compile(r'"([a-z_0-9]+)",\s*new AnimationChannel\(\s*AnimationChannel\.Targets\.(POSITION|ROTATION|SCALE),(.*)', re.S)


def parse(file, const):
    src = (DEFS / (file + ".java")).read_text()
    m = re.search(r"AnimationDefinition " + const + r" = AnimationDefinition\.Builder\.withLength\(([\d.]+)F\)(.*?)\.build\(\);", src, re.S)
    if not m:
        sys.exit("missing definition %s.%s" % (file, const))
    length = num(m.group(1))
    body = m.group(2)
    looping = ".looping()" in body
    channels = []
    for chunk in body.split(".addAnimation(")[1:]:
        cm = CHANNEL.search(chunk)
        if not cm:
            sys.exit("%s.%s: unparsed channel" % (file, const))
        keys = []
        for km in KEY.finditer(cm.group(3)):
            a, b, c = [num(x) for x in km.group(3).split(",")]
            keys.append((F32(num(km.group(1))), convert(km.group(2), a, b, c), km.group(4)))
        channels.append((cm.group(1), cm.group(2), keys))
    # every Keyframe in the body must have been captured
    total = body.count("new Keyframe(")
    got = sum(len(k) for _, _, k in channels)
    if total != got:
        sys.exit("%s.%s: parsed %d of %d keyframes" % (file, const, got, total))
    return length, looping, channels


def screaming(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).upper()


def main():
    out = []
    out.append("//! Keyframe animation definitions as data, generated by `scripts/gen-keyframes.py`.")
    out.append("//! Regenerate with that script; do not edit by hand.")
    out.append("")
    out.append("use super::{AnimDef, Anim, ChannelDef, Interp::{CatmullRom, Linear}, Key, Target::{Position, Rotation, Scale}};")
    out.append("")
    out.append("const fn k(time: f32, value: [f32; 3], interp: super::Interp) -> Key {")
    out.append("    Key { time, value, interp }")
    out.append("}")
    out.append("")
    total = 0
    for file, const, variant in WANTED:
        length, looping, channels = parse(file, const)
        out.append("pub static %s: AnimDef = AnimDef {" % screaming(variant))
        out.append("    length: %s,\n    looping: %s,\n    channels: &[" % (fmt(length), "true" if looping else "false"))
        for bone, target, keys in channels:
            out.append('        ChannelDef { bone: "%s", target: %s, keys: &[' % (bone, target.capitalize()))
            for t, v, i in keys:
                total += 1
                out.append("            k(%s, [%s, %s, %s], %s)," % (fmt(t), fmt(v[0]), fmt(v[1]), fmt(v[2]), "CatmullRom" if i == "CATMULLROM" else "Linear"))
            out.append("        ] },")
        out.append("    ],\n};")
        out.append("")
    out.append("impl Anim {")
    out.append("    /// The definition this animation plays.")
    out.append("    #[must_use]")
    out.append("    pub fn def(self) -> &'static AnimDef {")
    out.append("        match self {")
    for _, _, variant in WANTED:
        out.append("            Anim::%s => &%s," % (variant, screaming(variant)))
    out.append("        }")
    out.append("    }")
    out.append("")
    out.append("    /// Every animation, in declaration order.")
    out.append("    pub const ALL: &'static [Anim] = &[")
    for _, _, variant in WANTED:
        out.append("        Anim::%s," % variant)
    out.append("    ];")
    out.append("}")
    out.append("")
    out.append("/// The number of keyframes across every definition, for the cross-check against the source count.")
    out.append("pub const KEYFRAME_COUNT: usize = %d;" % total)
    OUT.write_text("\n".join(out) + "\n")
    # The enum body, printed for pasting into mod.rs the first time.
    print("variants:", ", ".join(v for _, _, v in WANTED))
    print("keyframes:", total)


main()
