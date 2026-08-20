# Placed Player Head Skin Design

## What it is

This change carries the owner skin encoded in a placed player head's block-entity
NBT through block-entity collection, batching, asynchronous skin loading, and GPU
texture selection. A head uses Steve only while its skin is absent, invalid, or
not yet available.

## Problem

The world retains raw block-entity NBT, but skull collection currently keeps only
the block position and state ID. `SkullSpawn`, `BlockEntityInstance`, and the GPU
draw batch all carry a static texture stem, so every player skull resolves to the
built-in Steve texture even when the server supplied a `textures` property.

Remote player entities already decode profile texture properties, deduplicate
downloads, and upload skin bind groups. The missing piece is preserving the same
identity for block entities and selecting that cache at draw time.

## Goals

- Decode the modern skull `profile` compound and its `textures` property.
- Reuse the existing texture-property decoder, downloader, and GPU skin cache.
- Keep static skull types on their existing built-in textures.
- Fall back safely while a skin is pending or when profile data is malformed.
- Batch heads with different skins separately.

## Non-goals

- Resolving profile names or UUIDs through Mojang at render time.
- Deriving the modern set of default skins from UUID when no texture property is
  present.
- Changing skull models, transformations, animation, or UV layout.
- Supporting profile patch/resource-location variants unless a captured packet or
  fixture demonstrates that the current server path emits them.

## Design

### NBT extraction

The block-entity collection path reads the retained NBT before constructing a
skull spawn. For player heads it looks for the modern `profile` compound and its
property list, then extracts the `textures` property's value and optional
signature. Parsing is pure, bounded, and tolerant of absent or wrong-typed tags.
Static skeleton, wither-skeleton, zombie, creeper, dragon, and piglin heads do not
enter this path.

The existing remote-skin module gains a reusable entry point accepting raw
profile properties or a texture property. Both entity profiles and block heads
then use the same Base64/JSON decoder, URL handling, and fetch cache.

### Texture identity

Replace the static-only skull texture field with a typed identity such as:

```text
Static(texture stem)
PlayerSkin(canonical URL)
```

That identity is carried by the spawn, resolved instance, render batch, and draw
batch. Batching keys include the full identity, so two player skins cannot share
one draw call accidentally. URL ownership should use a cheap cloneable string
type because it crosses frame-local collections.

### Loading and rendering

When collection discovers a player-skin URL, it requests it through the existing
deduplicating remote-skin loader. Completed downloads continue through the current
GPU upload path and populate the existing player-skin bind-group cache.

At draw time, a static identity consults the block-entity texture set. A player
identity consults the dynamic player-skin cache. If the dynamic texture is not
installed yet, the draw uses the existing Steve player-head texture for that
frame. Because the batch retains the dynamic identity, a later frame selects the
real texture without rebuilding world data.

## Failure handling

Absent, malformed, unsupported, or undecodable profile data falls back to Steve.
Network and image-decode failures use the remote-skin loader's deduplicated
warning/failure cache and do not remove the head. No authentication token or
private profile data is introduced into the texture request.

## Tests

- Valid modern player-head NBT extracts the expected texture URL.
- Missing, wrong-typed, and malformed profile/property data returns no skin.
- Static skull types ignore unrelated profile NBT.
- Two URLs produce distinct batch keys; repeated URLs coalesce.
- Repeated collection requests one download through the existing cache.
- Pending/failed skins select Steve, while an installed bind group is selected on
  the next render.
- Existing skull orientation, lighting, and batching tests remain green.

## How to change it

NBT extraction belongs beside block-entity collection; texture-property decoding
belongs in the shared remote-skin module; typed texture identity belongs in the
renderer-facing block-entity types. New skull profile encodings should be added as
parser fixtures before widening production parsing.

## Configuration and dependencies

There is no new setting. The feature depends on retained block-entity NBT,
`lodestone-assets` texture-property decoding, the shell remote-skin loader, the
renderer block-entity batching model, and the GPU player-skin cache.
