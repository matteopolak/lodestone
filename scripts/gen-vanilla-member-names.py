#!/usr/bin/env python3
"""Regenerates xtask/vanilla-member-names.txt from the reference sources.

Lists every camelCase name the decompiled reference release (`mc-version`)
calls or declares as a method, minus the names that belong to libraries a
comment may cite freely (JDK, Netty, Brigadier, JOML, Bukkit/Paper). `cargo
xtask check-comment-voice` fails on any of these written in a cited form
(`name(`, `Type.name`, `Type::name`) in a comment or doc unless this workspace's
own code spells the name itself.
"""
import os
import re
import sys

root = os.path.join(os.path.dirname(__file__), "..")
version = open(os.path.join(root, "mc-version")).read().strip()

# Names the reference source calls but that are not its own: citing them is fine.
LIBRARY_NAMES = set("""
applyAsInt charAt comparingDouble currentTimeMillis doubleToRawLongBits endsWith
floorDiv floorMod forEach getBytes getOrDefault ifPresent indexOf isBlank isEmpty
isFinite isHighSurrogate lowestOneBit maxMemory nameUUIDFromBytes
numberOfLeadingZeros numberOfTrailingZeros orElse orElseGet parseInt peekLast
putAll readBoolean readByte readLong readString readUnquotedString
requireNonNullElse reverseBytes setAccessible startsWith thenComparingInt
toByteArray toLowerCase getRuntime getFirst getLast addFirst addLast
createDirectory deleteIfExists computeIfAbsent computeIfPresent toString hashCode
equals compareTo getClass getMessage getOnlinePlayers getBlockAt sendMessage
rotateX rotateY rotateZ rotateAround rotationX rotationY rotationZ rotationXYZ
rotationYXZ rotationZYX libFuzzer toList toArray orElseThrow ifPresentOrElse
nextDouble nextFloat nextInt nextLong nextBoolean nextGaussian writeByte writeFloat
writeShort writeInt writeLong writeDouble writeBoolean writeChar readFloat readShort
readInt readDouble listOf getKey getValue getName getSize getType getString getLength
getIndex isActive
""".split())

names = set()
call = re.compile(r"\b([a-z][a-z0-9]*(?:[A-Z][a-z0-9]*)+)\s*\(")
for sub in ("src", "client-src"):
    base = os.path.join(root, ".cache", "mc", version, sub)
    for dirpath, _, files in os.walk(base):
        for f in files:
            if f.endswith(".java"):
                text = open(os.path.join(dirpath, f), errors="ignore").read()
                names.update(call.findall(text))
kept = sorted(n for n in names if len(n) >= 6 and n not in LIBRARY_NAMES)
if not kept:
    sys.exit("no reference sources found; run the cache setup first")
with open(os.path.join(root, "xtask", "vanilla-member-names.txt"), "w") as out:
    out.write("\n".join(kept) + "\n")
print(f"wrote {len(kept)} names")
