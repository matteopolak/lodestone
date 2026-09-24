# Aquifer region cache

## What it is

`AquiferRegionCache` is a bounded, request-owned memo for aquifer candidate locations and fluid statuses. It lets chunk-bound `AquiferSystem` values reuse candidates whose absolute aquifer grid coordinates cross a chunk boundary while retaining each chunk's own shortcut and working caches.

## How it works

The cache is a fixed-size open-addressing table owned by the region executor. A key is the absolute `(grid_x, grid_y, grid_z)` coordinate; location and status are populated lazily and can be retained independently. The region prefix also keeps one `StartSampler` for its source-start walk, whose bounded per-chunk aquifer cache is reused across every target in the request. The caller passes `&mut AquiferRegionCache` only to the region-only vertical resolver, so the scalar aquifer path has no shared-cache branch or synchronization overhead.

## How to change it

Create one `AquiferRegionCache` for a bounded request and pass `&mut` to each enabled aquifer's region-only vertical resolver. Do not retain it on a generator or use it across worlds. Preserve the absolute-grid key and keep the local aquifer caches and density/shortcut checks ahead of shared lookup. Use the scalar/shared 4×4 control when changing capacity or key handling.

## Configuration

`AquiferRegionCache::new` uses the default bounded capacity in `aquifer/mod.rs`; tests and tightly bounded callers may use `with_capacity`. No environment variable or global setting controls the cache.

## Dependencies

The cache uses standard-library vectors and closures. Status computation depends on the aquifer's positional locations, preliminary-surface cache, and five point-density routes supplied by the owning generator.
