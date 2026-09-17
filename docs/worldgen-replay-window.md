# Worldgen replay window

## What it is

The Overworld replay window is the request-owned preparation shared by adjacent
FEATURES completions. It keeps terrain products, height columns, and source
selection plans alive for the request without adding another generator-global
cache.

## How it works

Products are stored once in a coordinate-sorted vector. Each target context
contains fixed `u16` indices for its 5×5 read window and nine source plans;
contexts therefore share the request window rather than cloning 25 product
handles or rebuilding keyed source maps. Height columns are copied once into a
shared `RegionHeightStorage`; `RegionHeights` is a compact fixed-slot view that
retains the same pre-clamp and missing-column failure semantics as the fixture
representation.

The request union is a list of admitted coordinates, not a bounding rectangle.
Adjacent targets share products and plans, while far-apart targets do not cause
an unbounded empty rectangle to be allocated. Context-aware decoration also
exposes its centre pre-ore product, allowing callers to reuse the product they
already admitted instead of probing the staged store again.

## How to change it

Keep product and plan vectors coordinate-sorted: fixed-index lookup uses binary
search before contexts are assembled. Update `context_from_window` when the
read radius or source schedule changes, and preserve the row-major `(x, z)` slot
conventions used by `RegionView`. Do not make a context own per-source maps or a
dense 80×80 table; those erase the sharing boundary this module provides.

## Configuration

There are no environment variables or runtime flags. The read radius comes
from `feature::region_view::WIDE_RADIUS`, and source order comes from the
Overworld stage schedule.

## Dependencies

The window is built by `overworld::decorate` from the staged
`OverworldGenerator` pre-ore products and decoration catalog. `RegionHeights`
and `RegionHeightStorage` live in `feature`; `RegionView`, vegetation grids, and
the lifecycle source consume the resulting fixed-index context.
